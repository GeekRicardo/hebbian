//! Workspace/project 持久化（独立于 session）。
//!
//! 项目是 session 之上的一层命名实体：每个项目保存一个主 workdir 和若干
//! allowed_paths。新建 session 时可以从项目复制这组默认路径，但 session 仍可独立修改。

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};

use super::lock;

const PROJECT_DIR: &str = "projects";

fn projects_root(data_dir: &Path) -> PathBuf {
    data_dir.join(PROJECT_DIR)
}

fn project_path(data_dir: &Path, id: &str) -> PathBuf {
    projects_root(data_dir).join(format!("{id}.json"))
}

fn workspace_path(data_dir: &Path, id: &str) -> PathBuf {
    projects_root(data_dir).join(format!("{id}.code-workspace"))
}

fn sanitize_component(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        let keep = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.';
        if keep {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').trim_matches('.').to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

fn default_id(name: &str) -> String {
    format!(
        "{}-{}",
        sanitize_component(name),
        uuid::Uuid::new_v4().simple()
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFolder {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceProject {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folders: Vec<WorkspaceFolder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

impl WorkspaceProject {
    pub fn workdir(&self) -> Option<&PathBuf> {
        self.folders.first().map(|folder| &folder.path)
    }

    pub fn allowed_paths(&self) -> Vec<PathBuf> {
        self.folders
            .iter()
            .skip(1)
            .map(|folder| folder.path.clone())
            .collect()
    }

    pub fn from_parts(
        id: String,
        name: String,
        workdir: PathBuf,
        allowed_paths: Vec<PathBuf>,
        source: Option<String>,
        created_at: i64,
        updated_at: i64,
    ) -> Self {
        let mut folders = vec![WorkspaceFolder {
            path: workdir,
            name: Some("workdir".to_string()),
        }];
        folders.extend(
            dedup_paths(allowed_paths)
                .into_iter()
                .map(|path| WorkspaceFolder { path, name: None }),
        );
        Self {
            id,
            name,
            folders,
            source,
            created_at,
            updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceProjectsFile {
    #[serde(default)]
    pub projects: Vec<WorkspaceProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceProjectInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub workdir: PathBuf,
    #[serde(default)]
    pub allowed_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VscodeWorkspaceFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    folders: Vec<VscodeWorkspaceFolder>,
}

#[derive(Debug, Deserialize)]
struct VscodeWorkspaceFolder {
    path: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct HebbianWorkspaceFile {
    name: String,
    folders: Vec<WorkspaceFolder>,
}

fn ensure_project_dir(data_dir: &Path) -> AppResult<()> {
    std::fs::create_dir_all(projects_root(data_dir))?;
    Ok(())
}

fn normalize_project(mut project: WorkspaceProject) -> WorkspaceProject {
    if project.id.trim().is_empty() {
        project.id = default_id(&project.name);
    } else {
        project.id = sanitize_component(&project.id);
    }
    project.name = project.name.trim().to_string();
    if project.name.is_empty() {
        project.name = "项目".to_string();
    }
    project.folders = dedup_folders(project.folders);
    if project
        .source
        .as_deref()
        .is_some_and(|s| s.trim().is_empty())
    {
        project.source = None;
    }
    project
}

fn dedup_folders(items: Vec<WorkspaceFolder>) -> Vec<WorkspaceFolder> {
    let mut out: Vec<WorkspaceFolder> = Vec::new();
    for item in items {
        if !out.iter().any(|folder| folder.path == item.path) {
            out.push(item);
        }
    }
    out
}

fn dedup_paths(items: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for item in items {
        if !out.iter().any(|p| p == &item) {
            out.push(item);
        }
    }
    out
}

fn write_project(data_dir: &Path, project: WorkspaceProject) -> AppResult<WorkspaceProject> {
    ensure_project_dir(data_dir)?;
    let project = normalize_project(project);
    let path = project_path(data_dir, &project.id);
    let bytes = serde_json::to_vec_pretty(&project)?;
    lock::write_atomic(&path, &bytes)?;
    Ok(project)
}

pub fn load(data_dir: &Path) -> AppResult<WorkspaceProjectsFile> {
    ensure_project_dir(data_dir)?;
    let root = projects_root(data_dir);
    let mut projects = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = match lock::read_locked(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "读取 project 文件失败");
                continue;
            }
        };
        match serde_json::from_slice::<WorkspaceProject>(&bytes) {
            Ok(project) => projects.push(normalize_project(project)),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "解析 project 文件失败");
            }
        }
    }
    projects.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(WorkspaceProjectsFile { projects })
}

pub fn list(data_dir: &Path) -> AppResult<Vec<WorkspaceProject>> {
    Ok(load(data_dir)?.projects)
}

pub fn get(data_dir: &Path, id: &str) -> AppResult<WorkspaceProject> {
    let path = project_path(data_dir, id);
    if !path.exists() {
        return Err(AppError::msg(format!("找不到项目：{id}")));
    }
    let bytes = lock::read_locked(&path)?;
    let project = serde_json::from_slice::<WorkspaceProject>(&bytes)?;
    Ok(normalize_project(project))
}

pub fn save(data_dir: &Path, input: WorkspaceProjectInput) -> AppResult<WorkspaceProject> {
    ensure_project_dir(data_dir)?;
    if input.workdir.as_os_str().is_empty() {
        return Err(AppError::msg("项目主目录不能为空"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let existing = input.id.as_deref().and_then(|id| get(data_dir, id).ok());
    let project = WorkspaceProject::from_parts(
        input.id.clone().unwrap_or_else(|| default_id(&input.name)),
        input.name,
        input.workdir,
        input.allowed_paths,
        input.source,
        existing.as_ref().map(|p| p.created_at).unwrap_or(now),
        now,
    );
    write_project(data_dir, project)
}

fn save_workspace_file(data_dir: &Path, project: &WorkspaceProject) -> AppResult<()> {
    let path = workspace_path(data_dir, &project.id);
    let file = HebbianWorkspaceFile {
        name: project.name.clone(),
        folders: project.folders.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&file)?;
    lock::write_atomic(&path, &bytes)
}

pub fn delete(data_dir: &Path, id: &str) -> AppResult<()> {
    let path = project_path(data_dir, id);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let workspace = workspace_path(data_dir, id);
    if workspace.exists() {
        std::fs::remove_file(workspace)?;
    }
    Ok(())
}

fn normalize_lexical(path: PathBuf) -> PathBuf {
    let mut prefix: Option<OsString> = None;
    let mut rooted = false;
    let mut parts: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
                parts.clear();
            }
            Component::RootDir => {
                rooted = true;
                parts.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|part| part != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push(OsString::from(".."));
                }
            }
            Component::Normal(part) => parts.push(part.to_os_string()),
        }
    }

    let mut out = PathBuf::new();
    if let Some(prefix) = prefix {
        out.push(prefix);
    }
    if rooted {
        out.push(Path::new("/"));
    }
    for part in parts {
        out.push(part);
    }
    out
}

fn normal_components(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect()
}

fn relative_from_base(target: &Path, base: &Path) -> PathBuf {
    let target = normalize_lexical(target.to_path_buf());
    let base = normalize_lexical(base.to_path_buf());
    if !target.is_absolute() || !base.is_absolute() {
        return target;
    }

    let target_parts = normal_components(&target);
    let base_parts = normal_components(&base);
    let common_len = target_parts
        .iter()
        .zip(base_parts.iter())
        .take_while(|(target_part, base_part)| target_part == base_part)
        .count();

    let mut out = PathBuf::new();
    for _ in common_len..base_parts.len() {
        out.push("..");
    }
    for part in &target_parts[common_len..] {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn resolve_imported_absolute_path(path: PathBuf, workspace_root: &Path) -> PathBuf {
    normalize_lexical(if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    })
}

fn resolve_imported_allowed_path(
    path: PathBuf,
    workspace_root: &Path,
    relative_base: &Path,
) -> PathBuf {
    if path.is_absolute() {
        normalize_lexical(path)
    } else {
        let absolute = normalize_lexical(workspace_root.join(path));
        relative_from_base(&absolute, relative_base)
    }
}

pub fn import_vscode_workspace(
    data_dir: &Path,
    text: &str,
    name: Option<String>,
    source_path: Option<&Path>,
) -> AppResult<WorkspaceProject> {
    let value: VscodeWorkspaceFile = serde_json::from_str(text)?;
    let mut folders = Vec::new();
    for folder in value.folders {
        folders.push(folder);
    }
    if folders.is_empty() {
        return Err(AppError::msg("workspace 文件里没有可用目录"));
    }
    let workspace_root = source_path
        .and_then(|path| path.parent())
        .unwrap_or_else(|| Path::new("."));
    let project_name = name.unwrap_or_else(|| {
        value
            .name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("导入项目")
            .to_string()
    });
    let workdir = resolve_imported_absolute_path(PathBuf::from(&folders[0].path), workspace_root);
    let allowed_relative_base = workdir.parent().unwrap_or(&workdir);
    let allowed_paths = folders
        .iter()
        .skip(1)
        .map(|folder| {
            resolve_imported_allowed_path(
                PathBuf::from(&folder.path),
                workspace_root,
                allowed_relative_base,
            )
        })
        .collect();
    let mut project = save(
        data_dir,
        WorkspaceProjectInput {
            id: None,
            name: project_name,
            workdir,
            allowed_paths,
            source: Some("vscode_workspace".to_string()),
        },
    )?;
    for (project_folder, vscode_folder) in project.folders.iter_mut().zip(folders.iter()) {
        project_folder.name = vscode_folder.name.clone();
    }
    let project = write_project(data_dir, project)?;
    save_workspace_file(data_dir, &project)?;
    Ok(project)
}

pub fn create(
    data_dir: &Path,
    name: String,
    workdir: PathBuf,
    allowed_paths: Vec<PathBuf>,
) -> AppResult<WorkspaceProject> {
    save(
        data_dir,
        WorkspaceProjectInput {
            id: None,
            name,
            workdir,
            allowed_paths,
            source: Some("manual".to_string()),
        },
    )
}

pub fn update(
    data_dir: &Path,
    id: &str,
    name: String,
    workdir: PathBuf,
    allowed_paths: Vec<PathBuf>,
) -> AppResult<WorkspaceProject> {
    save(
        data_dir,
        WorkspaceProjectInput {
            id: Some(id.to_string()),
            name,
            workdir,
            allowed_paths,
            source: Some("manual".to_string()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("hebbian-project-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_and_load_projects_roundtrip() {
        let dir = tmp("roundtrip");
        let saved = create(
            &dir,
            "My Project".to_string(),
            PathBuf::from("/tmp/work"),
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/a")],
        )
        .unwrap();
        assert_eq!(saved.workdir(), Some(&PathBuf::from("/tmp/work")));
        assert_eq!(saved.allowed_paths(), vec![PathBuf::from("/tmp/a")]);

        let loaded = list(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "My Project");
        assert_eq!(loaded[0].workdir(), Some(&PathBuf::from("/tmp/work")));
    }

    #[test]
    fn import_vscode_workspace_uses_first_folder_as_workdir() {
        let dir = tmp("import");
        let saved = import_vscode_workspace(
            &dir,
            r#"{"folders":[{"path":"/a"},{"path":"/b"},{"path":"/c"}]}"#,
            None,
            None,
        )
        .unwrap();
        assert_eq!(saved.workdir(), Some(&PathBuf::from("/a")));
        assert_eq!(
            saved.allowed_paths(),
            vec![PathBuf::from("/b"), PathBuf::from("/c")]
        );
    }

    #[test]
    fn import_vscode_workspace_resolves_relative_paths_against_workspace_root() {
        let dir = tmp("import-relative");
        let source = PathBuf::from("/repo/workspaces/hebbian.code-workspace");
        let saved = import_vscode_workspace(
            &dir,
            r#"{"folders":[{"path":"repo-root"},{"path":"src"},{"path":"/abs/data"}]}"#,
            None,
            Some(&source),
        )
        .unwrap();
        assert_eq!(
            saved.workdir(),
            Some(&PathBuf::from("/repo/workspaces/repo-root"))
        );
        assert_eq!(
            saved.allowed_paths(),
            vec![PathBuf::from("src"), PathBuf::from("/abs/data"),]
        );
    }

    #[test]
    fn import_vscode_workspace_reexpresses_relative_allowed_paths_from_workdir_parent() {
        let dir = tmp("import-relative-siblings");
        let source = PathBuf::from("/Users/ricardo/code/ricardo/other/hebbian.code-workspace");
        let saved = import_vscode_workspace(
            &dir,
            r#"{"folders":[{"path":"../rust/hebbian"},{"path":"sub2api"},{"path":"../claude-code-haha"},{"path":"../rust/cc-switch"}]}"#,
            Some("hebbian".to_string()),
            Some(&source),
        )
        .unwrap();
        assert_eq!(
            saved.workdir(),
            Some(&PathBuf::from("/Users/ricardo/code/ricardo/rust/hebbian"))
        );
        assert_eq!(
            saved.allowed_paths(),
            vec![
                PathBuf::from("../other/sub2api"),
                PathBuf::from("../claude-code-haha"),
                PathBuf::from("cc-switch"),
            ]
        );
    }
}
