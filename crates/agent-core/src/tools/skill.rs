//! Skill 系统（架构 §6.1.3）：按三层来源加载 SKILL.md 并按需注入：
//!
//! 1. `~/.hebbian/skills/`                          ← 全局
//! 2. `~/.hebbian/projects/<enc(workdir)>/skills/`  ← 项目私有（hebbian 内聚）
//! 3. `<workdir>/.claude/skills/`                   ← 项目代码内嵌（跟 git 同步）
//!
//! 同名 skill 后者覆盖前者。`~/.claude/skills/` **不默认加载**——通过
//! [`crate::storage::skills::import_from_claude`] 一次性导入。
//!
//! - SKILL.md 头部 frontmatter（YAML）解析 name + description
//! - SkillTool 是「读取 SKILL.md 套壳」：模型给出 skill 名，我们把整篇内容塞回去
//! - 工具 description 动态包含可用 skill 列表（让模型知道有哪些 skill 可调用）
//! - read-only：默认 auto-approve

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Tool;
use crate::storage::projects;

const MAX_SKILL_BYTES: u64 = 200 * 1024;
const MAX_DESC_PREVIEW: usize = 200;

/// 一个加载好的 skill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// 目录名——`<skills_dir>/<name>/SKILL.md` 拼路径用，永远存在。
    pub name: String,
    /// frontmatter `name:` 字段，仅当与目录名**不同**时填。模型在 prompt 里
    /// 看到的「公开名」优先取它（如果有），调用 Skill 工具时按 name 或 alias 任一匹配。
    /// 用于支持 Claude Code 风格的 skill——目录名是简写、frontmatter 写完整名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub description: String,
    pub path: PathBuf,
    pub source: SkillSource,
    /// 用户在 hebbian 里是否启用该 skill。`false` 时 SkillTool 不会把它暴露给模型。
    /// 由 `storage::skills::apply_disabled` 根据 `~/.hebbian/disabled_skills.json` 填写。
    pub enabled: bool,
    /// 所属 collection id（架构 §6.1.3）。仅给 `CoreClient::list_skills` 这条
    /// 路径用——前端按它分组展示。SkillTool 运行时路径不填（None），不影响模型。
    /// 仅 Global source 的 skill 可能有值；Project / ProjectCode 永远 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
}

