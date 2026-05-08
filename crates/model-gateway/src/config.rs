use platform::storage;
use platform::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    Anthropic,
    Gemini,
    /// DeepSeek `chat.deepseek.com` 网页端协议（PoW + 路径式 SSE），
    /// 用账号登录拿到的 token 走这一路。
    Deepseek,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    ApiKey,
    OauthCodex,
    OauthClaudeCode,
    OauthGeminiCli,
}

impl Default for AuthMode {
    fn default() -> Self {
        AuthMode::ApiKey
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub auth_mode: AuthMode,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_expires_at: Option<i64>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProvidersFile {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub default_provider_id: Option<String>,
}

pub fn load(data_dir: &Path) -> AppResult<ProvidersFile> {
    let path = storage::providers_path(data_dir);
    storage::read_json(&path)
}

pub fn save(data_dir: &Path, file: &ProvidersFile) -> AppResult<()> {
    let path = storage::providers_path(data_dir);
    storage::write_json(&path, file)
}

pub fn get(data_dir: &Path, id: &str) -> AppResult<Provider> {
    load(data_dir)?
        .providers
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| AppError::msg(format!("provider {id} not found")))
}

pub fn upsert(data_dir: &Path, provider: Provider) -> AppResult<Provider> {
    let mut file = load(data_dir)?;
    if let Some(existing) = file.providers.iter_mut().find(|p| p.id == provider.id) {
        *existing = provider.clone();
    } else {
        file.providers.push(provider.clone());
    }
    save(data_dir, &file)?;
    Ok(provider)
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
    /// 该预设的默认模型；为空时由前端按惯例选 models[0]。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<&'static str>,
    pub website: &'static str,
    pub note: &'static str,
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "openai",
        name: "OpenAI",
        kind: ProviderKind::Openai,
        base_url: "https://api.openai.com/v1",
        models: &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "o1-mini",
            "o1-preview",
        ],
        default_model: Some("gpt-4o"),
        website: "https://platform.openai.com/api-keys",
        note: "官方 OpenAI 接口",
    },
    ProviderPreset {
        id: "deepseek",
        name: "DeepSeek (API Key)",
        kind: ProviderKind::Openai,
        // 与 deepseek-tui 对齐：beta endpoint 默认对所有地区开放，
        // 解锁 strict tool mode 等 beta 特性。`api.deepseek.com/v1` 仍可手动改回。
        base_url: "https://api.deepseek.com/beta",
        // V4 家族（1M 上下文，支持 reasoning_content 思维链）
        models: &[
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v4-pro-search",
            "deepseek-v4-flash-search",
            "deepseek-v4-vision",
            "deepseek-chat",
            "deepseek-reasoner",
        ],
        default_model: Some("deepseek-v4-pro"),
        website: "https://platform.deepseek.com/",
        note: "深度求索 V4 系列（1M 上下文 · 支持 thinking · beta endpoint，含 strict tool）",
    },
    ProviderPreset {
        id: "deepseek_web",
        name: "DeepSeek (账号登录)",
        kind: ProviderKind::Deepseek,
        base_url: "https://chat.deepseek.com",
        models: &[
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v4-pro-search",
            "deepseek-v4-flash-search",
            "deepseek-v4-vision",
            "deepseek-v4-pro-nothinking",
            "deepseek-v4-flash-nothinking",
        ],
        default_model: Some("deepseek-v4-pro"),
        website: "https://chat.deepseek.com/",
        note: "用 chat.deepseek.com 账号登录（带 PoW + 路径式 SSE），免 API Key",
    },
    ProviderPreset {
        id: "zhipu_glm",
        name: "智谱 GLM",
        kind: ProviderKind::Openai,
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &["glm-4.6", "glm-4-plus", "glm-4-air", "glm-4-flash"],
        default_model: Some("glm-4.6"),
        website: "https://open.bigmodel.cn/",
        note: "智谱 AI，OpenAI 兼容",
    },
    ProviderPreset {
        id: "kimi",
        name: "Kimi (Moonshot)",
        kind: ProviderKind::Openai,
        base_url: "https://api.moonshot.cn/v1",
        models: &[
            "moonshot-v1-128k",
            "moonshot-v1-32k",
            "kimi-k2-0711-preview",
        ],
        default_model: Some("moonshot-v1-128k"),
        website: "https://platform.moonshot.cn/",
        note: "月之暗面 Kimi，OpenAI 兼容",
    },
    ProviderPreset {
        id: "qwen",
        name: "阿里百炼 (Qwen)",
        kind: ProviderKind::Openai,
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        models: &["qwen-max", "qwen-plus", "qwen-turbo", "qwen3-coder-plus"],
        default_model: Some("qwen-plus"),
        website: "https://bailian.console.aliyun.com/",
        note: "通义千问，OpenAI 兼容端点",
    },
    ProviderPreset {
        id: "doubao",
        name: "豆包 (火山方舟)",
        kind: ProviderKind::Openai,
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        models: &["doubao-seed-1-6", "doubao-pro-256k", "doubao-1-5-pro-32k"],
        default_model: Some("doubao-seed-1-6"),
        website: "https://console.volcengine.com/ark",
        note: "字节跳动豆包，OpenAI 兼容",
    },
    ProviderPreset {
        id: "siliconflow",
        name: "SiliconFlow",
        kind: ProviderKind::Openai,
        base_url: "https://api.siliconflow.cn/v1",
        models: &["Qwen/Qwen2.5-72B-Instruct", "deepseek-ai/DeepSeek-V3"],
        default_model: Some("deepseek-ai/DeepSeek-V3"),
        website: "https://cloud.siliconflow.cn/",
        note: "硅基流动，多模型聚合",
    },
    ProviderPreset {
        id: "minimax",
        name: "MiniMax",
        kind: ProviderKind::Openai,
        base_url: "https://api.minimaxi.com/v1",
        models: &["MiniMax-M1", "abab6.5s-chat"],
        default_model: Some("MiniMax-M1"),
        website: "https://www.minimaxi.com/",
        note: "MiniMax，OpenAI 兼容",
    },
    ProviderPreset {
        id: "stepfun",
        name: "StepFun (阶跃)",
        kind: ProviderKind::Openai,
        base_url: "https://api.stepfun.com/v1",
        models: &["step-2-16k", "step-1-flash"],
        default_model: Some("step-2-16k"),
        website: "https://platform.stepfun.com/",
        note: "阶跃星辰，OpenAI 兼容",
    },
    ProviderPreset {
        id: "anthropic",
        name: "Anthropic Claude",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.anthropic.com",
        models: &[
            "claude-opus-4-5",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-3-5-sonnet-latest",
        ],
        default_model: Some("claude-sonnet-4-5"),
        website: "https://console.anthropic.com/",
        note: "官方 Anthropic 接口",
    },
    ProviderPreset {
        id: "zhipu_glm_anthropic",
        name: "智谱 GLM (Anthropic 入口)",
        kind: ProviderKind::Anthropic,
        base_url: "https://open.bigmodel.cn/api/anthropic",
        models: &["glm-4.6", "glm-4-plus"],
        default_model: Some("glm-4.6"),
        website: "https://open.bigmodel.cn/",
        note: "智谱提供的 Anthropic 兼容端点",
    },
    ProviderPreset {
        id: "kimi_anthropic",
        name: "Kimi (Anthropic 入口)",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.moonshot.cn/anthropic",
        models: &["kimi-k2-0711-preview"],
        default_model: Some("kimi-k2-0711-preview"),
        website: "https://platform.moonshot.cn/",
        note: "Moonshot 提供的 Anthropic 兼容端点",
    },
    ProviderPreset {
        id: "deepseek_anthropic",
        name: "DeepSeek (Anthropic 入口)",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.deepseek.com/anthropic",
        models: &["deepseek-v4-pro", "deepseek-v4-flash", "deepseek-chat"],
        default_model: Some("deepseek-v4-pro"),
        website: "https://platform.deepseek.com/",
        note: "DeepSeek 提供的 Anthropic 兼容端点",
    },
    ProviderPreset {
        id: "packycode_anthropic",
        name: "PackyCode",
        kind: ProviderKind::Anthropic,
        base_url: "https://www.packyapi.com",
        models: &["claude-sonnet-4-5", "claude-opus-4-5"],
        default_model: Some("claude-sonnet-4-5"),
        website: "https://www.packycode.com/",
        note: "第三方 Claude 代理",
    },
    ProviderPreset {
        id: "gemini",
        name: "Google Gemini",
        kind: ProviderKind::Gemini,
        base_url: "https://generativelanguage.googleapis.com",
        models: &[
            "gemini-2.0-flash",
            "gemini-2.0-flash-thinking-exp",
            "gemini-1.5-pro",
            "gemini-1.5-flash",
        ],
        default_model: Some("gemini-2.0-flash"),
        website: "https://aistudio.google.com/apikey",
        note: "Google AI Studio API Key",
    },
];

pub fn list_presets() -> Vec<ProviderPreset> {
    PRESETS.to_vec()
}
