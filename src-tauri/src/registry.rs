//! 注册表读取：枚举已安装软件的 Uninstall 键。

use std::collections::HashSet;
use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
    HKEY_CURRENT_USER, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_READ, REG_SAM_FLAGS,
    REG_VALUE_TYPE,
};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

use crate::types::{AppSource, InstalledApp};

const UNINSTALL_PATHS: &[(AppSource, &str)] = &[
    (AppSource::RegistryMachine64, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
    (AppSource::RegistryMachine32, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
    (AppSource::RegistryUser, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
];

/// 枚举所有注册表中的卸载条目。
pub fn enumerate_registry_apps() -> Vec<InstalledApp> {
    let mut out: Vec<InstalledApp> = Vec::new();
    // 按 id（source + 子键名）去重：不再用“显示名+发布者”去重，
    // 否则同名软件的并行版本 / HKCU 与 HKLM 两份安装会被静默隐藏。
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (source, path) in UNINSTALL_PATHS {
        let root = match source {
            AppSource::RegistryUser => HKEY_CURRENT_USER,
            _ => HKEY_LOCAL_MACHINE,
        };
        if let Ok(subkeys) = enum_subkeys(root, path) {
            for subkey in subkeys {
                let full = format!("{}\\{}", path, subkey);
                if let Some(app) = read_app(root, &full, *source) {
                    if app.display_name.trim().is_empty() || !seen_ids.insert(app.id.clone()) {
                        continue;
                    }
                    out.push(app);
                }
            }
        }
    }

    out
}

/// 展开 `%VAR%` 形式的环境变量（注册表里 InstallLocation / UninstallString 常见）。
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

fn read_app(root: HKEY, full_path: &str, source: AppSource) -> Option<InstalledApp> {
    let hkey = open_key(root, full_path, KEY_READ).ok()?;
    let get = |name: &str| -> Option<String> {
        query_value(hkey, name)
            .map(|(s, _)| expand_env_vars(&s))
            .filter(|s| !s.trim().is_empty())
    };

    let display_name = get("DisplayName")?;
    if display_name.trim().is_empty() {
        return None;
    }

    let install_location = get("InstallLocation");
    let uninstall_string = get("UninstallString");
    let quiet_uninstall = get("QuietUninstallString");
    let display_icon = get("DisplayIcon");
    let publisher = get("Publisher");
    let version = get("DisplayVersion");
    let install_date = get("InstallDate");

    let is_system_component = query_dword(hkey, "SystemComponent") == 1
        // Windows 更新 / 驱动包通常会带 ParentKeyName / ReleaseType
        || query_value(hkey, "ParentKeyName").is_some()
        || query_value(hkey, "ReleaseType").is_some();
    let estimated_size_kb = {
        let v = query_dword(hkey, "EstimatedSize");
        if v == 0 { None } else { Some(v as u64) }
    };

    unsafe { let _ = RegCloseKey(hkey); }

    let subkey = subkey_name(full_path);
    let id = format!("{:?}::{}", source, subkey);
    let protection = crate::safety::classify_registry_app(
        &display_name,
        publisher.as_deref(),
        Some(&subkey),
        is_system_component,
    );
    let (is_protected, protected_reason) = match protection {
        Some(reason) => (true, Some(reason)),
        None => (false, None),
    };

    Some(InstalledApp {
        id,
        display_name,
        publisher,
        version,
        install_location,
        install_date,
        uninstall_string,
        quiet_uninstall_string: quiet_uninstall,
        display_icon,
        estimated_size_kb,
        source,
        registry_key: Some(full_path.to_string()),
        is_system_component,
        is_protected,
        protected_reason,
        package_family: None,
    })
}

fn subkey_name(full: &str) -> String {
    full.rsplit('\\').next().unwrap_or(full).to_string()
}

fn open_key(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> Result<HKEY, String> {
    let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut handle = HKEY::default();
    unsafe {
        let status = RegOpenKeyExW(root, PCWSTR(path_wide.as_ptr()), None, access, &mut handle);
        if status.is_ok() {
            Ok(handle)
        } else {
            Err(format!("RegOpenKeyExW failed for {}: 0x{:X}", path, status.0))
        }
    }
}

fn query_value(hkey: HKEY, name: &str) -> Option<(String, u32)> {
    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let mut len: u32 = 0;
    // 先探测长度：超长值不会被 4096 字符缓冲静默截断。
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut ty as *mut REG_VALUE_TYPE),
            None,
            Some(&mut len),
        )
    };
    // ERROR_MORE_DATA == 234：需要缓冲区，len 此时为所需字节数。
    if !status.is_ok() && status.0 != 234 {
        return None;
    }
    if len == 0 {
        return None;
    }
    // 只接受字符串类型（REG_SZ / REG_EXPAND_SZ / REG_MULTI_SZ），
    // 避免把 DWORD 等二进制值当 UTF-16 文本解码。
    if !matches!(ty.0, 1 | 2 | 7) {
        return None;
    }
    let mut buf = vec![0u16; ((len as usize) / 2).max(1)];
    let mut got_len = (buf.len() * 2) as u32;
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut ty as *mut REG_VALUE_TYPE),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut got_len),
        )
    };
    if !status.is_ok() {
        return None;
    }
    let mut chars: Vec<u16> = buf[..(got_len as usize / 2)].to_vec();
    while chars.last() == Some(&0) {
        chars.pop();
    }
    if chars.starts_with(&[0xFEFF]) {
        chars.remove(0);
    }
    let s = String::from_utf16_lossy(&chars);
    Some((s.trim_end_matches('\0').to_string(), ty.0))
}

