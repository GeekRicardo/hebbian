//! 对话历史持久化 (rollout v1)。
//!
//! ## 文件格式
//!
//! 落盘到 `<data_dir>/sessions/<YYYY-MM-DD>/<session_id>.jsonl`，
//! 每行一个 [`RolloutLine`]：
//!
//! - 第 1 行 **必须**是 [`RolloutLine::Meta`]：身份字段 (id / source / created_at / forked_from)
//!   + 起始时刻可变状态的快照 (title / provider_id / model / reasoning / ...)
//! - 之后任意顺序：
//!   - [`RolloutLine::Message`] — 用户 / 助手 / marker 消息（追加）
//!   - [`RolloutLine::MetaUpdate`] — 可变状态的增量补丁（rename、token_stats、…）
//!   - [`RolloutLine::Event`] — 原始 Event 事件流（给 replay 用，**当前版本不写**，读侧跳过）
//!
//! 折叠规则：从空 [`Session`] 开始，按行顺序应用：Meta 设置初始状态，Message 追加，
//! MetaUpdate 按字段 last-wins 覆盖，Event 跳过。
//!
//! ## 写入策略
//!
//! - [`save`]：全量重写（Meta + 所有 Message），相当于"compaction"
//! - [`append_message`] / [`rename`] / [`bump_token_stats`]：append-only，单行追加
//! - [`fork`] / [`truncate_*`]：经由 `save` 重写新文件
//!
//! ## 兼容旧 `.json`
//!
//! `<id>.json` 是 v0 格式（整 Session 一份 JSON）。`load` 优先读 `.jsonl`，
//! 找不到时回落到 `.json`。任何写操作（save / append / rename）会触发
//! 「读老 json → 写新 jsonl → 老 json 改名 .json.bak」的一次性迁移。

use chrono::{TimeZone, Utc};
use common::attachments::MessageAttachment;
use common::{AppError, AppResult};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::rules::RuleFileState;
use crate::run_mode::RunMode;
use protocol::todo::TodoItem;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Marker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageMeta {
    Switch {
        from_provider: String,
        from_model: String,
        to_provider: String,
        to_model: String,
    },
    Interrupted,
    /// 上下文压缩的分界标记。LLM 看到的 transcript 会跳过此标记之前的所有消息，
    /// 并把 `summary` 作为前情概要注入；标记之后的消息正常参与对话。
    CompactBoundary {
        summary: String,
        before_tokens: usize,
        after_tokens: usize,
    },
    /// 推理参数变化标记（thinking on/off、effort 档位、1M 上下文）。
    /// `None` 表示之前没有 reasoning 配置（沿用模型默认）。
    ReasoningSwitch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<common::reasoning::ReasoningConfig>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<common::reasoning::ReasoningConfig>,
    },
    /// 系统注入的通知（架构 §4.12.5 修订 / 借鉴 CC 2.1 `<task-notification>`）。
    /// 物理 role 仍是 `User`（model API 必须 user/assistant/system 三选一，给 model 看的
    /// 后台事件只能借 user 通道），`content` 是 `<wakeup>...</wakeup>` 等 LLM 可读 XML。
    /// 通过 meta 标记让 surface 区别渲染为系统通知条而非用户气泡，避免视觉污染。
    SystemNotification {
        /// 通知来源类别：`bg_task_finished` / `cron_fired`。
        kind: String,
        /// 关联的后台 task_id（`bg_task_finished` 才有）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// 触发该通知的 tool_call.id；surface 据此把通知卡片关联回触发它的 tool_call。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    /// 本轮后台记忆抽取写入的记忆摘要（架构 §4.14）。抽取在 `RunFinished` 之后异步
    /// 完成，那时本轮 assistant 早已落盘——故这条摘要单独作为 `Role::Marker` 消息
    /// append 到 session.jsonl 末尾，随会话持久化，重启后从同一条 marker 重建渲染。
    /// transcript rebuild 对 `Role::Marker` 走 `_ => {}` 天然跳过，模型看不到它。
    MemoryWrites {
        items: Vec<protocol::MemoryWriteItem>,
    },
    /// `//goal` 一次裁决结果（架构 §4.8.3）。goal judge 在 turn 收尾判 transcript 是否
    /// 满足完成条件后，把这条结果作为 `Role::Marker` append 到 session.jsonl，随会话持久化、
    /// 重启可重建。落在记忆摘要之前（goal 裁决在 turn 结束瞬间、记忆抽取在 RunFinished 之后）。
    /// transcript rebuild 对 `Role::Marker` 走 `_ => {}` 天然跳过，模型看不到它。
    GoalOutcome {
        /// 裁决类型：`set`（刚设目标，由 surface 落）/ `achieved`（达成）/
        /// `impossible`（判不可达）/ `progress`（续跑一轮）；后三者由 agent_loop 裁决时落。
        kind: String,
        /// 该目标的完成条件原文，供 UI 标明是哪个 goal。
        condition: String,
        /// judge 给出的理由 / 还差什么。
        reason: String,
        /// 续跑轮次（仅 `progress` 有意义；终态恒为已累计的最终轮数）。
        iteration: u32,
    },
    /// Stop hook（cargo check / tsc 等后置 verify）一次执行的结果（架构 §4.8.3）。
    /// turn 自然结束跑 Stop hook 后，把结果作为 `Role::Marker` append——让消息流显示
    /// 「跑了哪个 verify、过没过」。transcript rebuild 对 `Role::Marker` 走 `_ => {}` 跳过，
    /// 模型看不到它（verify 失败的修复提示走单独的 `<hook-feedback>` user 消息）。
    HookOutcome {
        /// hook 点位名（当前恒为 `Stop`）。
        event: String,
        /// 执行结论：`passed`（verify 通过）/ `injected`（verify 失败，已注入续跑修复）/
        /// `blocked`（hook 阻断）。
        status: String,
        /// verify 失败 / 阻断时的提示文本（passed 时为空）。
        detail: String,
    },
    /// 机主不活跃时，主对话的某条 HITL（审批 / 提问）被转发到聊天渠道（微信等）的痕迹
    /// （架构 §7.5.1，2026-06-20）。物理 `Role::Marker`，不进 model transcript，仅供
    /// surface 渲染一条「已转发到微信」分隔线小条，让机主回到电脑后知道这条审批/问题
    /// 当时被转发出去、以及在渠道侧的最终处置。`status` 落盘后随渠道回复更新（pending →
    /// resolved），让一条 marker 同时承载「转发」与「回复结果」两个事实。
    ChannelForward {
        /// 渠道 id（`wechat` 等）。
        channel: String,
        /// 转发的是审批还是提问。
        kind: ChannelForwardKind,
        /// 处置状态：转发即落 `Pending`，渠道回复到达后原地更新为 `Resolved`。
        status: ChannelForwardStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelForwardKind {
    Approval,
    Question,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChannelForwardStatus {
    /// 已转发，等待机主在渠道侧回复。
    Pending,
    /// 机主在渠道侧已处置，`outcome` 是人话结论（如「已通过」「已拒绝」「选了：右上角」）。
    Resolved { outcome: String },
}

impl MessageMeta {
    /// 是否是给 surface 区别渲染用的 system-notification（wakeup / cron 等）。
    /// 调用方：MessageBubble 渲染、jsonl rebuild 决定是否纳入 model transcript（纳入）。
    pub fn is_system_notification(&self) -> bool {
        matches!(self, MessageMeta::SystemNotification { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// 这次调用以失败收场（执行错误 / 入参解析失败 / 被拒 / Bash 退出码非 0）。
    /// false 时不落盘，老 jsonl 向下兼容。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// 子 NestedRun（subagent）的过程：子文本 / 子 reasoning / 子工具调用，按时序（架构 §4.4.11.8）。
    /// 仅 `name=="Task"` 的调用可能非空。run 结束时 surface 把累积的子事件（带
    /// `subagent_call_id == 本 call id`）写进这里，随父 message 落**主** session.jsonl，
    /// 前端 streaming / 重建都从此渲染嵌套区。老 jsonl 无此字段，serde default 给空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested: Vec<MessagePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
    },
    /// 模型的思维链 / 推理过程。落盘后 UI 以折叠块呈现。
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        arguments: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// 这次调用以失败收场（执行错误 / 入参解析失败 / 被拒 / Bash 退出码非 0）。
        /// 前端用它把状态点渲染成红色。false 时不落盘，老 jsonl 向下兼容。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<MessageToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<MessagePart>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MessageMeta>,
    /// 这条消息源自某次 Task subagent 子 NestedRun（架构 §4.4.11.8）。
    /// 值 = 父侧 Task 工具调用的 call_id。`transcript::from_session` 重建父
    /// transcript 时跳过 `Some(_)` 的 Message——子事件已经在子 session.jsonl 自成一份，
    /// 父只需要 Task 工具调用的 ToolResult（子终态文本）即可。
    /// 老 jsonl 没这个字段，serde default 给 None，不破坏向下兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_call_id: Option<String>,
    /// 本 Run 总耗时（毫秒）。仅落在「一个 Run 结束时写盘的最后一条 assistant
    /// message」上，其余消息（user / marker / 子段 assistant）为 `None`。
    /// 渲染层据此在该气泡操作行显示「· 1.8s」。随 jsonl 持久化，重启后仍可见。
    /// 老 jsonl 无此字段，serde default 给 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_duration_ms: Option<u64>,
}

impl Message {
    /// 这条消息是不是 system-notification（wakeup / cron 等系统注入的通知）。
    /// 渲染时区别于普通用户气泡，rebuild 模型 transcript 时仍纳入（model 应当看见后台事件）。
    pub fn is_system_notification(&self) -> bool {
        self.meta
            .as_ref()
            .is_some_and(MessageMeta::is_system_notification)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// 对话工作目录。`None` = 用全局默认（通常 `~/`）。
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// 对话开始时锁定的允许路径覆盖。`None` = 用全局默认。
    /// 一旦本对话发出过 user message，UI 不再允许从这里删除条目（否则会破坏
    /// 已基于这组路径建立的 prompt cache + 已生效的工具行为）。
    /// 运行时新增的允许路径请使用 `runtime_allowed_paths` / `pending_runtime_allowed_paths`。
    #[serde(default)]
    pub allowed_paths: Option<Vec<PathBuf>>,
    /// 对话开始之后追加的允许路径，且已经通过上一条 user message 中的
    /// `<workspace-update>` 段通知过模型。仅作 `allows()` 判定用，不进 system prompt。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_allowed_paths: Vec<PathBuf>,
    /// 对话开始之后追加、还没通知模型的允许路径。下次 send_message 时
    /// `Workspace::take_pending_announcement` 会 drain 它们注入到 user message 头部，
    /// 然后 surface 端把它们移到 `runtime_allowed_paths`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_runtime_allowed_paths: Vec<PathBuf>,
    /// 对话启用的非内置工具（来自 `tool_manifest`）。`None` = 用全局默认。
    #[serde(default)]
    pub enabled_tools: Option<Vec<String>>,
    /// 对话使用的 skill 目录列表。`None` = 用全局默认。
    #[serde(default)]
    pub skill_dirs: Option<Vec<PathBuf>>,
    /// 推理 / thinking 配置。`None` = 沿用模型默认（多数模型默认关闭）。
    /// 在 desktop 选模型时，对支持 thinking 的模型（claude-opus-4 / gpt-5 等）
    /// 默认填 `Some({enabled: true, effort: Extra})`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<common::reasoning::ReasoningConfig>,
    /// 整个对话累计的 token 用量。每次 run 结束由 surface 累加进 session.json，
    /// 用来在输入框旁的 TokenStatsPanel 直接展示，无需重跑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_stats: Option<TokenStats>,
    /// 创建该 session 的 surface："desktop" / "cli" / 其他。
    /// 老 `.json` 没这个字段，反序列化时为 `None`。读 jsonl 时从 Meta 行的 `source` 字段填入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 创建该 session 时绑定的 workspace/project。老对话为 None；UI 可以用 workdir 兜底归类。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// 本对话的 [`RunMode`]（架构 §4.4.3 / §8）。Desktop mode chip 切换时持久化到这里，
    /// chat 路径每次 send_message 从这里取真值传给 SessionConfig。
    /// 老 jsonl 无此字段反序列化默认 [`RunMode::Default`]（老值 AskBeforeEdits / EditAutomatically 经 serde alias 映射）。
    #[serde(default)]
    pub run_mode: RunMode,
    /// 启用的全局规则文件路径列表。None = 继承全局默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_rules: Option<Vec<PathBuf>>,
    /// 项目规则文件开关状态。None = 自动发现（workdir 下的默认 on）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_files: Option<Vec<RuleFileState>>,
    /// TodoWrite 工具维护的当前 todo 列表（架构 §4.4.6）。
    /// 模型每次调 TodoWrite 时整列表覆盖；落盘走 [`MetaUpdate::todos`]，
    /// 重启可恢复并在右侧 sidebar 第 3 个 tab 展示。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<TodoItem>,
    /// PlanMode 下 ExitPlanMode 通过审批前后写入的"当前 plan"绝对路径
    /// （形如 `~/.hebbian/sessions/<sid>/plans/plan-<ts>.md`）。
    /// `None` 表示当前 session 没有待审批 / 已审批的 plan。历史 plan 仍可在
    /// 目录里列出，这里只标当前活跃那份（架构 §4.4.5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_plan: Option<String>,
    /// 会话当前的完成条件目标（架构 §4.8.3）。None = 无目标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<ActiveGoal>,
    /// 进入 PlanMode 之前的 [`RunMode`]，用于 ExitPlanMode 审批通过后切回去
    /// （架构 §4.4.5）。默认 `None`——表示从未进过 PlanMode；如果未来切到
    /// PlanMode 时找不到 pre_plan_mode 则回落到 `Default`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_plan_mode: Option<RunMode>,
    /// 上一个 Run 非正常结束（截断 / 拒答 / 拦截 / 请求失败 / 轮数超限）留下的续作入口
    /// （架构 §4.3）。`None` = 上一轮正常完成。重启可恢复，前端据此渲染 ContinueBar。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continue: Option<PendingContinue>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 对话级 token 累计。
///
/// `input_tokens` / `output_tokens` 与 provider 账单对齐；
/// `cache_read_tokens` / `cache_creation_tokens` **已包含在** `input_tokens` 内，
/// 单独展示给用户评估缓存命中率。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// run 总轮数（含 failed / cancelled）
    #[serde(default)]
    pub run_count: u64,
    /// 最新一次 run 的用量快照（上面的字段是整个对话累计）。
    /// 供 TokenStatsPanel hover 展开「最新一次」的缓存命中明细。
    #[serde(default)]
    pub last_input_tokens: u64,
    #[serde(default)]
    pub last_output_tokens: u64,
    #[serde(default)]
    pub last_cache_read_tokens: u64,
    #[serde(default)]
    pub last_cache_creation_tokens: u64,
    /// 与 `last_input_tokens` 配对的本地估算值：采到那次服务端真值时，
    /// 同一份 transcript 用 [`crate::context::budget::estimate_transcript_tokens`]
    /// 估出的 token 数。两者的比值用来校准本地估算（见 `calibrated_transcript_tokens`）。
    #[serde(default)]
    pub last_estimated_tokens: u64,
}

impl TokenStats {
    pub fn accumulate(&mut self, delta: TokenStats) {
        self.input_tokens += delta.input_tokens;
        self.output_tokens += delta.output_tokens;
        self.cache_read_tokens += delta.cache_read_tokens;
        self.cache_creation_tokens += delta.cache_creation_tokens;
        self.run_count += delta.run_count;
        // last_* 覆盖为本次 delta（一次模型请求）的用量，供 hover 看最新一次。
        self.last_input_tokens = delta.input_tokens;
        self.last_output_tokens = delta.output_tokens;
        self.last_cache_read_tokens = delta.cache_read_tokens;
        self.last_cache_creation_tokens = delta.cache_creation_tokens;
        self.last_estimated_tokens = delta.last_estimated_tokens;
    }
}

/// per-turn 累加一次模型请求的 token 用量到 session.token_stats（append 一行 MetaUpdate）。
/// agent_loop 每次模型请求完成时调用，让 cache 指示器在 run 进行中就能实时刷新。
/// 失败不传染（拿不到 session / 写盘失败都不该影响主请求结果）。
pub fn bump_token_stats(data_dir: &Path, session_id: &str, delta: TokenStats) {
    let _ = update_meta(data_dir, session_id, |session| {
        let mut stats = session.token_stats.unwrap_or_default();
        stats.accumulate(delta);
        session.token_stats = Some(stats);
        Ok(())
    });
}

fn default_stream() -> bool {
    true
}

/// 会话当前的「完成条件」目标（架构 §4.8.3 / §8）。模型每次想结束 turn 时
/// 由 judge LLM 判 transcript 是否满足 `condition`，没满足就注入续跑。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGoal {
    /// 用户用 `//goal <条件>` 设的完成条件原文。
    pub condition: String,
    /// 设目标的时间戳（ms）。
    pub created_at: i64,
    /// 已自动续跑轮数（展示 / 日志用，不做上限）。
    pub iterations: u32,
    /// judge 上次判定「还差什么」；首次设目标时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
    /// 刚设目标待落「目标已设」marker 的一次性标志（同 [`PendingContinue`] 的一次性模式）。
    /// set_active_goal 设目标时置 true；agent_loop run 启动时（`Goal set` user 消息已落盘后）
    /// 落一条 `GoalOutcome{kind:"set"}` marker 并清回 false——保证 set marker 物理排在
    /// 触发它的 user 消息之后，且与裁决 marker 走同一条 agent_core 串行落盘流，不靠前端抢落。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_set_marker: bool,
}

/// 一次「非正常结束」留下的续作入口（架构 §4.3 / §7.3）。落在 session 状态里，
/// 重启后 ContinueBar 仍可见。正常完成的下一个 Run 会清空它。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingContinue {
    /// 触发时刻（Unix epoch ms）。
    pub at: i64,
    /// 为什么中断——决定 `AutoByReason` 策略下点 continue 走续写还是重发。
    pub kind: ContinueKind,
    /// 给用户看的一句话（toast 与 ContinueBar 共用）。
    pub message: String,
}

/// 中断原因分类（架构 §4.11.4 的 [`FinishReason`] + run 级失败的并集）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinueKind {
    /// 被 token 上限截断——`AutoByReason` 默认续写。
    Truncated,
    /// 模型拒答。
    Refused,
    /// 被内容安全策略拦截。
    Filtered,
    /// 模型请求失败（HTTP/网络/JSON）——`AutoByReason` 默认重发。
    NetworkError,
    /// 工具调用轮数超限。
    MaxIterations,
    /// 其它未归类。
    Other,
}

/// [`ContinueKind`] 的稳定小写串，用于事件 dedup_key 传递给 surface（与前端
/// `ContinueKind` 字面量一致）。
pub fn continue_kind_str(kind: ContinueKind) -> &'static str {
    match kind {
        ContinueKind::Truncated => "truncated",
        ContinueKind::Refused => "refused",
        ContinueKind::Filtered => "filtered",
        ContinueKind::NetworkError => "network_error",
        ContinueKind::MaxIterations => "max_iterations",
        ContinueKind::Other => "other",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub date: String,
    /// 创建该 session 的 surface（前端 Sidebar 用于显示徽章）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 创建该 session 时绑定的 workspace/project。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// 对话工作目录，用于项目列表兜底匹配老会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    /// session.jsonl 的磁盘绝对路径，供前端 @ 引用对话时直接拿路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub meta: SessionMeta,
    pub snippet: Option<String>,
    pub matched_in: &'static str,
}

// ════════════════════════════════════════════════════════════════════════════
// rollout v1：jsonl 行类型
// ════════════════════════════════════════════════════════════════════════════

/// 当前 rollout schema 版本号。读侧遇到不认识的版本应该报错而不是静默降级。
pub const ROLLOUT_SCHEMA: u32 = 1;

/// 新会话的默认标题。session_titler 用它来判断是否需要自动生成标题——
/// 当前 title 还等于这个值才触发，以避免覆盖用户的手动重命名。
pub const DEFAULT_TITLE: &str = "新对话";

/// jsonl 单行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RolloutLine {
    /// 第 1 行：身份 + 起始时刻状态快照
    Meta(RolloutMeta),
    /// 用户 / 助手 / marker 消息
    Message(Message),
    /// 可变字段的增量补丁
    MetaUpdate(MetaUpdate),
    /// 原始 protocol::Event 流（给 replay 用，当前不写）。
    /// 用 `Value` 透传以避免 protocol crate 依赖循环。
    Event(Value),
}

/// 第 1 行：rollout meta 头。
///
/// 字段分两类：
/// - **不可变身份**：`schema` / `id` / `source` / `created_at` / `forked_from`
/// - **可变快照**：title / provider_id / model / 等等。后续 [`MetaUpdate`] 行可以覆盖。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutMeta {
    pub schema: u32,
    pub id: String,
    /// 创建该 session 的 surface："desktop" / "cli" / 未来其他
    pub source: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,

    pub title: String,
    pub provider_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_paths: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_allowed_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_runtime_allowed_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_dirs: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<common::reasoning::ReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_stats: Option<TokenStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// [`RunMode`] 起始快照。老 RolloutMeta 无此字段反序列化为 `Default`。
    #[serde(default)]
    pub run_mode: RunMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_rules: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_files: Option<Vec<RuleFileState>>,
    /// TodoWrite 维护的 todo 列表当前快照（架构 §4.4.6）。save 全量重写时把
    /// 在 jsonl 末尾累积的 meta_update 行折叠回这里，避免下次 load 丢失。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<TodoItem>,
    /// PlanMode 下当前活跃 plan 的绝对路径（架构 §4.4.5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<ActiveGoal>,
    /// 进入 PlanMode 之前的 RunMode；ExitPlanMode 审批通过后切回去用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_plan_mode: Option<RunMode>,
    /// 上一个 Run 非正常结束留下的续作入口（架构 §4.3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continue: Option<PendingContinue>,
}

/// 可变字段补丁。每个 `Some(_)` 字段都会按 last-wins 覆盖到最终 [`Session`]。
/// 当前 schema 不支持「清空成 None」语义；要清空请走 [`save`] 全量重写。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaUpdate {
    /// 写入时间戳（ms）。让 fold 能精确算出 updated_at。
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_paths: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_allowed_paths: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_runtime_allowed_paths: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_dirs: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<common::reasoning::ReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_stats: Option<TokenStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// 切换 [`RunMode`] 时下发的补丁。`None` 表示本次更新不动 RunMode。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_mode: Option<RunMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_rules: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_files: Option<Vec<RuleFileState>>,
    /// TodoWrite 整列表覆盖（架构 §4.4.6）。空 vec 也是有效值（清空 todo）。
    /// 用 `Option<Vec<_>>` 区分"本次更新不动 todos"和"清空 todos"。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
    /// ExitPlanMode 落盘 plan 后写入"当前 plan"绝对路径（架构 §4.4.5）。
    /// `None` = 本次更新不动；要清空走 [`clear_active_plan`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_plan: Option<String>,
    /// 显式清空 `active_plan`（plan revert / session reset 等场景）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_active_plan: bool,
    /// 设置 / 更新会话目标。`None` = 本次更新不动；要清空走 `clear_active_goal`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<ActiveGoal>,
    /// 显式清空 `active_goal`（达成 / 判不可能 / 用户 //goal clear）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_active_goal: bool,
    /// 进入 PlanMode 时记录的"切换前 RunMode"。审批通过后据此切回去。
    /// `None` = 本次更新不动；要清空走 [`clear_pre_plan_mode`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_plan_mode: Option<RunMode>,
    /// 显式清空 `pre_plan_mode`（ExitPlanMode 审批通过、切回非 PlanMode 后调用）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_pre_plan_mode: bool,
    /// 上一个 Run 非正常结束留下的续作入口（架构 §4.3）。
    /// `None` = 本次更新不动；要清空走 [`clear_pending_continue`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continue: Option<PendingContinue>,
    /// 显式清空 `pending_continue`（下一个 Run 正常完成 / 用户点了 continue 后调用）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_pending_continue: bool,
}

