//! AI 分析模块。OpenAI 兼容 API（OpenAI / DeepSeek / OpenRouter / Ollama / LM Studio / 通义千问 等）。
//!
//! 关键约定：
//! - 支持无 API Key 的本地服务（Ollama / LM Studio / 自建代理）：key 为空时不发 Authorization 头。
//! - Base URL 使用前统一归一化（补 scheme、去尾部 `/`、无路径时补 `/v1`）。
//! - 模型返回的 JSON 兼容 Markdown 代码围栏/前后说明文字，键名兼容 camelCase 与 snake_case。
//! - 部分兼容服务不支持 `response_format: json_object`，遇到 400/422 会自动去掉重试一次。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{
    AiAppInfo, AiConfig, AiResidualAssessment, AiTestResult, AiUninstallRisk, EffectiveAi,
    EnvProbe, InstalledApp, ResidualItem,
};

const DEFAULT_TEMPERATURE: f32 = 0.2;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 600;
/// 连接测试统一压到较短超时，避免设置页卡 90 秒。
const TEST_TIMEOUT_CAP_SECS: u64 = 30;
/// 模型列表请求超时。
const MODELS_TIMEOUT_SECS: u64 = 20;

// ===== 纯工具函数（可单测） =====

/// 归一化 Base URL：去空白、去尾部 `/`、缺 scheme 补 `http://`、无路径时补 `/v1`。
///
/// 例：
/// - `https://api.openai.com/v1`      -> 原样
/// - `https://api.deepseek.com`       -> `https://api.deepseek.com/v1`
/// - `127.0.0.1:11434`                -> `http://127.0.0.1:11434/v1`
/// - `http://localhost:1234/v1`       -> 原样
/// - `https://openrouter.ai/api/v1`   -> 原样
pub fn normalize_base_url(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return s;
    }
    // 去掉所有尾部 `/`（用户常复制成 .../v1/）
    while let Some(rest) = s.strip_suffix('/') {
        s = rest.to_string();
    }

    let lower = s.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        // 本机/局域网服务默认 http，公网默认 https（避免把 api.openai.com 当成明文 http）。
        let host = s
            .split(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let private_172 = host
            .split('.')
            .nth(1)
            .and_then(|x| x.parse::<u8>().ok())
            .map(|o| (16..=31).contains(&o))
            .unwrap_or(false);
        let is_local = host.starts_with("localhost")
            || host.starts_with("127.")
            || host.starts_with("0.0.0.0")
            || host.starts_with("[::1]")
            || host.starts_with("192.168.")
            || host.starts_with("10.")
            || (host.starts_with("172.") && private_172)
            || !host.contains('.');
        s = format!("{}{}", if is_local { "http://" } else { "https://" }, s);
    }

    // 仅当 URL 没有路径（或只有根 `/`）时补 /v1；已有路径（/v1、/api/v1 等）保持不变。
    let scheme_end = match s.find("://") {
        Some(i) => i + 3,
        None => return s,
    };
    let rest = &s[scheme_end..];
    match rest.find('/') {
        None => s.push_str("/v1"),
        Some(0) => {
            // 防御性：形如 "https://host/"（理论上前面已去掉尾部斜杠）
            s.truncate(scheme_end);
            s.push_str("/v1");
        }
        Some(_) => {}
    }
    s
}

/// 聊天补全接口完整 URL。
pub fn chat_completions_url(base: &str) -> String {
    format!("{}/chat/completions", normalize_base_url(base))
}

/// 模型列表接口完整 URL。
pub fn models_url(base: &str) -> String {
    format!("{}/models", normalize_base_url(base))
}

/// 读取非空环境变量（去空白）。空值/空白/不存在都视为未设置。
pub fn env_value(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        }
        Err(_) => None,
    }
}

/// 敏感值脱敏（按字符切分，UTF-8 安全）：
/// 空 → ""；字符数 > 8 → `前4…后4`；否则 `••••`。
pub fn mask_secret(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = t.chars().collect();
    if chars.len() > 8 {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{}…{}", head, tail)
    } else {
        "••••".into()
    }
}

/// 按字符截断（不是字节），多字节文本不会 panic。
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push_str("…(截断)");
        out
    }
}

/// 从模型输出中提取 JSON：先剥离 Markdown 代码围栏，再取第一个 `{` 到最后一个 `}`。
/// 这样即使模型返回「好的，结果如下：```json ... ``` 希望有帮助」也能解析。
pub fn extract_json(raw: &str) -> String {
    let cleaned = strip_code_fences(raw);
    let trimmed = cleaned.trim().trim_start_matches('\u{feff}').trim();
    // 围栏被压成一行等极端情况下 cleaned 可能丢失内容，回退到原始文本做括号提取。
    let s = if trimmed.contains('{') {
        trimmed
    } else {
        raw.trim().trim_start_matches('\u{feff}').trim()
    };
    if s.is_empty() {
        return String::new();
    }
    match (s.find('{'), s.rfind('}')) {
        (Some(start), Some(end)) if end > start => s[start..=end].to_string(),
        _ => s.to_string(),
    }
}

