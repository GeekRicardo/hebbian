//! 记忆系统落盘（架构 §4.14 / §6）。
//!
//! 两个作用域：
//! - `Global`：`<data_dir>/memory/<slug>.md`——跨项目的用户画像 / 偏好 / 通用习惯。
//! - `Project`：`<data_dir>/projects/<enc(workdir)>/memory/<slug>.md`——某项目的结构 /
//!   架构 / 约定 / 坑，解决「同一项目每次新对话都要重新探索」。
//!
//! 一条记忆 = 一个 md 文件，frontmatter 携带 L0 摘要供注入初筛，正文是 L1 概览 + L2
//! 详情。所有读写必经 [`super::lock`]（共享 `~/.hebbian/` 的多 surface 并发安全）。
//!
//! frontmatter 故意手写极简解析而非引 YAML 库：字段全是单行 `key: value`，五个固定键，
//! 手写十几行即可，省一个依赖。代价是 value 不能跨行——`write` 会把 summary 内的换行
//! 压成空格保证这一前提成立。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use common::{AppError, AppResult};

use super::{lock, projects};

/// 记忆系统统一日志（架构 §4.14）：所有记忆动作都过这两个宏，第一个参数是动作分类
/// （`Write` / `Read` / `Query` / `Cursor` / `Extract` / `Inject`），输出形如
/// `[Memory:Write] ...`。这样既能 `grep '[Memory:Write]'` 单看某类，也能 `grep '[Memory:'`
/// 捞全部；同时挂 `target = "memory"`，支持 `RUST_LOG=memory=debug` 单独给记忆模块调级。
macro_rules! mem_log {
    ($cat:expr, $($arg:tt)*) => {
        ::tracing::info!(target: "memory", "[Memory:{}] {}", $cat, format_args!($($arg)*))
    };
}
macro_rules! mem_warn {
    ($cat:expr, $($arg:tt)*) => {
        ::tracing::warn!(target: "memory", "[Memory:{}] {}", $cat, format_args!($($arg)*))
    };
}
pub(crate) use {mem_log, mem_warn};

/// 记忆作用域。`prefix()` 是 id 里的可读前缀，也是注入清单中 `[proj/xxx]` 的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    Global,
    Project,
}

impl MemoryScope {
    pub fn prefix(self) -> &'static str {
        match self {
            MemoryScope::Global => "global",
            MemoryScope::Project => "proj",
        }
    }

    fn from_prefix(s: &str) -> Option<Self> {
        match s {
            "global" => Some(MemoryScope::Global),
            "proj" => Some(MemoryScope::Project),
            _ => None,
        }
    }
}

/// 记忆时效性二分（架构 §4.14）。决定要不要遗忘衰减。
///
/// 这一刀几乎无歧义（有没有「发生时间」）；更细的主题分类全交给 [`MemoryL0::tags`]
/// 自由标签，不做硬类目——避免「一条记忆强行归到某个桶」的边界争议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// 跨会话稳定的事实——X 在哪、为什么这么设计、红线、用户长期偏好。注入主力。
    #[default]
    Stable,
    /// 发生过的具体事件，带时间——「2026-06 修好 partial sidecar，根因 BufWriter」。会衰减。
    Episode,
}

impl MemoryKind {
    fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Stable => "stable",
            MemoryKind::Episode => "episode",
        }
    }

    fn from_str(s: &str) -> Self {
        match s.trim() {
            "episode" => MemoryKind::Episode,
            _ => MemoryKind::Stable,
        }
    }
}

/// 读取层级（架构 §4.14 的 L1/L0L2）。`Overview` 只回正文「## 概览」段，
/// `Full` 回整篇正文。短记忆没有概览段时 `Overview` 回退为整篇。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLevel {
    Overview,
    Full,
}

/// 注入用的 L0 条目：模型扫一眼 summary 自己挑，要详情再 `ReadMemory(id)`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryL0 {
    pub id: String,
    pub summary: String,
    pub category: String,
    /// 时效性二分（架构 §4.14）。老记忆缺字段默认 `stable`。
    #[serde(default)]
    pub kind: MemoryKind,
    /// 自由主题标签（架构 §4.14）。服务激活扩散的检索/聚类，不做硬桶。
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `.memory_log.jsonl` 的一行：后台抽取 / 主动写入的审计记录（只增）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLogEntry {
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// `wrote` | `extracted` | `failed`。
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub detail: String,
}

impl MemoryLogEntry {
    pub fn new(outcome: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            turn_id: None,
            outcome: outcome.into(),
            model: None,
            attempts: None,
            detail: detail.into(),
        }
    }
}

