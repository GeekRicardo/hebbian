//! PreviewCapture：内置浏览器「元素对话」旁支会话专用的截图工具（架构 §8.5）。
//!
//! 给模型「眼睛」：截预览页（或某元素局部）为 PNG，作为图片附件进下一轮模型
//! 上下文（弱模型由 VisionBridge 转文字）。读路径走 PreviewBridge（CDP），
//! 与信号工具不同——这是真实回执，不是单向确认。
//!
//! 仅在旁支会话注入（带 bridge 构造）；无 bridge 时返回明确的不可用提示。

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use common::attachments::MessageAttachment;
use common::{AppError, AppResult};
use serde::Deserialize;
use serde_json::Value;

use crate::preview_bridge::PreviewBridge;
use crate::tools::{Tool, ToolCtx, ToolOutput};

pub const PREVIEW_CAPTURE_TOOL_NAME: &str = "PreviewCapture";

#[derive(Debug, Deserialize)]
pub struct PreviewCaptureInput {
    /// 只截某元素的包围盒（CSS selector）；缺省截整个视口。
    #[serde(default)]
    pub selector: Option<String>,
}

pub struct PreviewCaptureTool {
    bridge: Option<Arc<dyn PreviewBridge>>,
}

impl PreviewCaptureTool {
    pub fn new(bridge: Option<Arc<dyn PreviewBridge>>) -> Self {
        Self { bridge }
    }
}

#[async_trait]
impl Tool for PreviewCaptureTool {
    fn name(&self) -> &str {
        PREVIEW_CAPTURE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Take a screenshot of the live page preview so you can SEE the result of your \
         changes. Pass `selector` to capture just one element's bounding box; omit it \
         for the full viewport. Use this after applying styles to verify they actually \
         took effect (another CSS rule may override yours), and before styling when \
         the user describes a visual problem you need to look at."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector to capture only that element. Omit for full viewport." }
            }
        })
    }

    async fn execute(&self, _input: Value) -> AppResult<String> {
        Err(AppError::msg("PreviewCapture 需要 execute_rich 路径"))
    }

    async fn execute_rich(&self, _ctx: ToolCtx, input: Value) -> AppResult<ToolOutput> {
        let parsed: PreviewCaptureInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewCapture input: {e}")))?;
        let Some(bridge) = self.bridge.as_ref() else {
            return Ok(ToolOutput {
                text: "当前预览不支持截图（未连接 CDP 通道）。请改用与用户对话确认视觉效果。"
                    .to_string(),
                attachments: Vec::new(),
                is_error: true,
            });
        };
        let png = bridge.capture(parsed.selector.as_deref()).await?;
        let scope = parsed
            .selector
            .as_deref()
            .map(|s| format!("元素 {s}"))
            .unwrap_or_else(|| "整个视口".to_string());
        Ok(ToolOutput {
            text: format!("已截取预览页（{scope}），见附图。"),
            attachments: vec![MessageAttachment::Image {
                name: "preview.png".to_string(),
                media_type: "image/png".to_string(),
                data: base64::engine::general_purpose::STANDARD.encode(&png),
            }],
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBridge;

    #[async_trait]
    impl PreviewBridge for FakeBridge {
        async fn capture(&self, _selector: Option<&str>) -> AppResult<Vec<u8>> {
            Ok(vec![1, 2, 3])
        }
        async fn matched_rules(&self, _selector: &str) -> AppResult<String> {
            unreachable!()
        }
        async fn eval(&self, _expression: &str) -> AppResult<String> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn capture_returns_image_attachment() {
        let tool = PreviewCaptureTool::new(Some(Arc::new(FakeBridge)));
        let out = tool
            .execute_rich(ToolCtx::noop(), serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(out.attachments.len(), 1);
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn no_bridge_degrades_with_clear_message() {
        let tool = PreviewCaptureTool::new(None);
        let out = tool
            .execute_rich(ToolCtx::noop(), serde_json::json!({}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.text.contains("不支持截图"));
    }
}
