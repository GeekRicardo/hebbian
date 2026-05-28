//! Hebbian 可观测性入口。
//!
//! 三路输出：
//! - stderr ANSI（dev 终端可见）
//! - 按天 rotate 的文件（`~/.hebbian/logs/hebbian.log.YYYY-MM-DD`）
//! - broadcast channel（供 Desktop LogPane 实时订阅）

pub mod attr;

use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

const BROADCAST_CAP: usize = 2048;

/// 结构化日志行，Tauri Channel 序列化后推给前端。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogLine {
    /// "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"
    pub level: String,
    /// 模块路径，例如 "agent_core::dispatch"
    pub target: String,
    /// 格式化后的日志正文（含额外字段）
    pub message: String,
    /// "HH:MM:SS.mmm"
    pub ts: String,
}

static LOG_TX: OnceLock<broadcast::Sender<LogLine>> = OnceLock::new();
// Mutex 让 WorkerGuard（Send + !Sync）可以住进 static
static APPENDER_GUARD: OnceLock<Mutex<tracing_appender::non_blocking::WorkerGuard>> =
    OnceLock::new();

/// 获取日志广播发送端（供 `subscribe_log_stream` Tauri 命令订阅）。
pub fn log_sender() -> Option<&'static broadcast::Sender<LogLine>> {
    LOG_TX.get()
}

/// 今天日志文件的路径（`~/.hebbian/logs/hebbian.log.YYYY-MM-DD`）。
pub fn today_log_path() -> Option<std::path::PathBuf> {
    let logs_dir = dirs::home_dir()?.join(".hebbian").join("logs");
    let date = chrono::Local::now().format("%Y-%m-%d");
    Some(logs_dir.join(format!("hebbian.log.{date}")))
}

/// 装本地日志：stderr ANSI + 文件 rotate + 前端广播。
/// 同步调用，重复调用安全（第二次起是 no-op）。
pub fn init(default_filter: &str) {
    // 广播通道
    let (tx, _) = broadcast::channel(BROADCAST_CAP);
    let _ = LOG_TX.set(tx);

    // 文件按天 rotate 到 ~/.hebbian/logs/
    let logs_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".hebbian")
        .join("logs");
    std::fs::create_dir_all(&logs_dir).ok();
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "hebbian.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = APPENDER_GUARD.set(Mutex::new(guard));

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr)
                .with_ansi(true),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(non_blocking)
                .with_ansi(true),
        )
        .with(BroadcastLayer)
        .try_init();
}

/// 把 tracing 事件广播给前端 LogPane。
/// 没有订阅者时 receiver_count() == 0，直接返回，零开销。
struct BroadcastLayer;

impl<S> Layer<S> for BroadcastLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let Some(tx) = LOG_TX.get() else { return };
        if tx.receiver_count() == 0 {
            return;
        }

        let meta = event.metadata();
        let level = meta.level().to_string();
        let target = meta.target().to_string();

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let ts = chrono::Local::now().format("%H:%M:%S%.3f").to_string();
        let _ = tx.send(LogLine {
            level,
            target,
            message: visitor.0,
            ts,
        });
    }
}

#[derive(Default)]
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={}", field.name(), value));
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            if self.0.is_empty() {
                // fmt::Arguments 的 Debug 等价于 Display
                self.0 = format!("{value:?}");
            }
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={:?}", field.name(), value));
        }
    }
}
