//! 旁支对话引擎（架构 §8.5）：从主对话 fork 一段历史、挂只读工具的临时讨论。
//!
//! surface 无关：BranchState 持有多条旁支的内存历史 + 运行中 run 的 cancel flag；
//! [`BranchEngine`] 暴露 create / discard / cancel / send 四个能力。desktop（Tauri command）
//! 与 hebweb（WS）各持有一个实例，命令层只负责"解析入参 + 注入事件投递闭包"。
//!
//! 与主对话的边界：旁支不进 session 列表、不落 jsonl、不持久化。引擎复用 [`crate::aside`]
//! 的 `run_aside`（纯内存、不落盘、emit `WireEvent`），模型 IO 仍写进主对话 model_io.jsonl
//! （kind=aside）供调试面板查看。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::CancelFlag;
use protocol::WireEvent;
use serde::Serialize;

use crate::aside::{run_aside, RunAsideArgs};
use crate::hooks::HookManager;
use crate::storage::sessions::{self, Message, MessagePart, Role};
use crate::system_prompt::{compose_system_prompt, EnvironmentSnapshot};
use crate::workspace::Workspace;
use crate::Harness;

/// 旁支会话只读工具集——读代码 / 查调用 / 查资料 / 查记忆，绝不改文件、不跑命令。
///
/// 刻意不含 Bash / Edit / Write 等写工具：旁支引擎 `run_aside` 纯内存、无审批闸门
/// （permission_store=None），靠"工具集里就没有写能力"从根上保证改不了任何东西。
/// MCP 工具按主对话配置动态发现后追加（外部能力、不碰本地源码）。
const BRANCH_TOOLS: &[&str] = &["Read", "Grep", "WebSearch", "Fetch", "ReadMemory"];

/// 追加到 system prompt 末尾，明确旁支的只读身份。
const BRANCH_SYSTEM_SUFFIX: &str = "\n\n\
你现在处在一段「旁支讨论」里：从主对话分叉出来、独立于主线的临时调查讨论。\
你挂的是一组只读工具：Read / Grep 读代码查实现、WebSearch / Fetch 查外部资料、\
ReadMemory 查长期记忆，以及主对话配置的 MCP 工具。你**没有任何写文件、改代码、\
跑命令的能力**——这是刻意的设计，旁支就是用来调查、定位、解释、查资料的安静助手。\
如果用户的诉求需要真正动手改代码或执行命令，请把要点和方案讲清楚，\
提示他回到主对话执行。";

/// 一条旁支的内存状态：从哪个主对话 fork 来 + 多轮历史。
struct BranchEntry {
    bound_session_id: String,
    history: Vec<Message>,
}

/// 一条旁支的元信息（创建后回给 surface，据此渲染子 tab）。
#[derive(Debug, Clone, Serialize)]
pub struct BranchInfo {
    pub branch_id: String,
    pub bound_session_id: String,
    /// fork 时从主对话继承了多少条消息（仅供 UI 显示「基于 N 条记录」）。
    pub inherited_count: usize,
}

/// 旁支历史的内存持有者。`key = branch_id`（surface 生成的不透明 token）。
#[derive(Default)]
pub struct BranchState {
    inner: Mutex<HashMap<String, BranchEntry>>,
    /// 运行中 run 的取消标志（key = branch_id）。cancel 置位它，run_aside 的 agent loop 检测到即中断。
    cancels: Mutex<HashMap<String, CancelFlag>>,
}

