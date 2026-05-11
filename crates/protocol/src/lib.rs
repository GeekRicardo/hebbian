//! Hebbian 协议 crate
//!
//! 这里只放数据类型，不放行为。所有 surface、channel、core 内部模块都基于
//! 本 crate 通信。任何向外暴露的协议变更都应该先改这里。

pub mod context;
pub mod error;
pub mod event;
pub mod ids;
pub mod permission;
pub mod submission;

pub use context::{ContextPolicy, TokenBudget, TurnOverrides};
pub use error::ErrorReport;
pub use event::{Event, EventPayload, RiskLevel, StepKind, StopReason};
pub use ids::{AgentRef, MessageId, PermissionRequestId, RunId, SubmissionId, TurnId};
pub use permission::{
    ApprovalDecision, PermissionKind, PermissionScope, QuestionOption, UserAnswer,
};
pub use submission::{Op, Submission, UserInput};
