//! 视觉辅助桥接（Vision Bridge）。
//!
//! 当目标模型不支持图片输入时，用一个配置好的「视觉辅助模型」先看图，
//! 把图片转成结构化文字描述，替换掉原始 Image 附件后再发给目标模型。
//!
//! 实现为 `ModelClient` 装饰器：包装 inner client，在 `complete` / `stream`
//! 前拦截 transcript 里的 Image 附件做转换。目标模型原生支持图片时直接透传。

use std::sync::Arc;

use async_trait::async_trait;
use common::attachments::MessageAttachment;
use common::CancelFlag;
use model_gateway::client::ModelClient;
use model_gateway::types::{
    ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ToolResult, TranscriptEntry,
    UserEntry,
};

const VISION_ANALYSIS_MAX_TOKENS: u32 = 900;

/// 视觉分析的 system prompt。指导视觉模型带着用户的问题语境去看图，
/// 输出结构化但紧凑的文字描述，供文本模型消费。
const VISION_SYSTEM_PROMPT: &str = "\
Analyze this image for another text-only AI model that cannot see images.
Return a concise note with these exact sections:
image_overview: what the image shows.
visible_text: important OCR or readable text.
objects_and_layout: key objects, positions, counts, relationships.
charts_or_data: chart/table/data details if present; otherwise say none.
user_request: restate the user's request in one short sentence.
user_request_answer: answer the user's request using the image when possible.
evidence: visual evidence supporting that answer.
uncertainty: anything unclear, hidden, or guessed.
Do not mention that you are a tool or a separate model.";

/// `VisionBridgeClient` 包装一个目标模型 client。
/// 如果请求里有图片附件，先用 `vision_client` + `vision_model` 把图片转成文字描述。
pub struct VisionBridgeClient {
    /// 目标模型 client（文本模型，可能不支持图片）。
    inner: Arc<dyn ModelClient>,
    /// 视觉辅助模型 client（必须支持图片输入）。
    vision_client: Arc<dyn ModelClient>,
    /// 视觉辅助模型 id。
    vision_model: String,
}

impl VisionBridgeClient {
    pub fn new(
        inner: Arc<dyn ModelClient>,
        vision_client: Arc<dyn ModelClient>,
        vision_model: String,
    ) -> Self {
        Self {
            inner,
            vision_client,
            vision_model,
        }
    }

