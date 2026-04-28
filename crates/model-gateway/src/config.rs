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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
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
        website: "https://platform.openai.com/api-keys",
        note: "官方 OpenAI 接口",
    },
    ProviderPreset {
        id: "deepseek",
        name: "DeepSeek",
        kind: ProviderKind::Openai,
        base_url: "https://api.deepseek.com/v1",
        models: &["deepseek-chat", "deepseek-reasoner"],
        website: "https://platform.deepseek.com/",
        note: "深度求索，OpenAI 兼容",
    },
    ProviderPreset {
        id: "zhipu_glm",
        name: "智谱 GLM",
        kind: ProviderKind::Openai,
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &["glm-4.6", "glm-4-plus", "glm-4-air", "glm-4-flash"],
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
        website: "https://platform.moonshot.cn/",
        note: "月之暗面 Kimi，OpenAI 兼容",
    },
    ProviderPreset {
        id: "qwen",
        name: "阿里百炼 (Qwen)",
        kind: ProviderKind::Openai,
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        models: &["qwen-max", "qwen-plus", "qwen-turbo", "qwen3-coder-plus"],
        website: "https://bailian.console.aliyun.com/",
        note: "通义千问，OpenAI 兼容端点",
    },
    ProviderPreset {
        id: "doubao",
        name: "豆包 (火山方舟)",
        kind: ProviderKind::Openai,
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        models: &["doubao-seed-1-6", "doubao-pro-256k", "doubao-1-5-pro-32k"],
        website: "https://console.volcengine.com/ark",
        note: "字节跳动豆包，OpenAI 兼容",
    },
    ProviderPreset {
        id: "siliconflow",
        name: "SiliconFlow",
        kind: ProviderKind::Openai,
        base_url: "https://api.siliconflow.cn/v1",
        models: &["Qwen/Qwen2.5-72B-Instruct", "deepseek-ai/DeepSeek-V3"],
        website: "https://cloud.siliconflow.cn/",
        note: "硅基流动，多模型聚合",
    },
    ProviderPreset {
        id: "minimax",
        name: "MiniMax",
        kind: ProviderKind::Openai,
        base_url: "https://api.minimaxi.com/v1",
        models: &["MiniMax-M1", "abab6.5s-chat"],
        website: "https://www.minimaxi.com/",
        note: "MiniMax，OpenAI 兼容",
    },
    ProviderPreset {
        id: "stepfun",
        name: "StepFun (阶跃)",
        kind: ProviderKind::Openai,
        base_url: "https://api.stepfun.com/v1",
        models: &["step-2-16k", "step-1-flash"],
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
        website: "https://console.anthropic.com/",
        note: "官方 Anthropic 接口",
    },
    ProviderPreset {
        id: "zhipu_glm_anthropic",
        name: "智谱 GLM (Anthropic 入口)",
        kind: ProviderKind::Anthropic,
        base_url: "https://open.bigmodel.cn/api/anthropic",
        models: &["glm-4.6", "glm-4-plus"],
        website: "https://open.bigmodel.cn/",
        note: "智谱提供的 Anthropic 兼容端点",
    },
    ProviderPreset {
        id: "kimi_anthropic",
        name: "Kimi (Anthropic 入口)",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.moonshot.cn/anthropic",
        models: &["kimi-k2-0711-preview"],
        website: "https://platform.moonshot.cn/",
        note: "Moonshot 提供的 Anthropic 兼容端点",
    },
    ProviderPreset {
        id: "deepseek_anthropic",
        name: "DeepSeek (Anthropic 入口)",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.deepseek.com/anthropic",
        models: &["deepseek-chat"],
        website: "https://platform.deepseek.com/",
        note: "DeepSeek 提供的 Anthropic 兼容端点",
    },
    ProviderPreset {
        id: "packycode_anthropic",
        name: "PackyCode",
        kind: ProviderKind::Anthropic,
        base_url: "https://www.packyapi.com",
        models: &["claude-sonnet-4-5", "claude-opus-4-5"],
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
        website: "https://aistudio.google.com/apikey",
        note: "Google AI Studio API Key",
    },
];

pub fn list_presets() -> Vec<ProviderPreset> {
    PRESETS.to_vec()
}