// ════════════════════════════════════════════════════════════════════════════
// 内部 helpers
// ════════════════════════════════════════════════════════════════════════════

fn now() -> i64 {
    Utc::now().timestamp_millis()
}

/// 架构 §4.9.3：session_id 形如 `{yyyymmddHHmm}-{shortUuid}`。
pub fn new_id() -> String {
    super::sessions_dir::new_session_id()
}

fn date_string(ts_ms: i64) -> String {
    Utc.timestamp_millis_opt(ts_ms)
        .single()
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".into())
}

fn root_dir(data_dir: &Path) -> PathBuf {
    let dir = data_dir.join("sessions");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 新布局：`~/.hebbian/sessions/<id>/session.jsonl`。
fn new_layout_path(data_dir: &Path, id: &str) -> PathBuf {
    super::sessions_dir::session_jsonl_path(data_dir, id)
}

/// 判断该 id 在新布局是否存在。
fn new_layout_exists(data_dir: &Path, id: &str) -> bool {
    new_layout_path(data_dir, id).exists()
}

/// 当前进程的 source 标识。Surface 显式调 [`set_default_source`] 覆盖；
/// CLI 在启动时设为 "cli"，desktop / 测试默认 "desktop"。
fn default_source() -> String {
    DEFAULT_SOURCE
        .get()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "desktop".to_string())
}

static DEFAULT_SOURCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// 设置当前进程默认 `source`。只允许设置一次（OnceLock 语义），第二次调用静默忽略。
/// CLI 在 `main` 起手调 `set_default_source("cli")` 即可。
pub fn set_default_source(source: &str) {
    let _ = DEFAULT_SOURCE.set(source.to_string());
}

/// 罗列所有 session 文件。架构 §4.9.1 目录化布局：
/// `~/.hebbian/sessions/<id>/session.jsonl`。
///
/// 兼容老布局：`~/.hebbian/sessions/<YYYY-MM-DD>/<id>.jsonl` 与
/// 平铺 `~/.hebbian/sessions/<id>.jsonl` / `<id>.json` 也会被收录。
/// 同 id 时优先返回新布局（目录化）文件。
fn all_session_files(data_dir: &Path) -> AppResult<Vec<PathBuf>> {
    let root = root_dir(data_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut new_layout: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::new();
    let mut legacy: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // 跳过「目录名带 `.`」的伪 session 目录——session_id 规范不含 `.`（§4.9.3）。
            // 历史脏数据 `<sid>.model_io/session.jsonl`（被老版本误迁移）也在此过滤掉。
            let dir_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if dir_name.contains('.') {
                continue;
            }
            // 跳过 `rollout-*/` 目录：早期 migrate_legacy_to_new 把孤儿
            // `rollout-<ts>-<uuid>.jsonl`（裸 Event 流、不带 schema header）当成
            // 平铺 legacy session 误迁成 `rollout-*/session.jsonl`，内容仍是裸 Event
            // 解析必失败。这类目录跟 `is_session_file` 的「rollout-」黑名单同义。
            if dir_name.starts_with("rollout-") {
                continue;
            }
            // 新布局：目录名 = session_id，里面有 session.jsonl
            let jsonl = path.join("session.jsonl");
            if jsonl.exists() {
                new_layout.insert(dir_name.to_string(), jsonl);
                continue;
            }
            // 老布局：按日期目录归档的 <id>.jsonl / <id>.json
            for sub in std::fs::read_dir(&path)? {
                let sub = sub?;
                if is_session_file(&sub.path()) {
                    legacy.push(sub.path());
                }
            }
        } else if is_session_file(&path) {
            legacy.push(path);
        }
    }
    let mut all: Vec<PathBuf> = new_layout.into_values().collect();
    // 老布局：同 id 去重（jsonl 优先于 json）
    legacy.sort();
    legacy.dedup_by(|a, b| {
        let a_id = a.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let b_id = b.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if a_id != b_id {
            return false;
        }
        let a_jsonl = a.extension().and_then(|s| s.to_str()) == Some("jsonl");
        let b_jsonl = b.extension().and_then(|s| s.to_str()) == Some("jsonl");
        match (a_jsonl, b_jsonl) {
            (true, false) => {
                *b = a.clone();
                true
            }
            (false, true) => true,
            _ => true,
        }
    });
    all.extend(legacy);
    Ok(all)
}

fn is_session_file(p: &Path) -> bool {
    if !matches!(
        p.extension().and_then(|s| s.to_str()),
        Some("jsonl") | Some("json")
    ) {
        return false;
    }
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // 排除老 CLI 用 `agent_core::Recorder` 直接写的孤儿事件流文件，
    // 文件名形如 `rollout-<ts>-<uuid>.jsonl`，里面是裸 `Event`、不带 schema header。
    if stem.starts_with("rollout-") {
        return false;
    }
    // 排除带「副扩展名」的辅助文件（如 `<sid>.model_io.jsonl`）。session_id 规范
    // 是 `{yyyymmddHHmm}-{shortUuid}` 或 uuid v4，本身不含 `.`，stem 出现 `.`
    // 必然是某种 sidecar，扫进来会导致 legacy migrate 误识别 + read_jsonl 解析失败。
    if stem.contains('.') {
        return false;
    }
    true
}

/// 找一个 id 的 jsonl 文件路径。优先新布局，否则回落到老布局并按需迁移。
///
/// 新布局：`sessions/<id>/session.jsonl`
/// 老布局：`sessions/<YYYY-MM-DD>/<id>.jsonl` 或 平铺 `sessions/<id>.jsonl`
fn find_jsonl(data_dir: &Path, id: &str) -> AppResult<Option<PathBuf>> {
    // 新布局优先
    let new_p = new_layout_path(data_dir, id);
    if new_p.exists() {
        return Ok(Some(new_p));
    }
    // 老布局兼容
    if let Some(old) = find_legacy_jsonl(data_dir, id)? {
        // 一次性迁移到新布局
        let migrated = migrate_legacy_to_new(data_dir, id, &old)?;
        return Ok(Some(migrated));
    }
    Ok(None)
}

