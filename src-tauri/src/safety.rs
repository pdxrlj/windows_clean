//! 安全护栏：所有破坏性操作的最后一道防线。
//!
//! 设计原则：即使前端被绕过、IPC 参数被伪造、或扫描规则出现误判，
//! 也绝不能删除 Windows 系统目录 / 系统级注册表 / 关键服务与计划任务，
//! 也绝不能卸载系统组件或系统依赖。
//!
//! 本模块只包含纯函数（外加少量只读环境变量查询），便于单元测试。

use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};

// ===== 基础规范化 =====

/// 统一成小写 + 反斜杠，去掉尾部多余分隔符（保留 `c:\` 形式的根）。
fn norm_path(s: &str) -> String {
    let mut out = s.trim().replace('/', "\\");
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    out.to_ascii_lowercase()
}

fn is_under(child: &str, parent: &str) -> bool {
    !parent.is_empty() && (child == parent || child.starts_with(&format!("{}\\", parent)))
}

fn env_norm(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| norm_path(&v))
        .filter(|v| !v.is_empty() && v.contains(':'))
}

fn windows_dirs() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(w) = env_norm("SystemRoot") {
        v.push(w);
    }
    if let Some(w) = env_norm("windir") {
        v.push(w);
    }
    v.push(r"c:\windows".into());
    v.sort();
    v.dedup();
    v
}

fn program_files_dirs() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = env_norm("ProgramFiles") {
        v.push(p);
    }
    if let Some(p) = env_norm("ProgramFiles(x86)") {
        v.push(p);
    }
    v.push(r"c:\program files".into());
    v.push(r"c:\program files (x86)".into());
    v.sort();
    v.dedup();
    v
}

fn program_data_dirs() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = env_norm("PROGRAMDATA") {
        v.push(p);
    }
    v.push(r"c:\programdata".into());
    v.sort();
    v.dedup();
    v
}

fn local_app_data_dirs() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = env_norm("LOCALAPPDATA") {
        v.push(p);
    }
    v
}

fn roaming_dirs() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = env_norm("APPDATA") {
        v.push(p);
    }
    v
}

fn profile_roots() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = env_norm("USERPROFILE") {
        v.push(p);
    }
    if let Some(home) = dirs::home_dir() {
        v.push(norm_path(&home.to_string_lossy()));
    }
    v.sort();
    v.dedup();
    v
}

pub fn start_menu_program_roots() -> Vec<String> {
    let mut v = Vec::new();
    for base in roaming_dirs().into_iter().chain(local_app_data_dirs()) {
        v.push(format!("{}\\microsoft\\windows\\start menu\\programs", base));
    }
    for base in program_data_dirs() {
        v.push(format!("{}\\microsoft\\windows\\start menu\\programs", base));
    }
    v
}

/// 这些目录本身（整目录）绝不允许作为删除目标。
fn exact_protected_roots() -> Vec<String> {
    let mut v = Vec::new();
    v.extend(windows_dirs());
    v.extend(program_files_dirs());
    v.extend(program_data_dirs());
    v.extend(local_app_data_dirs());
    v.extend(roaming_dirs());
    v.extend(profile_roots());
    v.push(r"c:\users".into());
    v.push(r"c:\".into());
    // 常见系统/引导目录（顶层名）
    for top in [
        r"c:\recovery",
        r"c:\perflogs",
        r"c:\boot",
        r"c:\efi",
        r"c:\$recycle.bin",
        r"c:\system volume information",
        r"c:\documents and settings",
    ] {
        v.push(top.to_string());
    }
    v.sort();
    v.dedup();
    v
}

/// 目录删除时额外禁止的系统子树。
fn protected_directory_subtrees() -> Vec<String> {
    let mut v = Vec::new();
    for pf in program_files_dirs() {
        v.push(format!("{}\\windowsapps", pf));
        v.push(format!("{}\\common files", pf));
        v.push(format!("{}\\windows defender", pf));
        v.push(format!("{}\\windows defender advanced threat protection", pf));
        v.push(format!("{}\\windows nt", pf));
        // Microsoft 系目录整体保护（含 Edge/Defender/Office 等共享目录）
        v.push(format!("{}\\microsoft", pf));
        v.push(format!("{}\\microsoft defender", pf));
    }
    for pd in program_data_dirs() {
        v.push(format!("{}\\microsoft", pd));
        v.push(format!("{}\\microsoft\\windows", pd));
        v.push(format!("{}\\microsoft\\windows defender", pd));
        v.push(format!("{}\\microsoft defender", pd));
        v.push(format!("{}\\packages", pd));
    }
    for la in local_app_data_dirs() {
        v.push(format!("{}\\microsoft", la));
        v.push(format!("{}\\microsoft\\windows", la));
        v.push(format!("{}\\microsoft\\windowsapps", la));
        v.push(format!("{}\\packages", la));
    }
    for ra in roaming_dirs() {
        v.push(format!("{}\\microsoft", ra));
        v.push(format!("{}\\microsoft\\windows", ra));
        v.push(format!("{}\\microsoft\\windowsapps", ra));
    }
    v.sort();
    v.dedup();
    v
}

// ===== 文件 / 目录删除 =====

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    File,
    Directory,
    Shortcut,
}

