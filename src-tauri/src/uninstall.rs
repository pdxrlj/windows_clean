//! 卸载执行器：解析并运行卸载串。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::thread;

use crate::safety;
use crate::types::{AppSource, InstalledApp, UninstallResult};

fn refused(reason: String) -> UninstallResult {
    UninstallResult {
        success: false,
        exit_code: None,
        log: format!("已拒绝执行：{}", reason),
        requires_reboot: false,
    }
}

/// 运行卸载。优先静默串，否则交互串。MSI 转为 msiexec /x /quiet。
///
/// 所有入口都会再做一次安全校验（受保护条目、程序白名单、Appx 包名），
/// 即便调用方传入被伪造的条目也不会执行危险命令。
pub fn run_uninstall(app: &InstalledApp, silent: bool) -> UninstallResult {
    let start = Instant::now();

    if app.is_protected {
        return refused(format!(
            "{}（受保护条目不允许卸载）",
            app.protected_reason
                .as_deref()
                .unwrap_or("系统组件或系统依赖")
        ));
    }

    // Store 应用走 Remove-AppxPackage
    if app.source == AppSource::Store {
        let full = match extract_appx_package_name(&app.uninstall_string) {
            Some(f) => f,
            None => return refused("无法解析 Store 包全名".into()),
        };
        if let Err(e) = safety::validate_appx_package_name(&full) {
            return refused(e);
        }
        match crate::store::remove_appx(&full) {
            Ok(_) => {
                return UninstallResult {
                    success: true,
                    exit_code: Some(0),
                    log: "Remove-AppxPackage 完成".into(),
                    requires_reboot: false,
                };
            }
            Err(e) => {
                return UninstallResult {
                    success: false,
                    exit_code: None,
                    log: e,
                    requires_reboot: false,
                };
            }
        }
    }

    // 选择卸载串
    let raw = if silent {
        app.quiet_uninstall_string
            .as_ref()
            .or(app.uninstall_string.as_ref())
    } else {
        app.uninstall_string.as_ref()
    };

    let raw = match raw {
        Some(r) if !r.is_empty() => r.clone(),
        _ => {
            return UninstallResult {
                success: false,
                exit_code: None,
                log: "没有可用的卸载串".into(),
                requires_reboot: false,
            }
        }
    };

    // MSI 处理：MsiExec.exe /I{GUID} -> /x{GUID} /quiet
    if raw.to_lowercase().contains("msiexec") {
        let guid = match extract_msi_guid(&raw) {
            Some(g) => g,
            None => return refused("卸载串包含 msiexec 但无法解析 MSI 产品码".into()),
        };
        let quiet_arg = if silent { "/quiet" } else { "/passive" };
        let args = vec![
            format!("/x{}", guid),
            quiet_arg.to_string(),
            "/norestart".to_string(),
        ];
        if let Err(e) = safety::validate_uninstall_invocation("msiexec.exe", &args) {
            return refused(e);
        }
        let mut cmd = Command::new(safety::system32_exe("msiexec.exe"));
        for a in args {
            cmd.arg(a);
        }
        return run_and_wait(cmd, start);
    }

    // 解析普通串
    let (program, args) = split_command(&raw);
    if program.is_empty() {
        return UninstallResult {
            success: false,
            exit_code: None,
            log: format!("无法解析卸载串: {}", raw),
            requires_reboot: false,
        };
    }
    if let Err(e) = safety::validate_uninstall_invocation(&program, &args) {
        return refused(e);
    }

    let mut cmd = if silent || has_silent_flags(&raw) {
        // 用户看不到卸载界面的情况（勾了静默，或注册表命令本身就带 /S /SILENT）：
        // 卸载器若是控制台程序，这里会闪一个黑框，用 CREATE_NO_WINDOW 隐掉。
        // 交互模式保持原样——控制台卸载器可能要显示提示文字，隐掉窗口反而会卡死。
        safety::hidden_command(&program)
    } else {
        Command::new(&program)
    };
    for a in args {
        cmd.arg(a);
    }
    run_and_wait(cmd, start)
}

/// 卸载串是否自带静默参数。
///
/// 前端勾“优先使用静默卸载命令”只是其中一种情况；注册表里那条命令本身就可能已经带了
/// `/SILENT`、`/VERYSILENT`、`/qn`，此时用户同样看不到任何界面，控制台窗口也该一起隐藏。
fn has_silent_flags(command: &str) -> bool {
    const KEYS: &[&str] = &[
        "/s",
        "/silent",
        "/verysilent",
        "/quiet",
        "/qn",
        "--silent",
        "--quiet",
    ];
    command
        .split_whitespace()
        .map(|a| a.trim_matches('"'))
        .any(|a| KEYS.iter().any(|k| a.eq_ignore_ascii_case(k)))
}

