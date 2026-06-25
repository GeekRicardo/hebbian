//! Run 落盘协调器（架构 §4.9.5，2026-06-25）：session.jsonl 的 user / assistant /
//! 系统通知由 agent_core 在 agent_loop 主体内单点串行 append，surface 只渲染。
//!
//! 历史上三个 surface（desktop / cli / web）各写一份「事件流 → Message 落盘」逻辑：
//! user 在 run 启动前落、assistant 等 run 收尾落，而 goal/compact/memory marker 由
//! agent_core 即时落——两套落盘时钟。assistant 落得晚、created_at 又被打成落盘时刻，
//! 导致 turn 末尾的 goal marker、后台即时落的通知在 created_at 排序后倒挂到 assistant
//! 之前。收归后所有 message 从同一条串行流按发生顺序 append，物理序 = emit 序 = 逻辑序。
//!
//! 两路职责分离（与 sink 同步闭包约束对齐）：
//! - **累积（sink 端，纯内存）**：[`RunPersister::observe`] 在 sink 闭包里被每个 Event 喂，
//!   只更新内存累积器（[`AssistantAccumulator`] 段 + [`NestedAccumulator`] 子过程），临界区 O(1)。
//! - **落盘（agent_loop 主体，async 安全点）**：[`RunPersister::flush_segment`]（drain 边界 /
//!   Step 边界）/ [`RunPersister::finish`]（run 收尾）持锁取出待落内容、`drop(guard)` 后再写盘。
//!
//! partial sidecar 走 actor 模式（仿 [`crate::recorder::Recorder`]）：sink 端只 `try_send`
//! 一个 fragment，后台 task 实时 fsync 每帧——既不阻塞 agent_loop 热路径，崩溃精度又与
//! recorder 一致（SIGKILL 窗口仅 channel 内瞬时帧）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use protocol::{Event, EventPayload};
use tokio::sync::mpsc;
use tracing::warn;

use crate::storage::nested::NestedAccumulator;
use crate::storage::sessions::{self, Message, MessageMeta, Role};
use crate::storage::sessions_dir::{self, PartialFragment};
use crate::turn_accumulator::AssistantAccumulator;

/// partial 写入 actor 的命令。
enum PartialCmd {
    /// 追加一帧增量。
    Append(PartialFragment),
    /// drain 边界：已落盘段对应的中间态清零（保留活性锁，下一帧重建文件）。
    Reset,
    /// run 收尾：删除 partial 文件 + 哨兵。
    Delete,
}

/// partial sidecar 的 actor 句柄。sink 端 clone 它做 fire-and-forget 写入。
#[derive(Clone)]
struct PartialActor {
    tx: mpsc::UnboundedSender<PartialCmd>,
}

impl PartialActor {
    /// 起一个后台 task：持 [`sessions_dir::PartialLiveGuard`] 活性锁，串行消费命令实时落盘。
    fn spawn(data_dir: PathBuf, session_id: String, msg_id: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<PartialCmd>();
        tokio::spawn(async move {
            // 活性锁：run 存活期间排他持有，恢复扫描据此跳过「活 run 正在写」的 partial
            // （架构 §4.9.3 恢复边界）。拿不到锁仅降级——丢的是误折叠防护，不是数据。
            let _live =
                sessions_dir::PartialLiveGuard::acquire(&data_dir, &session_id, &msg_id).ok();
            let mut wrote_text = false;
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    PartialCmd::Append(frag) => {
                        if matches!(frag, PartialFragment::Text { .. }) {
                            wrote_text = true;
                        }
                        if let Err(e) =
                            sessions_dir::append_partial(&data_dir, &session_id, &msg_id, &frag)
                        {
                            warn!(error = %e, msg_id = %msg_id, "append_partial 失败");
                        }
                    }
                    PartialCmd::Reset => {
                        wrote_text = false;
                        let _ = sessions_dir::clear_partial(&data_dir, &session_id, &msg_id);
                    }
                    PartialCmd::Delete => {
                        let _ = sessions_dir::delete_partial(&data_dir, &session_id, &msg_id);
                    }
                }
            }
            let _ = wrote_text;
        });
        Self { tx }
    }

    fn append(&self, frag: PartialFragment) {
        let _ = self.tx.send(PartialCmd::Append(frag));
    }

    fn reset(&self) {
        let _ = self.tx.send(PartialCmd::Reset);
    }

    fn delete(&self) {
        let _ = self.tx.send(PartialCmd::Delete);
    }
}