/// 老布局 `<YYYY-MM-DD>/<id>.jsonl` 或平铺 `<id>.jsonl`。
fn find_legacy_jsonl(data_dir: &Path, id: &str) -> AppResult<Option<PathBuf>> {
    find_legacy_session_file_with_ext(data_dir, id, "jsonl")
}

fn find_legacy_json(data_dir: &Path, id: &str) -> AppResult<Option<PathBuf>> {
    find_legacy_session_file_with_ext(data_dir, id, "json")
}

fn find_legacy_session_file_with_ext(
    data_dir: &Path,
    id: &str,
    ext: &str,
) -> AppResult<Option<PathBuf>> {
    let root = root_dir(data_dir);
    if !root.exists() {
        return Ok(None);
    }
    let flat = root.join(format!("{id}.{ext}"));
    if flat.exists() {
        return Ok(Some(flat));
    }
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        // 跳过新布局的 <id>/ 目录（new layout 已被 find_jsonl 优先匹配）
        if entry.path().join("session.jsonl").exists() {
            continue;
        }
        let candidate = entry.path().join(format!("{id}.{ext}"));
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// 把老布局 jsonl 一次性迁移到新布局 `sessions/<id>/session.jsonl`，
/// 完成后老文件改名为 `.bak` 留底（不删，避免误操作）。
fn migrate_legacy_to_new(data_dir: &Path, id: &str, legacy: &Path) -> AppResult<PathBuf> {
    let target = new_layout_path(data_dir, id);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 用 rename 优先（同卷直接 mv）；跨卷退回 copy
    if std::fs::rename(legacy, &target).is_err() {
        std::fs::copy(legacy, &target)?;
        let bak = legacy.with_extension("jsonl.bak");
        let _ = std::fs::rename(legacy, &bak);
    }
    tracing::info!(
        from = %legacy.display(),
        to = %target.display(),
        "session 老布局已迁移到新目录"
    );
    Ok(target)
}

/// 新布局路径：始终 `sessions/<id>/session.jsonl`，不再按日期分目录。
fn jsonl_path_for(data_dir: &Path, id: &str, _created_at: i64) -> AppResult<PathBuf> {
    let target = new_layout_path(data_dir, id);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(target)
}

fn meta_from_session(s: &Session, source: String, forked_from: Option<String>) -> RolloutMeta {
    let source = s.source.clone().unwrap_or(source);
    RolloutMeta {
        schema: ROLLOUT_SCHEMA,
        id: s.id.clone(),
        source,
        created_at: s.created_at,
        forked_from,
        title: s.title.clone(),
        provider_id: s.provider_id.clone(),
        model: s.model.clone(),
        system_prompt: s.system_prompt.clone(),
        prompt_id: s.prompt_id.clone(),
        stream: s.stream,
        workdir: s.workdir.clone(),
        allowed_paths: s.allowed_paths.clone(),
        runtime_allowed_paths: s.runtime_allowed_paths.clone(),
        pending_runtime_allowed_paths: s.pending_runtime_allowed_paths.clone(),
        enabled_tools: s.enabled_tools.clone(),
        skill_dirs: s.skill_dirs.clone(),
        reasoning: s.reasoning.clone(),
        token_stats: s.token_stats,
        project_id: s.project_id.clone(),
        run_mode: s.run_mode,
        global_rules: s.global_rules.clone(),
        rules_files: s.rules_files.clone(),
        todos: s.todos.clone(),
        active_plan: s.active_plan.clone(),
        active_goal: s.active_goal.clone(),
        pre_plan_mode: s.pre_plan_mode,
        pending_continue: s.pending_continue.clone(),
    }
}

fn apply_meta(s: &mut Session, m: RolloutMeta) {
    s.id = m.id;
    s.source = Some(m.source);
    s.title = m.title;
    s.provider_id = m.provider_id;
    s.model = m.model;
    s.system_prompt = m.system_prompt;
    s.prompt_id = m.prompt_id;
    s.stream = m.stream;
    s.workdir = m.workdir;
    s.allowed_paths = m.allowed_paths;
    s.runtime_allowed_paths = m.runtime_allowed_paths;
    s.pending_runtime_allowed_paths = m.pending_runtime_allowed_paths;
    s.enabled_tools = m.enabled_tools;
    s.skill_dirs = m.skill_dirs;
    s.reasoning = m.reasoning;
    s.token_stats = m.token_stats;
    s.project_id = m.project_id;
    s.run_mode = m.run_mode;
    s.global_rules = m.global_rules.clone();
    s.rules_files = m.rules_files;
    s.todos = m.todos;
    s.active_plan = m.active_plan;
    s.active_goal = m.active_goal;
    s.pre_plan_mode = m.pre_plan_mode;
    s.pending_continue = m.pending_continue;
    s.created_at = m.created_at;
}

fn apply_update(s: &mut Session, u: MetaUpdate) {
    if let Some(v) = u.title {
        s.title = v;
    }
    if let Some(v) = u.provider_id {
        s.provider_id = v;
    }
    if let Some(v) = u.model {
        s.model = v;
    }
    if let Some(v) = u.system_prompt {
        s.system_prompt = Some(v);
    }
    if let Some(v) = u.prompt_id {
        s.prompt_id = Some(v);
    }
    if let Some(v) = u.stream {
        s.stream = v;
    }
    if let Some(v) = u.workdir {
        s.workdir = Some(v);
    }
    if let Some(v) = u.allowed_paths {
        s.allowed_paths = Some(v);
    }
    if let Some(v) = u.runtime_allowed_paths {
        s.runtime_allowed_paths = v;
    }
    if let Some(v) = u.pending_runtime_allowed_paths {
        s.pending_runtime_allowed_paths = v;
    }
    if let Some(v) = u.enabled_tools {
        s.enabled_tools = Some(v);
    }
    if let Some(v) = u.skill_dirs {
        s.skill_dirs = Some(v);
    }
    if let Some(v) = u.reasoning {
        s.reasoning = Some(v);
    }
    if let Some(v) = u.token_stats {
        s.token_stats = Some(v);
    }
    if let Some(v) = u.project_id {
        s.project_id = Some(v);
    }
    if let Some(v) = u.run_mode {
        s.run_mode = v;
    }
    if let Some(v) = u.global_rules {
        s.global_rules = Some(v);
    }
    if let Some(v) = u.rules_files {
        s.rules_files = Some(v);
    }
    if let Some(v) = u.todos {
        s.todos = v;
    }
    // active_plan：先看 clear 再看 set，让 clear 与 set 不能同帧并存——若并存以 set 为准。
    if u.clear_active_plan {
        s.active_plan = None;
    }
    if let Some(v) = u.active_plan {
        s.active_plan = Some(v);
    }
    if u.clear_active_goal {
        s.active_goal = None;
    }
    if let Some(v) = u.active_goal {
        s.active_goal = Some(v);
    }
    if u.clear_pre_plan_mode {
        s.pre_plan_mode = None;
    }
    if let Some(v) = u.pre_plan_mode {
        s.pre_plan_mode = Some(v);
    }
    if u.clear_pending_continue {
        s.pending_continue = None;
    }
    if let Some(v) = u.pending_continue {
        s.pending_continue = Some(v);
    }
}

/// 把 jsonl 文件折叠回一个完整 [`Session`]。
fn read_jsonl(path: &Path) -> AppResult<Session> {
    let content = std::fs::read_to_string(path)?;

    // 兼容历史脏数据：早期版本把老 `<id>.json`（pretty-printed JSON 整对象）
    // 直接 rename 成 `<id>/session.jsonl`，没做格式转换，结果每次 list 扫描时
    // `serde_json::from_str::<RolloutLine>` 在每一行上都报 "missing field type"
    // 一堆 warn 刷屏。这里检测「内容首字符是 `{` 且紧跟换行」（pretty JSON 起手式）
    // → 尝试当整文件 JSON 解析，成功后回写为合法 jsonl 自愈，避免下次再警。
    let head = content.trim_start();
    if head.starts_with("{\n") || head.starts_with("{\r\n") {
        if let Ok(session) = serde_json::from_str::<Session>(&content) {
            let source = session.source.clone().unwrap_or_else(default_source);
            if let Err(e) = write_jsonl_full(path, &session, source) {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "检测到 pretty-JSON session，但重写为 jsonl 失败"
                );
            } else {
                tracing::info!(
                    path = %path.display(),
                    "把老 .json 格式的 session 文件重写为合法 jsonl"
                );
            }
            return Ok(session);
        }
    }

    let mut session = Session {
        id: String::new(),
        title: "新对话".to_string(),
        provider_id: String::new(),
        model: String::new(),
        system_prompt: None,
        prompt_id: None,
        stream: true,
        messages: Vec::new(),
        workdir: None,
        allowed_paths: None,
        runtime_allowed_paths: Vec::new(),
        pending_runtime_allowed_paths: Vec::new(),
        enabled_tools: None,
        skill_dirs: None,
        reasoning: None,
        token_stats: None,
        source: None,
        project_id: None,
        run_mode: RunMode::default(),
        global_rules: None,
        rules_files: None,
        todos: Vec::new(),
        active_plan: None,
        active_goal: None,
        pre_plan_mode: None,
        pending_continue: None,
        created_at: 0,
        updated_at: 0,
    };
    let mut latest_ts: i64 = 0;
    let mut got_meta = false;
    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: RolloutLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[hebbian] skip malformed rollout line {}:{}: {e}",
                    path.display(),
                    lineno + 1
                );
                continue;
            }
        };
        match parsed {
            RolloutLine::Meta(m) => {
                if m.schema > ROLLOUT_SCHEMA {
                    return Err(AppError::msg(format!(
                        "session {} schema {} 比当前可读最大版本 {ROLLOUT_SCHEMA} 还新；请升级 hebbian",
                        m.id, m.schema
                    )));
                }
                latest_ts = latest_ts.max(m.created_at);
                apply_meta(&mut session, m);
                got_meta = true;
            }
            RolloutLine::Message(msg) => {
                latest_ts = latest_ts.max(msg.created_at);
                session.messages.push(msg);
            }
            RolloutLine::MetaUpdate(u) => {
                latest_ts = latest_ts.max(u.at);
                apply_update(&mut session, u);
            }
            RolloutLine::Event(_) => {
                // 当前版本不消费 event 行
            }
        }
    }
    if !got_meta {
        return Err(AppError::msg(format!(
            "session 文件 {} 缺少 Meta 头行",
            path.display()
        )));
    }
    // 逻辑顺序 = created_at 升序，不是物理 append 顺序（架构 §4.9.5 消息顺序契约）。
    // 插队场景下两者背离：插队 user 即写即落（§4.12.5），而它前面那段 assistant 要
    // 等下一个 drain 边界才落盘，物理上 user 反在前。stable sort 只纠正时间戳倒挂的
    // 插队消息，相等保持物理序——同一时刻的多条消息、assistant 内嵌的 tool 配对都不乱。
    // 三个 surface 共用 read_jsonl 这一个入口，下游 from_session / UI / list 全信任此序。
    session.messages.sort_by_key(|m| m.created_at);
    session.updated_at = latest_ts.max(session.created_at);
    Ok(session)
}

fn top_level_i64_field(json: &str, field: &str) -> Option<i64> {
    let bytes = json.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' if depth == 1 => {
                let key_start = i + 1;
                i += 1;
                let mut escaped = false;
                while i < bytes.len() {
                    let b = bytes[i];
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        break;
                    }
                    i += 1;
                }
                if i >= bytes.len() {
                    return None;
                }
                let key = &json[key_start..i];
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i >= bytes.len() || bytes[i] != b':' {
                    continue;
                }
                i += 1;
                if key != field {
                    continue;
                }
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let value_start = i;
                if i < bytes.len() && bytes[i] == b'-' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == value_start || (i == value_start + 1 && bytes[value_start] == b'-') {
                    return None;
                }
                return json[value_start..i].parse().ok();
            }
            b'"' => {
                i += 1;
                let mut escaped = false;
                while i < bytes.len() {
                    let b = bytes[i];
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// 只解析 meta 信息，跳过 Message/Event 行的 content 反序列化。
/// 用于 `list()` 等只需要 SessionMeta 的场景，避免解析 15MB+ 大文件的 messages。
/// 返回 (Session 骨架, 非 marker message 数量)。
fn read_jsonl_meta_only(path: &Path) -> AppResult<(Session, usize)> {
    let content = std::fs::read_to_string(path)?;

    // 兼容老 pretty-JSON 格式（同 read_jsonl）
    let head = content.trim_start();
    if head.starts_with("{\n") || head.starts_with("{\r\n") {
        if let Ok(session) = serde_json::from_str::<Session>(&content) {
            let source = session.source.clone().unwrap_or_else(default_source);
            if let Err(e) = write_jsonl_full(path, &session, source) {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "检测到 pretty-JSON session，但重写为 jsonl 失败"
                );
            }
            let count = session
                .messages
                .iter()
                .filter(|m| !matches!(m.role, Role::Marker))
                .count();
            return Ok((session, count));
        }
    }

    let mut session = Session {
        id: String::new(),
        title: "新对话".to_string(),
        provider_id: String::new(),
        model: String::new(),
        system_prompt: None,
        prompt_id: None,
        stream: true,
        messages: Vec::new(),
        workdir: None,
        allowed_paths: None,
        runtime_allowed_paths: Vec::new(),
        pending_runtime_allowed_paths: Vec::new(),
        enabled_tools: None,
        skill_dirs: None,
        reasoning: None,
        token_stats: None,
        source: None,
        project_id: None,
        run_mode: RunMode::default(),
        global_rules: None,
        rules_files: None,
        todos: Vec::new(),
        active_plan: None,
        active_goal: None,
        pre_plan_mode: None,
        pending_continue: None,
        created_at: 0,
        updated_at: 0,
    };
    let mut latest_ts: i64 = 0;
    let mut got_meta = false;
    let mut message_count: usize = 0;

    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 快速跳过 message/event 行的 content 反序列化：serde internally tagged enum
        // 把 type 字段放在最前面，字符串匹配 O(1) 跳过，避免反序列化巨大的
        // Message content / tool_calls。这是 list() 性能的关键优化——231 个 session
        // 中最大的 15MB，全解析会卡死主线程。
        if trimmed.starts_with(r#"{"type":"message"#) {
            if let Some(created_at) = top_level_i64_field(trimmed, "created_at") {
                latest_ts = latest_ts.max(created_at);
            }
            // 统计非 marker message 数量。marker 行的特征是 `"role":"marker"` 紧跟在
            // type/id 之后（前 100 字符内），content 为空字符串。
            // 不用完整反序列化，用字符串启发式匹配——实际场景中 content 几乎不可能包含
            // `"role":"marker"` 这个精确子串（注意引号）。
            let is_marker = trimmed.len() < 200 && trimmed.contains(r#""role":"marker"#);
            if !is_marker {
                message_count += 1;
            }
            continue;
        }
        if trimmed.starts_with(r#"{"type":"event"#) {
            continue;
        }

        // 只有 meta/meta_update 行才完整解析
        let parsed: RolloutLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[hebbian] skip malformed rollout line {}:{}: {e}",
                    path.display(),
                    lineno + 1
                );
                continue;
            }
        };
        match parsed {
            RolloutLine::Meta(m) => {
                if m.schema > ROLLOUT_SCHEMA {
                    return Err(AppError::msg(format!(
                        "session {} schema {} 比当前可读最大版本 {ROLLOUT_SCHEMA} 还新；请升级 hebbian",
                        m.id, m.schema
                    )));
                }
                latest_ts = latest_ts.max(m.created_at);
                apply_meta(&mut session, m);
                got_meta = true;
            }
            RolloutLine::Message(msg) => {
                latest_ts = latest_ts.max(msg.created_at);
                if !matches!(msg.role, Role::Marker) {
                    message_count += 1;
                }
            }
            RolloutLine::MetaUpdate(u) => {
                latest_ts = latest_ts.max(u.at);
                apply_update(&mut session, u);
            }
            RolloutLine::Event(_) => {}
        }
    }
    if !got_meta {
        return Err(AppError::msg(format!(
            "session 文件 {} 缺少 Meta 头行",
            path.display()
        )));
    }
    session.updated_at = latest_ts.max(session.created_at);
    Ok((session, message_count))
}