fn run_and_wait(mut cmd: Command, _start: Instant) -> UninstallResult {
    // 用 null 而不是 piped：不读取管道时子进程写满缓冲区会阻塞，
    // 导致卸载器卡死直到 30 分钟超时。这里不需要卸载器输出。
    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // ERROR_ELEVATION_REQUIRED (740)：当前进程没有管理员权限，
            // 而软件自带的卸载器要求提权（常见于 debug/dev 构建）。
            let log = if e.raw_os_error() == Some(740) {
                "需要管理员权限：当前 CleanSweep 未以管理员身份运行，无法启动该卸载程序。\n\
                 解决方法：重新启动 CleanSweep，并在 Windows UAC 弹框中选择「是」允许管理员权限；\n\
                 开发调试时可关闭程序后在「以管理员身份运行」的终端里执行 pnpm tauri dev；\n\
                 安装版（release）启动时会自动请求管理员权限。"
                    .to_string()
            } else {
                format!("启动卸载程序失败: {}", e)
            };
            return UninstallResult {
                success: false,
                exit_code: None,
                log,
                requires_reboot: false,
            };
        }
    };
    let pid = child.id();

    // 等待最多 30 分钟
    let timeout = Duration::from_secs(30 * 60);
    let mut elapsed = Duration::ZERO;
    let step = Duration::from_millis(500);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                let mut requires_reboot = false;
                // 1603 (fatal error) 时尝试取输出
                // 3010 表示需要重启
                if code == Some(3010) {
                    requires_reboot = true;
                }
                return UninstallResult {
                    success: matches!(code, Some(0) | Some(1605) | Some(1641) | Some(3010)),
                    exit_code: code,
                    log: format!("退出码 {}", code.unwrap_or(-1)),
                    requires_reboot,
                };
            }
            Ok(None) => {
                thread::sleep(step);
                elapsed += step;
                if elapsed >= timeout {
                    // 只杀直接子进程不够：NSIS/Inno 等卸载器会再拉起子进程。
                    // 用 taskkill /T 结束整棵进程树（绝对路径，避免 PATH 劫持）。
                    let _ = safety::hidden_command(safety::system32_exe("taskkill.exe"))
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .output();
                    let _ = child.kill();
                    return UninstallResult {
                        success: false,
                        exit_code: None,
                        log: format!("卸载超时 ({}秒)，已结束进程树 {}", timeout.as_secs(), pid),
                        requires_reboot: false,
                    };
                }
            }
            Err(e) => {
                return UninstallResult {
                    success: false,
                    exit_code: None,
                    log: format!("等待失败: {}", e),
                    requires_reboot: false,
                };
            }
        }
    }
}

fn extract_appx_package_name(s: &Option<String>) -> Option<String> {
    let s = s.as_ref()?;
    let i = s.find("-Package '")? + "-Package '".len();
    let j = s[i..].find('\'')?;
    Some(s[i..i + j].to_string())
}

fn extract_msi_guid(s: &str) -> Option<String> {
    // 提取 {XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}
    let lo = s.find('{')?;
    let hi = s[lo..].find('}')?;
    Some(s[lo..lo + hi + 1].to_string())
}

/// 把 "C:\Program Files\App\uninst.exe" /S 这样的串拆成 program + args
pub(crate) fn split_command(raw: &str) -> (String, Vec<String>) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (String::new(), vec![]);
    }
    if raw.starts_with('"') {
        if let Some(end) = raw[1..].find('"') {
            let program = raw[1..1 + end].to_string();
            let rest = raw[1 + end + 1..].trim();
            let args = shell_like_split(rest);
            return (program, args);
        }
    }
    // 没有引号：取第一个 token 作为 program，但若 token 含空格且不是 exe 路径，则按空格切
    let mut parts = raw.split_whitespace();
    let program = parts.next().unwrap_or("").to_string();
    let args: Vec<String> = parts.map(|s| s.to_string()).collect();
    // 如果 program 自身不含 .exe 但看起来路径含空格，尝试取前缀
    if !program.to_lowercase().ends_with(".exe") && !program.to_lowercase().ends_with(".com") {
        // 退一步：尝试把 raw 当成 "带空格的路径 参数"
        // 用 ASCII 小写查找：保持字节长度不变，避免非 ASCII 字符（İ/ẞ/K）导致的切片越界 panic。
        if let Some(idx) = raw.to_ascii_lowercase().find(".exe") {
            let prog = raw[..idx + 4].trim_start_matches('"').to_string();
            let rest = raw[idx + 4..].trim();
            let args = shell_like_split(rest);
            return (prog, args);
        }
    }
    (program, args)
}

fn shell_like_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut qchar = '"';
    for ch in s.chars() {
        if ch == '"' || ch == '\'' {
            if in_quote && ch == qchar {
                in_quote = false;
                continue;
            } else if !in_quote {
                in_quote = true;
                qchar = ch;
                continue;
            }
        }
        if ch.is_whitespace() && !in_quote {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::has_silent_flags;

    #[test]
    fn detects_silent_flags_without_matching_paths() {
        assert!(has_silent_flags(r#""C:\Program Files\App\unins000.exe" /SILENT"#));
        assert!(has_silent_flags(r"C:\App\uninst.exe /S"));
        assert!(has_silent_flags(r"msiexec /x{1234} /qn"));
        assert!(has_silent_flags(r"uninst.exe --quiet"));
        // 路径里的 \S 不是参数，不能误判成静默
        assert!(!has_silent_flags(r#""D:\Software\App\uninst.exe""#));
        assert!(!has_silent_flags(r"C:\Program Files\App\uninstall.exe"));
    }
}
