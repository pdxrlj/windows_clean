//! 枚举 Store / MSIX / UWP 应用，通过 PowerShell 调用。

#![allow(non_snake_case)]

use serde::Deserialize;

use crate::types::{AppSource, InstalledApp};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AppxPkg {
    Name: Option<String>,
    PackageFullName: Option<String>,
    PackageFamilyName: Option<String>,
    Publisher: Option<String>,
    Version: Option<String>,
    InstallLocation: Option<String>,
    NonRemovable: Option<bool>,
    SignatureKind: Option<String>,
    IsFramework: Option<bool>,
}

/// 列举当前用户的 Appx 包。
pub fn enumerate_store_apps() -> Vec<InstalledApp> {
    let script = r#"
$ErrorActionPreference='SilentlyContinue'
$items = @(Get-AppxPackage | Sort-Object Name | ForEach-Object {
    [PSCustomObject]@{
        Name=$_.Name
        PackageFullName=$_.PackageFullName
        PackageFamilyName=$_.PackageFamilyName
        Publisher=$_.Publisher
        Version=[string]$_.Version
        InstallLocation=$_.InstallLocation
        NonRemovable=[bool]$_.NonRemovable
        SignatureKind=[string]$_.SignatureKind
        IsFramework=[bool]$_.IsFramework
    }
})
ConvertTo-Json -InputObject $items -Depth 4
"#;
    let out = crate::safety::powershell_command(script).output();
    let mut apps = Vec::new();
    if let Ok(out) = out {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let pkgs: Vec<AppxPkg> = if stdout.starts_with('[') {
                serde_json::from_str(&stdout).unwrap_or_default()
            } else if stdout.is_empty() {
                vec![]
            } else {
                serde_json::from_str(&format!("[{}]", stdout)).unwrap_or_default()
            };
            for p in pkgs {
                let name = match p.Name {
                    Some(n) if !n.is_empty() => n,
                    _ => continue,
                };
                let full = p.PackageFullName.unwrap_or_default();
                let protection = crate::safety::classify_store_package(
                    &name,
                    p.NonRemovable.unwrap_or(false),
                    p.SignatureKind.as_deref().unwrap_or(""),
                    p.IsFramework.unwrap_or(false),
                );
                let (is_protected, protected_reason) = match protection {
                    Some(reason) => (true, Some(reason)),
                    None => (false, None),
                };
                apps.push(InstalledApp {
                    id: format!("store::{}", full),
                    display_name: name,
                    publisher: clean_publisher(p.Publisher.as_deref()),
                    version: p.Version,
                    install_location: p.InstallLocation,
                    install_date: None,
                    // Remove-AppxPackage -Package <fullname>
                    uninstall_string: Some(format!("Remove-AppxPackage -Package '{}'", full)),
                    quiet_uninstall_string: Some(format!("Remove-AppxPackage -Package '{}'", full)),
                    display_icon: None,
                    estimated_size_kb: None,
                    source: AppSource::Store,
                    registry_key: None,
                    is_system_component: is_protected,
                    is_protected,
                    protected_reason,
                    package_family: p.PackageFamilyName,
                });
            }
        }
    }
    apps
}

fn clean_publisher(p: Option<&str>) -> Option<String> {
    p.map(|s| {
        // CN=Microsoft ... -> Microsoft
        if let Some(idx) = s.find('=') {
            let after = &s[idx + 1..];
            after.split(',').next().unwrap_or(after).to_string()
        } else {
            s.to_string()
        }
    })
}

/// 卸载一个 Appx 包（仅当前用户；系统包/框架包会被拒绝）。
pub fn remove_appx(package_full_name: &str) -> Result<(), String> {
    let name = package_full_name.trim();
    crate::safety::validate_appx_package_name(name)?;

    // 包名已通过字符白名单校验，可安全嵌入单引号字符串。
    // 执行前再次从系统查询该包状态，双重确认它不是系统/框架/不可移除包。
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$p = Get-AppxPackage | Where-Object {{ $_.PackageFullName -eq '{name}' }}
if ($null -eq $p) {{ exit 3 }}
if ($p.NonRemovable -or $p.SignatureKind -eq 'System' -or $p.IsFramework) {{ exit 4 }}
Remove-AppxPackage -Package $p.PackageFullName
"#,
        name = name
    );
    let out = crate::safety::powershell_command(&script)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    match out.status.code() {
        Some(3) => Err("未找到该 Store 包，可能已被卸载。".into()),
        Some(4) => Err("该 Store 包属于系统/框架/不可移除包，已拒绝卸载。".into()),
        _ => Err(String::from_utf8_lossy(&out.stderr).to_string()),
    }
}
