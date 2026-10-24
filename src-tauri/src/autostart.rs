//! 自启动项管理：扫描注册表 Run 键与计划任务，支持禁用（而非删除）。

use serde::{Deserialize, Serialize};

use crate::registry as reg;
use crate::safety;
use crate::types::ResidualItem;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartEntry {
    pub id: String,
    pub kind: String, // "registry" | "task"
    pub name: String,
    pub value: Option<String>,
    pub location: String,
    pub disabled: bool,
}

/// 只允许操作固定的 Run/RunOnce 键；写操作前会再次校验。
fn run_key_of(entry: &AutostartEntry) -> Result<(&str, &str), String> {
    let (root, path) = entry
        .location
        .split_once('\\')
        .ok_or_else(|| "位置格式错误".to_string())?;
    safety::validate_run_key_location(root, path)?;
    safety::validate_registry_value_name(&entry.name)?;
    Ok((root, path))
}

/// 禁用注册表自启动的备份位置。
///
/// 注意：不能把 Run 值改名成 `disabled_xxx` 留在原键里——Windows 会执行 Run 键下
/// 的**所有**值，改名并不会真正禁用。正确做法是把值移出 Run 键保存，恢复时再写回。
const BACKUP_KEY: &str = r"SOFTWARE\CleanSweep\DisabledStartup";

fn backup_root(root: &str) -> &'static windows_registry::Key {
    if root.eq_ignore_ascii_case("HKCU") {
        &windows_registry::CURRENT_USER
    } else {
        &windows_registry::LOCAL_MACHINE
    }
}

fn fnv32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

fn backup_value_name(location: &str, name: &str) -> String {
    format!("v{:08x}", fnv32(&format!("{}|{}", location, name)))
}

fn write_backup(location: &str, name: &str, value: &str) -> Result<(), String> {
    let (root, _) = location
        .split_once('\\')
        .ok_or_else(|| "位置格式错误".to_string())?;
    let key = backup_root(root)
        .create(BACKUP_KEY)
        .map_err(|e| e.to_string())?;
    let vn = backup_value_name(location, name);
    if key.get_string(&vn).is_ok() {
        return Err("已存在禁用备份，拒绝覆盖".into());
    }
    key.set_string(&vn, &format!("{}\n{}\n{}", location, name, value))
        .map_err(|e| e.to_string())
}

