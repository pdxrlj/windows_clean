mod ai;
mod autostart;
mod capabilities;
mod config;
mod firewall;
mod registry;
mod residuals;
mod safety;
mod services;
mod store;
mod types;
mod uninstall;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use parking_lot::RwLock;
use tauri::{Emitter, State};

use crate::ai::Client as AiClient;
use crate::registry::enumerate_registry_apps;
use crate::residuals::ScanContext;
use crate::store::enumerate_store_apps;
use crate::types::*;
use crate::uninstall::run_uninstall;

/// 当前进程是否管理员权限。
#[cfg(windows)]
fn is_admin() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}
#[cfg(not(windows))]
fn is_admin() -> bool {
    false
}

/// 非 Windows 平台不请求提权。
#[cfg(not(windows))]
fn relaunch_elevated() -> bool {
    false
}

/// 以管理员身份重新启动当前程序并等待其退出（会弹出 UAC 授权框）。
///
/// 返回 true：用户同意并以提权实例运行完毕（当前实例可以退出）；
/// 返回 false：用户取消 UAC 或提权启动失败（当前实例继续以普通权限运行）。
#[cfg(windows)]
fn relaunch_elevated() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = OsStr::new("runas")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    // SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE：
    // 等待提权实例结束，且不要为它新开控制台窗口。
    info.fMask = SEE_MASK_NOCLOSEPROCESS | 0x0000_8000;
    info.lpVerb = PCWSTR(verb.as_ptr());
    info.lpFile = PCWSTR(file.as_ptr());
    info.nShow = 1; // SW_SHOWNORMAL

    if unsafe { ShellExecuteExW(&mut info) }.is_err() {
        return false; // 用户取消 UAC 或启动失败
    }
    if info.hProcess.is_invalid() {
        return false;
    }
    unsafe {
        // 原进程保持存活，dev 模式下的 Vite 服务不会被 tauri CLI 提前关闭。
        WaitForSingleObject(info.hProcess, INFINITE);
        let _ = CloseHandle(info.hProcess);
    }
    true
}

/// 扫描后的残留缓存：appId -> residuals
struct ResidualCache(Arc<RwLock<HashMap<String, Vec<ResidualItem>>>>);

/// 残留索引：只有本会话扫描确认过的项才允许删除。
/// 前端传来的 path / kind / id 一律不可信，删除时只按 id 从索引里取服务端记录。
#[derive(Default)]
struct ResidualIndexState {
    by_id: HashMap<String, ResidualItem>,
    /// app id -> 该应用最近一次扫描出的残留 id（用于重新扫描时清理旧索引）
    by_app: HashMap<String, std::collections::HashSet<String>>,
}
struct ResidualIndex(Arc<RwLock<ResidualIndexState>>);

/// 本会话刚刚卸载的应用（卸载后已从系统枚举中消失，但仍需要扫描/复查残留）。
/// 只保存服务端枚举出来的 InstalledApp，且受保护条目不会进入。
struct RecentlyRemoved(Arc<RwLock<HashMap<String, InstalledApp>>>);

fn cache_residuals(
    cache: &ResidualCache,
    index: &ResidualIndex,
    app_id: &str,
    items: &[ResidualItem],
) {
    cache.0.write().insert(app_id.to_string(), items.to_vec());
    let mut idx = index.0.write();
    // 重新扫描后，旧的（已不存在/已删除）残留 id 不能再保留删除权限。
    if let Some(old_ids) = idx.by_app.remove(app_id) {
        for id in old_ids {
            idx.by_id.remove(&id);
        }
    }
    let mut ids = std::collections::HashSet::new();
    for it in items {
        idx.by_id.insert(it.id.clone(), it.clone());
        ids.insert(it.id.clone());
    }
    idx.by_app.insert(app_id.to_string(), ids);
}

