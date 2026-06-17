//! Session 标题自动生成（utility 短调用）。
//!
//! 用一次「无工具、关思考、短输出」的 LLM 调用，把用户首条消息提炼成一个对话标题。
//! 不进 agent loop，也不进 transcript——纯辅助。
//!
//! 关键点：
//! - `ReasoningConfig.enabled = Some(false)`。对 DeepSeek thinking 模型，这会让
//!   `model-gateway::protocols::openai::apply_deepseek_compat` 走「thinking: disabled」
//!   分支，避免短输出耗在推理上 / 触发 thinking 最小 32K 预算的硬下限。
//! - `max_tokens` 给到 128 就够。
//!
//! **触发时机**：Harness::spawn_run 在每个 Run 的首个 `TurnFinished` 事件后异步
//! spawn 一个独立 task 调 [`generate_for_session`]，与主 run 完全解耦——失败不
//! 影响主流程，事件流通过 [`EventPayload::SessionTitleChanged`] 通知 surface。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::{AppResult, CancelFlag, ReasoningConfig};
use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};

use crate::storage::sessions::{self, Message, Role, Session};

const TITLE_INSTRUCTION: &str =
    "为以下用户消息生成一个 4-12 字的中文对话主题作为标题，只输出标题本身，\
     不要引号、标点、解释或前后缀。\n\n用户消息：\n";

const TITLE_MAX_TOKENS: u32 = 128;
const TITLE_MAX_CHARS: usize = 32;
const TITLE_TRIM_CHARS: &[char] = &[
    ' ', '\t', '\n', '"', '\'', '`', '「', '」', '“', '”', '【', '】', '(', ')', '（', '）',
];

/// 模型短调用失败时的兜底：截 session 首条 user message 开头若干字符。
/// CJK / 全角字符 10 个，纯英文 15 个；超出加 `…` 后缀；session 没有 user 消息时回到 `DEFAULT_TITLE`。
const FALLBACK_LIMIT_CJK: usize = 10;
const FALLBACK_LIMIT_LATIN: usize = 15;

/// 用「关思考」的 utility 短调用让模型从用户首条消息提炼对话标题。
/// 调用方负责把返回值写入 `Session.title`（建议先 trim 检查非空再写）。
pub async fn generate_title(
    client: &dyn ModelClient,
    model: &str,
    user_message: &str,
) -> Result<String, ModelError> {
    let trimmed_user = user_message.trim();
    if trimmed_user.is_empty() {
        return Ok(String::new());
    }
    let prompt = format!("{TITLE_INSTRUCTION}{trimmed_user}");
    let req = ModelRequest {
        model: model.into(),
        system: None,
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: vec![],
        max_tokens: TITLE_MAX_TOKENS,
        reasoning: Some(ReasoningConfig {
            enabled: Some(false),
            effort: None,
            long_context: None,
        }),
    };
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let raw_text = match client.complete(req, cancel).await? {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text,
    };
    Ok(sanitize_title(&raw_text))
}

