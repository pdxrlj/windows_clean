//! 卸载后残留扫描（只读）。
//!
//! 安全原则：
//! - 只提出“应用专属”的候选，绝不用 substring / 空名匹配去命中系统目录；
//! - 任何候选在生成前先过 `safety` 校验，系统目录/系统注册表/关键服务不会出现在列表里；
//! - 真正删除前 `delete_residual` 还会再校验一次（双重防线）。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::registry;
use crate::safety::{self, TargetKind};
use crate::types::{ResidualItem, ResidualKind};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

/// 扫描配置：给定应用名、发布者、安装目录，从常见位置找残留。
pub struct ScanContext {
    pub app_name: String,
    pub publisher: Option<String>,
    pub install_location: Option<String>,
    /// 发布者注册表归一化名（无空格小写）。如 "microsoft"。
    pub publisher_key_hints: Vec<String>,
}

/// 目录大小统计上限，避免扫描超大目录卡死。
const MAX_WALK_ENTRIES: usize = 20_000;
const MAX_WALK_TIME: Duration = Duration::from_millis(1500);
/// 应用名归一化后至少这么长才做匹配，避免空/短名匹配一切。
const MIN_APP_NAME_LEN: usize = 2;

fn norm(s: &str) -> String {
    s.to_lowercase().replace([' ', '-', '_', '.'], "")
}

fn vendor_dirs() -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut v: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !p.as_os_str().is_empty() && seen.insert(p.clone()) {
            v.push(p);
        }
    };
    if let Ok(p) = std::env::var("ProgramFiles") {
        push(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("ProgramFiles(x86)") {
        push(PathBuf::from(p));
    }
    if let Some(p) = dirs::data_local_dir() {
        push(p); // %LOCALAPPDATA%
    }
    if let Some(p) = dirs::data_dir() {
        push(p); // %APPDATA% (Roaming)
    }
    if let Ok(p) = std::env::var("PROGRAMDATA") {
        push(PathBuf::from(p));
    }
    v
}

fn safe_dir_size(p: &Path) -> Option<u64> {
    let deadline = Instant::now() + MAX_WALK_TIME;
    let mut total = 0u64;
    let mut visited = 0usize;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        if visited > MAX_WALK_ENTRIES || Instant::now() > deadline {
            break;
        }
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            visited += 1;
            if visited > MAX_WALK_ENTRIES {
                break;
            }
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            // 不跟随符号链接，避免统计/遍历到系统目录。
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                if let Ok(m) = entry.metadata() {
                    total += m.len();
                }
            }
        }
    }
    Some(total)
}

/// 这些目录名属于系统/共享目录，任何情况下都不作为应用残留。
fn protected_child_name(n: &str) -> bool {
    matches!(
        n,
        "windows"
            | "microsoft"
            | "windowsapps"
            | "packages"
            | "commonfiles"
            | "system32"
            | "syswow64"
            | "winsxs"
            | "temp"
            | "default"
            | "allusers"
            | "public"
            | "programs"
            | "programdata"
            | "users"
            | "windowsdefender"
            | "internetexplorer"
    )
}

/// 名称匹配：只允许完全相等，或 `厂商名 + 子目录名 == 应用名`（如 google+chrome）。
/// 绝不使用 substring，避免把 `Windows` / `Microsoft` 之类系统目录误判为残留；
/// 也不把与应用同名的厂商根目录（如应用名 = 发布者名 = Google）当成可删项。
fn name_matches(child: &str, app_norm: &str, vendor_norms: &[String]) -> bool {
    let cn = norm(child);
    if cn.is_empty() || protected_child_name(&cn) {
        return false;
    }
    if cn == app_norm {
        // 这是共享的厂商根目录（所有 Google/Mozilla 应用共用），不整目录删除
        return !vendor_norms.iter().any(|v| *v == cn);
    }
    vendor_norms
        .iter()
        .any(|v| v.len() >= 3 && format!("{}{}", v, cn) == app_norm)
}

fn target_kind(kind: ResidualKind) -> TargetKind {
    match kind {
        ResidualKind::Directory => TargetKind::Directory,
        ResidualKind::Shortcut => TargetKind::Shortcut,
        _ => TargetKind::File,
    }
}

fn add_path(
    out: &mut Vec<ResidualItem>,
    seen: &mut HashSet<String>,
    p: &Path,
    kind: ResidualKind,
    id_prefix: &str,
) {
    let path_s = p.to_string_lossy().to_string();
    let key = norm(&path_s);
    if key.is_empty() || !seen.insert(key) {
        return;
    }
    let size_bytes = if kind == ResidualKind::Directory {
        safe_dir_size(p)
    } else {
        fs::metadata(p).map(|m| m.len()).ok()
    };
    out.push(ResidualItem {
        id: format!("{}:{}", id_prefix, path_s),
        kind,
        path: path_s.clone(),
        display: path_s,
        detail: size_bytes.map(|b| format!("{} 字节", b)),
        size_bytes,
        in_use: false,
        ai_verdict: None,
    });
}

