//! 卸载后残留扫描（只读）。
//!
//! 安全原则：
//! - 只提出“应用专属”的候选，绝不用 substring / 空名匹配去命中系统目录；
//!   别名后缀只接受已知组件词或版本号样式（`Quark 2.1` 可、`QuarkXPress` 不可）；
//! - 任何候选在生成前先过 `safety` 校验，系统目录/系统注册表/关键服务不会出现在列表里；
//! - 真正删除前 `delete_residual` 还会再校验一次（双重防线）；
//!   因占用失败时只结束目标目录内的进程后重试，仍失败才记为失败。

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::registry;
use crate::safety::{self, TargetKind};
use crate::types::{AiGuessedItem, ResidualItem, ResidualKind};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

/// 扫描配置：给定应用名、发布者、安装目录，从常见位置找残留。
pub struct ScanContext {
    pub app_name: String,
    pub publisher: Option<String>,
    pub install_location: Option<String>,
    /// 发布者注册表归一化名（无空格小写）。如 "microsoft"。
    pub publisher_key_hints: Vec<String>,
    /// 从显示名 / 安装路径 / 卸载命令 / 注册表键推导出的英文别名（已归一化）。
    /// 用于解决“显示名是中文、实际目录是英文”的软件，例如 夸克 -> Quark。
    pub aliases: Vec<String>,
    /// 安装目录、卸载器路径等原始路径线索（小写），用于匹配任务/服务的命令行。
    pub path_hints: Vec<String>,
    /// 额外需要扫描一层子目录的根目录（如安装目录的父目录 D:\software）。
    pub extra_roots: Vec<PathBuf>,
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
    if let Ok(p) = std::env::var("LOCALAPPDATA") {
        // 低完整性级别目录（浏览器/插件类应用常用）
        push(PathBuf::from(p).join("Low"));
    }
    if let Some(p) = dirs::data_local_dir() {
        push(p.clone()); // %LOCALAPPDATA%
        push(p.join("Programs")); // 用户级安装目录
    }
    if let Some(p) = dirs::data_dir() {
        push(p); // %APPDATA% (Roaming)
    }
    if let Ok(p) = std::env::var("PROGRAMDATA") {
        push(PathBuf::from(p));
    }
    if let Some(p) = dirs::home_dir() {
        push(p); // 用户 profile 根：主要为了 .开头配置目录，已知文件夹由 protected_child_name 拦截
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
            // 系统容器：即使名字匹配出错也绝不能作为残留项
            | "wow6432node"
            | "classes"
            | "policies"
            | "currentversion"
            | "uninstall"
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
            // 用户 profile 下的已知文件夹：绝不能作为应用残留被删
            | "downloads"
            | "documents"
            | "pictures"
            | "music"
            | "videos"
            | "links"
            | "favorites"
            | "contacts"
            | "savedgames"
            | "searches"
            | "screenshots"
            | "appdata"
            | "onedrive"
    )
}

/// 名称匹配：只允许完全相等，或 `厂商名 + 子目录名 == 应用名`（如 google+chrome）。
/// 绝不使用任意 substring，避免把 `Windows` / `Microsoft` 之类系统目录误判为残留；
/// 也不把与应用同名的厂商根目录（如应用名 = 发布者名 = Google）当成可删项。
///
/// 另外接受从安装路径/卸载命令推导出的英文别名：
/// `quark` 可匹配 `QuarkUpdater` / `quark-cloud-drive` / `The Quark Authors`，
/// 但拒绝后缀不在已知组件词表里的名字（例如 `QuarkXPress`）。
fn name_matches(
    child: &str,
    app_norm: &str,
    vendor_norms: &[String],
    aliases: &[String],
) -> bool {
    let cn = norm(child);
    if cn.is_empty() || protected_child_name(&cn) {
        return false;
    }
    if cn == app_norm {
        // 这是共享的厂商根目录（所有 Google/Mozilla 应用共用），不整目录删除
        return !vendor_norms.iter().any(|v| *v == cn);
    }
    if vendor_norms
        .iter()
        .any(|v| v.len() >= 3 && format!("{}{}", v, cn) == app_norm)
    {
        return true;
    }
    aliases.iter().any(|a| alias_matches(&cn, a))
}