// ── 路径定位 ───────────────────────────────────────────────────────────────

/// 某作用域的 memory 根目录。`Project` 必须给 workdir，否则报错（降级由调用方决定）。
fn memory_root(data_dir: &Path, workdir: Option<&Path>, scope: MemoryScope) -> AppResult<PathBuf> {
    match scope {
        MemoryScope::Global => Ok(data_dir.join("memory")),
        MemoryScope::Project => {
            let wd = workdir
                .ok_or_else(|| AppError::msg("project 记忆需要 workdir，但当前对话未绑定项目"))?;
            Ok(projects::project_dir(data_dir, wd).join("memory"))
        }
    }
}

fn record_path(root: &Path, slug: &str) -> PathBuf {
    root.join(format!("{slug}.md"))
}

/// `<prefix>/<slug>`——注入清单展示、`ReadMemory` 入参。
pub fn make_id(scope: MemoryScope, slug: &str) -> String {
    format!("{}/{}", scope.prefix(), slug)
}

/// 反解析 id → (scope, slug)。非法前缀返回 None。
pub fn parse_id(id: &str) -> Option<(MemoryScope, String)> {
    let (prefix, slug) = id.split_once('/')?;
    let scope = MemoryScope::from_prefix(prefix)?;
    let slug = sanitize_slug(slug);
    if slug.is_empty() {
        return None;
    }
    Some((scope, slug))
}

// ── 公开 API ───────────────────────────────────────────────────────────────

/// upsert 一条记忆。`slug` 是稳定身份——同 slug 重复写即覆盖更新（让调用方控制
/// 「更新已有」还是「新建」的粒度）。返回该条的 L0 供事件 / 注入使用。
///
/// `importance` / `last_active` 由系统管理（写入时取默认 / now），不暴露给调用方——
/// 它们是激活强化 / 遗忘的派生状态，由深睡重算，不该让抽取器或工具直接设定。
pub fn write(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
    slug: &str,
    kind: MemoryKind,
    category: &str,
    tags: &[String],
    summary: &str,
    body: &str,
) -> AppResult<MemoryL0> {
    let slug = sanitize_slug(slug);
    if slug.is_empty() {
        return Err(AppError::msg("记忆 slug 不能为空"));
    }
    let category = sanitize_inline(category);
    let summary = sanitize_inline(summary);
    let tags: Vec<String> = tags
        .iter()
        .map(|t| sanitize_inline(t))
        .filter(|t| !t.is_empty())
        .collect();
    let id = make_id(scope, &slug);

    let root = memory_root(data_dir, workdir, scope)?;
    let path = record_path(&root, &slug);
    let now = chrono::Utc::now().to_rfc3339();
    // upsert：保留已有记忆的 importance（激活强化的成果不能被重写清零）；新记忆取默认。
    let importance = read_existing(&path)
        .map(|r| r.importance)
        .unwrap_or(DEFAULT_IMPORTANCE);
    let rec = MemoryRecord {
        id: id.clone(),
        scope,
        category: category.clone(),
        summary: summary.clone(),
        kind,
        tags: tags.clone(),
        importance,
        last_active: now.clone(),
        updated_at: now,
        body: body.trim().to_string(),
    };
    lock::write_atomic(&path, render_md(&rec).as_bytes())?;
    // L0 预览（id + kind + category + summary）+ 落盘绝对路径，便于直接定位刚写的文件。
    mem_log!(
        "Write",
        "{id} kind={} category={category} L0={summary:?} → {}",
        kind.as_str(),
        path.display()
    );

    Ok(MemoryL0 {
        id,
        summary,
        category,
        kind,
        tags,
    })
}

/// 读已存在的记忆文件（不存在 / 解析失败 → None）。供 `write` 的 upsert 保留 importance。
fn read_existing(path: &Path) -> Option<MemoryRecord> {
    let bytes = lock::read_locked(path).ok()?;
    parse_md(&String::from_utf8_lossy(&bytes))
}