/// 卸载程序返回后，等待系统真正完成卸载（有的卸载器会先拉起子进程再立刻退出），
/// 避免在卸载仍在进行时扫描残留/给出“已卸载干净”误判。
fn wait_for_uninstall_settle(app_id: &str) {
    let started = std::time::Instant::now();
    loop {
        if find_installed_app(app_id).is_none() {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(15) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    // 再给文件系统/注册表一个短暂的稳定窗口。
    std::thread::sleep(std::time::Duration::from_millis(800));
}

/// 前端传来的 InstalledApp 不可信：必须与当前系统枚举结果按 id 完全匹配。
fn find_installed_app(id: &str) -> Option<InstalledApp> {
    if id.starts_with("store::") {
        enumerate_store_apps().into_iter().find(|a| a.id == id)
    } else {
        enumerate_registry_apps().into_iter().find(|a| a.id == id)
    }
}

fn protected_app_error(app: &InstalledApp) -> String {
    format!(
        "“{}”受保护，已拒绝卸载：{}",
        app.display_name,
        app.protected_reason
            .as_deref()
            .unwrap_or("系统组件或系统依赖")
    )
}

#[tauri::command(async)]
fn list_installed_apps() -> Vec<InstalledApp> {
    let mut v = enumerate_registry_apps();
    v.extend(enumerate_store_apps());
    v
}

#[tauri::command]
fn get_ai_config() -> AiConfig {
    config::get()
}

/// 当前进程是否以管理员身份运行（用于提示 dev/debug 版提权问题）。
#[tauri::command]
fn is_elevated() -> bool {
    is_admin()
}

#[tauri::command]
fn save_ai_config(cfg: AiConfig) -> Result<(), String> {
    config::save(cfg)
}

/// 返回环境变量探测结果：变量名 → { found, masked }。空值视为未找到。
#[tauri::command]
fn probe_env_vars(names: Vec<String>) -> HashMap<String, EnvProbe> {
    ai::probe_env_vars(&names)
}

/// 返回当前生效配置（key 已脱敏）+ 是否来自环境变量 + 配置文件路径。
#[tauri::command]
fn resolve_ai_effective() -> EffectiveAi {
    ai::resolve_effective()
}

/// 轻量连接测试：使用传入配置，不保存、不要求 enabled、最小请求、短超时。
#[tauri::command]
async fn test_ai_connection(cfg: AiConfig) -> Result<AiTestResult, String> {
    AiClient::new(cfg).test_connection().await
}

/// 拉取服务商可用模型列表；不支持时返回可操作错误提示。
#[tauri::command]
async fn list_ai_models(cfg: AiConfig) -> Result<Vec<String>, String> {
    AiClient::new(cfg).list_models().await
}

#[tauri::command]
async fn ai_analyze_app(app: InstalledApp) -> Result<AiAppInfo, String> {
    let client = AiClient::new(config::get());
    client.analyze_app(&app).await
}

#[tauri::command]
async fn ai_analyze_risk(app: InstalledApp) -> Result<AiUninstallRisk, String> {
    let client = AiClient::new(config::get());
    client.analyze_uninstall_risk(&app).await
}

#[tauri::command(async)]
fn uninstall_app(
    app: InstalledApp,
    silent: bool,
    cache: State<'_, ResidualCache>,
    index: State<'_, ResidualIndex>,
    recent: State<'_, RecentlyRemoved>,
    app_handle: tauri::AppHandle,
) -> Result<UninstallResult, String> {
    // 绝不使用前端传入的 uninstall_string / install_location：按 id 重新枚举。
    let real = find_installed_app(&app.id).ok_or_else(|| {
        "应用不在当前系统枚举列表中（可能已被卸载或条目被篡改），已拒绝执行".to_string()
    })?;
    if real.is_protected {
        return Err(protected_app_error(&real));
    }
    // 卸载后应用会从枚举中消失；记录下来供“扫描残留 / 复查是否卸载干净”使用。
    recent.0.write().insert(real.id.clone(), real.clone());

    app_handle.emit("uninstall-log", "开始卸载…").ok();
    let result = run_uninstall(&real, silent);
    app_handle
        .emit(
            "uninstall-log",
            format!("卸载结束 success={} code={:?}", result.success, result.exit_code),
        )
        .ok();

    if result.success {
        // 等待系统真正完成卸载（有些卸载器会拉起子进程后立刻退出）。
        wait_for_uninstall_settle(&real.id);
        let ctx = build_scan_context(&real);
        let residuals = residuals::scan_residuals(&ctx);
        cache_residuals(&cache, &index, &real.id, &residuals);
        app_handle
            .emit(
                "residuals-found",
                serde_json::json!({ "appId": real.id, "items": residuals }),
            )
            .ok();
    }
    Ok(result)
}

#[tauri::command(async)]
fn scan_residuals_for(
    app: InstalledApp,
    cache: State<'_, ResidualCache>,
    index: State<'_, ResidualIndex>,
    recent: State<'_, RecentlyRemoved>,
) -> Vec<ResidualItem> {
    // 优先使用当前枚举结果；若应用刚被卸载（已不在枚举里），退回到会话内记录。
    let real = find_installed_app(&app.id)
        .or_else(|| recent.0.read().get(&app.id).cloned());
    let Some(real) = real else {
        return vec![];
    };
    if real.is_protected {
        return vec![];
    }
    let ctx = build_scan_context(&real);
    let r = residuals::scan_residuals(&ctx);
    cache_residuals(&cache, &index, &real.id, &r);
    r
}

#[tauri::command]
async fn ai_assess_residuals(
    app: InstalledApp,
    residuals: Vec<ResidualItem>,
) -> Result<AiResidualAssessment, String> {
    let client = AiClient::new(config::get());
    client.assess_residuals(&app, &residuals).await
}

/// 删除指定残留项（用户已确认）。
/// 只按 id 从服务端索引取记录；索引里没有的项（伪造/过期）一律拒绝。
#[tauri::command(async)]
fn delete_residuals(
    items: Vec<ResidualItem>,
    index: State<'_, ResidualIndex>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<DeleteResult>, String> {
    let mut out = Vec::with_capacity(items.len());
    for requested in &items {
        let stored = index.0.read().by_id.get(&requested.id).cloned();
        let Some(item) = stored else {
            out.push(DeleteResult {
                id: requested.id.clone(),
                success: false,
                message: "该残留项未经本次扫描确认，已拒绝删除（请重新扫描后再试）".into(),
            });
            continue;
        };
        app_handle
            .emit(
                "delete-progress",
                serde_json::json!({ "id": item.id, "display": item.display }),
            )
            .ok();
        let (ok, msg) = residuals::delete_residual(&item);
        if ok {
            index.0.write().by_id.remove(&item.id);
        }
        out.push(DeleteResult {
            id: item.id,
            success: ok,
            message: msg,
        });
    }
    Ok(out)
}

#[tauri::command(async)]
fn get_cached_residuals(app_id: String, cache: State<'_, ResidualCache>) -> Vec<ResidualItem> {
    cache.0.read().get(&app_id).cloned().unwrap_or_default()
}

#[tauri::command(async)]
fn open_in_explorer(path: String) -> Result<(), String> {
    let p = path.trim();
    if p.is_empty() {
        return Err("路径为空".into());
    }
    if p.starts_with("HKLM") || p.starts_with("HKCU") {
        Command::new(safety::windows_exe("regedit.exe"))
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !Path::new(p).is_absolute() {
        return Err("只能打开绝对路径".into());
    }
    // 只允许打开“文件夹”：explorer.exe <文件> 会按 ShellExecute 关联执行，
    // 绝不能让渲染层借此启动任意程序；对文件则打开其所在目录。
    let target = std::fs::canonicalize(p).map_err(|_| "路径不存在".to_string())?;
    let dir = if target.is_dir() {
        target
    } else {
        target
            .parent()
            .map(|x| x.to_path_buf())
            .ok_or_else(|| "路径没有父目录".to_string())?
    };
    Command::new(safety::windows_exe("explorer.exe"))
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ===== 应用权限管理 =====

#[tauri::command(async)]
fn firewall_is_blocked(exe: String) -> bool {
    match safety::validate_firewall_target_allow_missing(&exe) {
        Ok(p) => firewall::is_blocked(&p.to_string_lossy()),
        Err(_) => false,
    }
}

#[tauri::command(async)]
fn firewall_block(exe: String) -> Result<String, String> {
    // 不允许把 Windows 系统程序（svchost、lsass 等）加入阻止规则。
    let target = safety::validate_firewall_target(&exe)?;
    let (ok, msg) = firewall::block(&target.to_string_lossy());
    if ok {
        Ok(msg)
    } else {
        Err(msg)
    }
}

#[tauri::command(async)]
fn firewall_unblock(exe: String) -> Result<String, String> {
    // 允许目标文件已被删除（仍需能清理旧规则），但拒绝系统目录/UNC 等危险路径。
    let target = safety::validate_firewall_target_allow_missing(&exe)?;
    let (ok, msg) = firewall::unblock(&target.to_string_lossy());
    if ok {
        Ok(msg)
    } else {
        Err(msg)
    }
}

#[tauri::command(async)]
fn firewall_list_managed() -> Vec<firewall::FirewallRule> {
    firewall::list_managed_rules()
}

/// Store 应用 capability 列表（所有 capability 的中文名）。
#[tauri::command]
fn list_capabilities() -> Vec<(String, String)> {
    capabilities::CAPABILITIES
        .iter()
        .map(|(c, l)| (c.to_string(), l.to_string()))
        .collect()
}

/// 读取某 UWP 包在所有 capability 下的当前值。
#[tauri::command(async)]
fn list_package_capabilities(package_family: String) -> Vec<capabilities::CapPermission> {
    capabilities::list_for_package(&package_family)
}

/// 设置某包某 capability 的值（Allow/Deny/Prompt）。
#[tauri::command(async)]
fn set_capability(cap: String, package_family: String, value: String) -> Result<String, String> {
    let (ok, msg) = capabilities::set_value(&cap, &package_family, &value);
    if ok {
        Ok(msg)
    } else {
        Err(msg)
    }
}

/// 从某应用安装目录/卸载串里推断主 exe（用于防火墙）。
/// 桌面应用：取 install_location 下与显示名同名/最大的 .exe。
#[tauri::command(async)]
fn guess_main_exe(app: InstalledApp) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(loc) = &app.install_location {
        let p = std::path::PathBuf::from(loc);
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                let mut exes: Vec<_> = rd
                    .flatten()
                    .filter(|e| {
                        e.file_type().map(|t| t.is_file()).unwrap_or(false)
                            && e.path().extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                    })
                    .map(|e| e.path())
                    .collect();
                exes.sort_by_key(|p| {
                    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
                });
                // 取最大的 3 个 exe 作为候选
                for e in exes.into_iter().rev().take(3) {
                    candidates.push(e.to_string_lossy().to_string());
                }
            }
        }
    }
    // 从卸载串里提取 exe
    if let Some(us) = &app.uninstall_string {
        if let Some(i) = us.to_ascii_lowercase().find(".exe") {
            let end = i + 4;
            // 向前找路径起始（引号或空格）
            let head = &us[..end];
            let start = head.rfind(['"', ' ', '/']).map(|x| x + 1).unwrap_or(0);
            let prog = us[start..end].trim_matches('"').to_string();
            if !candidates.contains(&prog) {
                candidates.push(prog);
            }
        }
    }
    candidates
}

// ===== 自启动管理 =====

#[tauri::command(async)]
fn list_autostart(app_name: Option<String>) -> Vec<autostart::AutostartEntry> {
    autostart::scan_autostart(app_name.as_deref())
}

#[tauri::command(async)]
fn disable_autostart(entry: autostart::AutostartEntry) -> Result<String, String> {
    let (ok, msg) = autostart::disable(&entry);
    if ok { Ok(msg) } else { Err(msg) }
}

#[tauri::command(async)]
fn enable_autostart(entry: autostart::AutostartEntry) -> Result<String, String> {
    let (ok, msg) = autostart::enable(&entry);
    if ok { Ok(msg) } else { Err(msg) }
}

#[tauri::command(async)]
fn remove_autostart(entry: autostart::AutostartEntry) -> Result<String, String> {
    let (ok, msg) = autostart::remove(&entry);
    if ok { Ok(msg) } else { Err(msg) }
}

fn build_scan_context(app: &InstalledApp) -> ScanContext {
    let publisher_key_hints = publisher_hints(app);
    ScanContext {
        app_name: app.display_name.clone(),
        publisher: app.publisher.clone(),
        install_location: app.install_location.clone(),
        publisher_key_hints,
    }
}

/// 从发布者名生成多个厂商目录线索。如 "Microsoft Corporation" -> ["microsoft", "microsoft corporation"]。
fn publisher_hints(app: &InstalledApp) -> Vec<String> {
    let mut v = vec![];
    if let Some(pub_name) = &app.publisher {
        let lower = pub_name.to_lowercase();
        v.push(lower.clone());
        if let Some(first) = lower.split(|c: char| c.is_whitespace() || c == ',').next() {
            if !first.is_empty() && first != lower {
                v.push(first.to_string());
            }
        }
        let stripped = lower
            .replace("corporation", "")
            .replace(",inc.", "")
            .replace(" inc.", "")
            .replace(" inc", "")
            .replace(" ltd.", "")
            .replace(" ltd", "")
            .replace(" co., ltd.", "")
            .replace(" limited", "")
            .trim()
            .to_string();
        if !stripped.is_empty() {
            v.push(stripped);
        }
    }
    v.push(app.display_name.to_lowercase());
    v.dedup();
    v.into_iter().filter(|s| !s.is_empty()).collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 启动即请求管理员权限：
    // - release 版有 requireAdministrator manifest，正常双击时 Windows 会先弹 UAC；
    // - dev/debug 版没有 manifest，这里主动弹 UAC 并以提权实例重新运行；
    // - 用户取消 UAC 时继续以普通权限运行（UI 顶部会提示功能可能失败）。
    if !is_admin() {
        if relaunch_elevated() {
            // 提权实例已正常退出，当前普通权限实例直接结束，避免出现两个窗口/进程。
            return;
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(ResidualCache(Arc::new(RwLock::new(HashMap::new()))))
        .manage(ResidualIndex(Arc::new(RwLock::new(
            ResidualIndexState::default(),
        ))))
        .manage(RecentlyRemoved(Arc::new(RwLock::new(HashMap::new()))))
        .invoke_handler(tauri::generate_handler![
            list_installed_apps,
            get_ai_config,
            is_elevated,
            save_ai_config,
            probe_env_vars,
            resolve_ai_effective,
            test_ai_connection,
            list_ai_models,
            ai_analyze_app,
            ai_analyze_risk,
            uninstall_app,
            scan_residuals_for,
            ai_assess_residuals,
            delete_residuals,
            get_cached_residuals,
            open_in_explorer,
            firewall_is_blocked,
            firewall_block,
            firewall_unblock,
            firewall_list_managed,
            list_capabilities,
            list_package_capabilities,
            set_capability,
            guess_main_exe,
            list_autostart,
            disable_autostart,
            enable_autostart,
            remove_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
