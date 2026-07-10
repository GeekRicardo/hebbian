//! CreateSubagent：会话级临时 subagent 创建工具（架构 §4.4.11.4 session 层）。
//!
//! 模型调 `CreateSubagent(name, description, system_prompt, ...)` 在当前会话内
//! 动态创建一个临时 subagent 定义。定义仅存内存（进程级路由表，按 session_id 隔离），
//! 进程重启即丢失。创建后模型可通过 `Task(subagent_type=name, ...)` 立即复用。
//!
//! 合并优先级：session 级 > 磁盘 > 内置（同名覆盖）。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::storage::subagents::{
    session_subagents_for, SubagentDefinition, SubagentPermission, SubagentSource,
};
use crate::tools::Tool;

pub const CREATE_SUBAGENT_TOOL_NAME: &str = "CreateSubagent";

#[derive(Debug, Deserialize)]
pub struct CreateSubagentInput {
    /// subagent 唯一 id，同时也是 Task 工具 `subagent_type` 参数值。kebab-case。
    pub name: String,
    /// 单行描述。Task 工具 description 里会平铺展示。
    pub description: String,
    /// system prompt 正文。
    pub system_prompt: String,
    /// 受限工具白名单（PascalCase）。`None` / 缺省 = 继承父的全工具集（除 Task 自身）。
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// 模型 id。`None` = 跟父 Run 用同模型。
    #[serde(default)]
    pub model: Option<String>,
    /// 单次 Task 调用的最大 ToolStep 次数。`None` = 用默认值。
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// 权限维度。`None` = Inherit（跟父 RunMode）。
    #[serde(default)]
    pub permission: Option<SubagentPermission>,
}

pub struct CreateSubagentTool {
    session_id: Option<String>,
}

impl CreateSubagentTool {
    pub fn new(session_id: Option<String>) -> Self {
        Self { session_id }
    }
}