    /// 扫描 entries 里所有 UserEntry 的 Image 附件，用视觉模型转成文字描述。
    async fn adapt_request(
        &self,
        mut req: ModelRequest,
        cancel: &CancelFlag,
    ) -> Result<ModelRequest, ModelError> {
        // 从后往前找最近的用户文字，作为视觉分析的情景上下文。
        let user_context = req
            .entries
            .iter()
            .rev()
            .find_map(|e| match e {
                TranscriptEntry::User(u) if !u.text.trim().is_empty() => Some(u.text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let mut adapted_entries = Vec::with_capacity(req.entries.len());
        for entry in req.entries {
            match entry {
                TranscriptEntry::User(user) if has_image_attachments(&user.attachments) => {
                    let context = if user.text.trim().is_empty() {
                        user_context.clone()
                    } else {
                        user.text.clone()
                    };
                    let adapted = self.adapt_user_entry(user, &context, cancel).await?;
                    adapted_entries.push(TranscriptEntry::User(adapted));
                }
                TranscriptEntry::ToolResults(results)
                    if results
                        .iter()
                        .any(|r| has_image_attachments(&r.attachments)) =>
                {
                    let adapted = self
                        .adapt_tool_results(results, &user_context, cancel)
                        .await?;
                    adapted_entries.push(TranscriptEntry::ToolResults(adapted));
                }
                other => adapted_entries.push(other),
            }
        }
        req.entries = adapted_entries;
        Ok(req)
    }

    /// 把工具结果里的图片附件逐个替换为视觉模型生成的文字描述，注入回 `content`。
    /// 弱文本模型据此「看见」工具读到的图片（架构 §4.4.1 / §4.11）。
    async fn adapt_tool_results(
        &self,
        results: Vec<ToolResult>,
        user_context: &str,
        cancel: &CancelFlag,
    ) -> Result<Vec<ToolResult>, ModelError> {
        let mut adapted = Vec::with_capacity(results.len());
        for mut result in results {
            if !has_image_attachments(&result.attachments) {
                adapted.push(result);
                continue;
            }
            let mut vision_notes = Vec::new();
            for attachment in &result.attachments {
                if let MessageAttachment::Image {
                    name,
                    media_type,
                    data,
                } = attachment
                {
                    let note = self
                        .analyze_image(media_type, data, user_context, cancel)
                        .await?;
                    vision_notes.push(format!("[图片 {name}]\n{note}"));
                }
            }
            if !vision_notes.is_empty() {
                let block = vision_notes.join("\n\n");
                result.content = format!(
                    "{}\n<vision-context>\n{block}\n</vision-context>",
                    result.content
                );
            }
            result.attachments.clear();
            adapted.push(result);
        }
        Ok(adapted)
    }

    /// 把一条 UserEntry 里的 Image 附件逐个替换为视觉模型生成的文字描述。
    async fn adapt_user_entry(
        &self,
        user: UserEntry,
        user_context: &str,
        cancel: &CancelFlag,
    ) -> Result<UserEntry, ModelError> {
        let mut new_attachments = Vec::with_capacity(user.attachments.len());
        let mut vision_notes = Vec::new();

        for attachment in user.attachments {
            match &attachment {
                MessageAttachment::Image {
                    name,
                    media_type,
                    data,
                } => {
                    let note = self
                        .analyze_image(media_type, data, user_context, cancel)
                        .await?;
                    vision_notes.push(format!("[图片 {name}]\n{note}"));
                }
                MessageAttachment::TextFile { .. } => {
                    new_attachments.push(attachment);
                }
            }
        }

        // 把视觉描述注入到用户文本前面，用 XML 标签包裹以便模型区分。
        let text = if vision_notes.is_empty() {
            user.text
        } else {
            let block = vision_notes.join("\n\n");
            format!(
                "<vision-context>\n{block}\n</vision-context>\n\n{}",
                user.text
            )
        };

        Ok(UserEntry {
            text,
            attachments: new_attachments,
        })
    }

    /// 用视觉辅助模型分析单张图片。
    async fn analyze_image(
        &self,
        media_type: &str,
        data: &str,
        user_context: &str,
        cancel: &CancelFlag,
    ) -> Result<String, ModelError> {
        let user_prompt = if user_context.is_empty() {
            "(no explicit text request)".to_string()
        } else {
            format!("User request:\n{user_context}")
        };

        let req = ModelRequest {
            model: self.vision_model.clone(),
            system: Some(VISION_SYSTEM_PROMPT.to_string()),
            entries: vec![TranscriptEntry::User(UserEntry {
                text: user_prompt,
                attachments: vec![MessageAttachment::Image {
                    name: "image".to_string(),
                    media_type: media_type.to_string(),
                    data: data.to_string(),
                }],
            })],
            tools: vec![],
            max_tokens: VISION_ANALYSIS_MAX_TOKENS,
            reasoning: Some(common::ReasoningConfig {
                enabled: Some(false),
                effort: None,
                long_context: None,
            }),
            compact_prompt_cache_key: None,
            meta: model_gateway::types::ModelCallMeta {
                tag: model_gateway::types::ModelCallTag::Vision,
                ..Default::default()
            },
        };

        match self.vision_client.complete(req, cancel.clone()).await? {
            ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => Ok(text),
        }
    }
}

fn has_image_attachments(attachments: &[MessageAttachment]) -> bool {
    attachments
        .iter()
        .any(|a| matches!(a, MessageAttachment::Image { .. }))
}

#[async_trait]
impl ModelClient for VisionBridgeClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        let req = self.adapt_request(req, &cancel).await?;
        self.inner.complete(req, cancel).await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let req = self.adapt_request(req, &cancel).await?;
        self.inner.stream(req, cancel, on_event).await
    }
}

/// 预构建的视觉辅助 client + model。在 async 上下文中创建（需要刷新 OAuth token），
/// 然后在同步闭包里通过 [`wrap_with_vision_client`] 包装到主 client 上。
#[derive(Clone)]
pub struct VisionConfig {
    pub client: Arc<dyn ModelClient>,
    pub model: String,
}

/// 从 providers.json 读取视觉辅助配置，构建 vision client（含 OAuth token 刷新）。
/// 未配置时返回 `None`。
pub async fn build_vision_client(
    data_dir: &std::path::Path,
) -> Result<Option<VisionConfig>, ModelError> {
    let providers_file = model_gateway::config::load(data_dir)
        .map_err(|e| ModelError::Other(format!("load providers: {e}")))?;

    let (vision_provider_id, vision_model) = match (
        providers_file.vision_provider_id,
        providers_file.vision_model,
    ) {
        (Some(pid), Some(model)) if !pid.is_empty() && !model.is_empty() => (pid, model),
        _ => return Ok(None),
    };

    let provider = model_gateway::config::get(data_dir, &vision_provider_id)
        .map_err(|e| ModelError::Other(format!("vision provider: {e}")))?;

    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| ModelError::Other(format!("vision provider token refresh: {e}")))?;

