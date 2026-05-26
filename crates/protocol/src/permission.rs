use serde::{Deserialize, Serialize};

/// 用户对一次审批请求的回应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// 批准这一次
    AllowOnce,
    /// 批准并记住（同 scope 内的同类调用以后不再问）
    ///
    /// `pattern` 控制记忆粒度：
    /// - `None` → 工具名级（旧行为，对 Bash 类工具被 hitl 黑名单兜回 AllowOnce）
    /// - `Some("git status")` → 命令前缀级，匹配 `git status` / `git status -uno` 等
    /// - `Some("git")` → 根命令级，匹配所有以 `git ` 开头的命令
    ///
    /// 命中前缀的判定见 [`crate::permission`] 文档（按空白 token 边界匹配）。
    AllowAndRemember {
        scope: PermissionScope,
        #[serde(default)]
        pattern: Option<String>,
        /// 额外要一同记忆的命令前缀（compound 命令场景）。
        /// 例：`cd /tmp && touch foo` 弹审批，用户选「整条都允许」 → `pattern = "cd"`,
        /// `extra_patterns = ["touch"]`。后端循环写多条 PermissionRule。
        /// 空数组（默认）= 仅按 `pattern` 写一条规则（旧行为）。
        #[serde(default)]
        extra_patterns: Vec<String>,
    },
    /// 拒绝
    Deny,
    /// 拒绝并把反馈作为 user message 注入下一轮
    DenyWithFeedback { feedback: String },
}

/// 审批记忆生效的范围（架构 §4.5.3）。
///
/// 四选一：
/// - [`Once`](Self::Once)：本次放行，不持久化。
///   注意：本枚举主要用于 [`ApprovalDecision::AllowAndRemember`] 的 `scope` 字段；
///   单次放行通常应直接发 [`ApprovalDecision::AllowOnce`]，无需带 scope。
/// - [`Session`](Self::Session)：写到该 session 的 `session.jsonl`，重开仍生效。
///   只对当前对话生效。
/// - [`Project`](Self::Project)：写到 `~/.hebbian/permissions.json`，rule.workdir = 当前
///   session.workdir。同一 workdir（含子目录）下任何对话都生效，其他项目不受影响。
/// - [`Global`](Self::Global)：写到 `~/.hebbian/permissions.json`，rule.workdir = null，
///   任意对话生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionScope {
    Once,
    Session,
    Project,
    Global,
}

/// 审批请求的类别（用于 UI 渲染分类）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionKind {
    /// 工具调用审批
    ToolCall {
        tool_name: String,
        input: serde_json::Value,
        /// 命令级记忆指纹（来自 [`crate::permission`] 文档：BashTool 给 `"git status -uno"`）。
        /// UI 据此渲染"记住 `git status` / 记住 `git`"两档按钮；
        /// `None` 时退回工具名级"总是允许 Bash"按钮。
        /// **历史字段**：等于 `command_segments[0]`，保留只为向前兼容（架构 §4.4.2）。
        #[serde(default)]
        fingerprint: Option<String>,
        /// Bash / PowerShell 的所有段 fingerprint。compound 命令
        /// `cd /tmp && touch foo` → `["cd /tmp", "touch foo"]`。
        /// UI 据此展示「每段独立 allow / 一次性 allow 整条」按钮，避免段级
        /// 判定（架构 §4.4.2）需要"全段都允许"时用户却只允许了第一段的体感落差。
        /// 非 Bash 工具为空 vec。
        #[serde(default)]
        command_segments: Vec<String>,
    },
    /// workspace 越界路径访问审批（Bash/Read/Write/Grep）
    PathAccess {
        tool_name: String,
        paths: Vec<String>,
    },
    /// 计划审批（"按这个计划继续吗？"）。PlanMode 下 agent 调 ExitPlanMode 时
    /// 触发；surface 端用 plan_markdown 渲染完整预览，配合三按钮（通过 /
    /// 编辑后通过 / 重新规划带反馈）。
    ///
    /// - `plan_id` 与同 session 下 `plans/<plan_id>.md` 文件名对齐
    /// - `plan_path` 落盘绝对路径（surface 可直接 read_plan_markdown 重新拉取）
    /// - `plan_markdown` 当前内容快照（编辑后通过的话由 surface 走
    ///   `update_plan_markdown` 命令 patch 文件再发 AllowOnce）
    /// - `summary` 短句摘要，UI 列表 / 通知用
    /// - `steps` 历史字段，留作向前兼容；新版本不再使用
    Plan {
        #[serde(default)]
        plan_id: String,
        #[serde(default)]
        plan_path: String,
        #[serde(default)]
        plan_markdown: String,
        #[serde(default)]
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        steps: Vec<String>,
    },
    /// 长 run 继续审批
    ContinueLongRun { iterations_used: u32 },
}

// ── Ask：agent 主动向用户提问 ────────────────────────────────────────────────

/// 一个候选选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    /// 短标签（按钮文字 / 选项行首），建议 1-12 字
    pub label: String,
    /// 详细说明（可空），用于 hover / 子行展示
    #[serde(default)]
    pub description: String,
}

/// 用户对一次 ask 的回应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserAnswer {
    /// 单选：选了某个固定选项（带回 label）
    Selected { label: String },
    /// 多选：选了若干固定选项（带回 labels，按用户勾选顺序）
    SelectedMulti { labels: Vec<String> },
    /// 用户在自由输入框写的文字
    Custom { text: String },
    /// 用户取消（TUI 中按 ESC、UI 关闭弹窗等）
    Cancelled,
}

impl UserAnswer {
    /// 把答案规约成将要注入下一轮的 tool_result 文本
    pub fn to_agent_text(&self) -> String {
        match self {
            UserAnswer::Selected { label } => format!("用户选择：{label}"),
            UserAnswer::SelectedMulti { labels } => {
                if labels.is_empty() {
                    "[用户未选任何选项]".to_string()
                } else {
                    format!("用户选择（多选）：{}", labels.join("、"))
                }
            }
            UserAnswer::Custom { text } => format!("用户输入：{text}"),
            UserAnswer::Cancelled => "[用户取消了提问]".to_string(),
        }
    }
}