pub fn scan_residuals(ctx: &ScanContext) -> Vec<ResidualItem> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let app_norm = norm(&ctx.app_name);
    // 空名 / 极短名会导致匹配到一切，直接不扫描。
    if app_norm.len() < MIN_APP_NAME_LEN {
        return out;
    }
    // 保留字段读取（发布者名用于日志/后续扩展；当前匹配只依赖归一化线索）。
    let _ = ctx.publisher.as_deref();
    let vendor_norms: Vec<String> = ctx
        .publisher_key_hints
        .iter()
        .map(|h| norm(h))
        .filter(|h| h.len() >= MIN_APP_NAME_LEN)
        .collect();

    // 1. 安装目录本体（仅当路径通过安全校验）
    if let Some(loc) = &ctx.install_location {
        if let Ok(p) = safety::validate_delete_path(loc, TargetKind::Directory) {
            if p.is_dir() {
                add_path(&mut out, &mut seen, &p, ResidualKind::Directory, "install_dir");
            }
        }
    }

    // 2. 常见厂商/应用数据目录：仅精确名或 厂商+应用 名匹配
    for base in vendor_dirs() {
        let Ok(rd) = fs::read_dir(&base) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name_matches(&name, &app_norm, &vendor_norms) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let kind = if ft.is_dir() {
                ResidualKind::Directory
            } else if ft.is_file() {
                ResidualKind::File
            } else {
                continue;
            };
            let p = e.path();
            let safe = match safety::validate_delete_path(&p.to_string_lossy(), target_kind(kind)) {
                Ok(p) => p,
                Err(_) => continue,
            };
            add_path(&mut out, &mut seen, &safe, kind, "app_dir");
        }

        // 厂商子目录（如 Program Files\Google\Chrome）
        for vh in &vendor_norms {
            let cand = base.join(vh);
            if !cand.is_dir() {
                continue;
            }
            if safety::validate_delete_path(&cand.to_string_lossy(), TargetKind::Directory).is_err() {
                continue;
            }
            let Ok(rd) = fs::read_dir(&cand) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name_matches(&name, &app_norm, &vendor_norms) {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                let kind = if ft.is_dir() {
                    ResidualKind::Directory
                } else if ft.is_file() {
                    ResidualKind::File
                } else {
                    continue;
                };
                let p = e.path();
                let safe = match safety::validate_delete_path(&p.to_string_lossy(), target_kind(kind)) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                add_path(&mut out, &mut seen, &safe, kind, "vendor_dir");
            }
        }
    }

    // 3. 开始菜单快捷方式（仅 .lnk，且必须位于开始菜单 Programs 下）
    for base in safety::start_menu_program_roots() {
        walk_for_shortcuts(
            Path::new(&base),
            &app_norm,
            &vendor_norms,
            &mut out,
            &mut seen,
        );
    }

    // 4. 注册表残留（只扫 SOFTWARE 下的应用专属键）
    scan_registry(&app_norm, &vendor_norms, &mut out);

    // 5. 服务与计划任务（services 模块内部已做系统项过滤）
    out.extend(crate::services::scan_for_app(&ctx.app_name));

    out
}

fn walk_for_shortcuts(
    base: &Path,
    app_norm: &str,
    vendor_norms: &[String],
    out: &mut Vec<ResidualItem>,
    seen: &mut HashSet<String>,
) {
    if !base.is_dir() {
        return;
    }
    let mut stack = vec![base.to_path_buf()];
    let mut visited = 0usize;
    while let Some(d) = stack.pop() {
        if visited > MAX_WALK_ENTRIES {
            break;
        }
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            visited += 1;
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                if !ft.is_symlink() {
                    stack.push(p);
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if !name.to_ascii_lowercase().ends_with(".lnk") {
                continue;
            }
            let stem = &name[..name.len() - 4];
            if !name_matches(stem, app_norm, vendor_norms) {
                continue;
            }
            if safety::validate_delete_path(&p.to_string_lossy(), TargetKind::Shortcut).is_err() {
                continue;
            }
            add_path(out, seen, &p, ResidualKind::Shortcut, "shortcut");
        }
    }
}

