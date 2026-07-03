use std::sync::{Arc, Mutex};

use crate::context::budget;
use crate::definition::CompactionPolicy;
use common::CancelFlag;
use model_gateway::client::ModelClient;
use model_gateway::types::{
    AssistantEntry, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, TranscriptEntry,
    UserEntry,
};

/// 默认的中文压缩 prompt。让模型把历史浓缩成接力摘要，下一个 LLM
/// 不读原对话也能继续工作。
pub const COMPACT_PROMPT: &str = "你正在执行【上下文压缩】。请把当前对话历史浓缩成一份简明、结构化的接力摘要，让另一个 LLM 能在不读原对话的情况下无缝继续工作。\n\n请覆盖：\n- 用户的核心目标 / 约束 / 偏好\n- 已完成的关键工作和重要决策（含影响后续判断的细节）\n- 仍未完成的事项 / 下一步\n- 关键数据：文件路径、命令、代码片段、错误信息、外部链接\n- 任何模型不读上下文就会丢的隐含上下文\n\n输出要求：\n- 直接给摘要正文，不要寒暄、不要 “以下是摘要” 之类的引导语\n- 紧凑但不丢关键信息；优先 bullet list\n- 保持中文";

/// 压缩结果：用于 surface 端展示前后对比。
pub struct CompactionResult {
    pub entries: Vec<TranscriptEntry>,
    pub before_tokens: usize,
    pub after_tokens: usize,
    /// LLM 摘要时填入；结构化裁剪时为空。
    pub summary: String,
}

/// 结构化裁剪：保留 system + 最近 keep_recent_turns 轮
pub fn compact_structural(
    system: Option<&str>,
    entries: Vec<TranscriptEntry>,
    policy: &CompactionPolicy,
) -> CompactionResult {
    let before_tokens = budget::estimate_transcript_tokens(system, &entries);

    if before_tokens <= policy.token_budget {
        let after_tokens = before_tokens;
        return CompactionResult {
            entries,
            before_tokens,
            after_tokens,
            summary: String::new(),
        };
    }

    let keep = policy.keep_recent_turns * 3;
    let total = entries.len();
    let raw_start = if total > keep { total - keep } else { 0 };

    // 从 raw_start 向后找第一个 User entry 作为起始点，保证 transcript 以 User 开头。
    // 若 [raw_start..total] 内没有 User（全是 asst/tool_result），则不强制对齐 User，
    // 直接用 raw_start——否则 skip(total) 会产生空 transcript，模型完全失忆。
    let start = {
        let mut s = raw_start;
        while s < total {
            if matches!(entries[s], TranscriptEntry::User(_)) {
                break;
            }
            s += 1;
        }
        if s >= total {
            raw_start
        } else {
            s
        }
    };

    let compacted: Vec<TranscriptEntry> = entries.into_iter().skip(start).collect();
    let after_tokens = budget::estimate_transcript_tokens(system, &compacted);

    CompactionResult {
        entries: compacted,
        before_tokens,
        after_tokens,
        summary: String::new(),
    }
}

/// 检查是否需要压缩。
///
/// 用 [`budget::calibrated_transcript_tokens`] 而非裸估算判断阈值：本地估算对
/// 代码 / JSON 工具结果系统性低估约 30%，裸估算会让 0.75 阈值（如 1M 模型 = 75 万）
/// 在服务端真实已逼近 100 万时仍判定「不用压」，下一轮请求直接撞 context 上限 400。
/// `last_real` / `last_estimated` 来自最近一次请求的服务端真值与配对估算，任一为 0
/// （新会话还没采样）时退化为裸估算，行为与历史一致。
pub fn needs_compaction(
    system: Option<&str>,
    entries: &[TranscriptEntry],
    policy: &CompactionPolicy,
    last_real: u64,
    last_estimated: u64,
) -> bool {
    budget::calibrated_transcript_tokens(system, entries, last_real, last_estimated)
        > policy.token_budget
}

