//! 旁支对话（branch / aside session）——右侧工作台「旁支对话」tab 的后端（架构 §8.5 QuickChat）。
//!
//! 一段从主对话 fork 出来的临时讨论：继承主对话此刻的聊天记录作为上下文，只挂只读工具
//! （Read / Grep），可以读代码、查调用、解释实现，但**改不了任何文件**。和「元素对话」共用
//! [`chat::run_aside`] 引擎——纯内存、不落盘、关掉即消失，模型 IO 仍写进主对话的
//! model_io.jsonl（kind=aside）供调试面板查看。
//!
//! 与主对话的边界：旁支不进 session 列表、不落 jsonl、不持久化。多个旁支历史按
//! `branch_id` 存在 [`BranchState`] 内存表里，主对话关闭 / 应用退出即一并丢弃。
//! 前端在右侧 tab 里管理多个子旁支，每个子旁支一条独立的内存历史。

use std::collections::HashMap;
use std::sync::Mutex;

use agent_core::hooks::HookManager;
use agent_core::storage::sessions::{self, Message};
use agent_core::system_prompt::{compose_system_prompt, EnvironmentSnapshot};
use agent_core::workspace::Workspace;
use agent_core::Harness;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::chat::{self, RunAsideArgs};
use crate::engine::EngineEvent;
use crate::error::{AppError, AppResult};

/// 旁支会话只读工具集——读代码 / 查调用 / 查资料 / 查记忆，绝不改文件、不跑命令。
///
/// 刻意不含 Bash / Edit / Write 等写工具：旁支引擎 `run_aside` 是纯内存、无审批闸门
/// （permission_store=None），靠「工具集里就没有写能力」从根上保证改不了任何东西。
/// MCP 工具按主对话配置动态发现后追加（外部能力、不碰本地源码）。
const BRANCH_TOOLS: &[&str] = &["Read", "Grep", "WebSearch", "Fetch", "ReadMemory"];

/// 旁支历史的内存持有者（Tauri managed state）。
///
/// `key = branch_id`（前端生成的不透明 token），`value = (bound_session_id, 多轮历史)`。
/// bound_session_id 记着这条旁支从哪个主对话 fork 来的——用于 model_io 落盘归属 +
/// 主对话关闭时连带清理。
#[derive(Default)]
pub struct BranchState {
    inner: Mutex<HashMap<String, BranchEntry>>,
    /// 旁支正在跑的 run 的取消标志（key = branch_id）。停止按钮（branch_cancel）置位它，
    /// run_aside 的 agent loop 检测到即中断。run 结束后移除。与注释旁支 `aside_cancels` 对称。
    cancels: Mutex<HashMap<String, common::CancelFlag>>,
}

struct BranchEntry {
    bound_session_id: String,
    history: Vec<Message>,
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
}

/// 一条旁支的元信息（创建后回给前端，前端据此渲染子 tab）。
#[derive(serde::Serialize)]
pub struct BranchInfo {
    pub branch_id: String,
    pub bound_session_id: String,
    /// fork 时从主对话继承了多少条消息（仅供 UI 显示「基于 N 条记录」）。
    pub inherited_count: usize,
}

