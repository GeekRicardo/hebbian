//! Skill 资产导入（架构 §6.1.3）。
//!
//! 提供从 `~/.claude/skills/` 一次性拷贝到 hebbian skills 目录的工具：
//! - Global  → `~/.hebbian/skills/<name>/`
//! - Project → `~/.hebbian/projects/<encode(workdir)>/skills/<name>/`
//!
//! 默认运行时不会读 `~/.claude/skills/`，需要的用户通过 surface 主动触发本导入。

use std::path::{Path, PathBuf};

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};

use super::projects;

/// 导入范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportScope {
    Global,
    Project,
}

/// 单条导入结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedSkill {
    pub name: String,
    pub dest: PathBuf,
    pub overwritten: bool,
}

/// `~/.hebbian/disabled_skills.json` 的形态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisabledSkillsFile {
    #[serde(default)]
    pub disabled: Vec<String>,
}

const DISABLED_FILE: &str = "disabled_skills.json";

pub fn disabled_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DISABLED_FILE)
}

pub fn load_disabled(data_dir: &Path) -> DisabledSkillsFile {
    let p = disabled_path(data_dir);
    if !p.exists() {
        return DisabledSkillsFile::default();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_disabled(data_dir: &Path, file: &DisabledSkillsFile) -> AppResult<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string_pretty(file)?;
    std::fs::write(disabled_path(data_dir), json)?;
    Ok(())
}

/// 把单个 skill 的启用状态写到 disabled_skills.json。
/// - `enabled=true` 从 disabled 列表里移除；不在列表则 no-op
/// - `enabled=false` 加入列表；已在列表则 no-op
pub fn set_skill_enabled(data_dir: &Path, name: &str, enabled: bool) -> AppResult<()> {
    let mut file = load_disabled(data_dir);
    if enabled {
        file.disabled.retain(|n| n != name);
    } else if !file.disabled.iter().any(|n| n == name) {
        file.disabled.push(name.to_string());
    }
    save_disabled(data_dir, &file)
}

/// 把 disabled_skills.json 的状态打到 skills 列表的 `enabled` 字段上。
/// 调用方有完整 skill 列表时用这个统一处理。
pub fn apply_disabled(data_dir: &Path, skills: &mut [crate::tools::skill::Skill]) {
    let set: std::collections::HashSet<String> = load_disabled(data_dir)
        .disabled
        .into_iter()
        .collect();
    for s in skills.iter_mut() {
        s.enabled = !set.contains(&s.name);
    }
}

/// 列出 `~/.claude/skills/` 下的 skill 名（仅子目录里含 SKILL.md 的算）。
pub fn list_claude_skills() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let root = home.join(".claude").join("skills");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").exists() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

/// 从 `~/.claude/skills/` 拷贝 skill 到 hebbian。
///
/// - `names = None` → 导入全部
/// - `overwrite = true` → 同名 skill 直接覆盖；否则跳过并在 `overwritten=false` 体现
/// - Project scope 必须传 `workdir`
pub fn import_from_claude(
    data_dir: &Path,
    scope: ImportScope,
    workdir: Option<&Path>,
    names: Option<&[String]>,
    overwrite: bool,
) -> AppResult<Vec<ImportedSkill>> {
    let home = dirs::home_dir().ok_or_else(|| AppError::msg("无法定位用户主目录"))?;
    let src_root = home.join(".claude").join("skills");
    if !src_root.exists() {
        return Ok(Vec::new());
    }
    let all = list_claude_skills();
    let selected: Vec<String> = match names {
        Some(filter) => all.into_iter().filter(|n| filter.contains(n)).collect(),
        None => all,
    };
    import_named_from_root(data_dir, scope, workdir, &src_root, &selected, overwrite)
}

/// 扫描结果：一个候选 skill。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedSkill {
    /// SKILL.md 所在目录名（claude code 用此作 skill id）。
    pub name: String,
    /// 相对 `src_dir` 的目录路径（`"a/b"` 形式），用于前端按第一段分组。
    /// `""` 表示 `src_dir` 自身就是一个 skill（顶层）。
    pub relative_path: String,
    /// 从 frontmatter `description` 字段或正文首段提取的简介。
    pub description: String,
    /// SKILL.md 所在目录的**绝对路径**——作为 selected_paths 匹配的唯一 key,
    /// 也是 import 拷贝时的源目录。
    pub dir_path: PathBuf,
}

