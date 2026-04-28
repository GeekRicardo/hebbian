// engine/context.rs
// 消息格式转换和 HTTP 工具函数。
// 把原来散落在 chat.rs 里的纯函数集中到这里，供引擎和其他模块复用。
//
// 三种 AI API 格式的消息结构：
//   OpenAI:    messages: [{ role: "system"|"user"|"assistant", content: "..." }]
//   Anthropic: messages: [{ role: "user"|"assistant", content: "..." }]（system 单独传）
//   Gemini:    contents: [{ role: "user"|"model", parts: [{ text: "..." }] }]

use crate::providers::{AuthMode, Provider, ProviderKind};
use crate::sessions::{Message, Role};
use reqwest::RequestBuilder;
use serde_json::{json, Value};

use super::types::Tool;

/// 创建一个带 User-Agent 的复用 HTTP 客户端
pub fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Hebbian/0.1")
        .build()
}

/// 过滤 Marker 角色的消息（供应商切换分隔符），只保留真正参与 AI 请求的消息
pub fn effective_messages(msgs: &[Message]) -> Vec<&Message> {
    msgs.iter()
        .filter(|m| !matches!(m.role, Role::Marker))
        .collect()
}

/// 将会话消息列表转换为 OpenAI messages 格式
///
/// system 提示词以第一条 {"role":"system"} 消息注入；
/// User/Assistant 消息直接映射；Marker 被过滤掉。
pub fn to_openai_messages(system: Option<&str>, msgs: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    // OpenAI 支持把 system 放进 messages 数组的第一位
    if let Some(s) = system {
        if !s.trim().is_empty() {
            out.push(json!({ "role": "system", "content": s }));
        }
    }
    for m in effective_messages(msgs) {
        let role = match m.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Marker => continue, // 过滤切换标记
        };
        out.push(json!({ "role": role, "content": m.content }));
    }
    out
}

/// 将会话消息列表转换为 Anthropic messages 格式
///
/// Anthropic 的 messages 只包含 user/assistant 轮次，
/// system 提示词通过请求体顶层的 "system" 字段单独传递。
pub fn to_anthropic_messages(msgs: &[Message]) -> Vec<Value> {
    effective_messages(msgs)
        .into_iter()
        .filter_map(|m| match m.role {
            Role::User => Some(json!({ "role": "user", "content": m.content })),
            Role::Assistant => Some(json!({ "role": "assistant", "content": m.content })),
            _ => None,
        })
        .collect()
}

/// 将会话消息列表转换为 Gemini contents 格式
///
/// Gemini 用 "user"/"model" 区分角色，内容放在 parts 数组里。
pub fn to_gemini_contents(msgs: &[Message]) -> Vec<Value> {
    effective_messages(msgs)
        .into_iter()
        .filter_map(|m| match m.role {
            Role::User => Some(json!({
                "role": "user",
                "parts": [{ "text": m.content }]
            })),
            Role::Assistant => Some(json!({
                "role": "model",
                "parts": [{ "text": m.content }]
            })),
            _ => None,
        })
        .collect()
}

/// 根据供应商类型和认证模式给 HTTP 请求添加认证头
///
/// 不同供应商的认证方式：
/// - OpenAI/Codex OAuth: Bearer token + ChatGPT-Account-Id
/// - OpenAI API Key: Bearer token
/// - Anthropic Claude Code OAuth: Bearer + anthropic-beta header
/// - Anthropic API Key: x-api-key header
/// - Gemini OAuth: Bearer + x-goog-api-client
/// - Gemini API Key: 通过 URL query param ?key= 传递（不在这里处理）
pub fn apply_auth(req: RequestBuilder, provider: &Provider) -> RequestBuilder {
    let mut req = req;
    match (provider.kind, provider.auth_mode) {
        (ProviderKind::Openai, AuthMode::OauthCodex) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("originator", "Hebbian");
            if let Some(acc) = &provider.account_id {
                req = req.header("ChatGPT-Account-Id", acc.as_str());
            }
        }
        (ProviderKind::Openai, _) => {
            req = req.bearer_auth(&provider.api_key);
        }
        (ProviderKind::Anthropic, AuthMode::OauthClaudeCode) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "oauth-2025-04-20");
        }
        (ProviderKind::Anthropic, _) => {
            req = req
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", "2023-06-01");
        }
        (ProviderKind::Gemini, AuthMode::OauthGeminiCli) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("x-goog-api-client", "GeminiCLI/1.0");
        }
        (ProviderKind::Gemini, _) => {
            // Gemini API Key 在调用处通过 URL 拼接 ?key=xxx，此处不处理
        }
    }
    // 追加用户在供应商配置里填写的自定义 header
    for (k, v) in &provider.extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req
}

// ========== 工具 Schema 转换 ==========
// 三种 API 格式对"工具定义"的结构要求不同，以下函数做格式适配。

/// 将工具列表转换为 OpenAI function calling 格式
///
/// 格式: [{"type":"function","function":{"name":...,"description":...,"parameters":{...}}}]
pub fn to_openai_tools(tools: &[&dyn Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters_schema(),
                }
            })
        })
        .collect()
}

/// 将工具列表转换为 Anthropic tool use 格式
///
/// 格式: [{"name":...,"description":...,"input_schema":{...}}]
pub fn to_anthropic_tools(tools: &[&dyn Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name(),
                "description": t.description(),
                "input_schema": t.parameters_schema(),
            })
        })
        .collect()
}

/// 将工具列表转换为 Gemini function calling 格式
///
/// 格式: [{"functionDeclarations":[{"name":...,"description":...,"parameters":{...}}]}]
pub fn to_gemini_tools(tools: &[&dyn Tool]) -> Value {
    let declarations: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name(),
                "description": t.description(),
                "parameters": t.parameters_schema(),
            })
        })
        .collect();
    json!([{ "functionDeclarations": declarations }])
}
