use crate::error::AppResult;
use model_gateway::config::Provider;
use platform::storage::sessions::{Message, Role};

const SYSTEM: &str = "你是一个严格的标题生成器。阅读给定对话，用不超过 16 个汉字（或 8 个英文单词）总结出一个简短、具体、没有标点和引号的标题，直接输出标题本身，不要任何前后缀。";

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
