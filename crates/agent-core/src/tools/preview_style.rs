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
    /// 改哪个元素：`@N`（选中元素）或任意 CSS selector（如 `.card-list > li`，
    /// 配合 allMatches 批量改一类元素）。缺省主元素 `@1`。
    #[serde(default)]
    pub target: Option<String>,
    /// target 为 selector 时：true = 应用到所有匹配元素（改「一类」而非「一个」）。
    #[serde(default, rename = "allMatches")]
    pub all_matches: bool,
}

pub struct PreviewStyleTool;

#[async_trait]
impl Tool for PreviewStyleTool {
    fn name(&self) -> &str {
        PREVIEW_STYLE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Apply a CSS style change to elements in the live page preview. `target` is \
         either @N (a user-selected element, defaults to @1) or any CSS selector like \
         `.card-list > li`. When the user's intent covers a class of elements (list \
         items, cards, all buttons of a kind), use a selector with allMatches=true so \
         every matching element changes together — changing only one of several \
         identical siblings looks broken. Call once per property; call again to tweak. \
         The change is applied immediately and the user sees it."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["prop", "value"],
            "properties": {
                "prop": { "type": "string", "description": "CSS property name, e.g. border-radius, color, font-size" },
                "value": { "type": "string", "description": "CSS value, e.g. 12px, #1f2328, 600" },
                "target": { "type": "string", "description": "@N for a selected element, or a CSS selector. Defaults to @1." },
                "allMatches": { "type": "boolean", "description": "When target is a selector: apply to all matching elements (default false = first match only)" }
            }
        })
    }

    /// 信号工具：返回确认即可。实际应用页面样式由 Desktop 观察本调用后下发 inspector。
    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: PreviewStyleInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewStyle input: {e}")))?;
        let target = parsed.target.as_deref().unwrap_or("@1");
        let scope = if parsed.all_matches {
            "（全部匹配元素）"
        } else {
            ""
        };
        Ok(format!(
            "已实时应用到预览元素 {target}{scope}：{} = {}",
            parsed.prop, parsed.value
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_target_and_returns_ack() {
        let out = PreviewStyleTool
            .execute(serde_json::json!({
                "prop": "color",
                "value": "#fff",
                "target": "@2"
            }))
            .await
            .unwrap();
        assert!(out.contains("color"), "确认句应含属性名，实际: {out}");
        assert!(out.contains("@2"), "确认句应含目标，实际: {out}");
    }

    #[tokio::test]
    async fn target_is_optional_defaults_primary() {
        let out = PreviewStyleTool
            .execute(serde_json::json!({ "prop": "color", "value": "#fff" }))
            .await
            .unwrap();
        assert!(out.contains("#fff"));
        assert!(out.contains("@1"), "缺省 target 应为 @1，实际: {out}");
    }
}