    let client = model_gateway::build_client(provider)?;

    Ok(Some(VisionConfig {
        client,
        model: vision_model,
    }))
}

/// 用预构建的 vision config 包装主 client。`None` 时原样返回（零开销）。
pub fn wrap_with_vision_client(
    inner: Arc<dyn ModelClient>,
    vision: Option<VisionConfig>,
) -> Arc<dyn ModelClient> {
    match vision {
        Some(cfg) => Arc::new(VisionBridgeClient::new(inner, cfg.client, cfg.model)),
        None => inner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::types::Usage;
    use std::sync::atomic::AtomicBool;

    /// 记录视觉模型收到的 prompt，用于验证用户情景上下文被正确传递。
    struct MockVisionClient {
        received_prompts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ModelClient for MockVisionClient {
        fn provider_id(&self) -> &str {
            "mock-vision"
        }
        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            // 记录用户 prompt
            if let Some(TranscriptEntry::User(u)) = req.entries.first() {
                self.received_prompts.lock().unwrap().push(u.text.clone());
            }
            Ok(ModelResponse::Done {
                text: "image_overview: a screenshot showing an error dialog".to_string(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
                attachments: vec![],
                usage: Usage::default(),
                finish: model_gateway::types::FinishReason::Stop,
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

    struct MockInnerClient {
        last_req: std::sync::Mutex<Option<ModelRequest>>,
    }

    #[async_trait]
    impl ModelClient for MockInnerClient {
        fn provider_id(&self) -> &str {
            "mock-inner"
        }
        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            *self.last_req.lock().unwrap() = Some(req);
            Ok(ModelResponse::Done {
                text: "done".to_string(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
                attachments: vec![],
                usage: Usage::default(),
                finish: model_gateway::types::FinishReason::Stop,
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
    async fn image_attachments_replaced_with_vision_notes() {
        let vision = Arc::new(MockVisionClient {
            received_prompts: std::sync::Mutex::new(Vec::new()),
        });
        let inner = Arc::new(MockInnerClient {
            last_req: std::sync::Mutex::new(None),
        });
        let bridge = VisionBridgeClient::new(inner.clone(), vision.clone(), "gpt-4o".to_string());

        let req = ModelRequest {
            model: String::new(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry {
                text: "这个截图里报了什么错".to_string(),
                attachments: vec![MessageAttachment::Image {
                    name: "screenshot.png".to_string(),
                    media_type: "image/png".to_string(),
                    data: "base64data".to_string(),
                }],
            })],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        bridge.complete(req, cancel).await.unwrap();

        // 验证视觉模型收到了用户上下文
        let prompts = vision.received_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("这个截图里报了什么错"));

        // 验证发给目标模型的请求已经没有图片附件
        let inner_req = inner.last_req.lock().unwrap();
        let inner_req = inner_req.as_ref().unwrap();
        if let TranscriptEntry::User(u) = &inner_req.entries[0] {
            assert!(
                !has_image_attachments(&u.attachments),
                "image attachments should have been removed"
            );
            assert!(
                u.text.contains("<vision-context>"),
                "vision notes should be injected into text"
            );
        } else {
            panic!("expected User entry");
        }
    }

    #[tokio::test]
    async fn no_image_attachments_passes_through() {
        let vision = Arc::new(MockVisionClient {
            received_prompts: std::sync::Mutex::new(Vec::new()),
        });
        let inner = Arc::new(MockInnerClient {
            last_req: std::sync::Mutex::new(None),
        });
        let bridge = VisionBridgeClient::new(inner.clone(), vision.clone(), "gpt-4o".to_string());

        let req = ModelRequest {
            model: String::new(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry {
                text: "hello".to_string(),
                attachments: vec![],
            })],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        bridge.complete(req, cancel).await.unwrap();

        // 视觉模型不应被调用
        assert!(vision.received_prompts.lock().unwrap().is_empty());

        // 内容应原样透传
        let inner_req = inner.last_req.lock().unwrap();
        let inner_req = inner_req.as_ref().unwrap();
        if let TranscriptEntry::User(u) = &inner_req.entries[0] {
            assert_eq!(u.text, "hello");
        }
    }

    #[tokio::test]
    async fn user_context_from_earlier_message_when_current_is_empty() {
        let vision = Arc::new(MockVisionClient {
            received_prompts: std::sync::Mutex::new(Vec::new()),
        });
        let inner = Arc::new(MockInnerClient {
            last_req: std::sync::Mutex::new(None),
        });
        let bridge = VisionBridgeClient::new(inner.clone(), vision.clone(), "gpt-4o".to_string());

        let req = ModelRequest {
            model: String::new(),
            system: None,
            entries: vec![
                TranscriptEntry::User(UserEntry {
                    text: "帮我看看这个界面的布局问题".to_string(),
                    attachments: vec![],
                }),
                // 后续跟了一条纯图片消息（工具截图等场景）
                TranscriptEntry::User(UserEntry {
                    text: String::new(),
                    attachments: vec![MessageAttachment::Image {
                        name: "ui.png".to_string(),
                        media_type: "image/png".to_string(),
                        data: "base64".to_string(),
                    }],
                }),
            ],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        bridge.complete(req, cancel).await.unwrap();

        // 视觉模型应拿到前一条消息的上下文
        let prompts = vision.received_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("帮我看看这个界面的布局问题"));
    }

    #[tokio::test]
    async fn tool_result_image_replaced_with_vision_notes() {
        let vision = Arc::new(MockVisionClient {
            received_prompts: std::sync::Mutex::new(Vec::new()),
        });
        let inner = Arc::new(MockInnerClient {
            last_req: std::sync::Mutex::new(None),
        });
        let bridge = VisionBridgeClient::new(inner.clone(), vision.clone(), "gpt-4o".to_string());

        // 工具读到一张图片：弱文本模型必须经 VisionBridge 转文字才能「看见」。
        let req = ModelRequest {
            model: String::new(),
            system: None,
            entries: vec![
                TranscriptEntry::User(UserEntry::text("看看这张图")),
                TranscriptEntry::ToolResults(vec![ToolResult {
                    call_id: "call_1".to_string(),
                    name: "Read".to_string(),
                    content: "已读取图片 a.png".to_string(),
                    artifact: None,
                    attachments: vec![MessageAttachment::Image {
                        name: "a.png".to_string(),
                        media_type: "image/png".to_string(),
                        data: "base64".to_string(),
                    }],
                }]),
            ],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
            compact_prompt_cache_key: None,
            meta: Default::default(),
        };

        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        bridge.complete(req, cancel).await.unwrap();

        let inner_req = inner.last_req.lock().unwrap();
        let inner_req = inner_req.as_ref().unwrap();
        match &inner_req.entries[1] {
            TranscriptEntry::ToolResults(results) => {
                // 图片附件被删除、转成 content 里的 vision-context 文字。
                assert!(results[0].attachments.is_empty(), "图片附件应被移除");
                assert!(
                    results[0].content.contains("<vision-context>"),
                    "应注入视觉描述，实际: {}",
                    results[0].content
                );
            }
            other => panic!("expected ToolResults, got {other:?}"),
        }
    }
}