/// 校验 name 为合法 kebab-case（仅小写字母 / 数字 / 连字符，不以连字符开头或结尾）。
fn validate_kebab_case(name: &str) -> AppResult<()> {
    if name.is_empty() {
        return Err(AppError::msg("name 不能为空"));
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !valid {
        return Err(AppError::msg(format!(
            "name `{name}` 不符合 kebab-case（仅小写字母、数字、连字符，不以连字符开头或结尾）"
        )));
    }
    Ok(())
}

#[async_trait]
impl Tool for CreateSubagentTool {
    fn name(&self) -> &str {
        CREATE_SUBAGENT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Create a temporary subagent definition for the current session. \
         The definition lives in memory only (lost on restart) and can be immediately \
         used via the Task tool with `subagent_type` set to the `name` you chose. \
         If a subagent with the same name already exists (built-in or disk-based), \
         the session-level definition takes precedence."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "description", "system_prompt"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Unique id for the subagent (kebab-case). Used as `subagent_type` in the Task tool."
                },
                "description": {
                    "type": "string",
                    "description": "One-line description of what this subagent does."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "The system prompt body for the subagent."
                },
                "tools": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Restricted tool whitelist (PascalCase tool names). Omit to inherit the parent's full toolset (except Task itself)."
                },
                "model": {
                    "type": "string",
                    "description": "Model id. Omit to use the same model as the parent Run."
                },
                "max_iterations": {
                    "type": "integer",
                    "description": "Max tool-call steps per Task invocation. Omit to use the default."
                },
                "permission": {
                    "type": "string",
                    "enum": ["inherit", "acceptEdits", "bypass"],
                    "description": "Permission mode for the subagent's NestedRun. `inherit` (default) follows the parent RunMode; `acceptEdits` forces Default semantics; `bypass` auto-approves whitelisted tools."
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let Some(sid) = &self.session_id else {
            return Err(AppError::msg(
                "CreateSubagent 需要会话上下文（session_id），当前未绑定会话",
            ));
        };

        let parsed: CreateSubagentInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid CreateSubagent input: {e}")))?;

        validate_kebab_case(&parsed.name)?;

        if parsed.description.trim().is_empty() {
            return Err(AppError::msg("description 不能为空"));
        }
        if parsed.system_prompt.trim().is_empty() {
            return Err(AppError::msg("system_prompt 不能为空"));
        }

        let def = SubagentDefinition {
            name: parsed.name.clone(),
            description: parsed.description,
            system_prompt: parsed.system_prompt,
            tools: parsed.tools,
            model: parsed.model,
            max_iterations: parsed.max_iterations,
            enabled: true,
            source: SubagentSource::Session,
            permission: parsed.permission,
        };

        let lock = session_subagents_for(sid);
        {
            let mut list = lock.write().expect("session subagents rwlock");
            // 同名覆盖：session 级定义覆盖已有的 session 级定义
            if let Some(existing) = list.iter_mut().find(|d| d.name == def.name) {
                *existing = def.clone();
            } else {
                list.push(def.clone());
            }
        }

        Ok(format!(
            "已创建临时 subagent `{}`。通过 Task 工具调用时设 `subagent_type=\"{}\"` 即可使用。",
            def.name, def.name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(name: &str) -> Value {
        json!({
            "name": name,
            "description": "test agent",
            "system_prompt": "You are a test agent."
        })
    }

    #[test]
    fn validate_kebab_case_accepts_valid_names() {
        assert!(validate_kebab_case("code-reviewer").is_ok());
        assert!(validate_kebab_case("my-agent-123").is_ok());
        assert!(validate_kebab_case("a").is_ok());
    }

    #[test]
    fn validate_kebab_case_rejects_invalid_names() {
        assert!(validate_kebab_case("").is_err());
        assert!(validate_kebab_case("CodeReviewer").is_err());
        assert!(validate_kebab_case("code_reviewer").is_err());
        assert!(validate_kebab_case("-leading").is_err());
        assert!(validate_kebab_case("trailing-").is_err());
        assert!(validate_kebab_case("has space").is_err());
    }

    #[tokio::test]
    async fn execute_without_session_id_returns_error() {
        let tool = CreateSubagentTool::new(None);
        let res = tool.execute(make_input("test-agent")).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("session_id"));
    }

    #[tokio::test]
    async fn execute_creates_session_subagent() {
        let sid = format!("test-create-{}", uuid::Uuid::new_v4());
        let tool = CreateSubagentTool::new(Some(sid.clone()));
        let res = tool.execute(make_input("my-agent")).await;
        assert!(res.is_ok());

        let defs = crate::storage::subagents::take_session_subagents(&sid);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "my-agent");
        assert_eq!(defs[0].source, SubagentSource::Session);
        assert!(defs[0].enabled);

        crate::storage::subagents::discard_session_subagents(&sid);
    }

    #[tokio::test]
    async fn execute_same_name_overwrites_existing_session_def() {
        let sid = format!("test-overwrite-{}", uuid::Uuid::new_v4());
        let tool = CreateSubagentTool::new(Some(sid.clone()));

        // 第一次创建
        tool.execute(json!({
            "name": "dup",
            "description": "first",
            "system_prompt": "v1"
        }))
        .await
        .unwrap();

        // 同名覆盖
        tool.execute(json!({
            "name": "dup",
            "description": "second",
            "system_prompt": "v2"
        }))
        .await
        .unwrap();

        let defs = crate::storage::subagents::take_session_subagents(&sid);
        assert_eq!(defs.len(), 1, "同名应覆盖，不应追加");
        assert_eq!(defs[0].description, "second");
        assert_eq!(defs[0].system_prompt, "v2");

        crate::storage::subagents::discard_session_subagents(&sid);
    }

    #[tokio::test]
    async fn execute_rejects_invalid_name() {
        let sid = format!("test-invalid-{}", uuid::Uuid::new_v4());
        let tool = CreateSubagentTool::new(Some(sid.clone()));
        let res = tool
            .execute(json!({
                "name": "InvalidName",
                "description": "x",
                "system_prompt": "y"
            }))
            .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("kebab-case"));

        crate::storage::subagents::discard_session_subagents(&sid);
    }

    #[tokio::test]
    async fn execute_with_optional_fields() {
        let sid = format!("test-optional-{}", uuid::Uuid::new_v4());
        let tool = CreateSubagentTool::new(Some(sid.clone()));
        tool.execute(json!({
            "name": "full-agent",
            "description": "full",
            "system_prompt": "be thorough",
            "tools": ["Read", "Grep", "Bash"],
            "model": "gpt-4o",
            "max_iterations": 30,
            "permission": "bypass"
        }))
        .await
        .unwrap();

        let defs = crate::storage::subagents::take_session_subagents(&sid);
        assert_eq!(defs.len(), 1);
        let d = &defs[0];
        assert_eq!(
            d.tools.as_deref(),
            Some(&["Read".to_string(), "Grep".to_string(), "Bash".to_string()][..])
        );
        assert_eq!(d.model.as_deref(), Some("gpt-4o"));
        assert_eq!(d.max_iterations, Some(30));
        assert_eq!(d.permission, Some(SubagentPermission::Bypass));

        crate::storage::subagents::discard_session_subagents(&sid);
    }
}
