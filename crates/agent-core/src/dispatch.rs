//! 工具派发器：把一组 tool call 跑出 `Vec<ToolResult>`。
//!
//! 接管以下职责（让 [`agent_loop`] 不必再 inline 200 行 closure）：
//! - 路径越界审批 → emit `PermissionRequested { kind: PathAccess }` + await
//! - 工具审批 → emit `PermissionRequested { kind: ToolCall }` + await
//! - 提问通路（`ask` 工具）→ emit `UserQuestionRequested` + await
//! - 工具执行（含超时输出截断）+ emit `ToolCallStarted` / `ToolCallFinished`
//!
//! 并发策略：`ReadOnly` 工具与同 turn 其他 ReadOnly 工具并发；需要 HITL
//! 的工具因 `await` 自然串行（无需特殊调度）。
//!
//! [`agent_loop`]: super::agent_loop

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::{join_all, BoxFuture};
use protocol::{
    ApprovalDecision, EventPayload, PermissionKind, PermissionRequestId, QuestionOption, RiskLevel,
    UserAnswer,
};
use serde::Deserialize;
use tokio::sync::oneshot;
use tracing::warn;

use crate::{
    agent_loop::EventSink,
    run_state::RunState,
    tools::{
        hitl::{HitlGate, PermissionDecision},
        registry::ToolRegistry,
        ASK_TOOL_NAME,
    },
    workspace::Workspace,
};
use model_gateway::types::{ModelError, ToolCall, ToolResult};
use platform::{runtime as cancellation, CancelFlag};

const MAX_TOOL_RESULT_INLINE: usize = 6_000;

/// `ask` 工具的输入。
#[derive(Debug, Deserialize)]
struct AskInput {
    question: String,
    options: Vec<QuestionOption>,
    /// 是否允许多选；缺省 false（单选）。
    #[serde(default)]
    multi: bool,
}

/// 一次越界路径审批的待解条目。
struct PathApproval {
    request_id: PermissionRequestId,
    paths: Vec<PathBuf>,
    waiter: oneshot::Receiver<ApprovalDecision>,
}

/// 派发一组 tool call 并发返回结果。所有依赖以 `Arc` 持有，clone 成本低。
pub struct ToolDispatcher {
    pub registry: Arc<ToolRegistry>,
    pub hitl: Arc<HitlGate>,
    pub workspace: Arc<Workspace>,
    pub state: Arc<RunState>,
    pub sink: EventSink,
    pub cancel: CancelFlag,
}