/// 遍历所有禁用备份，返回 (location, original name, value)。
fn list_backups() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for root in ["HKCU", "HKLM"] {
        let Ok(k) = backup_root(root).open(BACKUP_KEY) else {
            continue;
        };
        let Ok(values) = k.values() else { continue };
        for (_vn, v) in values {
            let data = value_to_string(&v);
            let mut parts = data.splitn(3, '\n');
            let (Some(loc), Some(name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let val = parts.next().unwrap_or("").to_string();
            out.push((loc.to_string(), name.to_string(), val));
        }
    }
    out
}

fn find_backup(location: &str, name: &str) -> Option<(String, String)> {
    let (root, _) = location.split_once('\\')?;
    let k = backup_root(root).open(BACKUP_KEY).ok()?;
    let vn = backup_value_name(location, name);
    let data = k.get_string(&vn).ok()?;
    let mut parts = data.splitn(3, '\n');
    let _loc = parts.next()?;
    let _name = parts.next()?;
    let value = parts.next().unwrap_or("").to_string();
    Some((vn, value))
}

fn delete_backup(location: &str, name: &str) -> bool {
    let Some((root, _)) = location.split_once('\\') else {
        return false;
    };
    let Ok(k) = backup_root(root).open(BACKUP_KEY) else {
        return false;
    };
    k.remove_value(&backup_value_name(location, name)).is_ok()
}

/// 扫描自启动项。可选过滤到与某应用相关的项。
pub fn scan_autostart(filter_name: Option<&str>) -> Vec<AutostartEntry> {
    let mut out = Vec::new();
    let lower = filter_name.map(|s| s.to_lowercase());

    let matches = |name: &str, value: &str| -> bool {
        match &lower {
            None => true,
            Some(l) => {
                let nn = name.to_lowercase();
                let vv = value.to_lowercase();
                nn.contains(l) || vv.contains(l) || l.contains(&nn)
            }
        }
    };

    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for &(root, path) in safety::RUN_KEYS {
        let key = match root {
            "HKCU" => windows_registry::CURRENT_USER,
            _ => windows_registry::LOCAL_MACHINE,
        };
        let k = match key.open(path) {
            Ok(k) => k,
            Err(_) => continue,
        };
        // 值列表
        let values = match k.values() {
            Ok(it) => it,
            Err(_) => continue,
        };
        for (name, value) in values {
            let val = value_to_string(&value);
            if matches(&name, &val) {
                seen.insert((format!("{}\\{}", root, path), name.clone()));
                out.push(AutostartEntry {
                    id: format!("reg:{}|{}", root, name),
                    kind: "registry".into(),
                    name: name.clone(),
                    value: Some(val.clone()),
                    location: format!("{}\\{}", root, path),
                    // 旧版 disabled_ 命名其实仍会被 Windows 执行，按“未禁用”展示；
                    // 新的禁用项已从 Run 键移出，以备份形式存在于下方。
                    disabled: false,
                });
            }
        }
    }

    // 真正被禁用（已移出 Run 键）的自启动项
    for (location, name, value) in list_backups() {
        if !matches(&name, &value) {
            continue;
        }
        let Some((root, path)) = location.split_once('\\') else {
            continue;
        };
        if safety::validate_run_key_location(root, path).is_err() {
            continue;
        }
        if !seen.insert((location.clone(), name.clone())) {
            continue;
        }
        out.push(AutostartEntry {
            id: format!("reg:{}|{}", root, name),
            kind: "registry".into(),
            name: name.clone(),
            value: Some(value),
            location,
            disabled: true,
        });
    }

    // 计划任务：用 PowerShell/JSON 列表（避免 schtasks 在中文 Windows 上的乱码）
    for t in crate::services::list_tasks() {
        if !matches(&t.name, "") {
            continue;
        }
        out.push(AutostartEntry {
            id: format!("task:{}", t.name),
            kind: "task".into(),
            name: t.name.clone(),
            value: None,
            location: "ScheduledTask".into(),
            disabled: false,
        });
    }

    out
}

/// 把注册表 Value（REG_SZ/REG_EXPAND_SZ，UTF-16LE）转成 String。
fn value_to_string(v: &windows_registry::Value) -> String {
    use windows_registry::Type;
    match v.ty() {
        Type::String | Type::ExpandString | Type::MultiString => {
            let wide = v.as_wide();
            let trimmed: Vec<u16> = wide.iter().copied().take_while(|&c| c != 0).collect();
            String::from_utf16_lossy(&trimmed)
        }
        _ => String::from_utf8_lossy(v.as_ref()).to_string(),
    }
}

/// 禁用一个自启动项。
/// - 注册表 Run：把值从 Run 键移出保存到 CleanSweep 备份键（Windows 会执行 Run 键下所有值，
///   仅改名是无效的）。
/// - 计划任务：schtasks /Change /TN <name> /DISABLE
pub fn disable(entry: &AutostartEntry) -> (bool, String) {
    if entry.kind == "registry" {
        let (root, path) = match run_key_of(entry) {
            Ok(v) => v,
            Err(e) => return (false, e),
        };
        if entry.name.starts_with("disabled_") {
            return (false, "检测到旧版 disabled_ 值，请直接删除该项".into());
        }
        let key = match root {
            "HKCU" => windows_registry::CURRENT_USER,
            _ => windows_registry::LOCAL_MACHINE,
        };
        let k = match key.open(path) {
            Ok(k) => k,
            Err(e) => return (false, e.to_string()),
        };
        // 忽略前端传来的 value：以注册表里的真实值为准。
        let val = match k.get_string(&entry.name) {
            Ok(v) => v,
            Err(_) => return (false, "该自启动项在注册表中不存在，已拒绝".into()),
        };
        if let Err(e) = write_backup(&entry.location, &entry.name, &val) {
            return (false, e);
        }
        if let Err(e) = k.remove_value(&entry.name) {
            delete_backup(&entry.location, &entry.name);
            return (false, e.to_string());
        }
        (true, format!("已禁用 {}（已从自启动键移出）", entry.name))
    } else if entry.kind == "task" {
        if let Err(e) = safety::validate_task_deletion(&entry.name) {
            return (false, e);
        }
        if !crate::services::task_exists(&entry.name) {
            return (false, "计划任务不存在或无法验证，已拒绝操作".into());
        }
        let o = safety::hidden_command(safety::system32_exe("schtasks.exe"))
            .args(["/Change", "/TN", &entry.name, "/DISABLE"])
            .output();
        match o {
            Ok(o) if o.status.success() => (true, format!("已禁用任务 {}", entry.name)),
            Ok(o) => (false, String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => (false, e.to_string()),
        }
    } else {
        (false, "未知类型".into())
    }
}

/// 启用一个自启动项（恢复）。
pub fn enable(entry: &AutostartEntry) -> (bool, String) {
    if entry.kind == "registry" {
        let (root, path) = match run_key_of(entry) {
            Ok(v) => v,
            Err(e) => return (false, e),
        };
        if entry.name.starts_with("disabled_") {
            return (false, "检测到旧版 disabled_ 值，请直接删除该项".into());
        }
        let key = match root {
            "HKCU" => windows_registry::CURRENT_USER,
            _ => windows_registry::LOCAL_MACHINE,
        };
        let k = match key.open(path) {
            Ok(k) => k,
            Err(e) => return (false, e.to_string()),
        };
        let (backup_vn, val) = match find_backup(&entry.location, &entry.name) {
            Some(v) => v,
            None => return (false, "找不到该自启动项的禁用备份".into()),
        };
        if let Err(e) = safety::validate_registry_value_name(&entry.name) {
            return (false, e);
        }
        if let Err(e) = k.set_string(&entry.name, &val) {
            return (false, e.to_string());
        }
        // 删除备份（直接按备份值名删除，避免重复 hash）。
        if let Some((root2, _)) = entry.location.split_once('\\') {
            if let Ok(bk) = backup_root(root2).open(BACKUP_KEY) {
                let _ = bk.remove_value(&backup_vn);
            }
        }
        (true, format!("已启用 {}", entry.name))
    } else if entry.kind == "task" {
        if let Err(e) = safety::validate_task_deletion(&entry.name) {
            return (false, e);
        }
        if !crate::services::task_exists(&entry.name) {
            return (false, "计划任务不存在或无法验证，已拒绝操作".into());
        }
        let o = safety::hidden_command(safety::system32_exe("schtasks.exe"))
            .args(["/Change", "/TN", &entry.name, "/ENABLE"])
            .output();
        match o {
            Ok(o) if o.status.success() => (true, format!("已启用任务 {}", entry.name)),
            Ok(o) => (false, String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => (false, e.to_string()),
        }
    } else {
        (false, "未知类型".into())
    }
}

/// 删除一个自启动项（与残留删除不同，这里不备份）。
pub fn remove(entry: &AutostartEntry) -> (bool, String) {
    if entry.kind == "registry" {
        let (root, path) = match run_key_of(entry) {
            Ok(v) => v,
            Err(e) => return (false, e),
        };
        let mut removed = false;
        if reg::delete_value_in_key(root, path, &entry.name).is_ok() {
            removed = true;
        }
        // 已禁用项：值在备份键里，一并删除。
        if delete_backup(&entry.location, &entry.name) {
            removed = true;
        }
        if removed {
            (true, format!("已删除 {}", entry.name))
        } else {
            (false, "未找到该自启动项".into())
        }
    } else if entry.kind == "task" {
        if let Err(e) = safety::validate_task_deletion(&entry.name) {
            return (false, e);
        }
        crate::services::delete(&ResidualItem {
            id: format!("task:{}", entry.name),
            kind: crate::types::ResidualKind::ScheduledTask,
            path: entry.name.clone(),
            display: entry.name.clone(),
            detail: None,
            size_bytes: None,
            in_use: false,
            ai_verdict: None,
        })
    } else {
        (false, "未知类型".into())
    }
}