/// 根据 session 选 provider + 调模型 → `Some(title)` 或 `None`。
///
/// 不读 `session.title`、不写 jsonl——纯计算。选 provider 的策略：
/// `providers.json` 里 `title_gen_enabled=true` 的优先，否则回退到 session 自己的 provider/model。
/// 任何环节失败（OAuth 刷新失败、模型调用失败、返回空标题）都返回 `None`，并各打一条带
/// `session_id` 的 `tracing::warn` —— 这条短调用全程 detached、不进 transcript，没有日志就
/// 完全无从诊断「标题为什么没生成」。日志可用 `grep <session_id> ~/.hebbian/logs/` 定位失败阶段。
async fn try_generate_for_session(
    data_dir: &Path,
    session_id: &str,
    session: &Session,
) -> TitleOutcome {
    let first_user = session
        .messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .map(|m| m.content.trim().to_string())
        .unwrap_or_default();
    if first_user.is_empty() {
        tracing::warn!(session_id, "标题生成跳过：session 无 user 消息");
        return TitleOutcome::Skipped;
    }

    let providers_file = match model_gateway::config::load(data_dir) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(session_id, error = %e, "标题生成失败：读取 providers 配置出错");
            return TitleOutcome::Failed(format!("读取模型配置失败：{e}"));
        }
    };
    let picked = providers_file
        .providers
        .into_iter()
        .find(|p| {
            p.enabled
                && p.title_gen_enabled
                && p.title_gen_model.as_deref().is_some_and(|m| !m.is_empty())
        })
        .map(|p| {
            let m = p.title_gen_model.clone().unwrap_or_default();
            (p, m)
        });
    let (provider, model) = match picked {
        Some(pm) => pm,
        None => match model_gateway::config::get(data_dir, &session.provider_id) {
            Ok(p) => (p, session.model.clone()),
            Err(e) => {
                tracing::warn!(
                    session_id,
                    provider_id = %session.provider_id,
                    error = %e,
                    "标题生成失败：无专用标题 provider，回退 session provider 也取不到"
                );
                return TitleOutcome::Failed(format!("找不到可用的模型供应商：{e}"));
            }
        },
    };

    let provider =
        match model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(session_id, error = %e, "标题生成失败：刷新 provider token 出错");
                return TitleOutcome::Failed(format!("刷新登录状态失败：{e}"));
            }
        };
    let client = match model_gateway::build_client(provider) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(session_id, error = %e, "标题生成失败：构建 model client 出错");
            return TitleOutcome::Failed(format!("初始化模型客户端失败：{e}"));
        }
    };

    let title = match generate_title(client.as_ref(), &model, &first_user).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(session_id, model = %model, error = %e, "标题生成失败：模型调用出错");
            return TitleOutcome::Failed(format!("调用模型 {model} 失败：{e}"));
        }
    };
    if title.is_empty() {
        tracing::warn!(session_id, model = %model, "标题生成失败：模型返回空标题");
        TitleOutcome::Failed(format!("模型 {model} 返回了空标题"))
    } else {
        tracing::info!(session_id, model = %model, title = %title, "标题生成成功");
        TitleOutcome::Generated(title)
    }
}

/// 自动标题生成的结果三态。harness 据此决定：成功 emit `SessionTitleChanged`、
/// 失败 emit `SessionTitleGenerationFailed`（surface 弹 toast）、跳过则什么都不做。
/// 区分「失败」与「跳过」是为了避免切回老对话（title 已改）误弹 toast。
pub enum TitleOutcome {
    /// 成功生成并已落盘。
    Generated(String),
    /// 正常跳过：title 已非默认值（用户重命名 / fork / resume）或 session 无 user 消息。
    Skipped,
    /// 真失败：选 provider / 刷 token / 建 client / 模型调用 / 落盘任一环节出错。
    /// 携带一句给用户看的简短原因。
    Failed(String),
}

/// 自动入口（Harness::spawn_run 首轮挂钩调用）：
/// 仅当当前 `session.title == DEFAULT_TITLE` 时才执行模型短调用 + rename 落盘。
pub async fn generate_for_session(data_dir: &Path, session_id: &str) -> TitleOutcome {
    let session = match sessions::load(data_dir, session_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(session_id, error = %e, "标题生成失败：加载 session 出错");
            return TitleOutcome::Failed(format!("加载会话失败：{e}"));
        }
    };
    if session.title != sessions::DEFAULT_TITLE {
        tracing::debug!(
            session_id,
            title = %session.title,
            "标题生成跳过：title 已非默认值（用户已重命名 / fork / resume）"
        );
        return TitleOutcome::Skipped;
    }
    let title = match try_generate_for_session(data_dir, session_id, &session).await {
        TitleOutcome::Generated(t) => t,
        other => return other,
    };
    if let Err(e) = sessions::rename(data_dir, session_id, title.clone()) {
        tracing::warn!(session_id, error = %e, "标题生成失败：rename 落盘出错");
        return TitleOutcome::Failed(format!("保存标题失败：{e}"));
    }
    TitleOutcome::Generated(title)
}

