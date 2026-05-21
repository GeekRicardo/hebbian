//! Span / Metric 属性键名常量。
//!
//! 优先沿用 OpenTelemetry GenAI semantic conventions（`gen_ai.*`），
//! Hebbian 自有的属性以 `hebbian.*` 命名空间隔离。

// ── GenAI（OpenTelemetry semantic conventions） ──────────────────────────────
pub const GEN_AI_SYSTEM: &str = "gen_ai.system";
pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
pub const GEN_AI_REQUEST_MAX_TOKENS: &str = "gen_ai.request.max_tokens";
pub const GEN_AI_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
pub const GEN_AI_USAGE_CACHE_READ_TOKENS: &str = "gen_ai.usage.cache_read_tokens";
pub const GEN_AI_USAGE_CACHE_CREATION_TOKENS: &str = "gen_ai.usage.cache_creation_tokens";
pub const GEN_AI_PROMPT: &str = "gen_ai.prompt";
pub const GEN_AI_COMPLETION: &str = "gen_ai.completion";

// ── Hebbian 业务属性 ─────────────────────────────────────────────────────────
pub const RUN_ID: &str = "hebbian.run.id";
pub const PARENT_RUN_ID: &str = "hebbian.run.parent_id";
pub const AGENT_ID: &str = "hebbian.agent.id";
pub const TURN_INDEX: &str = "hebbian.turn.index";
pub const TURN_ID: &str = "hebbian.turn.id";

pub const STREAMING: &str = "hebbian.model.streaming";
pub const STOP_REASON: &str = "hebbian.turn.stop_reason";

pub const TOOL_NAME: &str = "hebbian.tool.name";
pub const TOOL_CALL_ID: &str = "hebbian.tool.call_id";
pub const TOOL_CLASS: &str = "hebbian.tool.class";
pub const TOOL_OUTCOME: &str = "hebbian.tool.outcome";
pub const TOOL_TRUNCATED: &str = "hebbian.tool.truncated";
pub const TOOL_RESULT_SIZE: &str = "hebbian.tool.result_bytes";

pub const PERMISSION_KIND: &str = "hebbian.permission.kind";
pub const PERMISSION_DECISION: &str = "hebbian.permission.decision";
pub const PERMISSION_REQUEST_ID: &str = "hebbian.permission.request_id";

pub const COMPACTION_BEFORE_TOKENS: &str = "hebbian.compaction.before_tokens";
pub const COMPACTION_AFTER_TOKENS: &str = "hebbian.compaction.after_tokens";
pub const MICROCOMPACT_SHADOWED: &str = "hebbian.microcompact.shadowed";
pub const MICROCOMPACT_KEPT: &str = "hebbian.microcompact.kept";

// ── 取值常量（避免散落 magic string） ─────────────────────────────────────────
pub mod outcome {
    pub const OK: &str = "ok";
    pub const DENIED: &str = "denied";
    pub const FAILED: &str = "failed";
    pub const NOT_FOUND: &str = "not_found";
}

pub mod tool_class {
    pub const READ_ONLY: &str = "read_only";
    pub const DESTRUCTIVE: &str = "destructive";
    pub const NEEDS_HUMAN_INPUT: &str = "needs_human_input";
}

pub mod run_outcome {
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
}