/// 列某作用域的全部 L0。目录不存在 → 空 vec（不是错误：新项目还没记忆很正常）。
/// 按 id 排序保证注入清单稳定（不随文件系统枚举顺序抖动，利于 prompt 可读 / diff）。
pub fn list_l0(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
) -> AppResult<Vec<MemoryL0>> {
    let root = memory_root(data_dir, workdir, scope)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            mem_log!("Query", "{} 作用域 0 条（尚无记忆目录）", scope.prefix());
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.into()),
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let bytes = lock::read_locked(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        if let Some(rec) = parse_md(&text) {
            out.push(MemoryL0 {
                id: rec.id,
                summary: rec.summary,
                category: rec.category,
                kind: rec.kind,
                tags: rec.tags,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    mem_log!("Query", "{} 作用域 {} 条", scope.prefix(), out.len());
    Ok(out)
}

/// 读一条记忆的详情。`Overview` 抽「## 概览」段，没有则回整篇。
pub fn read(
    data_dir: &Path,
    workdir: Option<&Path>,
    id: &str,
    level: MemoryLevel,
) -> AppResult<String> {
    let (scope, slug) = parse_id(id).ok_or_else(|| AppError::msg(format!("非法记忆 id：{id}")))?;
    mem_log!("Read", "{id} level={level:?}");
    let root = memory_root(data_dir, workdir, scope)?;
    let path = record_path(&root, &slug);
    let bytes = lock::read_locked(&path).map_err(|_| AppError::msg(format!("记忆不存在：{id}")))?;
    let text = String::from_utf8_lossy(&bytes);
    let rec = parse_md(&text).ok_or_else(|| AppError::msg(format!("记忆解析失败：{id}")))?;
    match level {
        MemoryLevel::Full => Ok(rec.body),
        MemoryLevel::Overview => Ok(extract_overview(&rec.body).unwrap_or(rec.body)),
    }
}

// ── 激活强化 / 遗忘衰减（Hebbian 闭环，架构 §4.14.8）──────────────────────────
//
// importance ∈ [0,1] 是每条记忆的当前重要度，喂给激活扩散的种子打分（越高越易浮现）。
// 它由一条纯函数从「距上次激活的时长」+ kind 单向导出（[`importance_from_age`]）：
//   - **强化（fire）**：召回命中一条记忆即 `bump_last_active` 把 last_active 打到 now →
//     age=0 → importance 重回上限。频繁用到的记忆自然保持高位。
//   - **衰减（forget）**：深睡 `consolidate_importance` 按 last_active 时效重算全量 →
//     久不激活的记忆按 kind 半衰期滑向地板；episode 半衰期短、地板低，最终成遗忘候选。
// 两端共用同一条 recency 曲线，无需额外记「激活计数」——last_active 一个时间戳承载全部状态。

/// stable 记忆地板（跨会话稳定事实，衰减慢、留得住）。
const STABLE_FLOOR: f32 = 0.4;
/// episode 记忆地板（具体事件，衰减快、可遗忘）。
const EPISODE_FLOOR: f32 = 0.1;
/// stable 半衰期（天）：约半年不激活才掉到地板中点。
const STABLE_HALF_LIFE_DAYS: f32 = 180.0;
/// episode 半衰期（天）：约一个月不激活即明显褪色。
const EPISODE_HALF_LIFE_DAYS: f32 = 30.0;
/// episode importance 跌破此值 → 遗忘候选（约 5.5 个月未激活）。
const FORGET_IMPORTANCE: f32 = 0.12;

/// 纯函数：由「距上次激活的天数」+ kind 导出当前 importance。
/// `age_days=0`（刚被激活）→ 上限 1.0；随时长按 kind 半衰期指数滑向地板。
/// 单调、无副作用——强化与衰减都落在这一条曲线上，便于单测钉死。
fn importance_from_age(kind: MemoryKind, age_days: f32) -> f32 {
    let (floor, half_life) = match kind {
        MemoryKind::Stable => (STABLE_FLOOR, STABLE_HALF_LIFE_DAYS),
        MemoryKind::Episode => (EPISODE_FLOOR, EPISODE_HALF_LIFE_DAYS),
    };
    let age = age_days.max(0.0);
    let recency = 0.5_f32.powf(age / half_life); // 1.0 刚激活 → 0 久远
    (floor + (1.0 - floor) * recency).clamp(floor, 1.0)
}

/// 距 RFC3339 时刻的天数（解析失败 → 0，按「刚激活」保守处理，不误伤）。
fn age_days_since(rfc3339: &str, now: chrono::DateTime<chrono::Utc>) -> f32 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() as f32 / 86_400.0)
        .unwrap_or(0.0)
        .max(0.0)
}

/// 召回命中即强化：把一条记忆的 last_active 打到 now（age 归零 → importance 回上限）。
/// 只改 last_active + updated_at 不动正文，走 upsert 同款原子写。off 热路径调用（见 session.rs）。
pub fn bump_last_active(data_dir: &Path, workdir: Option<&Path>, id: &str) -> AppResult<()> {
    let (scope, slug) = parse_id(id).ok_or_else(|| AppError::msg(format!("非法记忆 id：{id}")))?;
    let root = memory_root(data_dir, workdir, scope)?;
    let path = record_path(&root, &slug);
    let Some(mut rec) = read_existing(&path) else {
        return Ok(()); // 记忆已被删/不存在：强化无对象，静默跳过
    };
    rec.last_active = chrono::Utc::now().to_rfc3339();
    lock::write_atomic(&path, render_md(&rec).as_bytes())?;
    Ok(())
}

/// 深睡衰减一轮的结果。
#[derive(Debug, Default, Clone)]
pub struct ConsolidateReport {
    /// 本轮 importance 被重算写回的条数。
    pub recomputed: usize,
    /// 跌破遗忘阈值的 episode id（**不自动删**——删用户记忆是破坏性操作，只标记 + 审计，
    /// 低 importance 已让它们在召回排序里自然沉底，软遗忘足够；硬删留人工/后续显式开关）。
    pub forget_candidates: Vec<String>,
}

/// 深睡尾部调用：按 last_active 时效重算某作用域全量 importance 并写回。
/// 目录不存在 → 空报告（新项目无记忆，非错误）。
pub fn consolidate_importance(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
) -> AppResult<ConsolidateReport> {
    let root = memory_root(data_dir, workdir, scope)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConsolidateReport::default()),
        Err(e) => return Err(e.into()),
    };
    let now = chrono::Utc::now();
    let mut report = ConsolidateReport::default();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Some(mut rec) = read_existing(&path) else {
            continue;
        };
        let next = importance_from_age(rec.kind, age_days_since(&rec.last_active, now));
        // 只在有实际变化时写回，避免深睡把全量记忆无谓 rewrite。
        if (next - rec.importance).abs() > f32::EPSILON {
            rec.importance = next;
            lock::write_atomic(&path, render_md(&rec).as_bytes())?;
            report.recomputed += 1;
        }
        if matches!(rec.kind, MemoryKind::Episode) && rec.importance <= FORGET_IMPORTANCE {
            report.forget_candidates.push(rec.id.clone());
        }
    }
    mem_log!(
        "Sleep",
        "{} 作用域 importance 重算 {} 条，遗忘候选 {} 条",
        scope.prefix(),
        report.recomputed,
        report.forget_candidates.len()
    );
    Ok(report)
}

