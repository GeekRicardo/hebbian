//! 规则文件自动发现与注入。
//!
//! 从 workspace 的 workdir + allowed_paths 递归向上扫描 CLAUDE.md / AGENTS.md
//! 等规则文件，注入到首条 user message 的 `<system-reminder>` 块中（不破 system
//! prompt cache）。
//!
//! 扫描策略照搬 Claude Code：
//! - 对每个路径向上走到文件系统根目录
//! - 每层检查：CLAUDE.md、AGENTS.md、.claude/CLAUDE.md、CLAUDE.local.md
//! - 按 canonical path 去重，根→叶排序（后面优先级更高）

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 扫描到的单个规则文件。
#[derive(Debug, Clone)]
pub struct RuleFile {
    pub path: PathBuf,
    pub content: String,
    pub source: RuleSource,
}

/// 规则文件来源，决定前端默认开关状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    /// ~/.claude/CLAUDE.md
    Global,
    /// 在 workdir 的祖先链上
    Workdir,
    /// 在某个 allowed_path 的祖先链上
    #[serde(rename = "allowed_path")]
    AllowedPath,
}

/// 前端传递的规则文件开关状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFileState {
    pub path: PathBuf,
    pub enabled: bool,
}

/// 发现请求：给 Tauri command 返回给前端的轻量信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFileInfo {
    pub path: String,
    pub source: RuleSource,
}

/// 每层目录检查的文件名列表。
const RULE_FILE_NAMES: &[&str] = &["CLAUDE.md", "AGENTS.md", ".claude/CLAUDE.md", "CLAUDE.local.md"];

/// 全局规则文件的默认路径列表。
pub fn default_global_rules() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = dirs::home_dir() {
        let p = h.join(".claude").join("CLAUDE.md");
        if p.exists() {
            v.push(p);
        }
    }
    v
}

/// 读取指定路径的全局规则文件。
fn read_global_rule(path: &Path) -> Option<RuleFile> {
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    Some(RuleFile {
        path: path.to_path_buf(),
        content,
        source: RuleSource::Global,
    })
}

/// 从路径列表中递归向上发现所有规则文件。
///
/// `workdir` 用于标记来源：workdir 祖先链上的文件标记为 [`RuleSource::Workdir`]，
/// 其余路径标记为 [`RuleSource::AllowedPath`]。
pub fn discover(workdir: &Path, allowed_paths: &[PathBuf]) -> Vec<RuleFile> {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut files: Vec<RuleFile> = Vec::new();

    // 先扫 workdir 祖先链
    collect_from_ancestors(workdir, RuleSource::Workdir, &mut seen, &mut files);

    // 再扫每个 allowed_path 祖先链
    for ap in allowed_paths {
        collect_from_ancestors(ap, RuleSource::AllowedPath, &mut seen, &mut files);
    }

    // 反转：从根到叶（后面优先级更高，与 Claude Code 一致）
    files.reverse();
    files
}