/// 别名匹配：允许别名本身、别名+已知组件后缀、别名+版本号样式后缀，或 `The <别名> Authors`。
pub(crate) fn alias_matches(child_norm: &str, alias: &str) -> bool {
    if alias.is_empty() {
        return false;
    }
    if child_norm == alias {
        return true;
    }
    // 太短的别名（如 qq）只用精确匹配，避免误伤。
    if alias.chars().count() < 4 {
        return false;
    }
    if let Some(rest) = child_norm.strip_prefix(alias) {
        if suffix_is_known_component(rest) {
            return true;
        }
        // 版本号/分隔符结尾的目录名：`Quark 2.1` -> `quark2.1`、`Quark-v3` -> `quarkv3`
        if suffix_is_version_like(rest) {
            return true;
        }
    }
    // 厂商容器目录：The Quark Authors / Quark Authors
    if let Some(head) = child_norm.strip_suffix("authors") {
        let head = head.strip_prefix("the").unwrap_or(head);
        if head == alias {
            return true;
        }
    }
    false
}

/// 判断剩余部分是否由已知的“伴随组件”词拼接而成。
/// 只允许白名单，不做 contains，避免 QuarkXPress 之类的误匹配。
fn suffix_is_known_component(mut rest: &str) -> bool {
    if rest.is_empty() {
        return false;
    }
    const TOKENS: &[&str] = &[
        "clouddrive",
        "uninstaller",
        "installer",
        "crashpad",
        "updater",
        "updates",
        "browser",
        "desktop",
        "helper",
        "service",
        "client",
        "agent",
        "update",
        "setup",
        "data",
        "cache",
        "logs",
        "uninstall",
        "pc",
        "app",
        "x64",
        "x86",
    ];
    while !rest.is_empty() {
        let mut matched = false;
        for tok in TOKENS {
            if let Some(after) = rest.strip_prefix(tok) {
                rest = after;
                matched = true;
                break;
            }
        }
        if !matched {
            return false;
        }
    }
    true
}

/// 判断剩余部分是否是版本号样式（仅由数字、分隔符、`v`/`x` 前缀、`bit`
/// 和常见版本尾缀词拼接而成），如 `2.1`、`-v3`、`_1.2.3_final`。
/// 输入是 norm() 之后的剩余段（空格/连字符/下划线/点已被剔除）。
/// 与应用名无关的词（如 xpress）不命中，保持既有白名单防误删语义。
fn suffix_is_version_like(rest: &str) -> bool {
    const VERSION_TAIL_TOKENS: &[&str] = &["beta", "rc", "final", "lite", "pro"];
    let mut any_digit = false;
    let bytes = rest.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let s = &rest[i..];
        if bytes[i].is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            any_digit = true;
            continue;
        }
        // `v3` / `x64`：前缀后紧跟数字才是版本写法（数字已由上面的分支处理 x64/64x）
        if matches!(bytes[i], b'v' | b'x') {
            let after = &s[1..];
            if after.starts_with(|c: char| c.is_ascii_digit()) {
                i += 1;
                continue;
            }
            return false;
        }
        if let Some(after) = s.strip_prefix("bit") {
            let _ = after;
            i += "bit".len();
            continue;
        }
        let mut matched = false;
        for tok in VERSION_TAIL_TOKENS {
            if s.starts_with(tok) {
                i += tok.len();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        let c = s.chars().next().unwrap_or('\u{0}');
        // 非 ASCII 字母（如中文）属于真实词，不能当分隔符跳过
        if c.is_ascii() && !c.is_ascii_alphanumeric() {
            i += c.len_utf8();
            continue;
        }
        return false;
    }
    any_digit
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

/// AI 推断的位置只允许落在这些“应用会写数据”的根目录下。
///
/// 刻意不包含用户 profile 根：`C:\Users\me\Documents\Foo` 这类路径名字能匹配上，
/// 但很可能是用户自己的资料，不该由 AI 的判断来决定删不删。
fn guess_roots(ctx: &ScanContext) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let mut v: Vec<PathBuf> = vendor_dirs()
        .into_iter()
        .filter(|p| Some(p) != home.as_ref())
        .collect();
    // 安装在非标准位置时（如 D:\software\Foo），其父级也是合法落点。
    if let Some(loc) = ctx.install_location.as_deref() {
        let trimmed = loc.trim().trim_end_matches(['\\', '/']);
        if let Some(parent) = Path::new(trimmed).parent() {
            if parent.components().count() >= 2 {
                v.push(parent.to_path_buf());
            }
        }
    }
    v
}

fn under_any_root(low_path: &str, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| {
        let rs = r.to_string_lossy().to_lowercase();
        let rs = rs.trim_end_matches('\\');
        !rs.is_empty()
            && low_path.len() > rs.len() + 1
            && low_path.starts_with(rs)
            && low_path.as_bytes().get(rs.len()) == Some(&b'\\')
    })
}

