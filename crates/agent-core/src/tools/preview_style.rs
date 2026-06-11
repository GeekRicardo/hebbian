//! PreviewStyle：内置浏览器「元素对话」旁支会话专用的信号工具（架构 §8.5）。
//!
//! 旁支会话绑定一个被选中的页面元素。LLM 调用本工具来「实时改这个元素的样式」——
//! 但 agent-core 不能碰浏览器 webview（不依赖 tauri）。所以本工具的 `execute` 只返回
//! 一句确认；**真正把样式应用到页面**由 Desktop surface 观察事件流里的本工具调用、
//! 下发给 inspector 完成（机制 B：工具调用即信号）。
//!
//! 仅在旁支会话的 `enabled_tools` 含 `PreviewStyle` 时才暴露给模型（不进 BUILTIN_TOOL_NAMES）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};

use crate::tools::Tool;

pub const PREVIEW_STYLE_TOOL_NAME: &str = "PreviewStyle";

#[derive(Debug, Deserialize)]
pub struct PreviewStyleInput {
    /// CSS 属性名，如 `border-radius` / `color` / `font-size`。
    pub prop: String,
    /// CSS 值，如 `12px` / `#1f2328` / `600`。
    pub value: String,
}

pub struct PreviewStyleTool;

#[async_trait]
impl Tool for PreviewStyleTool {
    fn name(&self) -> &str {
        PREVIEW_STYLE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Apply a CSS style change to the web element currently under discussion, \
         live in the page preview. Use this to iteratively adjust the element's look \
         (color, size, spacing, border, font, etc.) while talking with the user. \
         The change is applied immediately and the user sees it. Call it once per \
         property; call again to tweak. Both `prop` (a CSS property name like \
         `border-radius`) and `value` (a CSS value like `12px`) are required."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["prop", "value"],
            "properties": {
                "prop": { "type": "string", "description": "CSS property name, e.g. border-radius, color, font-size" },
                "value": { "type": "string", "description": "CSS value, e.g. 12px, #1f2328, 600" }
            }
        })
    }

    /// 信号工具：返回确认即可。实际应用页面样式由 Desktop 观察本调用后下发 inspector。
    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: PreviewStyleInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewStyle input: {e}")))?;
        Ok(format!(
            "已实时应用到预览元素：{} = {}",
            parsed.prop, parsed.value
        ))
    }
}