/// 全量重写 jsonl 文件（meta + messages）。clean slate，过去的 MetaUpdate 行被丢弃。
fn write_jsonl_full(path: &Path, s: &Session, source: String) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    let meta_line = serde_json::to_string(&RolloutLine::Meta(meta_from_session(s, source, None)))?;
    buf.push_str(&meta_line);
    buf.push('\n');
    for m in &s.messages {
        let line = serde_json::to_string(&RolloutLine::Message(m.clone()))?;
        buf.push_str(&line);
        buf.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn append_line(path: &Path, line: &RolloutLine) -> AppResult<()> {
    let serialized = serde_json::to_string(line)?;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(serialized.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// 找到该 session 的 jsonl 文件路径；如果只存在旧 `<id>.json`，就先迁移。
fn ensure_jsonl(data_dir: &Path, id: &str) -> AppResult<PathBuf> {
    if let Some(p) = find_jsonl(data_dir, id)? {
        return Ok(p);
    }
    let s = load(data_dir, id)?; // 这里会读 legacy json
    let source = preserve_source(data_dir, id).unwrap_or_else(default_source);
    let target = jsonl_path_for(data_dir, &s.id, s.created_at)?;
    write_jsonl_full(&target, &s, source)?;
    archive_legacy_json(data_dir, id);
    Ok(target)
}

/// 读 jsonl 第一行 Meta 拿到原始 source。仅用于 save 时保留 surface 字段。
fn preserve_source(data_dir: &Path, id: &str) -> Option<String> {
    let path = find_jsonl(data_dir, id).ok().flatten()?;
    let f = std::fs::File::open(&path).ok()?;
    use std::io::{BufRead, BufReader};
    let mut first = String::new();
    let mut reader = BufReader::new(f);
    reader.read_line(&mut first).ok()?;
    let line: RolloutLine = serde_json::from_str(first.trim()).ok()?;
    if let RolloutLine::Meta(m) = line {
        Some(m.source)
    } else {
        None
    }
}

/// 把旧 `<id>.json` 改名 `<id>.json.bak`（保险，不删）。
fn archive_legacy_json(data_dir: &Path, id: &str) {
    if let Ok(Some(legacy)) = find_legacy_json(data_dir, id) {
        let bak = legacy.with_extension("json.bak");
        let _ = std::fs::rename(&legacy, &bak);
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 公共 API
// ════════════════════════════════════════════════════════════════════════════

pub fn list(data_dir: &Path) -> AppResult<Vec<SessionMeta>> {
    let mut out = Vec::new();
    for file in all_session_files(data_dir)? {
        // 性能关键路径：用 meta-only 解析，跳过 Message/Event 行的 content 反序列化。
        // 231 个 session 中最大 15MB/748 行，全解析会阻塞 Tauri 主线程导致页面空白。
        let result = match file.extension().and_then(|s| s.to_str()) {
            Some("jsonl") => read_jsonl_meta_only(&file),
            Some("json") => {
                // legacy json 文件通常不大，走完整解析
                match common::storage::read_json_required::<Session>(&file) {
                    Ok(s) => {
                        let count = s
                            .messages
                            .iter()
                            .filter(|m| !matches!(m.role, Role::Marker))
                            .count();
                        Ok((s, count))
                    }
                    Err(e) => Err(e),
                }
            }
            _ => continue,
        };
        let (session, count) = match result {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push(SessionMeta {
            id: session.id,
            title: session.title,
            provider_id: session.provider_id,
            model: session.model,
            created_at: session.created_at,
            updated_at: session.updated_at,
            message_count: count,
            date: date_string(session.created_at),
            source: session.source,
            project_id: session.project_id,
            workdir: session.workdir,
            path: Some(file),
        });
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

/// 纯读：找到 session 的 jsonl 或 legacy json 并解析。**不触发 partial 恢复**——
/// 该函数在 turn 进行中被 [`append_message`] / [`rename`] 等内部写入路径反复调用，
/// 此时活跃 partial 还在被流式 append，若内嵌 recover 会把当前 turn 当成"中断"误折叠。
///
/// Surface 加载会话历史的入口请用 [`load_with_partial_recovery`]——它在打开
/// jsonl 之前会先把上次进程中断时残留的 partial sidecar 折叠成
/// `Assistant + Interrupted marker` 写入主 jsonl。
pub fn load(data_dir: &Path, id: &str) -> AppResult<Session> {
    if let Some(p) = find_jsonl(data_dir, id)? {
        return read_jsonl(&p);
    }
    if let Some(p) = find_legacy_json(data_dir, id)? {
        return common::storage::read_json_required(&p);
    }
    Err(AppError::msg(format!("session {id} not found")))
}

/// 轻量读取 session 的累计 token 用量，跳过 message/event 行的反序列化（同 `list()`）。
/// agent_loop 启动时用它播种估算校准比值——已加载的长会话首轮就能拿到上次的服务端真值。
/// 找不到 / 解析失败 / 还没采过样都返回 `None`，调用方退化为裸估算。
pub fn load_token_stats(data_dir: &Path, id: &str) -> Option<TokenStats> {
    let path = find_jsonl(data_dir, id).ok()??;
    let (session, _) = read_jsonl_meta_only(&path).ok()?;
    session.token_stats
}

/// Surface 入口语义：恢复 partial 残留后再加载 session。
/// 桌面 / CLI / hebweb 在用户打开会话历史 / 发送新消息前应走这条路径。
///
/// 两类 partial 区别对待（架构 §7.8.5 步骤⑥）：
/// - **死 partial**（写者已退，真中断）→ `recover_and_append_interrupted_partials` 折成
///   `Assistant + Interrupted marker` 落进 jsonl，随 `load` 读出。
/// - **活 partial**（写者还在跑，hebcore run 进行中）→ **不落盘**，加载后把已累积的流式
///   内容拼成一条进行中的 assistant message 追加到返回的 Session（内存态）——让用户切到
///   正在跑的对话能看到流式内容；run 收尾 hebcore 落正式 message，下次加载读正式的。
pub fn load_with_partial_recovery(data_dir: &Path, id: &str) -> AppResult<Session> {
    if let Err(e) = recover_and_append_interrupted_partials(data_dir, id) {
        tracing::warn!(session = %id, error = %e, "恢复 partial 失败");
    }
    let mut session = load(data_dir, id)?;
    append_live_partials(data_dir, id, &mut session);
    Ok(session)
}

/// 把活 partial 的已累积流式内容拼进 session（内存态、不落盘）。死 partial 已被
/// `recover_and_append_interrupted_partials` 处理掉，这里只剩活的。
fn append_live_partials(data_dir: &Path, id: &str, session: &mut Session) {
    let partials = match super::sessions_dir::recover_interrupted_partials(data_dir, id) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(session = %id, error = %e, "扫描活 partial 失败");
            return;
        }
    };
    for p in &partials {
        if !p.alive {
            continue;
        }
        if let Some(msg) = partial_to_live_message(p) {
            session.messages.push(msg);
        }
    }
}

/// 中断恢复时追加在残片末尾的话术。同时进 `MessagePart::Text` 与 `content`，
/// 模型读 transcript 能识别"上一轮输出未走完"，UI 直接渲染同一行。
pub const INTERRUPTED_TAIL_NOTICE: &str = "输出中断";

/// 扫描 `<session>/partial/` 残留文件，折叠成 assistant 消息追加进 session.jsonl，
/// 紧跟一条 `Interrupted` marker，并删除 partial 文件。返回追加的中断段数量。
///
/// 仅在 session.jsonl 已存在时执行（避免为孤立 partial 创建空 session）；
/// 不递归调用 `load`，使用 `find_jsonl` 直接定位文件，规避恢复期间的重入。
pub fn recover_and_append_interrupted_partials(data_dir: &Path, id: &str) -> AppResult<usize> {
    let partials = super::sessions_dir::recover_interrupted_partials(data_dir, id)?;
    if partials.is_empty() {
        return Ok(0);
    }
    let Some(path) = find_jsonl(data_dir, id)? else {
        return Ok(0);
    };
    let mut appended = 0usize;
    for p in &partials {
        // 活 partial（写者还在跑）只在加载时内存渲染（见 load_with_partial_recovery），
        // **不落盘、不删**——hebcore run 收尾会把这段正式落进 jsonl，折盘会重复两份。
        if p.alive {
            continue;
        }
        // 跨进程独占折盘（§7.8.5）：抢该死 partial 的 `.live` 锁。抢不到 = 别的恢复者
        // 正在折它（或写者刚复活）→ 跳过，避免两 surface 并发打开同一崩溃 session 把同一段
        // 重复折两份进 jsonl。持锁跨越 append + delete，结束（drop）才释放。
        let Some(_guard) =
            super::sessions_dir::PartialLiveGuard::try_acquire(data_dir, id, &p.msg_id)
        else {
            continue;
        };
        // 持锁后复查 partial 还在——别的恢复者可能在我抢到锁前已折盘 + 删（幂等兜底）。
        if !super::sessions_dir::partial_path(data_dir, id, &p.msg_id).exists() {
            continue;
        }
        if let Some(msg) = partial_to_interrupted_message(p) {
            append_line(&path, &RolloutLine::Message(msg))?;
            append_line(
                &path,
                &RolloutLine::Message(Message {
                    id: new_id(),
                    role: Role::Marker,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: now(),
                    meta: Some(MessageMeta::Interrupted),
                    subagent_call_id: None,
                    run_duration_ms: None,
                }),
            )?;
            appended += 1;
        }
        let _ = super::sessions_dir::delete_partial(data_dir, id, &p.msg_id);
    }
    Ok(appended)
}

/// 把 partial 折叠结果翻译成可落盘的 assistant 消息。
///
/// 规则：
/// - 无 `name` 的 tool_call 直接丢——name 缺失意味着流式 delta 首帧没透过来，
///   只剩残缺 arguments，模型读不出工具身份、UI 也无法渲染，留下只是噪声
/// - 有 `name` 的保留，arguments 即便不是合法 JSON 也保留原文（input 落 Null），
///   让模型在下一轮自行判断"这次工具调用没走完"
/// - 末尾追加 [`INTERRUPTED_TAIL_NOTICE`]：part 与 content 各加一份
/// - text / reasoning / 有名 tool_call 全空时返回 None（无内容无需保存）
fn partial_to_interrupted_message(
    partial: &super::sessions_dir::RecoveredPartial,
) -> Option<Message> {
    partial_to_message(partial, false)
}

/// 把活 partial（写者还在跑）组装成一条**进行中的流式** assistant message——只读出来
/// 渲染，**不加中断话术、不落盘**（架构 §7.8.5 步骤⑥）。surface 加载会话历史时把它
/// 拼进返回的 Session，让用户看到正在跑的 run 的已累积内容；hebcore run 收尾会把这段
/// 正式落进 jsonl，下次加载 partial 已删、读正式的。
fn partial_to_live_message(
    partial: &super::sessions_dir::RecoveredPartial,
) -> Option<Message> {
    partial_to_message(partial, true)
}

/// 把 partial 折叠结果翻译成 assistant 消息。`live=true` 时是进行中的流式渲染
/// （不追中断话术、id 按 msg_id 稳定，避免每次 load 生成新 id 让前端重复渲染）；
/// `live=false` 时是中断恢复（末尾追加 [`INTERRUPTED_TAIL_NOTICE`]）。
///
/// 规则：
/// - 无 `name` 的 tool_call 直接丢——name 缺失意味着流式 delta 首帧没透过来，
///   只剩残缺 arguments，模型读不出工具身份、UI 也无法渲染，留下只是噪声
/// - 有 `name` 的保留，arguments 即便不是合法 JSON 也保留原文（input 落 Null），
///   让模型在下一轮自行判断"这次工具调用没走完"
/// - text / reasoning / 有名 tool_call 全空时返回 None（无内容无需保存）
fn partial_to_message(
    partial: &super::sessions_dir::RecoveredPartial,
    live: bool,
) -> Option<Message> {
    let named_tool_calls: Vec<(u32, String, String)> = partial
        .tool_calls
        .iter()
        .filter_map(|(idx, (name, args))| name.as_ref().map(|n| (*idx, n.clone(), args.clone())))
        .collect();

    if partial.text.is_empty() && partial.reasoning.is_empty() && named_tool_calls.is_empty() {
        return None;
    }

    let mut parts: Vec<MessagePart> = Vec::new();
    if !partial.reasoning.is_empty() {
        parts.push(MessagePart::Reasoning {
            text: partial.reasoning.clone(),
        });
    }
    if !partial.text.is_empty() {
        parts.push(MessagePart::Text {
            text: partial.text.clone(),
        });
    }
    for (idx, name, args) in &named_tool_calls {
        // 中断时 args 可能是不完整 JSON；fallback 用空 object 而非 null，
        // 避免恢复续聊时向 API 发出 null input 被 400 拒绝。
        let input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
        let (result, duration_ms) = partial
            .tool_results
            .get(idx)
            .map(|(r, d)| (Some(r.clone()), Some(*d)))
            .unwrap_or((None, None));
        parts.push(MessagePart::ToolCall {
            id: format!("recovered-{idx}"),
            name: name.clone(),
            input,
            arguments: args.clone(),
            result: result.clone(),
            duration_ms,
            // 中断恢复的调用结果未知，不标失败。
            is_error: false,
        });
    }
    // 中断恢复才追加「输出中断」话术；活流式不加（它还在跑）。
    if !live {
        parts.push(MessagePart::Text {
            text: INTERRUPTED_TAIL_NOTICE.to_string(),
        });
    }

    let mut content = partial.text.clone();
    if !live {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(INTERRUPTED_TAIL_NOTICE);
    }

    let tool_calls: Vec<MessageToolCall> = named_tool_calls
        .iter()
        .map(|(idx, name, args)| {
            let (result, duration_ms) = partial
                .tool_results
                .get(idx)
                .map(|(r, d)| (Some(r.clone()), Some(*d)))
                .unwrap_or((None, None));
            MessageToolCall {
                id: format!("recovered-{idx}"),
                name: name.clone(),
                input: serde_json::from_str(args).unwrap_or_else(|_| json!({})),
                result,
                duration_ms,
                is_error: false,
                nested: Vec::new(),
            }
        })
        .collect();

    Some(Message {
        // 活流式用按 msg_id 稳定的 id：多次 load 同一活 partial 返回同一 id，
        // 前端按 id 去重不会重复渲染。中断恢复落盘走 new_id（一次性）。
        id: if live {
            format!("live-{}", partial.msg_id)
        } else {
            new_id()
        },
        role: Role::Assistant,
        content,
        attachments: Vec::new(),
        tool_calls,
        parts,
        created_at: now(),
        meta: None,
        subagent_call_id: None,
        run_duration_ms: None,
    })
}

/// 把 old `<date>/<id>.jsonl` 或平铺 `<id>.jsonl` 迁移到新布局 `<id>/session.jsonl`。
/// 仅迁移老 jsonl；老 `.json` 在第一次写入时由 `ensure_jsonl` 兜底迁移到 jsonl。
pub fn migrate_legacy_layout_if_needed(data_dir: &Path) -> AppResult<usize> {
    let root = root_dir(data_dir);
    if !root.exists() {
        return Ok(0);
    }
    let mut moved = 0usize;
    let entries: Vec<_> = std::fs::read_dir(&root)?.flatten().collect();
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            // 平铺 <id>.jsonl。用 is_session_file 过滤，排除 rollout-*.jsonl 与
            // 带副扩展名的 sidecar（如 `<sid>.model_io.jsonl`）——否则 file_stem
            // 会取到 `<sid>.model_io` 当成 session_id 错迁移。
            if is_session_file(&path) {
                if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                    if !new_layout_exists(data_dir, id) {
                        let _ = migrate_legacy_to_new(data_dir, id, &path);
                        moved += 1;
                    }
                }
            }
            continue;
        }
        // 跳过新布局目录
        if path.join("session.jsonl").exists() {
            continue;
        }
        // 老的 <date>/<id>.jsonl
        let subs: Vec<_> = match std::fs::read_dir(&path) {
            Ok(rd) => rd.flatten().collect(),
            Err(_) => continue,
        };
        for sub in subs {
            let sub_p = sub.path();
            if !is_session_file(&sub_p) {
                continue;
            }
            if let Some(id) = sub_p.file_stem().and_then(|s| s.to_str()) {
                if !new_layout_exists(data_dir, id) {
                    let _ = migrate_legacy_to_new(data_dir, id, &sub_p);
                    moved += 1;
                }
            }
        }
    }
    Ok(moved)
}

fn load_from_path(path: &Path) -> AppResult<Session> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("jsonl") => read_jsonl(path),
        Some("json") => common::storage::read_json_required(path),
        _ => Err(AppError::msg(format!(
            "无法识别的 session 文件: {}",
            path.display()
        ))),
    }
}

/// 全量写入（compaction / fork）。会清掉过去所有 MetaUpdate 行的痕迹。
pub fn save(data_dir: &Path, mut s: Session) -> AppResult<Session> {
    s.updated_at = now();
    let target = jsonl_path_for(data_dir, &s.id, s.created_at)?;
    let source = preserve_source(data_dir, &s.id).unwrap_or_else(default_source);
    write_jsonl_full(&target, &s, source)?;
    archive_legacy_json(data_dir, &s.id);
    Ok(s)
}

/// 只追加会话元数据更新，不触碰已经落盘的消息历史。
pub fn update_meta(
    data_dir: &Path,
    id: &str,
    f: impl FnOnce(&mut Session) -> AppResult<()>,
) -> AppResult<Session> {
    let mut session = load(data_dir, id)?;
    f(&mut session)?;
    let at = now();
    session.updated_at = at;
    let path = ensure_jsonl(data_dir, id)?;
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at,
            title: Some(session.title.clone()),
            provider_id: Some(session.provider_id.clone()),
            model: Some(session.model.clone()),
            system_prompt: session.system_prompt.clone(),
            prompt_id: session.prompt_id.clone(),
            stream: Some(session.stream),
            workdir: session.workdir.clone(),
            allowed_paths: session.allowed_paths.clone(),
            runtime_allowed_paths: Some(session.runtime_allowed_paths.clone()),
            pending_runtime_allowed_paths: Some(session.pending_runtime_allowed_paths.clone()),
            enabled_tools: session.enabled_tools.clone(),
            skill_dirs: session.skill_dirs.clone(),
            reasoning: session.reasoning.clone(),
            token_stats: session.token_stats,
            project_id: session.project_id.clone(),
            run_mode: Some(session.run_mode),
            global_rules: session.global_rules.clone(),
            rules_files: session.rules_files.clone(),
            todos: Some(session.todos.clone()),
            active_plan: session.active_plan.clone(),
            active_goal: session.active_goal.clone(),
            pre_plan_mode: session.pre_plan_mode,
            pending_continue: session.pending_continue.clone(),
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

pub fn delete(data_dir: &Path, id: &str) -> AppResult<()> {
    // 新布局：整目录删除（含 partial / tool_results / compactions / plans / meta.json）
    let new_dir = super::sessions_dir::session_dir(data_dir, id);
    if new_dir.exists() {
        std::fs::remove_dir_all(&new_dir)?;
    }
    if let Some(path) = find_legacy_jsonl(data_dir, id)? {
        std::fs::remove_file(path)?;
    }
    if let Some(path) = find_legacy_json(data_dir, id)? {
        std::fs::remove_file(path)?;
    }
    // .json.bak 也一起清掉（老布局残留）
    let root = root_dir(data_dir);
    let bak_flat = root.join(format!("{id}.json.bak"));
    if bak_flat.exists() {
        let _ = std::fs::remove_file(&bak_flat);
    }
    if let Ok(rd) = std::fs::read_dir(&root) {
        for entry in rd.flatten() {
            if entry.path().is_dir() {
                let bak = entry.path().join(format!("{id}.json.bak"));
                if bak.exists() {
                    let _ = std::fs::remove_file(&bak);
                }
            }
        }
    }
    Ok(())
}

pub fn create(
    data_dir: &Path,
    provider_id: String,
    model: String,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
) -> AppResult<Session> {
    create_with_source(
        data_dir,
        provider_id,
        model,
        system_prompt,
        prompt_id,
        default_source(),
    )
}

/// 显式指定 surface 来源的 create。CLI 应传 `"cli"`，desktop / 测试用默认值即可。
pub fn create_with_source(
    data_dir: &Path,
    provider_id: String,
    model: String,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
    source: String,
) -> AppResult<Session> {
    let now_ts = now();
    let default_reasoning = common::reasoning::default_reasoning_for_model(&model);
    let mut session = Session {
        id: new_id(),
        title: "新对话".into(),
        provider_id,
        model,
        system_prompt,
        prompt_id,
        stream: true,
        messages: Vec::new(),
        workdir: None,
        allowed_paths: None,
        runtime_allowed_paths: Vec::new(),
        pending_runtime_allowed_paths: Vec::new(),
        enabled_tools: None,
        skill_dirs: None,
        reasoning: default_reasoning,
        token_stats: None,
        source: Some(source.clone()),
        project_id: None,
        run_mode: RunMode::default(),
        global_rules: None,
        rules_files: None,
        todos: Vec::new(),
        active_plan: None,
        active_goal: None,
        pre_plan_mode: None,
        pending_continue: None,
        created_at: now_ts,
        updated_at: now_ts,
    };
    let target = jsonl_path_for(data_dir, &session.id, session.created_at)?;
    write_jsonl_full(&target, &session, source.clone())?;
    // 初始化目录骨架 + meta.json（架构 §4.9.1）。
    super::sessions_dir::ensure_session_dirs(data_dir, &session.id)?;
    let _ = super::sessions_dir::save_meta(
        data_dir,
        &super::sessions_dir::SessionDirMeta {
            session_id: session.id.clone(),
            created_at: session.created_at,
            agent: source.clone(),
            workdir: session.workdir.clone(),
            provider: session.provider_id.clone(),
            model: session.model.clone(),
            last_interrupted_at: None,
        },
    );
    session.updated_at = now_ts;
    Ok(session)
}

pub fn create_with_workspace(
    data_dir: &Path,
    provider_id: String,
    model: String,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
    source: String,
    project_id: Option<String>,
    workdir: Option<PathBuf>,
    allowed_paths: Vec<PathBuf>,
) -> AppResult<Session> {
    let mut session = create_with_source(
        data_dir,
        provider_id,
        model,
        system_prompt,
        prompt_id,
        source,
    )?;
    session.project_id = project_id;
    session.workdir = workdir;
    if !allowed_paths.is_empty() {
        session.allowed_paths = Some(allowed_paths);
    }
    save(data_dir, session)
}

pub fn append_event(data_dir: &Path, id: &str, event: &protocol::Event) -> AppResult<()> {
    let path = ensure_jsonl(data_dir, id)?;
    let value = serde_json::to_value(event)?;
    append_line(&path, &RolloutLine::Event(value))
}

pub fn append_message(data_dir: &Path, id: &str, msg: Message) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    // 落盘是 agent-core 对外（文件系统）的统一出口：所有 user/assistant/marker 都经这里。
    tracing::info!(
        target: "storage",
        session_id = id,
        message_id = %msg.id,
        role = ?msg.role,
        bytes = msg.content.len(),
        "[Storage:Append] 落盘 message"
    );
    append_line(&path, &RolloutLine::Message(msg))?;
    load(data_dir, id)
}

pub fn insert_switch_marker(data_dir: &Path, id: &str, meta: MessageMeta) -> AppResult<Session> {
    append_message(
        data_dir,
        id,
        Message {
            id: new_id(),
            role: Role::Marker,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: now(),
            meta: Some(meta),
            subagent_call_id: None,
            run_duration_ms: None,
        },
    )
}

/// 落一条「已转发到渠道」的 marker（status=Pending），返回它的消息 id。
/// 机主在渠道侧回复后，用这个 id 调 [`resolve_channel_forward_marker`] 把 status 改成
/// Resolved（即写即落：转发与回复都成为 session.jsonl 的可回溯事实，架构 §7.5.1）。
pub fn append_channel_forward_marker(
    data_dir: &Path,
    id: &str,
    channel: String,
    kind: ChannelForwardKind,
) -> AppResult<String> {
    let marker_id = new_id();
    append_message(
        data_dir,
        id,
        Message {
            id: marker_id.clone(),
            role: Role::Marker,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: now(),
            meta: Some(MessageMeta::ChannelForward {
                channel,
                kind,
                status: ChannelForwardStatus::Pending,
            }),
            subagent_call_id: None,
            run_duration_ms: None,
        },
    )?;
    Ok(marker_id)
}

/// 把某条 ChannelForward marker 的 status 从 Pending 更新为 Resolved（机主在渠道侧已处置）。
/// 找不到该 marker（被压缩归档 / 截断）时静默返回 Ok——回复落地不应因痕迹丢失而失败。
pub fn resolve_channel_forward_marker(
    data_dir: &Path,
    id: &str,
    marker_id: &str,
    outcome: String,
) -> AppResult<()> {
    let mut session = load(data_dir, id)?;
    let mut changed = false;
    for message in &mut session.messages {
        if message.id != marker_id {
            continue;
        }
        if let Some(MessageMeta::ChannelForward { status, .. }) = &mut message.meta {
            *status = ChannelForwardStatus::Resolved { outcome };
            changed = true;
        }
        break;
    }
    if changed {
        save(data_dir, session)?;
    }
    Ok(())
}

/// 回填某条已落盘 assistant 段的 `run_duration_ms`（run 收尾耗时徽章）。
/// 用于「末段在 run 收尾前已被预落」的情形（goal/Stop-hook 续跑判定要先 flush 让 marker
/// 排在 assistant 之后）：收尾时该段已在盘上、无新内容可 flush，故按 id 回填耗时，保证
/// run 耗时徽章只盖在本 run 真正的最后一段上（RunPersister::finish 调用）。
/// 找不到该 id（被压缩归档 / fork 截断）时静默返回 Ok。
pub fn set_message_run_duration(
    data_dir: &Path,
    id: &str,
    msg_id: &str,
    run_duration_ms: u64,
) -> AppResult<()> {
    let mut session = load(data_dir, id)?;
    let mut changed = false;
    for message in &mut session.messages {
        if message.id == msg_id {
            message.run_duration_ms = Some(run_duration_ms);
            changed = true;
            break;
        }
    }
    if changed {
        save(data_dir, session)?;
    }
    Ok(())
}

/// 推理参数切换的 marker（thinking on/off / effort / 1M context）。
/// 仅当 `from != to` 才该调用——上层负责对比并决定是否插入。
pub fn insert_reasoning_switch_marker(
    data_dir: &Path,
    id: &str,
    from: Option<common::reasoning::ReasoningConfig>,
    to: Option<common::reasoning::ReasoningConfig>,
) -> AppResult<Session> {
    insert_switch_marker(data_dir, id, MessageMeta::ReasoningSwitch { from, to })
}

pub fn fork(data_dir: &Path, session_id: &str, up_to_message_id: &str) -> AppResult<Session> {
    let src = load(data_dir, session_id)?;
    let mut msgs = Vec::new();
    for m in &src.messages {
        msgs.push(m.clone());
        if m.id == up_to_message_id {
            break;
        }
    }
    let now_ts = now();
    let new = Session {
        id: new_id(),
        title: format!("{} (分支)", src.title),
        provider_id: src.provider_id,
        model: src.model,
        system_prompt: src.system_prompt,
        prompt_id: src.prompt_id,
        stream: src.stream,
        messages: msgs,
        workdir: src.workdir,
        allowed_paths: src.allowed_paths,
        runtime_allowed_paths: src.runtime_allowed_paths,
        pending_runtime_allowed_paths: src.pending_runtime_allowed_paths,
        enabled_tools: src.enabled_tools,
        skill_dirs: src.skill_dirs,
        reasoning: src.reasoning,
        token_stats: src.token_stats,
        project_id: src.project_id,
        // 分支沿用父对话的 surface 来源
        source: src.source,
        run_mode: src.run_mode,
        global_rules: src.global_rules.clone(),
        rules_files: src.rules_files.clone(),
        todos: src.todos.clone(),
        active_plan: src.active_plan.clone(),
        active_goal: src.active_goal.clone(),
        pre_plan_mode: src.pre_plan_mode,
        pending_continue: src.pending_continue.clone(),
        created_at: now_ts,
        updated_at: now_ts,
    };
    let target = jsonl_path_for(data_dir, &new.id, new.created_at)?;
    let mut meta = meta_from_session(&new, default_source(), Some(src.id.clone()));
    let mut buf = String::new();
    meta.created_at = now_ts;
    let line = serde_json::to_string(&RolloutLine::Meta(meta))?;
    buf.push_str(&line);
    buf.push('\n');
    for m in &new.messages {
        let line = serde_json::to_string(&RolloutLine::Message(m.clone()))?;
        buf.push_str(&line);
        buf.push('\n');
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("jsonl.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, &target)?;
    let mut new = new;
    new.updated_at = now_ts;
    Ok(new)
}

pub fn rename(data_dir: &Path, id: &str, title: String) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            title: Some(title),
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

/// 切换会话 [`RunMode`]，追加一行 [`MetaUpdate`] 即可（不重写 messages）。
/// 与 [`rename`] 同 pattern，IO 成本与切换频次匹配。
///
/// **pre_plan_mode 自动管理**（架构 §4.4.5）：当且仅当
/// **从非 PlanMode 进 PlanMode** 时，把当前 run_mode 同时记到 pre_plan_mode，
/// 让 ExitPlanMode 审批通过后能切回去。其他切换不动 pre_plan_mode；
/// dispatch 路径离开 PlanMode 时如要清空请显式调 [`set_pre_plan_mode`]。
pub fn set_run_mode(data_dir: &Path, id: &str, mode: RunMode) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    let prev = load(data_dir, id).ok();
    let record_pre_plan_mode = match (prev.as_ref().map(|s| s.run_mode), mode) {
        (Some(from), RunMode::PlanMode) if from != RunMode::PlanMode => Some(from),
        _ => None,
    };
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            run_mode: Some(mode),
            pre_plan_mode: record_pre_plan_mode,
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

/// TodoWrite 工具更新 todo 列表（架构 §4.4.6）。整列表覆盖语义。
/// 沿用 [`set_run_mode`] 的 append-only MetaUpdate 模式。
pub fn set_todos(data_dir: &Path, id: &str, todos: Vec<TodoItem>) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            todos: Some(todos),
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

/// ExitPlanMode 落盘 plan 后写入"当前 plan"绝对路径（架构 §4.4.5）。
/// 传 `None` 表示清空 active_plan（如 plan revert / 切回 PlanMode 前的状态）。
pub fn set_active_plan(data_dir: &Path, id: &str, plan_path: Option<String>) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    let (set, clear) = match plan_path {
        Some(p) => (Some(p), false),
        None => (None, true),
    };
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            active_plan: set,
            clear_active_plan: clear,
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

/// 设置 / 清空会话目标（架构 §4.8.3 / §8）。
/// `Some(goal)` 写入或覆盖；`None` 清空。沿用 [`set_active_plan`] 的 append-only 模式。
pub fn set_active_goal(
    data_dir: &Path,
    id: &str,
    goal: Option<ActiveGoal>,
) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    let (set, clear) = match goal {
        Some(g) => (Some(g), false),
        None => (None, true),
    };
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            active_goal: set,
            clear_active_goal: clear,
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

/// 进入 PlanMode 时记录"切换前 RunMode"（架构 §4.4.5）。
/// 传 `None` 表示清空（ExitPlanMode 审批通过、切回非 PlanMode 后调用）。
pub fn set_pre_plan_mode(data_dir: &Path, id: &str, mode: Option<RunMode>) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    let (set, clear) = match mode {
        Some(m) => (Some(m), false),
        None => (None, true),
    };
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            pre_plan_mode: set,
            clear_pre_plan_mode: clear,
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

/// 写入 / 清空「续作入口」（架构 §4.3）。`Some(_)` = 上一个 Run 非正常结束；
/// `None` = 正常完成或用户已点 continue，清空它。沿用 append-only MetaUpdate 模式。
pub fn set_pending_continue(
    data_dir: &Path,
    id: &str,
    pending: Option<PendingContinue>,
) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    let clear = pending.is_none();
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            pending_continue: pending,
            clear_pending_continue: clear,
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}

pub fn truncate_after(data_dir: &Path, id: &str, message_id: &str) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    if let Some(idx) = s.messages.iter().position(|m| m.id == message_id) {
        s.messages.truncate(idx + 1);
    }
    save(data_dir, s)
}

pub fn truncate_inclusive(data_dir: &Path, id: &str, message_id: &str) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    if let Some(idx) = s.messages.iter().position(|m| m.id == message_id) {
        s.messages.truncate(idx);
    }
    save(data_dir, s)
}