/// 逐段拒绝“Windows 会做特殊解释”的路径写法：结尾点/空格、8.3 短名、NTFS ADS。
fn bad_component(c: &str) -> bool {
    if c.is_empty() || c.ends_with('.') || c.ends_with(' ') || c.contains(':') {
        return true;
    }
    let bytes = c.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'~' && bytes[i + 1].is_ascii_digit() {
            return true;
        }
        i += 1;
    }
    false
}

fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// 纯字符串层面的路径校验（不含 canonicalize，便于对真实路径复用）。
fn validate_delete_path_shape(path: &str, kind: TargetKind) -> Result<(), String> {
    let raw = path.trim();
    if raw.is_empty() {
        return Err("路径为空".into());
    }
    if raw.contains('\0') || raw.contains('\r') || raw.contains('\n') {
        return Err("路径包含非法字符".into());
    }
    if raw.contains('%') {
        return Err("路径包含未展开的环境变量，已拒绝".into());
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err("拒绝相对路径".into());
    }
    let mut normal_count = 0usize;
    for comp in p.components() {
        match comp {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(_) => {}
                _ => return Err("拒绝 UNC / 非本地磁盘路径".into()),
            },
            Component::RootDir => {}
            Component::Normal(os) => {
                normal_count += 1;
                let s = os.to_string_lossy();
                if bad_component(&s) {
                    return Err(format!(
                        "路径片段包含 Windows 特殊解释字符（结尾点/空格、8.3 短名、冒号）: {}",
                        s
                    ));
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err("路径包含 . 或 ..，已拒绝".into());
            }
        }
    }
    if normal_count < 2 {
        return Err("拒绝删除驱动器根目录或层级过浅的路径".into());
    }
    let n = norm_path(raw);
    if n.contains("\\\\") {
        return Err("路径包含重复分隔符，已拒绝".into());
    }

    // Windows 目录及其子目录：绝对禁止
    for w in windows_dirs() {
        if is_under(&n, &w) {
            return Err("Windows 系统目录下的内容不允许删除".into());
        }
    }
    // 系统/用户根目录本身
    for r in exact_protected_roots() {
        if n == r {
            return Err("系统或用户根目录不允许删除".into());
        }
    }
    // 目录型额外保护（文件同样不允许落在这些系统子树里；快捷方式单独按开始菜单校验）
    if kind != TargetKind::Shortcut {
        for t in protected_directory_subtrees() {
            if is_under(&n, &t) {
                return Err("受保护的系统目录不允许删除".into());
            }
        }
    }
    // 快捷方式只允许开始菜单 Programs 下的 .lnk
    if kind == TargetKind::Shortcut {
        if !n.ends_with(".lnk") {
            return Err("快捷方式项必须是 .lnk 文件".into());
        }
        let in_start_menu = start_menu_program_roots()
            .iter()
            .any(|r| is_under(&n, r));
        if !in_start_menu {
            return Err("快捷方式不在开始菜单 Programs 目录内，已拒绝".into());
        }
    }
    Ok(())
}

/// 校验一个待删除的绝对路径是否安全。返回应当被删除的（已解析的）路径。
///
/// 先做字符串校验；若路径存在，再 canonicalize 解析掉 junction / symlink /
/// 8.3 短名 / 结尾点空格 / `\\?\` 前缀，然后对真实路径重复全部校验。
pub fn validate_delete_path(path: &str, kind: TargetKind) -> Result<PathBuf, String> {
    let raw = path.trim();
    validate_delete_path_shape(raw, kind)?;
    match fs::canonicalize(raw) {
        Ok(canon) => {
            let real = strip_verbatim(&canon.to_string_lossy());
            validate_delete_path_shape(&real, kind)?;
            Ok(PathBuf::from(real))
        }
        Err(_) => {
            // 路径存在却无法解析真实路径（权限/链接/损坏）：fail closed。
            if fs::symlink_metadata(raw).is_ok() {
                return Err("无法解析路径的真实位置（可能是链接或权限问题），已拒绝".into());
            }
            // 路径尚不存在（如刚被卸载/删除）：只能做字符串校验，删除时系统会自然失败。
            Ok(PathBuf::from(raw))
        }
    }
}

// ===== 注册表 =====