/// 某作用域 id → importance 映射，供召回种子打分读取（reinforced 记忆更易浮现）。
/// 目录不存在 → 空表。
pub fn load_importance(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
) -> AppResult<std::collections::HashMap<String, f32>> {
    let root = memory_root(data_dir, workdir, scope)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::HashMap::new())
        }
        Err(e) => return Err(e.into()),
    };
    let mut out = std::collections::HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if let Some(rec) = read_existing(&path) {
            out.insert(rec.id, rec.importance);
        }
    }
    Ok(out)
}

/// 追加一条审计日志到该作用域 memory 目录下的 `.memory_log.jsonl`。
pub fn append_log(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
    entry: &MemoryLogEntry,
) -> AppResult<()> {
    let root = memory_root(data_dir, workdir, scope)?;
    let path = root.join(".memory_log.jsonl");
    let line = serde_json::to_string(entry)?;
    lock::append_jsonl(&path, &line)
}

// ── 关联网络 links.jsonl（架构 §4.14）────────────────────────────────────────
//
// Hebbian 边：两条记忆的关联强度。共现 → 加权（fire together, wire together），
// 长期不共现 → 深睡衰减、归零删除。单机几百条记忆用邻接表足够，不上图数据库。
// 整张表深睡时重算后整体覆盖落盘（`save_links`），平时只读（`load_links`）供激活扩散。

/// 一条关联边。`weight ∈ [0,1]`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryLink {
    pub from: String,
    pub to: String,
    pub weight: f32,
    #[serde(default)]
    pub updated_at: String,
}

fn links_path(root: &Path) -> PathBuf {
    root.join("links.jsonl")
}

/// 读某作用域的全部关联边。文件不存在 → 空 vec。坏行跳过（容错）。
pub fn load_links(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
) -> AppResult<Vec<MemoryLink>> {
    let root = memory_root(data_dir, workdir, scope)?;
    let bytes = match lock::read_locked(&links_path(&root)) {
        Ok(b) => b,
        Err(_) => return Ok(Vec::new()),
    };
    let text = String::from_utf8_lossy(&bytes);
    let links = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<MemoryLink>(l).ok())
        .collect();
    Ok(links)
}