/// 撤销一次压缩：删掉指定的 [`MessageMeta::CompactBoundary`] marker，让 transcript
/// 回到压缩前的完整历史（用户可换模型重新压缩）。
///
/// **仅允许撤销"最后一条压缩 marker"**——即该 marker 之后不能有任何非 marker 消息
/// （刚压缩完、还没产生新对话）。压缩后又聊过的不允许撤销，避免上下文错乱。
/// 校验失败返回 Err；message_id 不是 CompactBoundary 也返回 Err。
pub fn undo_compaction(data_dir: &Path, id: &str, marker_id: &str) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    let idx = s
        .messages
        .iter()
        .position(|m| m.id == marker_id)
        .ok_or_else(|| AppError::msg(format!("找不到消息 {marker_id}")))?;
    if !matches!(
        s.messages[idx].meta,
        Some(MessageMeta::CompactBoundary { .. })
    ) {
        return Err(AppError::msg("该消息不是压缩标记，无法撤销"));
    }
    // marker 之后若已有新的非 marker 消息（用户已继续对话），不允许撤销。
    let has_following_content = s.messages[idx + 1..]
        .iter()
        .any(|m| !matches!(m.role, Role::Marker));
    if has_following_content {
        return Err(AppError::msg("压缩后已有新对话，无法撤销"));
    }
    s.messages.remove(idx);
    save(data_dir, s)
}

