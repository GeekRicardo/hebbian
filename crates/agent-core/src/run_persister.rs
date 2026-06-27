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
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::storage::nested::NestedAccumulator;
use crate::storage::sessions::{self, Message, MessageMeta, Role};
use crate::storage::sessions_dir::{self, PartialFragment};
use crate::turn_accumulator::AssistantAccumulator;

/// partial 写入 actor 的命令。
enum PartialCmd {
    /// 追加一帧增量（fire-and-forget，热路径不等 ack）。
    Append(PartialFragment),
    /// drain 边界：已落盘段对应的中间态清零（保留活性锁，下一帧重建文件）。处理完 ack。
    Reset(oneshot::Sender<()>),
    /// run 收尾：删除 partial 文件 + 哨兵。处理完 ack。
    Delete(oneshot::Sender<()>),
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
                    // Reset/Delete 排在该段所有 Append 之后串行处理——处理完才 ack，调用方
                    // （flush_segment/finish）锁外 await 这个 ack，由此「jsonl 段已落 + partial
                    // 已清理」建立 happens-before：进程随后退出 / 下次 load 时 partial 不会残留
                    // 整段被重复折成 Interrupted（§4.9.5 修订，#5）。
                    PartialCmd::Reset(ack) => {
                        wrote_text = false;
                        let _ = sessions_dir::clear_partial(&data_dir, &session_id, &msg_id);
                        let _ = ack.send(());
                    }
                    PartialCmd::Delete(ack) => {
                        let _ = sessions_dir::delete_partial(&data_dir, &session_id, &msg_id);
                        let _ = ack.send(());
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

    /// 投 Reset 命令，返回 ack receiver。actor 清完 partial 中间态后 ack；send 失败
    /// （actor 已退）时 receiver 立即 Err，调用方 await 不阻塞、降级继续。
    fn reset(&self) -> oneshot::Receiver<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let _ = self.tx.send(PartialCmd::Reset(ack_tx));
        ack_rx
    }

    /// 投 Delete 命令，返回 ack receiver（语义同 [`reset`](Self::reset)）。
    fn delete(&self) -> oneshot::Receiver<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let _ = self.tx.send(PartialCmd::Delete(ack_tx));
        ack_rx
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
    /// 本 run 最后一条成功落盘的 assistant message。surface 不再自行累积返回值，
    /// 收尾时从这里取（架构 §7.8.3 事件累积归一）——assistant message 的产出点
    /// 收敛到 agent_core 唯一一份，desktop `send_message` 的 `Message` 返回值即取自此。
    last_message: Arc<Mutex<Option<Message>>>,
    /// 本 run 的 assistant message id（= partial 文件名）。模型调用 meta.message_id 用它，
    /// 让 `[model]` 日志 / model_io 把每次模型调用关联到它将产出的那条 assistant 消息。
    msg_id: String,
}

struct PersistState {
    data_dir: PathBuf,
    session_id: String,
    /// 当前正在累积的 assistant 段（drain / step 边界 flush 后 reset）。
    seg: AssistantAccumulator,
    /// 子 NestedRun 过程累积（架构 §4.4.11.8），落盘前 sync 进段的 tool_calls.nested。
    nested: NestedAccumulator,
    /// 全 run 累加器（不随段 reset）：finish 时 build 出「本 run 完整 assistant」写进
    /// [`RunPersister::last_message`] 给 surface 返回值用。与分段落盘 `seg` 并行——jsonl
    /// 要分段（插队 user 插在段间，§4.9.5），但 surface 返回值要完整的一轮 assistant
    /// （§7.8.3）。等价复刻 desktop 历史「全 run parts + 分段 segment_parts」两份累加器。
    full: AssistantAccumulator,
    full_nested: NestedAccumulator,
    /// partial sidecar actor（实时落每帧增量，崩溃兜底）。
    partial: PartialActor,
    /// 本 run 最后一条成功落盘的 assistant 段 id。run 收尾耗时徽章只盖本 run 末段——
    /// 但末段可能在收尾前已被中间 flush 预落（goal/Stop-hook 续跑判定要先 flush 让 marker
    /// 排在 assistant 之后），此时收尾 flush 无新内容，[`RunPersister::finish`] 据此 id
    /// 回填耗时，避免徽章丢失或误盖到中间段（中间 flush 一律不带耗时）。
    last_segment_id: Option<String>,
    /// 共享给 [`RunPersister::last_message`] 的句柄：finish 时写 full.build()。
    last_message: Arc<Mutex<Option<Message>>>,
}