/// 手动重生成（surface 「重新生成标题」入口）：
/// 无视当前 title；模型失败时 fallback 截首条 user message；总是 rename 落盘 + 返回新 Session。
/// 唯一可能的失败：session 文件本身 load/rename 失败。
pub async fn regenerate_session_title(data_dir: &Path, session_id: &str) -> AppResult<Session> {
    let session = sessions::load(data_dir, session_id)?;
    let title = match try_generate_for_session(data_dir, session_id, &session).await {
        TitleOutcome::Generated(t) => t,
        TitleOutcome::Skipped | TitleOutcome::Failed(_) => {
            fallback_from_messages(&session.messages)
        }
    };
    sessions::rename(data_dir, session_id, title)
}

/// 模型调用失败时的兜底标题：截 session 首条 user message 开头若干字符。
/// 永远返回非空字符串——session 没有 user 消息时回到 `DEFAULT_TITLE`。
pub fn fallback_from_messages(messages: &[Message]) -> String {
    let first_user = messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .map(|m| m.content.trim())
        .unwrap_or("");
    if first_user.is_empty() {
        return sessions::DEFAULT_TITLE.to_string();
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

fn is_wide_char(c: char) -> bool {
    c.len_utf8() >= 3
}

fn sanitize_title(raw: &str) -> String {
    let first_line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let cleaned = first_line.trim_matches(|c: char| TITLE_TRIM_CHARS.contains(&c));
    if cleaned.is_empty() {
        return String::new();
    }
    if cleaned.chars().count() > TITLE_MAX_CHARS {
        cleaned.chars().take(TITLE_MAX_CHARS).collect()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use model_gateway::types::{ModelStreamEvent, Usage};

    struct StaticClient {
        last_req: std::sync::Mutex<Option<ModelRequest>>,
        reply: String,
    }

    #[async_trait]
    impl ModelClient for StaticClient {
        fn provider_id(&self) -> &str {
            "static"
        }
        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            *self.last_req.lock().unwrap() = Some(req);
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: self.reply.clone(),
                reasoning: String::new(),
                attachments: vec![],
                usage: Usage::default(),
                reasoning_signature: String::new(),
            })
        }
        async fn stream(
            &self,
            req: ModelRequest,
            cancel: CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            self.complete(req, cancel).await
        }
    }

    #[tokio::test]
    async fn passes_reasoning_disabled_to_client() {
        let client = StaticClient {
            last_req: std::sync::Mutex::new(None),
            reply: "天气查询".into(),
        };
        let title = generate_title(&client, "deepseek-v4-pro", "北京今天热不热")
            .await
            .unwrap();
        assert_eq!(title, "天气查询");
        let req = client.last_req.lock().unwrap().clone().unwrap();
        let cfg = req.reasoning.expect("reasoning should be Some");
        assert_eq!(cfg.enabled, Some(false));
        assert!(req.tools.is_empty());
        assert_eq!(req.max_tokens, TITLE_MAX_TOKENS);
    }

    #[tokio::test]
    async fn empty_user_message_returns_empty_without_call() {
        let client = StaticClient {
            last_req: std::sync::Mutex::new(None),
            reply: "不应被使用".into(),
        };
        let title = generate_title(&client, "deepseek-v4-pro", "   ")
            .await
            .unwrap();
        assert_eq!(title, "");
        assert!(client.last_req.lock().unwrap().is_none());
    }

    #[test]
    fn sanitize_strips_quotes_and_caps_length() {
        assert_eq!(sanitize_title("「天气查询」"), "天气查询");
        assert_eq!(sanitize_title("\"hello world\""), "hello world");
        assert_eq!(sanitize_title("  多余空格  "), "多余空格");
        // 多行：取第一非空行
        assert_eq!(
            sanitize_title("\n\n第一行标题\n后续解释不应保留"),
            "第一行标题"
        );
        // 超长截断
        let long: String = std::iter::repeat('字').take(40).collect();
        let cut = sanitize_title(&long);
        assert_eq!(cut.chars().count(), 32);
    }
}
