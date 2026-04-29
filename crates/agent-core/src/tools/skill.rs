//! Skill 系统：从 `~/.claude/skills/` 和 `<cwd>/.claude/skills/` 加载 SKILL.md 并按需注入。
//!
//! - SKILL.md 头部 frontmatter（YAML）解析 name + description
//! - SkillTool 是「读取 SKILL.md 套壳」：模型给出 skill 名，我们把整篇内容塞回去
//! - 工具 description 动态包含可用 skill 列表（让模型知道有哪些 skill 可调用）
//! - read-only：默认 auto-approve

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use platform::{AppError, AppResult};
use serde_json::{json, Value};

use super::Tool;

const MAX_SKILL_BYTES: u64 = 200 * 1024;
const MAX_DESC_PREVIEW: usize = 200;

/// 一个加载好的 skill
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub source: SkillSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// `~/.claude/skills/<name>/SKILL.md`
    User,
    /// `<workdir>/.claude/skills/<name>/SKILL.md`
    Project,
}

/// 默认 skill 目录：`~/.claude/skills` + `<workdir>/.claude/skills`
pub fn default_skill_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".claude/skills"));
    }
    out.push(workdir.join(".claude/skills"));
    out
}

/// 从给定的目录列表加载 skills。后面的目录会覆盖前面的同名 skill
/// （约定：列表前段 = user/global，后段 = project，project 优先）。
pub fn load_skills(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut out: BTreeMap<String, Skill> = BTreeMap::new();
    for (idx, dir) in dirs.iter().enumerate() {
        let source = if idx == 0 {
            SkillSource::User
        } else {
            SkillSource::Project
        };
        load_dir_into(dir, source, &mut out);
    }
    out.into_values().collect()
}

fn load_dir_into(dir: &Path, source: SkillSource, out: &mut BTreeMap<String, Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&skill_md) else {
            continue;
        };
        let (name_opt, desc_opt) = parse_frontmatter(&content);
        let name = name_opt
            .or_else(|| path.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "unnamed".into());
        let description = desc_opt.unwrap_or_else(|| extract_first_paragraph(&content));
        out.insert(
            name.clone(),
            Skill {
                name,
                description,
                path: skill_md,
                source,
            },
        );
    }
}

/// 极简 YAML frontmatter 解析：只取 `name:` 和 `description:`
fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let mut lines = content.lines();
    if lines.next().map(|s| s.trim()) != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("name:") {
            name = Some(strip_quotes(rest.trim()).to_string());
        } else if let Some(rest) = trimmed.strip_prefix("description:") {
            description = Some(strip_quotes(rest.trim()).to_string());
        }
    }
    (name, description)
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c: char| c == '"' || c == '\'')
}

fn extract_first_paragraph(content: &str) -> String {
    let after_frontmatter = if content.starts_with("---") {
        content
            .splitn(3, "---")
            .nth(2)
            .unwrap_or(content)
            .trim_start()
    } else {
        content
    };
    let para: String = after_frontmatter
        .lines()
        .skip_while(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
        .take_while(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if para.len() > MAX_DESC_PREVIEW {
        format!("{}…", &para[..MAX_DESC_PREVIEW])
    } else {
        para
    }
}

pub struct SkillTool {
    skills: Arc<Vec<Skill>>,
    /// 拼好的描述（含可用 skill 列表）
    description: String,
}

impl SkillTool {
    pub fn new(skills: Vec<Skill>) -> Self {
        let description = render_description(&skills);
        Self {
            skills: Arc::new(skills),
            description,
        }
    }
}

fn render_description(skills: &[Skill]) -> String {
    let mut s = String::from(
        "在主对话里执行一个 skill。Skill 是放在 `.claude/skills/<name>/SKILL.md` 里的\
         markdown 指令包；调用本工具会把整篇 SKILL.md 内容回填到对话里，\
         让模型按其中的指令行动。\n\n",
    );
    if skills.is_empty() {
        s.push_str(
            "当前**没有**可用的 skills。用户可以在 `~/.claude/skills/<name>/SKILL.md` \
             或项目 `.claude/skills/<name>/SKILL.md` 里创建。",
        );
    } else {
        s.push_str("可用 skills（按名字调用）：\n");
        for skill in skills {
            let scope = match skill.source {
                SkillSource::User => "user",
                SkillSource::Project => "project",
            };
            let preview = first_words(&skill.description, 80);
            s.push_str(&format!("- `{}` ({scope}): {preview}\n", skill.name));
        }
    }
    s
}

fn first_words(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["skill"],
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "skill 名称（不带前缀斜杠）"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let name = input["skill"]
            .as_str()
            .ok_or_else(|| AppError::msg("Skill: 缺少 skill"))?
            .trim()
            .trim_start_matches('/');
        let skill = self
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| AppError::msg(format!("Skill: 未找到 skill `{name}`")))?;

        let meta = std::fs::metadata(&skill.path)
            .map_err(|e| AppError::msg(format!("Skill: stat 失败 {e}")))?;
        if meta.len() > MAX_SKILL_BYTES {
            return Err(AppError::msg(format!(
                "Skill: SKILL.md 过大（{} 字节），跳过",
                meta.len()
            )));
        }
        let content = std::fs::read_to_string(&skill.path)
            .map_err(|e| AppError::msg(format!("Skill: 读取失败 {e}")))?;

        let body = strip_frontmatter(&content);
        let base_dir = skill
            .path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(format!(
            "Skill: {}\nBase directory: {base_dir}\n\n{body}",
            skill.name
        ))
    }
}

fn strip_frontmatter(content: &str) -> &str {
    if !content.starts_with("---") {
        return content;
    }
    content
        .splitn(3, "---")
        .nth(2)
        .map(|s| s.trim_start_matches('\n'))
        .unwrap_or(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, frontmatter: &str, body: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content = if frontmatter.is_empty() {
            body.to_string()
        } else {
            format!("---\n{frontmatter}\n---\n{body}")
        };
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn parses_frontmatter_name_and_description() {
        let (name, desc) =
            parse_frontmatter("---\nname: commit\ndescription: \"Commit changes\"\n---\nbody\n");
        assert_eq!(name.as_deref(), Some("commit"));
        assert_eq!(desc.as_deref(), Some("Commit changes"));
    }

    #[tokio::test]
    async fn executes_returns_skill_md_body() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join(".claude/skills");
        write_skill(
            &skills_dir,
            "demo",
            "name: demo\ndescription: A demo",
            "Hello from skill",
        );

        let mut m = BTreeMap::new();
        load_dir_into(&skills_dir, SkillSource::Project, &mut m);
        let skills: Vec<_> = m.into_values().collect();

        let tool = SkillTool::new(skills);
        let out = tool.execute(json!({"skill": "demo"})).await.unwrap();
        assert!(out.contains("Hello from skill"));
        assert!(out.contains("Skill: demo"));
    }

    #[tokio::test]
    async fn unknown_skill_errors() {
        let tool = SkillTool::new(Vec::new());
        let res = tool.execute(json!({"skill": "missing"})).await;
        assert!(res.is_err());
    }
}
