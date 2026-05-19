//! agent-core 的类型 facade。
//!
//! 协议本身在 [`protocol`] crate；这里的别名是为了让现有 caller（desktop chat.rs 等）
//! 不必一次性改全部 import 路径。新代码请直接 use `protocol::Event` / `EventPayload`。

pub use protocol::{
    AgentRef, ApprovalDecision, ContextPolicy as ProtocolContextPolicy, EditAction,
    ErrorReport, Event as AgentEvent, EventPayload as AgentEventPayload, MessageId, Op,
    PermissionKind, PermissionRequestId, PermissionScope, RiskLevel, RunId, StopReason,
    Submission, SubmissionId, TokenBudget, TurnId, TurnOverrides, UserInput,
};
