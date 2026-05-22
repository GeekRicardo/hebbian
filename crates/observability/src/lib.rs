//! Hebbian 可观测性入口。
//!
//! 当前定位：**只装本地 stderr 日志**，不接外部 trace/metric backend。
//!
//! 想看模型 IO 原文走 `~/.hebbian/sessions/<sid>/model_io.jsonl` + Model I/O
//! 调试器抽屉 / `heb model-io <sid>`；想看 transcript 走 `session.jsonl`。
//! 跨服务追踪 / SRE 监控大盘场景目前不存在，所以不挂 OTLP。
//!
//! 历史背景：本 crate 之前装过完整 `tracing-opentelemetry` + OTLP HTTP exporter
//! + Histogram/Counter，但大字段（gen_ai.prompt / langfuse.*）一旦挂上 INFO span，
//! `tracing_subscriber::fmt` 默认会把它们串到每条事件前缀刷屏，且 Langfuse 已停用。
//! 详见 2026-05-22 changelog。

pub mod attr;

use tracing_subscriber::EnvFilter;

/// 装本地 stderr 日志。同步函数，任何线程都能调；重复调用安全。
///
/// `RUST_LOG` 优先；否则用 `default_filter`。
pub fn init(default_filter: &str) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}
