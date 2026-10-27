//! Windows 防火墙规则管理：为指定 exe 添加/移除联网阻止规则。
//! 使用 netsh advfirewall firewall（兼容性好，无需 COM）。

use serde::{Deserialize, Serialize};

use crate::safety;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirewallRule {
    pub name: String,
    pub exe_path: String,
    pub direction_in: bool,
    pub direction_out: bool,
    pub enabled: bool,
}

/// PowerShell `Get-NetFirewallRule` 的输出（PascalCase 属性）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsRule {
    #[serde(default)]
    name: String,
    #[serde(default)]
    program: Option<String>,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

const RULE_PREFIX: &str = "CleanSweep_Block_";

/// 比较 exe 路径时的归一化形式：去空白、统一反斜杠、忽略大小写。
fn norm_exe(path: &str) -> String {
    path.trim().replace('/', "\\").to_ascii_lowercase()
}

/// FNV-1a 64，用于把完整 exe 路径映射成短后缀，避免不同目录下同名 exe 的规则互相覆盖。
fn path_hash(path: &str) -> String {
    let normalized = norm_exe(path);
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in normalized.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash >> 32) as u32 ^ (hash as u32))
}

fn rule_name(exe: &str) -> String {
    // 取文件名（不含扩展）作为规则名一部分；只保留安全字符，避免名称里的
    // 空格/引号等被 netsh 自己的分词器重新解释。路径哈希防止同名 exe 冲突。
    let stem = std::path::Path::new(exe)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".into());
    let mut safe: String = stem
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    safe.truncate(40);
    if safe.is_empty() {
        safe.push_str("app");
    }
    format!("{}{}_{}", RULE_PREFIX, safe, path_hash(exe))
}

/// 判断是否已有该 exe 的阻止规则。
pub fn is_blocked(exe: &str) -> bool {
    let name = rule_name(exe);
    rule_exists(&name)
}

/// 用 PowerShell 查询规则是否存在。
/// 不用 `netsh ... show rule` 是因为其输出在中文 Windows 上被本地化，
/// 解析 "Rule Name" 会永远失败。
fn ps_rule_script(body: &str) -> Option<String> {
    let script = format!("$ErrorActionPreference='SilentlyContinue'\n{}\n", body);
    let out = safety::powershell_command(&script).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn rule_exists(name: &str) -> bool {
    // name 由 rule_name() 生成，只含 ASCII 字母数字/下划线/连字符，可安全内嵌单引号串。
    let body = format!(
        "$r = Get-NetFirewallRule -DisplayName '{}' -ErrorAction SilentlyContinue\nif ($r) {{ 'YES' }} else {{ 'NO' }}",
        name
    );
    ps_rule_script(&body)
        .map(|s| s.contains("YES"))
        .unwrap_or(false)
}

/// 读取本工具创建的所有防火墙规则（合并同名 In/Out 两条规则）。
pub fn list_managed_rules() -> Vec<FirewallRule> {
    // Windows PowerShell 5.1 不支持 ConvertTo-Json -AsArray，必须用 -InputObject 包数组。
    let body = r#"$items = @(Get-NetFirewallRule -DisplayName 'CleanSweep_Block_*' | ForEach-Object {
  $app = $_ | Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue
  [PSCustomObject]@{
    Name = $_.DisplayName
    Program = $app.Program
    Direction = [string]$_.Direction
    Enabled = [bool]$_.Enabled
  }
})
ConvertTo-Json -InputObject $items -Depth 3"#;
    let text = match ps_rule_script(body) {
        Some(t) if !t.is_empty() => t,
        _ => return vec![],
    };
    let parsed: Vec<PsRule> = if text.starts_with('[') {
        serde_json::from_str(&text).unwrap_or_default()
    } else {
        serde_json::from_str(&format!("[{}]", text)).unwrap_or_default()
    };
    let mut merged: Vec<FirewallRule> = Vec::new();
    for r in parsed {
        if r.name.is_empty() || !r.name.starts_with(RULE_PREFIX) {
            continue;
        }
        let dir = r.direction.unwrap_or_default().to_ascii_lowercase();
        let in_dir = dir.contains("in");
        let out_dir = dir.contains("out");
        let program = r.program.unwrap_or_default();
        if let Some(existing) = merged
            .iter_mut()
            .find(|m| m.name == r.name && m.exe_path == program)
        {
            existing.direction_in |= in_dir;
            existing.direction_out |= out_dir;
            existing.enabled = existing.enabled || r.enabled.unwrap_or(true);
        } else {
            merged.push(FirewallRule {
                name: r.name,
                exe_path: program,
                direction_in: in_dir,
                direction_out: out_dir,
                enabled: r.enabled.unwrap_or(true),
            });
        }
    }
    merged
}