/// 整体覆盖落盘关联网络（深睡重算后调用）。原子写：`weight<=0` 的边在调用方剔除后传入。
pub fn save_links(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
    links: &[MemoryLink],
) -> AppResult<()> {
    let root = memory_root(data_dir, workdir, scope)?;
    let mut buf = String::new();
    for l in links {
        buf.push_str(&serde_json::to_string(l)?);
        buf.push('\n');
    }
    lock::write_atomic(&links_path(&root), buf.as_bytes())?;
    mem_log!("Link", "{} 作用域 {} 条边落盘", scope.prefix(), links.len());
    Ok(())
}

// ── 抽取游标（架构 §4.14）────────────────────────────────────────────────────
//
// 「上一次成功抽取覆盖到的 message id」。后台抽取成功 → 推进；失败 → 不动，下次从同一
// 游标起重抽（自动补抽失败那段）。独立小文件存在 session 目录下，不污染 §4.9.1 meta.json
// 的最小字段集——游标是记忆系统的派生状态，丢了大不了多抽一轮（去重兜底），不进 jsonl
// 的强一致体系。

fn cursor_path(data_dir: &Path, session_id: &str) -> PathBuf {
    super::sessions_dir::session_dir(data_dir, session_id).join("memory_cursor")
}

/// 读抽取游标；不存在（从未抽过）返回 `None`。
pub fn read_cursor(data_dir: &Path, session_id: &str) -> Option<String> {
    let bytes = lock::read_locked(&cursor_path(data_dir, session_id)).ok()?;
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 推进抽取游标到 `message_id`。
pub fn write_cursor(data_dir: &Path, session_id: &str, message_id: &str) -> AppResult<()> {
    let _ = super::sessions_dir::ensure_session_dirs(data_dir, session_id);
    lock::write_atomic(&cursor_path(data_dir, session_id), message_id.as_bytes())?;
    mem_log!("Cursor", "session={session_id} 推进 -> {message_id}");
    Ok(())
}

/// 清除抽取游标。历史回灌需要从第一条消息重抽时使用；文件不存在视为成功。
pub fn clear_cursor(data_dir: &Path, session_id: &str) -> AppResult<()> {
    let path = cursor_path(data_dir, session_id);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            mem_log!("Cursor", "session={session_id} 清除");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::msg(format!("清除记忆抽取游标失败：{e}"))),
    }
}

// ── frontmatter 序列化 / 解析（手写极简） ────────────────────────────────────

struct MemoryRecord {
    id: String,
    scope: MemoryScope,
    category: String,
    summary: String,
    /// 时效性二分（架构 §4.14）。
    kind: MemoryKind,
    /// 自由主题标签（架构 §4.14）。
    tags: Vec<String>,
    /// 激活强化 / 时间衰减后的当前重要度 [0,1]。深睡重算。
    importance: f32,
    /// 上次被激活的时刻（RFC3339）。遗忘判据。
    last_active: String,
    updated_at: String,
    body: String,
}

/// 默认重要度——新记忆中性起点，深睡按联结度 / 激活频率重算。
const DEFAULT_IMPORTANCE: f32 = 0.5;

fn render_md(rec: &MemoryRecord) -> String {
    format!(
        "---\nid: {}\nscope: {}\nkind: {}\ncategory: {}\ntags: {}\nsummary: {}\nimportance: {}\nlast_active: {}\nupdated_at: {}\n---\n{}\n",
        rec.id,
        rec.scope.prefix(),
        rec.kind.as_str(),
        rec.category,
        rec.tags.join(", "),
        rec.summary,
        rec.importance,
        rec.last_active,
        rec.updated_at,
        rec.body,
    )
}