fn looks_like_registry_path(p: &str) -> bool {
    let u = p.trim().to_ascii_uppercase();
    [
        "HKCU\\",
        "HKLM\\",
        "HKEY_CURRENT_USER\\",
        "HKEY_LOCAL_MACHINE\\",
    ]
    .iter()
    .any(|prefix| u.starts_with(prefix))
}

/// 逐条验证 AI 推断的位置，只保留“确实存在 + 通过安全校验 + 与这款软件相关”的项。
///
/// 这是 AI 参与卸载时唯一的信任边界：AI 只能提供**候选**，能不能删由这里决定。
/// 验证不通过的一律静默丢弃，不向用户展示“也许能删”的模糊项。
///
/// `other_locations` 是其他已安装软件的安装目录：宁可漏删，也不能删到别人。
pub fn verify_ai_guesses(
    ctx: &ScanContext,
    guesses: &[AiGuessedItem],
    other_locations: &[String],
) -> Vec<ResidualItem> {
    let app_norm = norm(&ctx.app_name);
    if app_norm.len() < MIN_APP_NAME_LEN {
        return Vec::new();
    }
    let vendor_norms: Vec<String> = ctx
        .publisher_key_hints
        .iter()
        .map(|h| norm(h))
        .filter(|h| h.len() >= MIN_APP_NAME_LEN)
        .collect();
    let aliases: Vec<String> = ctx
        .aliases
        .iter()
        .map(|a| norm(a))
        .filter(|a| a.chars().count() >= MIN_APP_NAME_LEN)
        .collect();
    let roots = guess_roots(ctx);
    let others: Vec<String> = other_locations
        .iter()
        .map(|s| s.trim().trim_end_matches(['\\', '/']).to_lowercase())
        .filter(|s| s.len() >= 4)
        .collect();

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for g in guesses {
        let item = if g.kind.eq_ignore_ascii_case("registry") || looks_like_registry_path(&g.path) {
            verify_registry_guess(g, &app_norm, &vendor_norms, &aliases)
        } else {
            verify_path_guess(g, &app_norm, &vendor_norms, &aliases, &roots, &others)
        };
        if let Some(it) = item {
            if seen.insert(norm(&it.path)) {
                out.push(it);
            }
        }
    }
    out
}

fn verify_path_guess(
    g: &AiGuessedItem,
    app_norm: &str,
    vendor_norms: &[String],
    aliases: &[String],
    roots: &[PathBuf],
    others: &[String],
) -> Option<ResidualItem> {
    let raw = g.path.trim().trim_end_matches(['\\', '/']);
    if raw.len() < 4 || !Path::new(raw).is_absolute() {
        return None;
    }
    let meta = fs::symlink_metadata(raw).ok()?;
    // junction / symlink / 占位符：和扫描器一致，整树不碰
    if is_reparse_meta(&meta) {
        return None;
    }
    let (kind, target) = if meta.is_dir() {
        (ResidualKind::Directory, TargetKind::Directory)
    } else if meta.is_file() {
        (ResidualKind::File, TargetKind::File)
    } else {
        return None;
    };
    // 安全校验：字符串形态 + canonicalize 后对真实路径复验
    let canon = safety::validate_delete_path(raw, target).ok()?;
    let canon_s = canon.to_string_lossy().to_string();
    let low = canon_s.to_lowercase();

    // 1) 只能落在已知的应用数据 / 程序根目录下
    if !under_any_root(&low, roots) {
        return None;
    }
    // 2) 不能是别的已安装软件的安装目录或其子目录
    for other in others {
        if low == *other || low.starts_with(&format!("{}\\", other)) {
            return None;
        }
    }
    // 3) 名字必须和这款软件相关（自身或父目录）
    let leaf = canon
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = canon
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if protected_child_name(&norm(&leaf)) {
        return None;
    }
    if !name_matches(&leaf, app_norm, vendor_norms, aliases)
        && !name_matches(&parent, app_norm, vendor_norms, aliases)
    {
        return None;
    }

    let size_bytes = if kind == ResidualKind::Directory {
        safe_dir_size(&canon)
    } else {
        Some(meta.len())
    };
    let id_prefix = if kind == ResidualKind::Directory { "ai:dir" } else { "ai:file" };
    Some(ResidualItem {
        id: format!("{}:{}", id_prefix, canon_s),
        kind,
        path: canon_s.clone(),
        display: canon_s,
        detail: Some(guess_reason(g, "AI 推断的位置")),
        size_bytes,
        in_use: false,
        ai_verdict: None,
    })
}