/// 为指定 exe 添加出站+入站阻止规则。返回 (success, message)。
pub fn block(exe: &str) -> (bool, String) {
    if exe.is_empty() {
        return (false, "缺少可执行文件路径".into());
    }
    let name = rule_name(exe);
    // 先移除已有（同名的，以及旧版本命名/路径写法不同但程序路径相同的规则）
    let _ = remove_rule(&name);
    if let Some(old) = find_rule_name_by_exe(exe) {
        let _ = remove_rule(&old);
    }
    let mut ok = true;
    let mut msgs = Vec::new();
    for (dir, _) in [("in", true), ("out", false)] {
        let o = safety::hidden_command(safety::system32_exe("netsh.exe"))
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={}", name),
                &format!("dir={}", dir),
                "action=block",
                &format!("program=\"{}\"", exe),
                "enable=yes",
                "profile=any",
            ])
            .output();
        match o {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                ok = false;
                msgs.push(format!(
                    "{}: {}",
                    dir,
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            Err(e) => {
                ok = false;
                msgs.push(format!("{}: {}", dir, e));
            }
        }
    }
    if ok {
        (true, format!("已禁止 {} 联网", exe))
    } else {
        (false, msgs.join("; "))
    }
}

/// 移除阻止规则，恢复联网。
pub fn unblock(exe: &str) -> (bool, String) {
    let name = match resolve_rule_name(exe) {
        Some(n) => n,
        None => return (false, "未找到对应规则".into()),
    };
    match remove_rule(&name) {
        Ok(()) => (true, format!("已恢复 {} 联网", exe)),
        Err(e) => (false, e),
    }
}

/// 找该 exe 对应的规则名：先按路径哈希算出的名字查；查不到再按规则里记录的
/// 程序路径兜底（例如恢复时拿到的路径大小写/分隔符与建规则时不同）。
fn resolve_rule_name(exe: &str) -> Option<String> {
    let computed = rule_name(exe);
    if rule_exists(&computed) {
        return Some(computed);
    }
    find_rule_name_by_exe(exe)
}

fn find_rule_name_by_exe(exe: &str) -> Option<String> {
    let target = norm_exe(exe);
    if target.is_empty() {
        return None;
    }
    list_managed_rules()
        .into_iter()
        .find(|r| norm_exe(&r.exe_path) == target)
        .map(|r| r.name)
}

/// netsh 的 `delete rule` 参数。
///
/// 必须把 `rule` 和 `name=<规则名>` 分成两个参数：拼成 `"rule name=<规则名>"`
/// 一个参数时，std::process 会因为里面含空格而给整串加引号，netsh 的分词器
/// 不认这种写法，只会回一句 "The following command was not found"，删除静默失败。
fn delete_args(name: &str) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "delete".into(),
        "rule".into(),
        format!("name={}", name),
    ]
}

/// 删除规则。返回真实失败原因，不要用「未找到对应规则」掩盖提权/权限问题。
fn remove_rule(name: &str) -> Result<(), String> {
    let out = safety::hidden_command(safety::system32_exe("netsh.exe"))
        .args(delete_args(name))
        .output()
        .map_err(|e| format!("调用 netsh 失败：{}", e))?;
    // 不用 netsh 的本地化输出判断成功，改用 PowerShell 复查规则是否已消失。
    if !rule_exists(name) {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let raw = if stdout.is_empty() { stderr } else { stdout };
    Err(netsh_error(&raw))
}

/// netsh 的报错是本地化的，这里翻译成能照着做的提示。
fn netsh_error(raw: &str) -> String {
    let low = raw.to_ascii_lowercase();
    if low.contains("requires elevation")
        || raw.contains("需要提升")
        || low.contains("access is denied")
        || raw.contains("拒绝访问")
    {
        return "需要管理员权限：请以管理员身份重启 CleanSweep 后再恢复联网".into();
    }
    if raw.is_empty() {
        "移除防火墙规则失败（netsh 没有输出错误信息）".into()
    } else {
        format!("移除防火墙规则失败：{}", raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_names_are_safe_and_unique_per_path() {
        let a = rule_name(r"C:\Program Files\A\app.exe");
        let b = rule_name(r"C:\Program Files\B\app.exe");
        assert_ne!(a, b, "不同路径的同名 exe 不能共用规则名");
        assert_eq!(
            a,
            rule_name(r"c:/program files/a/APP.EXE"),
            "大小写/斜杠归一化后应一致"
        );
        assert!(a.starts_with(RULE_PREFIX));
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert!(a.len() < 120);
    }

    /// netsh 只认 `delete rule name=X` 这种分词：任何一个参数里带空格，
    /// std::process 就会给整串加引号，netsh 直接报「命令不存在」。
    #[test]
    fn delete_args_are_split_and_never_requoted() {
        let args = delete_args("CleanSweep_Block_app_1234abcd");
        assert_eq!(
            args,
            [
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                "name=CleanSweep_Block_app_1234abcd"
            ]
        );
        assert!(
            args.iter().all(|a| !a.chars().any(char::is_whitespace)),
            "带空格的参数会被引号包住，netsh 无法解析"
        );
    }

    #[test]
    fn same_exe_written_differently_maps_to_one_key() {
        let a = norm_exe(r"D:\software\youku\uninstaller.exe");
        let b = norm_exe(r" d:/SOFTWARE/Youku/Uninstaller.EXE ");
        assert_eq!(a, b);
        assert_eq!(path_hash(&a), path_hash(&b));
    }

    #[test]
    fn netsh_errors_are_humanized() {
        assert!(
            netsh_error("The requested operation requires elevation (Run as administrator).")
                .contains("管理员权限")
        );
        assert!(netsh_error("请求的操作需要提升(作为管理员运行)。").contains("管理员权限"));
        assert!(netsh_error("No rules match the specified criteria.").contains("No rules"));
    }
}
