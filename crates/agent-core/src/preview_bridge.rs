//! PreviewBridge：内置浏览器预览页的**观察通道** trait（架构 §8.5）。
//!
//! 旁支会话的工具需要看到预览页的真实状态（截图 / 生效的 CSS 规则 / DOM 结构），
//! 但 agent-core 不能依赖 tauri / webview。本 trait 是那条边界：agent-core 只定义
//! 能力面，Desktop 用 CDP 客户端实现它（连 CEF / 任意 Chromium 的 DevTools 端口）。
//!
//! 只覆盖**读路径**。改动（样式/结构/交互）仍走信号工具 + inspector 通道——
//! inspector 持有提交到主对话所需的 diff 账本，绕过它会丢精确改动记录。

use async_trait::async_trait;
use common::AppResult;

#[async_trait]
pub trait PreviewBridge: Send + Sync {
    /// 截当前预览页（PNG 字节）。`selector` 给定时只截该元素的包围盒。
    async fn capture(&self, selector: Option<&str>) -> AppResult<Vec<u8>>;

    /// 查询 selector 首个匹配元素的生效 CSS 规则链（来源选择器 + 声明，
    /// 按优先级排列）。返回给模型直接可读的文本。
    async fn matched_rules(&self, selector: &str) -> AppResult<String>;

    /// 在预览页执行 JS 表达式，返回 JSON 序列化的求值结果。
    /// 工具用它读 DOM 结构 / 兄弟元素 / computed style 等任意页面状态。
    async fn eval(&self, expression: &str) -> AppResult<String>;
}