// ════════════════════════════════════════════════════════════════════════════
// 搜索
// ════════════════════════════════════════════════════════════════════════════

enum SearchMatcher {
    Literal {
        needle: String,
        case_sensitive: bool,
    },
    Regex(Regex),
}

impl SearchMatcher {
    fn new(query: &str, case_sensitive: bool, regex: bool) -> Option<Self> {
        if regex {
            let re = RegexBuilder::new(query)
                .case_insensitive(!case_sensitive)
                .build()
                .ok()?;
            return Some(Self::Regex(re));
        }

        Some(Self::Literal {
            needle: if case_sensitive {
                query.to_string()
            } else {
                query.to_lowercase()
            },
            case_sensitive,
        })
    }

    fn find(&self, text: &str) -> Option<(usize, usize)> {
        match self {
            SearchMatcher::Literal {
                needle,
                case_sensitive,
            } => {
                let haystack = if *case_sensitive {
                    text.to_string()
                } else {
                    text.to_lowercase()
                };
                haystack
                    .find(needle)
                    .map(|start| (start, start + needle.len()))
            }
            SearchMatcher::Regex(re) => re.find(text).map(|m| (m.start(), m.end())),
        }
    }
}

pub fn search(
    data_dir: &Path,
    query: &str,
    case_sensitive: bool,
    regex: bool,
) -> AppResult<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(list(data_dir)?
            .into_iter()
            .map(|m| SearchHit {
                meta: m,
                snippet: None,
                matched_in: "",
            })
            .collect());
    }
    let matcher = match SearchMatcher::new(q, case_sensitive, regex) {
        Some(matcher) => matcher,
        None => return Ok(Vec::new()),
    };

    let mut hits = Vec::new();
    for file in all_session_files(data_dir)? {
        let s: Session = match load_from_path(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let count = s
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::Marker))
            .count();
        let title_hit = matcher.find(&s.title).is_some();
        let content_hit = s.messages.iter().find_map(|m| {
            if matches!(m.role, Role::Marker) {
                return None;
            }
            matcher
                .find(&m.content)
                .map(|(start, end)| (m.content.clone(), start, end))
        });
        if !title_hit && content_hit.is_none() {
            continue;
        }
        let snippet = content_hit
            .as_ref()
            .map(|(content, start, end)| make_snippet_from_range(content, *start, *end, 60));
        hits.push(SearchHit {
            meta: SessionMeta {
                id: s.id,
                title: s.title,
                provider_id: s.provider_id,
                model: s.model,
                created_at: s.created_at,
                updated_at: s.updated_at,
                message_count: count,
                date: date_string(s.created_at),
                source: s.source,
                project_id: s.project_id,
                workdir: s.workdir,
                path: Some(file),
            },
            snippet,
            matched_in: if title_hit { "title" } else { "content" },
        });
    }
    hits.sort_by(|a, b| b.meta.updated_at.cmp(&a.meta.updated_at));
    Ok(hits)
}

fn make_snippet_from_range(content: &str, start: usize, end: usize, ctx: usize) -> String {
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    let start_pos = chars.iter().position(|(i, _)| *i >= start).unwrap_or(0);
    let end_pos = chars
        .iter()
        .position(|(i, _)| *i >= end)
        .unwrap_or(chars.len());
    make_snippet_from_char_range(content, start_pos, end_pos, ctx)
}

