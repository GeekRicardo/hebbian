//! Skill 集合（架构 §6.1.3）。
//!
//! 用户从同一个来源（GitHub 仓库 / 本地目录）一次性导入的多个 skill，记一条
//! Collection 元数据，用于：
//! - SkillsPane 按集合分组展示
//! - 整组卸载（一次性删 collection 内所有 skill 目录）
//!
//! 落盘到 `~/.hebbian/skill_collections.json`。仅作用于 **global** scope 导入
//! （即 `~/.hebbian/skills/<name>/`）；project scope 的 import 不记录——后者已经
//! 用 project_dir 自然分组，没必要再加一层。
//!
//! 与 disabled_skills.json 平行，二者无主外键关系：collection 记 skill 目录名，
//! disabled 也记目录名，两边可以共存（被禁用的 skill 仍属于某个 collection）。

use std::path::{Path, PathBuf};

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};

const FILE: &str = "skill_collections.json";

/// 集合来源——决定 SkillsPane 上展示的「来源」徽标 + 卸载提示文案。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CollectionSource {
    /// 来自 git clone --depth=1
    Github {
        repo_url: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subpath: Option<String>,
    },
    /// 来自用户本地目录（Tauri dialog 选的目录或拖拽进来的目录）
    Dir { src_dir: PathBuf },
    /// **虚拟集合**：用户手放的 / 老导入的 Global skill 没有 sidecar 记录时，
    /// `list_skill_collections` 会为每个孤儿 skill 合成一条 Local 集合（label = skill
    /// 目录名 = `~/.hebbian/skills/<name>/` 的 name；path = 该目录绝对路径）。
    /// **不**落盘到 `skill_collections.json`——只在运行时合成。
    Local { path: PathBuf },
}

impl CollectionSource {
    /// 给 UI 用的短展示字符串。
    pub fn display(&self) -> String {
        match self {
            CollectionSource::Github { repo_url, subpath } => {
                let base = repo_url
                    .trim_end_matches('/')
                    .trim_end_matches(".git")
                    .to_string();
                match subpath {
                    Some(p) if !p.is_empty() => format!("{base} ({p})"),
                    _ => base,
                }
            }
            CollectionSource::Dir { src_dir } => src_dir.display().to_string(),
            CollectionSource::Local { path } => path.display().to_string(),
        }
    }
}

/// 虚拟集合的 id 前缀——用 `local:<skill_name>` 形式生成稳定 id。Skill 目录名在
/// `~/.hebbian/skills/` 下唯一，加前缀避免与 sidecar uuid 冲突。
pub const LOCAL_ID_PREFIX: &str = "local:";

pub fn synthetic_local_id(skill_name: &str) -> String {
    format!("{LOCAL_ID_PREFIX}{skill_name}")
}

/// id 是否是 `list_skill_collections` 合成出的虚拟 collection（前端 UI 收到时不用区分，
/// 但 delete_skill_collection 需要根据这个走"删单个 skill 目录"而非"删 sidecar 记录"分支）。
pub fn is_synthetic_local_id(id: &str) -> bool {
    id.starts_with(LOCAL_ID_PREFIX)
}