const MAX_SCAN_DEPTH: usize = 8;

/// 递归扫描 `src_dir` 找所有含 SKILL.md 的目录。找到一个 SKILL.md 后不再深入
/// 该目录（避免一个 skill 内部嵌套被误抓）。跳过 `.xxx` / node_modules / target。
pub fn scan_skill_dir(src_dir: &Path) -> AppResult<Vec<ScannedSkill>> {
    if !src_dir.exists() {
        return Err(AppError::msg(format!("源目录不存在：{}", src_dir.display())));
    }
    let mut out = Vec::new();
    walk_scan(src_dir, src_dir, &mut out, 0);
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(out)
}

fn walk_scan(root: &Path, dir: &Path, out: &mut Vec<ScannedSkill>, depth: usize) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let skill_md = dir.join("SKILL.md");
    if skill_md.exists() {
        let Some(name) = dir.file_name().and_then(|s| s.to_str()).map(String::from) else {
            return;
        };
        let rel = if depth == 0 {
            String::new()
        } else {
            dir.strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| name.clone())
        };
        let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
        let description = extract_description(&content);
        out.push(ScannedSkill {
            name,
            relative_path: rel,
            description,
            dir_path: dir.to_path_buf(),
        });
        return; // 不深入
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let n = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if n.starts_with('.') || n == "node_modules" || n == "target" {
            continue;
        }
        walk_scan(root, &path, out, depth + 1);
    }
}

/// 从 SKILL.md 内容里取 description：先看 frontmatter，再取首段。
fn extract_description(content: &str) -> String {
    if let Some(d) = parse_yaml_field(content, "description") {
        return d;
    }
    // 跳过 frontmatter 后取首段
    let body = if content.starts_with("---") {
        content
            .split("\n---\n")
            .nth(1)
            .or_else(|| content.split("---\n").nth(2))
            .unwrap_or(content)
    } else {
        content
    };
    body.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("")
        .to_string()
}

/// 极简 YAML frontmatter 字段提取。
fn parse_yaml_field(content: &str, field: &str) -> Option<String> {
    if !content.starts_with("---") {
        return None;
    }
    let mut lines = content.lines();
    lines.next(); // 跳过开头 `---`
    let prefix = format!("{field}:");
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let v = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            return Some(v.to_string());
        }
    }
    None
}