impl Skill {
    /// 模型在 prompt / 命令面板里看到的公开名：frontmatter alias 优先，回退目录名。
    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }

    /// SkillTool 调用时是否命中该 skill：目录名或 alias 任一匹配。
    pub fn matches(&self, key: &str) -> bool {
        self.name == key || self.alias.as_deref() == Some(key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// `~/.hebbian/skills/<name>/SKILL.md`
    Global,
    /// `~/.hebbian/projects/<enc>/skills/<name>/SKILL.md`
    Project,
    /// `<workdir>/.claude/skills/<name>/SKILL.md`
    ProjectCode,
}

/// 默认 skill 目录（含 source 标签），按"前 → 后覆盖"顺序排列。
pub fn default_skill_dirs(data_dir: &Path, workdir: &Path) -> Vec<(SkillSource, PathBuf)> {
    vec![
        (SkillSource::Global, data_dir.join("skills")),
        (
            SkillSource::Project,
            projects::project_dir(data_dir, workdir).join("skills"),
        ),
        (SkillSource::ProjectCode, workdir.join(".claude/skills")),
    ]
}

/// 从带 source 的目录列表加载 skills。后面的目录会覆盖前面的同名 skill。
pub fn load_skills(sources: &[(SkillSource, PathBuf)]) -> Vec<Skill> {
    let mut out: BTreeMap<String, Skill> = BTreeMap::new();
    for (source, dir) in sources {
        load_dir_into(dir, *source, &mut out);
    }
    out.into_values().collect()
}

/// 只查一层 `<skills_dir>/<skill-name>/SKILL.md`，不递归。
///
/// `Skill.name` 永远是目录名（read_skill_md 靠它拼路径）；frontmatter 的 `name`
/// 字段若存在且与目录名不同，存到 `Skill.alias`——既作为模型 prompt 里的公开名，
/// 也参与 lookup 兜底匹配。这样 `~/.hebbian/skills/karpathy/` 里
/// `name: karpathy-guidelines` 的 skill 可以被 `/karpathy` 或 `/karpathy-guidelines`
/// 任一调到（Claude Code 风格的 skill 经常这么写）。
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
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".into());
        let alias = name_opt.filter(|n| !n.is_empty() && n != &name);
        let description = desc_opt.unwrap_or_else(|| extract_first_paragraph(&content));
        out.insert(
            name.clone(),
            Skill {
                name,
                alias,
                description,
                path: skill_md,
                source,
                enabled: true,
                collection_id: None,
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
            "当前**没有**可用的 skills。用户可以在 `~/.hebbian/skills/<name>/SKILL.md`、\
             项目目录 `~/.hebbian/projects/<enc>/skills/<name>/SKILL.md` 或项目代码内嵌的 \
             `<workdir>/.claude/skills/<name>/SKILL.md` 里创建。",
        );
    } else {
        s.push_str("可用 skills（按名字调用）：\n");
        for skill in skills {
            let scope = match skill.source {
                SkillSource::Global => "global",
                SkillSource::Project => "project",
                SkillSource::ProjectCode => "project-code",
            };
            let preview = first_words(&skill.description, 80);
            // alias != 目录名时同时列出，让模型知道两个名字都能调（不少 Claude Code
            // 风格 skill 目录名是简写 / frontmatter name 是完整名，如 karpathy /
            // karpathy-guidelines）
            match &skill.alias {
                Some(alias) => s.push_str(&format!(
                    "- `{alias}`（或 `{}`，{scope}）: {preview}\n",
                    skill.name
                )),
                None => s.push_str(&format!("- `{}` ({scope}): {preview}\n", skill.name)),
            }
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
            .find(|s| s.matches(name))
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

    /// 回归 2026-05-23：~/.hebbian/skills/karpathy/SKILL.md 的 frontmatter
    /// `name: karpathy-guidelines` 之前会被丢弃，导致模型按 `/karpathy-guidelines`
    /// 调用时 SkillTool lookup 失败。
    #[tokio::test]
    async fn frontmatter_alias_is_callable_alongside_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        write_skill(
            &skills_dir,
            "karpathy",
            "name: karpathy-guidelines\ndescription: Behavioral guidelines",
            "Karpathy body",
        );

        let mut m = BTreeMap::new();
        load_dir_into(&skills_dir, SkillSource::Global, &mut m);
        let skills: Vec<_> = m.into_values().collect();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "karpathy");
        assert_eq!(skills[0].alias.as_deref(), Some("karpathy-guidelines"));
        assert_eq!(skills[0].display_name(), "karpathy-guidelines");

        let tool = SkillTool::new(skills);
        // 目录名调用照常工作
        let by_dir = tool.execute(json!({"skill": "karpathy"})).await.unwrap();
        assert!(by_dir.contains("Karpathy body"));
        // 带前缀斜杠 + frontmatter 完整名也能命中
        let by_alias = tool
            .execute(json!({"skill": "/karpathy-guidelines"}))
            .await
            .unwrap();
        assert!(by_alias.contains("Karpathy body"));
    }

    /// frontmatter `name:` 等于目录名时 alias 应为 None（避免冗余显示）。
    #[test]
    fn alias_is_none_when_frontmatter_matches_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        write_skill(
            &skills_dir,
            "commit",
            "name: commit\ndescription: same",
            "body",
        );
        let mut m = BTreeMap::new();
        load_dir_into(&skills_dir, SkillSource::Global, &mut m);
        let skills: Vec<_> = m.into_values().collect();
        assert_eq!(skills[0].alias, None);
    }
}
