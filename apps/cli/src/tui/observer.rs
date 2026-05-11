//! TuiObserver：保留接口对称（与 CliObserver 并行实现）。
//!
//! 目前 TUI 并不通过 `RunHandle::drive(observer)` 走，而是在 `app.rs` 内的主循环里
//! 直接 `recv()` 事件并自行渲染，因为 ratatui 重绘和键盘事件 race 用 select 更直接，
//! TurnObserver 的同步 `on_event` 回调难以承载 UI 状态。
//!
//! 这个模块留作未来重构成 `TurnObserver` 的占位。

pub struct TuiObserver;
