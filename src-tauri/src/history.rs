//! 本地卸载历史（SQLite）。
//!
//! - 数据库位置：`%APPDATA%\CleanSweep\history.db`；
//! - 每次卸载后记录软件快照、卸载结果、当次扫描到的残留；
//! - 之后可以随时重新扫描历史软件的残留，并让 AI 复查；
//! - 数据库只存本地，不上传。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::types::{InstalledApp, ResidualItem, UninstallResult};

/// 一条卸载历史记录（前端直接使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallRecord {
    pub id: i64,
    /// 卸载时的软件快照（之后即使软件从系统消失也能重新扫描）。
    pub app: InstalledApp,
    /// Unix 毫秒时间戳。
    pub uninstalled_at: i64,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub log: String,
    pub residual_count: i64,
    pub last_scan_at: Option<i64>,
    pub residuals: Vec<ResidualItem>,
    pub ai_summary: Option<String>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS uninstall_history (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    app_id              TEXT NOT NULL,
    display_name        TEXT NOT NULL,
    uninstalled_at      INTEGER NOT NULL,
    success             INTEGER NOT NULL,
    exit_code           INTEGER,
    log                 TEXT NOT NULL DEFAULT '',
    residual_count      INTEGER NOT NULL DEFAULT 0,
    residuals_json      TEXT NOT NULL DEFAULT '[]',
    last_scan_at        INTEGER,
    ai_summary          TEXT,
    app_json            TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_uninstall_history_time
    ON uninstall_history(uninstalled_at DESC);
"#;

pub fn path() -> PathBuf {
    config::data_dir().join("history.db")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn open_at(p: &Path) -> Result<Connection, String> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建历史目录失败: {}", e))?;
    }
    let conn = Connection::open(p).map_err(|e| format!("打开历史数据库失败: {}", e))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("设置数据库超时失败: {}", e))?;
    conn.execute_batch(SCHEMA)
        .map_err(|e| format!("初始化历史数据库失败: {}", e))?;
    Ok(conn)
}

fn open() -> Result<Connection, String> {
    open_at(&path())
}

fn insert_with(
    conn: &Connection,
    app: &InstalledApp,
    result: &UninstallResult,
    residuals: &[ResidualItem],
) -> Result<i64, String> {
    let app_json = serde_json::to_string(app).map_err(|e| format!("序列化软件信息失败: {}", e))?;
    let residuals_json =
        serde_json::to_string(residuals).map_err(|e| format!("序列化残留失败: {}", e))?;
    conn.execute(
        r#"INSERT INTO uninstall_history
           (app_id, display_name, uninstalled_at, success, exit_code, log,
            residual_count, residuals_json, last_scan_at, ai_summary, app_json)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)"#,
        params![
            app.id,
            app.display_name,
            now_ms(),
            if result.success { 1 } else { 0 },
            result.exit_code,
            result.log,
            residuals.len() as i64,
            residuals_json,
            now_ms(),
            app_json,
        ],
    )
    .map_err(|e| format!("写入卸载历史失败: {}", e))?;
    Ok(conn.last_insert_rowid())
}

/// 记录一次卸载及当次扫描到的残留。
pub fn record(
    app: &InstalledApp,
    result: &UninstallResult,
    residuals: &[ResidualItem],
) -> Result<i64, String> {
    let conn = open()?;
    insert_with(&conn, app, result, residuals)
}

struct RawRecord {
    id: i64,
    app_json: String,
    uninstalled_at: i64,
    success: bool,
    exit_code: Option<i32>,
    log: String,
    residual_count: i64,
    residuals_json: String,
    last_scan_at: Option<i64>,
    ai_summary: Option<String>,
}

fn parse_row(r: RawRecord) -> Option<UninstallRecord> {
    let app: InstalledApp = serde_json::from_str(&r.app_json).ok()?;
    let residuals: Vec<ResidualItem> = serde_json::from_str(&r.residuals_json).unwrap_or_default();
    Some(UninstallRecord {
        id: r.id,
        app,
        uninstalled_at: r.uninstalled_at,
        success: r.success,
        exit_code: r.exit_code,
        log: r.log,
        residual_count: r.residual_count,
        last_scan_at: r.last_scan_at,
        residuals,
        ai_summary: r.ai_summary,
    })
}

fn read_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecord> {
    Ok(RawRecord {
        id: row.get(0)?,
        app_json: row.get(1)?,
        uninstalled_at: row.get(2)?,
        success: row.get::<_, i64>(3)? != 0,
        exit_code: row.get(4)?,
        log: row.get(5)?,
        residual_count: row.get(6)?,
        residuals_json: row.get(7)?,
        last_scan_at: row.get(8)?,
        ai_summary: row.get(9)?,
    })
}

