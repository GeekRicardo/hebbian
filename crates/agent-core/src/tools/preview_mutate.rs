//! PreviewMutate：内置浏览器「元素对话」旁支会话专用的结构操作信号工具（架构 §8.5）。
//!
//! 与 PreviewStyle 同源：agent-core 不碰 webview，execute 只返回确认；真正在预览页
//! 新增/删除/改文本由 Desktop 观察事件流里的本工具调用、下发 inspector 执行。
//! 预览改动是草稿态（刷新即失），最终由「提交到主对话」让主对话改源码落地。
//!
//! 仅在旁支会话的 `enabled_tools` 含 `PreviewMutate` 时才暴露给模型（不进 BUILTIN_TOOL_NAMES）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};

use crate::tools::Tool;

pub const PREVIEW_MUTATE_TOOL_NAME: &str = "PreviewMutate";

#[derive(Debug, Deserialize)]
pub struct PreviewMutateInput {
    /// 操作类型：`append`（在目标内追加）/ `remove`（删目标）/ `setText`（改目标文本）。
    pub op: String,
    /// 操作哪个选中元素（`@2`）；缺省主元素 `@1`。
    #[serde(default)]
    pub target: Option<String>,
    /// `op=append` 时：要追加的 HTML 片段。
    #[serde(default)]
    pub html: Option<String>,
    /// `op=setText` 时：新文本内容。
    #[serde(default)]
    pub text: Option<String>,
}

pub struct PreviewMutateTool;

#[async_trait]
impl Tool for PreviewMutateTool {
    fn name(&self) -> &str {
        PREVIEW_MUTATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Mutate the DOM structure of a selected web element live in the page preview. \
         op=append adds an HTML fragment inside the target; op=remove deletes the target; \
         op=setText replaces the target's text. target is @N (defaults to @1). \
         This is a DRAFT in the preview only (lost on reload) — the user will later submit \
         it so the main conversation edits the real source. Keep appended `html` semantically \
         clean so it maps back to JSX easily."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": { "type": "string", "enum": ["append", "remove", "setText"], "description": "append | remove | setText" },
                "target": { "type": "string", "description": "Which selected element, like @2. Defaults to @1." },
                "html": { "type": "string", "description": "HTML fragment to append (op=append)" },
                "text": { "type": "string", "description": "New text content (op=setText)" }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: PreviewMutateInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewMutate input: {e}")))?;
        let target = parsed.target.as_deref().unwrap_or("@1");
        Ok(format!("已对预览元素 {target} 执行结构操作：{}", parsed.op))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_returns_ack() {
        let out = PreviewMutateTool
            .execute(
                serde_json::json!({ "op": "append", "target": "@1", "html": "<button>x</button>" }),
            )
            .await
            .unwrap();
        assert!(out.contains("append"));
        assert!(out.contains("@1"));
    }

    #[tokio::test]
    async fn target_defaults_to_primary() {
        let out = PreviewMutateTool
            .execute(serde_json::json!({ "op": "remove" }))
            .await
            .unwrap();
        assert!(out.contains("@1"), "缺省 target 应为 @1，实际: {out}");
    }

    #[tokio::test]
    async fn invalid_input_errors() {
        let r = PreviewMutateTool
            .execute(serde_json::json!({ "foo": "bar" }))
            .await;
        assert!(r.is_err(), "缺 op 应报错");
    }
}