impl BranchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 主对话关闭 / 删除时连带清掉它的全部旁支历史。
    pub fn drop_for_session(&self, session_id: &str) {
        self.inner
            .lock()
            .unwrap()
            .retain(|_, e| e.bound_session_id != session_id);
    }

    /// 丢弃一条旁支（关闭子 tab）：从内存表删掉，历史随之释放。
    /// 纯内存操作，不需要 data_dir，故直接挂在 state 上供 surface 调。
    pub fn discard(&self, branch_id: &str) {
        self.inner.lock().unwrap().remove(branch_id);
    }

    /// 停止一条旁支正在跑的 run：置位它的 cancel flag，run_aside 的 agent loop 检测到即中断。
    pub fn cancel(&self, branch_id: &str) {
        if let Some(flag) = self.cancels.lock().unwrap().get(branch_id) {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 查一条旁支绑定的主对话 id（surface 把旁支事件路由到该主对话的事件通道时用）。
    pub fn bound_session_of(&self, branch_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .get(branch_id)
            .map(|e| e.bound_session_id.clone())
    }
}

/// 旁支引擎：surface 无关的 create / discard / cancel / send。
///
/// data_dir 注入一次，之后各操作复用。BranchState 内部 `Arc`，可被多处共享。
pub struct BranchEngine {
    data_dir: std::path::PathBuf,
    state: Arc<BranchState>,
}

impl BranchEngine {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self {
            data_dir,
            state: Arc::new(BranchState::new()),
        }
    }

    /// 复用已有 BranchState（surface 想自己持有时）。
    pub fn with_state(data_dir: std::path::PathBuf, state: Arc<BranchState>) -> Self {
        Self { data_dir, state }
    }

    pub fn state(&self) -> &Arc<BranchState> {
        &self.state
    }

    /// 新建一条旁支：从主对话当前聊天记录 fork 一份历史存进内存表。
    /// `up_to_message_id` = 分叉点（含该条）；`None` = 继承全部历史。
    pub fn create(
        &self,
        session_id: String,
        up_to_message_id: Option<String>,
    ) -> Result<BranchInfo, String> {
        let session = sessions::load(&self.data_dir, &session_id).map_err(|e| e.to_string())?;
        let history = fork_history(&session.messages, up_to_message_id.as_deref());
        let inherited_count = history.len();

        let branch_id = format!("branch-{}", sessions::new_id());
        self.state.inner.lock().unwrap().insert(
            branch_id.clone(),
            BranchEntry {
                bound_session_id: session_id.clone(),
                history,
            },
        );
        Ok(BranchInfo {
            branch_id,
            bound_session_id: session_id,
            inherited_count,
        })
    }

    /// 丢弃一条旁支（关闭子 tab）。
    pub fn discard(&self, branch_id: &str) {
        self.state.discard(branch_id);
    }

    /// 停止一条旁支正在跑的 run。没有正在跑的 run 时静默无操作。
    pub fn cancel(&self, branch_id: &str) {
        self.state.cancel(branch_id);
    }

    /// 向旁支发一轮消息，经 `emit_event` 流式回 [`WireEvent`]。跑完把本轮 user + assistant
    /// 追加进内存历史，下一轮自动续接。返回本轮 assistant message。
    pub async fn send<F: Fn(WireEvent) + Send + Sync>(
        &self,
        branch_id: String,
        content: String,
        attachments: Vec<common::attachments::MessageAttachment>,
        provider_id: Option<String>,
        model: Option<String>,
        emit_event: F,
    ) -> Result<Message, String> {
        let dd = &self.data_dir;

        // 取旁支当前历史 + 绑定的主对话 id（短临 lock，跑模型时不持锁）。
        let (bound_session_id, history) = {
            let map = self.state.inner.lock().unwrap();
            let entry = map
                .get(&branch_id)
                .ok_or_else(|| "这条旁支对话已经关掉了".to_string())?;
            (entry.bound_session_id.clone(), entry.history.clone())
        };

        let session = sessions::load(dd, &bound_session_id).map_err(|e| e.to_string())?;
        let provider_id = provider_id
            .filter(|s| !s.is_empty())
            .unwrap_or(session.provider_id.clone());
        let model = model
            .filter(|s| !s.is_empty())
            .unwrap_or(session.model.clone());
        if provider_id.is_empty() {
            return Err("这个对话还没配置模型，没法开旁支讨论".to_string());
        }

        // 旁支 workspace = 主对话 workspace（同 workdir + allowed_paths），Read/Grep 才能读项目。
        let settings = crate::storage::settings::load(dd);
        let workdir = session
            .workdir
            .clone()
            .or_else(|| settings.conversation.workdir.clone())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let allowed_paths = session
            .allowed_paths
            .clone()
            .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
        let workspace = Workspace::with_runtime_state(
            workdir.clone(),
            allowed_paths,
            session.runtime_allowed_paths.clone(),
            session.pending_runtime_allowed_paths.clone(),
        );

        // system = 主对话 persona + BASE prompt（只读旁支不注入 rules，避免误导改文件指令）。
        let mut system_prompt = compose_system_prompt(session.system_prompt.as_deref());
        system_prompt.push_str(BRANCH_SYSTEM_SUFFIX);
        // 只对首轮注入 <environment>（让模型知道 cwd / 平台），续接轮历史里已有。
        let user_content = if history.is_empty() {
            let env = EnvironmentSnapshot::from_workspace(&workspace);
            crate::system_prompt::prepend_environment(content, &env)
        } else {
            content
        };

        // 只读工具集 + MCP（MCP 由 enabled_tools 控暴露）。无任何写工具——旁支引擎无审批闸门，
        // 靠"工具集里没有写能力"从根上保证改不了文件。
        let project_workdir = crate::tools::memory_project_workdir(workspace.workdir());
        let mut tools: Vec<Box<dyn crate::tools::Tool>> = vec![
            Box::new(crate::tools::read::ReadTool::new(
                Some(dd.clone()),
                None,
                None,
            )),
            Box::new(crate::tools::grep::GrepTool::new(workspace.clone())),
            Box::new(crate::tools::web_search::WebSearchTool),
            Box::new(crate::tools::web_fetch::WebFetchTool),
            Box::new(crate::tools::read_memory::ReadMemoryTool::new(
                Some(dd.clone()),
                project_workdir,
            )),
        ];
        let mcp_config = crate::storage::mcp::load(dd).with_cwd(workspace.workdir().to_path_buf());
        let mcp_tools = crate::tools::mcp::discover_tools(&mcp_config).await;
        let mcp_names: Vec<String> = mcp_tools.iter().map(|t| t.name().to_string()).collect();
        tools.extend(mcp_tools);

        let mut enabled_tools: Vec<String> = BRANCH_TOOLS.iter().map(|s| s.to_string()).collect();
        enabled_tools.extend(mcp_names.iter().cloned());

        tracing::info!(
            target: "branch",
            %branch_id,
            %bound_session_id,
            %provider_id,
            %model,
            history_len = history.len(),
            mcp_tools = ?mcp_names,
            "[Branch:Send] 旁支开跑：只读工具集 + MCP"
        );

        let harness = Arc::new(Harness::new(tools, HookManager::new(vec![])));

        // 本次 run 的 cancel flag 存进 state，cancel() 置位它中断 agent loop。run 结束移除。
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.state
            .cancels
            .lock()
            .unwrap()
            .insert(branch_id.clone(), cancel_flag.clone());

        let result = run_aside(RunAsideArgs {
            data_dir: dd,
            bound_session_id: &bound_session_id,
            provider_id: &provider_id,
            model: &model,
            system_prompt,
            history,
            user_content,
            attachments,
            harness,
            workspace,
            enabled_tools,
            cancel_flag,
            emit_event,
        })
        .await;

        self.state.cancels.lock().unwrap().remove(&branch_id);

        let (updated_history, assistant_msg) = result.inspect_err(|e| {
            tracing::warn!(target: "branch", %branch_id, error = %e, "[Branch:Send] 旁支一轮失败");
        })?;
        if let Some(entry) = self.state.inner.lock().unwrap().get_mut(&branch_id) {
            entry.history = updated_history;
        }
        tracing::info!(
            target: "branch",
            %branch_id,
            reply_len = assistant_msg.content.len(),
            "[Branch:Send] 旁支一轮完成"
        );
        Ok(assistant_msg)
    }
}

