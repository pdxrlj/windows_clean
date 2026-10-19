//! Store/UWP 应用的 Capability（摄像头/麦克风/位置/网络等）限制。
//!
//! UWP 应用的每个 capability 权限存储在：
//!   HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\Capabilities\ConsentStore\<Capability>\<PackageFamilyName>
//! Value "Value" = "Allow" / "Deny" / "Prompt"。
//! 把 Value 改为 "Deny" 即可拒绝该应用使用该 capability。

use serde::{Deserialize, Serialize};

use crate::registry as reg;
use windows_registry::CURRENT_USER;

const CONSENT_BASE: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\Capabilities\ConsentStore";

/// 可限制的 capability 列表（Windows 10/11 系统键）。
pub const CAPABILITIES: &[(&str, &str)] = &[
    ("location", "位置"),
    ("webcam", "摄像头"),
    ("microphone", "麦克风"),
    // 网络对 UWP 通常无法单独 deny（默认 Allow），改用防火墙更可靠
    ("userAccountInformation", "账户信息"),
    ("contacts", "联系人"),
    ("appointments", "日历"),
    ("phoneCall", "电话"),
    ("userNotificationListener", "通知监听"),
    ("radios", "收音机"),
    ("bluetooth", "蓝牙"),
    ("documentsLibrary", "文档库"),
    ("picturesLibrary", "图片库"),
    ("videosLibrary", "视频库"),
    ("musicLibrary", "音乐库"),
    ("enterpriseData", "企业数据"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapPermission {
    pub capability: String,
    pub label: String,
    pub package_family: Option<String>,
    pub value: Option<String>,
}

/// 校验 capability 名必须来自白名单。
fn valid_capability(cap: &str) -> bool {
    CAPABILITIES.iter().any(|(c, _)| *c == cap)
}

/// 校验 PackageFamilyName：只允许字母/数字/点/下划线/连字符，防止路径穿越。
fn valid_package_family(package_family: &str) -> bool {
    let p = package_family.trim();
    !p.is_empty()
        && p.len() <= 200
        && !p.contains("..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 校验取值：只允许 Allow / Deny / Prompt。
fn valid_value(value: &str) -> bool {
    matches!(value, "Allow" | "Deny" | "Prompt")
}

/// 读取某 capability 下所有应用及其当前值（用户级 ConsentStore）。
#[allow(dead_code)]
pub fn list_for_capability(cap: &str) -> Vec<CapPermission> {
    if !valid_capability(cap) {
        return vec![];
    }
    let path = format!("{}\\{}", CONSENT_BASE, cap);
    let subs = reg::list_subkeys("HKCU", &path);
    let mut out = Vec::new();
    let label = CAPABILITIES
        .iter()
        .find(|(c, _)| *c == cap)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| cap.to_string());
    for s in subs {
        let full = format!("{}\\{}", path, s);
        let value = reg::get_value("HKCU", &full, "Value");
        out.push(CapPermission {
            capability: cap.to_string(),
            label: label.clone(),
            package_family: Some(s),
            value,
        });
    }
    out
}

/// 列出某 UWP 包在所有 capability 下的当前值。
/// 未设置过的 capability 也会返回（value=None），便于用户从 UI 首次设置。
pub fn list_for_package(package_family: &str) -> Vec<CapPermission> {
    if !valid_package_family(package_family) {
        return vec![];
    }
    let mut out = Vec::new();
    for (cap, label) in CAPABILITIES {
        let path = format!("{}\\{}", CONSENT_BASE, cap);
        let full = format!("{}\\{}", path, package_family);
        let value = reg::get_value("HKCU", &full, "Value");
        out.push(CapPermission {
            capability: cap.to_string(),
            label: label.to_string(),
            package_family: Some(package_family.to_string()),
            value,
        });
    }
    out
}

/// 设置某包某 capability 的值："Allow" | "Deny" | "Prompt"。
/// 返回 (success, message)。
pub fn set_value(cap: &str, package_family: &str, value: &str) -> (bool, String) {
    if !valid_capability(cap) {
        return (false, "未知的 capability，已拒绝".into());
    }
    if !valid_package_family(package_family) {
        return (false, "PackageFamilyName 非法，已拒绝".into());
    }
    if !valid_value(value) {
        return (false, "只允许 Allow / Deny / Prompt".into());
    }
    let path = format!("{}\\{}\\{}", CONSENT_BASE, cap, package_family);
    match CURRENT_USER.create(&path) {
        Ok(k) => match k.set_string("Value", value) {
            Ok(_) => (
                true,
                format!("已将 {} 的 {} 设为 {}", package_family, cap, value),
            ),
            Err(e) => (false, e.to_string()),
        },
        Err(e) => (false, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_capability_and_traversal() {
        assert!(valid_capability("webcam"));
        assert!(!valid_capability(r"..\Run"));
        assert!(!valid_capability("unknown"));
        assert!(valid_package_family("Microsoft.WindowsStore_8wekyb3d8bbwe"));
        assert!(!valid_package_family(r"..\Microsoft\Windows\CurrentVersion\Run"));
        assert!(!valid_package_family("foo\\bar"));
        assert!(!valid_package_family(""));
        assert!(valid_value("Deny"));
        assert!(!valid_value("Maybe"));
    }
}
