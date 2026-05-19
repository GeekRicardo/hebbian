//! Hebbian 可观测性入口。
//!
//! 通过 [`init`] 一次性挂上：
//! - `tracing_subscriber::fmt`（stderr 日志，env-filter 控制级别）
//! - `tracing-opentelemetry` layer（把 `tracing` span 镜像成 OTel span）
//! - OTLP HTTP exporter（trace + metrics），由 `OTEL_EXPORTER_OTLP_ENDPOINT` 控制
//!
//! 没设 `OTEL_EXPORTER_OTLP_ENDPOINT` 时只装 stderr 日志，零网络副作用。
//!
//! ## 内置 OTel 后台 runtime
//!
//! [`init`] 是同步函数，可以在 `#[tokio::main]` 之外的入口（如 Tauri 的 sync `run()`）
//! 直接调用。批处理导出 task 跑在 observability 内部独占的 multi-thread runtime
//! 上（1 worker），不依赖 surface 是否提供 tokio 上下文。
//!
//! ## Span 层级（与 Langfuse 对齐）
//!
//! ```text
//! run                                ← agent-core::harness::spawn_run
//! ├── turn                           ← agent-core::agent_loop 每轮
//! │   ├── compaction (条件触发)
//! │   ├── microcompact (条件触发)
//! │   ├── model.request              ← model-gateway provider
//! │   │     attrs: gen_ai.system / .request.model / .usage.* / hebbian.streaming
//! │   ├── tool.call                  ← agent-core::dispatch 每个 call
//! │   │   ├── permission.check (条件触发)
//! │   │     attrs: tool.name / .class / .outcome / .duration_ms
//! ```

pub mod attr;
pub mod metrics;

use once_cell::sync::Lazy;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{
    metrics::{PeriodicReader, SdkMeterProvider},
    runtime,
    trace::TracerProvider,
    Resource,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// OTel 批量导出 task 跑的内部 runtime。1 worker 即可（导出量小、阻塞少）。
static OTEL_RT: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("otel-export")
        .enable_all()
        .build()
        .expect("build OTel export runtime")
});

/// `init` 返回的守卫。drop 时刷新并关闭 trace / metric provider，
/// 进程退出前丢失数据的概率显著降低。
pub struct OtelGuard {
    tracer_provider: Option<TracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Some(tp) = self.tracer_provider.take() {
            let _ = tp.shutdown();
        }
        if let Some(mp) = self.meter_provider.take() {
            let _ = mp.shutdown();
        }
    }
}

/// 一站式初始化：装日志 + (可选) OTLP 导出。同步函数，任何线程都能调。
///
/// `OTEL_EXPORTER_OTLP_ENDPOINT` 缺失时只装 stderr 日志，返回空守卫。
/// 重复调用安全（subscriber 已注册时静默跳过）。
pub fn init(service_name: &str, default_filter: &str) -> OtelGuard {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr);

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
    let Some(endpoint) = endpoint else {
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .try_init();
        return OtelGuard {
            tracer_provider: None,
            meter_provider: None,
        };
    };

    // 进入内部 runtime：BatchSpanProcessor / PeriodicReader spawn 后台 task 时
    // 会落到 OTEL_RT 上，与 surface 的 runtime 完全隔离。
    let _enter = OTEL_RT.enter();

    let resource = Resource::new(vec![
        KeyValue::new("service.name", service_name.to_string()),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ]);

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/traces", endpoint.trim_end_matches('/')))
        .with_protocol(Protocol::HttpBinary)
        .build()
        .expect("build OTLP span exporter");
    let tracer_provider = TracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(span_exporter, runtime::Tokio)
        .build();
    let tracer = opentelemetry::trace::TracerProvider::tracer(&tracer_provider, "hebbian");
    global::set_tracer_provider(tracer_provider.clone());

    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/metrics", endpoint.trim_end_matches('/')))
        .with_protocol(Protocol::HttpBinary)
        .build()
        .expect("build OTLP metric exporter");
    let reader = PeriodicReader::builder(metric_exporter, runtime::Tokio).build();
    let meter_provider = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_reader(reader)
        .build();
    global::set_meter_provider(meter_provider.clone());

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .try_init();

    OtelGuard {
        tracer_provider: Some(tracer_provider),
        meter_provider: Some(meter_provider),
    }
}

/// 仅装 stderr 日志，不接 OTLP。在不需要 OTel 时使用。
pub fn init_logging_only(default_filter: &str) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}