/// 从主对话消息列表 fork 一段历史：`up_to`=分叉点（含该条）；`None`=继承全部。
///
/// 每条 assistant message 经 [`flatten_tool_calls`] 把工具调用折叠成正文摘要——主对话历史里
/// 的 Bash / Edit 等 tool_use block 不在旁支只读工具集声明里，原样喂给模型会被 provider 以
/// 「tool_use 工具名未声明」直接 400。折叠后旁支看得到「主对话做过什么」，但不再是真 tool_use。
pub fn fork_history(messages: &[Message], up_to: Option<&str>) -> Vec<Message> {
    let mut out = Vec::new();
    for m in messages {
        out.push(flatten_tool_calls(m.clone()));
        if up_to == Some(m.id.as_str()) {
            break;
        }
    }
    out
}

/// 把一条 assistant 消息里的工具调用折叠成正文摘要，清空 `tool_calls` / `parts` 里的 ToolCall。
fn flatten_tool_calls(mut msg: Message) -> Message {
    if msg.role != Role::Assistant {
        return msg;
    }
    let mut summaries: Vec<String> = Vec::new();
    for call in &msg.tool_calls {
        summaries.push(tool_call_summary(&call.name, &call.input));
    }
    for part in &msg.parts {
        if let MessagePart::ToolCall { name, input, .. } = part {
            summaries.push(tool_call_summary(name, input));
        }
    }
    msg.tool_calls.clear();
    msg.parts
        .retain(|p| !matches!(p, MessagePart::ToolCall { .. }));

    if !summaries.is_empty() {
        if !msg.content.is_empty() {
            msg.content.push('\n');
        }
        msg.content.push_str(&summaries.join("\n"));
    }
    msg
}

