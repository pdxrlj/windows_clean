//! 配置存储（JSON 文件，放在应用数据目录）。
//!
//! 健壮性约定：
//! - 旧/残缺配置：`AiConfig` 带 `#[serde(default)]`，缺字段用默认值。
//! - 损坏配置：先重命名为 `ai_config.json.bak` 再使用默认值，不静默丢弃原文件。
//! - 保存：归一化后先写同目录临时文件，再 rename 覆盖，避免写到一半崩溃损坏配置。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use parking_lot::RwLock;

use crate::types::AiConfig;

/// 配置文件中 AiConfig 各字段的默认值（serde default 时使用）。
const DEFAULT_TEMPERATURE: f32 = 0.2;
#[cfg(test)]
const DEFAULT_TIMEOUT_SECS: u64 = 90;

static CONFIG_PATH: Lazy<PathBuf> = Lazy::new(|| {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("CleanSweep");
    let _ = fs::create_dir_all(&p);
    p.push("ai_config.json");
    p
});

static CONFIG: Lazy<RwLock<AiConfig>> = Lazy::new(|| RwLock::new(load()));

/// 配置文件绝对路径：`%APPDATA%/CleanSweep/ai_config.json`。
pub fn path() -> PathBuf {
    CONFIG_PATH.clone()
}

/// 读取并归一化配置；文件不存在 → 默认配置；
/// 文件存在但无法解析 → 备份为 `.bak` 后使用默认配置。
fn load() -> AiConfig {
    let p = path();
    let data = match fs::read_to_string(&p) {
        Ok(d) => d,
        Err(_) => return AiConfig::default(),
    };
    match serde_json::from_str::<AiConfig>(&data) {
        Ok(mut cfg) => {
            normalize(&mut cfg);
            cfg
        }
        Err(_) => {
            // 损坏的配置不要静默覆盖：留一份 .bak 方便用户/排查找回。
            let _ = fs::rename(&p, backup_path(&p));
            AiConfig::default()
        }
    }
}

/// 当前内存中的配置（clone）。
pub fn get() -> AiConfig {
    CONFIG.read().clone()
}

/// 保存配置：归一化 → 序列化 → 临时文件 → rename 原子替换。
pub fn save(cfg: AiConfig) -> Result<(), String> {
    let mut cfg = cfg;
    normalize(&mut cfg);

    let data = serde_json::to_string_pretty(&cfg).map_err(|e| format!("序列化配置失败: {}", e))?;
    let p = path();
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }

    let tmp = tmp_path(&p);
    let write_res = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(format!("写入配置失败: {}", e));
    }
    // Windows 上 std::fs::rename 使用 MOVEFILE_REPLACE_EXISTING，可原子覆盖已存在文件。
    if let Err(e) = fs::rename(&tmp, &p) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("替换配置文件失败: {}", e));
    }

    *CONFIG.write() = cfg;
    Ok(())
}

/// 归一化配置：去空白、去 base_url 尾部 `/`、夹取 temperature/timeout。
pub fn normalize(cfg: &mut AiConfig) {
    cfg.base_url = trim_trailing_slash(cfg.base_url.trim());
    cfg.api_key = cfg.api_key.trim().to_string();
    cfg.model = cfg.model.trim().to_string();
    cfg.env_api_key_name = cfg.env_api_key_name.trim().to_string();
    cfg.env_base_url_name = cfg.env_base_url_name.trim().to_string();
    cfg.env_model_name = cfg.env_model_name.trim().to_string();

    if !cfg.temperature.is_finite() {
        cfg.temperature = DEFAULT_TEMPERATURE;
    }
    cfg.temperature = cfg.temperature.clamp(0.0, 2.0);

    cfg.timeout_secs = cfg.timeout_secs.clamp(5, 600);
}

fn trim_trailing_slash(mut s: &str) -> String {
    while let Some(rest) = s.strip_suffix('/') {
        s = rest;
    }
    s.to_string()
}

/// `<file>.bak`，例如 ai_config.json -> ai_config.json.bak。
fn backup_path(p: &Path) -> PathBuf {
    let mut name = p.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    name.push(".bak");
    p.with_file_name(name)
}