/// 校验一个待删除的注册表项是否安全（只允许 SOFTWARE 下的应用专属键）。
pub fn validate_registry_delete_path(root: &str, path: &str) -> Result<(), String> {
    let root = root.trim().to_ascii_uppercase();
    if root != "HKLM" && root != "HKCU" {
        return Err("只允许清理 HKCU / HKLM 下的应用残留项".into());
    }
    let p = path.trim().trim_matches('\\').replace('/', "\\");
    if p.is_empty() {
        return Err("注册表路径为空".into());
    }
    let segs: Vec<String> = p
        .split('\\')
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    if segs
        .iter()
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err("注册表路径包含非法片段".into());
    }
    if segs[0] != "software" {
        return Err("只允许清理 SOFTWARE 下的应用残留项".into());
    }
    // 系统级子树：绝对保护
    for s in &segs {
        if s == "microsoft" {
            return Err("Microsoft 系统注册表项受保护".into());
        }
        if matches!(
            s.as_str(),
            "classes"
                | "policies"
                | "systemcertificates"
                | "cryptography"
                | "windows"
                | "windows nt"
                | "currentversion"
                | "uninstall"
        ) {
            return Err("注册表路径命中受保护的系统子树".into());
        }
    }
    // 深度：不允许删厂商根键或整棵 SOFTWARE
    let min = if root == "HKLM" {
        if segs.get(1).map(|s| s.as_str()) == Some("wow6432node") {
            4
        } else {
            3
        }
    } else {
        2
    };
    if segs.len() < min {
        return Err("注册表路径层级过浅（疑似厂商根键），已拒绝".into());
    }
    Ok(())
}

// ===== 服务 / 计划任务 =====

/// Windows 关键服务：停/删任一都可能导致系统无法启动或功能失效。
const PROTECTED_SERVICES: &[&str] = &[
    "rpcss",
    "dcomlaunch",
    "rpceptmapper",
    "lsm",
    "power",
    "plugplay",
    "winmgmt",
    "trustedinstaller",
    "eventlog",
    "schedule",
    "profsvc",
    "usermanager",
    "themes",
    "sysmain",
    "audiosrv",
    "audiodg",
    "spooler",
    "windefend",
    "wscsvc",
    "securityhealthservice",
    "sense",
    "wdnissvc",
    "webthreatdefsvc",
    "webthreatdefusersvc",
    "mpssvc",
    "bfe",
    "mpsdrv",
    "wdfilter",
    "wdnisdrv",
    "nsi",
    "dhcp",
    "dnscache",
    "lanmanworkstation",
    "lanmanserver",
    "netman",
    "nla",
    "cryptsvc",
    "keyiso",
    "samss",
    "seclogon",
    "appinfo",
    "uxsms",
    "wuauserv",
    "bits",
    "wlansvc",
    "wcmsvc",
    "dosvc",
    "appxdeployment",
    "appxdeploymentserver",
    "staterepository",
    "deviceassociation",
    "devicemanager",
    "wudfsvc",
    "shellhwdetection",
    "sens",
    "diagtrack",
];

pub fn protected_service_name(name: &str) -> Option<&'static str> {
    let l = name.trim().to_ascii_lowercase();
    PROTECTED_SERVICES.iter().find(|s| **s == l).copied()
}

pub fn validate_service_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 256 {
        return Err("服务名非法".into());
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '$'))
    {
        return Err("服务名包含非法字符".into());
    }
    Ok(())
}

/// 从 `sc qc` 的 BINARY_PATH_NAME 中取出可执行文件路径（可能带参数、带引号）。
pub fn extract_executable_path(binary_path: &str) -> String {
    let s = binary_path.trim();
    if s.starts_with('"') {
        if let Some(end) = s[1..].find('"') {
            return s[1..1 + end].to_string();
        }
    }
    let lower = s.to_ascii_lowercase();
    if let Some(idx) = lower.find(".exe") {
        return s[..idx + 4].trim().to_string();
    }
    s.split_whitespace().next().unwrap_or("").to_string()
}

/// 校验能否删除某个服务：必须是非系统服务，且二进制不在 Windows 目录/驱动目录。
pub fn validate_service_deletion(name: &str, binary_path: Option<&str>) -> Result<(), String> {
    validate_service_name(name)?;
    if let Some(p) = protected_service_name(name) {
        return Err(format!("系统关键服务 {} 受保护，已拒绝删除", p));
    }
    let bp = binary_path
        .map(extract_executable_path)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "无法验证服务的可执行文件路径，已拒绝删除".to_string())?;
    if bp.contains('%') {
        return Err("服务路径包含未展开的环境变量，已拒绝删除".into());
    }
    let lower = norm_path(&bp);
    if lower.ends_with("\\drivers\\") || lower.ends_with(".sys") {
        return Err("驱动服务受保护，已拒绝删除".into());
    }
    validate_delete_path(&bp, TargetKind::File)?;
    Ok(())
}

/// 校验能否删除某个计划任务：禁止 Windows 内置任务目录。
///
/// 注意 `schtasks` 会接受不带前导 `\` 的相对名字（如 `Microsoft\Windows\...`），
/// 因此这里先把名字规范成以 `\` 开头的绝对形式，再按路径段判断。
pub fn validate_task_deletion(name: &str) -> Result<(), String> {
    let raw = name.trim();
    if raw.is_empty() || raw.len() > 512 {
        return Err("计划任务名非法".into());
    }
    if raw.contains('\0') || raw.contains('\r') || raw.contains('\n') {
        return Err("计划任务名包含非法字符".into());
    }
    let canon = raw.replace('/', "\\");
    if !canon.starts_with('\\') {
        return Err("计划任务名必须是绝对路径（以 \\ 开头），已拒绝".into());
    }
    let lower = canon.to_ascii_lowercase();
    let segs: Vec<&str> = lower.split('\\').skip(1).filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Err("计划任务名非法".into());
    }
    if lower.contains("\\\\") {
        return Err("计划任务名包含重复分隔符，已拒绝".into());
    }
    for s in &segs {
        if *s == "microsoft" || s.starts_with("windows") || s.contains("defender") {
            return Err("Windows 内置计划任务受保护，已拒绝删除".into());
        }
    }
    Ok(())
}