/// 新建一条旁支：从主对话当前的聊天记录 fork 一份历史存进内存表。
///
/// `up_to_message_id` 为分叉点（含该条）；`None` = 继承主对话全部历史。
#[tauri::command]
pub fn branch_create(
    app: AppHandle,
    branch_state: State<'_, BranchState>,
    session_id: String,
    up_to_message_id: Option<String>,
) -> AppResult<BranchInfo> {
    let dd = crate::data_dir(&app)?;
    let session = sessions::load(&dd, &session_id)?;

    let history = fork_history(&session.messages, up_to_message_id.as_deref());
    let inherited_count = history.len();

    let branch_id = format!("branch-{}", sessions::new_id());
    branch_state.inner.lock().unwrap().insert(
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

/// 丢弃一条旁支（关闭子 tab）：从内存表删掉，历史随之释放。
#[tauri::command]
pub fn branch_discard(branch_state: State<'_, BranchState>, branch_id: String) {
    branch_state.inner.lock().unwrap().remove(&branch_id);
}

/// 停止一条旁支正在跑的 run：置位它的 cancel flag，run_aside 的 agent loop 检测到即中断
/// （与注释旁支的 heb:aside:stop 对称）。没有正在跑的 run 时静默无操作。
#[tauri::command]
pub fn branch_cancel(branch_state: State<'_, BranchState>, branch_id: String) {
    if let Some(flag) = branch_state.cancels.lock().unwrap().get(&branch_id) {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 向旁支发一轮消息，流式回事件（复用主对话同款 `EngineEvent` channel + 前端渲染）。
///
/// 跑完把本轮 user + assistant 追加进内存历史，下一轮自动续接。
#[tauri::command]
pub async fn branch_send(
    app: AppHandle,
    branch_state: State<'_, BranchState>,
    branch_id: String,
    content: String,
    attachments: Option<Vec<common::attachments::MessageAttachment>>,
    provider_id: Option<String>,
    model: Option<String>,
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let dd = crate::data_dir(&app)?;
    let attachments = attachments.unwrap_or_default();

    // 取旁支当前历史 + 绑定的主对话 id（短临 lock，跑模型时不持锁）。
    let (bound_session_id, history) = {
        let map = branch_state.inner.lock().unwrap();
        let entry = map
            .get(&branch_id)
            .ok_or_else(|| AppError::msg("这条旁支对话已经关掉了"))?;
        (entry.bound_session_id.clone(), entry.history.clone())
    };

    let session = sessions::load(&dd, &bound_session_id)?;
    let provider_id = provider_id
        .filter(|s| !s.is_empty())
        .unwrap_or(session.provider_id.clone());
    let model = model
        .filter(|s| !s.is_empty())
        .unwrap_or(session.model.clone());
    if provider_id.is_empty() {
        return Err(AppError::msg("这个对话还没配置模型，没法开旁支讨论"));
    }

    // 旁支 workspace = 主对话 workspace（同 workdir + allowed_paths），Read/Grep 才能读项目。
    let settings = agent_core::storage::settings::load(&dd);
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
        agent_core::system_prompt::prepend_environment(content, &env)
    } else {
        content
    };

    // 只读工具集：读代码 / 查调用 / 查资料 / 查记忆。无任何写工具——旁支引擎无审批闸门，
    // 靠「工具集里没有写能力」从根上保证改不了文件。MCP 工具按主对话配置动态发现后追加。
    let project_workdir = agent_core::tools::memory_project_workdir(workspace.workdir());
    let mut tools: Vec<Box<dyn agent_core::tools::Tool>> = vec![
        Box::new(agent_core::tools::read::ReadTool::new(
            Some(dd.clone()),
            None,
            None,
        )),
        Box::new(agent_core::tools::grep::GrepTool::new(workspace.clone())),
        Box::new(agent_core::tools::web_search::WebSearchTool),
        Box::new(agent_core::tools::web_fetch::WebFetchTool),
        Box::new(agent_core::tools::read_memory::ReadMemoryTool::new(
            Some(dd.clone()),
            project_workdir,
        )),
    ];
    let mcp_config =
        agent_core::storage::mcp::load(&dd).with_cwd(workspace.workdir().to_path_buf());
    let mcp_tools = agent_core::tools::mcp::discover_tools(&mcp_config).await;
    let mcp_names: Vec<String> = mcp_tools.iter().map(|t| t.name().to_string()).collect();
    tools.extend(mcp_tools);

    // enabled_tools = 固定只读集 + 发现到的 MCP 工具名（MCP 由 enabled_tools 控暴露）。
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

    let harness = std::sync::Arc::new(Harness::new(tools, HookManager::new(vec![])));

    // 停止按钮用：把本次 run 的 cancel flag 存进 state，branch_cancel 置位它中断 agent loop。
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    branch_state
        .cancels
        .lock()
        .unwrap()
        .insert(branch_id.clone(), cancel_flag.clone());
    let result = chat::run_aside(RunAsideArgs {
        data_dir: &dd,
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
        emit_event: move |event| {
            let _ = on_event.send(event);
        },
    })
    .await;
    // run 结束移除 cancel flag（无论成功/失败/取消）。
    branch_state.cancels.lock().unwrap().remove(&branch_id);

    let (updated_history, assistant_msg) = match result {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "branch",
                %branch_id,
                error = %e,
                "[Branch:Send] 旁支一轮失败"
            );
            return Err(e);
        }
    };
    if let Some(entry) = branch_state.inner.lock().unwrap().get_mut(&branch_id) {
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

/// 追加到 system prompt 末尾，明确旁支的只读身份。
const BRANCH_SYSTEM_SUFFIX: &str = "\n\n\
你现在处在一段「旁支讨论」里：从主对话分叉出来、独立于主线的临时调查讨论。\
你挂的是一组只读工具：Read / Grep 读代码查实现、WebSearch / Fetch 查外部资料、\
ReadMemory 查长期记忆，以及主对话配置的 MCP 工具。你**没有任何写文件、改代码、\
跑命令的能力**——这是刻意的设计，旁支就是用来调查、定位、解释、查资料的安静助手。\
如果用户的诉求需要真正动手改代码或执行命令，请把要点和方案讲清楚，\
提示他回到主对话执行。";

/// 从主对话消息列表 fork 一段历史：`up_to`=分叉点（含该条）；`None`=继承全部。
///
/// 抽成纯函数便于回归测试——fork 的核心语义是「截到分叉点为止」，截错会让旁支带上
/// 用户没选的后续上下文，或漏掉分叉点本身。
///
/// fork 出来的每条 assistant message 都经 [`flatten_tool_calls`] 把工具调用折叠成正文
/// 摘要——主对话历史里的 Bash / Edit 等 tool_use block 不在旁支的只读工具集声明里，
/// 原样喂给模型会被 provider 以「tool_use 工具名未声明」直接 400。折叠后旁支看得到
/// 「主对话做过什么」，但不再是真 tool_use，请求合法。
fn fork_history(messages: &[Message], up_to: Option<&str>) -> Vec<Message> {
    let take_until = |out: &mut Vec<Message>| {
        for m in messages {
            out.push(flatten_tool_calls(m.clone()));
            if up_to == Some(m.id.as_str()) {
                break;
            }
        }
    };
    let mut out = Vec::new();
    take_until(&mut out);
    out
}

/// 把一条消息里的工具调用折叠成正文摘要，清空 `tool_calls` / `parts` 里的 ToolCall。
///
/// 旁支只挂只读工具，主对话历史里的写工具 tool_use 必须先消解掉再喂给模型。折叠规则：
/// 每个工具调用渲染成一行 `[调用 <name>: <入参摘要>]` 追加到 `content` 末尾；保留 parts
/// 里的 Text / Reasoning（它们是合法正文，不会触发 400）。
fn flatten_tool_calls(mut msg: Message) -> Message {
    if msg.role != sessions::Role::Assistant {
        return msg;
    }

    let mut summaries: Vec<String> = Vec::new();
    for call in &msg.tool_calls {
        summaries.push(tool_call_summary(&call.name, &call.input));
    }
    // parts 里也可能承载 ToolCall（from_session 优先读 parts），一并折叠后剔除。
    for part in &msg.parts {
        if let sessions::MessagePart::ToolCall { name, input, .. } = part {
            summaries.push(tool_call_summary(name, input));
        }
    }
    msg.tool_calls.clear();
    msg.parts
        .retain(|p| !matches!(p, sessions::MessagePart::ToolCall { .. }));

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
    use agent_core::storage::sessions::Role;
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

    /// 构造一条带工具调用的 assistant message（id / 正文 / 一个 tool_call）。
    fn assistant_with_tool(id: &str, text: &str, tool: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: text.to_string(),
            tool_calls: vec![sessions::MessageToolCall {
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
        // 含分叉点、丢弃其后的 c/d
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
        // 主对话历史里的 Bash tool_use：fork 后必须折叠成正文、清空 tool_calls，
        // 否则旁支只读工具集声明不含 Bash，喂给模型会被 provider 400。
        let msgs = vec![
            msg("u1"),
            assistant_with_tool("a1", "我来跑下测试", "Bash", json!({ "command": "cargo test" })),
        ];
        let forked = fork_history(&msgs, None);
        let assistant = &forked[1];
        assert!(
            assistant.tool_calls.is_empty(),
            "fork 后 assistant 不应再残留 tool_calls（会触发 provider 400）"
        );
        assert!(
            assistant.content.contains("[调用 Bash")
                && assistant.content.contains("我来跑下测试"),
            "工具调用应折叠成正文摘要并保留原正文，实际: {}",
            assistant.content
        );
    }

    #[test]
    fn fork_flattens_tool_call_parts() {
        // parts 里承载 ToolCall（from_session 优先读 parts）也要折叠剔除。
        let mut m = msg("a1");
        m.role = Role::Assistant;
        m.content = "看一下文件".to_string();
        m.parts = vec![sessions::MessagePart::ToolCall {
            id: "c1".to_string(),
            name: "Edit".to_string(),
            input: json!({ "file_path": "/tmp/x.rs" }),
            arguments: String::new(),
            result: Some("ok".to_string()),
            duration_ms: None,
            is_error: false,
        }];
        let forked = fork_history(&[m], None);
        let a = &forked[0];
        assert!(
            !a.parts
                .iter()
                .any(|p| matches!(p, sessions::MessagePart::ToolCall { .. })),
            "parts 里的 ToolCall 应被剔除"
        );
        assert!(a.content.contains("[调用 Edit"), "应折叠成正文: {}", a.content);
    }

    #[test]
    fn fork_keeps_user_messages_intact() {
        // user message 不该被折叠改写。
        let mut u = msg("u1");
        u.content = "原始问题".to_string();
        let forked = fork_history(&[u], None);
        assert_eq!(forked[0].content, "原始问题");
    }
}