/// 单个工具调用的一行正文摘要：`[调用 <name>: <入参首项截断>]`。
fn tool_call_summary(name: &str, input: &serde_json::Value) -> String {
    let brief = input
        .as_object()
        .and_then(|o| {
            o.values()
                .find_map(|v| v.as_str())
                .map(|s| s.chars().take(80).collect::<String>())
        })
        .unwrap_or_default();
    if brief.is_empty() {
        format!("[调用 {name}]")
    } else {
        format!("[调用 {name}: {brief}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sessions::MessageToolCall;
    use serde_json::json;

    fn msg(id: &str) -> Message {
        Message {
            id: id.to_string(),
            role: Role::User,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        }
    }

    fn assistant_with_tool(id: &str, text: &str, tool: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: text.to_string(),
            tool_calls: vec![MessageToolCall {
                id: format!("{id}-call"),
                name: tool.to_string(),
                input,
                result: Some("done".to_string()),
                duration_ms: None,
                is_error: false,
                nested: Vec::new(),
            }],
            ..msg(id)
        }
    }

    #[test]
    fn fork_full_history_when_no_anchor() {
        let msgs = vec![msg("a"), msg("b"), msg("c")];
        let forked = fork_history(&msgs, None);
        assert_eq!(forked.len(), 3);
        assert_eq!(forked.last().unwrap().id, "c");
    }

    #[test]
    fn fork_truncates_inclusive_at_anchor() {
        let msgs = vec![msg("a"), msg("b"), msg("c"), msg("d")];
        let forked = fork_history(&msgs, Some("b"));
        assert_eq!(
            forked.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn fork_missing_anchor_keeps_all() {
        let msgs = vec![msg("a"), msg("b")];
        let forked = fork_history(&msgs, Some("zzz"));
        assert_eq!(forked.len(), 2);
    }

    #[test]
    fn drop_for_session_clears_only_bound_branches() {
        let state = BranchState::new();
        {
            let mut map = state.inner.lock().unwrap();
            map.insert(
                "b1".into(),
                BranchEntry {
                    bound_session_id: "s1".into(),
                    history: vec![],
                },
            );
            map.insert(
                "b2".into(),
                BranchEntry {
                    bound_session_id: "s2".into(),
                    history: vec![],
                },
            );
        }
        state.drop_for_session("s1");
        let map = state.inner.lock().unwrap();
        assert!(!map.contains_key("b1"));
        assert!(map.contains_key("b2"));
    }

    #[test]
    fn fork_flattens_assistant_tool_calls() {
        let msgs = vec![
            msg("u1"),
            assistant_with_tool(
                "a1",
                "我来跑下测试",
                "Bash",
                json!({ "command": "cargo test" }),
            ),
        ];
        let forked = fork_history(&msgs, None);
        let assistant = &forked[1];
        assert!(assistant.tool_calls.is_empty());
        assert!(
            assistant.content.contains("[调用 Bash") && assistant.content.contains("我来跑下测试")
        );
    }

    #[test]
    fn fork_keeps_user_messages_intact() {
        let mut u = msg("u1");
        u.content = "原始问题".to_string();
        let forked = fork_history(&[u], None);
        assert_eq!(forked[0].content, "原始问题");
    }
}