/// 移除 ```json ... ``` 围栏标记，保留围栏内部内容。
fn strip_code_fences(raw: &str) -> String {
    if !raw.contains("```") {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find("```") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        // 跳过 ``` 后同一行的语言标记（如 json），保留其后的内容
        let inner = match after.find('\n') {
            Some(nl) => &after[nl + 1..],
            None => "",
        };
        if let Some(end) = inner.find("```") {
            out.push_str(&inner[..end]);
            rest = &inner[end + 3..];
        } else {
            out.push_str(inner);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// 夹取温度到 0.0~2.0；非有限值回退默认。
fn clamp_temperature(t: f32) -> f32 {
    if t.is_finite() {
        t.clamp(0.0, 2.0)
    } else {
        DEFAULT_TEMPERATURE
    }
}

/// 分析类请求超时（秒），与配置夹取规则一致。
fn request_timeout(cfg: &AiConfig) -> Duration {
    Duration::from_secs(cfg.timeout_secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS))
}

// ===== 配置解析（环境变量 / 静态字段） =====

#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolved {
    base_url: String,
    api_key: String,
    model: String,
}

/// 解析实际生效的值：use_env 时逐项读环境变量；空值/缺失回退静态字段。
/// 所有值去空白，base_url 归一化。API Key 允许为空（本地服务无需 Key）。
fn resolve_values(cfg: &AiConfig) -> Resolved {
    let pick = |env_name: &str, fallback: &str| -> String {
        if cfg.use_env {
            env_value(env_name).unwrap_or_else(|| fallback.trim().to_string())
        } else {
            fallback.trim().to_string()
        }
    };
    Resolved {
        base_url: normalize_base_url(&pick(&cfg.env_base_url_name, &cfg.base_url)),
        api_key: pick(&cfg.env_api_key_name, &cfg.api_key),
        model: pick(&cfg.env_model_name, &cfg.model),
    }
}

/// 只要求 Base URL 与模型名；API Key 可选。
fn endpoint_ready(r: &Resolved) -> Result<(), String> {
    if r.base_url.trim().is_empty() {
        return Err("AI Base URL 为空：请在设置中填写服务商地址（如 https://api.openai.com/v1，本地 Ollama 填 http://127.0.0.1:11434）。".into());
    }
    if r.model.trim().is_empty() {
        return Err("AI 模型名为空：请在设置中填写模型名（如 gpt-6-luna / deepseek-flash）。".into());
    }
    Ok(())
}

/// 当前生效配置 + 脱敏信息（供设置页展示）。
pub fn resolve_effective() -> EffectiveAi {
    let cfg = crate::config::get();
    let r = resolve_values(&cfg);
    let ready = cfg.enabled && !r.base_url.trim().is_empty() && !r.model.trim().is_empty();
    EffectiveAi {
        enabled: cfg.enabled,
        ready,
        base_url: r.base_url,
        base_url_from_env: cfg.use_env && env_value(&cfg.env_base_url_name).is_some(),
        model: r.model,
        model_from_env: cfg.use_env && env_value(&cfg.env_model_name).is_some(),
        api_key_masked: mask_secret(&r.api_key),
        api_key_from_env: cfg.use_env && env_value(&cfg.env_api_key_name).is_some(),
        use_env: cfg.use_env,
        config_path: crate::config::path().to_string_lossy().to_string(),
    }
}

/// 探测环境变量：found = 存在且非空，masked 为脱敏后的值。返回的键与传入名称一致。
pub fn probe_env_vars(names: &[String]) -> HashMap<String, EnvProbe> {
    let mut out = HashMap::new();
    for name in names {
        let lookup = name.trim();
        if lookup.is_empty() {
            continue;
        }
        let probe = match env_value(lookup) {
            Some(v) => EnvProbe {
                found: true,
                masked: mask_secret(&v),
            },
            None => EnvProbe {
                found: false,
                masked: String::new(),
            },
        };
        out.insert(name.clone(), probe);
    }
    out
}

// ===== HTTP 请求结构 =====

#[derive(Debug, Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<ChatMsg<'a>>,
    /// 部分新模型不再接受自定义 temperature，出错时会重试省略该字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct ChatMsg<'a> {
    role: &'a str,
    content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    ty: &'static str,
}

#[derive(Debug, Deserialize)]
struct ChatResp {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatRespMessage,
}

#[derive(Debug, Deserialize)]
struct ChatRespMessage {
    #[serde(default)]
    content: Option<String>,
}

/// 单次聊天请求选项。
#[derive(Debug, Clone, Copy)]
struct ChatOpts {
    /// 是否尝试 response_format: json_object（不支持的服务会自动去掉重试）。
    json_mode: bool,
    timeout: Duration,
}

impl ChatOpts {
    fn analysis(cfg: &AiConfig) -> Self {
        Self {
            json_mode: true,
            timeout: request_timeout(cfg),
        }
    }
}

pub struct Client {
    cfg: AiConfig,
    http: reqwest::Client,
}

