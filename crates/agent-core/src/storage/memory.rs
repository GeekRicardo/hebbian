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
fn memory_root(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
) -> AppResult<PathBuf> {
    match scope {
        MemoryScope::Global => Ok(data_dir.join("memory")),
        MemoryScope::Project => {
            let wd = workdir.ok_or_else(|| {
                AppError::msg("project 记忆需要 workdir，但当前对话未绑定项目")
            })?;
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
pub fn write(
    data_dir: &Path,
    workdir: Option<&Path>,
    scope: MemoryScope,
    slug: &str,
    category: &str,
    summary: &str,
    body: &str,
) -> AppResult<MemoryL0> {
    let slug = sanitize_slug(slug);
    if slug.is_empty() {
        return Err(AppError::msg("记忆 slug 不能为空"));
    }
    let category = sanitize_inline(category);
    let summary = sanitize_inline(summary);
    let id = make_id(scope, &slug);

    let root = memory_root(data_dir, workdir, scope)?;
    let path = record_path(&root, &slug);
    let rec = MemoryRecord {
        id: id.clone(),
        scope,
        category: category.clone(),
        summary: summary.clone(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        body: body.trim().to_string(),
    };
    lock::write_atomic(&path, render_md(&rec).as_bytes())?;

    Ok(MemoryL0 {
        id,
        summary,
        category,
    })
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
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
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
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
    let root = memory_root(data_dir, workdir, scope)?;
    let path = record_path(&root, &slug);
    let bytes = lock::read_locked(&path)
        .map_err(|_| AppError::msg(format!("记忆不存在：{id}")))?;
    let text = String::from_utf8_lossy(&bytes);
    let rec = parse_md(&text).ok_or_else(|| AppError::msg(format!("记忆解析失败：{id}")))?;
    match level {
        MemoryLevel::Full => Ok(rec.body),
        MemoryLevel::Overview => Ok(extract_overview(&rec.body).unwrap_or(rec.body)),
    }
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

// ── frontmatter 序列化 / 解析（手写极简） ────────────────────────────────────

struct MemoryRecord {
    id: String,
    scope: MemoryScope,
    category: String,
    summary: String,
    updated_at: String,
    body: String,
}

fn render_md(rec: &MemoryRecord) -> String {
    format!(
        "---\nid: {}\nscope: {}\ncategory: {}\nsummary: {}\nupdated_at: {}\n---\n{}\n",
        rec.id,
        rec.scope.prefix(),
        rec.category,
        rec.summary,
        rec.updated_at,
        rec.body,
    )
}

/// 解析 frontmatter + 正文。不以 `---` 开头视为无 frontmatter（整篇是 body，其余字段空）。
fn parse_md(text: &str) -> Option<MemoryRecord> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let header = &rest[..end];
    let body = &rest[end + 5..]; // 跳过 "\n---\n"

    let mut id = String::new();
    let mut scope = MemoryScope::Global;
    let mut category = String::new();
    let mut summary = String::new();
    let mut updated_at = String::new();
    for line in header.lines() {
        let Some((k, v)) = line.split_once(": ") else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "id" => id = v.to_string(),
            "scope" => scope = MemoryScope::from_prefix(v).unwrap_or(MemoryScope::Global),
            "category" => category = v.to_string(),
            "summary" => summary = v.to_string(),
            "updated_at" => updated_at = v.to_string(),
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    Some(MemoryRecord {
        id,
        scope,
        category,
        summary,
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

    #[test]
    fn write_then_list_l0_roundtrips_summary() {
        let dd = tmp_dir();
        write(
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
        write(&dd, None, MemoryScope::Global, "k", "c", "v1", "b1").unwrap();
        write(&dd, None, MemoryScope::Global, "k", "c", "v2", "b2").unwrap();
        let l0 = list_l0(&dd, None, MemoryScope::Global).unwrap();
        assert_eq!(l0.len(), 1, "同 slug 应覆盖而非新增");
        assert_eq!(l0[0].summary, "v2");
    }

    #[test]
    fn read_overview_vs_full() {
        let dd = tmp_dir();
        write(
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
        write(&dd, None, MemoryScope::Global, "k", "c", "s", "只有正文没有概览段").unwrap();
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
        write(
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
        let r = write(&dd, None, MemoryScope::Project, "k", "c", "s", "b");
        assert!(r.is_err());
    }

    #[test]
    fn summary_newlines_flattened() {
        let dd = tmp_dir();
        write(&dd, None, MemoryScope::Global, "k", "c", "a\nb\n  c", "body").unwrap();
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
}