/// Run 落盘协调器。`Arc<Mutex<>>` 共享：sink 端做内存累积，agent_loop 主体在安全点落盘。
///
/// 落盘语义（与 desktop 历史 observer 等价，但移进 agent_core）：
/// - **段切分**：drain 边界（插队）/ ModelStep Done / ToolStep 完成时 [`flush_segment`]，
///   把当前累积段落成一条 assistant message，created_at = 段首内容时刻。
/// - **尾段兜底**：run 收尾 [`finish`] 补落最后一段（cancel/fail 时是未达段边界的残留）。
/// - **不变量**：内存累积器 + partial sidecar 永远只描述「尚未写入 session.jsonl」的内容，
///   段落盘成功即清零，cancel / 崩溃恢复都不与已落盘段重复。
///
/// [`flush_segment`]: RunPersister::flush_segment
/// [`finish`]: RunPersister::finish
pub struct RunPersister {
    inner: Arc<Mutex<PersistState>>,
}

struct PersistState {
    data_dir: PathBuf,
    session_id: String,
    /// 当前正在累积的 assistant 段。
    seg: AssistantAccumulator,
    /// 子 NestedRun 过程累积（架构 §4.4.11.8），落盘前 sync 进段的 tool_calls.nested。
    nested: NestedAccumulator,
    /// partial sidecar actor（实时落每帧增量，崩溃兜底）。
    partial: PartialActor,
}

impl RunPersister {
    /// 起一个落盘协调器。`msg_id` 是本 run 的 partial 文件名（恢复时折叠成 assistant）。
    pub fn new(data_dir: PathBuf, session_id: String) -> Self {
        let msg_id = sessions::new_id();
        let partial = PartialActor::spawn(data_dir.clone(), session_id.clone(), msg_id);
        Self {
            inner: Arc::new(Mutex::new(PersistState {
                data_dir,
                session_id,
                seg: AssistantAccumulator::new(),
                nested: NestedAccumulator::default(),
                partial,
            })),
        }
    }

    /// sink 端 clone 一份，在事件闭包里 [`observe`] 每个 Event。
    ///
    /// [`observe`]: RunPersisterHandle::observe
    pub fn handle(&self) -> RunPersisterHandle {
        RunPersisterHandle {
            inner: self.inner.clone(),
        }
    }