impl Client {
    pub fn new(cfg: AiConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(request_timeout(&cfg))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, http }
    }

    fn resolved(&self) -> Resolved {
        resolve_values(&self.cfg)
    }

    /// 分析类命令必须先启用 AI；`chat` 本身不检查 enabled（连接测试需要忽略 enabled）。
    fn ensure_enabled(&self) -> Result<(), String> {
        if self.cfg.enabled {
            Ok(())
        } else {
            Err("AI 未启用。请在设置中启用 AI 后再试。".into())
        }
    }

    /// 发起 chat/completions 请求。失败时逐级降级，兼容更多 OpenAI 兼容服务：
    /// 1) 带 response_format: json_object + temperature；
    /// 2) 去掉 response_format（如 OpenRouter / 部分本地服务不支持）；
    /// 3) 再去掉 temperature（如部分新模型只接受默认温度）。
    async fn chat(&self, system: &str, user: &str, opts: ChatOpts) -> Result<String, String> {
        let r = self.resolved();
        endpoint_ready(&r)?;
        let mut url = chat_completions_url(&r.base_url);
        // 若显式写了 /v1 但服务商只接受根路径（DeepSeek 文档即根路径），404 时回退一次。
        let alt_url = r
            .base_url
            .strip_suffix("/v1")
            .filter(|b| !b.is_empty())
            .map(|b| format!("{}/chat/completions", b));

        let make = |json_mode: bool, with_temperature: bool| ChatReq {
            model: &r.model,
            messages: vec![
                ChatMsg {
                    role: "system",
                    content: system.to_string(),
                },
                ChatMsg {
                    role: "user",
                    content: user.to_string(),
                },
            ],
            temperature: if with_temperature {
                Some(clamp_temperature(self.cfg.temperature))
            } else {
                None
            },
            response_format: if json_mode {
                Some(ResponseFormat { ty: "json_object" })
            } else {
                None
            },
        };

        let (mut status, mut text) = self
            .send_chat(&url, &r, &make(opts.json_mode, true), opts.timeout)
            .await?;

        if !status.is_success() && status.as_u16() == 404 {
            if let Some(alt) = &alt_url {
                let (alt_status, alt_text) = self
                    .send_chat(alt, &r, &make(opts.json_mode, true), opts.timeout)
                    .await?;
                url = alt.clone();
                status = alt_status;
                text = alt_text;
            }
        }

        // 部分服务不支持 response_format，返回 400/422；去掉后重试一次。
        let (status, text) = if !status.is_success()
            && opts.json_mode
            && matches!(status.as_u16(), 400 | 422)
        {
            self.send_chat(&url, &r, &make(false, true), opts.timeout)
                .await?
        } else {
            (status, text)
        };

        // 部分新模型不接受自定义 temperature；再降一级重试。
        let (status, text) = if !status.is_success() && matches!(status.as_u16(), 400 | 422) {
            self.send_chat(&url, &r, &make(false, false), opts.timeout)
                .await?
        } else {
            (status, text)
        };

        if !status.is_success() {
            return Err(http_error(status, &text));
        }

        let parsed: ChatResp = serde_json::from_str(&text).map_err(|e| {
            format!(
                "解析 AI 响应失败: {} | 原始: {}",
                e,
                truncate_chars(&text, 400)
            )
        })?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();
        let content = content.trim();
        if content.is_empty() {
            return Err(format!(
                "AI 返回内容为空。原始响应: {}",
                truncate_chars(&text, 400)
            ));
        }
        Ok(content.to_string())
    }

    async fn send_chat(
        &self,
        url: &str,
        r: &Resolved,
        body: &ChatReq<'_>,
        timeout: Duration,
    ) -> Result<(reqwest::StatusCode, String), String> {
        let req = self.http.post(url).timeout(timeout).json(body);
        let req = with_auth(req, &r.api_key);
        let resp = req.send().await.map_err(|e| network_error(&e, timeout))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("读取 AI 响应失败: {}", e))?;
        Ok((status, text))
    }

    /// 轻量连接测试：一次最小 chat 请求，不保存任何配置、不检查 enabled。
    pub async fn test_connection(&self) -> Result<AiTestResult, String> {
        let r = self.resolved();
        endpoint_ready(&r)?;
        let timeout = Duration::from_secs(
            self.cfg
                .timeout_secs
                .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS)
                .min(TEST_TIMEOUT_CAP_SECS),
        );
        let started = Instant::now();
        let reply = self
            .chat(
                "你是一个连通性测试助手，只做最小回复。",
                "请只回复 OK。",
                ChatOpts {
                    json_mode: false,
                    timeout,
                },
            )
            .await?;
        let latency_ms = started.elapsed().as_millis() as u64;
        Ok(AiTestResult {
            model: r.model.clone(),
            latency_ms,
            message: format!("连接正常，回复：{}", truncate_chars(reply.trim(), 40)),
        })
    }

    /// 拉取模型列表：GET {base}/models，支持 `{"data":[{"id":...}]}` 与 `["id"]` 两种返回。
    pub async fn list_models(&self) -> Result<Vec<String>, String> {
        let r = self.resolved();
        if r.base_url.trim().is_empty() {
            return Err("AI Base URL 为空：请先填写服务商地址后再拉取模型列表。".into());
        }
        let url = models_url(&r.base_url);
        let alt_url = r
            .base_url
            .strip_suffix("/v1")
            .filter(|b| !b.is_empty())
            .map(|b| format!("{}/models", b));
        let timeout = Duration::from_secs(MODELS_TIMEOUT_SECS);
        let fetch = |url: String| {
            let req = self.http.get(&url).timeout(timeout);
            let req = with_auth(req, &r.api_key);
            async move {
                let resp = req.send().await.map_err(|e| network_error(&e, timeout))?;
                let status = resp.status();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| format!("读取模型列表响应失败: {}", e))?;
                Ok::<(reqwest::StatusCode, String), String>((status, text))
            }
        };
        let (mut status, mut text) = fetch(url).await?;
        if !status.is_success() && status.as_u16() == 404 {
            if let Some(alt) = alt_url {
                let (alt_status, alt_text) = fetch(alt).await?;
                status = alt_status;
                text = alt_text;
            }
        }
        if !status.is_success() {
            return Err(format!(
                "{}。该服务商可能不支持模型列表，请手动填写模型名。",
                http_error(status, &text)
            ));
        }
        let mut ids = parse_model_ids(&text).map_err(|e| {
            format!(
                "{}。该服务商可能不支持模型列表，请手动填写模型名。",
                e
            )
        })?;
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return Err("服务商未返回可用模型。该服务商可能不支持模型列表，请手动填写模型名。".into());
        }
        Ok(ids)
    }

    pub async fn analyze_app(&self, app: &InstalledApp) -> Result<AiAppInfo, String> {
        self.ensure_enabled()?;
        let sys = "你是一名 Windows 系统与软件专家。根据用户提供的已安装软件信息，分析其用途、类别、是否可安全卸载，并指出风险点和替代品。必须只返回严格 JSON（键名为 camelCase），不要输出 Markdown 代码围栏或额外说明。";
        let user = format!(
            r#"软件名: {}
发布者: {}
版本: {}
安装目录: {}
卸载串: {}
请只返回如下 JSON（键名必须为 camelCase）: {{"summary":"string 简明用途说明(中文)","category":"string 类别如 浏览器/开发工具/驱动/安全软件 等","isSafeToRemove":bool,"riskNotes":["string"],"alternatives":["string"]}}"#,
            app.display_name,
            app.publisher.as_deref().unwrap_or(""),
            app.version.as_deref().unwrap_or(""),
            app.install_location.as_deref().unwrap_or(""),
            app.uninstall_string.as_deref().unwrap_or(""),
        );
        let raw = self
            .chat(sys, &user, ChatOpts::analysis(&self.cfg))
            .await?;
        parse_json::<AiAppInfo>(&raw)
    }

    pub async fn analyze_uninstall_risk(
        &self,
        app: &InstalledApp,
    ) -> Result<AiUninstallRisk, String> {
        self.ensure_enabled()?;
        let sys = "你是 Windows 系统专家。评估卸载该软件的风险等级、可能受影响的其他组件和依赖，并给出建议。必须只返回严格 JSON（键名为 camelCase），不要输出 Markdown 代码围栏或额外说明。";
        let user = format!(
            r#"软件名: {}
发布者: {}
版本: {}
安装目录: {}
请只返回如下 JSON（键名必须为 camelCase）: {{"riskLevel":"low|medium|high|critical","dependencies":["string 可能依赖该软件的其他东西"],"affectedComponents":["string 受影响组件"],"recommendation":"string 建议(中文)"}}"#,
            app.display_name,
            app.publisher.as_deref().unwrap_or(""),
            app.version.as_deref().unwrap_or(""),
            app.install_location.as_deref().unwrap_or(""),
        );
        let raw = self
            .chat(sys, &user, ChatOpts::analysis(&self.cfg))
            .await?;
        parse_json::<AiUninstallRisk>(&raw)
    }

    pub async fn assess_residuals(
        &self,
        app: &InstalledApp,
        residuals: &[ResidualItem],
    ) -> Result<AiResidualAssessment, String> {
        self.ensure_enabled()?;
        let sys = "你是 Windows 清理专家。卸载某软件后发现了以下残留。请逐项评估每个残留是否可以安全删除（safe）/需谨慎(caution)/不建议删除(keep)/无法判断(unknown)，并说明理由。同时给出整体结论。必须只返回严格 JSON（键名为 camelCase），不要输出 Markdown 代码围栏或额外说明。";
        let items_json: Vec<String> = residuals
            .iter()
            .map(|r| {
                format!(
                    r#"{{"id":"{}","kind":"{}","path":"{}","detail":{}}}"#,
                    r.id,
                    kind_str(r.kind),
                    r.path.replace('"', "'"),
                    r.detail
                        .as_ref()
                        .map(|s| format!("\"{}\"", s.replace('"', "'")))
                        .unwrap_or_else(|| "null".into())
                )
            })
            .collect();
        let user = format!(
            r#"已卸载软件: {} (发布者: {})
残留项:
{}

请只返回如下 JSON（键名必须为 camelCase）: {{"overall":"safe|caution|keep|unknown","perItem":[{{"residualId":"string","verdict":"safe|caution|keep|unknown","reason":"string"}}]}}"#,
            app.display_name,
            app.publisher.as_deref().unwrap_or(""),
            items_json.join(",\n")
        );
        let raw = self
            .chat(sys, &user, ChatOpts::analysis(&self.cfg))
            .await?;
        parse_json::<AiResidualAssessment>(&raw)
    }
}