// ===== 卸载命令 =====

/// 这些程序作为“卸载入口”一律拒绝，避免被当成任意命令执行器。
const DENY_PROGRAMS: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "wscript.exe",
    "cscript.exe",
    "mshta.exe",
    "rundll32.exe",
    "regsvr32.exe",
    "reg.exe",
    "regedit.exe",
    "wmic.exe",
    "diskpart.exe",
    "bcdedit.exe",
    "bcdboot.exe",
    "format.com",
    "shutdown.exe",
    "taskkill.exe",
    "net.exe",
    "net1.exe",
    "takeown.exe",
    "icacls.exe",
    "schtasks.exe",
    "sc.exe",
    "attrib.exe",
    "del.exe",
    "erase.exe",
    "rd.exe",
    "rmdir.exe",
    "mountvol.exe",
    "fsutil.exe",
    "vssadmin.exe",
    "wbadmin.exe",
    "dism.exe",
    "sfc.exe",
    "pnputil.exe",
    "certutil.exe",
    "bitsadmin.exe",
    "bootrec.exe",
    "reagentc.exe",
];

const DENY_PROGRAM_EXTS: &[&str] = &["bat", "cmd", "ps1", "vbs", "vbe", "js", "jse", "wsf", "hta", "scr", "msi", "com"];

fn file_name_lower(p: &str) -> String {
    p.trim()
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn extension_lower(p: &str) -> String {
    let name = file_name_lower(p);
    name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default()
}

/// 校验卸载调用：程序 + 参数是否允许以管理员身份执行。
pub fn validate_uninstall_invocation(program: &str, args: &[String]) -> Result<(), String> {
    let prog = program.trim();
    if prog.is_empty() {
        return Err("卸载程序为空".into());
    }
    if prog.contains('\0') || prog.contains('\r') || prog.contains('\n') {
        return Err("卸载程序包含非法字符".into());
    }
    let base = file_name_lower(prog);
    if base.is_empty() {
        return Err("无法识别卸载程序".into());
    }

    let is_msiexec = base == "msiexec.exe";
    if !is_msiexec {
        if DENY_PROGRAMS.contains(&base.as_str()) {
            return Err(format!("拒绝执行系统工具作为卸载程序: {}", base));
        }
        let ext = extension_lower(prog);
        if DENY_PROGRAM_EXTS.contains(&ext.as_str()) {
            return Err(format!("拒绝执行脚本/安装包作为卸载程序: .{}", ext));
        }
    }

    // msiexec 只允许卸载操作（/x、/uninstall），且必须是 System32 下的 msiexec
    if is_msiexec {
        let bare_ok = prog.eq_ignore_ascii_case("msiexec.exe");
        let sys_msiexec = norm_path(&system32_exe("msiexec.exe"));
        let prog_norm = norm_path(prog);
        if !bare_ok && prog_norm != sys_msiexec {
            return Err("msiexec 必须是 System32 下的 msiexec.exe，已拒绝".into());
        }
        let has_uninstall = args.iter().any(|a| {
            let l = a.trim().to_ascii_lowercase();
            l.starts_with("/x")
                || l.starts_with("-x")
                || l.starts_with("/uninstall")
                || l.starts_with("-uninstall")
        });
        if !has_uninstall {
            return Err("msiexec 只允许以 /x 卸载方式执行".into());
        }
        return Ok(());
    }

    // 拒绝 UNC / 非本地磁盘路径（避免借网络路径执行或删除）
    for comp in Path::new(prog).components() {
        if let Component::Prefix(prefix) = comp {
            if !matches!(prefix.kind(), Prefix::Disk(_)) {
                return Err("拒绝 UNC / 非本地磁盘路径".into());
            }
        }
    }

    // Windows 目录下的可执行文件一律拒绝（系统程序不是应用卸载器）
    let prog_norm = norm_path(prog);
    for w in windows_dirs() {
        if is_under(&prog_norm, &w) {
            return Err("拒绝执行 Windows 系统目录下的程序作为卸载入口".into());
        }
    }
    // 卸载程序本身必须能在本地磁盘上定位（允许不存在，但必须是绝对路径或裸文件名）
    if prog.contains('%') {
        return Err("卸载程序路径包含未展开的环境变量，已拒绝".into());
    }
    if !Path::new(prog).is_absolute() {
        return Err("卸载程序不是绝对路径，已拒绝执行".into());
    }
    Ok(())
}

// ===== Store / Appx =====

/// Appx 包全名只允许安全字符，防止 PowerShell 字符串注入。
pub fn validate_appx_package_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 256 {
        return Err("Appx 包名非法".into());
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '~'))
    {
        return Err("Appx 包名包含非法字符，已拒绝执行".into());
    }
    if !n.contains('_') {
        return Err("Appx 包名格式异常，已拒绝执行".into());
    }
    Ok(())
}