fn scan_registry(app_norm: &str, vendor_norms: &[String], out: &mut Vec<ResidualItem>) {
    let bases: &[(&str, &str)] = &[
        ("HKCU", r"SOFTWARE"),
        ("HKLM", r"SOFTWARE"),
        ("HKLM", r"SOFTWARE\WOW6432Node"),
    ];
    let mut seen: HashSet<String> = HashSet::new();

    for (root, base) in bases {
        let subs = registry::list_subkeys(root, base);
        for s in subs {
            let sn = norm(&s);
            if sn.is_empty() || protected_child_name(&sn) {
                continue;
            }
            if sn == app_norm {
                // 与应用同名的厂商根目录（如 HKCU\SOFTWARE\Google）不整体删除
                if vendor_norms.iter().any(|v| *v == sn) {
                    continue;
                }
                let path = format!("{}\\{}", base, s);
                push_reg_if_safe(out, &mut seen, root, &path);
                continue;
            }
            // 厂商键 -> 子键精确匹配（子键路径本身会再做安全校验）
            if vendor_norms.iter().any(|v| *v == sn) {
                let vendor_path = format!("{}\\{}", base, s);
                for s2 in registry::list_subkeys(root, &vendor_path) {
                    let s2n = norm(&s2);
                    if s2n.is_empty() || protected_child_name(&s2n) {
                        continue;
                    }
                    let matched = s2n == app_norm
                        || vendor_norms
                            .iter()
                            .any(|v| v.len() >= 3 && format!("{}{}", v, s2n) == app_norm);
                    if matched {
                        let path = format!("{}\\{}", vendor_path, s2);
                        push_reg_if_safe(out, &mut seen, root, &path);
                    }
                }
            }
        }
    }
}

fn push_reg_if_safe(
    out: &mut Vec<ResidualItem>,
    seen: &mut HashSet<String>,
    root: &str,
    path: &str,
) {
    if safety::validate_registry_delete_path(root, path).is_err() {
        return;
    }
    let id = format!("reg:{}:{}", root, path);
    if !seen.insert(id.clone()) {
        return;
    }
    let mut sub_count = 0usize;
    if let Ok(k) = open_root(root, path) {
        if let Ok(it) = k.keys() {
            sub_count = it.take(200).count();
        }
    }
    out.push(ResidualItem {
        id,
        kind: ResidualKind::RegistryKey,
        path: format!("{}\\{}", root, path),
        display: path.to_string(),
        detail: if sub_count > 0 {
            Some(format!("{} 个子键", sub_count))
        } else {
            None
        },
        size_bytes: None,
        in_use: false,
        ai_verdict: None,
    });
}

fn open_root(root: &str, path: &str) -> Result<windows_registry::Key, String> {
    let r = match root {
        "HKCU" => CURRENT_USER.open(path),
        "HKLM" => LOCAL_MACHINE.open(path),
        _ => return Err(format!("bad root: {}", root)),
    };
    r.map_err(|e| e.to_string())
}

/// 元数据是否为重解析点（junction / symlink / OneDrive 占位符等）。
fn is_reparse_meta(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        let _ = m;
        false
    }
}

/// 目录树里是否存在任何重解析点。
/// 恶意卸载器可能把安装目录内的子目录做成指向 C:\Windows 的 junction，
/// 删除前必须拒绝跟随任意层级的重定向。
fn contains_reparse_point(p: &Path) -> bool {
    if let Ok(m) = fs::symlink_metadata(p) {
        if is_reparse_meta(&m) {
            return true;
        }
    }
    let mut stack = vec![p.to_path_buf()];
    let mut visited = 0usize;
    while let Some(d) = stack.pop() {
        if visited > MAX_WALK_ENTRIES {
            return true; // 无法完整确认时按不安全处理
        }
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            visited += 1;
            if visited > MAX_WALK_ENTRIES {
                return true;
            }
            let p = entry.path();
            let Ok(m) = fs::symlink_metadata(&p) else { continue };
            if is_reparse_meta(&m) {
                return true;
            }
            if m.is_dir() {
                stack.push(p);
            }
        }
    }
    false
}