/// 浅 clone 一个 git 仓库到临时目录后扫 skills，**结束清理**。
/// 用户从前端选哪些导入后，由调用方再调一次 `import_from_github` 真正拷贝。
pub fn scan_skill_github(
    repo_url: &str,
    subpath: Option<&str>,
) -> AppResult<Vec<ScannedSkill>> {
    let tmp = std::env::temp_dir().join(format!("hebbian-scan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    let clone_res = std::process::Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg("--quiet")
        .arg(repo_url)
        .arg(&tmp)
        .output();
    let output = match clone_res {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(AppError::msg(format!(
                "未找到 git（或调用失败）：{e}；请先安装 git CLI"
            )));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(AppError::msg(format!(
            "git clone 失败：{}",
            stderr.lines().next().unwrap_or("未知错误")
        )));
    }
    let root = match subpath {
        Some(p) => tmp.join(p.trim_start_matches('/')),
        None => tmp.clone(),
    };
    let result = scan_skill_dir(&root);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// 列出 `src_dir` 下能作为 skill 的子目录名（向后兼容入口）。
pub fn list_skills_in_dir(src_dir: &Path) -> Vec<String> {
    scan_skill_dir(src_dir)
        .map(|v| v.into_iter().map(|s| s.name).collect())
        .unwrap_or_default()
}

/// 从本地目录拷贝 skill 到 hebbian。
///
/// - `src_dir`：扫描根（可多层嵌套）
/// - `selected_relative_paths`：若给定，仅拷贝这些相对路径对应的 skill；为 None 则全部
/// - 落盘到 hebbian 时**目录名 = ScannedSkill.name**（最后一段），不保留嵌套层级
pub fn import_from_dir(
    data_dir: &Path,
    scope: ImportScope,
    workdir: Option<&Path>,
    src_dir: &Path,
    selected_relative_paths: Option<&[String]>,
    overwrite: bool,
) -> AppResult<Vec<ImportedSkill>> {
    let scanned = scan_skill_dir(src_dir)?;
    if scanned.is_empty() {
        return Err(AppError::msg(format!(
            "源目录里没有可导入的 skill（每个 skill 子目录需包含 SKILL.md）：{}",
            src_dir.display()
        )));
    }
    let chosen: Vec<&ScannedSkill> = match selected_relative_paths {
        Some(filter) => scanned
            .iter()
            .filter(|s| filter.iter().any(|p| p == &s.relative_path))
            .collect(),
        None => scanned.iter().collect(),
    };
    if chosen.is_empty() {
        return Ok(Vec::new());
    }
    let dst_root = match scope {
        ImportScope::Global => data_dir.join("skills"),
        ImportScope::Project => {
            let wd = workdir.ok_or_else(|| AppError::msg("Project scope 导入需要 workdir"))?;
            projects::project_dir(data_dir, wd).join("skills")
        }
    };
    std::fs::create_dir_all(&dst_root)?;

    let mut out = Vec::new();
    for s in chosen {
        let src = &s.dir_path;
        let dst = dst_root.join(&s.name);
        let existed = dst.exists();
        if existed && !overwrite {
            out.push(ImportedSkill {
                name: s.name.clone(),
                dest: dst,
                overwritten: false,
            });
            continue;
        }
        if existed {
            std::fs::remove_dir_all(&dst)?;
        }
        copy_dir_all(src, &dst)?;
        out.push(ImportedSkill {
            name: s.name.clone(),
            dest: dst,
            overwritten: existed,
        });
    }
    Ok(out)
}

/// 从 git 仓库下载 skill 到 hebbian。clone 到临时目录后 `import_from_dir`，结束 cleanup。
///
/// `selected_relative_paths` 与 `import_from_dir` 同义。
pub fn import_from_github(
    data_dir: &Path,
    scope: ImportScope,
    workdir: Option<&Path>,
    repo_url: &str,
    subpath: Option<&str>,
    selected_relative_paths: Option<&[String]>,
    overwrite: bool,
) -> AppResult<Vec<ImportedSkill>> {
    let tmp = std::env::temp_dir().join(format!("hebbian-skills-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    let clone_result = std::process::Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg("--quiet")
        .arg(repo_url)
        .arg(&tmp)
        .output();
    let cleanup = |path: &Path| {
        let _ = std::fs::remove_dir_all(path);
    };
    let output = match clone_result {
        Ok(o) => o,
        Err(e) => {
            cleanup(&tmp);
            return Err(AppError::msg(format!(
                "未找到 git（或调用失败）：{e}；请先安装 git CLI"
            )));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        cleanup(&tmp);
        return Err(AppError::msg(format!(
            "git clone 失败：{}",
            stderr.lines().next().unwrap_or("未知错误")
        )));
    }
    let root = match subpath {
        Some(p) => tmp.join(p.trim_start_matches('/')),
        None => tmp.clone(),
    };
    let result = import_from_dir(data_dir, scope, workdir, &root, selected_relative_paths, overwrite);
    cleanup(&tmp);
    result
}

/// 内部 helper：从给定 `src_root` 拷贝指定一组 skill 名字到目标 scope。
fn import_named_from_root(
    data_dir: &Path,
    scope: ImportScope,
    workdir: Option<&Path>,
    src_root: &Path,
    names: &[String],
    overwrite: bool,
) -> AppResult<Vec<ImportedSkill>> {
    let dst_root = match scope {
        ImportScope::Global => data_dir.join("skills"),
        ImportScope::Project => {
            let wd = workdir.ok_or_else(|| AppError::msg("Project scope 导入需要 workdir"))?;
            projects::project_dir(data_dir, wd).join("skills")
        }
    };
    std::fs::create_dir_all(&dst_root)?;

    let mut out = Vec::new();
    for name in names {
        let src = src_root.join(name);
        if !src.join("SKILL.md").exists() {
            continue;
        }
        let dst = dst_root.join(name);
        let existed = dst.exists();
        if existed && !overwrite {
            out.push(ImportedSkill {
                name: name.clone(),
                dest: dst,
                overwritten: false,
            });
            continue;
        }
        if existed {
            std::fs::remove_dir_all(&dst)?;
        }
        copy_dir_all(&src, &dst)?;
        out.push(ImportedSkill {
            name: name.clone(),
            dest: dst,
            overwritten: existed,
        });
    }
    Ok(out)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_claude_skills_returns_empty_when_dir_missing() {
        let _ = list_claude_skills();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-skills-test-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_skill(dir: &Path, name: &str, body: &str) {
        let p = dir.join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn import_from_dir_handles_single_skill_dir() {
        let data_dir = tmp("single-data");
        let src_root = tmp("single-src");
        write_skill(&src_root, "my-skill", "# my-skill\nhello");
        let src = src_root.join("my-skill");

        let imported =
            import_from_dir(&data_dir, ImportScope::Global, None, &src, None, true).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "my-skill");
        assert!(data_dir.join("skills").join("my-skill").join("SKILL.md").exists());
    }

    #[test]
    fn import_from_dir_handles_collection_root() {
        let data_dir = tmp("collection-data");
        let src_root = tmp("collection-src");
        write_skill(&src_root, "a", "# a");
        write_skill(&src_root, "b", "# b");
        // 不是 skill 的子目录应被忽略
        std::fs::create_dir_all(src_root.join("c-not-skill")).unwrap();

        let imported =
            import_from_dir(&data_dir, ImportScope::Global, None, &src_root, None, true).unwrap();
        assert_eq!(imported.len(), 2);
        let names: Vec<&str> = imported.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn import_from_dir_project_scope_requires_workdir() {
        let data_dir = tmp("proj-no-wd");
        let src_root = tmp("proj-src");
        write_skill(&src_root, "x", "# x");
        let err = import_from_dir(
            &data_dir,
            ImportScope::Project,
            None,
            &src_root,
            None,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("workdir"));
    }

    #[test]
    fn import_from_dir_project_writes_under_project() {
        let data_dir = tmp("proj-data");
        let src_root = tmp("proj-src2");
        write_skill(&src_root, "skl", "# skl");
        let wd = PathBuf::from("/Users/x/proj");
        let imported = import_from_dir(
            &data_dir,
            ImportScope::Project,
            Some(&wd),
            &src_root,
            None,
            true,
        )
        .unwrap();
        assert_eq!(imported.len(), 1);
        let enc = projects::encode_workdir(&wd);
        assert!(data_dir
            .join("projects")
            .join(enc)
            .join("skills")
            .join("skl")
            .join("SKILL.md")
            .exists());
    }

    #[test]
    fn list_skills_in_dir_detects_self_as_skill() {
        let dir = tmp("list-self");
        write_skill(&dir, "demo", "# demo");
        // 传 demo 自身
        let result = list_skills_in_dir(&dir.join("demo"));
        assert_eq!(result, vec!["demo".to_string()]);
    }
}
