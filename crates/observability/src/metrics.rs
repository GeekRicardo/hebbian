//! 全局 Histogram / Counter 句柄。lazy 初始化，OTel 未启用时记录到 noop meter，无副作用。
//!
//! 用法：
//! ```ignore
//! use observability::metrics;
//! use opentelemetry::KeyValue;
//!
//! metrics::tool_duration().record(
//!     duration_ms as f64,
//!     &[KeyValue::new("tool", "Bash"), KeyValue::new("outcome", "ok")],
//! );
//! ```

use once_cell::sync::Lazy;
use opentelemetry::{
    global,
    metrics::{Counter, Histogram, Meter},
};

fn meter() -> Meter {
    global::meter("hebbian")
}

// 时延直方图（毫秒）。
pub static RUN_DURATION: Lazy<Histogram<f64>> = Lazy::new(|| {
    meter()
        .f64_histogram("hebbian.run.duration_ms")
        .with_description("一次 run 总耗时（毫秒）")
        .build()
});

pub static TURN_DURATION: Lazy<Histogram<f64>> = Lazy::new(|| {
    meter()
        .f64_histogram("hebbian.turn.duration_ms")
        .with_description("单轮 turn 耗时（毫秒）")
        .build()
});

pub static MODEL_DURATION: Lazy<Histogram<f64>> = Lazy::new(|| {
    meter()
        .f64_histogram("hebbian.model.duration_ms")
        .with_description("一次模型调用的耗时（毫秒）")
        .build()
});

pub static TOOL_DURATION: Lazy<Histogram<f64>> = Lazy::new(|| {
    meter()
        .f64_histogram("hebbian.tool.duration_ms")
        .with_description("一次工具执行的耗时（毫秒）")
        .build()
});

pub static PERMISSION_WAIT: Lazy<Histogram<f64>> = Lazy::new(|| {
    meter()
        .f64_histogram("hebbian.permission.wait_ms")
        .with_description("HITL 审批 / 提问的等待耗时（毫秒）")
        .build()
});

// 计数器。
pub static TOOL_CALLS: Lazy<Counter<u64>> = Lazy::new(|| {
    meter()
        .u64_counter("hebbian.tool.calls")
        .with_description("工具调用次数（按 tool/outcome 分组）")
        .build()
});

pub static RUN_OUTCOMES: Lazy<Counter<u64>> = Lazy::new(|| {
    meter()
        .u64_counter("hebbian.run.outcomes")
        .with_description("Run 结束计数（按 outcome 分组）")
        .build()
});

pub static TOKEN_USAGE: Lazy<Counter<u64>> = Lazy::new(|| {
    meter()
        .u64_counter("gen_ai.client.token.usage")
        .with_description("Token 用量计数（按 token_type/model 分组）")
        .build()
});

// 工具方法：常用记录入口。
pub fn record_tool_duration(tool: &str, outcome: &str, duration_ms: f64) {
    use opentelemetry::KeyValue;
    TOOL_DURATION.record(
        duration_ms,
        &[
            KeyValue::new("tool", tool.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
    TOOL_CALLS.add(
        1,
        &[
            KeyValue::new("tool", tool.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
}

pub fn record_permission_wait(kind: &str, decision: &str, wait_ms: f64) {
    use opentelemetry::KeyValue;
    PERMISSION_WAIT.record(
        wait_ms,
        &[
            KeyValue::new("kind", kind.to_string()),
            KeyValue::new("decision", decision.to_string()),
        ],
    );
}

pub fn record_model_call(provider: &str, model: &str, streaming: bool, duration_ms: f64) {
    use opentelemetry::KeyValue;
    MODEL_DURATION.record(
        duration_ms,
        &[
            KeyValue::new("provider", provider.to_string()),
            KeyValue::new("model", model.to_string()),
            KeyValue::new("streaming", streaming),
        ],
    );
}

pub fn record_token_usage(provider: &str, model: &str, kind: &str, tokens: u64) {
    use opentelemetry::KeyValue;
    if tokens == 0 {
        return;
    }
    TOKEN_USAGE.add(
        tokens,
        &[
            KeyValue::new("gen_ai.system", provider.to_string()),
            KeyValue::new("gen_ai.request.model", model.to_string()),
            KeyValue::new("gen_ai.token.type", kind.to_string()),
        ],
    );
}

pub fn record_run_outcome(outcome: &str, agent: &str, duration_ms: f64) {
    use opentelemetry::KeyValue;
    RUN_DURATION.record(
        duration_ms,
        &[
            KeyValue::new("outcome", outcome.to_string()),
            KeyValue::new("agent", agent.to_string()),
        ],
    );
    RUN_OUTCOMES.add(
        1,
        &[
            KeyValue::new("outcome", outcome.to_string()),
            KeyValue::new("agent", agent.to_string()),
        ],
    );
}

pub fn record_turn_duration(turn_index: u32, stop_reason: &str, duration_ms: f64) {
    use opentelemetry::KeyValue;
    TURN_DURATION.record(
        duration_ms,
        &[
            KeyValue::new("turn", turn_index as i64),
            KeyValue::new("stop_reason", stop_reason.to_string()),
        ],
    );
}