/// 已知的 Store 系统包（删掉会破坏开始菜单/商店/设置/安全中心等）。
const PROTECTED_STORE_PACKAGES: &[&str] = &[
    "microsoft.windows.shellexperiencehost",
    "microsoft.windows.startmenuexperiencehost",
    "microsoft.windows.search",
    "microsoft.windowsstore",
    "microsoft.storepurchaseapp",
    "microsoft.desktopappinstaller",
    "microsoft.windows.secureassessmentbrowser",
    "microsoft.aad.brokerplugin",
    "microsoft.accountscontrol",
    "microsoft.credentialdialoghost",
    "microsoft.windows.cortana",
    "microsoft.windows.client.cbs",
    "microsoft.windows.client.core",
    "microsoft.windows.client.shell",
    "microsoft.sechealthui",
    "microsoft.windows.defender",
    "microsoft.windows.security",
    "microsoft.windows.immersivecontrolpanel",
    "microsoft.ui.xaml",
    "microsoft.vclibs",
    "microsoft.services.store.engagement",
];

/// 返回保护原因；None 表示可以正常卸载。
pub fn classify_store_package(
    name: &str,
    non_removable: bool,
    signature_kind: &str,
    is_framework: bool,
) -> Option<String> {
    if non_removable {
        return Some("Store 系统包（NonRemovable）".into());
    }
    if signature_kind.eq_ignore_ascii_case("system") {
        return Some("Store 系统包（SignatureKind=System）".into());
    }
    if is_framework {
        return Some("系统框架包（IsFramework，其他应用依赖）".into());
    }
    let l = name.trim().to_ascii_lowercase();
    if PROTECTED_STORE_PACKAGES
        .iter()
        .any(|p| l == *p || l.starts_with(&format!("{}.", p)))
    {
        return Some("系统内置 Store 组件（删除可能导致系统功能异常）".into());
    }
    None
}

// ===== 受保护应用判定 =====

const RUNTIME_HINTS: &[&str] = &[
    "visual c++ redistributable",
    "visual c++ 2015",
    "microsoft visual c++",
    ".net runtime",
    ".net desktop runtime",
    "windows desktop runtime",
    "asp.net core runtime",
    "dotnet runtime",
    "webview2 runtime",
    "microsoft .net host",
    "microsoft .net runtime",
    "microsoft .net sdk",
    "windows software development kit",
    "windows sdk",
    "microsoft update health tools",
    "microsoft windows desktop runtime",
    "windows package manager",
    "app installer",
    "directx",
];

const UPDATE_HINTS: &[&str] = &[
    "update for ",
    "security update",
    "hotfix",
    "cumulative update",
    "microsoft .net framework",
    "windows malicious software removal tool",
    "microsoft edge update",
    "windows 更新",
    "更新程序",
    "补丁",
];

/// 这些名称属于 Windows 内置/系统级应用，卸载可能影响系统功能。
const PROTECTED_APP_NAMES: &[&str] = &[
    "microsoft edge",
    "microsoft edge webview2 runtime",
    "microsoft defender",
    "windows defender",
    "windows security",
    "microsoft store",
    "microsoft edge update",
];

/// 判定一个注册表 Uninstall 条目是否受保护；返回保护原因。
pub fn classify_registry_app(
    display_name: &str,
    publisher: Option<&str>,
    registry_subkey: Option<&str>,
    is_system_component: bool,
) -> Option<String> {
    if is_system_component {
        return Some("系统组件（SystemComponent=1）".into());
    }
    let name = display_name.trim();
    let lower = name.to_ascii_lowercase();
    if lower.is_empty() {
        return Some("无法识别的空名称条目".into());
    }
    if PROTECTED_APP_NAMES.iter().any(|p| {
        lower == *p || lower.starts_with(&format!("{} ", p)) || lower.starts_with(&format!("{}(", p))
    }) {
        return Some("Windows 系统组件/内置浏览器，卸载可能影响系统功能".into());
    }
    let sub = registry_subkey
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if sub.starts_with("kb") || sub.contains("\\kb") {
        return Some("Windows 更新/补丁（KB 条目）".into());
    }
    if let Some(p) = publisher {
        let pl = p.to_ascii_lowercase();
        if pl.contains("microsoft") && lower.starts_with("windows driver package") {
            return Some("系统驱动包".into());
        }
    }
    if lower.starts_with("windows driver package") || lower.contains("驱动程序包") {
        return Some("系统驱动包（删除可能导致设备不可用）".into());
    }
    if UPDATE_HINTS.iter().any(|h| lower.contains(h)) {
        return Some("Windows 更新/系统补丁".into());
    }
    if RUNTIME_HINTS.iter().any(|h| lower.contains(h)) {
        return Some("系统运行库/开发依赖（卸载可能导致其他软件无法运行）".into());
    }
    if lower.starts_with("microsoft windows ") {
        return Some("Windows 系统组件".into());
    }
    None
}