/// 解析模型输出（先做容错 JSON 提取）。
fn parse_json<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, String> {
    let json = extract_json(raw);
    if json.trim().is_empty() {
        return Err(format!(
            "AI 未返回 JSON 内容。原始: {}",
            truncate_chars(raw, 400)
        ));
    }
    serde_json::from_str::<T>(&json).map_err(|e| {
        format!(
            "AI 返回 JSON 解析失败: {} | 原始: {}",
            e,
            truncate_chars(&json, 400)
        )
    })
}

/// API Key 为空时不发 Authorization 头（Ollama / LM Studio 等本地服务）。
fn with_auth(req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
    let key = api_key.trim();
    if key.is_empty() {
        req
    } else {
        req.bearer_auth(key)
    }
}

fn http_error(status: reqwest::StatusCode, body: &str) -> String {
    let code = status.as_u16();
    let hint = match code {
        401 | 403 => "API Key 无效、已过期或没有该模型的权限",
        404 => "接口路径不存在：请检查 Base URL 是否缺少 /v1，以及模型名是否正确",
        400 | 422 => "请求被服务商拒绝：可能不支持 JSON 模式或参数不兼容",
        408 => "请求超时，请检查网络或代理",
        429 => "请求过于频繁或额度不足，请稍后重试",
        500..=599 => "服务商服务异常，请稍后重试",
        _ => "请检查 Base URL、API Key 与模型名",
    };
    format!(
        "HTTP {}（{}）：{} | 原始响应: {}",
        code,
        status.canonical_reason().unwrap_or(""),
        hint,
        truncate_chars(body.trim(), 500)
    )
}