/// 解析 frontmatter + 正文。不以 `---` 开头视为无 frontmatter（整篇是 body，其余字段空）。
/// 新增字段（kind/tags/importance/last_active）对老记忆缺失时走默认值——向后兼容。
fn parse_md(text: &str) -> Option<MemoryRecord> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let header = &rest[..end];
    let body = &rest[end + 5..]; // 跳过 "\n---\n"

    let mut id = String::new();
    let mut scope = MemoryScope::Global;
    let mut category = String::new();
    let mut summary = String::new();
    let mut kind = MemoryKind::Stable;
    let mut tags: Vec<String> = Vec::new();
    let mut importance = DEFAULT_IMPORTANCE;
    let mut last_active = String::new();
    let mut updated_at = String::new();
    for line in header.lines() {
        let Some((k, v)) = line.split_once(": ") else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "id" => id = v.to_string(),
            "scope" => scope = MemoryScope::from_prefix(v).unwrap_or(MemoryScope::Global),
            "kind" => kind = MemoryKind::from_str(v),
            "category" => category = v.to_string(),
            "tags" => {
                tags = v
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            }
            "summary" => summary = v.to_string(),
            "importance" => importance = v.parse().unwrap_or(DEFAULT_IMPORTANCE),
            "last_active" => last_active = v.to_string(),
            "updated_at" => updated_at = v.to_string(),
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    // 老记忆没有 last_active → 退回 updated_at（首次激活前以更新时间为准）。
    if last_active.is_empty() {
        last_active = updated_at.clone();
    }
    Some(MemoryRecord {
        id,
        scope,
        category,
        summary,
        kind,
        tags,
        importance,
        last_active,
        updated_at,
        body: body.trim().to_string(),
    })
}

/// 抽正文里第一个「## 概览」段（到下一个 `## ` 标题或文末）。
fn extract_overview(body: &str) -> Option<String> {
    let start = body.find("## 概览")?;
    let after = &body[start..];
    let rest = &after["## 概览".len()..];
    let end = rest.find("\n## ").map(|i| i + "## 概览".len());
    let section = match end {
        Some(e) => &after[..e],
        None => after,
    };
    Some(section.trim().to_string())
}

// ── 字符串清洗 ───────────────────────────────────────────────────────────────