/// 同目录唯一临时文件，避免并发保存互相踩踏。
fn tmp_path(p: &Path) -> PathBuf {
    let mut name = p.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    name.push(format!(".{}.{}.tmp", std::process::id(), nanos));
    p.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯函数测试，不触碰真实 %APPDATA%，也不读写配置文件。
    #[test]
    fn normalize_trims_and_strips_trailing_slash() {
        let mut cfg = AiConfig {
            base_url: "  https://api.openai.com/v1/  ".into(),
            api_key: "  sk-test  ".into(),
            model: " gpt-4o-mini ".into(),
            env_api_key_name: " OPENAI_API_KEY ".into(),
            ..Default::default()
        };
        normalize(&mut cfg);
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.api_key, "sk-test");
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.env_api_key_name, "OPENAI_API_KEY");
    }

    #[test]
    fn normalize_clamps_temperature_and_timeout() {
        let mut cfg = AiConfig {
            temperature: 9.5,
            timeout_secs: 9999,
            ..Default::default()
        };
        normalize(&mut cfg);
        assert_eq!(cfg.temperature, 2.0);
        assert_eq!(cfg.timeout_secs, 600);

        let mut cfg = AiConfig {
            temperature: -3.0,
            timeout_secs: 1,
            ..Default::default()
        };
        normalize(&mut cfg);
        assert_eq!(cfg.temperature, 0.0);
        assert_eq!(cfg.timeout_secs, 5);

        let mut cfg = AiConfig {
            temperature: f32::NAN,
            timeout_secs: 0,
            ..Default::default()
        };
        normalize(&mut cfg);
        assert_eq!(cfg.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(cfg.timeout_secs, 5);
    }

    #[test]
    fn partial_json_uses_defaults() {
        // 模拟旧版本配置文件：缺 temperature / timeoutSecs，甚至缺大半个字段。
        let cfg: AiConfig = serde_json::from_str(
            r#"{"enabled":true,"baseUrl":"https://api.deepseek.com","model":"deepseek-chat"}"#,
        )
        .expect("旧配置应能解析");
        assert!(cfg.enabled);
        assert_eq!(cfg.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(cfg.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(cfg.env_base_url_name, "OPENAI_BASE_URL");
        assert_eq!(cfg.use_env, false);
    }

    #[test]
    fn empty_object_loads_default_config() {
        let cfg: AiConfig = serde_json::from_str("{}").expect("空对象应能解析");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.model, "gpt-6-luna");
        assert_eq!(cfg.temperature, 0.2);
        assert_eq!(cfg.timeout_secs, 90);
    }

    #[test]
    fn config_serializes_camel_case_contract() {
        // 锁定前后端契约：AiConfig 的 JSON 键名必须是 camelCase。
        let v = serde_json::to_value(AiConfig::default()).expect("默认配置应可序列化");
        assert_eq!(v["baseUrl"], "https://api.openai.com/v1");
        assert_eq!(v["apiKey"], "");
        assert_eq!(v["model"], "gpt-6-luna");
        assert_eq!(v["enabled"], false);
        assert_eq!(v["useEnv"], false);
        assert_eq!(v["envApiKeyName"], "OPENAI_API_KEY");
        assert_eq!(v["envBaseUrlName"], "OPENAI_BASE_URL");
        assert_eq!(v["envModelName"], "OPENAI_MODEL");
        // f32 -> f64 序列化会有浮点误差，按近似值断言
        let temp = v["temperature"].as_f64().expect("temperature 应为数字");
        assert!((temp - 0.2).abs() < 1e-6, "temperature 默认应为 0.2，实际 {}", temp);
        assert_eq!(v["timeoutSecs"], 90);
        assert!(v.get("base_url").is_none(), "不应出现 snake_case 键");

        // Tauri IPC 用 to_string 序列化：f32 应格式化为紧凑的 0.2，而不是 0.20000000298023224。
        let json = serde_json::to_string(&AiConfig::default()).expect("默认配置应可序列化");
        assert!(
            json.contains("\"temperature\":0.2"),
            "temperature 序列化结果意外: {}",
            json
        );
    }
}