    /// 落一条 user message（run 首条 / drain 出的插队 user）。created_at = 调用时刻
    /// （user 输入到达即真实产生时刻）。
    pub fn append_user(
        &self,
        content: String,
        attachments: Vec<common::attachments::MessageAttachment>,
        meta: Option<MessageMeta>,
    ) {
        let st = self.inner.lock().unwrap();
        let msg = Message {
            id: sessions::new_id(),
            role: Role::User,
            content,
            attachments,
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: chrono::Utc::now().timestamp_millis(),
            meta,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        if let Err(e) = sessions::append_message(&st.data_dir, &st.session_id, msg) {
            warn!(error = %e, "append_user 落盘失败");
        }
    }

    /// 段边界：把当前累积段落成一条 assistant message 落盘（无内容则跳过），重置累积器
    /// + partial 中间态。`run_duration_ms` 仅在该段是本 run 最后落盘段时传 `Some`。
    pub fn flush_segment(&self, run_duration_ms: Option<u64>) -> Option<Message> {
        let mut st = self.inner.lock().unwrap();
        st.flush_locked(run_duration_ms)
    }

    /// run 收尾（Done/Suspended）：补落最后一段，落盘后删除 partial 文件。
    pub fn finish(&self, run_duration_ms: Option<u64>) -> Option<Message> {
        let mut st = self.inner.lock().unwrap();
        let msg = st.flush_locked(run_duration_ms);
        st.partial.delete();
        msg
    }

    /// cancel/fail 收尾：补落残留尾段 + 紧跟一条 `Interrupted` marker，删 partial。
    pub fn finish_interrupted(&self) {
        let mut st = self.inner.lock().unwrap();
        st.flush_locked(None);
        let marker = Message {
            id: sessions::new_id(),
            role: Role::Marker,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: chrono::Utc::now().timestamp_millis(),
            meta: Some(MessageMeta::Interrupted),
            subagent_call_id: None,
            run_duration_ms: None,
        };
        if let Err(e) = sessions::append_message(&st.data_dir, &st.session_id, marker) {
            warn!(error = %e, "Interrupted marker 落盘失败");
        }
        st.partial.delete();
    }
}

impl PersistState {
    /// 持锁落当前段：accumulator build 出 message（created_at = 段首内容时刻），append，
    /// 重置段 + partial。无内容返回 None（不落空段）。
    fn flush_locked(&mut self, run_duration_ms: Option<u64>) -> Option<Message> {
        let seg = std::mem::replace(&mut self.seg, AssistantAccumulator::new());
        let nested = std::mem::take(&mut self.nested);
        let mut msg = match seg.build() {
            Some(m) => m,
            None => {
                self.partial.reset();
                return None;
            }
        };
        nested.sync_into(&mut msg.tool_calls);
        msg.run_duration_ms = run_duration_ms;
        if let Err(e) = sessions::append_message(&self.data_dir, &self.session_id, msg.clone()) {
            warn!(error = %e, "assistant 段落盘失败");
        }
        self.partial.reset();
        Some(msg)
    }
}

/// sink 端的累积句柄（clone 自 [`RunPersister::handle`]）。
#[derive(Clone)]
pub struct RunPersisterHandle {
    inner: Arc<Mutex<PersistState>>,
}

impl RunPersisterHandle {
    /// 在 sink 闭包里被每个 Event 喂：纯内存累积 + partial fire-and-forget 写帧。
    pub fn observe(&self, event: &Event) {
        let mut st = self.inner.lock().unwrap();
        // 子 NestedRun 事件单独累积（架构 §4.4.11.8）。
        if let Some(call_id) = event.subagent_call_id.as_deref() {
            st.nested.record(call_id, &event.payload);
            return;
        }
        st.seg.on_event(event);
        // partial 写帧（actor，异步 fsync）。
        if let Some(frag) = partial_fragment_of(&event.payload) {
            st.partial.append(frag);
        }
    }
}

/// 把内容事件投影成 partial 增量帧（与 desktop 历史 PartialFileWriter 映射一致）。
fn partial_fragment_of(payload: &EventPayload) -> Option<PartialFragment> {
    match payload {
        EventPayload::TextDelta { text } => Some(PartialFragment::Text { text: text.clone() }),
        EventPayload::Reasoning { text } => {
            Some(PartialFragment::Reasoning { text: text.clone() })
        }
        EventPayload::ToolCallStarted {
            index, name, input, ..
        } => {
            let args = serde_json::to_string(input)
                .ok()
                .filter(|s| s != "null")
                .unwrap_or_default();
            Some(PartialFragment::ToolCall {
                index: *index as u32,
                name: Some(name.clone()),
                arguments_chunk: args,
            })
        }
        EventPayload::ToolCallDelta {
            index,
            name,
            arguments_delta,
            ..
        } => arguments_delta
            .as_ref()
            .map(|chunk| PartialFragment::ToolCall {
                index: *index as u32,
                name: name.clone(),
                arguments_chunk: chunk.clone(),
            }),
        EventPayload::ToolCallFinished {
            index,
            result,
            duration_ms,
            ..
        } => Some(PartialFragment::ToolResult {
            index: *index as u32,
            result: result.clone(),
            duration_ms: *duration_ms,
        }),
        _ => None,
    }
}
