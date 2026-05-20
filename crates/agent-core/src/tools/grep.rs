//! Grep 工具：基于 ripgrep 同源 crates 在 workspace 范围内搜索文件内容。
//!
//! - read-only：默认 auto-approve
//! - 默认搜 workdir；可指定 `path`，但必须落在 workspace 内
//! - 默认 `files_with_matches` 模式；`output_mode: "content"` 显示匹配行

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{AppError, AppResult};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks::Lossy, BinaryDetection, MmapChoice, SearcherBuilder};
use ignore::{
    overrides::OverrideBuilder,
    types::{Types, TypesBuilder},
    WalkBuilder,
};
use serde_json::{json, Value};
use tokio::time;

use super::Tool;
use crate::workspace::Workspace;

const MAX_OUTPUT_BYTES: usize = 30_000;
const TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_EXCLUDE_GLOBS: &[&str] = &["!.git", "!.svn", "!.hg", "!node_modules", "!target"];

pub struct GrepTool {
    workspace: Arc<Workspace>,
}

impl GrepTool {
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "用 ripgrep 在文件内容中搜索正则。默认搜对话的 workdir。\
         output_mode 默认 \"files_with_matches\"，可选 \"content\" 显示匹配行（带行号）\
         或 \"count\" 显示每个文件的命中次数。\
         glob 用于按文件名过滤；type 用于按语言过滤（如 rust/py/ts）。\
         path 必须在对话允许的路径范围内。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "正则模式（ripgrep 语法）"
                },
                "path": {
                    "type": "string",
                    "description": "搜索路径（绝对路径）。不传则用对话 workdir。"
                },
                "glob": {
                    "type": "string",
                    "description": "文件名 glob，比如 \"*.rs\" 或 \"*.{ts,tsx}\""
                },
                "type": {
                    "type": "string",
                    "description": "ripgrep 语言类型，比如 rust / py / ts / go / js"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["files_with_matches", "content", "count"],
                    "description": "默认 files_with_matches"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "忽略大小写。默认 false。"
                },
                "head_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "限制输出前 N 行。默认 200。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| AppError::msg("Grep: 缺少 pattern"))?;

        let path = match input["path"].as_str().filter(|s| !s.is_empty()) {
            // 越界检查在 agent_loop 统一做
            Some(p) => PathBuf::from(p),
            None => self.workspace.workdir().to_path_buf(),
        };

        let mode = input["output_mode"]
            .as_str()
            .unwrap_or("files_with_matches");
        let case_insensitive = input["case_insensitive"].as_bool().unwrap_or(false);
        let head_limit = input["head_limit"].as_u64().unwrap_or(200) as usize;

        match mode {
            "files_with_matches" | "count" | "content" => {}
            other => return Err(AppError::msg(format!("Grep: 无效的 output_mode {other}"))),
        }

        let glob = input["glob"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let file_type = input["type"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let search = SearchRequest {
            path,
            mode: mode.to_string(),
            pattern: pattern.to_string(),
            case_insensitive,
            glob,
            file_type,
        };
        let lines = match time::timeout(
            TIMEOUT,
            tokio::task::spawn_blocking(move || run_search(search)),
        )
        .await
        {
            Ok(Ok(result)) => result?,
            Ok(Err(e)) => return Err(AppError::msg(format!("Grep: 搜索任务失败 {e}"))),
            Err(_) => return Err(AppError::msg("Grep: 搜索超时（30s）")),
        };

        if lines.is_empty() {
            return Ok("(无匹配)".into());
        }
        let total = lines.len();
        let kept: Vec<&str> = lines.iter().map(String::as_str).take(head_limit).collect();
        let mut result = kept.join("\n");
        if total > head_limit {
            result.push_str(&format!(
                "\n…（共 {total} 行，已截到前 {head_limit} 行；调高 head_limit 看更多）"
            ));
        }
        Ok(truncate_bytes(&result, MAX_OUTPUT_BYTES))
    }
}

struct SearchRequest {
    path: PathBuf,
    mode: String,
    pattern: String,
    case_insensitive: bool,
    glob: Option<String>,
    file_type: Option<String>,
}

fn run_search(request: SearchRequest) -> AppResult<Vec<String>> {
    let mut lines = Vec::new();
    let display_root = if request.path.is_file() {
        request
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| request.path.clone())
    } else {
        request.path.clone()
    };
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(request.case_insensitive)
        .multi_line(false)
        .line_terminator(Some(b'\n'))
        .build(&request.pattern)
        .map_err(|e| AppError::msg(format!("Grep: 无效的正则 {e}")))?;
    let mut walker = WalkBuilder::new(&request.path);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .overrides(build_overrides(&display_root, request.glob.as_deref())?);
    if let Some(types) = build_types(request.file_type.as_deref())? {
        walker.types(types);
    }
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .memory_map(mmap_choice())
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();

    for entry in walker.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!("Grep: skip unreadable entry: {err}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ty| ty.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = display_path(path, &display_root);
        match request.mode.as_str() {
            "files_with_matches" => {
                let mut matched = false;
                searcher
                    .search_path(
                        &matcher,
                        path,
                        Lossy(|_, _| {
                            matched = true;
                            Ok(false)
                        }),
                    )
                    .map_err(grep_io_error)?;
                if matched {
                    lines.push(rel);
                }
            }
            "count" => {
                let mut count = 0usize;
                searcher
                    .search_path(
                        &matcher,
                        path,
                        Lossy(|_, _| {
                            count += 1;
                            Ok(true)
                        }),
                    )
                    .map_err(grep_io_error)?;
                if count > 0 {
                    lines.push(format!("{rel}:{count}"));
                }
            }
            "content" => {
                searcher
                    .search_path(
                        &matcher,
                        path,
                        Lossy(|line_number, line| {
                            lines.push(format!(
                                "{rel}:{line_number}:{}",
                                line.trim_end_matches('\n')
                            ));
                            Ok(true)
                        }),
                    )
                    .map_err(grep_io_error)?;
            }
            _ => unreachable!("mode was validated before search"),
        }
    }

    Ok(lines)
}