fn network_error(e: &reqwest::Error, timeout: Duration) -> String {
    if e.is_timeout() {
        format!(
            "请求超时（{} 秒）：请检查网络/代理，或在高级设置中增大超时时间。",
            timeout.as_secs()
        )
    } else if e.is_connect() {
        "无法连接到 AI 服务：请检查 Base URL 是否正确、服务是否已启动、网络或代理设置。".into()
    } else {
        format!("请求失败: {}", e)
    }
}

/// 解析 /models 返回：兼容 `{"data":[{"id":"m"}]}`、`{"models":[...]}`、`["m"]`。
fn parse_model_ids(text: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| {
        format!(
            "解析模型列表失败: {} | 原始: {}",
            e,
            truncate_chars(text, 300)
        )
    })?;
    let mut ids = Vec::new();
    match &v {
        Value::Array(arr) => collect_model_ids(arr, &mut ids),
        Value::Object(obj) => {
            if let Some(Value::Array(arr)) = obj.get("data") {
                collect_model_ids(arr, &mut ids);
            } else if let Some(Value::Array(arr)) = obj.get("models") {
                collect_model_ids(arr, &mut ids);
            }
        }
        _ => {}
    }
    Ok(ids)
}

fn collect_model_ids(arr: &[Value], out: &mut Vec<String>) {
    for item in arr {
        let id = match item {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        if let Some(id) = id {
            let id = id.trim().to_string();
            if !id.is_empty() {
                out.push(id);
            }
        }
    }
}

fn kind_str(k: crate::types::ResidualKind) -> &'static str {
    use crate::types::ResidualKind::*;
    match k {
        File => "file",
        Directory => "directory",
        RegistryKey => "registry_key",
        Shortcut => "shortcut",
        Service => "service",
        ScheduledTask => "task",
        EnvVar => "envvar",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AiSafetyVerdict, AppSource, ResidualKind, RiskLevel};

    // ----- Base URL 归一化 -----

    #[test]
    fn normalize_known_provider_urls() {
        // 契约里明确给出的 5 个例子
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("127.0.0.1:11434"),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            normalize_base_url("http://localhost:1234/v1"),
            "http://localhost:1234/v1"
        );
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
    }

    #[test]
    fn normalize_trims_and_handles_trailing_slashes() {
        assert_eq!(
            normalize_base_url("  https://api.openai.com/v1/  "),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url("http://localhost:11434/"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1//"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("api.openai.com"),
            "https://api.openai.com/v1",
            "公网域名默认 https"
        );
        assert_eq!(
            normalize_base_url("192.168.1.20:8080"),
            "http://192.168.1.20:8080/v1",
            "局域网默认 http"
        );
        assert_eq!(normalize_base_url(""), "");
        // 已含非根路径的服务前缀保持不变
        assert_eq!(
            normalize_base_url("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn normalize_is_idempotent() {
        for raw in [
            "https://api.openai.com/v1",
            "https://api.deepseek.com",
            "127.0.0.1:11434",
            "http://localhost:1234/v1",
            "https://openrouter.ai/api/v1",
        ] {
            let once = normalize_base_url(raw);
            assert_eq!(normalize_base_url(&once), once, "raw = {}", raw);
        }
    }

    #[test]
    fn endpoint_urls() {
        assert_eq!(
            chat_completions_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            models_url("http://localhost:11434"),
            "http://localhost:11434/v1/models"
        );
    }

    // ----- JSON 提取 -----

    #[test]
    fn extract_json_strips_code_fences_and_prose() {
        let fenced = "```json\n{\"summary\":\"ok\",\"isSafeToRemove\":true}\n```";
        assert_eq!(
            extract_json(fenced),
            "{\"summary\":\"ok\",\"isSafeToRemove\":true}"
        );

        let prose = "好的，分析结果如下：\n```json\n{\"a\": 1, \"b\": {\"c\": 2}}\n```\n希望有帮助。";
        assert_eq!(extract_json(prose), "{\"a\": 1, \"b\": {\"c\": 2}}");

        let bare_prose = "Here you go: {\"x\":[1,2,3]} thanks";
        assert_eq!(extract_json(bare_prose), "{\"x\":[1,2,3]}");

        let plain = "{\"ok\":true}";
        assert_eq!(extract_json(plain), "{\"ok\":true}");

        // 极端情况：围栏与内容挤在一行
        assert_eq!(
            extract_json("```json{\"ok\":true}```"),
            "{\"ok\":true}"
        );

        // 没有 JSON 时原样返回（由上层报错）
        assert_eq!(extract_json("  no json here  "), "no json here");
    }

    // ----- camelCase / snake_case 兼容 + 枚举大小写 -----

    #[test]
    fn parse_app_info_camel_and_snake_case() {
        let camel: AiAppInfo = serde_json::from_str(
            r#"{"summary":"浏览器","category":"网络","isSafeToRemove":true,"riskNotes":["会丢书签"],"alternatives":["Edge"]}"#,
        )
        .expect("camelCase 应解析");
        assert!(camel.is_safe_to_remove);
        assert_eq!(camel.risk_notes, vec!["会丢书签"]);

        let snake: AiAppInfo = serde_json::from_str(
            r#"{"summary":"浏览器","category":"网络","is_safe_to_remove":true,"risk_notes":["会丢书签"],"alternatives":[]}"#,
        )
        .expect("snake_case 应解析");
        assert!(snake.is_safe_to_remove);

        // 缺省集合字段按空处理
        let minimal: AiAppInfo =
            serde_json::from_str(r#"{"summary":"x","isSafeToRemove":false}"#)
                .expect("最小字段应解析");
        assert!(minimal.risk_notes.is_empty());
        assert!(minimal.alternatives.is_empty());
    }

    #[test]
    fn parse_uninstall_risk_aliases_and_case_insensitive_enum() {
        let camel: AiUninstallRisk = serde_json::from_str(
            r#"{"riskLevel":"High","dependencies":[],"affectedComponents":["驱动"],"recommendation":"x"}"#,
        )
        .expect("camelCase 应解析");
        assert_eq!(camel.risk_level, RiskLevel::High);

        let snake_upper: AiUninstallRisk = serde_json::from_str(
            r#"{"risk_level":"CRITICAL","dependencies":[],"affected_components":[],"recommendation":"x"}"#,
        )
        .expect("snake_case + 大写枚举应解析");
        assert_eq!(snake_upper.risk_level, RiskLevel::Critical);

        // 风险等级未知值报错，避免静默降级
        assert!(serde_json::from_str::<AiUninstallRisk>(
            r#"{"riskLevel":"extreme","dependencies":[],"affectedComponents":[],"recommendation":"x"}"#
        )
        .is_err());
    }

    #[test]
    fn parse_residual_assessment_aliases_and_case_insensitive_verdict() {
        let snake: AiResidualAssessment = serde_json::from_str(
            r#"{"overall":"SAFE","per_item":[{"residual_id":"r1","verdict":"Caution","reason":"可能被占用"},{"residual_id":"r2","verdict":"UNKNOWN","reason":"?"}]}"#,
        )
        .expect("snake_case + 混合大小写应解析");
        assert_eq!(snake.overall, AiSafetyVerdict::Safe);
        assert_eq!(snake.per_item.len(), 2);
        assert_eq!(snake.per_item[0].residual_id, "r1");
        assert_eq!(snake.per_item[0].verdict, AiSafetyVerdict::Caution);
        assert_eq!(snake.per_item[1].verdict, AiSafetyVerdict::Unknown);

        let camel: AiResidualAssessment = serde_json::from_str(
            r#"{"overall":"caution","perItem":[{"residualId":"r1","verdict":"safe","reason":"x"}]}"#,
        )
        .expect("camelCase 应解析");
        assert_eq!(camel.per_item[0].verdict, AiSafetyVerdict::Safe);

        // 未知 verdict 回退 Unknown，不炸整段解析
        let fallback: AiResidualAssessment =
            serde_json::from_str(r#"{"overall":"weird","perItem":[]}"#).unwrap();
        assert_eq!(fallback.overall, AiSafetyVerdict::Unknown);
    }

    #[test]
    fn verdict_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&AiSafetyVerdict::Safe).unwrap(),
            "\"safe\""
        );
        assert_eq!(
            serde_json::to_string(&AiSafetyVerdict::Caution).unwrap(),
            "\"caution\""
        );
        assert_eq!(
            serde_json::to_string(&RiskLevel::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&AiSafetyVerdict::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn effective_ai_serializes_camel_case_contract() {
        let e = EffectiveAi {
            enabled: true,
            ready: true,
            base_url: "http://127.0.0.1:11434/v1".into(),
            base_url_from_env: false,
            model: "llama3".into(),
            model_from_env: false,
            api_key_masked: String::new(),
            api_key_from_env: false,
            use_env: false,
            config_path: "C:\\x\\ai_config.json".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["ready"], true);
        assert_eq!(v["baseUrl"], "http://127.0.0.1:11434/v1");
        assert_eq!(v["apiKeyMasked"], "");
        assert_eq!(v["configPath"], "C:\\x\\ai_config.json");
        assert!(v.get("base_url").is_none(), "不应出现 snake_case 键");
    }

    #[test]
    fn parse_json_tolerates_fences_end_to_end() {
        let raw = "```json\n{\"summary\":\"x\",\"isSafeToRemove\":false}\n```";
        let info: AiAppInfo = parse_json(raw).expect("带围栏的返回应能解析");
        assert_eq!(info.summary, "x");
    }

    // ----- UTF-8 安全截断 / 脱敏 -----

    #[test]
    fn truncate_is_char_safe() {
        // 10 个中文字符，截 4 个字符不应 panic 且结果正确
        let s = "你好世界这是一个测试";
        let t = truncate_chars(s, 4);
        assert!(t.starts_with("你好世界"));
        assert!(t.ends_with("(截断)"));
        assert_eq!(truncate_chars(s, 100), s);
        // emoji（多字节）也不能 panic
        let e = "😀😀😀😀😀😀";
        let et = truncate_chars(e, 3);
        assert!(et.starts_with("😀😀😀"));
        assert!(et.ends_with("(截断)"));
    }

    #[test]
    fn mask_is_char_safe() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("   "), "");
        assert_eq!(mask_secret("short"), "••••");
        assert_eq!(mask_secret("12345678"), "••••");
        // 中文/emoji key（理论上罕见）不能按字节切 panic
        let masked = mask_secret("中文密钥测试用abcdefgh");
        assert!(masked.contains('…'));
        assert_eq!(mask_secret("sk-1234567890abcdef"), "sk-1…cdef");
    }

    // ----- 无 Key 支持 -----

    #[test]
    fn api_key_is_optional() {
        let cfg = AiConfig {
            base_url: "127.0.0.1:11434".into(),
            api_key: String::new(),
            model: "llama3".into(),
            use_env: false,
            ..Default::default()
        };
        let r = resolve_values(&cfg);
        assert_eq!(r.base_url, "http://127.0.0.1:11434/v1");
        assert!(r.api_key.is_empty());
        assert!(endpoint_ready(&r).is_ok(), "无 Key 的本地服务应可用");

        // 缺 Base URL / 模型名仍然报错
        assert!(endpoint_ready(&Resolved {
            base_url: "".into(),
            api_key: "k".into(),
            model: "m".into(),
        })
        .is_err());
        assert!(endpoint_ready(&Resolved {
            base_url: "http://x/v1".into(),
            api_key: "".into(),
            model: "  ".into(),
        })
        .is_err());
    }

    #[test]
    fn empty_api_key_omits_authorization_header() {
        let http = reqwest::Client::new();
        let body = serde_json::json!({"model":"m","messages":[]});
        let url = "http://127.0.0.1:9/v1/chat/completions";

        let built = with_auth(http.post(url).json(&body), "   ")
            .build()
            .expect("empty key request builds");
        assert!(built
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none());

        let built = with_auth(http.post(url).json(&body), "sk-test")
            .build()
            .expect("keyed request builds");
        assert_eq!(
            built
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer sk-test"
        );
    }

    // ----- 环境变量回退 -----

    #[test]
    fn empty_env_value_falls_back_to_static_config() {
        // 唯一变量名，避免与其他并行测试互相影响
        let name = "CLEANSWEEP_TEST_EMPTY_ENV_VALUE";
        std::env::set_var(name, "   ");
        let cfg = AiConfig {
            use_env: true,
            env_api_key_name: name.into(),
            api_key: "static-key".into(),
            ..Default::default()
        };
        assert_eq!(resolve_values(&cfg).api_key, "static-key");

        std::env::set_var(name, "  env-key  ");
        assert_eq!(resolve_values(&cfg).api_key, "env-key");
        std::env::remove_var(name);

        // 变量名空白视为不读环境变量
        let cfg = AiConfig {
            use_env: true,
            env_api_key_name: "   ".into(),
            api_key: "static-key".into(),
            ..Default::default()
        };
        assert_eq!(resolve_values(&cfg).api_key, "static-key");
    }

    #[test]
    fn probe_env_vars_masks_and_detects() {
        let name = "CLEANSWEEP_TEST_PROBE_ENV_VALUE";
        std::env::set_var(name, "sk-probe-1234567890");
        let probes = probe_env_vars(&[name.to_string(), "CLEANSWEEP_TEST_NOT_EXIST_XYZ".into()]);
        std::env::remove_var(name);

        let found = probes.get(name).expect("应包含探测项");
        assert!(found.found);
        assert_eq!(found.masked, "sk-p…7890");

        let missing = probes
            .get("CLEANSWEEP_TEST_NOT_EXIST_XYZ")
            .expect("应包含探测项");
        assert!(!missing.found);
        assert_eq!(missing.masked, "");
    }

    // ----- /models 解析 -----

    #[test]
    fn parse_models_object_and_array_shapes() {
        let ids = parse_model_ids(r#"{"data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#).unwrap();
        assert_eq!(ids, vec!["b", "a", "a"]);

        let ids = parse_model_ids(r#"["gpt-4o-mini","deepseek-chat"]"#).unwrap();
        assert_eq!(ids, vec!["gpt-4o-mini", "deepseek-chat"]);

        let ids = parse_model_ids(r#"{"models":[{"id":"x"}]}"#).unwrap();
        assert_eq!(ids, vec!["x"]);

        assert!(parse_model_ids("not json").is_err());
    }

    // 覆盖 kind_str 及类型引用
    #[test]
    fn residual_kind_helper() {
        let _ = ResidualItem {
            id: "r".into(),
            kind: ResidualKind::Directory,
            path: "C:\\x".into(),
            display: "x".into(),
            detail: None,
            size_bytes: None,
            in_use: false,
            ai_verdict: None,
        };
        assert_eq!(kind_str(ResidualKind::ScheduledTask), "task");
    }

    // ----- 回环 HTTP 集成测试（本地 mock 服务，不访问外网） -----

    /// 读取一条完整的 HTTP/1.1 请求（header + content-length 指定的 body）。
    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut tmp).expect("读取请求失败");
            if n == 0 {
                return String::from_utf8_lossy(&buf).to_string();
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_len = head
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        while buf.len() < header_end + content_len {
            let n = stream.read(&mut tmp).expect("读取请求体失败");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// 启动 N 次请求的本地 mock（每次连接返回一条响应），返回 base_url 与请求记录句柄。
    fn mock_server(
        responses: Vec<(u16, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock 端口失败");
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (code, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept 失败");
                requests.push(read_http_request(&mut stream));
                let reason = match code {
                    200 => "OK",
                    400 => "Bad Request",
                    404 => "Not Found",
                    422 => "Unprocessable Entity",
                    _ => "Error",
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    code,
                    reason,
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).expect("写响应失败");
                let _ = stream.flush();
            }
            requests
        });
        (format!("127.0.0.1:{}", port), handle)
    }

    fn chat_completion_body(content: &str) -> String {
        serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        })
        .to_string()
    }

    fn sample_app() -> InstalledApp {
        InstalledApp {
            id: "test-app".into(),
            display_name: "Test App".into(),
            publisher: Some("Test Publisher".into()),
            version: Some("1.0".into()),
            install_location: Some("C:\\Program Files\\Test".into()),
            install_date: None,
            uninstall_string: Some("C:\\Program Files\\Test\\uninstall.exe".into()),
            quiet_uninstall_string: None,
            display_icon: None,
            estimated_size_kb: None,
            source: AppSource::RegistryUser,
            registry_key: None,
            is_system_component: false,
            is_protected: false,
            protected_reason: None,
            package_family: None,
        }
    }

    #[tokio::test]
    async fn http_test_connection_without_api_key_sends_no_auth_header() {
        let (base, handle) = mock_server(vec![(200, chat_completion_body("OK"))]);
        let cfg = AiConfig {
            enabled: false, // 连接测试不应要求 enabled
            base_url: format!("{}/", base), // 也验证尾部斜杠归一化
            api_key: String::new(),
            model: "mock-model".into(),
            ..Default::default()
        };
        let client = Client::new(cfg);
        let res = client.test_connection().await.expect("连接测试应成功");
        assert_eq!(res.model, "mock-model");
        assert!(res.message.contains("连接正常"));

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert!(
            req.starts_with("POST /v1/chat/completions HTTP/1.1"),
            "URL 归一化错误: {}",
            req
        );
        let lower = req.to_ascii_lowercase();
        assert!(
            !lower.contains("authorization:"),
            "无 API Key 时不应发送 Authorization 头: {}",
            req
        );
        assert!(
            !lower.contains("max_tokens"),
            "连接测试不应发送 max_tokens（新模型可能已弃用该参数）: {}",
            req
        );
    }

    #[tokio::test]
    async fn http_analyze_app_parses_fenced_snake_case_json() {
        let content = "```json\n{\"summary\":\"用途说明\",\"category\":\"工具\",\"is_safe_to_remove\":true,\"risk_notes\":[\"注意\"],\"alternatives\":[\"Edge\"]}\n```";
        let (base, handle) = mock_server(vec![(200, chat_completion_body(content))]);
        let cfg = AiConfig {
            enabled: true,
            base_url: base, // 无 /v1，应自动补全
            api_key: "sk-test".into(),
            model: "mock-model".into(),
            ..Default::default()
        };
        let client = Client::new(cfg);
        let info = client.analyze_app(&sample_app()).await.expect("分析应成功");
        assert_eq!(info.summary, "用途说明");
        assert_eq!(info.category.as_deref(), Some("工具"));
        assert!(info.is_safe_to_remove);
        assert_eq!(info.risk_notes, vec!["注意"]);
        assert_eq!(info.alternatives, vec!["Edge"]);

        let requests = handle.join().unwrap();
        let req = &requests[0];
        assert!(
            req.starts_with("POST /v1/chat/completions HTTP/1.1"),
            "URL 归一化错误: {}",
            req
        );
        let lower = req.to_ascii_lowercase();
        assert!(
            lower.contains("authorization: bearer sk-test"),
            "有 API Key 时应发送 Bearer 头: {}",
            req
        );
        assert!(
            lower.contains("json_object"),
            "分析请求应启用 JSON 模式: {}",
            req
        );
        assert!(req.contains("camelCase") || req.contains("isSafeToRemove"));
    }

    #[tokio::test]
    async fn http_list_models_sorts_and_dedupes() {
        let body = r#"{"data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#;
        let (base, handle) = mock_server(vec![(200, body.to_string())]);
        let cfg = AiConfig {
            base_url: format!("{}/v1", base),
            model: String::new(), // 拉取模型列表不要求模型名
            ..Default::default()
        };
        let ids = Client::new(cfg)
            .list_models()
            .await
            .expect("模型列表应成功");
        assert_eq!(ids, vec!["a", "b"]);

        let requests = handle.join().unwrap();
        assert!(
            requests[0].starts_with("GET /v1/models HTTP/1.1"),
            "模型列表 URL 错误: {}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn http_404_on_v1_falls_back_to_bare_base() {
        // DeepSeek 2026 文档只给根路径；显式 /v1 万一 404，应自动回退到根路径再试。
        let ok = chat_completion_body("{\"summary\":\"x\",\"isSafeToRemove\":false}");
        let (base, handle) = mock_server(vec![
            (404, "{\"error\":\"not found\"}".into()),
            (200, ok),
        ]);
        let cfg = AiConfig {
            enabled: true,
            base_url: format!("{}/v1", base),
            model: "mock-model".into(),
            api_key: "sk-test".into(),
            ..Default::default()
        };
        let info = Client::new(cfg)
            .analyze_app(&sample_app())
            .await
            .expect("404 后应回退到根路径并成功");
        assert_eq!(info.summary, "x");

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 2, "应恰好尝试两次");
        assert!(
            requests[0].starts_with("POST /v1/chat/completions HTTP/1.1"),
            "{}",
            requests[0]
        );
        assert!(
            requests[1].starts_with("POST /chat/completions HTTP/1.1"),
            "{}",
            requests[1]
        );
    }

    #[tokio::test]
    async fn http_degrades_json_mode_then_temperature() {
        let ok = chat_completion_body("{\"summary\":\"x\",\"isSafeToRemove\":false}");
        let (base, handle) = mock_server(vec![
            (400, "{\"error\":\"response_format is not supported\"}".into()),
            (400, "{\"error\":\"temperature is not supported\"}".into()),
            (200, ok),
        ]);
        let cfg = AiConfig {
            enabled: true,
            base_url: format!("{}/v1", base),
            model: "mock-model".into(),
            ..Default::default()
        };
        let info = Client::new(cfg)
            .analyze_app(&sample_app())
            .await
            .expect("逐级降级后应成功");
        assert_eq!(info.summary, "x");

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 3, "应依次尝试 3 种请求形态");
        assert!(requests[0].to_ascii_lowercase().contains("json_object"));
        let second = requests[1].to_ascii_lowercase();
        assert!(!second.contains("response_format"), "第二次应去掉 JSON 模式");
        assert!(second.contains("temperature"), "第二次仍带 temperature");
        let third = requests[2].to_ascii_lowercase();
        assert!(!third.contains("response_format"));
        assert!(!third.contains("temperature"), "第三次应去掉 temperature");
    }
}
