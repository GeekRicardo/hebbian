//! 运行模式（架构 §4.4.3）：决定派发器对工具调用的审批策略。
//!
//! - `AskBeforeEdits`：destructive 工具（Bash/PowerShell/Edit/Write）都要审批
//! - `EditAutomatically`：编辑类自动放行；命令类仍要审批
//! - `PlanMode`：工具列表过滤删除 Edit/Write/Bash/PowerShell，注入 ExitPlanMode（本期占位 TODO）
//! - `AutoMode`：调一次轻量 LLM judge 决定 Allow / Deny / Ask（仅 claude-opus-4-7 启用）

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunMode {
    AskBeforeEdits,
    EditAutomatically,
    PlanMode,
    AutoMode,
}

impl Default for RunMode {
    fn default() -> Self {
        RunMode::AskBeforeEdits
    }
}

impl RunMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunMode::AskBeforeEdits => "AskBeforeEdits",
            RunMode::EditAutomatically => "EditAutomatically",
            RunMode::PlanMode => "PlanMode",
            RunMode::AutoMode => "AutoMode",
        }
    }

    /// 从协议字符串解析（接受 kebab-case 与 PascalCase）。
    /// `Op::SwitchRunMode { new_mode: String }` 在 actor 路径上调用本函数。
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ask-before-edits" | "askbeforeedits" | "ask" => Some(RunMode::AskBeforeEdits),
            "edit-automatically" | "editautomatically" | "edit-auto" | "auto-edit" => {
                Some(RunMode::EditAutomatically)
            }
            "plan-mode" | "planmode" | "plan" => Some(RunMode::PlanMode),
            "auto-mode" | "automode" | "auto" => Some(RunMode::AutoMode),
            _ => None,
        }
    }
}
