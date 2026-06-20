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

/// 复合命令里单段相对白名单的状态（审批弹窗据此决定该段怎么展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSegmentStatus {
    /// 只读段：免审批、免记忆（灰显）。
    Readonly,
    /// 已命中某条 allow 规则：本次无需再处理（✓ 跳过）。
    Whitelisted,
    /// 不可记忆命令（rm/dd/…）：红色、不可勾选、每次必须确认（架构 §4.4.2.3）。
    Unmemorable,
    /// 会写且尚未进白名单：本次要决定是否加入（可勾选）。
    NeedsApproval,
}

/// 一段命令 + 它的白名单状态。审批弹窗逐段渲染，让用户看清「哪些已放行、哪些待批、
/// 哪些（rm）永远要确认」（架构 §4.4.2 / §4.4.2.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalSegment {
    pub fingerprint: String,
    pub status: ApprovalSegmentStatus,
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
        /// Bash / PowerShell 里「会写 + 可记忆 + 尚未进白名单」的段 fingerprint——
        /// 即弹窗里可勾选记忆的那些。非 Bash 工具为空 vec（架构 §4.4.2）。
        #[serde(default)]
        command_segments: Vec<String>,
        /// 完整段级状态（含只读 / 已白名单 / 不可记忆 / 待审批），供弹窗逐段展示：
        /// 已白名单段标 ✓ 跳过、rm 段红色禁选。空 = 非 Bash 或老事件（架构 §4.4.2.3）。
        #[serde(default)]
        segments: Vec<ApprovalSegment>,
        /// 整条命令出于安全原因**任何作用域都不可记住**（危险复合模式，如 `cd X && git …`，
        /// 架构 §4.4.2.2）。为 true 时弹窗应隐藏作用域/记忆区，只留「允许一次 / 拒绝」。
        /// 注意：仅含 rm 这类不可记忆段**不**置 true——那只让 rm 段不可记，良性段仍可记。
        #[serde(default)]
        refuse_remember: bool,
    },
    /// workspace 越界路径访问审批（Bash/Read/Write/Grep）
    PathAccess {
        tool_name: String,
        paths: Vec<String>,
    },
    /// 计划审批（"按这个计划继续吗？"）。PlanMode 下 agent 调 PlanMode(action=submit)
    /// 时触发；surface 端用 plan_markdown 渲染完整预览，配合三按钮（通过 /
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

/// 一道子题（多题 ask 用）。
///
/// 老的单题 ask 仍走 `EventPayload::UserQuestionRequested` 顶层
/// `question / options / multi`，本类型只在新的 `questions` 字段非空时使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskQuestion {
    /// 题目标题（必填）
    pub title: String,
    /// 题目说明，给用户更多上下文（可空）
    #[serde(default)]
    pub description: String,
    /// 候选选项
    pub options: Vec<QuestionOption>,
    /// 是否多选；缺省 false（单选）
    #[serde(default)]
    pub multi: bool,
}

/// 多题答案的一项：题目标题 + 子答案。
///
/// 落到 tool_result 时按 `title: <子答案文本>` 行序拼回，让模型知道每个答案
/// 对应的是哪道子题（避免 surface 端要把题目重新塞回 ToolResult 文本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiQuestionAnswer {
    pub title: String,
    pub answer: SingleAnswer,
}

/// 单题答案：四种 wire 形态与老 `UserAnswer` 完全对齐。
///
/// 抽出来给 `UserAnswer::Multi.items` 用——多题答案里每一项都是一道子题的
/// 单题答案，但**不允许再嵌套 Multi**（用类型把这一约束钉死）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SingleAnswer {
    Selected { label: String },
    SelectedMulti { labels: Vec<String> },
    Custom { text: String },
    Cancelled,
}

impl SingleAnswer {
    pub fn to_agent_text(&self) -> String {
        match self {
            SingleAnswer::Selected { label } => format!("选择：{label}"),
            SingleAnswer::SelectedMulti { labels } => {
                if labels.is_empty() {
                    "[未选]".to_string()
                } else {
                    format!("多选：{}", labels.join("、"))
                }
            }
            SingleAnswer::Custom { text } => format!("输入：{text}"),
            SingleAnswer::Cancelled => "[已取消]".to_string(),
        }
    }
}

/// 用户对一次 ask 的回应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserAnswer {
    /// 单题单选：选了某个固定选项（带回 label）
    Selected { label: String },
    /// 单题多选：选了若干固定选项（带回 labels，按用户勾选顺序）
    SelectedMulti { labels: Vec<String> },
    /// 单题自由输入：用户在自由输入框写的文字
    Custom { text: String },
    /// 用户取消整轮提问（TUI 中按 ESC、UI 关闭弹窗等）
    Cancelled,
    /// 多题答案：每道子题一个 [`MultiQuestionAnswer`]。
    /// 整轮取消请直接用 [`Cancelled`] 而不是给每道题都塞 `Cancelled`。
    Multi { items: Vec<MultiQuestionAnswer> },
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
            UserAnswer::Multi { items } => {
                if items.is_empty() {
                    "[用户未作答]".to_string()
                } else {
                    let lines: Vec<String> = items
                        .iter()
                        .map(|item| format!("- {}: {}", item.title, item.answer.to_agent_text()))
                        .collect();
                    format!("用户回答（多题）：\n{}", lines.join("\n"))
                }
            }
        }
    }
}
