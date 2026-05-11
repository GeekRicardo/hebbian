//! 全屏 TUI（架构 §8）。
//!
//! 入口 [`run_tui`]：构造好的 `Session` 进来，启动 ratatui 主循环。
//! REPL 简易模式（rustyline 行编辑器）仍在 `crate::session`。

pub mod app;
pub mod components;
pub mod observer;
pub mod theme;

pub use app::run as run_tui;
