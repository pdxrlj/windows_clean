use serde::{Deserialize, Serialize};

/// 一条已安装软件记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    pub id: String,
    pub display_name: String,
    pub publisher: Option<String>,
    pub version: Option<String>,
    pub install_location: Option<String>,
    pub install_date: Option<String>,
    pub uninstall_string: Option<String>,
    pub quiet_uninstall_string: Option<String>,
    pub display_icon: Option<String>,
    pub estimated_size_kb: Option<u64>,
    pub source: AppSource,
    pub registry_key: Option<String>,
    pub is_system_component: bool,
    /// 是否禁止卸载（系统组件 / Windows 更新 / 驱动包 / 运行库 / 受保护 Store 包）。
    #[serde(default)]
    pub is_protected: bool,
    /// 禁止卸载的原因，展示给用户。
    #[serde(default)]
    pub protected_reason: Option<String>,
    /// Store 应用的 PackageFamilyName（用于 ConsentStore 能力管理）。
    #[serde(default)]
    pub package_family: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppSource {
    /// HKLM 64位 Uninstall 键
    RegistryMachine64,
    /// HKLM 32位 (WOW6432) Uninstall 键
    RegistryMachine32,
    /// HKCU Uninstall 键
    RegistryUser,
    /// UWP / MSIX / Store 应用
    Store,
    /// Steam 游戏（若发现）
    Steam,
}

/// 残留项
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidualItem {
    pub id: String,
    pub kind: ResidualKind,
    pub path: String,
    pub display: String,
    /// 子项（目录里的文件、注册表子键等）数量或简述
    pub detail: Option<String>,
    /// 估计占用字节数（目录/文件）
    pub size_bytes: Option<u64>,
    /// 是否可能在被使用/锁定
    pub in_use: bool,
    /// AI 评估的安全等级
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_verdict: Option<AiSafetyVerdict>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResidualKind {
    File,
    Directory,
    RegistryKey,
    Shortcut,
    Service,
    ScheduledTask,
    EnvVar,
}

/// AI 安全等级。
///
/// 序列化给前端为小写（safe/caution/keep/unknown）；反序列化大小写不敏感
/// （"Low"、"SAFE"、"Caution" 均可），无法识别的值回退为 Unknown，
/// 避免模型偶发的大小写差异导致整段 JSON 解析失败。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AiSafetyVerdict {
    /// 可放心删除
    Safe,
    /// 谨慎删除，可能影响其他软件
    Caution,
    /// 不建议删除
    Keep,
    /// 无法判断
    Unknown,
}

impl AiSafetyVerdict {
    /// 大小写不敏感解析；未知值回退为 Unknown。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "safe" | "safe_to_remove" => Self::Safe,
            "caution" | "careful" | "warning" | "risky" => Self::Caution,
            "keep" | "do_not_delete" | "donotdelete" | "unsafe" => Self::Keep,
            _ => Self::Unknown,
        }
    }
}

impl<'de> Deserialize<'de> for AiSafetyVerdict {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s))
    }
}

/// AI 用途分析结果。字段兼容 camelCase / snake_case 两种键名。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAppInfo {
    pub summary: String,
    pub category: Option<String>,
    #[serde(default, alias = "is_safe_to_remove")]
    pub is_safe_to_remove: bool,
    #[serde(default, alias = "risk_notes")]
    pub risk_notes: Vec<String>,
    #[serde(default)]
    pub alternatives: Vec<String>,
}

/// AI 卸载风险分析。字段兼容 camelCase / snake_case 两种键名。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUninstallRisk {
    #[serde(alias = "risk_level")]
    pub risk_level: RiskLevel,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default, alias = "affected_components")]
    pub affected_components: Vec<String>,
    #[serde(default)]
    pub recommendation: String,
}

/// 卸载风险等级。序列化为小写（low/medium/high/critical）；
/// 反序列化大小写不敏感，未知值报错（风险等级不能静默改判）。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    /// 大小写不敏感解析；无法识别返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "minimal" | "minor" => Some(Self::Low),
            "medium" | "moderate" | "mid" => Some(Self::Medium),
            "high" | "major" => Some(Self::High),
            "critical" | "severe" => Some(Self::Critical),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for RiskLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("无法识别的风险等级: {}", s))
        })
    }
}

/// AI 残留评估。字段兼容 camelCase / snake_case 两种键名。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiResidualAssessment {
    pub overall: AiSafetyVerdict,
    #[serde(default, alias = "per_item")]
    pub per_item: Vec<AiResidualItemVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiResidualItemVerdict {
    #[serde(alias = "residual_id")]
    pub residual_id: String,
    pub verdict: AiSafetyVerdict,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub log: String,
    pub requires_reboot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub id: String,
    pub success: bool,
    pub message: String,
}

/// AI 配置。序列化键名为 camelCase。
///
/// `#[serde(default)]` 保证旧版本/残缺的 ai_config.json 也能加载：
/// 缺失字段使用 Default 里的默认值，而不是整份配置加载失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub enabled: bool,
    /// 若为 true，则每次读取/调用 AI 时优先从环境变量取 base_url/api_key/model
    /// （由 env_api_key_name / env_base_url_name / env_model_name 指定变量名）。
    pub use_env: bool,
    /// 环境变量名：API Key。空则不读。
    pub env_api_key_name: String,
    /// 环境变量名：Base URL。
    pub env_base_url_name: String,
    /// 环境变量名：Model。
    pub env_model_name: String,
    /// 采样温度，0.0 ~ 2.0，默认 0.2。
    pub temperature: f32,
    /// 请求超时（秒），5 ~ 600，默认 90。
    pub timeout_secs: u64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "gpt-6-luna".into(),
            enabled: false,
            use_env: false,
            env_api_key_name: "OPENAI_API_KEY".into(),
            env_base_url_name: "OPENAI_BASE_URL".into(),
            env_model_name: "OPENAI_MODEL".into(),
            temperature: 0.2,
            timeout_secs: 90,
        }
    }
}

/// 环境变量探测结果（敏感值已脱敏）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvProbe {
    /// 变量存在且值非空（空白视为未设置）。
    pub found: bool,
    /// 缺失时为 ""；长度 > 8 个字符时为「前4…后4」；否则 "••••"。
    pub masked: String,
}

/// 当前实际生效的 AI 配置（key 已脱敏）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAi {
    pub enabled: bool,
    /// enabled 且 baseUrl / model 非空（不要求 API Key，本地服务可无 Key）。
    pub ready: bool,
    pub base_url: String,
    pub base_url_from_env: bool,
    pub model: String,
    pub model_from_env: bool,
    pub api_key_masked: String,
    pub api_key_from_env: bool,
    pub use_env: bool,
    /// 配置文件绝对路径。
    pub config_path: String,
}

/// 连接测试结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTestResult {
    pub model: String,
    pub latency_ms: u64,
    pub message: String,
}