/// 构造 LLM 摘要压缩请求。调用方需要日志 / model_io dump 时可以先拿到真实 request，
/// 再交给 [`compact_request_with_llm`] 执行，避免日志里的 payload 与实际请求不一致。
pub fn build_compaction_request(
    system: Option<&str>,
    entries: Vec<TranscriptEntry>,
    custom_instructions: Option<&str>,
) -> (usize, ModelRequest) {
    let before_tokens = budget::estimate_transcript_tokens(system, &entries);

    let mut summarize_entries = entries;
    let prompt_text = match custom_instructions.map(str::trim).filter(|s| !s.is_empty()) {
        Some(extra) => format!("{COMPACT_PROMPT}\n\n附加指令：{extra}"),
        None => COMPACT_PROMPT.to_string(),
    };

    // 末尾若已是 user（典型场景：用户主动调 /compact 时刚 push 完 user message），
    // 直接合并到现有 content，避免出现两条连续 user entry——Anthropic provider
    // 不接受连续相同 role 的 message，会返回 400。
    if let Some(TranscriptEntry::User(user_entry)) = summarize_entries.last_mut() {
        user_entry.text = format!("{}\n\n{}", user_entry.text, prompt_text);
    } else {
        summarize_entries.push(TranscriptEntry::User(UserEntry::text(prompt_text)));
    }

    let req = ModelRequest {
        model: String::new(),
        system: system.map(str::to_string),
        entries: summarize_entries,
        tools: Vec::new(),
        max_tokens: 4096,
        reasoning: None,
        meta: model_gateway::types::ModelCallMeta {
            tag: model_gateway::types::ModelCallTag::Compaction,
            ..Default::default()
        },
    };
    (before_tokens, req)
}

/// 执行已构造好的 LLM 摘要压缩请求，并把摘要转换成新的 transcript entries。
pub async fn compact_request_with_llm(
    client: &dyn ModelClient,
    req: ModelRequest,
    before_tokens: usize,
) -> Result<CompactionResult, ModelError> {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    compact_request_with_llm_progress(client, req, before_tokens, cancel, |_| {}).await
}

/// 同 [`compact_request_with_llm`]，但会在压缩摘要流式输出期间回调已产出的估算 token。
pub async fn compact_request_with_llm_progress<F>(
    client: &dyn ModelClient,
    req: ModelRequest,
    before_tokens: usize,
    cancel: CancelFlag,
    on_progress: F,
) -> Result<CompactionResult, ModelError>
where
    F: Fn(usize) + Send + Sync,
{
    let system = req.system.clone();
    let streamed_text = Mutex::new(String::new());
    let on_stream = |event: ModelStreamEvent| {
        if let ModelStreamEvent::TextDelta { text } = event {
            let mut streamed_text = streamed_text.lock().unwrap();
            streamed_text.push_str(&text);
            on_progress(budget::estimate_tokens(&streamed_text));
        }
    };
    let response = client.stream(req, cancel, &on_stream).await?;
    let summary = match response {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text,
    };

    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err(ModelError::Other("压缩失败：模型返回了空摘要".to_string()));
    }

    let new_entries = vec![
        TranscriptEntry::User(UserEntry::text(format!("[前情概要]\n{summary}"))),
        TranscriptEntry::Assistant(AssistantEntry {
            text: "已收到前情概要，将基于此继续。".to_string(),
            reasoning: String::new(),
            reasoning_signature: String::new(),
            tool_calls: Vec::new(),
        }),
    ];
    let after_tokens = budget::estimate_transcript_tokens(system.as_deref(), &new_entries);

    Ok(CompactionResult {
        entries: new_entries,
        before_tokens,
        after_tokens,
        summary,
    })
}

