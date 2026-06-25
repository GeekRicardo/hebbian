use chrono::Utc;
use common::storage;
use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub content: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PromptsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt_id: Option<String>,
    #[serde(default)]
    pub prompts: Vec<Prompt>,
}

fn path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("prompts.json")
}

pub fn load(data_dir: &Path) -> AppResult<PromptsFile> {
    let mut file: PromptsFile = storage::read_json(&path(data_dir))?;
    let mut changed = false;
    if file.prompts.is_empty() {
        file.prompts = default_presets();
        changed = true;
    }
    changed |= normalize_default_prompt(&mut file);
    if changed {
        storage::write_json(&path(data_dir), &file)?;
    }
    Ok(file)
}

pub fn save(data_dir: &Path, file: &PromptsFile) -> AppResult<()> {
    let mut file = PromptsFile {
        default_prompt_id: file.default_prompt_id.clone(),
        prompts: file.prompts.clone(),
    };
    normalize_default_prompt(&mut file);
    storage::write_json(&path(data_dir), &file)
}

pub fn upsert(data_dir: &Path, mut prompt: Prompt) -> AppResult<Prompt> {
    let now = Utc::now().timestamp_millis();
    let mut file = load(data_dir)?;
    if let Some(existing) = file.prompts.iter_mut().find(|p| p.id == prompt.id) {
        prompt.created_at = existing.created_at;
        prompt.updated_at = now;
        *existing = prompt.clone();
    } else {
        if prompt.id.is_empty() {
            prompt.id = uuid::Uuid::new_v4().to_string();
        }
        prompt.created_at = now;
        prompt.updated_at = now;
        file.prompts.push(prompt.clone());
    }
    normalize_default_prompt(&mut file);
    save(data_dir, &file)?;
    Ok(prompt)
}

pub fn delete(data_dir: &Path, id: &str) -> AppResult<()> {
    let mut file = load(data_dir)?;
    file.prompts.retain(|p| p.id != id);
    normalize_default_prompt(&mut file);
    save(data_dir, &file)
}

pub fn set_default(data_dir: &Path, id: Option<String>) -> AppResult<PromptsFile> {
    let mut file = load(data_dir)?;
    let normalized = id.filter(|value| !value.is_empty());
    if let Some(ref prompt_id) = normalized {
        if !file.prompts.iter().any(|prompt| prompt.id == *prompt_id) {
            return Err(AppError::msg(format!("prompt {prompt_id} not found")));
        }
    }
    file.default_prompt_id = normalized;
    normalize_default_prompt(&mut file);
    save(data_dir, &file)?;
    Ok(file)
}

fn normalize_default_prompt(file: &mut PromptsFile) -> bool {
    let default_is_valid = file
        .default_prompt_id
        .as_ref()
        .is_some_and(|id| file.prompts.iter().any(|prompt| prompt.id == *id));
    if default_is_valid {
        return false;
    }

    // 兜底优先名为 Hebbian 的角色，缺失才退回第一个。
    let next = file
        .prompts
        .iter()
        .find(|prompt| prompt.name == "Hebbian")
        .or_else(|| file.prompts.first())
        .map(|prompt| prompt.id.clone());
    if file.default_prompt_id == next {
        return false;
    }
    file.default_prompt_id = next;
    true
}

/// 主助手人格：在 base_system 的工程 harness 之上叠一层温暖、克制、守边界的
/// 价值观（语气 / 安全 / 用户福祉 / 中立 / 认错 / 谦逊）。编译进二进制，作为
/// 默认 persona 第一条。
const PERSONA_FABLE: &str = include_str!("../../prompts/persona_fable.md");

fn default_presets() -> Vec<Prompt> {
    let now = Utc::now().timestamp_millis();
    vec![
        Prompt {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Fable5".into(),
            avatar: "🌟".into(),
            content: PERSONA_FABLE.into(),
            created_at: now,
            updated_at: now,
        },
        Prompt {
            id: uuid::Uuid::new_v4().to_string(),
            name: "通用助手".into(),
            avatar: "🤖".into(),
            content: "你是一位友好、耐心、知识渊博的通用助手。请用简洁清晰的中文回答。".into(),
            created_at: now,
            updated_at: now,
        },
        Prompt {
            id: uuid::Uuid::new_v4().to_string(),
            name: "代码搭档".into(),
            avatar: "💻".into(),
            content: "你是一位资深全栈工程师，擅长 Rust、TypeScript、Python。回答需给出可运行代码、注明关键权衡，避免过度设计。".into(),
            created_at: now,
            updated_at: now,
        },
        Prompt {
            id: uuid::Uuid::new_v4().to_string(),
            name: "翻译官".into(),
            avatar: "🌐".into(),
            content: "你是一位专业译员。用户给你任意语言的文字，你翻译成地道、通顺的目标语言（默认中↔英），并在必要时加简短注释。".into(),
            created_at: now,
            updated_at: now,
        },
        Prompt {
            id: uuid::Uuid::new_v4().to_string(),
            name: "写作伙伴".into(),
            avatar: "✍️".into(),
            content: "你是一位写作教练，帮助用户打磨表达。给出改后的版本，并用简短 bullet 解释修改缘由。".into(),
            created_at: now,
            updated_at: now,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hebbian-prompts-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp data dir");
        dir
    }

    #[test]
    fn load_bootstraps_default_prompt_id() {
        let dir = temp_data_dir("bootstrap-default");
        let file = load(&dir).expect("load prompts");

        let default_id = file
            .default_prompt_id
            .as_deref()
            .expect("default prompt id");
        assert!(
            file.prompts.iter().any(|prompt| prompt.id == default_id),
            "default id should point at an existing prompt"
        );
    }

    #[test]
    fn deleting_default_prompt_falls_back_to_remaining_prompt() {
        let dir = temp_data_dir("delete-default");
        let file = load(&dir).expect("load prompts");
        let deleted_id = file.default_prompt_id.clone().expect("default prompt id");

        delete(&dir, &deleted_id).expect("delete default prompt");

        let file = load(&dir).expect("reload prompts");
        let next_default_id = file
            .default_prompt_id
            .as_deref()
            .expect("next default prompt id");
        assert_ne!(next_default_id, deleted_id);
        assert!(
            file.prompts
                .iter()
                .any(|prompt| prompt.id == next_default_id),
            "fallback default should point at an existing prompt"
        );
    }

    #[test]
    fn set_default_rejects_unknown_prompt() {
        let dir = temp_data_dir("reject-unknown");
        load(&dir).expect("load prompts");

        let err = set_default(&dir, Some("missing".to_string()))
            .expect_err("unknown default prompt id should fail");

        assert!(err.to_string().contains("prompt missing not found"));
    }

    #[test]
    fn normalize_prefers_hebbian_when_default_invalid() {
        let mut file = PromptsFile {
            default_prompt_id: Some("gone".into()),
            prompts: vec![
                Prompt {
                    id: "a".into(),
                    name: "Fable5".into(),
                    avatar: "🌟".into(),
                    content: "x".into(),
                    created_at: 0,
                    updated_at: 0,
                },
                Prompt {
                    id: "b".into(),
                    name: "Hebbian".into(),
                    avatar: "🤖".into(),
                    content: "y".into(),
                    created_at: 0,
                    updated_at: 0,
                },
            ],
        };
        normalize_default_prompt(&mut file);
        assert_eq!(file.default_prompt_id.as_deref(), Some("b"));
    }
}