/// 从虚拟 id 反推 skill 目录名（成功 = `local:karpathy` → `Some("karpathy")`）。
pub fn skill_name_from_local_id(id: &str) -> Option<&str> {
    id.strip_prefix(LOCAL_ID_PREFIX)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCollection {
    /// 唯一 id（uuid v4），用于卸载 / 前端 React key
    pub id: String,
    /// 显示名——GitHub repo 末段或本地目录末段，便于用户辨识
    pub label: String,
    pub source: CollectionSource,
    /// ISO8601 时间戳；前端可用于排序
    pub imported_at: String,
    /// 属于本集合的 skill 目录名（= 与 `~/.hebbian/skills/<name>/` 的 `<name>` 一致）。
    /// 用户重命名 / 手动删除某个 skill 目录后，这里可能指向不存在的 skill——
    /// `list_with_status` 会过滤掉这种 stale entry。
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillCollectionsFile {
    #[serde(default)]
    pub collections: Vec<SkillCollection>,
}

fn path_for(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

pub fn load(data_dir: &Path) -> SkillCollectionsFile {
    let p = path_for(data_dir);
    if !p.exists() {
        return SkillCollectionsFile::default();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(data_dir: &Path, file: &SkillCollectionsFile) -> AppResult<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string_pretty(file)?;
    std::fs::write(path_for(data_dir), json)?;
    Ok(())
}

/// 从 GitHub repo url 推断短展示名：取最后一段、去 `.git` 后缀。
/// `https://github.com/obra/superpowers.git` → `superpowers`
pub fn label_from_github(repo_url: &str) -> String {
    repo_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(repo_url)
        .trim_end_matches(".git")
        .to_string()
}

/// 从本地目录路径推断短展示名：basename。
pub fn label_from_dir(src_dir: &Path) -> String {
    src_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| src_dir.display().to_string())
}

/// 追加一条 collection（已存在的 id 会替换；通常 id 由 uuid 保证唯一）。
pub fn append(data_dir: &Path, collection: SkillCollection) -> AppResult<SkillCollection> {
    let mut file = load(data_dir);
    file.collections.retain(|c| c.id != collection.id);
    file.collections.push(collection.clone());
    save(data_dir, &file)?;
    Ok(collection)
}

/// 按 id 删除一条 collection；返回被删除的记录（可能为 None 表示找不到）。
/// **不删 skill 物理目录**——调用方自己决定是否删（卸载场景=删；仅整理元数据=不删）
pub fn remove(data_dir: &Path, id: &str) -> AppResult<Option<SkillCollection>> {
    let mut file = load(data_dir);
    let pos = file.collections.iter().position(|c| c.id == id);
    let removed = pos.map(|i| file.collections.remove(i));
    if removed.is_some() {
        save(data_dir, &file)?;
    }
    Ok(removed)
}

/// 反查：某个 skill 目录名属于哪个 collection。同一 skill 名在多个 collection
/// 重复时返回第一个（不该发生——append 时同一 collection 内的 skill_names 由
/// import 路径保证唯一；跨 collection 重名会因目录覆盖而只剩最后导入那条）。
pub fn find_by_skill(data_dir: &Path, skill_name: &str) -> Option<SkillCollection> {
    load(data_dir)
        .collections
        .into_iter()
        .find(|c| c.skills.iter().any(|n| n == skill_name))
}

/// 记录一次新 import 的便捷入口——生成 uuid，写文件，返回创建的记录。
pub fn record_import(
    data_dir: &Path,
    label: impl Into<String>,
    source: CollectionSource,
    skills: Vec<String>,
) -> AppResult<SkillCollection> {
    if skills.is_empty() {
        return Err(AppError::msg("空 skill 列表无法记录为 collection"));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let collection = SkillCollection {
        id: uuid::Uuid::new_v4().to_string(),
        label: label.into(),
        source,
        imported_at: now,
        skills,
    };
    append(data_dir, collection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("hebbian-skill-coll-test-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn load_returns_empty_when_file_missing() {
        let dir = tmp("missing");
        let file = load(&dir);
        assert!(file.collections.is_empty());
    }

    #[test]
    fn record_then_find_round_trip() {
        let dir = tmp("roundtrip");
        let c = record_import(
            &dir,
            "superpowers",
            CollectionSource::Github {
                repo_url: "https://github.com/obra/superpowers".into(),
                subpath: None,
            },
            vec!["brainstorming".into(), "writing-skills".into()],
        )
        .unwrap();

        let found = find_by_skill(&dir, "brainstorming").expect("should find");
        assert_eq!(found.id, c.id);
        assert_eq!(found.label, "superpowers");
        match &found.source {
            CollectionSource::Github { repo_url, .. } => {
                assert!(repo_url.ends_with("/superpowers"))
            }
            _ => panic!("wrong source kind"),
        }

        assert!(find_by_skill(&dir, "unknown-skill").is_none());
    }

    #[test]
    fn remove_returns_record_and_persists() {
        let dir = tmp("remove");
        let c = record_import(
            &dir,
            "dir-import",
            CollectionSource::Dir {
                src_dir: PathBuf::from("/tmp/some-dir"),
            },
            vec!["alpha".into()],
        )
        .unwrap();
        let removed = remove(&dir, &c.id).unwrap();
        assert!(removed.is_some());
        // 同 id 不可再被找到
        assert!(find_by_skill(&dir, "alpha").is_none());
        // 二次删除返回 None，不报错
        assert!(remove(&dir, &c.id).unwrap().is_none());
    }

    #[test]
    fn empty_skills_rejected() {
        let dir = tmp("empty-rejected");
        let err = record_import(
            &dir,
            "x",
            CollectionSource::Dir {
                src_dir: PathBuf::from("/tmp/x"),
            },
            vec![],
        )
        .unwrap_err();
        assert!(err.to_string().contains("空"));
    }

    #[test]
    fn label_helpers() {
        assert_eq!(
            label_from_github("https://github.com/obra/superpowers"),
            "superpowers"
        );
        assert_eq!(
            label_from_github("https://github.com/obra/superpowers.git"),
            "superpowers"
        );
        assert_eq!(
            label_from_github("https://github.com/foo/bar/"),
            "bar"
        );
        assert_eq!(
            label_from_dir(&PathBuf::from("/Users/me/proj/skills-dir")),
            "skills-dir"
        );
    }

    #[test]
    fn source_display_includes_subpath() {
        let s = CollectionSource::Github {
            repo_url: "https://github.com/o/r".into(),
            subpath: Some("packages/skills".into()),
        };
        assert!(s.display().contains("packages/skills"));
    }

    #[test]
    fn local_id_helpers_round_trip() {
        let id = synthetic_local_id("karpathy");
        assert_eq!(id, "local:karpathy");
        assert!(is_synthetic_local_id(&id));
        assert_eq!(skill_name_from_local_id(&id), Some("karpathy"));
        // 非 local id 不被误识别
        assert!(!is_synthetic_local_id("some-uuid-here"));
        assert_eq!(skill_name_from_local_id("some-uuid-here"), None);
    }

    #[test]
    fn append_replaces_existing_same_id() {
        let dir = tmp("replace");
        let c1 = SkillCollection {
            id: "fixed-id".into(),
            label: "old".into(),
            source: CollectionSource::Dir {
                src_dir: PathBuf::from("/old"),
            },
            imported_at: "2026-01-01T00:00:00Z".into(),
            skills: vec!["a".into()],
        };
        append(&dir, c1).unwrap();

        let c2 = SkillCollection {
            id: "fixed-id".into(),
            label: "new".into(),
            source: CollectionSource::Dir {
                src_dir: PathBuf::from("/new"),
            },
            imported_at: "2026-02-01T00:00:00Z".into(),
            skills: vec!["b".into()],
        };
        append(&dir, c2).unwrap();

        let file = load(&dir);
        assert_eq!(file.collections.len(), 1);
        assert_eq!(file.collections[0].label, "new");
    }
}