fn grep_io_error(err: io::Error) -> AppError {
    AppError::msg(format!("Grep: 搜索失败 {err}"))
}

fn mmap_choice() -> MmapChoice {
    #[cfg(target_os = "macos")]
    {
        MmapChoice::never()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: Matches ripgrep's performance-oriented search strategy. The
        // searcher only maps regular files selected by the ignore walker, and
        // falls back to normal reads when mmap is not appropriate.
        unsafe { MmapChoice::auto() }
    }
}

fn build_overrides(root: &Path, glob: Option<&str>) -> AppResult<ignore::overrides::Override> {
    let mut builder = OverrideBuilder::new(root);
    for glob in DEFAULT_EXCLUDE_GLOBS {
        builder
            .add(glob)
            .map_err(|e| AppError::msg(format!("Grep: 无效的默认排除 glob {glob}: {e}")))?;
    }
    if let Some(glob) = glob {
        builder
            .add(glob)
            .map_err(|e| AppError::msg(format!("Grep: 无效的 glob {glob}: {e}")))?;
    }
    builder
        .build()
        .map_err(|e| AppError::msg(format!("Grep: 无效的 glob 配置 {e}")))
}

fn build_types(file_type: Option<&str>) -> AppResult<Option<Types>> {
    let Some(file_type) = file_type else {
        return Ok(None);
    };
    let mut builder = TypesBuilder::new();
    builder.add_defaults();
    for (name, glob) in extra_type_defs() {
        builder
            .add(name, glob)
            .map_err(|e| AppError::msg(format!("Grep: 内置 type 定义失败 {name}:{glob}: {e}")))?;
    }
    builder.select(file_type);
    builder
        .build()
        .map(Some)
        .map_err(|e| AppError::msg(format!("Grep: 无效的 type {file_type}: {e}")))
}

fn extra_type_defs() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("rs", "*.rs"),
        ("py", "*.py"),
        ("ts", "*.ts"),
        ("tsx", "*.tsx"),
        ("js", "*.js"),
        ("javascript", "*.{js,jsx,mjs,cjs}"),
        ("md", "*.{md,markdown}"),
        ("yml", "*.{yaml,yml}"),
    ])
}

fn display_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn truncate_bytes(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[已截断，共 {} 字节]", &s[..end], s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn workspace_at(path: &std::path::Path) -> Arc<Workspace> {
        Workspace::new(path, Vec::new())
    }

    #[tokio::test]
    async fn finds_matching_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "nothing here").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"pattern": "hello"})).await.unwrap();
        assert!(out.contains("a.txt"));
        assert!(!out.contains("b.txt"));
    }

    #[tokio::test]
    async fn content_mode_includes_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(tmp.path().join("a.txt")).unwrap();
        writeln!(f, "first").unwrap();
        writeln!(f, "needle here").unwrap();
        writeln!(f, "third").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool
            .execute(json!({
                "pattern": "needle",
                "output_mode": "content",
            }))
            .await
            .unwrap();
        assert!(out.contains("2:needle here"));
    }

    #[tokio::test]
    async fn no_matches_returns_friendly_message() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "abc").unwrap();
        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"pattern": "xyzunique"})).await.unwrap();
        assert!(out.contains("无匹配"));
    }

    #[tokio::test]
    async fn filters_by_type_and_brace_glob() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.ts"), "needle").unwrap();
        std::fs::write(tmp.path().join("b.tsx"), "needle").unwrap();
        std::fs::write(tmp.path().join("c.js"), "needle").unwrap();
        std::fs::write(tmp.path().join("d.txt"), "needle").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool
            .execute(json!({
                "pattern": "needle",
                "glob": "*.{ts,tsx}",
                "type": "ts",
            }))
            .await
            .unwrap();

        assert!(out.contains("a.ts"));
        assert!(out.contains("b.tsx"));
        assert!(!out.contains("c.js"));
        assert!(!out.contains("d.txt"));
    }

    #[tokio::test]
    async fn count_mode_counts_matches_per_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "needle\nneedle\nother").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "needle").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool
            .execute(json!({
                "pattern": "needle",
                "output_mode": "count",
            }))
            .await
            .unwrap();

        assert!(out.contains("a.txt:2"));
        assert!(out.contains("b.txt:1"));
    }

    #[tokio::test]
    async fn respects_gitignore_like_ripgrep() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(tmp.path().join("kept.txt"), "needle").unwrap();
        std::fs::write(tmp.path().join("ignored.log"), "needle").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"pattern": "needle"})).await.unwrap();

        assert!(out.contains("kept.txt"));
        assert!(!out.contains("ignored.log"));
    }

    #[tokio::test]
    async fn does_not_require_rg_on_path() {
        if std::env::var_os("HEBBIAN_GREP_EMPTY_PATH_CHILD").is_none() {
            let bin = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("tools::grep::tests::does_not_require_rg_on_path")
                .arg("--nocapture")
                .env("PATH", bin.path())
                .env("HEBBIAN_GREP_EMPTY_PATH_CHILD", "1")
                .output()
                .unwrap();

            assert!(
                output.status.success(),
                "child test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello from rust grep").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"pattern": "rust grep"})).await.unwrap();
        assert!(out.contains("a.txt"));
    }
}
