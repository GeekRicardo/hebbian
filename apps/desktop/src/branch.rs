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

/// 旁支会话只读工具集——读代码 / 查调用 / 解释实现，绝不改文件。
const BRANCH_TOOLS: &[&str] = &["Read", "Grep"];

/// 旁支历史的内存持有者（Tauri managed state）。
///
/// `key = branch_id`（前端生成的不透明 token），`value = (bound_session_id, 多轮历史)`。
/// bound_session_id 记着这条旁支从哪个主对话 fork 来的——用于 model_io 落盘归属 +
/// 主对话关闭时连带清理。
#[derive(Default)]
pub struct BranchState {
    inner: Mutex<HashMap<String, BranchEntry>>,
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

/// 向旁支发一轮消息，流式回事件（复用主对话同款 `EngineEvent` channel + 前端渲染）。
///
/// 跑完把本轮 user + assistant 追加进内存历史，下一轮自动续接。
#[tauri::command]
pub async fn branch_send(
    app: AppHandle,
    branch_state: State<'_, BranchState>,
    branch_id: String,
    content: String,
    provider_id: Option<String>,
    model: Option<String>,
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let dd = crate::data_dir(&app)?;

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

    let read_tool = Box::new(agent_core::tools::read::ReadTool::new(
        Some(dd.clone()),
        None,
        None,
    ));
    let grep_tool = Box::new(agent_core::tools::grep::GrepTool::new(workspace.clone()));
    let harness = std::sync::Arc::new(Harness::new(
        vec![read_tool, grep_tool],
        HookManager::new(vec![]),
    ));

    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = chat::run_aside(RunAsideArgs {
        data_dir: &dd,
        bound_session_id: &bound_session_id,
        provider_id: &provider_id,
        model: &model,
        system_prompt,
        history,
        user_content,
        attachments: Vec::new(),
        harness,
        workspace,
        enabled_tools: BRANCH_TOOLS.iter().map(|s| s.to_string()).collect(),
        cancel_flag,
        emit_event: move |event| {
            let _ = on_event.send(event);
        },
    })
    .await?;

    let (updated_history, assistant_msg) = result;
    if let Some(entry) = branch_state.inner.lock().unwrap().get_mut(&branch_id) {
        entry.history = updated_history;
    }
    Ok(assistant_msg)
}

/// 追加到 system prompt 末尾，明确旁支的只读身份。
const BRANCH_SYSTEM_SUFFIX: &str = "\n\n\
你现在处在一段「旁支讨论」里：从主对话分叉出来、独立于主线的临时讨论。\
你只有 Read 和 Grep 两个只读工具，可以读代码、查实现、解释调用关系，\
但**改不了任何文件、也跑不了命令**。如果用户的诉求需要真正动手改代码，\
请把要点讲清楚，提示他回到主对话执行。";

/// 从主对话消息列表 fork 一段历史：`up_to`=分叉点（含该条）；`None`=继承全部。
///
/// 抽成纯函数便于回归测试——fork 的核心语义是「截到分叉点为止」，截错会让旁支带上
/// 用户没选的后续上下文，或漏掉分叉点本身。
fn fork_history(messages: &[Message], up_to: Option<&str>) -> Vec<Message> {
    match up_to {
        Some(mid) => {
            let mut out = Vec::new();
            for m in messages {
                out.push(m.clone());
                if m.id == mid {
                    break;
                }
            }
            out
        }
        None => messages.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::storage::sessions::Role;

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
}