/// 只读句柄：surface 收尾时读取本 run 最后落盘的 assistant message（[`RunPersister`]
/// 在段边界 / run 收尾写入）。clone 进 [`crate::harness::RunHandle`] 随事件流带出。
#[derive(Clone, Default)]
pub struct LastMessageHandle {
    inner: Arc<Mutex<Option<Message>>>,
}

impl LastMessageHandle {
    /// 取出当前记录的最后落盘 message（clone）。run 无内容产出时为 `None`。
    pub fn get(&self) -> Option<Message> {
        self.inner.lock().unwrap().clone()
    }
}

impl RunPersister {
    /// 起一个落盘协调器。`msg_id` 是本 run 的 partial 文件名（恢复时折叠成 assistant）。
    pub fn new(data_dir: PathBuf, session_id: String) -> Self {
        let msg_id = sessions::new_id();
        let partial =
            PartialActor::spawn(data_dir.clone(), session_id.clone(), msg_id.clone());
        let last_message: Arc<Mutex<Option<Message>>> = Arc::new(Mutex::new(None));
        Self {
            inner: Arc::new(Mutex::new(PersistState {
                data_dir,
                session_id,
                seg: AssistantAccumulator::new(),
                nested: NestedAccumulator::default(),
                full: AssistantAccumulator::new(),
                full_nested: NestedAccumulator::default(),
                partial,
                last_segment_id: None,
                last_message: last_message.clone(),
            })),
            last_message,
            msg_id,
        }
    }

    /// 本 run 的 assistant message id（partial 文件名）。供模型调用 meta.message_id 关联。
    pub fn msg_id(&self) -> &str {
        &self.msg_id
    }