fn collect_from_ancestors(
    start: &Path,
    source: RuleSource,
    seen: &mut std::collections::HashSet<PathBuf>,
    out: &mut Vec<RuleFile>,
) {
    let mut current = canonicalize_lossy(start);

    loop {
        for name in RULE_FILE_NAMES {
            let candidate = current.join(name);
            let canon = canonicalize_lossy(&candidate);
            if seen.contains(&canon) {
                continue;
            }
            if !candidate.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(&candidate) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if content.trim().is_empty() {
                continue;
            }
            seen.insert(canon.clone());
            out.push(RuleFile {
                path: canon,
                content,
                source,
            });
        }

        // 走到根目录就停
        let parent = current.parent();
        if parent.is_none() || parent == Some(Path::new("")) {
            break;
        }
        let parent = parent.unwrap();
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
}

/// 把规则文件列表格式化为 `<system-reminder>` 块。
///
/// 格式与 Claude Code 一致：
///
/// ```text
/// <system-reminder>
/// Codebase and user instructions are shown below...
///
/// Contents of /path (project instructions):
/// <content>
/// ...
/// </system-reminder>
/// ```
pub fn format_injection(files: &[RuleFile]) -> String {
    if files.is_empty() {
        return String::new();
    }

    let mut s = String::from("<system-reminder>\n");
    s.push_str(
        "Codebase and user instructions are shown below. Be sure to adhere to these \
         instructions. IMPORTANT: These instructions OVERRIDE any default behavior and \
         you MUST follow them exactly as written.\n",
    );

    for f in files {
        s.push('\n');
        let label = match f.source {
            RuleSource::Global => "user's private global instructions",
            RuleSource::Workdir => "project instructions, checked into the codebase",
            RuleSource::AllowedPath => "project instructions from allowed path",
        };
        s.push_str(&format!(
            "Contents of {} ({}):\n",
            f.path.display(),
            label
        ));
        s.push_str(&f.content);
        s.push('\n');
    }

    s.push_str("</system-reminder>\n");
    s
}

/// 根据用户配置的 [`RuleFileState`] 列表读取并返回应注入的规则文件。
///
/// - `global_rules`: 已启用的全局规则文件路径列表（如 `~/.claude/CLAUDE.md`）
/// - `rules_files`: `Some` → 只注入 enabled 的文件；`None` → 自动发现，
///   workdir 祖先链上的默认 enabled
pub fn resolve_injection_files(
    global_rules: &[PathBuf],
    rules_files: Option<&[RuleFileState]>,
    workdir: &Path,
    allowed_paths: &[PathBuf],
) -> Vec<RuleFile> {
    let mut files: Vec<RuleFile> = Vec::new();

    // 全局规则文件
    for path in global_rules {
        if let Some(gf) = read_global_rule(path) {
            files.push(gf);
        }
    }

    match rules_files {
        Some(states) => {
            // 按用户保存的开关状态读取
            for state in states {
                if !state.enabled {
                    continue;
                }
                if !state.path.exists() {
                    continue;
                }
                let content = match std::fs::read_to_string(&state.path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if content.trim().is_empty() {
                    continue;
                }
                let source = classify_source(&state.path, workdir);
                files.push(RuleFile {
                    path: state.path.clone(),
                    content,
                    source,
                });
            }
        }
        None => {
            // 自动发现：workdir 祖先链上的默认 on，其他默认 off
            let discovered = discover(workdir, allowed_paths);
            for f in discovered {
                if f.source == RuleSource::Workdir {
                    files.push(f);
                }
            }
        }
    }

    files
}

fn classify_source(path: &Path, workdir: &Path) -> RuleSource {
    let canon_path = canonicalize_lossy(path);
    let canon_workdir = canonicalize_lossy(workdir);
    if canon_path.starts_with(&canon_workdir) {
        RuleSource::Workdir
    } else {
        RuleSource::AllowedPath
    }
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let mut suffix: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(p) = std::fs::canonicalize(cur) {
            let mut out = p;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                suffix.push(name);
                cur = parent;
            }
            _ => break,
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_finds_claude_md_in_ancestry() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("CLAUDE.md");
        std::fs::write(&md, "# test rules").unwrap();

        let files = discover(tmp.path(), &[]);
        assert!(files.iter().any(|f| f.path == canonicalize_lossy(&md)));
    }

    #[test]
    fn discover_skips_empty_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "").unwrap();

        let files = discover(tmp.path(), &[]);
        assert!(files.iter().all(|f| f.path != canonicalize_lossy(&tmp.path().join("CLAUDE.md"))));
    }

    #[test]
    fn discover_dedups_canonical_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("CLAUDE.md");
        std::fs::write(&md, "# dup test").unwrap();

        // 同一个目录传两次 — 不应重复
        let files = discover(tmp.path(), &[tmp.path().to_path_buf()]);
        let count = files
            .iter()
            .filter(|f| f.path == canonicalize_lossy(&md))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn format_injection_produces_system_reminder_block() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("CLAUDE.md");
        std::fs::write(&md, "# project rules").unwrap();

        let files = vec![RuleFile {
            path: canonicalize_lossy(&md),
            content: "# project rules".into(),
            source: RuleSource::Workdir,
        }];
        let output = format_injection(&files);
        assert!(output.starts_with("<system-reminder>"));
        assert!(output.contains("# project rules"));
        assert!(output.contains("project instructions"));
        assert!(output.ends_with("</system-reminder>\n"));
    }

    #[test]
    fn format_injection_empty_returns_empty_string() {
        assert_eq!(format_injection(&[]), "");
    }

    #[test]
    fn resolve_injection_auto_discover_only_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# workdir rules").unwrap();
        let extra = tempfile::tempdir().unwrap();
        std::fs::write(extra.path().join("AGENTS.md"), "# extra rules").unwrap();

        let files = resolve_injection_files(&[], None, tmp.path(), &[extra.path().to_path_buf()]);
        // auto discover 只注入 workdir 祖先链上的
        assert!(files.iter().any(|f| f.content.contains("workdir rules")));
        assert!(!files.iter().any(|f| f.content.contains("extra rules")));
    }

    #[test]
    fn resolve_injection_with_explicit_states() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("CLAUDE.md");
        std::fs::write(&md, "# explicit").unwrap();

        let states = vec![RuleFileState {
            path: md.clone(),
            enabled: true,
        }];
        let files =
            resolve_injection_files(&[], Some(&states), tmp.path(), &[]);
        assert!(files.iter().any(|f| f.content.contains("explicit")));
    }

    #[test]
    fn resolve_injection_respects_disabled_state() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("CLAUDE.md");
        std::fs::write(&md, "# disabled").unwrap();

        let states = vec![RuleFileState {
            path: md.clone(),
            enabled: false,
        }];
        let files =
            resolve_injection_files(&[], Some(&states), tmp.path(), &[]);
        assert!(!files.iter().any(|f| f.content.contains("disabled")));
    }
}
