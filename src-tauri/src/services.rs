//! Windows 服务与计划任务（Scheduled Task）的扫描与删除。
//!
//! 安全约束：
//! - 系统关键服务、二进制位于 Windows 系统目录的服务，永远不会被扫描为残留；
//! - `\Microsoft\...` 下的 Windows 内置计划任务永远不作为残留，也永远不允许删除；
//! - 删除前 `safety` 模块会再次校验，并重新查询服务/任务确认其真实身份。

use std::process::Command;

use serde::Deserialize;

use crate::safety;
use crate::types::ResidualItem;

#[derive(Debug)]
pub struct ServiceInfo {
    pub name: String,
    pub display_name: String,
    pub binary_path: Option<String>,
}

fn sc_exe() -> String {
    safety::system32_exe("sc.exe")
}

fn schtasks_exe() -> String {
    safety::system32_exe("schtasks.exe")
}

/// 展开 `%VAR%` 形式的环境变量（服务 ImagePath 里常见）。
fn expand_env_vars(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => {
                        out.push_str(&v);
                        rest = &after[end + 1..];
                    }
                    Err(_) => {
                        out.push('%');
                        rest = after;
                    }
                }
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 规范化服务 ImagePath：展开环境变量、去掉 `\??\` 前缀和引号。
fn normalize_image_path(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.starts_with(r"\??\") {
        s = s[4..].to_string();
    }
    let s = expand_env_vars(&s);
    s.trim().to_string()
}

/// 列出所有服务及其可执行路径。
///
/// 直接读注册表 `HKLM\SYSTEM\CurrentControlSet\Services`（比每个服务都调用
/// `sc qc` 快两个数量级），返回的 ImagePath 会展开环境变量并去掉 `\??\` 前缀。
pub fn list_services() -> Vec<ServiceInfo> {
    let mut out = Vec::new();
    let base = match windows_registry::LOCAL_MACHINE.open(r"SYSTEM\CurrentControlSet\Services")
    {
        Ok(k) => k,
        Err(_) => return out,
    };
    let keys = match base.keys() {
        Ok(it) => it,
        Err(_) => return out,
    };
    for name in keys {
        let k = match base.open(&name) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let display = k
            .get_string("DisplayName")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.clone());
        let bin = k
            .get_string("ImagePath")
            .ok()
            .map(|s| normalize_image_path(&s))
            .filter(|s| !s.trim().is_empty());
        out.push(ServiceInfo {
            name,
            display_name: display,
            binary_path: bin,
        });
    }
    out
}

/// 删除一个服务。返回 (success, message)。
/// 删除前从注册表重新读取真实 ImagePath，并拒绝系统关键服务 / Windows 目录下的服务。
pub fn delete_service(name: &str) -> (bool, String) {
    let name = name.trim();
    if let Err(e) = safety::validate_service_name(name) {
        return (false, e);
    }
    let base = match windows_registry::LOCAL_MACHINE.open(r"SYSTEM\CurrentControlSet\Services")
    {
        Ok(k) => k,
        Err(e) => return (false, format!("无法读取服务注册表：{}", e)),
    };
    let k = match base.open(name) {
        Ok(k) => k,
        Err(_) => return (false, "服务不存在或无法验证，已拒绝删除".into()),
    };
    let bin = k
        .get_string("ImagePath")
        .ok()
        .map(|s| normalize_image_path(&s))
        .filter(|s| !s.trim().is_empty());
    if let Err(e) = safety::validate_service_deletion(name, bin.as_deref()) {
        return (false, format!("安全校验拒绝删除服务：{}", e));
    }
    // 停服务失败不阻塞删除（系统服务已被上面的校验拦下）
    let _ = Command::new(sc_exe()).args(["stop", name]).output();
    let o = match Command::new(sc_exe()).args(["delete", name]).output() {
        Ok(o) => o,
        Err(e) => return (false, e.to_string()),
    };
    if o.status.success() {
        (true, "已删除服务".into())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        let out = String::from_utf8_lossy(&o.stdout);
        let msg = if err.trim().is_empty() {
            out.to_string()
        } else {
            err.to_string()
        };
        (false, msg.trim().to_string())
    }
}

#[derive(Debug)]
pub struct TaskInfo {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsTask {
    #[serde(default)]
    task_name: Option<String>,
    #[serde(default)]
    task_path: Option<String>,
}

/// 列出计划任务（递归）。返回完整任务路径（TaskPath + TaskName）。
///
/// 用 PowerShell `Get-ScheduledTask` + JSON，而不是解析 `schtasks` 的 CSV：
/// 后者在中文 Windows 上按 OEM 代码页输出，非 ASCII 任务名会变成乱码。
pub fn list_tasks() -> Vec<TaskInfo> {
    let script = r#"$items = @(Get-ScheduledTask | ForEach-Object {
  [PSCustomObject]@{ TaskName = $_.TaskName; TaskPath = $_.TaskPath }
})
ConvertTo-Json -InputObject $items -Depth 3"#;
    let o = Command::new(safety::powershell_exe())
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output();
    let stdout = match o {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return vec![],
    };
    if stdout.is_empty() {
        return vec![];
    }
    let parsed: Vec<PsTask> = if stdout.starts_with('[') {
        serde_json::from_str(&stdout).unwrap_or_default()
    } else {
        serde_json::from_str(&format!("[{}]", stdout)).unwrap_or_default()
    };
    let mut out = Vec::new();
    for t in parsed {
        let name = match t.task_name {
            Some(n) if !n.trim().is_empty() => n,
            _ => continue,
        };
        let path = t.task_path.unwrap_or_else(|| "\\".into());
        let path = if path.ends_with('\\') {
            path
        } else {
            format!("{}\\", path)
        };
        let full = format!("{}{}", path, name);
        out.push(TaskInfo {
            name: full.clone(),
            path: full,
        });
    }
    out
}

/// 任务是否真实存在（删除前二次确认）。
pub fn task_exists(name: &str) -> bool {
    Command::new(schtasks_exe())
        .args(["/Query", "/TN", name, "/FO", "CSV", "/NH"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 删除计划任务。只允许非 Windows 内置任务，且必须出现在完整任务列表里。
pub fn delete_task(name: &str) -> (bool, String) {
    if let Err(e) = safety::validate_task_deletion(name) {
        return (false, format!("安全校验拒绝删除计划任务：{}", e));
    }
    let belongs = list_tasks()
        .iter()
        .any(|t| t.name.eq_ignore_ascii_case(name));
    if !belongs {
        return (false, "计划任务不存在或不在系统任务列表中，已拒绝删除".into());
    }
    let o = match Command::new(schtasks_exe())
        .args(["/Delete", "/TN", name, "/F"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return (false, e.to_string()),
    };
    if o.status.success() {
        (true, "已删除计划任务".into())
    } else {
        let e = String::from_utf8_lossy(&o.stderr);
        let s = String::from_utf8_lossy(&o.stdout);
        let msg = if e.trim().is_empty() { s } else { e };
        (false, msg.trim().to_string())
    }
}

/// 匹配服务/任务：判断其名称或二进制路径是否与该应用相关。
fn matches_app(app_name: &str, binary_path: Option<&str>, display: &str, name: &str) -> bool {
    let an = norm(app_name);
    if an.len() < 2 {
        return false;
    }
    for t in [display, name] {
        let tn = norm(t);
        if tn.is_empty() {
            continue;
        }
        if tn == an || tn.contains(&an) || (tn.len() >= 3 && an.contains(&tn)) {
            return true;
        }
    }
    if an.len() >= 3 {
        if let Some(b) = binary_path {
            let bn = norm(b);
            if !bn.is_empty() && bn.contains(&an) {
                return true;
            }
        }
    }
    false
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace([' ', '-', '_', '.'], "")
}

/// 扫描与该应用相关的服务和计划任务，返回残留项。
pub fn scan_for_app(app_name: &str) -> Vec<ResidualItem> {
    use crate::types::{ResidualKind, ResidualItem};
    let mut out = Vec::new();

    for s in list_services() {
        // 关键服务 / 二进制在 Windows 目录 / 无法验证路径 → 一律不作为残留
        if safety::validate_service_deletion(&s.name, s.binary_path.as_deref()).is_err() {
            continue;
        }
        if matches_app(app_name, s.binary_path.as_deref(), &s.display_name, &s.name) {
            let path = s.binary_path.clone().unwrap_or_else(|| s.name.clone());
            out.push(ResidualItem {
                id: format!("svc:{}", s.name),
                kind: ResidualKind::Service,
                path,
                display: format!("服务: {} ({})", s.display_name, s.name),
                detail: s.binary_path.clone(),
                size_bytes: None,
                in_use: false,
                ai_verdict: None,
            });
        }
    }

    for t in list_tasks() {
        // Windows 内置任务（\Microsoft\...）永不作为残留
        if safety::validate_task_deletion(&t.name).is_err() {
            continue;
        }
        if matches_app(app_name, None, &t.name, &t.name) {
            out.push(ResidualItem {
                id: format!("task:{}", t.name),
                kind: ResidualKind::ScheduledTask,
                path: t.path.clone(),
                display: format!("计划任务: {}", t.name),
                detail: None,
                size_bytes: None,
                in_use: false,
                ai_verdict: None,
            });
        }
    }

    out
}

/// 删除一个服务或计划任务残留。
pub fn delete(item: &ResidualItem) -> (bool, String) {
    match item.kind {
        crate::types::ResidualKind::Service => delete_service(&extract_id(item)),
        crate::types::ResidualKind::ScheduledTask => delete_task(&extract_id(item)),
        _ => (false, "非服务/任务类型".into()),
    }
}

fn extract_id(item: &ResidualItem) -> String {
    // id 形如 "svc:MyService" / "task:\My Task"
    item.id.splitn(2, ':').nth(1).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_matching_avoids_empty_tokens() {
        // 空显示名不能导致“匹配一切”
        assert!(!matches_app("Some App", None, "", ""));
        assert!(!matches_app("", None, "SomeAppSvc", "SomeAppSvc"));
        assert!(matches_app("Some App", None, "", "SomeAppSvc"));
        assert!(matches_app("Chrome", None, "Google Chrome Elevation", "GoogleChromeElevationService"));
    }

    #[test]
    fn extract_id_keeps_task_path() {
        let item = ResidualItem {
            id: r"task:\MyApp\Updater".into(),
            kind: crate::types::ResidualKind::ScheduledTask,
            path: String::new(),
            display: String::new(),
            detail: None,
            size_bytes: None,
            in_use: false,
            ai_verdict: None,
        };
        assert_eq!(extract_id(&item), r"\MyApp\Updater");
    }
}