/// slug 受限字符集：小写字母 / 数字 / `-`，其余转 `-`，去重连续 `-` 并 trim。
fn sanitize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// 单行字段：把换行 / 回车压成空格，保证手写 frontmatter 的「value 不跨行」前提。
fn sanitize_inline(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("heb-mem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 测试辅助：用默认 kind=stable + 空 tags 写一条（收口旧签名调用点）。
    fn w(
        dd: &Path,
        wd: Option<&Path>,
        scope: MemoryScope,
        slug: &str,
        category: &str,
        summary: &str,
        body: &str,
    ) -> AppResult<MemoryL0> {
        write(
            dd,
            wd,
            scope,
            slug,
            MemoryKind::Stable,
            category,
            &[],
            summary,
            body,
        )
    }

    #[test]
    fn write_then_list_l0_roundtrips_summary() {
        let dd = tmp_dir();
        w(
            &dd,
            None,
            MemoryScope::Global,
            "lang-pref",
            "preferences",
            "用户要求始终用中文回复",
            "## 详情\n回复一律中文。",
        )
        .unwrap();

        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(l0.len(), 1);
        assert_eq!(l0[0].id, "global/lang-pref");
        assert_eq!(l0[0].summary, "用户要求始终用中文回复");
        assert_eq!(l0[0].category, "preferences");
    }

    #[test]
    fn write_same_slug_upserts() {
        let dd = tmp_dir();
        w(&dd, None, MemoryScope::Global, "k", "c", "v1", "b1").unwrap();
        w(&dd, None, MemoryScope::Global, "k", "c", "v2", "b2").unwrap();
        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(l0.len(), 1, "同 slug 应覆盖而非新增");
        assert_eq!(l0[0].summary, "v2");
    }

    #[test]
    fn read_overview_vs_full() {
        let dd = tmp_dir();
        w(
            &dd,
            None,
            MemoryScope::Global,
            "arch",
            "architecture",
            "分层 DAG",
            "## 概览\n一句话概览。\n\n## 详情\n很长的详情内容。",
        )
        .unwrap();
        let ov = read(&dd, None, "global/arch", MemoryLevel::Overview).unwrap();
        assert!(ov.contains("一句话概览"));
        assert!(!ov.contains("很长的详情"), "overview 不应含详情段");
        let full = read(&dd, None, "global/arch", MemoryLevel::Full).unwrap();
        assert!(full.contains("很长的详情"));
    }

    #[test]
    fn read_overview_falls_back_to_full_when_no_overview() {
        let dd = tmp_dir();
        w(
            &dd,
            None,
            MemoryScope::Global,
            "k",
            "c",
            "s",
            "只有正文没有概览段",
        )
        .unwrap();
        let ov = read(&dd, None, "global/k", MemoryLevel::Overview).unwrap();
        assert!(ov.contains("只有正文"));
    }

    #[test]
    fn list_l0_empty_when_no_dir() {
        let dd = tmp_dir();
        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert!(l0.is_empty());
    }

    #[test]
    fn project_scope_uses_workdir_dir() {
        let dd = tmp_dir();
        let wd = Path::new("/tmp/some/project");
        w(
            &dd,
            Some(wd),
            MemoryScope::Project,
            "structure",
            "structure",
            "Rust workspace，agent-core 是大脑",
            "## 详情\n目录布局……",
        )
        .unwrap();
        // 落到 projects/<enc>/memory/ 下
        let expect = projects::project_dir(&dd, wd).join("memory/structure.md");
        assert!(expect.exists(), "项目记忆应落到 project 目录");
        let l0 = list_l0(&dd, Some(wd), MemoryScope::Project).unwrap();
        assert_eq!(l0[0].id, "proj/structure");
    }

    #[test]
    fn project_scope_without_workdir_errs() {
        let dd = tmp_dir();
        let r = w(&dd, None, MemoryScope::Project, "k", "c", "s", "b");
        assert!(r.is_err());
    }

    #[test]
    fn summary_newlines_flattened() {
        let dd = tmp_dir();
        w(
            &dd,
            None,
            MemoryScope::Global,
            "k",
            "c",
            "a\nb\n  c",
            "body",
        )
        .unwrap();
        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(l0[0].summary, "a b c", "summary 换行应压成空格");
    }

    #[test]
    fn append_log_writes_jsonl() {
        let dd = tmp_dir();
        append_log(
            &dd,
            None,
            MemoryScope::Global,
            &MemoryLogEntry::new("wrote", "测试"),
        )
        .unwrap();
        let p = dd.join("memory/.memory_log.jsonl");
        let s = std::fs::read_to_string(p).unwrap();
        assert!(s.contains("\"outcome\":\"wrote\""));
    }

    #[test]
    fn parse_id_rejects_bad_prefix() {
        assert!(parse_id("bogus/x").is_none());
        assert_eq!(
            parse_id("proj/architecture"),
            Some((MemoryScope::Project, "architecture".to_string()))
        );
    }

    #[test]
    fn cursor_roundtrip_and_default_none() {
        let dd = tmp_dir();
        // 从未抽过 → None
        assert!(read_cursor(&dd, "sess-1").is_none());
        // 写入后读回
        write_cursor(&dd, "sess-1", "msg-42").unwrap();
        assert_eq!(read_cursor(&dd, "sess-1").as_deref(), Some("msg-42"));
        // 推进覆盖
        write_cursor(&dd, "sess-1", "msg-99").unwrap();
        assert_eq!(read_cursor(&dd, "sess-1").as_deref(), Some("msg-99"));
    }

    // ── 新字段（架构 §4.14 演进）回归测试 ──

    #[test]
    fn kind_and_tags_roundtrip() {
        let dd = tmp_dir();
        write(
            &dd,
            None,
            MemoryScope::Global,
            "fix-x",
            MemoryKind::Episode,
            "bug",
            &["pitfall".into(), "provider".into()],
            "修好了 X",
            "## 详情\n根因是 Y。",
        )
        .unwrap();
        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(l0[0].kind, MemoryKind::Episode);
        assert_eq!(
            l0[0].tags,
            vec!["pitfall".to_string(), "provider".to_string()]
        );
    }

    #[test]
    fn old_record_without_new_fields_defaults_to_stable() {
        // 模拟老格式记忆文件（无 kind/tags/importance/last_active）。
        let dd = tmp_dir();
        let root = dd.join("memory");
        std::fs::create_dir_all(&root).unwrap();
        let old = "---\nid: global/legacy\nscope: global\ncategory: arch\nsummary: 老记忆\nupdated_at: 2026-01-01T00:00:00+00:00\n---\n## 详情\n正文。";
        std::fs::write(root.join("legacy.md"), old).unwrap();
        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(l0[0].kind, MemoryKind::Stable, "老记忆缺 kind 默认 stable");
        assert!(l0[0].tags.is_empty(), "老记忆缺 tags 默认空");
    }

    #[test]
    fn upsert_preserves_importance() {
        // importance 是激活强化的成果，upsert（同 slug 重写）不能清零。
        let dd = tmp_dir();
        let slug = "imp-keep";
        w(&dd, None, MemoryScope::Global, slug, "c", "v1", "b1").unwrap();
        // 手动把 importance 抬到 0.9（模拟激活强化）
        let path = dd.join("memory").join(format!("{slug}.md"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let bumped = raw.replace("importance: 0.5", "importance: 0.9");
        std::fs::write(&path, bumped).unwrap();
        // 再 upsert（更新内容）
        w(&dd, None, MemoryScope::Global, slug, "c", "v2", "b2").unwrap();
        let rec = read_existing(&path).unwrap();
        assert_eq!(rec.importance, 0.9, "upsert 应保留已强化的 importance");
        assert_eq!(rec.summary, "v2", "内容应已更新");
    }

    #[test]
    fn importance_curve_reinforces_and_decays_by_kind() {
        // 刚激活（age=0）→ 上限 1.0（强化到顶）。
        assert!((importance_from_age(MemoryKind::Stable, 0.0) - 1.0).abs() < 1e-4);
        assert!((importance_from_age(MemoryKind::Episode, 0.0) - 1.0).abs() < 1e-4);
        // 半衰期处 → 落到 floor + (1-floor)/2（recency=0.5）。
        let stable_half = importance_from_age(MemoryKind::Stable, STABLE_HALF_LIFE_DAYS);
        assert!((stable_half - (STABLE_FLOOR + (1.0 - STABLE_FLOOR) * 0.5)).abs() < 1e-4);
        // 同样时长下 episode 衰减更快（半衰期短）→ importance 更低。
        let stable_30 = importance_from_age(MemoryKind::Stable, 30.0);
        let episode_30 = importance_from_age(MemoryKind::Episode, 30.0);
        assert!(episode_30 < stable_30, "同 30 天 episode 应比 stable 掉得更狠");
        // 久远 → 各自逼近地板，不穿透。
        assert!(importance_from_age(MemoryKind::Stable, 3650.0) >= STABLE_FLOOR - 1e-4);
        assert!(importance_from_age(MemoryKind::Episode, 3650.0) >= EPISODE_FLOOR - 1e-4);
        assert!(importance_from_age(MemoryKind::Episode, 3650.0) <= FORGET_IMPORTANCE);
    }

    #[test]
    fn bump_last_active_reinforces_then_sleep_decays() {
        let dd = tmp_dir();
        // 写一条 episode，手动把 last_active 拨到很久以前（模拟长期不用）。
        write(&dd, None, MemoryScope::Global, "ep", MemoryKind::Episode,
            "bug", &[], "老事件", "## 详情\n根因 Y。").unwrap();
        let path = dd.join("memory").join("ep.md");
        let raw = std::fs::read_to_string(&path).unwrap();
        let stale = raw.lines().map(|l| if l.starts_with("last_active: ") {
            "last_active: 2020-01-01T00:00:00+00:00".to_string()
        } else { l.to_string() }).collect::<Vec<_>>().join("\n");
        std::fs::write(&path, stale).unwrap();

        // 深睡衰减：久未激活的 episode → importance 掉到地板、进遗忘候选。
        let report = consolidate_importance(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(report.recomputed, 1);
        assert!(report.forget_candidates.contains(&"global/ep".to_string()),
            "长期不激活的 episode 应成遗忘候选");
        assert!(read_existing(&path).unwrap().importance <= FORGET_IMPORTANCE);

        // 强化：召回命中 → bump_last_active 把 age 归零 → 再重算 importance 回上限、脱离遗忘候选。
        bump_last_active(&dd, None, "global/ep").unwrap();
        let report2 = consolidate_importance(&dd, None, MemoryScope::Global).unwrap();
        assert!(report2.forget_candidates.is_empty(), "刚强化过的 episode 不该是遗忘候选");
        assert!(read_existing(&path).unwrap().importance > 0.9, "刚激活 importance 应回到高位");

        // load_importance 供召回排序读取。
        let imp = load_importance(&dd, None, MemoryScope::Global).unwrap();
        assert!(*imp.get("global/ep").unwrap() > 0.9);
    }

    #[test]
    fn links_roundtrip_and_empty_when_absent() {
        let dd = tmp_dir();
        // 不存在 → 空
        assert!(load_links(&dd, None, MemoryScope::Global)
            .unwrap()
            .is_empty());
        // 先建 memory 目录（save_links 需要 root 存在）
        w(&dd, None, MemoryScope::Global, "a", "c", "sa", "ba").unwrap();
        let links = vec![
            MemoryLink {
                from: "global/a".into(),
                to: "global/b".into(),
                weight: 0.6,
                updated_at: "2026-06-23T00:00:00+00:00".into(),
            },
            MemoryLink {
                from: "global/a".into(),
                to: "global/c".into(),
                weight: 0.3,
                updated_at: "2026-06-23T00:00:00+00:00".into(),
            },
        ];
        save_links(&dd, None, MemoryScope::Global, &links).unwrap();
        let got = load_links(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].from, "global/a");
        assert_eq!(got[0].weight, 0.6);
    }
}