fn query_dword(hkey: HKEY, name: &str) -> u32 {
    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut val: u32 = 0;
    let mut len = 4u32;
    let mut ty: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    unsafe {
        let status = RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut ty as *mut REG_VALUE_TYPE),
            Some(&mut val as *mut u32 as *mut u8),
            Some(&mut len),
        );
        if status.is_ok() && ty.0 == 4 {
            val
        } else {
            0
        }
    }
}

fn enum_subkeys(root: HKEY, path: &str) -> Result<Vec<String>, String> {
    let hkey = open_key(
        root,
        path,
        KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE | KEY_READ,
    )?;
    let mut result = Vec::new();
    let mut buf = [0u16; 512];
    let mut index: u32 = 0;
    loop {
        unsafe {
            let status = RegEnumKeyW(hkey, index, Some(&mut buf));
            if status.is_ok() {
                // 找到结尾的 NUL
                let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                let s = String::from_utf16_lossy(&buf[..n]);
                if !s.is_empty() {
                    result.push(s);
                }
                index += 1;
            } else {
                break;
            }
        }
    }
    unsafe { let _ = RegCloseKey(hkey); }
    Ok(result)
}

/// 删除一个注册表键（递归）。仅允许 SOFTWARE 下的应用专属键，系统子树一律拒绝。
pub fn delete_registry_key(root_root: &str, path: &str) -> Result<(), String> {
    crate::safety::validate_registry_delete_path(root_root, path)?;
    let key = match root_root {
        "HKCU" => CURRENT_USER,
        "HKLM" => LOCAL_MACHINE,
        _ => return Err(format!("unsupported root: {}", root_root)),
    };
    let (parent_path, subkey) = match path.rsplit_once('\\') {
        Some((p, s)) => (p, s),
        None => return Err("invalid path".into()),
    };
    let k = key.create(parent_path).map_err(|e| e.to_string())?;
    k.remove_tree(subkey).map_err(|e| e.to_string())?;
    Ok(())
}

/// 取键值，返回字符串。
pub fn get_value(root: &str, path: &str, name: &str) -> Option<String> {
    let key = match root {
        "HKCU" => CURRENT_USER,
        "HKLM" => LOCAL_MACHINE,
        _ => return None,
    };
    let k = key.open(path).ok()?;
    k.get_string(name).ok()
}

/// 删除某个键下的某个值。仅允许 Windows Run/RunOnce 自启动键（自启动管理专用）。
pub fn delete_value_in_key(root: &str, path: &str, name: &str) -> Result<(), String> {
    crate::safety::validate_run_key_location(root, path)?;
    crate::safety::validate_registry_value_name(name)?;
    let key = match root {
        "HKCU" => CURRENT_USER,
        "HKLM" => LOCAL_MACHINE,
        _ => return Err(format!("bad root: {}", root)),
    };
    let k = key.open(path).map_err(|e| e.to_string())?;
    k.remove_value(name).map_err(|e| e.to_string())
}

/// 取子键列表。
pub fn list_subkeys(root: &str, path: &str) -> Vec<String> {
    let key = match root {
        "HKCU" => CURRENT_USER,
        "HKLM" => LOCAL_MACHINE,
        _ => return vec![],
    };
    match key.open(path) {
        Ok(k) => match k.keys() {
            Ok(it) => it.collect(),
            Err(_) => vec![],
        },
        Err(_) => vec![],
    }
}