fn verify_registry_guess(
    g: &AiGuessedItem,
    app_norm: &str,
    vendor_norms: &[String],
    aliases: &[String],
) -> Option<ResidualItem> {
    let p = g.path.trim().replace('/', "\\");
    let p = p.trim_matches('\\');
    let (root_raw, rest) = p.split_once('\\')?;
    let root = match root_raw.to_ascii_uppercase().as_str() {
        "HKCU" | "HKEY_CURRENT_USER" => "HKCU",
        "HKLM" | "HKEY_LOCAL_MACHINE" => "HKLM",
        _ => return None,
    };
    let rest = rest.trim_matches('\\');
    if rest.is_empty() {
        return None;
    }
    safety::validate_registry_delete_path(root, rest).ok()?;
    // 必须真实存在，否则就是 AI 编的
    let key = open_root(root, rest).ok()?;
    let related = rest
        .split('\\')
        .filter(|s| !s.is_empty())
        .rev()
        .take(2)
        .any(|s| name_matches(s, app_norm, vendor_norms, aliases));
    if !related {
        return None;
    }
    let sub = key.keys().ok().map(|it| it.take(200).count()).unwrap_or(0);
    let fallback = if sub > 0 {
        format!("{} 个子键", sub)
    } else {
        "AI 推断的注册表项".to_string()
    };
    Some(ResidualItem {
        id: format!("ai:reg:{}:{}", root, rest),
        kind: ResidualKind::RegistryKey,
        path: format!("{}\\{}", root, rest),
        display: format!("{}\\{}", root, rest),
        detail: Some(guess_reason(g, &fallback)),
        size_bytes: None,
        in_use: false,
        ai_verdict: None,
    })
}

fn guess_reason(g: &AiGuessedItem, fallback: &str) -> String {
    let r = g.reason.trim();
    if r.is_empty() {
        fallback.to_string()
    } else {
        r.to_string()
    }
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
    let aliases: Vec<String> = ctx
        .aliases
        .iter()
        .map(|a| norm(a))
        .filter(|a| a.chars().count() >= MIN_APP_NAME_LEN)
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
            if !name_matches(&name, &app_norm, &vendor_norms, &aliases) {
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
                if !name_matches(&name, &app_norm, &vendor_norms, &aliases) {
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

    // 2.5 安装目录的父目录（如 D:\software）下与应用相关的同级目录。
    // 有些软件把更新器/网盘组件放在安装目录旁边，这里只扫一层、不递归。
    for base in &ctx.extra_roots {
        let Ok(rd) = fs::read_dir(base) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name_matches(&name, &app_norm, &vendor_norms, &aliases) {
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
            add_path(&mut out, &mut seen, &safe, kind, "sibling");
        }
    }

    // 3. 快捷方式（仅 .lnk，且必须位于开始菜单 Programs / 桌面 / SendTo 下）
    for base in safety::allowed_shortcut_roots() {
        walk_for_shortcuts(
            Path::new(&base),
            &app_norm,
            &vendor_norms,
            &aliases,
            &mut out,
            &mut seen,
        );
    }

    // 3.5 用户 profile 下 .开头配置目录（如 .gradle / .quark）
    scan_dot_dirs(&app_norm, &vendor_norms, &aliases, &mut out, &mut seen);

    // 4. 注册表残留（只扫 SOFTWARE 下的应用专属键）
    scan_registry(&app_norm, &vendor_norms, &aliases, &mut out);

    // 4.5 Run/RunOnce 自启动值（只读；删除时按值精确删除，不删整个键）
    scan_run_values(&app_norm, &aliases, &ctx.path_hints, &mut out);

    // 5. 服务与计划任务（按名称、别名、二进制/任务命令行路径匹配）
    out.extend(crate::services::scan_for_app(
        &ctx.app_name,
        &aliases,
        &ctx.path_hints,
    ));

    out
}

fn walk_for_shortcuts(
    base: &Path,
    app_norm: &str,
    vendor_norms: &[String],
    aliases: &[String],
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
            if !name_matches(stem, app_norm, vendor_norms, aliases) {
                continue;
            }
            if safety::validate_delete_path(&p.to_string_lossy(), TargetKind::Shortcut).is_err() {
                continue;
            }
            add_path(out, seen, &p, ResidualKind::Shortcut, "shortcut");
        }
    }
}

