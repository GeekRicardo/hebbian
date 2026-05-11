use crate::error::AppResult;
use model_gateway::config::Provider;
use agent_core::storage::sessions::{Message, Role};

const SYSTEM: &str = "你是一个严格的标题生成器。阅读给定对话，用不超过 16 个汉字（或 8 个英文单词）总结出一个简短、具体、没有标点和引号的标题，直接输出标题本身，不要任何前后缀。";

/// 中文场景 10 个字，英文场景 15 个字（英文窄、信息密度低）。
const FALLBACK_LIMIT_CJK: usize = 10;
const FALLBACK_LIMIT_LATIN: usize = 15;

pub async fn generate(provider: &Provider, model: &str, messages: &[Message]) -> AppResult<String> {
    let convo: Vec<Message> = messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .take(8)
        .cloned()
        .collect();

    if convo.is_empty() {
        return Ok("新对话".to_string());
    }

    let mut bundle = String::from("请为以下对话生成标题：\n\n");
    for m in &convo {
        let role = match m.role {
            Role::User => "用户",
            Role::Assistant => "助手",
            _ => continue,
        };
        bundle.push_str(&format!("[{}] ", role));
        let snippet: String = m.content.chars().take(200).collect();
        bundle.push_str(&snippet);
        if m.content.len() > 200 {
            bundle.push_str("…");
        }
        bundle.push('\n');
    }

    crate::chat::send_once(
        provider,
        model,
        Some(SYSTEM),
        &[Message {
            id: String::new(),
            role: Role::User,
            content: bundle,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
        }],
    )
    .await
}

/// 模型生成失败时的兜底：截取第一条用户消息开头若干字符。
/// 含中日韩等宽字符 → 取 10 个字；纯英文 / ASCII → 取 15 个。
pub fn fallback_from_first_user(messages: &[Message]) -> String {
    let first_user = messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .map(|m| m.content.trim())
        .unwrap_or("");

    if first_user.is_empty() {
        return "新对话".to_string();
    }

    let limit = if first_user.chars().take(20).any(is_wide_char) {
        FALLBACK_LIMIT_CJK
    } else {
        FALLBACK_LIMIT_LATIN
    };

    let mut chars = first_user.chars();
    let head: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 简单粗暴：UTF-8 长度 ≥ 3 字节的字符基本都是 CJK / 日文 / 韩文 / 全角符号。
fn is_wide_char(c: char) -> bool {
    c.len_utf8() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str) -> Message {
        Message {
            id: String::new(),
            role: Role::User,
            content: content.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
        }
    }

    #[test]
    fn fallback_chinese_takes_10_chars() {
        let msgs = [user_msg("帮我写一个用 Rust 实现的简单 HTTP 服务器")];
        // 10 个字符（含空格与 ASCII 字母）+ 省略号
        assert_eq!(fallback_from_first_user(&msgs), "帮我写一个用 Rus…");
    }

    #[test]
    fn fallback_english_takes_15_chars() {
        let msgs = [user_msg("write a simple http server in rust")];
        assert_eq!(fallback_from_first_user(&msgs), "write a simple …");
    }

    #[test]
    fn fallback_short_input_no_ellipsis() {
        let msgs = [user_msg("你好")];
        assert_eq!(fallback_from_first_user(&msgs), "你好");
    }

    #[test]
    fn fallback_empty_returns_default() {
        assert_eq!(fallback_from_first_user(&[]), "新对话");
        assert_eq!(fallback_from_first_user(&[user_msg("   ")]), "新对话");
    }
}