    /// surface 收尾读「本 run 完整 assistant message」的只读句柄（架构 §7.8.3）。
    pub fn last_message_handle(&self) -> LastMessageHandle {
        LastMessageHandle {
            inner: self.last_message.clone(),
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
    /// + partial 中间态。**不盖 run 耗时**——run 耗时徽章只该出现在本 run 最后一段，由
    /// [`finish`] 统一负责（§4.9.5）。段落盘只记录 `last_segment_id` 供 finish 回填。
    ///
    /// async：落 jsonl 后 await partial Reset 的 ack，建立「段已落 + partial 已清」的
    /// happens-before（#5），避免进程随后退出时 partial 残整段被重复折成 Interrupted。
    ///
    /// [`finish`]: RunPersister::finish
    pub async fn flush_segment(&self) -> Option<Message> {
        let (msg, ack) = {
            let mut st = self.inner.lock().unwrap();
            st.flush_locked()
        };
        // 锁外等 actor 清完 partial 中间态（actor 退出则立即 Err，降级继续）。
        let _ = ack.await;
        msg
    }

    /// run 收尾（Done/Suspended）：补落最后一段，build 完整一轮 assistant 供 surface
    /// 返回值（架构 §7.8.3），删除 partial 文件。`run_duration_ms` 盖到本 run 最后一段：
    /// 收尾 flush 有新内容则盖新段；无新内容（末段已被中间 flush 预落）则按 `last_segment_id`
    /// 回填那条已落盘段——保证耗时徽章只在末段、不丢不误盖（§4.9.5）。
    ///
    /// async：await partial Delete 的 ack，建立「段已落 + partial 已删」的 happens-before（#5）。
    pub async fn finish(&self, run_duration_ms: u64) -> Option<Message> {
        let (msg, reset_ack, delete_ack) = {
            let mut st = self.inner.lock().unwrap();
            let (msg, reset_ack) = st.flush_locked();
            match &msg {
                // 收尾 flush 落了新末段：直接回填它。
                Some(m) => {
                    if let Err(e) = sessions::set_message_run_duration(
                        &st.data_dir,
                        &st.session_id,
                        &m.id,
                        run_duration_ms,
                    ) {
                        warn!(error = %e, "run 耗时回填新末段失败");
                    }
                }
                // 收尾无新段（末段已预落）：回填已落盘的 last_segment_id。
                None => {
                    if let Some(seg_id) = st.last_segment_id.clone() {
                        if let Err(e) = sessions::set_message_run_duration(
                            &st.data_dir,
                            &st.session_id,
                            &seg_id,
                            run_duration_ms,
                        ) {
                            warn!(error = %e, "run 耗时回填末段失败");
                        }
                    }
                }
            }
            st.capture_full_message(Some(run_duration_ms));
            let delete_ack = st.partial.delete();
            (msg, reset_ack, delete_ack)
        };
        // 锁外等 actor 串行处理完 Reset（flush_locked 投的）再处理 Delete——两个 ack
        // 都到，保证返回前 partial 已彻底删除（#5 happens-before）。
        let _ = reset_ack.await;
        let _ = delete_ack.await;
        // 返回值带上耗时（surface 透传用）：build 时 flush_locked 不盖，这里补上。
        msg.map(|mut m| {
            m.run_duration_ms = Some(run_duration_ms);
            m
        })
    }

    /// cancel/fail 收尾：补落残留尾段 + 紧跟一条 `Interrupted` marker，删 partial。
    /// async：await partial Delete 的 ack（#5 happens-before，避免残 partial 二次折叠）。
    pub async fn finish_interrupted(&self) {
        let (reset_ack, delete_ack) = {
            let mut st = self.inner.lock().unwrap();
            let (_msg, reset_ack) = st.flush_locked();
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
            let delete_ack = st.partial.delete();
            (reset_ack, delete_ack)
        };
        let _ = reset_ack.await;
        let _ = delete_ack.await;
    }
}

impl PersistState {
    /// 持锁落当前段：accumulator build 出 message（created_at = 段首内容时刻），append，
    /// 重置段 + partial。无内容返回 `(None, ack)`（不落空段，仍清 partial 中间态）。**不盖
    /// run 耗时**——记录本段 id 到 `last_segment_id`，由 [`RunPersister::finish`] 统一把 run
    /// 耗时回填到本 run 末段。返回 partial Reset 的 ack receiver，调用方锁外 await 它建立
    /// 「jsonl 段已落 + partial 已清」的 happens-before（#5）。
    fn flush_locked(&mut self) -> (Option<Message>, oneshot::Receiver<()>) {
        let seg = std::mem::replace(&mut self.seg, AssistantAccumulator::new());
        let nested = std::mem::take(&mut self.nested);
        let mut msg = match seg.build() {
            Some(m) => m,
            None => {
                let ack = self.partial.reset();
                return (None, ack);
            }
        };
        nested.sync_into(&mut msg.tool_calls);
        if let Err(e) = sessions::append_message(&self.data_dir, &self.session_id, msg.clone()) {
            warn!(error = %e, "assistant 段落盘失败");
        }
        self.last_segment_id = Some(msg.id.clone());
        let ack = self.partial.reset();
        (Some(msg), ack)
    }

    /// run 收尾把全 run 累加器 build 成「完整一轮 assistant」写进 last_message，供 surface
    /// 返回值用（架构 §7.8.3）。分段落盘走 `seg`（jsonl 插队分段），完整产出走 `full`。
    fn capture_full_message(&mut self, run_duration_ms: Option<u64>) {
        let full = std::mem::replace(&mut self.full, AssistantAccumulator::new());
        let full_nested = std::mem::take(&mut self.full_nested);
        if let Some(mut msg) = full.build() {
            full_nested.sync_into(&mut msg.tool_calls);
            msg.run_duration_ms = run_duration_ms;
            *self.last_message.lock().unwrap() = Some(msg);
        }
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
        // 子 NestedRun 事件单独累积（架构 §4.4.11.8）：分段 `nested` 与全 run `full_nested`
        // 各喂一份——前者随段落盘进 jsonl，后者随 finish 进 surface 返回值。
        if let Some(call_id) = event.subagent_call_id.as_deref() {
            st.nested.record(call_id, &event.payload);
            st.full_nested.record(call_id, &event.payload);
            return;
        }
        st.seg.on_event(event);
        st.full.on_event(event);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_state::RunState;
    use crate::storage::sessions;
    use protocol::RunId;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("heb-persister-{name}-{}", sessions::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 回归（#13 续跑中间段误带耗时徽章）：goal/Stop-hook 续跑场景——段1 中间 flush、
    /// 段2 收尾 finish。run 耗时徽章（run_duration_ms）必须**只**盖在末段（段2），中间段
    /// （段1）不带。修前 flush_segment(Some(elapsed)) 给每个中间段都盖累计耗时。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_duration_only_on_last_segment() {
        let dir = temp_dir("dur-last-seg");
        let session = sessions::create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let sid = session.id.clone();

        let persister = RunPersister::new(dir.clone(), sid.clone());
        let handle = persister.handle();
        let state = RunState::new(RunId::new());

        // 段1：模型产出一段文本 → 中间 flush（模拟 goal NotYet 续跑前先落段排 marker）。
        handle.observe(&state.event(EventPayload::TextDelta { text: "第一轮".into() }));
        let seg1 = persister.flush_segment().await.expect("段1 应落盘");

        // 段2：续跑后再产出一段 → run 收尾 finish 盖耗时。
        handle.observe(&state.event(EventPayload::TextDelta { text: "第二轮".into() }));
        let seg2 = persister.finish(1234).await.expect("段2 应落盘");

        // finish 返回的末段带耗时（surface 透传）。
        assert_eq!(seg2.run_duration_ms, Some(1234), "finish 返回的末段应带耗时");

        // 落盘 jsonl 的事实校验：段1 无耗时，段2 有。
        let loaded = sessions::load(&dir, &sid).unwrap();
        let m1 = loaded
            .messages
            .iter()
            .find(|m| m.id == seg1.id)
            .expect("段1 应在 jsonl");
        let m2 = loaded
            .messages
            .iter()
            .find(|m| m.id == seg2.id)
            .expect("段2 应在 jsonl");
        assert_eq!(m1.content, "第一轮");
        assert_eq!(m2.content, "第二轮");
        assert_eq!(m1.run_duration_ms, None, "中间段不该带 run 耗时徽章（#13）");
        assert_eq!(m2.run_duration_ms, Some(1234), "末段应带 run 耗时徽章");
    }

    /// 回归（#13 边界）：goal achieved/impossible 不续跑——末段在裁决前已被中间 flush 预落，
    /// 收尾 finish 此刻无新内容可 flush。耗时必须按 last_segment_id **回填**到那条已落盘的
    /// 末段，而不是丢失。修前 finish 的 flush_locked 返回 None → 末段永远拿不到耗时。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_duration_backfilled_when_last_segment_preflushed() {
        let dir = temp_dir("dur-backfill");
        let session = sessions::create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let sid = session.id.clone();

        let persister = RunPersister::new(dir.clone(), sid.clone());
        let handle = persister.handle();
        let state = RunState::new(RunId::new());

        // 唯一一段文本 → flush（模拟 goal achieved：裁决前先 flush 让 marker 排其后）。
        handle.observe(&state.event(EventPayload::TextDelta { text: "唯一段".into() }));
        let seg = persister.flush_segment().await.expect("段应落盘");

        // 收尾 finish：累积器已空，无新段——耗时应回填到已落盘的 seg。
        let finished = persister.finish(999).await;
        assert!(finished.is_none(), "收尾无新段时 finish 返回 None");

        let loaded = sessions::load(&dir, &sid).unwrap();
        let m = loaded
            .messages
            .iter()
            .find(|m| m.id == seg.id)
            .expect("段应在 jsonl");
        assert_eq!(
            m.run_duration_ms,
            Some(999),
            "末段已预落时，run 耗时应回填到它（#13 边界，不丢徽章）"
        );
    }

    /// 当前 session 的 partial 目录下还残留几个 `.partial.jsonl` 文件（不含 `.live`/`.lock` 哨兵）。
    fn count_partial_files(dir: &std::path::Path, sid: &str) -> usize {
        let pdir = crate::storage::sessions_dir::partial_dir(dir, sid);
        let Ok(entries) = std::fs::read_dir(&pdir) else {
            return 0;
        };
        entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".partial.jsonl")
            })
            .count()
    }

    /// 回归（#5 段落盘后异步删 partial 的崩溃窗口）：finish 返回后 partial 文件必须已被
    /// 物理删除——await Delete 的 ack 建立了 happens-before。修前 finish 只把 Delete 投进
    /// actor channel 就返回，进程随后退出 / 下次 load 时 partial 残整段被重复折成 Interrupted。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finish_deletes_partial_before_returning() {
        let dir = temp_dir("finish-del-partial");
        let session = sessions::create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let sid = session.id.clone();

        let persister = RunPersister::new(dir.clone(), sid.clone());
        let handle = persister.handle();
        let state = RunState::new(RunId::new());

        // 产出内容并实时写 partial（actor append）。
        handle.observe(&state.event(EventPayload::TextDelta {
            text: "流式内容".into(),
        }));
        // 给 actor 一点时间把 append 落盘（partial 文件确实建起来）。
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            count_partial_files(&dir, &sid),
            1,
            "run 进行中应有一个活 partial 文件"
        );

        // 收尾：finish 必须 await Delete ack，返回时 partial 已删（happens-before）。
        let _ = persister.finish(100).await;
        assert_eq!(
            count_partial_files(&dir, &sid),
            0,
            "finish 返回后 partial 必须已物理删除（#5 happens-before，否则会被二次折叠）"
        );
    }
}