impl ToolDispatcher {
    /// 派发整组 tool call。返回按 call_index 排序的 ToolResult。
    /// `dispatch_offset` 是这一轮在整个 run 内的全局起始 index。
    pub async fn run_calls(
        &self,
        calls: &[ToolCall],
        dispatch_offset: usize,
    ) -> Result<Vec<ToolResult>, ModelError> {
        let mut tasks: Vec<BoxFuture<'static, Result<(usize, ToolResult), ModelError>>> =
            Vec::with_capacity(calls.len());

        for (call_index, call) in calls.iter().enumerate() {
            if cancellation::is_cancelled(&self.cancel) {
                self.hitl.cancel_all_pending();
                break;
            }
            let dispatch_index = dispatch_offset + call_index;
            tasks.push(if call.name == ASK_TOOL_NAME {
                self.spawn_ask(call.clone(), call_index, dispatch_index)
            } else {
                self.spawn_tool(call.clone(), call_index, dispatch_index)
            });
        }

        let mut results = Vec::with_capacity(tasks.len());
        for outcome in join_all(tasks).await {
            results.push(outcome?);
        }
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, r)| r).collect())
    }

    /// 普通工具派发：路径审批 → 工具审批 → 执行。
    fn spawn_tool(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let tool = self.registry.find(&call.name);

        // 路径越界检查（同步）
        let out_of_scope: Vec<PathBuf> = tool
            .as_ref()
            .map(|t| {
                t.affected_paths(&call.input)
                    .into_iter()
                    .filter(|p| !self.workspace.allows(p))
                    .collect()
            })
            .unwrap_or_default();

        let path_pending = if out_of_scope.is_empty() {
            None
        } else {
            Some(self.request_path_approval(&call.name, out_of_scope))
        };

        // 工具审批
        let class = tool
            .as_ref()
            .map(|t| t.classify(&call.input))
            .unwrap_or(crate::tools::ToolClass::ReadOnly);
        let permission = self.hitl.check(&call.name, &class);
        if let PermissionDecision::NeedsApproval { request_id, .. } = &permission {
            self.emit(EventPayload::PermissionRequested {
                request_id: request_id.clone(),
                kind: PermissionKind::ToolCall {
                    tool_name: call.name.clone(),
                    input: call.input.clone(),
                },
                summary: format!("工具 {} 请求执行", call.name),
                risk: RiskLevel::Medium,
            });
        }

        let state = self.state.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();
        let workspace = self.workspace.clone();

        Box::pin(async move {
            // 路径审批
            if let Some(p) = path_pending {
                match await_path_decision(&sink, &state, &workspace, p).await {
                    Ok(()) => {}
                    Err(reason) => {
                        return Ok(deny_tool(
                            call,
                            call_index,
                            dispatch_index,
                            &state,
                            &sink,
                            reason,
                        ));
                    }
                }
            }

            // 工具审批
            match await_permission_decision(&sink, &state, permission).await {
                Ok(()) => {}
                Err(reason) => {
                    return Ok(deny_tool(
                        call,
                        call_index,
                        dispatch_index,
                        &state,
                        &sink,
                        reason,
                    ));
                }
            }

            if cancellation::is_cancelled(&cancel) {
                return Err(ModelError::Cancelled);
            }

            // 执行
            sink(state.event(EventPayload::ToolCallStarted {
                index: dispatch_index,
                call_id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            }));

            let started = Instant::now();
            let raw = match tool {
                Some(t) => t.execute(call.input.clone()).await.unwrap_or_else(|e| {
                    warn!(tool = %call.name, error = %e, "tool exec error");
                    format!("工具执行错误: {e}")
                }),
                None => {
                    warn!(tool = %call.name, "tool not in registry");
                    format!("未找到工具: {}", call.name)
                }
            };
            let duration_ms = started.elapsed().as_millis() as u64;

            let (content, truncated) = truncate_tool_result(raw);

            sink(state.event(EventPayload::ToolCallFinished {
                index: dispatch_index,
                call_id: call.id.clone(),
                result: content.clone(),
                duration_ms,
                truncated,
            }));

            Ok((
                call_index,
                ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content,
                },
            ))
        })
    }

    /// `ask` 工具派发：emit UserQuestionRequested + await UserAnswer。
    fn spawn_ask(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let hitl = self.hitl.clone();
        let state = self.state.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();

        Box::pin(async move {
            let (question, options, multi) = match parse_ask_input(&call.input) {
                Ok(parts) => parts,
                Err(err) => {
                    return Ok(finish_ask_with_error(
                        call,
                        call_index,
                        dispatch_index,
                        &state,
                        &sink,
                        err,
                    ));
                }
            };

            sink(state.event(EventPayload::ToolCallStarted {
                index: dispatch_index,
                call_id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            }));

            let (request_id, waiter) = hitl.open_question();
            sink(state.event(EventPayload::UserQuestionRequested {
                request_id: request_id.clone(),
                question,
                options,
                multi,
            }));

            let answer = waiter.await.unwrap_or(UserAnswer::Cancelled);

            sink(state.event(EventPayload::UserQuestionAnswered {
                request_id,
                answer: answer.clone(),
            }));

            if cancellation::is_cancelled(&cancel) {
                return Err(ModelError::Cancelled);
            }

            let content = answer.to_agent_text();
            sink(state.event(EventPayload::ToolCallFinished {
                index: dispatch_index,
                call_id: call.id.clone(),
                result: content.clone(),
                duration_ms: 0,
                truncated: false,
            }));

            Ok((
                call_index,
                ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content,
                },
            ))
        })
    }

    /// 申请越界路径访问审批：开 pending + emit `PermissionRequested { kind: PathAccess }`。
    fn request_path_approval(&self, tool_name: &str, paths: Vec<PathBuf>) -> PathApproval {
        let (request_id, waiter) = self.hitl.open_approval();
        let path_strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let summary = if path_strings.len() == 1 {
            format!("工具 {tool_name} 想访问越界路径：{}", path_strings[0])
        } else {
            format!("工具 {tool_name} 想访问 {} 个越界路径", path_strings.len())
        };
        self.emit(EventPayload::PermissionRequested {
            request_id: request_id.clone(),
            kind: PermissionKind::PathAccess {
                tool_name: tool_name.to_string(),
                paths: path_strings,
            },
            summary,
            risk: RiskLevel::Medium,
        });
        PathApproval {
            request_id,
            paths,
            waiter,
        }
    }

    fn emit(&self, payload: EventPayload) {
        (self.sink)(self.state.event(payload));
    }
}