fn make_snippet_from_char_range(
    content: &str,
    start_pos: usize,
    end_pos: usize,
    ctx: usize,
) -> String {
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    let start_char = start_pos.saturating_sub(ctx);
    let end_char = (end_pos + ctx).min(chars.len());
    let start_byte = chars.get(start_char).map(|(i, _)| *i).unwrap_or(0);
    let end_byte = chars
        .get(end_char)
        .map(|(i, _)| *i)
        .unwrap_or(content.len());
    let mut out = String::new();
    if start_byte > 0 {
        out.push('…');
    }
    out.push_str(&content[start_byte..end_byte]);
    if end_byte < content.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hebbian-sessions-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp data dir");
        dir
    }

    fn save_session(data_dir: &Path, title: &str, content: &str) -> Session {
        let session = create(
            data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .expect("create session");
        rename(data_dir, &session.id, title.to_string()).expect("rename session");
        append_message(
            data_dir,
            &session.id,
            Message {
                id: new_id(),
                role: Role::User,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: now(),
                meta: None,
                subagent_call_id: None,
            run_duration_ms: None,
            },
        )
        .expect("append message")
    }

    /// 回归测试：set_todos 落 meta_update 行后，全量 save 重写 jsonl，todos 不能丢。
    /// 这是 2026-05-26 "完成的 todo 在 sidebar 消失" bug 的真正根因——save 调
    /// accumulate：累计字段累加、last_* 覆盖为最新一次 run。
    /// 支撑 TokenStatsPanel 的「全程平均（累计）+ hover 看最新一次（last_*）」。
    #[test]
    fn token_stats_accumulate_tracks_cumulative_and_last() {
        let mut s = TokenStats::default();
        s.accumulate(TokenStats {
            input_tokens: 100,
            output_tokens: 5,
            cache_creation_tokens: 100,
            run_count: 1,
            ..Default::default()
        });
        s.accumulate(TokenStats {
            input_tokens: 80,
            output_tokens: 8,
            cache_read_tokens: 70,
            cache_creation_tokens: 2,
            run_count: 1,
            last_estimated_tokens: 64,
            ..Default::default()
        });
        // 累计字段累加
        assert_eq!(s.input_tokens, 180);
        assert_eq!(s.output_tokens, 13);
        assert_eq!(s.cache_read_tokens, 70);
        assert_eq!(s.run_count, 2);
        // last_* 覆盖为最新一次（第二次）的值，不累加
        assert_eq!(s.last_input_tokens, 80);
        assert_eq!(s.last_output_tokens, 8);
        assert_eq!(s.last_cache_read_tokens, 70);
        assert_eq!(s.last_cache_creation_tokens, 2);
        // 估算校准的配对值同样覆盖为最新一次
        assert_eq!(s.last_estimated_tokens, 64);
    }

    /// 回归：估算校准样本（last_input + last_estimated）经 jsonl 往返后能被
    /// `load_token_stats` 轻量读回——agent_loop 启动时据此给已加载的长会话播种
    /// 校准比值。旧会话没有 last_estimated_tokens 字段时 serde default 回 0，
    /// 校准退化为裸估算，与历史行为一致。
    #[test]
    fn load_token_stats_roundtrips_calibration_sample() {
        let dir = temp_data_dir("calib-roundtrip");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        bump_token_stats(
            &dir,
            &s.id,
            TokenStats {
                input_tokens: 998_850,
                output_tokens: 100,
                run_count: 1,
                last_estimated_tokens: 710_000,
                ..Default::default()
            },
        );
        let stats = load_token_stats(&dir, &s.id).expect("token_stats 应已落盘");
        assert_eq!(stats.last_input_tokens, 998_850);
        assert_eq!(stats.last_estimated_tokens, 710_000);
        // 没采过样的会话返回 last_estimated_tokens=0 → 校准退化。
        let fresh = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let none = load_token_stats(&dir, &fresh.id);
        assert!(none.map(|s| s.last_estimated_tokens).unwrap_or(0) == 0);
    }

    /// write_jsonl_full 把 meta + messages 重写，把累积的 meta_update 行抹掉，
    /// 同时 RolloutMeta 缺 todos 字段，没法把当前 todos 折叠到新写的 meta 行里。
    #[test]
    fn set_todos_survives_full_save_rewrite() {
        use protocol::todo::{TodoItem, TodoStatus};
        let dir = temp_data_dir("todos-save");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let todos = vec![TodoItem {
            id: "t1".into(),
            content: "Write code".into(),
            active_form: "Writing code".into(),
            status: TodoStatus::Completed,
        }];
        set_todos(&dir, &s.id, todos.clone()).unwrap();
        // append 一条 message 然后全量 save——模拟 chat.rs accumulate_session_tokens 路径
        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.todos.len(), 1, "set_todos 应已让 load 看到 todos");
        let mut s2 = loaded;
        s2.token_stats = Some(TokenStats {
            input_tokens: 100,
            ..Default::default()
        });
        save(&dir, s2).unwrap();
        // 再 load 应仍看到 todos——bug 时这里会 0
        let after_save = load(&dir, &s.id).unwrap();
        assert_eq!(
            after_save.todos.len(),
            1,
            "save 全量重写后 todos 不能消失；token_stats 持久化路径会触发 save"
        );
        assert_eq!(after_save.todos[0].status, TodoStatus::Completed);
    }

    #[test]
    fn update_meta_does_not_rewrite_message_history() {
        let dir = temp_data_dir("meta-update-history");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        for idx in 0..3 {
            append_message(
                &dir,
                &s.id,
                Message {
                    id: new_id(),
                    role: Role::User,
                    content: format!("message {idx}"),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: now(),
                    meta: None,
                    subagent_call_id: None,
            run_duration_ms: None,
                },
            )
            .unwrap();
        }

        update_meta(&dir, &s.id, |session| {
            session.token_stats = Some(TokenStats {
                input_tokens: 100,
                run_count: 1,
                ..Default::default()
            });
            session.runtime_allowed_paths = vec![PathBuf::from("/tmp/runtime")];
            session.pending_runtime_allowed_paths = vec![PathBuf::from("/tmp/pending")];
            Ok(())
        })
        .unwrap();

        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.messages[0].content, "message 0");
        assert_eq!(loaded.token_stats.unwrap().input_tokens, 100);
        assert_eq!(
            loaded.runtime_allowed_paths,
            vec![PathBuf::from("/tmp/runtime")]
        );
        assert_eq!(
            loaded.pending_runtime_allowed_paths,
            vec![PathBuf::from("/tmp/pending")]
        );

        let path = crate::storage::sessions_dir::session_jsonl_path(&dir, &s.id);
        let content = std::fs::read_to_string(&path).unwrap();
        let message_lines = content
            .lines()
            .filter(|line| line.contains("\"type\":\"message\""))
            .count();
        assert_eq!(message_lines, 3, "update_meta 不能重写或截断 message 行");
    }
    #[test]
    fn set_todos_persists_and_load_restores() {
        use protocol::todo::{TodoItem, TodoStatus};
        let dir = temp_data_dir("todos");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        // 初始无 todos
        assert!(s.todos.is_empty());

        let todos = vec![
            TodoItem {
                id: "t1".into(),
                content: "Write code".into(),
                active_form: "Writing code".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: "t2".into(),
                content: "Run tests".into(),
                active_form: "Running tests".into(),
                status: TodoStatus::Pending,
            },
        ];
        set_todos(&dir, &s.id, todos.clone()).expect("set_todos");

        // load 折叠 jsonl 应得到完整 todos
        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.todos.len(), 2);
        assert_eq!(loaded.todos[0].id, "t1");
        assert_eq!(loaded.todos[0].status, TodoStatus::InProgress);

        // jsonl 文件本身必须包含 meta_update 行
        let path = crate::storage::sessions_dir::session_jsonl_path(&dir, &s.id);
        let content = std::fs::read_to_string(&path).unwrap();
        let has_meta_update = content
            .lines()
            .any(|l| l.contains("\"type\":\"meta_update\"") && l.contains("todos"));
        assert!(
            has_meta_update,
            "session.jsonl 应有 meta_update todos 行；内容:\n{content}"
        );

        // 全部标 completed 再写一次，确认 last-wins 折叠正确
        let todos2 = vec![
            TodoItem {
                id: "t1".into(),
                content: "Write code".into(),
                active_form: "Writing code".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                id: "t2".into(),
                content: "Run tests".into(),
                active_form: "Running tests".into(),
                status: TodoStatus::Completed,
            },
        ];
        set_todos(&dir, &s.id, todos2).expect("set_todos 2");
        let loaded2 = load(&dir, &s.id).unwrap();
        assert_eq!(loaded2.todos.len(), 2);
        assert!(loaded2
            .todos
            .iter()
            .all(|t| t.status == TodoStatus::Completed));
    }

    #[test]
    fn create_then_load_round_trip() {
        let dir = temp_data_dir("rt");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.title, "新对话");
        assert_eq!(loaded.messages.len(), 0);
        assert_eq!(loaded.provider_id, "openai");
        assert_eq!(loaded.model, "gpt-x");
    }

    #[test]
    fn active_goal_set_clear_roundtrip() {
        let dir = temp_data_dir("active-goal");
        let id = create(&dir, "openai".into(), "gpt-x".into(), None, None)
            .unwrap()
            .id;

        let goal = ActiveGoal {
            condition: "所有测试通过".to_string(),
            created_at: 1,
            iterations: 0,
            last_reason: None,
            pending_set_marker: false,
        };
        let s = set_active_goal(&dir, &id, Some(goal.clone())).unwrap();
        assert_eq!(s.active_goal.as_ref().unwrap().condition, "所有测试通过");

        let s2 = load(&dir, &id).unwrap();
        assert_eq!(s2.active_goal, Some(goal));

        // 覆盖更新：set 一个带 last_reason/iterations 的新目标，load 应 last-wins 读回全字段
        let goal_b = ActiveGoal {
            condition: "PR 已合并".to_string(),
            created_at: 2,
            iterations: 3,
            last_reason: Some("还差 review 通过".to_string()),
            pending_set_marker: false,
        };
        let _ = set_active_goal(&dir, &id, Some(goal_b.clone())).unwrap();
        assert_eq!(load(&dir, &id).unwrap().active_goal, Some(goal_b));

        let s3 = set_active_goal(&dir, &id, None).unwrap();
        assert_eq!(s3.active_goal, None);
        assert_eq!(load(&dir, &id).unwrap().active_goal, None);
    }

    #[test]
    fn create_with_workspace_persists_project_defaults() {
        let dir = temp_data_dir("workspace-create");
        let session = create_with_workspace(
            &dir,
            "openai".into(),
            "gpt-x".into(),
            None,
            None,
            "desktop".into(),
            Some("proj-1".into()),
            Some(PathBuf::from("/tmp/project")),
            vec![PathBuf::from("/tmp/extra")],
        )
        .unwrap();

        let loaded = load(&dir, &session.id).unwrap();
        assert_eq!(loaded.project_id.as_deref(), Some("proj-1"));
        assert_eq!(loaded.workdir, Some(PathBuf::from("/tmp/project")));
        assert_eq!(
            loaded.allowed_paths,
            Some(vec![PathBuf::from("/tmp/extra")])
        );
    }

    /// 正例：带 SystemNotification meta 的 user message 落盘后 round-trip 字段保留；
    /// is_system_notification 返回 true。
    #[test]
    fn system_notification_meta_round_trip_and_helper() {
        let dir = temp_data_dir("sysnotif_round_trip");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let wakeup = Message {
            id: new_id(),
            role: Role::User,
            content: "<wakeup kind=\"bg_task_finished\">...</wakeup>".into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: now(),
            meta: Some(MessageMeta::SystemNotification {
                kind: "bg_task_finished".into(),
                task_id: Some("bash_003".into()),
                tool_use_id: Some("call_xyz".into()),
            }),
            subagent_call_id: None,
            run_duration_ms: None,
        };
        assert!(
            wakeup.is_system_notification(),
            "is_system_notification 正例必须为 true"
        );
        append_message(&dir, &s.id, wakeup.clone()).unwrap();
        let loaded = load(&dir, &s.id).unwrap();
        let reloaded = loaded.messages.last().expect("wakeup 已落盘");
        match &reloaded.meta {
            Some(MessageMeta::SystemNotification {
                kind,
                task_id,
                tool_use_id,
            }) => {
                assert_eq!(kind, "bg_task_finished");
                assert_eq!(task_id.as_deref(), Some("bash_003"));
                assert_eq!(tool_use_id.as_deref(), Some("call_xyz"));
            }
            other => panic!("meta round-trip 失败，got {other:?}"),
        }
        assert!(
            reloaded.is_system_notification(),
            "load 回来后 is_system_notification 仍应为 true"
        );
    }

    /// 反例：
    /// 1. meta=None 的普通 user message → is_system_notification 必须为 false
    /// 2. meta=Some(其它 variant) → is_system_notification 也必须为 false
    ///    （CompactBoundary / Interrupted / Switch / ReasoningSwitch 不算系统通知）
    #[test]
    fn is_system_notification_false_for_plain_and_other_meta() {
        let plain = Message {
            id: "m1".into(),
            role: Role::User,
            content: "hi".into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        assert!(
            !plain.is_system_notification(),
            "meta=None 不应被当作 system notification"
        );

        let interrupted = Message {
            meta: Some(MessageMeta::Interrupted),
            ..plain.clone()
        };
        assert!(
            !interrupted.is_system_notification(),
            "Interrupted variant 不应被当作 system notification"
        );

        let compact = Message {
            meta: Some(MessageMeta::CompactBoundary {
                summary: "...".into(),
                before_tokens: 100,
                after_tokens: 50,
            }),
            ..plain.clone()
        };
        assert!(
            !compact.is_system_notification(),
            "CompactBoundary variant 不应被当作 system notification"
        );
    }

    /// 序列化稳定性：SystemNotification 必须以 type tag "system_notification"
    /// 写入 jsonl（snake_case），让前端 / 老脚本能稳定 deserialize。
    #[test]
    fn system_notification_serializes_with_snake_case_tag() {
        let meta = MessageMeta::SystemNotification {
            kind: "bg_task_finished".into(),
            task_id: Some("bash_001".into()),
            tool_use_id: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(
            json.contains("\"type\":\"system_notification\""),
            "got {json}"
        );
        assert!(json.contains("\"kind\":\"bg_task_finished\""));
        assert!(json.contains("\"task_id\":\"bash_001\""));
        // tool_use_id=None 必须省略（skip_serializing_if）
        assert!(!json.contains("tool_use_id"), "None 应被 skip：{json}");
    }

    #[test]
    fn append_message_persists_and_reloads() {
        let dir = temp_data_dir("append");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let msg = Message {
            id: new_id(),
            role: Role::User,
            content: "hi".into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: now(),
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        append_message(&dir, &s.id, msg.clone()).unwrap();
        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "hi");
    }

    /// 回归（架构 §4.9.5 消息顺序契约）：插队场景下物理 append 顺序会出现
    /// `user_orig → 插队 user(早) → assistant 段(晚)` 的时间戳倒挂——assistant 段
    /// 内容更早产出但 drain 落盘时刻更晚，物理上排在插队 user 之后。load 必须按
    /// created_at stable sort 把它纠正回 `user_orig → assistant → 插队 user`。
    ///
    /// A/B 翻转：去掉 read_jsonl 末尾的 sort_by_key，本测试必 fail（物理顺序里
    /// 插队 user 在 assistant 之前）。
    #[test]
    fn load_reorders_messages_by_created_at_not_physical_order() {
        let dir = temp_data_dir("reorder");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();

        let mk = |role: Role, content: &str, ts: i64| Message {
            id: new_id(),
            role,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: ts,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };

        // 物理 append 顺序 = 落盘顺序，模拟插队 race：
        //   1) 原始 user（t=100，最早输入）
        //   2) 插队 user（t=200，流式途中即写即落）
        //   3) assistant 段（t=150，内容早就在流式输出，但 drain 落盘晚于插队 user）
        append_message(&dir, &s.id, mk(Role::User, "原始问题", 100)).unwrap();
        append_message(&dir, &s.id, mk(Role::User, "插队消息", 200)).unwrap();
        append_message(&dir, &s.id, mk(Role::Assistant, "助手回答", 150)).unwrap();

        let loaded = load(&dir, &s.id).unwrap();
        let order: Vec<&str> = loaded
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["原始问题", "助手回答", "插队消息"],
            "load 应按 created_at 升序纠正时间戳倒挂的插队顺序"
        );
    }

    /// created_at 相等时 stable sort 必须保持物理 append 顺序——不能打乱同一时刻
    /// 落盘的 assistant↔后续消息配对（架构 §4.9.5）。
    #[test]
    fn load_keeps_physical_order_for_equal_created_at() {
        let dir = temp_data_dir("reorder-stable");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let mk = |role: Role, content: &str, ts: i64| Message {
            id: new_id(),
            role,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: ts,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        append_message(&dir, &s.id, mk(Role::User, "a", 100)).unwrap();
        append_message(&dir, &s.id, mk(Role::Assistant, "b", 100)).unwrap();
        append_message(&dir, &s.id, mk(Role::User, "c", 100)).unwrap();

        let loaded = load(&dir, &s.id).unwrap();
        let order: Vec<&str> = loaded
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(order, vec!["a", "b", "c"], "相等时刻保持物理序");
    }

    #[test]
    fn rename_writes_meta_update_and_folds() {
        let dir = temp_data_dir("rename");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        rename(&dir, &s.id, "新标题".into()).unwrap();
        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.title, "新标题");
    }

    #[test]
    fn save_overwrites_meta_update_history() {
        // 多次 rename 后 save 一份新的 session，旧的 MetaUpdate 行被丢弃
        let dir = temp_data_dir("compact");
        let mut s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        rename(&dir, &s.id, "v1".into()).unwrap();
        rename(&dir, &s.id, "v2".into()).unwrap();
        s.title = "final".into();
        save(&dir, s.clone()).unwrap();

        let path = find_jsonl(&dir, &s.id).unwrap().unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "save 后应该只剩 1 行 meta");
        let parsed: RolloutLine = serde_json::from_str(lines[0]).unwrap();
        assert!(matches!(parsed, RolloutLine::Meta(_)));
        assert_eq!(load(&dir, &s.id).unwrap().title, "final");
    }

    #[test]
    fn legacy_json_is_readable_and_migrates_on_write() {
        let dir = temp_data_dir("legacy");
        // 手写一份老 .json 进去
        let id = new_id();
        let now_ts = now();
        let legacy_dir = root_dir(&dir).join(date_string(now_ts));
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join(format!("{id}.json"));
        let session = Session {
            id: id.clone(),
            title: "老对话".into(),
            provider_id: "openai".into(),
            model: "gpt-3.5".into(),
            system_prompt: None,
            prompt_id: None,
            stream: true,
            messages: vec![Message {
                id: new_id(),
                role: Role::User,
                content: "hello".into(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: now_ts,
                meta: None,
                subagent_call_id: None,
            run_duration_ms: None,
            }],
            workdir: None,
            allowed_paths: None,
            runtime_allowed_paths: Vec::new(),
            pending_runtime_allowed_paths: Vec::new(),
            enabled_tools: None,
            skill_dirs: None,
            reasoning: None,
            token_stats: None,
            source: None,
            project_id: None,
            run_mode: RunMode::default(),
            global_rules: None,
            rules_files: None,
            todos: Vec::new(),
            active_plan: None,
            active_goal: None,
            pre_plan_mode: None,
            pending_continue: None,
            created_at: now_ts,
            updated_at: now_ts,
        };
        std::fs::write(&legacy_path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();

        // 直接 load 应该读到老 json
        let loaded = load(&dir, &id).unwrap();
        assert_eq!(loaded.title, "老对话");
        assert_eq!(loaded.messages.len(), 1);

        // rename 触发迁移
        rename(&dir, &id, "升级了".into()).unwrap();
        assert!(find_jsonl(&dir, &id).unwrap().is_some());
        assert!(legacy_path.with_extension("json.bak").exists());
        assert!(!legacy_path.exists());
        assert_eq!(load(&dir, &id).unwrap().title, "升级了");
    }

    #[test]
    fn list_orders_by_updated_at_desc() {
        let dir = temp_data_dir("list");
        let s1 = save_session(&dir, "first", "msg1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let s2 = save_session(&dir, "second", "msg2");
        let metas = list(&dir).unwrap();
        let ids: Vec<_> = metas.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec![s2.id, s1.id]);
    }

    #[test]
    fn list_moves_session_to_top_after_new_message() {
        let dir = temp_data_dir("list-message-time");
        let older = save_session(&dir, "older", "msg1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let newer = save_session(&dir, "newer", "msg2");
        std::thread::sleep(std::time::Duration::from_millis(5));

        append_message(
            &dir,
            &older.id,
            Message {
                id: new_id(),
                role: Role::User,
                content: "fresh message".into(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: now(),
                meta: None,
                subagent_call_id: None,
            run_duration_ms: None,
            },
        )
        .unwrap();

        let metas = list(&dir).unwrap();
        let ids: Vec<_> = metas.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec![older.id, newer.id]);
    }

    #[test]
    fn fork_copies_messages_up_to_marker() {
        let dir = temp_data_dir("fork");
        let s = save_session(&dir, "原对话", "u1");
        let m2 = Message {
            id: new_id(),
            role: Role::Assistant,
            content: "a1".into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: now(),
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        append_message(&dir, &s.id, m2.clone()).unwrap();
        let m3 = Message {
            id: new_id(),
            role: Role::User,
            content: "u2".into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: now(),
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        append_message(&dir, &s.id, m3).unwrap();

        let forked = fork(&dir, &s.id, &m2.id).unwrap();
        assert_eq!(forked.messages.len(), 2);
        assert_eq!(forked.messages.last().unwrap().id, m2.id);
        assert!(forked.title.contains("分支"));
    }

    #[test]
    fn list_self_heals_pretty_json_session_files() {
        // 早期 desktop 把老 `<id>.json`（pretty-printed JSON）裸 rename 成
        // `<id>/session.jsonl`。本应是 jsonl 但实际是格式化 JSON：list 时每行
        // 都 "missing field type" 刷 warn。检测 + 自愈：用 JSON 解析 + 重写为
        // 合法 jsonl，下次 list 就静默。
        let dir = temp_data_dir("pretty-json-heal");
        let sid = "aee7f54d-b873-4d23-b794-6288e0a83d6f";
        let session_dir = root_dir(&dir).join(sid);
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("session.jsonl");
        // 整文件是 pretty-printed Session JSON（模仿磁盘脏数据）
        let pretty = serde_json::to_string_pretty(&serde_json::json!({
            "id": sid,
            "title": "老格式 session",
            "provider_id": "anthropic",
            "model": "claude-opus-4-7",
            "stream": true,
            "messages": [],
            "created_at": 1777272707015_i64,
            "updated_at": 1777274494974_i64,
        }))
        .unwrap();
        std::fs::write(&path, pretty).unwrap();

        // 第一次 list：检测到 pretty-JSON 并自愈写回 jsonl
        let metas = list(&dir).unwrap();
        assert!(metas.iter().any(|m| m.id == sid), "list 应能返回该 session");

        // 自愈后磁盘上的第一行必须是合法 Meta jsonl
        let healed = std::fs::read_to_string(&path).unwrap();
        let first = healed.lines().next().expect("至少一行");
        let parsed: RolloutLine = serde_json::from_str(first).expect("第一行应是合法 jsonl");
        assert!(matches!(parsed, RolloutLine::Meta(_)));
    }

    #[test]
    fn list_and_migrate_ignore_sidecar_jsonl_with_dot_in_stem() {
        // 形如 `<sid>.model_io.jsonl` 的 dump sidecar（HEBBIAN_DUMP_MODEL_IO 触发，
        // 旧版本一度把它平铺写在 sessions/ 根目录）：list 不能把它当 session；
        // 老布局迁移也不能把 `<sid>.model_io` 错当 session_id 迁成新目录。
        let dir = temp_data_dir("model-io-sidecar");
        let real = save_session(&dir, "real", "hi");
        let sidecar = root_dir(&dir).join(format!("{}.model_io.jsonl", real.id));
        std::fs::write(&sidecar, r#"{"ts":"2026","run_id":"r","turn":1}"#).unwrap();
        // 同名「脏目录」：旧 bug 留下的副产物 `<sid>.model_io/session.jsonl`
        let dirty = root_dir(&dir).join(format!("{}.model_io", real.id));
        std::fs::create_dir_all(&dirty).unwrap();
        std::fs::write(dirty.join("session.jsonl"), "{}\n").unwrap();

        let metas = list(&dir).unwrap();
        let ids: Vec<&str> = metas.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec![real.id.as_str()]);

        let moved = migrate_legacy_layout_if_needed(&dir).unwrap();
        assert_eq!(moved, 0, "sidecar 不能被 legacy migrate 迁走");
        assert!(sidecar.exists(), "sidecar 文件保持原位");
    }

    #[test]
    fn list_skips_legacy_recorder_rollout_files() {
        // 老 CLI 用 agent_core::Recorder 写的孤儿事件流文件应该被忽略，
        // 不能让 list 报 "missing field type"。
        let dir = temp_data_dir("orphan");
        let s = save_session(&dir, "real", "hi");
        let orphan = root_dir(&dir).join("rollout-20260509T042931-abc.jsonl");
        std::fs::write(
            &orphan,
            // 裸 Event 行，没有 type 包装，会让 RolloutLine 反序列化报错
            r#"{"id":"x","seq":1,"run_id":"r","payload":{"TextDelta":{"text":"hi"}}}"#,
        )
        .unwrap();
        let metas = list(&dir).unwrap();
        let ids: Vec<&str> = metas.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec![s.id.as_str()]);
    }

    #[test]
    fn delete_removes_jsonl_and_legacy() {
        let dir = temp_data_dir("delete");
        let s = save_session(&dir, "tbd", "hi");
        delete(&dir, &s.id).unwrap();
        assert!(find_jsonl(&dir, &s.id).unwrap().is_none());
        assert!(find_legacy_json(&dir, &s.id).unwrap().is_none());
    }

    #[test]
    fn regex_search_matches_titles_and_message_content() {
        let dir = temp_data_dir("regex-global");
        let title_hit = save_session(&dir, "Release 2026 Notes", "nothing here");
        let content_hit = save_session(&dir, "Planning", "error 502 happened");
        save_session(&dir, "Scratch", "error abc happened");

        let hits = search(&dir, r"\d{3}", false, true).expect("regex search");
        let ids: Vec<_> = hits.iter().map(|hit| hit.meta.id.as_str()).collect();

        assert!(ids.contains(&title_hit.id.as_str()));
        assert!(ids.contains(&content_hit.id.as_str()));
        assert_eq!(ids.len(), 2);
        assert_eq!(
            hits.iter()
                .find(|hit| hit.meta.id == title_hit.id)
                .expect("title hit")
                .matched_in,
            "title"
        );
        assert_eq!(
            hits.iter()
                .find(|hit| hit.meta.id == content_hit.id)
                .expect("content hit")
                .matched_in,
            "content"
        );
    }

    /// 回归：上次进程中断时残留在 partial sidecar 的流式输出，必须能在 surface 下次
    /// 加载会话时折叠进 session.jsonl，并满足三条规则：
    /// 1. 无 `name` 的 tool_call 直接丢弃——流式 delta 没透首帧 name 时只剩残缺 args，
    ///    保留会让模型读到 "unknown" 误以为真的发起过那次调用
    /// 2. 有 `name` 的 tool_call 保留，arguments 即便不是合法 JSON 也保留原文
    /// 3. 残片末尾追加 [`INTERRUPTED_TAIL_NOTICE`]——同时进 part 与 content，
    ///    模型读 transcript 能看到"上一轮没走完"
    /// 4. 紧跟一条 `Interrupted` marker；partial 文件被删除
    #[test]
    fn load_with_partial_recovery_folds_residue_and_drops_unnamed_tool_calls() {
        let dir = temp_data_dir("partial-recovery");
        let s = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();

        // 模拟流式中断后磁盘上残留的 partial：text + reasoning + 一个有名 tool_call
        // (Bash) + 一个无名 tool_call（只有 args chunk，没透 name）。
        let msg_id = "msg-interrupted";
        super::super::sessions_dir::append_partial(
            &dir,
            &s.id,
            msg_id,
            &super::super::sessions_dir::PartialFragment::Reasoning {
                text: "想想看…".into(),
            },
        )
        .unwrap();
        super::super::sessions_dir::append_partial(
            &dir,
            &s.id,
            msg_id,
            &super::super::sessions_dir::PartialFragment::Text {
                text: "我打算先".into(),
            },
        )
        .unwrap();
        super::super::sessions_dir::append_partial(
            &dir,
            &s.id,
            msg_id,
            &super::super::sessions_dir::PartialFragment::ToolCall {
                index: 0,
                name: Some("Bash".into()),
                arguments_chunk: r#"{"command":"l"#.into(),
            },
        )
        .unwrap();
        super::super::sessions_dir::append_partial(
            &dir,
            &s.id,
            msg_id,
            &super::super::sessions_dir::PartialFragment::ToolCall {
                index: 1,
                name: None,
                arguments_chunk: r#"{"path":""#.into(),
            },
        )
        .unwrap();

        let loaded = load_with_partial_recovery(&dir, &s.id).unwrap();
        assert_eq!(
            loaded.messages.len(),
            2,
            "应追加 1 条 assistant + 1 条 Interrupted marker"
        );
        let assistant = &loaded.messages[0];
        let marker = &loaded.messages[1];

        assert_eq!(assistant.role, Role::Assistant);
        assert!(
            assistant.content.ends_with(INTERRUPTED_TAIL_NOTICE),
            "content 末尾必须带中断话术，便于 AI 读历史时识别：{}",
            assistant.content
        );
        assert!(
            assistant.content.contains("我打算先"),
            "content 必须保留中断前的可见文本：{}",
            assistant.content
        );

        let part_kinds: Vec<&str> = assistant
            .parts
            .iter()
            .map(|p| match p {
                MessagePart::Reasoning { .. } => "reasoning",
                MessagePart::Text { .. } => "text",
                MessagePart::ToolCall { .. } => "tool_call",
            })
            .collect();
        assert_eq!(
            part_kinds,
            vec!["reasoning", "text", "tool_call", "text"],
            "parts 顺序：reasoning / 残片文本 / 仅保留 Bash 一个 tool_call / 末尾中断话术"
        );

        let tool_call_names: Vec<&str> = assistant
            .parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_call_names,
            vec!["Bash"],
            "无名 tool_call 必须被丢弃，仅保留有名"
        );
        assert_eq!(assistant.tool_calls.len(), 1);
        assert_eq!(assistant.tool_calls[0].name, "Bash");

        assert_eq!(marker.role, Role::Marker);
        assert!(matches!(marker.meta, Some(MessageMeta::Interrupted)));

        // partial 主文件应被删除（避免下次再被恢复一遍）；.lock 文件 best-effort 留底。
        let partial_main = dir
            .join("sessions")
            .join(&s.id)
            .join("partial")
            .join(format!("{msg_id}.partial.jsonl"));
        assert!(
            !partial_main.exists(),
            "partial 主文件应被删除：{}",
            partial_main.display()
        );

        // 二次 load 不再追加（恢复是幂等的，否则每次刷新都会插一对 assistant+marker）。
        let loaded_again = load_with_partial_recovery(&dir, &s.id).unwrap();
        assert_eq!(loaded_again.messages.len(), 2, "幂等：重复加载不应再次追加");
    }

    #[test]
    fn regex_search_respects_case_sensitivity() {
        let dir = temp_data_dir("regex-case");
        let session = save_session(&dir, "Build", "Error 500 happened");

        let insensitive = search(&dir, "error \\d+", false, true).expect("search insensitive");
        assert_eq!(insensitive.len(), 1);
        assert_eq!(insensitive[0].meta.id, session.id);

        let sensitive = search(&dir, "error \\d+", true, true).expect("search sensitive");
        assert!(sensitive.is_empty());
    }

    /// 转发痕迹即写即落（架构 §7.5.1）：转发时落 Pending marker，机主回复后原地
    /// 更新为 Resolved——两个事实都成为 session.jsonl 可回溯内容，且重启后仍在。
    #[test]
    fn channel_forward_marker_persists_and_resolves() {
        let dir = temp_data_dir("channel-forward-marker");
        let session = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();

        let marker_id = append_channel_forward_marker(
            &dir,
            &session.id,
            "wechat".into(),
            ChannelForwardKind::Approval,
        )
        .unwrap();

        // 转发后：marker 落盘且为 Pending。
        let after_forward = load(&dir, &session.id).unwrap();
        let marker = after_forward
            .messages
            .iter()
            .find(|m| m.id == marker_id)
            .expect("转发 marker 必须落盘");
        assert!(matches!(marker.role, Role::Marker));
        assert!(matches!(
            &marker.meta,
            Some(MessageMeta::ChannelForward {
                status: ChannelForwardStatus::Pending,
                kind: ChannelForwardKind::Approval,
                ..
            })
        ));

        // 机主回复后：同一条 marker 原地更新为 Resolved，结论可读回。
        resolve_channel_forward_marker(&dir, &session.id, &marker_id, "已通过".into()).unwrap();
        let reloaded = load(&dir, &session.id).unwrap();
        let resolved = reloaded
            .messages
            .iter()
            .find(|m| m.id == marker_id)
            .expect("更新后 marker 仍在");
        match &resolved.meta {
            Some(MessageMeta::ChannelForward {
                status: ChannelForwardStatus::Resolved { outcome },
                ..
            }) => assert_eq!(outcome, "已通过"),
            other => panic!("期望 Resolved，实际 {other:?}"),
        }
        // 不应新增消息——是原地更新而非追加。
        assert_eq!(after_forward.messages.len(), reloaded.messages.len());
    }

    /// 流式可见性（架构 §7.8.5 步骤⑥）：run 进行中（partial 写者持 `.live` 锁）时，
    /// `load_with_partial_recovery` 应把活 partial 的已累积内容读出来拼进 Session
    /// （进行中渲染，**不加「输出中断」话术、不折盘、不删 partial**）。
    ///
    /// A/B：旧逻辑「活 partial 直接跳过」→ load 看不到流式内容（messages 不含它）。
    /// 新逻辑 → messages 末尾出现一条含流式文本、不带中断话术的 assistant。
    #[test]
    fn live_partial_is_rendered_not_folded() {
        use super::super::sessions_dir::{self, PartialFragment, PartialLiveGuard};

        let dir = temp_data_dir("live-partial-render");
        let session = create(&dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let sid = session.id.clone();
        let msg_id = "live-msg-1";

        // 模拟 hebcore run 进行中：持活性锁 + 写两帧流式 text。
        let _guard = PartialLiveGuard::acquire(&dir, &sid, msg_id).unwrap();
        sessions_dir::append_partial(
            &dir,
            &sid,
            msg_id,
            &PartialFragment::Text {
                text: "正在流式".into(),
            },
        )
        .unwrap();
        sessions_dir::append_partial(
            &dir,
            &sid,
            msg_id,
            &PartialFragment::Text {
                text: "输出中".into(),
            },
        )
        .unwrap();

        // surface 加载会话：应看到活 partial 的流式内容。
        let loaded = load_with_partial_recovery(&dir, &sid).unwrap();
        let live = loaded
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("活 partial 应被读出渲染成 assistant message");
        assert_eq!(live.content, "正在流式输出中", "活 partial 流式文本应完整读出");
        assert!(
            !live.content.contains(INTERRUPTED_TAIL_NOTICE),
            "活 partial 不该带「输出中断」话术（它还在跑）"
        );

        // 不该落盘：纯读 jsonl（不含 partial 恢复）应仍无这条 assistant。
        let raw = load(&dir, &sid).unwrap();
        assert!(
            !raw.messages.iter().any(|m| m.role == Role::Assistant),
            "活 partial 不该被折盘进 jsonl（hebcore run 收尾会正式落盘，折盘会重复）"
        );

        // partial 文件仍在（没被删）。
        assert!(
            sessions_dir::partial_path(&dir, &sid, msg_id).exists(),
            "活 partial 文件不该被删除"
        );

        // 多次 load 同一活 partial 返回稳定 id（前端按 id 去重不重复渲染）。
        let loaded2 = load_with_partial_recovery(&dir, &sid).unwrap();
        let id2 = loaded2
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.id.clone())
            .unwrap();
        assert_eq!(live.id, id2, "同一活 partial 多次 load 应返回稳定 id");
    }
}
