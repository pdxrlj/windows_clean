//! 强制删除前的系统还原点。
//!
//! 强制删除绕过软件自带的卸载器，删错之后没有“重装一次”这条退路，
//! 所以先让 Windows 自己的系统保护记一份快照。
//!
//! 失败是常见情况（系统保护未开启、当前不是管理员、24 小时内刚建过），
//! 所以这里返回 `Result` 而不是吞掉错误——由调用方把原因如实告诉用户，
//! 绝不静默跳过。

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::safety;

/// 创建还原点最多等这么久。`Checkpoint-Computer` 正常要 20–90 秒。
const TIMEOUT: Duration = Duration::from_secs(150);

/// 创建一个系统还原点。成功时返回一句可直接展示给用户的结果描述。
pub fn create_restore_point(description: &str) -> Result<String, String> {
    let desc = sanitize(description);
    let ps = powershell_path()?;

    // 成功 / 已有近期还原点 / 失败 三种情况都由脚本自己在 stdout 里标注，
    // 避免去匹配不同语言版本的 Windows 错误消息。
    let script = format!(
        "$ErrorActionPreference='Stop'; try {{ \
Checkpoint-Computer -Description '{desc}' -RestorePointType 'MODIFY_SETTINGS'; \
'WCREATED' }} catch {{ \
$last = Get-ComputerRestorePoint -ErrorAction SilentlyContinue | \
Sort-Object -Property SequenceNumber | Select-Object -Last 1; \
if ($last) {{ 'WEXISTING:' + $last.CreationTime.ToString('yyyy-MM-dd HH:mm') }} \
else {{ 'WFAILED:' + $_.Exception.Message }} }}"
    );

    let mut cmd = safety::hidden_command(&ps);
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &script,
    ]);
    let (stdout, stderr) = run_with_timeout(cmd)?;

    for line in stdout.lines() {
        let line = line.trim();
        if line == "WCREATED" {
            return Ok("已创建系统还原点".into());
        }
        if let Some(rest) = line.strip_prefix("WEXISTING:") {
            return Ok(format!(
                "24 小时内已有还原点（{}），无需新建",
                rest.trim()
            ));
        }
        if let Some(rest) = line.strip_prefix("WFAILED:") {
            return Err(humanize(rest.trim()));
        }
    }

    let fallback = if stderr.trim().is_empty() {
        "命令没有返回预期结果".to_string()
    } else {
        stderr.trim().to_string()
    };
    Err(humanize(&fallback))
}

/// 只保留能安全嵌入 PowerShell 单引号字符串的字符。
///
/// 软件名里可能带 `'`、`;`、`$()`，任何一个都能让脚本执行别的命令，
/// 所以这里做白名单而不是转义。
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .take(120)
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')' | ':' | '.') {
                c
            } else {
                ' '
            }
        })
        .collect();
    let t = cleaned.trim();
    if t.is_empty() {
        "CleanSweep force removal".to_string()
    } else {
        t.to_string()
    }
}

fn powershell_path() -> Result<String, String> {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let p = format!(
        r"{}\System32\WindowsPowerShell\v1.0\powershell.exe",
        windir.trim_end_matches('\\')
    );
    // 必须用 Windows PowerShell：Checkpoint-Computer 在 PowerShell 7 里不存在。
    if std::path::Path::new(&p).is_file() {
        Ok(p)
    } else {
        Err("找不到 Windows PowerShell，无法创建还原点".into())
    }
}

fn run_with_timeout(mut cmd: Command) -> Result<(String, String), String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动系统还原命令：{}", e))?;

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("创建系统还原点超时（150 秒），已放弃".into());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("等待系统还原命令失败：{}", e)),
        }
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_end(&mut stdout);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_end(&mut stderr);
    }
    Ok((
        String::from_utf8_lossy(&stdout).to_string(),
        String::from_utf8_lossy(&stderr).to_string(),
    ))
}

/// 把 PowerShell 的原始错误翻成用户能照着做的提示。
fn humanize(msg: &str) -> String {
    let low = msg.to_ascii_lowercase();
    let text = if low.contains("access is denied")
        || low.contains("requires elevation")
        || msg.contains("拒绝访问")
        || msg.contains("需要提升")
    {
        "需要管理员权限（请以管理员身份运行 CleanSweep 后重试）".to_string()
    } else if low.contains("disabled")
        || low.contains("not enabled")
        || msg.contains("未启用")
        || msg.contains("被禁用")
    {
        "系统保护未开启，无法创建还原点（可在「系统属性 → 系统保护」中为系统盘开启）".to_string()
    } else if low.contains("frequency") {
        "24 小时内已创建过还原点，本次跳过".to_string()
    } else {
        msg.trim().to_string()
    };
    format!("未能创建系统还原点：{}", text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_shell_metacharacters() {
        let s = sanitize("Foo's App; Remove-Item C:\\ -Recurse $()");
        assert!(!s.contains('\''), "单引号必须被清掉: {s}");
        assert!(!s.contains(';'), "分号必须被清掉: {s}");
        assert!(!s.contains('$'), "变量符号必须被清掉: {s}");
        assert!(s.contains("Foo"));
    }

    #[test]
    fn sanitize_keeps_chinese_app_names() {
        assert_eq!(sanitize("夸克网盘"), "夸克网盘");
    }

    #[test]
    fn humanize_maps_common_failures() {
        assert!(humanize("Access is denied.").contains("管理员权限"));
        assert!(humanize("System Restore is disabled on this system.").contains("系统保护未开启"));
        assert!(humanize("some other error").contains("some other error"));
    }
}
