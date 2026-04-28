use serde::{Deserialize, Serialize};

use crate::context::TurnOverrides;
use crate::ids::{AgentRef, PermissionRequestId, RunId, SubmissionId};
use crate::permission::ApprovalDecision;

/// 外界向 Core 发出的所有意图都是一个 Submission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub id: SubmissionId,
    pub op: Op,
}

impl Submission {
    pub fn new(op: Op) -> Self {
        Self {
            id: SubmissionId::new(),
            op,
        }
    }
}

/// 用户输入（文本 + 可选附件元数据，附件本体走 BlobStore）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInput {
    pub text: String,
    /// 附件 blob_id 列表（具体内容由 surface 上传到 BlobStore 后引用）
    #[serde(default)]
    pub attachment_refs: Vec<String>,
}

impl UserInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachment_refs: Vec::new(),
        }
    }
}

/// 所有可对 Core 发起的操作
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    /// 启动一次新的 run
    StartRun {
        agent: AgentRef,
        input: UserInput,
        #[serde(default)]
        turn_overrides: Option<TurnOverrides>,
        /// 子 run 时填，为父 run 的 RunId
        #[serde(default)]
        parent: Option<RunId>,
    },

    /// 在已有 run 上追加一条用户消息（继续多轮对话）
    SendUserMessage { run_id: RunId, input: UserInput },

    /// 回应一次审批请求
    Approve {
        request_id: PermissionRequestId,
        decision: ApprovalDecision,
    },

    /// 回应一次 agent 主动提问
    AnswerQuestion {
        request_id: PermissionRequestId,
        answer: crate::permission::UserAnswer,
    },

    /// 中断 run（含级联取消子 run）
    Interrupt { run_id: RunId },

    /// 订阅一个 run 的事件流（用于断线重连或多端观察）
    /// 实际订阅在 Harness::subscribe 中处理；此 Op 主要用于跨 surface 协议
    Subscribe {
        run_id: RunId,
        #[serde(default)]
        since_seq: Option<u64>,
    },

    /// 显式压缩
    Compact { run_id: RunId },

    /// 回滚到指定 turn
    Rollback { run_id: RunId, to_turn: u32 },

    /// 从某个 run 在某 turn 处分叉
    Fork {
        from: RunId,
        #[serde(default)]
        at_turn: Option<u32>,
        #[serde(default)]
        agent: Option<AgentRef>,
    },
}