/// LLM 摘要式压缩：用一次 `complete()` 调用把整段对话浓缩成一份简明摘要，
/// 然后用 `[前情概要 + assistant 确认]` 这一对消息替换原 entries。
pub async fn compact_with_llm(
    client: &dyn ModelClient,
    system: Option<&str>,
    entries: Vec<TranscriptEntry>,
    custom_instructions: Option<&str>,
) -> Result<CompactionResult, ModelError> {
    let (before_tokens, req) = build_compaction_request(system, entries, custom_instructions);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    compact_request_with_llm_progress(client, req, before_tokens, cancel, |_| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use common::CancelFlag;
    use model_gateway::types::{ModelStreamEvent, Usage};

    /// 极简 mock：固定返回一段非空摘要，不关心入参。
    struct StubClient;

    #[async_trait]
    impl ModelClient for StubClient {
        fn provider_id(&self) -> &str {
            "stub"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "摘要正文".to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
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

    /// 捕获 stream 入参的 mock，用来断言 compact_with_llm 传入的 entries 形态。
    struct CapturingClient {
        captured: std::sync::Mutex<Option<Vec<TranscriptEntry>>>,
    }

    #[async_trait]
    impl ModelClient for CapturingClient {
        fn provider_id(&self) -> &str {
            "capturing"
        }

        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            *self.captured.lock().unwrap() = Some(req.entries);
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "摘要正文".to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
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
    async fn merges_prompt_when_last_entry_is_user() {
        let client = CapturingClient {
            captured: std::sync::Mutex::new(None),
        };
        let entries = vec![
            TranscriptEntry::User(UserEntry::text("第一条 user")),
            TranscriptEntry::Assistant(AssistantEntry {
                text: "回复".into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                reasoning_signature: String::new(),
            }),
            TranscriptEntry::User(UserEntry::text("用户主动 /compact")),
        ];

        let _ = compact_with_llm(&client, None, entries, None)
            .await
            .unwrap();

        let captured = client.captured.lock().unwrap().clone().unwrap();
        // 仍是 3 条（没有新增第 4 条 user），最后一条 user 的 text 同时包含原文与 prompt
        assert_eq!(captured.len(), 3);
        let last = captured.last().expect("non-empty");
        match last {
            TranscriptEntry::User(u) => {
                assert!(u.text.contains("用户主动 /compact"));
                assert!(u.text.contains("上下文压缩"));
            }
            _ => panic!("last entry should be user"),
        }
        // 最后两条不应都是 user（即倒数第二条不是 user）
        let second_last = &captured[captured.len() - 2];
        assert!(
            !matches!(second_last, TranscriptEntry::User(_)),
            "should not produce two consecutive user entries"
        );
    }

    #[tokio::test]
    async fn appends_prompt_when_last_entry_is_not_user() {
        let client = CapturingClient {
            captured: std::sync::Mutex::new(None),
        };
        let entries = vec![
            TranscriptEntry::User(UserEntry::text("第一条 user")),
            TranscriptEntry::Assistant(AssistantEntry {
                text: "结尾是 assistant".into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                reasoning_signature: String::new(),
            }),
        ];

        let _ = compact_with_llm(&client, None, entries, None)
            .await
            .unwrap();

        let captured = client.captured.lock().unwrap().clone().unwrap();
        // 新增一条 user prompt，共 3 条
        assert_eq!(captured.len(), 3);
        match captured.last().unwrap() {
            TranscriptEntry::User(u) => assert!(u.text.contains("上下文压缩")),
            _ => panic!("last entry should be user"),
        }
    }

    struct CancelAwareClient;

    #[async_trait]
    impl ModelClient for CancelAwareClient {
        fn provider_id(&self) -> &str {
            "cancel-aware"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            if common::runtime::is_cancelled(&cancel) {
                return Err(ModelError::Cancelled);
            }
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "摘要正文".to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
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
    async fn compact_request_uses_provided_cancel_flag() {
        let (_before_tokens, req) = build_compaction_request(
            None,
            vec![TranscriptEntry::User(UserEntry::text("hi"))],
            None,
        );
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let result = compact_request_with_llm_progress(&CancelAwareClient, req, 1, cancel, |_| {})
            .await;

        assert!(matches!(result, Err(ModelError::Cancelled)));
    }

    #[tokio::test]
    async fn returns_compacted_pair_on_success() {
        let entries = vec![TranscriptEntry::User(UserEntry::text("hi"))];
        let result = compact_with_llm(&StubClient, None, entries, None)
            .await
            .unwrap();
        assert_eq!(result.entries.len(), 2);
        assert!(matches!(result.entries[0], TranscriptEntry::User(_)));
        assert!(matches!(result.entries[1], TranscriptEntry::Assistant(_)));
        assert_eq!(result.summary, "摘要正文");
    }

    /// 回归：transcript 里只有一条 User（第 0 条），后面全是 assistant+tool_result 对。
    /// 旧逻辑在「找 User 起点」时走到 total，skip(total) = 空 transcript，模型完全失忆。
    /// 修复后：找不到 User 时退回 raw_start，至少保留最后 N 条。
    #[test]
    fn compact_structural_no_user_in_window_does_not_empty_transcript() {
        use model_gateway::types::{AssistantEntry, ToolResult, UserEntry};
        // 构造 1 user + 50 assistant+tool 对 = 101 条
        // keep_recent_turns * 3 = 8 * 3 = 24；raw_start = 101 - 24 = 77
        // entries[77..101] 全是 assistant/tool_result，找不到 User
        let mut entries = vec![TranscriptEntry::User(UserEntry::text("initial user"))];
        for _ in 0..50 {
            entries.push(TranscriptEntry::Assistant(AssistantEntry {
                text: "doing work".to_string(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
                tool_calls: vec![model_gateway::types::ToolCall {
                    id: "c1".to_string(),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }],
            }));
            entries.push(TranscriptEntry::ToolResults(vec![ToolResult {
                call_id: "c1".to_string(),
                name: "Bash".to_string(),
                content: "file1.rs\nfile2.rs".to_string(),
                artifact: None,
                attachments: Vec::new(),
            }]));
        }
        assert_eq!(entries.len(), 101);

        let policy = crate::definition::CompactionPolicy {
            token_budget: 1, // 强制触发
            keep_recent_turns: 8,
            strategy: crate::definition::CompactionStrategy::Structural,
        };
        let result = compact_structural(None, entries, &policy);

        // 修复前：空 transcript；修复后：非空（保留 raw_start 到末尾）
        assert!(
            !result.entries.is_empty(),
            "compact_structural 不应返回空 transcript"
        );
        // 应该保留最后 24 条（raw_start = 77）
        assert_eq!(result.entries.len(), 24);
    }

    /// 回归：本地估算系统性低估时，裸估算会让该压的没压（下一轮请求撞 context
    /// 上限 400）；校准后用「最近真值/估值」比值放大，正确触发压缩。
    /// 对应 bug：指示器显示 71% 但服务端真实 998k/1M、自动压缩没触发。
    #[test]
    fn calibration_triggers_compaction_when_raw_estimate_underreports() {
        use model_gateway::types::{ToolResult, UserEntry};

        // 构造一段估算约 5 万 token 的历史。
        let big = "x".repeat(200_000); // ~5 万 token（4 字符/token）
        let entries = vec![
            TranscriptEntry::User(UserEntry::text("task")),
            TranscriptEntry::ToolResults(vec![ToolResult {
                call_id: "c1".to_string(),
                name: "Read".to_string(),
                content: big,
                artifact: None,
                attachments: Vec::new(),
            }]),
        ];
        let policy = crate::definition::CompactionPolicy {
            token_budget: 75_000, // 1M 模型的 0.75 阈值量级
            keep_recent_turns: 8,
            strategy: crate::definition::CompactionStrategy::LlmSummary,
        };

        let raw = budget::estimate_transcript_tokens(None, &entries) as u64;
        assert!(raw < policy.token_budget as u64, "裸估算应低于阈值");

        // 无样本：退化为裸估算，不触发（与历史行为一致）。
        assert!(!needs_compaction(None, &entries, &policy, 0, 0));

        // 最近一次服务端真值是估值的 1.6 倍（典型代码/JSON 低估）。
        // 校准后 raw * 1.6 越过阈值 → 触发。
        let last_real = raw * 16 / 10;
        let last_estimated = raw;
        assert!(needs_compaction(
            None,
            &entries,
            &policy,
            last_real,
            last_estimated
        ));
    }

    /// 回归：先发一条短文字（采到含恒定开销的服务端真值 / 极小估值 → 畸形校准比值），
    /// 再单独发一张图片，不应立即触发压缩。
    ///
    /// 修复前三处缺陷叠加必然误触发：① 图片按 base64 字节估成 ~17.8 万 token（实际原生
    /// 编码或 VisionBridge 转文字后 token 量级极小）；② estimate_transcript_tokens 不含
    /// system + tool 定义的恒定开销，只发短文字时估值趋近 0；③ 真值/估值比值无上界，被
    /// 钉成 ~9 倍乘数。图片估值再被这个乘数放大到百万级，碾过 80k 阈值。
    #[test]
    fn single_image_message_does_not_trigger_compaction() {
        use common::attachments::MessageAttachment;
        use model_gateway::types::UserEntry;

        let prompt = "参考这个页面给 sidebar 加文件目录树";

        // 第一轮只发短文字，采到这一刻的本地估值与服务端真值。
        let first_turn = vec![TranscriptEntry::User(UserEntry::text(prompt))];
        let last_estimated = budget::estimate_transcript_tokens(None, &first_turn) as u64;
        let last_real = 31_782; // 服务端真值：含 system + 全部 tool 定义的恒定开销。

        // 第二轮：用户单独发一张 ~2MB base64 截图。
        let with_image = vec![
            TranscriptEntry::User(UserEntry::text(prompt)),
            TranscriptEntry::User(UserEntry {
                text: String::new(),
                attachments: vec![MessageAttachment::Image {
                    name: "image.png".to_string(),
                    media_type: "image/png".to_string(),
                    data: "A".repeat(2_140_744),
                }],
            }),
        ];

        let policy = crate::definition::CompactionPolicy {
            token_budget: 80_000,
            keep_recent_turns: 8,
            strategy: crate::definition::CompactionStrategy::LlmSummary,
        };

        assert!(
            !needs_compaction(None, &with_image, &policy, last_real, last_estimated),
            "单发一张图片不应触发压缩"
        );
    }
}