/// 删除单个残留项。返回 (success, message)。删除前会做第二轮安全校验。
pub fn delete_residual(item: &ResidualItem) -> (bool, String) {
    match item.kind {
        ResidualKind::Directory => match safety::validate_delete_path(&item.path, TargetKind::Directory)
        {
            Ok(p) => {
                if contains_reparse_point(&p) {
                    return (
                        false,
                        "目录内包含指向其他位置的链接/重解析点，已拒绝删除".into(),
                    );
                }
                match fs::remove_dir_all(&p) {
                    Ok(_) => (true, "已删除目录".into()),
                    Err(e) => (false, e.to_string()),
                }
            }
            Err(e) => (false, format!("安全校验拒绝删除：{}", e)),
        },
        ResidualKind::File => match safety::validate_delete_path(&item.path, TargetKind::File) {
            Ok(p) => {
                if let Ok(m) = fs::symlink_metadata(&p) {
                    if is_reparse_meta(&m) {
                        return (false, "目标是指向其他位置的链接/重解析点，已拒绝删除".into());
                    }
                }
                match fs::remove_file(&p) {
                    Ok(_) => (true, "已删除文件".into()),
                    Err(e) => (false, e.to_string()),
                }
            }
            Err(e) => (false, format!("安全校验拒绝删除：{}", e)),
        },
        ResidualKind::Shortcut => {
            match safety::validate_delete_path(&item.path, TargetKind::Shortcut) {
                Ok(p) => match fs::remove_file(&p) {
                    Ok(_) => (true, "已删除快捷方式".into()),
                    Err(e) => (false, e.to_string()),
                },
                Err(e) => (false, format!("安全校验拒绝删除：{}", e)),
            }
        }
        ResidualKind::RegistryKey => {
            // path 形如 HKLM\SOFTWARE\Foo\Bar
            let parts: Vec<&str> = item.path.splitn(2, '\\').collect();
            if parts.len() != 2 {
                return (false, "路径格式错误".into());
            }
            match registry::delete_registry_key(parts[0], parts[1]) {
                Ok(_) => (true, "已删除注册表项".into()),
                Err(e) => (false, e),
            }
        }
        ResidualKind::Service | ResidualKind::ScheduledTask => {
            // services 模块内部会按类型再做系统项校验
            crate::services::delete(item)
        }
        ResidualKind::EnvVar => (false, "暂不支持环境变量删除".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matching_is_not_substring_based() {
        let vendors = vec!["microsoft".to_string()];
        // 精确匹配
        assert!(name_matches("Slack", "slack", &[]));
        // 厂商 + 应用
        assert!(name_matches("Edge", "microsoftedge", &vendors));
        // 绝不能把 Microsoft / Windows 目录当残留
        assert!(!name_matches("Microsoft", "microsoftteams", &vendors));
        assert!(!name_matches("Windows", "microsoftwindows", &vendors));
        assert!(!name_matches("WindowsApps", "foo", &[]));
        assert!(!name_matches("", "foo", &[]));
        // 不再使用 contains("")：短应用名不匹配
        assert!(!name_matches("Slack", "s", &[]));
    }

    #[test]
    fn short_app_names_are_not_scanned() {
        let ctx = ScanContext {
            app_name: "Q".into(),
            publisher: None,
            install_location: None,
            publisher_key_hints: vec![],
        };
        assert!(scan_residuals(&ctx).is_empty());
    }

    #[test]
    fn install_location_must_pass_safety() {
        // 直接用一个不存在的安全路径验证生成逻辑不会崩溃；
        // C:\Windows 这类危险路径在 safety 层已被拒绝（见 safety 测试）。
        let ctx = ScanContext {
            app_name: "SomeApplication".into(),
            publisher: Some("SomeVendor".into()),
            install_location: Some(r"C:\Windows\System32".into()),
            publisher_key_hints: vec!["somevendor".into()],
        };
        let items = scan_residuals(&ctx);
        assert!(
            items
                .iter()
                .all(|i| !i.path.to_lowercase().contains("windows\\system32")),
            "系统目录绝不能出现在残留列表"
        );
    }

    /// 用真实的 Microsoft 发布者线索扫一遍本机：任何被列出的残留项都必须能通过
    /// 删除前同一套安全校验（防止扫描规则回归后把系统项放进列表）。
    #[test]
    fn scan_output_always_passes_delete_guards() {
        let ctx = ScanContext {
            app_name: "Microsoft Edge".into(),
            publisher: Some("Microsoft Corporation".into()),
            install_location: None,
            publisher_key_hints: vec![
                "microsoft".into(),
                "microsoft corporation".into(),
                "microsoftedge".into(),
            ],
        };
        let items = scan_residuals(&ctx);
        for it in &items {
            match it.kind {
                ResidualKind::Directory | ResidualKind::File | ResidualKind::Shortcut => {
                    assert!(
                        safety::validate_delete_path(&it.path, target_kind(it.kind)).is_ok(),
                        "扫描产生了不安全的路径项: {:?}",
                        it
                    );
                }
                ResidualKind::RegistryKey => {
                    let (root, path) = it
                        .path
                        .split_once('\\')
                        .unwrap_or_else(|| panic!("注册表路径格式异常: {}", it.path));
                    assert!(
                        safety::validate_registry_delete_path(root, path).is_ok(),
                        "扫描产生了不安全的注册表项: {}",
                        it.path
                    );
                }
                ResidualKind::Service => {
                    let name = it.id.trim_start_matches("svc:");
                    assert!(
                        safety::validate_service_deletion(name, Some(&it.path)).is_ok(),
                        "扫描产生了不安全的服务项: {:?}",
                        it
                    );
                }
                ResidualKind::ScheduledTask => {
                    let name = it.id.trim_start_matches("task:");
                    assert!(
                        safety::validate_task_deletion(name).is_ok(),
                        "扫描产生了不安全的计划任务: {:?}",
                        it
                    );
                }
                ResidualKind::EnvVar => {}
            }
        }
    }
}
