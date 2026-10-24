mod ai;
mod autostart;
mod capabilities;
mod config;
mod firewall;
mod history;
mod registry;
mod residuals;
mod restore;
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

/// 收集“仍与这个应用相关”的进程（安装目录内、或卸载器所在目录内）。
/// 用来识别卸载器父进程退出后，仍留在后台继续卸载的子进程/卸载窗口。
fn related_uninstall_processes(app: &InstalledApp) -> Vec<String> {
    use sysinfo::{ProcessesToUpdate, System};

    let mut needles: Vec<String> = Vec::new();
    if let Some(loc) = app.install_location.as_deref() {
        let n = loc.trim_end_matches(['\\', '/']).to_ascii_lowercase();
        if n.len() >= 6 {
            needles.push(n);
        }
    }
    for raw in [app.uninstall_string.as_deref(), app.quiet_uninstall_string.as_deref()]
        .into_iter()
        .flatten()
    {
        let (program, _) = crate::uninstall::split_command(raw);
        if let Some(parent) = std::path::Path::new(&program).parent() {
            let n = parent
                .to_string_lossy()
                .trim_end_matches(['\\', '/'])
                .to_ascii_lowercase();
            if n.len() >= 6 {
                needles.push(n);
            }
        }
    }
    needles.sort();
    needles.dedup();
    if needles.is_empty() {
        return Vec::new();
    }

    let path_is_under = |path: &str, base: &str| {
        path == base
            || path.starts_with(&format!("{}\\", base))
            || path.starts_with(&format!("{}/", base))
    };

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let self_pid = std::process::id();
    let mut out = Vec::new();
    for (pid, p) in sys.processes() {
        if pid.as_u32() == self_pid {
            continue;
        }
        let exe = p
            .exe()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let cmd = p
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        let hit = needles
            .iter()
            .any(|n| (!exe.is_empty() && path_is_under(&exe, n)) || cmd.contains(n));
        if hit {
            let name = p.name().to_string_lossy().to_string();
            out.push(if name.is_empty() { exe } else { name });
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 卸载器返回后，等待卸载真正完成：
/// - 应用从“已安装列表”中消失；
/// - 安装目录 / 卸载器目录下没有仍在运行的相关进程（含卸载器拉起的子进程）。
/// 返回 true 表示已确认完成；false 表示等待超时（可能仍在后台卸载）。
fn wait_for_uninstall_finish(app: &InstalledApp, app_handle: &tauri::AppHandle) -> bool {
    use std::time::{Duration, Instant};

    let started = Instant::now();
    let mut last_log = Instant::now() - Duration::from_secs(10);
    loop {
        let installed = find_installed_app(&app.id).is_some();
        let procs = related_uninstall_processes(app);
        if !installed && procs.is_empty() {
            // 再给文件系统/注册表一个短暂的稳定窗口。
            std::thread::sleep(Duration::from_millis(1200));
            return true;
        }
        if started.elapsed() > Duration::from_secs(600) {
            return false;
        }
        if last_log.elapsed() > Duration::from_secs(3) {
            let msg = if procs.is_empty() {
                "卸载程序已退出，但软件仍在已安装列表中，继续等待卸载完成…".to_string()
            } else {
                let shown: Vec<String> = procs.iter().take(4).cloned().collect();
                format!(
                    "卸载程序已退出，检测到卸载器/应用进程仍在运行（{}），继续等待卸载完成…",
                    shown.join("、")
                )
            };
            app_handle.emit("uninstall-log", msg).ok();
            last_log = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
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
    let mut result = run_uninstall(&real, silent);
    app_handle
        .emit(
            "uninstall-log",
            format!("卸载结束 success={} code={:?}", result.success, result.exit_code),
        )
        .ok();

    let mut scanned: Vec<ResidualItem> = Vec::new();
    if result.success {
        app_handle
            .emit(
                "uninstall-log",
                "卸载程序已退出，正在确认卸载是否真正完成（等待应用进程与安装记录消失）…",
            )
            .ok();
        let finished = wait_for_uninstall_finish(&real, &app_handle);
        if finished {
            let ctx = build_scan_context(&real);
            let residuals = residuals::scan_residuals(&ctx);
            cache_residuals(&cache, &index, &real.id, &residuals);
            scanned = residuals.clone();
            app_handle
                .emit(
                    "residuals-found",
                    serde_json::json!({ "appId": real.id, "items": residuals }),
                )
                .ok();
        } else {
            // 卸载器仍在后台运行：不自动扫描（避免扫描/删除正在被卸载的文件），
            // 明确告知用户等待完成后再手动扫描。
            result.success = false;
            result.requires_reboot = true;
            result.log.push_str(
                "\n\n警告：卸载程序已退出，但等待 10 分钟仍未检测到卸载完成。\n\
                 软件自带的卸载器可能仍在后台运行（例如仍在删除文件或等待操作）。\n\
                 请等待其窗口/进程结束后，再点击「扫描残留」；必要时重启电脑后再扫描。",
            );
        }
    }
    // 记录本地卸载历史：用于之后重新扫描残留 / AI 复查（即使程序重启）。
    if let Err(e) = history::record(&real, &result, &scanned) {
        app_handle
            .emit("uninstall-log", format!("写入卸载历史失败: {}", e))
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

#[tauri::command(async)]
fn list_uninstall_history(limit: Option<u32>) -> Result<Vec<history::UninstallRecord>, String> {
    history::list(limit.unwrap_or(100))
}

/// 其他已安装软件的安装目录。用于拦住 AI 猜错的情况：
/// 如果 AI 推断的位置落在别人家的安装目录里，直接丢掉。
fn other_install_locations(self_id: &str) -> Vec<String> {
    list_installed_apps()
        .into_iter()
        .filter(|a| a.id != self_id)
        .filter_map(|a| a.install_location)
        .filter(|s| s.trim().len() >= 4)
        .collect()
}

/// 扫描残留 + 让 AI 推断还可能漏掉的位置。
///
/// AI 只提供**候选**：每条都要过 `residuals::verify_ai_guesses` 的存在性、
/// 安全路径、名字相关性、其他软件目录重合一整套校验，通过后才会进列表。
/// AI 调用失败不影响规则扫描结果，错误会带回前端提示。
#[tauri::command(async)]
async fn ai_suggest_residuals(
    app: InstalledApp,
    cache: State<'_, ResidualCache>,
    index: State<'_, ResidualIndex>,
    recent: State<'_, RecentlyRemoved>,
) -> Result<SuggestedResiduals, String> {
    let real = find_installed_app(&app.id).or_else(|| recent.0.read().get(&app.id).cloned());
    let Some(real) = real else {
        return Ok(SuggestedResiduals {
            items: Vec::new(),
            ai_summary: String::new(),
            ai_guessed: 0,
            ai_error: None,
        });
    };
    if real.is_protected {
        return Err("该条目属于受保护软件，已拒绝扫描".into());
    }
    suggest_for_app(&real, &cache, &index).await
}

/// 规则扫描 + AI 推断补漏的公共实现。向导页（应用还在枚举里）和
/// 卸载历史页（应用已不在枚举中）走同一条路径，避免两边能力不一致。
async fn suggest_for_app(
    app: &InstalledApp,
    cache: &ResidualCache,
    index: &ResidualIndex,
) -> Result<SuggestedResiduals, String> {
    let ctx = build_scan_context(app);
    let base = residuals::scan_residuals(&ctx);

    let client = AiClient::new(config::get());
    let (extra, summary, error) = match client.suggest_cleanup(app, &base).await {
        Ok(guess) => {
            let others = other_install_locations(&app.id);
            let extra = residuals::verify_ai_guesses(&ctx, &guess.items, &others);
            (extra, guess.summary, None)
        }
        Err(e) => (Vec::new(), String::new(), Some(e)),
    };

    let mut items = base;
    let mut seen: std::collections::HashSet<String> =
        items.iter().map(|i| i.id.clone()).collect();
    let mut guessed = 0usize;
    for it in extra {
        if seen.insert(it.id.clone()) {
            items.push(it);
            guessed += 1;
        }
    }
    cache_residuals(cache, index, &app.id, &items);
    Ok(SuggestedResiduals {
        items,
        ai_summary: summary,
        ai_guessed: guessed,
        ai_error: error,
    })
}

/// 重新扫描历史记录对应的软件残留（即使程序已重启、应用已不在枚举中）。
/// 扫描结果会写入服务端索引，因此随后可以复用 delete_residuals 安全删除。
#[tauri::command(async)]
fn scan_history_residuals(
    id: i64,
    cache: State<'_, ResidualCache>,
    index: State<'_, ResidualIndex>,
) -> Result<Vec<ResidualItem>, String> {
    let rec = history::get(id)?.ok_or_else(|| "历史记录不存在".to_string())?;
    if rec.app.is_protected {
        return Err("该历史记录属于受保护软件，已拒绝扫描".into());
    }
    let ctx = build_scan_context(&rec.app);
    let residuals = residuals::scan_residuals(&ctx);
    cache_residuals(&cache, &index, &rec.app.id, &residuals);
    history::update_scan(id, &residuals)?;
    Ok(residuals)
}

#[tauri::command(async)]
fn delete_uninstall_history(id: i64) -> Result<(), String> {
    history::delete(id)
}

/// 历史页的“重新扫描”：规则扫描 + AI 推断补漏。
/// 应用已经卸载，枚举里找不到它，只能靠历史快照里的字段；
/// 而快照里的名字往往和残留目录名对不上（网易云音乐 / Netease），
/// 只跑规则会得到“0 项”这种假干净结论，必须让 AI 补漏。
#[tauri::command(async)]
async fn ai_suggest_history_residuals(
    id: i64,
    cache: State<'_, ResidualCache>,
    index: State<'_, ResidualIndex>,
) -> Result<SuggestedResiduals, String> {
    let rec = history::get(id)?.ok_or_else(|| "历史记录不存在".to_string())?;
    if rec.app.is_protected {
        return Err("该历史记录属于受保护软件，已拒绝扫描".into());
    }
    let out = suggest_for_app(&rec.app, &cache, &index).await?;
    history::update_scan(id, &out.items)?;
    Ok(out)
}

#[tauri::command]
async fn ai_assess_residuals(
    app: InstalledApp,
    residuals: Vec<ResidualItem>,
) -> Result<AiResidualAssessment, String> {
    let client = AiClient::new(config::get());
    client.assess_residuals(&app, &residuals).await
}

/// 删除后复查：让 AI 判断这款软件是否算卸载干净。
/// 只做结论判定，不会返回任何新的删除目标。
#[tauri::command(async)]
async fn ai_verify_clean(
    app: InstalledApp,
    cleaned: usize,
    leftovers: Vec<ResidualItem>,
) -> Result<AiCleanliness, String> {
    let client = AiClient::new(config::get());
    client.verify_clean(&app, cleaned, &leftovers).await
}

/// 按服务端索引逐项删除。前端传来的 path/kind 一律忽略，只认 id；
/// 索引里没有的项（伪造或重新扫描后已过期）直接拒绝。
fn delete_items_checked(
    items: &[ResidualItem],
    index: &ResidualIndex,
    app_handle: &tauri::AppHandle,
) -> Vec<DeleteResult> {
    let mut out = Vec::with_capacity(items.len());
    for requested in items {
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
    out
}

/// 删除指定残留项（用户已确认）。
/// 只按 id 从服务端索引取记录；索引里没有的项（伪造/过期）一律拒绝。
#[tauri::command(async)]
fn delete_residuals(
    items: Vec<ResidualItem>,
    index: State<'_, ResidualIndex>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<DeleteResult>, String> {
    Ok(delete_items_checked(&items, &index, &app_handle))
}

/// 强制删除：不运行软件自带的卸载器，直接删掉选中的残留项。
///
/// 用于卸载器缺失/损坏的软件——这正是普通卸载工具（包括 Geek 的普通模式）
/// 无法处理的场景。风险高于常规卸载，所以先建系统还原点，再逐项删除；
/// 还原点失败**不阻止**删除，但原因会一并返回，由前端如实展示。
#[tauri::command(async)]
fn force_remove_app(
    app: InstalledApp,
    items: Vec<ResidualItem>,
    remove_entry: bool,
    index: State<'_, ResidualIndex>,
    recent: State<'_, RecentlyRemoved>,
    app_handle: tauri::AppHandle,
) -> Result<ForceRemoveOutcome, String> {
    // 仍然按 id 重新枚举，不信任前端传来的字段。
    let real = find_installed_app(&app.id)
        .or_else(|| recent.0.read().get(&app.id).cloned())
        .ok_or_else(|| {
            "应用不在当前系统枚举列表中（可能已被卸载或条目被篡改），已拒绝执行"
                .to_string()
        })?;
    if real.is_protected {
        return Err(protected_app_error(&real));
    }
    // 删完之后它就不在枚举里了，先记下来供“复查是否卸载干净”使用。
    recent.0.write().insert(real.id.clone(), real.clone());

    app_handle
        .emit("uninstall-log", "强制删除：正在创建系统还原点…")
        .ok();
    let (restore_point, restore_point_error) =
        match restore::create_restore_point(&format!(
            "CleanSweep 强制删除前快照 - {}",
            real.display_name
        )) {
            Ok(msg) => {
                app_handle.emit("uninstall-log", &msg).ok();
                (Some(msg), None)
            }
            Err(e) => {
                app_handle
                    .emit("uninstall-log", format!("还原点：{}", e))
                    .ok();
                (None, Some(e))
            }
        };

    let results = delete_items_checked(&items, &index, &app_handle);

    let mut entry_removed = false;
    let mut entry_error = None;
    if remove_entry {
        match registry::delete_uninstall_entry(&real) {
            Ok(_) => entry_removed = true,
            Err(e) => entry_error = Some(e),
        }
    }

    Ok(ForceRemoveOutcome {
        restore_point,
        restore_point_error,
        results,
        entry_removed,
        entry_error,
    })
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

/// 注册表里的 InstallLocation 经常是空的（大量软件根本不写这个值），
/// 但 DisplayIcon / UninstallString 一般指向安装目录里的文件，可以从它们反推。
/// 不反推的话，"安装目录本体"这一步会被整个跳过——目录明明还在，却一条都扫不到。
fn effective_install_location(app: &InstalledApp) -> Option<String> {
    if let Some(loc) = app.install_location.as_deref() {
        if !loc.trim().is_empty() {
            return Some(loc.trim().to_string());
        }
    }
    let mut candidates: Vec<String> = Vec::new();
    if let Some(icon) = app.display_icon.as_deref() {
        // DisplayIcon 形如 `C:\App\app.exe,0`，也可能带引号。
        let p = icon.split(',').next().unwrap_or(icon).trim().trim_matches('"');
        if !p.is_empty() {
            candidates.push(p.to_string());
        }
    }
    for raw in [
        app.uninstall_string.as_deref(),
        app.quiet_uninstall_string.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let (program, _) = crate::uninstall::split_command(raw);
        if !program.is_empty() {
            candidates.push(program);
        }
    }
    for c in candidates {
        let Some(dir) = std::path::Path::new(&c).parent() else {
            continue;
        };
        // 至少要有 `D:\Vendor\App` 这样的深度，否则反推出来可能只是一个盘根目录。
        if dir.components().count() < 3 {
            continue;
        }
        let text = dir.to_string_lossy().to_string();
        // 反推结果要过删除安全校验：避免把 `C:\Windows\System32` 这类目录
        // 当成安装目录，再顺着它把系统目录放进扫描白名单。
        if crate::safety::validate_delete_path(&text, crate::safety::TargetKind::Directory).is_err() {
            continue;
        }
        return Some(text);
    }
    None
}

fn build_scan_context(app: &InstalledApp) -> ScanContext {
    let publisher_key_hints = publisher_hints(app);
    let (aliases, path_hints, extra_roots) = scan_hints(app);
    ScanContext {
        app_name: app.display_name.clone(),
        publisher: app.publisher.clone(),
        install_location: effective_install_location(app),
        publisher_key_hints,
        aliases,
        path_hints,
        extra_roots,
    }
}

fn norm_alias(s: &str) -> String {
    s.to_lowercase().replace([' ', '-', '_', '.'], "")
}

fn generic_alias(n: &str) -> bool {
    const GENERIC: &[&str] = &[
        "programfiles",
        "programfilesx86",
        "programdata",
        "localappdata",
        "appdata",
        "roaming",
        "users",
        "windows",
        "system32",
        "syswow64",
        "microsoft",
        "windowsapps",
        "software",
        "programs",
        "program",
        "commonfiles",
        "common",
        "steamapps",
        "data",
        "local",
        "temp",
        "uninstall",
        "uninstaller",
        "unins000",
        "unins001",
        "setup",
        "install",
        "installer",
        "app",
        "application",
        "bin",
        "x64",
        "x86",
        "amd64",
        "updater",
        "update",
        "helper",
        "service",
        "client",
        "browser",
        "desktop",
        "version",
        "resources",
        "currentversion",
        "current",
        // 注册表重定向容器：出现为原名就会被当成“系统组件残留”
        "wow6432node",
        "classes",
        "policies",
        "registrymachine",
        "registryuser",
        "store",
        "corporation",
        "inc",
        "ltd",
        "limited",
    ];
    // `Program Files (x86)` 这类带括号的目录名归一化后不等于列表里的写法，
    // 去掉括号再比一次，否则它会变成一条别名。
    let bare: String = n.chars().filter(|c| *c != '(' && *c != ')').collect();
    n.chars().count() < 2 || GENERIC.contains(&n) || GENERIC.contains(&bare.as_str())
}

fn push_alias(out: &mut Vec<String>, s: &str) {
    let n = norm_alias(s.trim());
    if n.is_empty() || generic_alias(&n) {
        return;
    }
    if !out.iter().any(|x| x == &n) {
        out.push(n);
    }
}

fn push_path_components(out: &mut Vec<String>, p: &str) {
    let username = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
    for comp in std::path::Path::new(p).components() {
        if let std::path::Component::Normal(c) = comp {
            let s = c.to_string_lossy();
            if !username.is_empty() && s.eq_ignore_ascii_case(&username) {
                continue;
            }
            push_alias(out, &s);
        }
    }
}

fn is_safe_extra_scan_root(p: &std::path::Path) -> bool {
    // 不要把驱动器根目录 / Windows 目录当作“同级目录扫描根”。
    if p.parent().is_none() {
        return false;
    }
    let normal = p
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if normal == 0 {
        return false;
    }
    !p.to_string_lossy().to_lowercase().contains("\\windows")
}

/// 从显示名 / 发布者 / 安装路径 / 卸载命令 / 注册表键 / 应用 id 推导扫描线索。
/// 例如“夸克”安装到 `D:\software\Quark`，会得到别名 `quark`，
/// 从而命中 QuarkUpdater、quark-cloud-drive 等英文残留。
fn scan_hints(app: &InstalledApp) -> (Vec<String>, Vec<String>, Vec<std::path::PathBuf>) {
    let mut aliases: Vec<String> = Vec::new();
    let mut path_hints: Vec<String> = Vec::new();
    let mut extra_roots: Vec<std::path::PathBuf> = Vec::new();

    push_alias(&mut aliases, &app.display_name);
    for w in app
        .display_name
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '-' | '_'))
    {
        push_alias(&mut aliases, w);
    }
    if let Some(p) = &app.publisher {
        push_alias(&mut aliases, p);
        if let Some(first) = p.split_whitespace().next() {
            push_alias(&mut aliases, first);
        }
    }
    // 用反推出来的目录，而不是注册表里那个大概率是 null 的 InstallLocation。
    if let Some(p) = effective_install_location(app) {
        push_path_components(&mut aliases, &p);
        path_hints.push(p.to_lowercase());
        if let Some(parent) = std::path::Path::new(&p).parent() {
            if is_safe_extra_scan_root(parent) {
                extra_roots.push(parent.to_path_buf());
            }
        }
    }
    for raw in [app.uninstall_string.as_deref(), app.quiet_uninstall_string.as_deref()]
        .into_iter()
        .flatten()
    {
        let (program, _) = crate::uninstall::split_command(raw);
        if program.is_empty() {
            continue;
        }
        // 系统目录里的卸载器（例如 msiexec.exe）不作为应用别名来源。
        let program_low = program.to_lowercase();
        let system_binary = program_low.contains("\\windows\\");
        if !system_binary {
            push_path_components(&mut aliases, &program);
            if let Some(stem) = std::path::Path::new(&program)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
            {
                push_alias(&mut aliases, &stem);
            }
        }
        path_hints.push(program_low.clone());
        if let Some(parent) = std::path::Path::new(&program).parent() {
            path_hints.push(parent.to_string_lossy().to_lowercase());
            if !system_binary && is_safe_extra_scan_root(parent) {
                extra_roots.push(parent.to_path_buf());
            }
        }
    }
    if let Some(key) = &app.registry_key {
        // 只有最后一段是这款软件的名字。前面的
        // SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall
        // 是固定系统路径：推成别名会把系统容器（如 HKCU\SOFTWARE\Wow6432Node）
        // 误认成这款软件的专属残留。
        if let Some(last) = key.rsplit('\\').next() {
            push_alias(&mut aliases, last);
        }
    }
    // 同理：id 形如 `RegistryUser::网易云音乐`，只有最后一段有意义。
    if let Some(last) = app.id.rsplit([':', '/', '\\']).next() {
        if last.chars().count() <= 60 {
            push_alias(&mut aliases, last);
        }
    }

    path_hints.retain(|h| h.len() >= 4);
    path_hints.sort();
    path_hints.dedup();
    extra_roots.sort();
    extra_roots.dedup();

    (aliases, path_hints, extra_roots)
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
            list_uninstall_history,
            scan_history_residuals,
            delete_uninstall_history,
            ai_assess_residuals,
            ai_verify_clean,
            ai_suggest_residuals,
            ai_suggest_history_residuals,
            force_remove_app,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(display: &str, registry_key: Option<&str>) -> InstalledApp {
        InstalledApp {
            id: "RegistryUser::网易云音乐".into(),
            display_name: display.into(),
            publisher: Some("网易".into()),
            version: Some("2.10.6".into()),
            install_location: Some(r"C:\Program Files (x86)\Netease\CloudMusic".into()),
            install_date: None,
            uninstall_string: Some(
                r#""C:\Program Files (x86)\Netease\CloudMusic\uninstall.exe""#.into(),
            ),
            quiet_uninstall_string: None,
            display_icon: None,
            estimated_size_kb: None,
            source: AppSource::RegistryUser,
            registry_key: registry_key.map(|s| s.to_string()),
            is_system_component: false,
            is_protected: false,
            protected_reason: None,
            package_family: None,
        }
    }

    /// 注册表键的系统路径段（SOFTWARE\WOW6432Node\Microsoft\...）不能被当成应用别名，
    /// 否则扫描器会把 `HKCU\SOFTWARE\Wow6432Node` 这类系统容器当成“本软件残留”。
    #[test]
    fn registry_system_path_segments_never_become_aliases() {
        let app = app_with(
            "网易云音乐",
            Some(r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\网易云音乐"),
        );
        let (aliases, _, _) = scan_hints(&app);
        for bad in [
            "wow6432node",
            "uninstall",
            "currentversion",
            "microsoft",
            "windows",
            "software",
            // 带括号的通用目录名也要被过滤
            "programfiles(x86)",
        ] {
            assert!(!aliases.iter().any(|x| x == bad), "别名里不应出现: {bad} ({aliases:?})");
        }
        assert!(
            aliases.iter().any(|x| x == "网易云音乐"),
            "应用名必须保留: {aliases:?}"
        );
        assert!(
            aliases.iter().any(|x| x == "cloudmusic") && aliases.iter().any(|x| x == "netease"),
            "安装路径里的产品名与厂商名必须保留: {aliases:?}"
        );
    }

    /// InstallLocation 为 null 时，必须能从 DisplayIcon / UninstallString 反推安装目录。
    /// 否则“安装目录本体”这一步会被整个跳过，目录还在却一条都扫不到。
    #[test]
    fn empty_install_location_falls_back_to_display_icon() {
        let mut app = app_with("网易云音乐", None);
        app.install_location = None;
        app.display_icon = Some(r"D:\software\CloudMusic\cloudmusic.exe".into());
        app.uninstall_string = Some(r"D:\software\CloudMusic\UnInstall.exe".into());

        let loc = effective_install_location(&app).expect("应能从 DisplayIcon 反推安装目录");
        assert_eq!(loc.to_lowercase(), r"d:\software\cloudmusic");
        // 反推出的目录要参与路径线索推导（包含派生目录扫描根）
        let (aliases, path_hints, extra_roots) = scan_hints(&app);
        assert!(aliases.iter().any(|x| x == "cloudmusic"), "{aliases:?}");
        assert!(path_hints.iter().any(|p| p.contains("cloudmusic")));
        assert!(
            extra_roots.iter().any(|p| p.to_string_lossy().eq_ignore_ascii_case(r"d:\software")),
            "应把安装目录的父目录作为同级扫描根: {extra_roots:?}"
        );
    }

    /// 反推只能接受“能通过删除安全校验”的目录，否则会把系统目录放进扫描白名单。
    #[test]
    fn install_location_inference_rejects_system_directories() {
        let mut app = app_with("Foo", None);
        app.install_location = None;
        app.display_icon = Some(r"C:\Windows\System32\foo.exe".into());
        app.uninstall_string = None;
        assert!(effective_install_location(&app).is_none());
    }

    /// 真实环境回归：本机网易云音乐卸载后残留仍在。
    /// 它的 InstallLocation 是 null，修复前会出现“0 项残留”的假干净结论。
    #[test]
    #[ignore = "需要本机真实环境：网易云音乐已卸载且残留仍在"]
    fn real_env_probe_netease() {
        let app = InstalledApp {
            id: "RegistryMachine32::网易云音乐".into(),
            display_name: "网易云音乐".into(),
            publisher: Some("网易公司".into()),
            version: Some("3.1.7.203649".into()),
            install_location: None,
            install_date: None,
            uninstall_string: Some(r"D:\software\CloudMusic\UnInstall.exe".into()),
            quiet_uninstall_string: None,
            display_icon: Some(r"D:\software\CloudMusic\cloudmusic.exe".into()),
            estimated_size_kb: None,
            source: AppSource::RegistryMachine32,
            registry_key: Some(
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\网易云音乐"
                    .into(),
            ),
            is_system_component: false,
            is_protected: false,
            protected_reason: None,
            package_family: None,
        };
        let ctx = build_scan_context(&app);
        println!("install_location = {:?}", ctx.install_location);
        println!("aliases = {:?}", ctx.aliases);
        println!("extra_roots = {:?}", ctx.extra_roots);
        let hits: Vec<String> = residuals::scan_residuals(&ctx)
            .into_iter()
            .map(|i| i.path)
            .collect();
        println!("--- hits ---\n{hits:#?}");
        assert!(
            hits.iter()
                .any(|p| p.eq_ignore_ascii_case(r"D:\software\CloudMusic")),
            "安装目录本体必须被扫到（不能因为 InstallLocation 为空就跳过）"
        );
    }
}
