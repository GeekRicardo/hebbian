//! 前端可见的引擎事件（与 desktop `apps/desktop/src/engine/mod.rs` 字段对齐）。
//!
//! v1 在 hebweb 内复制一份是为了让浏览器前端能消费同一份 JSON 结构（前端 types.ts 不动）。
//! v2 会把这套类型 + 翻译函数抽到共享 crate（暂名 `crates/surface-events`），desktop / hebweb
//! 一起依赖那里。届时本文件删除。
//!
//! 与 desktop 版差异：本文件不渲染 StepStarted/Finished、PermissionAutoJudged、EditSnapshot*
//! 这些 v1 浏览器 surface 暂不需要的事件（前端 store 会忽略未知 variant）。

use protocol::{Event as AgentEvent, EventPayload};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    TextDelta {
        text: String,
        /// 子 NestedRun 事件来源标识（架构 §4.4.11.8）。前端按此字段嵌套渲染到父 Task 卡片内部。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    TextDone {
        full_text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolStart {
        index: usize,
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolDone {
        index: usize,
        id: String,
        result: String,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    /// 工具执行中的流式输出片段（架构 §4.4.1）。Bash 前台 stdout/stderr 按 chunk 推过来。
    ToolOutputDelta {
        index: usize,
        id: String,
        chunk: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    RunSuspended {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes_at_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waiting_for_task_ids: Vec<String>,
    },
    RunResumed {
        cause: String,
    },
    PermissionRequested {
        request_id: String,
        kind: String,
        tool_name: String,
        input: Value,
        summary: String,
        risk: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
        /// Bash / PowerShell compound 命令的所有段 fingerprint（架构 §4.4.2）。
        /// 前端据此渲染"多选 list + scope 按钮"——每段一行 checkbox。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command_segments: Vec<String>,
        /// 完整段级状态（只读 / 已白名单 / 不可记忆 / 待审批），弹窗逐段展示
        /// （架构 §4.4.2.3）。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        segments: Vec<protocol::ApprovalSegment>,
        /// 危险复合模式：任何作用域都不可记住，弹窗隐藏记忆区（架构 §4.4.2.2）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refuse_remember: bool,
    },
    PermissionResolved {
        request_id: String,
        decision: String,
    },
    RunModeChanged {
        from: String,
        to: String,
    },
    /// Turn 边界——一次"模型请求 + 可选 tool_call 批"结束（架构 §3 / §4.2）。
    /// 前端据此把当前 streaming bubble 冻结成"已完成 turn 快照"，下一个 Turn 起新
    /// streaming bubble；streaming 中的插队 user message 才能落在它真正回应的 Turn
    /// 之后、下个 Turn 之前。
    TurnFinished {
        /// "end_turn" / "max_iterations" / "permission_denied" / "cancelled" / "failed"
        stop_reason: String,
    },
    UserQuestionRequested {
        request_id: String,
        question: String,
        options: Vec<QuestionOptionDto>,
        #[serde(default)]
        multi: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        questions: Vec<AskQuestionDto>,
    },
    UserQuestionAnswered {
        request_id: String,
        kind: String,
        text: String,
    },
    /// 新会话首轮跑完后，agent_core 后台 task 异步生成的标题已落盘 jsonl。
    /// 前端用它更新 sidebar / chat header；落盘已由 agent_core 完成。
    SessionTitleChanged {
        session_id: String,
        title: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionOptionDto {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskQuestionDto {
    pub title: String,
    pub description: String,
    pub options: Vec<QuestionOptionDto>,
    pub multi: bool,
}

impl From<protocol::QuestionOption> for QuestionOptionDto {
    fn from(o: protocol::QuestionOption) -> Self {
        Self {
            label: o.label,
            description: o.description,
        }
    }
}

impl From<protocol::AskQuestion> for AskQuestionDto {
    fn from(q: protocol::AskQuestion) -> Self {
        Self {
            title: q.title,
            description: q.description,
            options: q.options.into_iter().map(Into::into).collect(),
            multi: q.multi,
        }
    }
}

/// AgentEvent → EngineEvent。仅翻译浏览器 surface 需要的事件，其余返回 None。
pub fn translate(event: &AgentEvent) -> Option<EngineEvent> {
    use EventPayload::*;
    let subagent = event.subagent_call_id.clone();
    Some(match &event.payload {
        TextDelta { text } => EngineEvent::TextDelta {
            text: text.clone(),
            subagent_call_id: subagent.clone(),
        },
        TextDone { full_text } => EngineEvent::TextDone {
            full_text: full_text.clone(),
            subagent_call_id: subagent.clone(),
        },
        Reasoning { text } => EngineEvent::Reasoning {
            text: text.clone(),
            subagent_call_id: subagent.clone(),
        },
        ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => EngineEvent::ToolCallDelta {
            index: *index,
            id: id.clone(),
            name: name.clone(),
            arguments_delta: arguments_delta.clone(),
            subagent_call_id: subagent.clone(),
        },
        ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => EngineEvent::ToolStart {
            index: *index,
            id: call_id.clone(),
            name: name.clone(),
            input: input.clone(),
            subagent_call_id: subagent.clone(),
        },
        ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            artifact_path,
            ..
        } => EngineEvent::ToolDone {
            index: *index,
            id: call_id.clone(),
            result: result.clone(),
            duration_ms: *duration_ms,
            artifact_path: artifact_path.clone(),
            subagent_call_id: subagent.clone(),
        },
        ToolCallOutputDelta {
            index,
            call_id,
            chunk,
        } => EngineEvent::ToolOutputDelta {
            index: *index,
            id: call_id.clone(),
            chunk: chunk.clone(),
            subagent_call_id: subagent.clone(),
        },
        RunFailed { error } => EngineEvent::Error {
            message: error.message.clone(),
        },
        RunSuspended {
            reason,
            resumes_at_ms,
            waiting_for_task_ids,
        } => EngineEvent::RunSuspended {
            reason: format!("{reason:?}").to_lowercase(),
            resumes_at_ms: *resumes_at_ms,
            waiting_for_task_ids: waiting_for_task_ids.clone(),
        },
        RunResumed { cause } => EngineEvent::RunResumed {
            cause: format!("{cause:?}"),
        },
        PermissionRequested {
            request_id,
            kind,
            summary,
            risk,
        } => {
            use protocol::PermissionKind::*;
            let (
                kind_str,
                tool_name,
                tool_input,
                paths,
                fingerprint,
                command_segments,
                segments,
                refuse_remember,
            ) = match kind {
                ToolCall {
                    tool_name,
                    input,
                    fingerprint,
                    command_segments,
                    segments,
                    refuse_remember,
                } => (
                    "tool_call",
                    tool_name.clone(),
                    input.clone(),
                    Vec::<String>::new(),
                    fingerprint.clone(),
                    command_segments.clone(),
                    segments.clone(),
                    *refuse_remember,
                ),
                PathAccess { tool_name, paths } => (
                    "path_access",
                    tool_name.clone(),
                    Value::Null,
                    paths.clone(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                ),
                Plan { .. } => (
                    "plan",
                    String::new(),
                    Value::Null,
                    Vec::new(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                ),
                ContinueLongRun { .. } => (
                    "continue_long_run",
                    String::new(),
                    Value::Null,
                    Vec::new(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                ),
            };
            EngineEvent::PermissionRequested {
                request_id: request_id.as_str().to_string(),
                kind: kind_str.into(),
                tool_name,
                input: tool_input,
                summary: summary.clone(),
                risk: format!("{risk:?}").to_lowercase(),
                paths,
                fingerprint,
                command_segments,
                segments,
                refuse_remember,
            }
        }
        PermissionResolved {
            request_id,
            decision,
        } => {
            use protocol::ApprovalDecision::*;
            EngineEvent::PermissionResolved {
                request_id: request_id.as_str().to_string(),
                decision: match decision {
                    AllowOnce => "allow_once".into(),
                    AllowAndRemember { .. } => "allow_and_remember".into(),
                    Deny => "deny".into(),
                    DenyWithFeedback { .. } => "deny_with_feedback".into(),
                },
            }
        }
        UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => EngineEvent::UserQuestionRequested {
            request_id: request_id.as_str().to_string(),
            question: question.clone(),
            options: options.iter().cloned().map(Into::into).collect(),
            multi: *multi,
            questions: questions.iter().cloned().map(Into::into).collect(),
        },
        UserQuestionAnswered { request_id, answer } => {
            use protocol::UserAnswer::*;
            let (kind, text) = match answer {
                Selected { label } => ("selected".to_string(), label.clone()),
                SelectedMulti { labels } => ("selected_multi".to_string(), labels.join("、")),
                Custom { text } => ("custom".to_string(), text.clone()),
                Cancelled => ("cancelled".to_string(), String::new()),
                Multi { items } => {
                    let text = items
                        .iter()
                        .map(|item| format!("{}: {}", item.title, item.answer.to_agent_text()))
                        .collect::<Vec<_>>()
                        .join("；");
                    ("multi".to_string(), text)
                }
            };
            EngineEvent::UserQuestionAnswered {
                request_id: request_id.as_str().to_string(),
                kind,
                text,
            }
        }
        RunModeChanged { from, to } => EngineEvent::RunModeChanged {
            from: from.clone(),
            to: to.clone(),
        },
        TurnFinished { stop_reason, .. } => EngineEvent::TurnFinished {
            stop_reason: match stop_reason {
                protocol::StopReason::EndTurn => "end_turn",
                protocol::StopReason::MaxIterations => "max_iterations",
                protocol::StopReason::PermissionDenied => "permission_denied",
                protocol::StopReason::Cancelled => "cancelled",
                protocol::StopReason::Failed => "failed",
            }
            .to_string(),
        },
        SessionTitleChanged { session_id, title } => EngineEvent::SessionTitleChanged {
            session_id: session_id.clone(),
            title: title.clone(),
        },
        _ => return None,
    })
}
