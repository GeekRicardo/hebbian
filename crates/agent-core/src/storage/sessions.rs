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
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::run_mode::RunMode;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Marker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// 老 jsonl 无此字段反序列化默认 [`RunMode::AskBeforeEdits`]。
    #[serde(default)]
    pub run_mode: RunMode,
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
}

impl TokenStats {
    pub fn accumulate(&mut self, delta: TokenStats) {
        self.input_tokens += delta.input_tokens;
        self.output_tokens += delta.output_tokens;
        self.cache_read_tokens += delta.cache_read_tokens;
        self.cache_creation_tokens += delta.cache_creation_tokens;
        self.run_count += delta.run_count;
    }
}

fn default_stream() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
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
}

#[derive(Debug, Serialize)]
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
    /// [`RunMode`] 起始快照。老 RolloutMeta 无此字段反序列化为 `AskBeforeEdits`。
    #[serde(default)]
    pub run_mode: RunMode,
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
    session.updated_at = latest_ts.max(session.created_at);
    Ok(session)
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
        let session = match load_from_path(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let count = session
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::Marker))
            .count();
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
        });
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn load(data_dir: &Path, id: &str) -> AppResult<Session> {
    if let Some(p) = find_jsonl(data_dir, id)? {
        return read_jsonl(&p);
    }
    if let Some(p) = find_legacy_json(data_dir, id)? {
        return common::storage::read_json_required(&p);
    }
    Err(AppError::msg(format!("session {id} not found")))
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

/// 全量写入（compaction）。会清掉过去所有 MetaUpdate 行的痕迹。
pub fn save(data_dir: &Path, mut s: Session) -> AppResult<Session> {
    s.updated_at = now();
    let target = jsonl_path_for(data_dir, &s.id, s.created_at)?;
    let source = preserve_source(data_dir, &s.id).unwrap_or_else(default_source);
    write_jsonl_full(&target, &s, source)?;
    archive_legacy_json(data_dir, &s.id);
    Ok(s)
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
        reasoning: None,
        token_stats: None,
        source: Some(source.clone()),
        project_id: None,
        run_mode: RunMode::default(),
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

pub fn append_message(data_dir: &Path, id: &str, msg: Message) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
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
        },
    )
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
pub fn set_run_mode(data_dir: &Path, id: &str, mode: RunMode) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            run_mode: Some(mode),
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
            },
        )
        .expect("append message")
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
        assert_eq!(loaded.allowed_paths, Some(vec![PathBuf::from("/tmp/extra")]));
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
        };
        append_message(&dir, &s.id, msg.clone()).unwrap();
        let loaded = load(&dir, &s.id).unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "hi");
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
}