// ===== 系统工具绝对路径（避免从当前目录/PATH 加载被替换的程序） =====

fn windows_root() -> String {
    std::env::var("SystemRoot")
        .ok()
        .map(|s| s.trim().trim_end_matches('\\').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r"C:\Windows".into())
}

/// `%SystemRoot%\System32\<name>`
pub fn system32_exe(name: &str) -> String {
    format!(r"{}\System32\{}", windows_root(), name)
}

/// `%SystemRoot%\<name>`（如 explorer.exe / regedit.exe）
pub fn windows_exe(name: &str) -> String {
    format!(r"{}\{}", windows_root(), name)
}

/// Windows PowerShell 的绝对路径。
pub fn powershell_exe() -> String {
    format!(
        r"{}\System32\WindowsPowerShell\v1.0\powershell.exe",
        windows_root()
    )
}

// ===== 自启动 Run 键 / 防火墙 =====

/// 允许操作的自启动注册表位置（只读扫描与写操作都只允许这些键）。
pub const RUN_KEYS: &[(&str, &str)] = &[
    ("HKCU", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"),
    ("HKLM", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"),
    ("HKLM", r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run"),
    ("HKCU", r"SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce"),
    ("HKLM", r"SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce"),
];

/// 校验自启动写操作的位置：必须是上面固定的 5 个 Run/RunOnce 键。
pub fn validate_run_key_location(root: &str, path: &str) -> Result<(), String> {
    let ok = RUN_KEYS.iter().any(|(r, p)| {
        r.eq_ignore_ascii_case(root.trim()) && p.eq_ignore_ascii_case(path.trim())
    });
    if ok {
        Ok(())
    } else {
        Err("只允许操作 Windows Run/RunOnce 自启动键，已拒绝".into())
    }
}

/// 校验注册表值名（自启动项名称）：不允许控制字符，长度受限。
pub fn validate_registry_value_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 255 {
        return Err("注册表值名非法".into());
    }
    if n.chars().any(|c| c.is_control()) {
        return Err("注册表值名包含控制字符".into());
    }
    Ok(())
}

/// 防火墙目标形状校验：本地绝对路径 .exe，且不在 Windows 系统目录下。
/// 与“删除文件”的校验不同：这里不禁止 Program Files\Microsoft 等应用目录，
/// 因为阻止 Office/Edge 等程序联网是合法需求，且此操作不删除任何文件。
fn validate_firewall_shape(exe: &str) -> Result<PathBuf, String> {
    let raw = exe.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("可执行文件路径为空".into());
    }
    if raw.contains('%') {
        return Err("路径包含未展开的环境变量，已拒绝".into());
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err("只允许使用绝对路径的 exe".into());
    }
    if !p
        .extension()
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
    {
        return Err("只允许对 .exe 应用设置防火墙规则".into());
    }
    for comp in p.components() {
        if let Component::Prefix(prefix) = comp {
            if !matches!(prefix.kind(), Prefix::Disk(_)) {
                return Err("拒绝 UNC / 非本地磁盘路径".into());
            }
        }
    }
    // 存在则解析真实路径，拒绝通过 junction/短名等指向系统目录的路径。
    let resolved = match fs::canonicalize(p) {
        Ok(c) => strip_verbatim(&c.to_string_lossy()),
        Err(_) => raw.to_string(),
    };
    let n = norm_path(&resolved);
    for w in windows_dirs() {
        if is_under(&n, &w) {
            return Err("不允许对 Windows 系统程序设置/修改防火墙规则".into());
        }
    }
    Ok(PathBuf::from(resolved))
}

/// 添加阻止规则时：目标 exe 必须真实存在。
pub fn validate_firewall_target(exe: &str) -> Result<PathBuf, String> {
    let p = validate_firewall_shape(exe)?;
    if !p.is_file() {
        return Err("可执行文件不存在，已拒绝".into());
    }
    Ok(p)
}

/// 恢复联网时：允许目标文件已被卸载/删除（规则仍可清理）。
pub fn validate_firewall_target_allow_missing(exe: &str) -> Result<PathBuf, String> {
    validate_firewall_shape(exe)
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(p: &str, kind: TargetKind) -> bool {
        validate_delete_path(p, kind).is_ok()
    }

    #[test]
    fn rejects_windows_and_system_roots() {
        for p in [
            r"C:\Windows",
            r"C:\Windows\System32",
            r"C:\Windows\System32\drivers",
            r"C:\Windows\WinSxS",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"C:\Users",
            r"C:\",
            r"C:\Windows\Temp",
        ] {
            assert!(!valid(p, TargetKind::Directory), "应拒绝: {}", p);
        }
    }

    #[test]
    fn rejects_traversal_and_relative() {
        for p in [
            r"C:\Program Files\..\Windows",
            r"..\Windows",
            r"Windows\System32",
            r"C:\Program Files\App\..\..\Windows",
            r"\\?\C:\Windows",
            // Windows 特殊解释：结尾点 / 结尾空格 / 8.3 短名 / ADS
            r"C:\Windows.\System32",
            r"C:\Windows \System32",
            r"C:\PROGRA~1\Common Files",
            r"C:\Program Files\App\file.txt:stream",
        ] {
            assert!(!valid(p, TargetKind::Directory), "应拒绝: {}", p);
        }
    }

    #[test]
    fn rejects_too_shallow_paths() {
        assert!(!valid(r"C:\SomeApp", TargetKind::Directory));
        assert!(!valid(r"D:\", TargetKind::Directory));
        assert!(valid(r"C:\Program Files\SomeApp", TargetKind::Directory));
        assert!(valid(r"D:\Games\SomeApp", TargetKind::Directory));
    }

    #[test]
    fn rejects_protected_directory_subtrees() {
        for p in [
            r"C:\Program Files\WindowsApps\Microsoft.Foo",
            r"C:\Program Files\Common Files\Foo",
            r"C:\ProgramData\Microsoft",
            r"C:\ProgramData\Microsoft\Windows\Foo",
            r"C:\ProgramData\Packages\Foo",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files\Microsoft\Edge",
            r"C:\Program Files\Windows Defender\Foo",
            r"C:\Program Files\Windows Defender Advanced Threat Protection\Foo",
            r"C:\Program Files (x86)\Microsoft\Edge",
        ] {
            assert!(!valid(p, TargetKind::Directory), "应拒绝: {}", p);
        }
        // 文件同样不允许落在这些系统子树
        assert!(!valid(
            r"C:\ProgramData\Microsoft\Windows Defender\Platform\MsMpEng.exe",
            TargetKind::File
        ));
    }

    #[test]
    fn allows_normal_app_dirs_and_files() {
        assert!(valid(r"C:\Program Files\Foo\Bar", TargetKind::Directory));
        assert!(valid(r"C:\Program Files\Foo\uninstall.exe", TargetKind::File));
        assert!(valid(r"C:\Users\User\AppData\Local\Foo\cache", TargetKind::Directory));
    }

    #[test]
    fn shortcut_must_be_lnk_under_start_menu() {
        let la = local_app_data_dirs()
            .into_iter()
            .next()
            .unwrap_or_else(|| r"C:\Users\User\AppData\Local".into());
        let p = format!(r"{}\Microsoft\Windows\Start Menu\Programs\Foo.lnk", la);
        assert!(validate_delete_path(&p, TargetKind::Shortcut).is_ok());
        assert!(
            validate_delete_path(&p.replace(".lnk", ".exe"), TargetKind::Shortcut).is_err()
        );
        assert!(validate_delete_path(r"C:\Program Files\Foo\Bar.lnk", TargetKind::Shortcut).is_err());
    }

    #[test]
    fn registry_guard_blocks_system_trees() {
        for (root, p) in [
            ("HKLM", r"SOFTWARE\Microsoft"),
            ("HKLM", r"SOFTWARE\Microsoft\Windows"),
            ("HKLM", r"SOFTWARE\Microsoft\Windows NT"),
            ("HKLM", r"SOFTWARE\Classes\Foo"),
            ("HKLM", r"SOFTWARE\Policies\Foo"),
            ("HKLM", r"SYSTEM\CurrentControlSet\Services"),
            ("HKCU", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"),
            ("HKLM", r"SOFTWARE\Google"),
            ("HKLM", r"SOFTWARE\WOW6432Node\Google"),
        ] {
            assert!(
                validate_registry_delete_path(root, p).is_err(),
                "应拒绝: {} {}",
                root,
                p
            );
        }
    }

    #[test]
    fn registry_guard_allows_app_specific_keys() {
        for (root, p) in [
            ("HKCU", r"SOFTWARE\Slack"),
            ("HKCU", r"SOFTWARE\Vendor\App"),
            ("HKLM", r"SOFTWARE\Vendor\App"),
            ("HKLM", r"SOFTWARE\WOW6432Node\Vendor\App"),
        ] {
            assert!(
                validate_registry_delete_path(root, p).is_ok(),
                "应允许: {} {}",
                root,
                p
            );
        }
        assert!(validate_registry_delete_path("HKCR", r"SOFTWARE\Foo\Bar").is_err());
    }

    #[test]
    fn service_guard_blocks_critical_and_system_binaries() {
        assert!(validate_service_deletion("RpcSs", Some(r"C:\Windows\System32\svchost.exe")).is_err());
        assert!(validate_service_deletion("WinDefend", Some(r"C:\Program Files\App\a.exe")).is_err());
        assert!(validate_service_deletion(
            "FooSvc",
            Some(r"C:\Windows\System32\foo.exe")
        )
        .is_err());
        assert!(validate_service_deletion(
            "FooSvc",
            Some(r"C:\Program Files\Foo\foo.exe")
        )
        .is_ok());
        assert!(validate_service_deletion("FooSvc", None).is_err());
        assert!(validate_service_deletion("bad name!", Some(r"C:\Program Files\Foo\foo.exe")).is_err());
        assert!(validate_service_deletion(
            "FooSvc",
            Some(r"C:\Program Files\Foo\driver.sys")
        )
        .is_err());
    }

    #[test]
    fn task_guard_blocks_builtin_tasks() {
        for t in [
            r"\Microsoft\Windows\WindowsUpdate\Scheduled Start",
            r"\Microsoft\Windows Defender\Scan",
            r"\Microsoft",
            // schtasks 会接受不带前导反斜杠的相对名字
            r"Microsoft\Windows Defender\Scan",
            r"Microsoft\WindowsUpdate\X",
            r"Microsoft\Office\OfficeTelemetryAgentLogOn",
            "/Microsoft/Windows/Defrag/ScheduledDefrag",
        ] {
            assert!(validate_task_deletion(t).is_err(), "应拒绝: {}", t);
        }
        assert!(validate_task_deletion(r"\MyApp\Updater").is_ok());
        assert!(validate_task_deletion(r"\MyApp\WindowsHelper").is_err());
    }

    #[test]
    fn uninstall_guard_blocks_shells_and_system_binaries() {
        for p in [
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\System32\rundll32.exe",
            r"C:\Windows\System32\regsvr32.exe",
            r"C:\Users\User\Downloads\evil.bat",
            r"C:\Users\User\Downloads\evil.ps1",
            r"C:\Windows\System32\msiexec.exe",
            // 改名的 msiexec / UNC 程序 / 带结尾空格的引号程序
            r"C:\Users\User\AppData\Roaming\msiexec.exe",
            r"\\server\share\setup.exe",
        ] {
            assert!(
                validate_uninstall_invocation(p, &[]).is_err(),
                "应拒绝: {}",
                p
            );
        }
        assert!(validate_uninstall_invocation(
            r"C:\Users\User\AppData\Roaming\msiexec.exe",
            &[r"/x{11111111-1111-1111-1111-111111111111}".into()]
        )
        .is_err());
    }

    #[test]
    fn uninstall_guard_allows_normal_uninstallers() {
        assert!(validate_uninstall_invocation(
            r"C:\Program Files\Foo\uninstall.exe",
            &["/S".into()]
        )
        .is_ok());
        assert!(validate_uninstall_invocation(
            r"C:\Windows\System32\msiexec.exe",
            &[r"/x{11111111-1111-1111-1111-111111111111}".into()]
        )
        .is_ok());
        assert!(validate_uninstall_invocation(
            r"C:\Windows\System32\msiexec.exe",
            &[r"/i{11111111-1111-1111-1111-111111111111}".into()]
        )
        .is_err());
    }

    #[test]
    fn appx_name_guard_blocks_injection() {
        assert!(validate_appx_package_name("Microsoft.Foo_1.0.0.0_x64__8wekyb3d8bbwe").is_ok());
        // 资源包可能带 ~ 后缀
        assert!(validate_appx_package_name("Microsoft.Foo_1.0.0.0_x64__8wekyb3d8bbwe~resource").is_ok());
        for bad in [
            "Microsoft.Foo'; Remove-Item C:\\ -Recurse;'",
            "Microsoft.Foo 1.0",
            "Microsoft.Foo`whoami`",
            "",
        ] {
            assert!(validate_appx_package_name(bad).is_err(), "应拒绝: {}", bad);
        }
    }

    #[test]
    fn protected_store_packages() {
        assert!(classify_store_package("Microsoft.Foo", true, "Store", false).is_some());
        assert!(classify_store_package("Microsoft.Foo", false, "System", false).is_some());
        assert!(classify_store_package("Microsoft.Foo", false, "Store", true).is_some());
        assert!(
            classify_store_package("Microsoft.Windows.ShellExperienceHost", false, "Store", false)
                .is_some()
        );
        assert!(classify_store_package("Contoso.App", false, "Store", false).is_none());
    }

    #[test]
    fn protected_app_classification() {
        assert!(classify_registry_app("Foo", None, None, true).is_some());
        assert!(classify_registry_app("Security Update for Windows (KB1234567)", None, Some("KB1234567"), false).is_some());
        assert!(classify_registry_app("Update for Windows 10", None, None, false).is_some());
        assert!(classify_registry_app("Windows Driver Package - NVIDIA", None, None, false).is_some());
        assert!(classify_registry_app(
            "Microsoft Visual C++ 2015-2022 Redistributable (x64)",
            Some("Microsoft Corporation"),
            None,
            false
        )
        .is_some());
        assert!(classify_registry_app(
            "Microsoft Windows Desktop Runtime",
            Some("Microsoft Corporation"),
            None,
            false
        )
        .is_some());
        assert!(classify_registry_app("Google Chrome", Some("Google LLC"), None, false).is_none());
        assert!(classify_registry_app("Slack", Some("Slack Technologies"), None, false).is_none());
    }

    #[test]
    fn extract_executable_handles_quotes_and_args() {
        assert_eq!(
            extract_executable_path(r#""C:\Program Files\Foo\svc.exe" -k run"#),
            r"C:\Program Files\Foo\svc.exe"
        );
        assert_eq!(
            extract_executable_path(r"C:\Program Files\Foo\svc.exe -k run"),
            r"C:\Program Files\Foo\svc.exe"
        );
    }
}
