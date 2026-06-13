//! PreviewAct：内置浏览器「元素对话」旁支会话专用的页面交互信号工具（架构 §8.5）。
//!
//! 与 PreviewStyle 同源：agent-core 不碰 webview，execute 只返回确认；真正在预览页
//! 点击/输入/滚动/hover/按键由 Desktop 观察事件流里的本工具调用、下发 inspector 执行。
//! 用途：触发弹窗 / hover 菜单 / 表单校验等交互态——前端不全是死样式，光调 CSS 测不出。
//!
//! 仅在旁支会话的 `enabled_tools` 含 `PreviewAct` 时才暴露给模型（不进 BUILTIN_TOOL_NAMES）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};

use crate::tools::Tool;

pub const PREVIEW_ACT_TOOL_NAME: &str = "PreviewAct";

#[derive(Debug, Deserialize)]
pub struct PreviewActInput {
    /// 动作：`click` / `type` / `scroll` / `hover` / `press`。
    pub action: String,
    /// 操作哪个元素：`@N`（选中元素）或任意 CSS selector（可操作页面上任何元素，
    /// 不限于圈选的）。缺省主元素 `@1`。
    #[serde(default)]
    pub target: Option<String>,
    /// `action=type` 时：要输入的文字。
    #[serde(default)]
    pub text: Option<String>,
    /// `action=press` 时：按键名（Enter / Escape / ArrowDown…）。
    #[serde(default)]
    pub key: Option<String>,
    /// `action=scroll` 时：滚动量（px，正 = 向下）。
    #[serde(default)]
    pub delta: Option<i32>,
}

pub struct PreviewActTool;

#[async_trait]
impl Tool for PreviewActTool {
    fn name(&self) -> &str {
        PREVIEW_ACT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Interact with the page preview to trigger interactive states (dialogs, hover \
         menus, form validation) that pure CSS tweaks can't reach. action=click clicks \
         the target; action=type focuses it and types `text`; action=hover dispatches \
         hover; action=press sends a `key` like Enter or Escape; action=scroll scrolls \
         by `delta` px. `target` is @N (a selected element, defaults to @1) or any CSS \
         selector — you can act on elements the user did not select, e.g. open a \
         dropdown to inspect its menu. Effects happen live in the preview so the user \
         sees them."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": { "type": "string", "enum": ["click", "type", "scroll", "hover", "press"], "description": "click | type | scroll | hover | press" },
                "target": { "type": "string", "description": "@N for a selected element, or a CSS selector. Defaults to @1." },
                "text": { "type": "string", "description": "Text to type (action=type)" },
                "key": { "type": "string", "description": "Key name like Enter, Escape, ArrowDown (action=press)" },
                "delta": { "type": "integer", "description": "Scroll amount in px, positive = down (action=scroll)" }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: PreviewActInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewAct input: {e}")))?;
        let target = parsed.target.as_deref().unwrap_or("@1");
        Ok(format!("已在预览元素 {target} 执行：{}", parsed.action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn click_returns_ack() {
        let out = PreviewActTool
            .execute(serde_json::json!({ "action": "click", "target": "@2" }))
            .await
            .unwrap();
        assert!(out.contains("click"));
        assert!(out.contains("@2"));
    }

    #[tokio::test]
    async fn target_defaults_to_primary() {
        let out = PreviewActTool
            .execute(serde_json::json!({ "action": "scroll", "delta": 200 }))
            .await
            .unwrap();
        assert!(out.contains("@1"), "缺省 target 应为 @1，实际: {out}");
    }

    #[tokio::test]
    async fn invalid_input_errors() {
        let r = PreviewActTool
            .execute(serde_json::json!({ "nope": 1 }))
            .await;
        assert!(r.is_err(), "缺 action 应报错");
    }
}