/// 等路径审批结果；批准则按 scope 累加到 workspace.allowed_dirs。
async fn await_path_decision(
    sink: &EventSink,
    state: &Arc<RunState>,
    workspace: &Arc<Workspace>,
    pending: PathApproval,
) -> Result<(), String> {
    let PathApproval {
        request_id,
        paths,
        waiter,
    } = pending;

    let decision = waiter.await.map_err(|_| "路径审批通道已关闭".to_string())?;
    sink(state.event(EventPayload::PermissionResolved {
        request_id,
        decision: decision.clone(),
    }));
    match decision {
        ApprovalDecision::AllowOnce => Ok(()),
        ApprovalDecision::AllowAndRemember { .. } => {
            for p in &paths {
                workspace.add_allowed_dir(p.clone());
            }
            Ok(())
        }
        ApprovalDecision::Deny => Err("用户拒绝路径访问".into()),
        ApprovalDecision::DenyWithFeedback { feedback } => Err(feedback),
    }
}

/// 等工具审批结果；emit `PermissionResolved`。
async fn await_permission_decision(
    sink: &EventSink,
    state: &Arc<RunState>,
    decision: PermissionDecision,
) -> Result<(), String> {
    match decision {
        PermissionDecision::Approved => Ok(()),
        PermissionDecision::Denied { reason } => Err(reason),
        PermissionDecision::NeedsApproval { request_id, waiter } => {
            let outcome = waiter.await.map_err(|_| "审批通道已关闭".to_string())?;
            sink(state.event(EventPayload::PermissionResolved {
                request_id,
                decision: outcome.clone(),
            }));
            match outcome {
                ApprovalDecision::AllowOnce | ApprovalDecision::AllowAndRemember { .. } => Ok(()),
                ApprovalDecision::Deny => Err("用户拒绝".into()),
                ApprovalDecision::DenyWithFeedback { feedback } => Err(feedback),
            }
        }
    }
}

/// 把"被拒"渲染为 ToolStarted/Finished + ToolResult，让 transcript 一致。
fn deny_tool(
    call: ToolCall,
    call_index: usize,
    dispatch_index: usize,
    state: &Arc<RunState>,
    sink: &EventSink,
    reason: String,
) -> (usize, ToolResult) {
    warn!(tool = %call.name, %reason, "tool denied");
    let denied = format!("工具调用被拒绝: {reason}");
    sink(state.event(EventPayload::ToolCallStarted {
        index: dispatch_index,
        call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
    }));
    sink(state.event(EventPayload::ToolCallFinished {
        index: dispatch_index,
        call_id: call.id.clone(),
        result: denied.clone(),
        duration_ms: 0,
        truncated: false,
    }));
    (
        call_index,
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: denied,
        },
    )
}

fn finish_ask_with_error(
    call: ToolCall,
    call_index: usize,
    dispatch_index: usize,
    state: &Arc<RunState>,
    sink: &EventSink,
    error: String,
) -> (usize, ToolResult) {
    sink(state.event(EventPayload::ToolCallStarted {
        index: dispatch_index,
        call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
    }));
    sink(state.event(EventPayload::ToolCallFinished {
        index: dispatch_index,
        call_id: call.id.clone(),
        result: error.clone(),
        duration_ms: 0,
        truncated: false,
    }));
    (
        call_index,
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: error,
        },
    )
}

fn truncate_tool_result(raw: String) -> (String, bool) {
    if raw.len() <= MAX_TOOL_RESULT_INLINE {
        return (raw, false);
    }

    let mut end = MAX_TOOL_RESULT_INLINE;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }

    (format!("{}…[已截断]", &raw[..end]), true)
}

fn parse_ask_input(
    input: &serde_json::Value,
) -> Result<(String, Vec<QuestionOption>, bool), String> {
    let parsed: AskInput = serde_json::from_value(input.clone())
        .map_err(|e| format!("ask 工具 input 解析失败：{e}"))?;
    if !(2..=5).contains(&parsed.options.len()) {
        return Err(format!(
            "ask 工具要求提供 2-5 个选项，实际给了 {} 个",
            parsed.options.len()
        ));
    }
    Ok((parsed.question, parsed.options, parsed.multi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_tool_result_preserves_utf8_char_boundaries() {
        let raw = format!("{}中", "a".repeat(MAX_TOOL_RESULT_INLINE - 1));
        assert!(raw.len() > MAX_TOOL_RESULT_INLINE);
        assert!(!raw.is_char_boundary(MAX_TOOL_RESULT_INLINE));

        let (content, truncated) = truncate_tool_result(raw);

        assert!(truncated);
        assert_eq!(
            content,
            format!("{}…[已截断]", "a".repeat(MAX_TOOL_RESULT_INLINE - 1))
        );
    }
}