const SELECT_COLUMNS: &str = "id, app_json, uninstalled_at, success, exit_code, log, \
                              residual_count, residuals_json, last_scan_at, ai_summary";

pub fn list(limit: u32) -> Result<Vec<UninstallRecord>, String> {
    let conn = open()?;
    let limit = limit.clamp(1, 500) as i64;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {} FROM uninstall_history ORDER BY uninstalled_at DESC LIMIT ?1",
            SELECT_COLUMNS
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![limit], read_raw)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        if let Ok(raw) = r {
            if let Some(rec) = parse_row(raw) {
                out.push(rec);
            }
        }
    }
    Ok(out)
}

pub fn get(id: i64) -> Result<Option<UninstallRecord>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {} FROM uninstall_history WHERE id = ?1",
            SELECT_COLUMNS
        ))
        .map_err(|e| e.to_string())?;
    let raw = stmt
        .query_row(params![id], read_raw)
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(raw.and_then(parse_row))
}

/// 重新扫描后更新残留快照（AI 复查/手动复查都会调用）。
pub fn update_scan(id: i64, residuals: &[ResidualItem]) -> Result<(), String> {
    let conn = open()?;
    let json = serde_json::to_string(residuals).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE uninstall_history
         SET residual_count = ?1, residuals_json = ?2, last_scan_at = ?3
         WHERE id = ?4",
        params![residuals.len() as i64, json, now_ms(), id],
    )
    .map_err(|e| format!("更新历史残留失败: {}", e))?;
    Ok(())
}

pub fn delete(id: i64) -> Result<(), String> {
    let conn = open()?;
    conn.execute("DELETE FROM uninstall_history WHERE id = ?1", params![id])
        .map_err(|e| format!("删除历史记录失败: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AppSource, UninstallResult};

    fn test_app() -> InstalledApp {
        InstalledApp {
            id: "RegistryUser::Demo".into(),
            display_name: "Demo".into(),
            publisher: Some("DemoVendor".into()),
            version: Some("1.0".into()),
            install_location: Some(r"C:\Program Files\Demo".into()),
            install_date: None,
            uninstall_string: Some(r"C:\Program Files\Demo\unins000.exe".into()),
            quiet_uninstall_string: None,
            display_icon: None,
            estimated_size_kb: Some(1024),
            source: AppSource::RegistryUser,
            registry_key: None,
            is_system_component: false,
            is_protected: false,
            protected_reason: None,
            package_family: None,
        }
    }

    fn sample_residual() -> ResidualItem {
        ResidualItem {
            id: "app_dir:C:\\Users\\me\\AppData\\Roaming\\Demo".into(),
            kind: crate::types::ResidualKind::Directory,
            path: r"C:\Users\me\AppData\Roaming\Demo".into(),
            display: "Demo data".into(),
            detail: Some("1 MB".into()),
            size_bytes: Some(1_000_000),
            in_use: false,
            ai_verdict: None,
        }
    }

    #[test]
    fn history_roundtrip() {
        let db = std::env::temp_dir().join(format!(
            "cleansweep-history-test-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&db);
        let conn = open_at(&db).expect("create db");
        let result = UninstallResult {
            success: true,
            exit_code: Some(0),
            log: "ok".into(),
            requires_reboot: false,
        };
        let id = insert_with(&conn, &test_app(), &result, &[sample_residual()])
            .expect("insert");
        let raw = conn
            .query_row(
                &format!(
                    "SELECT {} FROM uninstall_history WHERE id = ?1",
                    SELECT_COLUMNS
                ),
                params![id],
                read_raw,
            )
            .expect("read");
        let rec = parse_row(raw).expect("parse");
        assert_eq!(rec.app.display_name, "Demo");
        assert_eq!(rec.residual_count, 1);
        assert!(rec.success);
        assert_eq!(rec.residuals[0].path, r"C:\Users\me\AppData\Roaming\Demo");

        conn.execute(
            "UPDATE uninstall_history SET residual_count = 0, residuals_json = '[]', last_scan_at = 1 WHERE id = ?1",
            params![id],
        )
        .expect("update");
        let raw2 = conn
            .query_row(
                &format!(
                    "SELECT {} FROM uninstall_history WHERE id = ?1",
                    SELECT_COLUMNS
                ),
                params![id],
                read_raw,
            )
            .expect("read2");
        let rec2 = parse_row(raw2).expect("parse2");
        assert_eq!(rec2.residual_count, 0);
        assert_eq!(rec2.last_scan_at, Some(1));

        drop(conn);
        let _ = std::fs::remove_file(&db);
    }
}