/// 扫描用户 profile 根下 .开头的配置目录（如 `.gradle`、`.quark`）。
/// 只收名称命中应用/别名的项，并照常过安全校验。
fn scan_dot_dirs(
    app_norm: &str,
    vendor_norms: &[String],
    aliases: &[String],
    out: &mut Vec<ResidualItem>,
    seen: &mut HashSet<String>,
) {
    let Some(home) = dirs::home_dir() else { return };
    let Ok(rd) = fs::read_dir(&home) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with('.') || !name_matches(&name, app_norm, vendor_norms, aliases) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let p = e.path();
        let Ok(safe) = safety::validate_delete_path(&p.to_string_lossy(), TargetKind::Directory)
        else {
            continue;
        };
        add_path(out, seen, &safe, ResidualKind::Directory, "profile_dot");
    }
}

fn scan_registry(
    app_norm: &str,
    vendor_norms: &[String],
    aliases: &[String],
    out: &mut Vec<ResidualItem>,
) {
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
            // 英文别名（如 HKCU\SOFTWARE\QuarkUpdater）
            if aliases.iter().any(|a| alias_matches(&sn, a)) {
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
                        || aliases.iter().any(|a| alias_matches(&s2n, a))
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

fn registry_value_string(v: &windows_registry::Value) -> String {
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

/// 路径里的某一段是否与别名匹配（如 `C:\...\QuarkUpdater\updater.exe` 的 QuarkUpdater）。
fn path_segment_matches(data_lower: &str, alias: &str) -> bool {
    data_lower.split(['\\', '/']).any(|seg| {
        let s = seg.trim().trim_matches('"');
        let s = s.strip_suffix(".exe").unwrap_or(s);
        alias_matches(&norm(s), alias)
    })
}

/// 扫描 Run/RunOnce 自启动值。只读；删除时按 (key, value) 精确删除。
fn scan_run_values(
    app_norm: &str,
    aliases: &[String],
    path_hints: &[String],
    out: &mut Vec<ResidualItem>,
) {
    let mut seen: HashSet<String> = HashSet::new();
    for &(root, path) in safety::RUN_KEYS {
        let key = match root {
            "HKCU" => CURRENT_USER,
            _ => LOCAL_MACHINE,
        };
        let Ok(k) = key.open(path) else { continue };
        let Ok(values) = k.values() else { continue };
        for (name, value) in values {
            // id 用 | 分隔，值名里含 | 的极端情况直接跳过。
            if name.is_empty() || name.contains('|') || name.contains('\\') {
                continue;
            }
            let data = registry_value_string(&value);
            let vn = norm(&name);
            let name_hit = vn == app_norm || aliases.iter().any(|a| alias_matches(&vn, a));
            let data_lower = data.to_lowercase();
            let data_hit = !name_hit
                && !data_lower.is_empty()
                && (path_hints.iter().any(|h| h.len() >= 4 && data_lower.contains(h))
                    || aliases
                        .iter()
                        .any(|a| a.chars().count() >= 4 && path_segment_matches(&data_lower, a)));
            if !name_hit && !data_hit {
                continue;
            }
            // 没有名称命中、只是路径里含别名的系统启动项，不列为候选。
            if !name_hit && data_lower.contains("\\windows\\") {
                continue;
            }
            let id = format!("runvalue:{}|{}|{}", root, path, name);
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(ResidualItem {
                id,
                kind: ResidualKind::RegistryValue,
                path: format!("{}\\{}\\{}", root, path, name),
                display: format!("启动项: {} = {}", name, data),
                detail: Some(data),
                size_bytes: None,
                in_use: false,
                ai_verdict: None,
            });
        }
    }
}

fn push_reg_if_safe(    out: &mut Vec<ResidualItem>,
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

/// 删除失败后，因占用重试的最大次数与递增延迟。
const DELETE_RETRY_ROUNDS: usize = 3;
const DELETE_RETRY_DELAYS: [Duration; DELETE_RETRY_ROUNDS] = [
    Duration::from_millis(600),
    Duration::from_millis(1200),
    Duration::from_millis(2000),
];

/// 是否属于“被占用/被锁”类失败（含内存映射导致的 AccessDenied）。
fn is_in_use_err(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        // ERROR_ACCESS_DENIED=5 / SHARING_VIOLATION=32 / LOCK_VIOLATION=33
        // 写删被内存映射的文件也会报 AccessDenied，同样按占用处理。
        if matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33)) {
            return true;
        }
    }
    false
}

/// 结束目标目录内的进程（仅限 exe 真实位于该目录之下）。
/// 与 lib.rs 的卸载等待逻辑同一套路：绝对路径 taskkill + /T /F 结束进程树。
/// 返回是否至少杀了一个进程。
fn kill_processes_under_dir(dir: &Path) -> bool {
    use sysinfo::{ProcessesToUpdate, System};

    let dir_low = dir.to_string_lossy().to_ascii_lowercase();
    if dir_low.contains("\\windows\\") || dir_low.chars().filter(|&c| c == '\\').count() < 2 {
        return false; // 不在明确的非系统目录里：不动任何进程
    }
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let self_pid = std::process::id();
    let mut pids: Vec<u32> = Vec::new();
    for (pid, p) in sys.processes() {
        if pid.as_u32() == self_pid {
            continue;
        }
        let exe_low = p.exe().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if exe_low.is_empty() {
            continue;
        }
        // 只杀 exe 位于目标目录（或其子目录）下的进程，绝不按名字全局匹配。
        if exe_low == dir_low
            || exe_low.starts_with(&format!("{dir_low}\\"))
            || exe_low.starts_with(&format!("{dir_low}/"))
        {
            pids.push(pid.as_u32());
        }
    }
    if pids.is_empty() {
        return false;
    }
    for pid in pids {
        let pid_s = pid.to_string();
        let _ = crate::safety::hidden_command(crate::safety::system32_exe("taskkill.exe"))
            .args(["/PID", &pid_s, "/T", "/F"])
            .output();
    }
    true
}

/// 单次删除动作（按类型），供重试包装。
fn delete_once(item: &ResidualItem, p: &Path) -> io::Result<()> {
    match item.kind {
        ResidualKind::Directory => fs::remove_dir_all(p),
        ResidualKind::File | ResidualKind::Shortcut => fs::remove_file(p),
        _ => unreachable!("delete_once 只处理文件系统项"),
    }
}

/// 删除文件系统残留项；因占用失败时，结束目标目录内的进程后递增延迟重试，
/// 共 `DELETE_RETRY_ROUNDS` 轮，仍失败才记为失败。
fn delete_fs_with_retry(item: &ResidualItem, p: &Path, ok_msg: &str) -> (bool, String) {
    // 目录型目标本身就是进程搜索根；文件型用父目录。
    let proc_root = match item.kind {
        ResidualKind::Directory => p.to_path_buf(),
        _ => p.parent().map(PathBuf::from).unwrap_or_else(|| p.to_path_buf()),
    };
    let mut killed_any = false;
    let mut last_err: Option<io::Error> = None;
    for i in 0..DELETE_RETRY_ROUNDS {
        match delete_once(item, p) {
            Ok(_) => return (true, ok_msg.into()),
            Err(e) => {
                if !is_in_use_err(&e) {
                    return (false, e.to_string());
                }
                last_err = Some(e);
                killed_any |= kill_processes_under_dir(&proc_root);
                thread::sleep(DELETE_RETRY_DELAYS[i]);
            }
        }
    }
    // 最后一轮杀过进程就再试一次，避免刚杀完就没机会了
    if killed_any && delete_once(item, p).is_ok() {
        return (true, ok_msg.into());
    }
    let msg = last_err
        .map(|e| format!("目标被占用，重试 {} 次后仍失败：{}", DELETE_RETRY_ROUNDS, e))
        .unwrap_or_else(|| "删除失败".into());
    (false, msg)
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
                    Err(e) if is_in_use_err(&e) => {
                        delete_fs_with_retry(item, &p, "已删除目录")
                    }
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
                    Err(e) if is_in_use_err(&e) => {
                        delete_fs_with_retry(item, &p, "已删除文件")
                    }
                    Err(e) => (false, e.to_string()),
                }
            }
            Err(e) => (false, format!("安全校验拒绝删除：{}", e)),
        },
        ResidualKind::Shortcut => {
            match safety::validate_delete_path(&item.path, TargetKind::Shortcut) {
                Ok(p) => match fs::remove_file(&p) {
                    Ok(_) => (true, "已删除快捷方式".into()),
                    Err(e) if is_in_use_err(&e) => {
                        delete_fs_with_retry(item, &p, "已删除快捷方式")
                    }
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
        ResidualKind::RegistryValue => {
            // id 形如 runvalue:HKCU|SOFTWARE\...\Run|ValueName
            let rest = item.id.strip_prefix("runvalue:").unwrap_or("");
            let parts: Vec<&str> = rest.splitn(3, '|').collect();
            if parts.len() != 3 {
                return (false, "启动项路径格式错误".into());
            }
            let (root, key_path, value_name) = (parts[0], parts[1], parts[2]);
            if let Err(e) = safety::validate_run_key_location(root, key_path) {
                return (false, format!("安全校验拒绝删除启动项：{}", e));
            }
            if let Err(e) = safety::validate_registry_value_name(value_name) {
                return (false, format!("安全校验拒绝删除启动项：{}", e));
            }
            match registry::delete_value_in_key(root, key_path, value_name) {
                Ok(_) => (true, "已删除启动项".into()),
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
    fn system_containers_are_never_residuals() {
        // 注册表重定向容器 / 系统子树：即使别名推断出错，也绝不能被匹配成残留项。
        for n in [
            "wow6432node",
            "classes",
            "policies",
            "currentversion",
            "uninstall",
            "microsoft",
            "windows",
        ] {
            assert!(protected_child_name(n), "{n} 必须被保护");
        }
        // 即使别名里混进了 wow6432node，名字匹配也不会命中。
        let aliases = vec!["wow6432node".to_string()];
        assert!(!name_matches("Wow6432Node", "网易云音乐", &[], &aliases));
    }

    #[test]
    fn name_matching_is_not_substring_based() {
        let vendors = vec!["microsoft".to_string()];
        let aliases = vec!["quark".to_string()];
        // 精确匹配
        assert!(name_matches("Slack", "slack", &[], &[]));
        // 厂商 + 应用
        assert!(name_matches("Edge", "microsoftedge", &vendors, &[]));
        // 别名 + 已知组件后缀
        assert!(name_matches("QuarkUpdater", "夸克", &[], &aliases));
        assert!(name_matches("quark-cloud-drive", "夸克", &[], &aliases));
        assert!(name_matches("The Quark Authors", "夸克", &[], &aliases));
        // 后缀不在白名单，不能误删其它 Quark 产品
        assert!(!name_matches("QuarkXPress", "夸克", &[], &aliases));
        assert!(!name_matches("QuarkHTM", "夸克", &[], &aliases));
        // 绝不能把 Microsoft / Windows 目录当残留
        assert!(!name_matches("Microsoft", "microsoftteams", &vendors, &[]));
        assert!(!name_matches("Windows", "microsoftwindows", &vendors, &[]));
        assert!(!name_matches("WindowsApps", "foo", &[], &[]));
        assert!(!name_matches("", "foo", &[], &[]));
        // 不再使用 contains("")：短应用名不匹配
        assert!(!name_matches("Slack", "s", &[], &[]));
    }

    #[test]
    fn alias_matching_handles_updater_and_vendor_containers() {
        assert!(alias_matches("quark", "quark"));
        assert!(alias_matches("quarkupdater", "quark"));
        assert!(alias_matches("quarkclouddriveupdater", "quark"));
        assert!(alias_matches("thequarkauthors", "quark"));
        assert!(!alias_matches("quarkxpress", "quark"));
        assert!(!alias_matches("quarkhtm", "quark"));
        // 短别名只做精确匹配
        assert!(alias_matches("qq", "qq"));
        assert!(!alias_matches("qqbrowser", "qq"));
    }

    #[test]
    fn alias_matching_accepts_version_suffixes() {
        // `Quark 2.1` / `Quark-v3` / `quark_1.2.3` 归一化后的剩余部分是版本号样式
        assert!(alias_matches("quark2.1", "quark"));
        assert!(alias_matches("quarkv3", "quark"));
        assert!(alias_matches("quark1.2.3", "quark"));
        assert!(alias_matches("quark2beta", "quark"));
        assert!(alias_matches("quark1x64", "quark"));
        // 与应用无关的词不能借分隔符/尾缀词误匹配
        assert!(!alias_matches("quarkxpress", "quark"));
        assert!(!alias_matches("quarkbeta", "quark"));
        assert!(!alias_matches("quarkpro", "quark"));
        assert!(!alias_matches("quarkvision", "quark"));
        // 短别名仍然只做精确匹配
        assert!(!alias_matches("qq2.1", "qq"));
    }

    #[test]
    fn path_segment_alias_matching_for_run_values() {
        assert!(path_segment_matches(
            r"c:\users\me\appdata\local\quarkupdater\quarkupdater\1.0.0.21\updater.exe",
            "quark"
        ));
        assert!(path_segment_matches(r"d:\software\quark\quark.exe", "quark"));
        assert!(!path_segment_matches(r"c:\program files\quarkxpress\app.exe", "quark"));
    }

    #[test]
    fn alias_scan_catches_updater_siblings() {
        // 模拟中文显示名 + 英文安装目录/更新器目录的场景。
        let base = std::env::temp_dir().join(format!(
            "cleansweep-alias-test-{}",
            std::process::id()
        ));
        let updater = base.join("QuarkUpdater");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&updater).expect("create test dir");
        std::fs::write(updater.join("updater.exe"), b"x").expect("write test file");
        let ctx = ScanContext {
            app_name: "夸克".into(),
            publisher: None,
            install_location: Some(base.join("Quark").to_string_lossy().into_owned()),
            publisher_key_hints: vec![],
            aliases: vec!["quark".into()],
            path_hints: vec![],
            extra_roots: vec![base.clone()],
        };
        let items = scan_residuals(&ctx);
        let hit = items
            .iter()
            .any(|i| i.path.to_lowercase().ends_with("quarkupdater"));
        let _ = std::fs::remove_dir_all(&base);
        assert!(
            hit,
            "别名扫描应识别安装目录旁的 QuarkUpdater，实际: {:#?}",
            items
        );
    }

    #[test]
    fn short_app_names_are_not_scanned() {
        let ctx = ScanContext {
            app_name: "Q".into(),
            publisher: None,
            install_location: None,
            publisher_key_hints: vec![],
            aliases: vec![],
            path_hints: vec![],
            extra_roots: vec![],
        };
        assert!(scan_residuals(&ctx).is_empty());
    }

    fn guess(path: &str, kind: &str) -> AiGuessedItem {
        AiGuessedItem {
            path: path.into(),
            kind: kind.into(),
            reason: String::new(),
        }
    }

    fn demo_ctx() -> ScanContext {
        ScanContext {
            app_name: "DemoAppVerifyTest".into(),
            publisher: Some("Demo Vendor".into()),
            install_location: Some(r"C:\Program Files\DemoAppVerifyTest".into()),
            publisher_key_hints: vec!["demovendor".into()],
            aliases: vec!["demoappverifytest".into()],
            path_hints: vec![],
            extra_roots: vec![],
        }
    }

    #[test]
    fn ai_guesses_must_survive_verification() {
        let ctx = demo_ctx();
        // 不存在的路径 / 相对路径 / 系统目录 / 系统目录下的同名目录
        let guesses = vec![
            guess(r"C:\Program Files\DemoAppVerifyTest\not-there-12345", "dir"),
            guess(r"Program Files\DemoAppVerifyTest", "dir"),
            guess(r"C:\Windows\System32\DemoAppVerifyTest", "dir"),
            guess(r"C:\Windows\DemoAppVerifyTest.exe", "file"),
        ];
        assert!(
            verify_ai_guesses(&ctx, &guesses, &[]).is_empty(),
            "不存在/越界/系统目录的 AI 猜测必须被全部拒绝"
        );

        // 注册表：系统关键位置、不存在的键、非 HKCU/HKLM 根都必须拒绝
        let regs = vec![
            guess(
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "registry",
            ),
            guess(r"HKLM\SOFTWARE\Classes\DemoAppVerifyTest", "registry"),
            guess(r"HKCU\Software\DemoVendorZzzNotExist", "registry"),
            guess(r"HKEY_USERS\S-1-5-21\Software\DemoAppVerifyTest", "registry"),
        ];
        assert!(
            verify_ai_guesses(&ctx, &regs, &[]).is_empty(),
            "注册表的 AI 猜测必须被安全规则全部拒绝"
        );
    }

    #[test]
    fn ai_guess_accepts_existing_app_dir_but_not_other_apps_dir() {
        let Some(base) = dirs::data_local_dir() else {
            return;
        };
        let dir = base.join("DemoAppVerifyTest");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let ctx = demo_ctx();
        let g = vec![guess(&dir.to_string_lossy(), "dir")];

        let ok = verify_ai_guesses(&ctx, &g, &[]);
        let blocked = verify_ai_guesses(&ctx, &g, &[dir.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(ok.len(), 1, "存在于应用数据根下的同名目录应被接受");
        assert!(ok[0].id.starts_with("ai:dir:"));
        assert!(
            blocked.is_empty(),
            "与其它已安装软件安装目录重合时必须拒绝"
        );
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
            aliases: vec!["someapplication".into()],
            path_hints: vec![],
            extra_roots: vec![],
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
            aliases: vec!["microsoftedge".into()],
            path_hints: vec![],
            extra_roots: vec![],
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
                ResidualKind::RegistryValue => {
                    let rest = it.id.strip_prefix("runvalue:").unwrap_or("");
                    let parts: Vec<&str> = rest.splitn(3, '|').collect();
                    assert_eq!(parts.len(), 3, "启动项 id 格式异常: {}", it.id);
                    assert!(
                        safety::validate_run_key_location(parts[0], parts[1]).is_ok(),
                        "扫描产生了不安全的启动项: {}",
                        it.id
                    );
                    assert!(
                        safety::validate_registry_value_name(parts[2]).is_ok(),
                        "扫描产生了不安全的启动项值名: {}",
                        it.id
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
