//! UI 运行态：把 core 推来的 `CoreUpdate` 收敛成视图直接消费的数据。
//!
//! 与原 Web 前端 `useStore` 的分工一致——单一事实源在这里，视图只读不算。
//! 流式增量（TextDelta / Reasoning / 工具进度）落在 `streaming`，run 结束后由
//! 落盘消息接管，避免「实时拼的文本」与「jsonl 里的真消息」两份各自演化。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agent_core::storage::projects::WorkspaceProject;
use agent_core::storage::sessions::{Message, MessageMeta, Role, Session, SessionMeta};
use agent_core::wakeup::PendingCron;
use protocol::{QuestionOption, WireEvent};
use serde_json::Value;

use crate::core::{Core, CoreUpdate, DirEntry};

/// 一条待处理的工具审批。
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub summary: String,
    /// 可记忆的候选 pattern 及其状态。**由 core 算好随事件发来**，
    /// UI 不自己解析命令——段级判定的规则在 core，前端再推一遍必然走样。
    pub segments: Vec<protocol::ApprovalSegment>,
    /// core 判定这条命令不允许记忆（含危险复合模式，架构 §4.4.2.3）。
    /// 为真时整个「记住」区不出现——不是灰掉，是根本不给。
    pub refuse_remember: bool,
}

/// 一条待回答的提问。
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub request_id: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi: bool,
}

/// 正在流式产出的助手回复。
#[derive(Debug, Default, Clone)]
pub struct StreamingTurn {
    pub text: String,
    pub reasoning: String,
    /// 本轮已经开始的工具调用，按时序。
    pub tools: Vec<StreamingTool>,
}

#[derive(Debug, Clone)]
pub struct StreamingTool {
    pub id: String,
    pub name: String,
    pub done: bool,
    pub is_error: bool,
}

impl StreamingTurn {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.reasoning.is_empty() && self.tools.is_empty()
    }
}

/// 左侧栏的两个标签页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Code,
    Chat,
}

pub struct AppState {
    pub core: Core,

    pub sessions: Vec<SessionMeta>,
    pub projects: Vec<WorkspaceProject>,
    pub providers: Vec<model_gateway::config::Provider>,
    pub settings: agent_core::storage::settings::Settings,

    pub current: Option<Session>,
    pub messages: Vec<Message>,
    pub streaming: StreamingTurn,

    /// 正在跑 run 的会话 id。侧栏状态点据此呼吸。
    pub running: HashSet<String>,
    /// 后台跑完但用户还没看过的会话 id。
    pub unread: HashSet<String>,
    /// 会话 id → 待处理的审批 / 提问（侧栏黄色呼吸边框据此点亮）。
    pub pending_approvals: HashMap<String, PendingApproval>,
    pub pending_questions: HashMap<String, PendingQuestion>,

    // ── 侧栏 UI 态 ──────────────────────────────────────────────
    pub tab: SidebarTab,
    pub query: String,
    pub search_case: bool,
    pub search_regex: bool,
    pub collapsed: HashSet<String>,

    /// 当前展开着的是哪张后台任务卡片（按 tool_call_id 记——定时唤醒没有 task_id）。
    pub expanded_task: Option<String>,
    /// 轮询回来的输出（任务编号 + 正文）。**和上面那个分开存**：
    /// 输出是按任务编号回来的，混在一起会让每轮刷新都把展开态覆盖成任务编号，
    /// 卡片一展开就立刻自己收起来。
    pub task_output: Option<(String, String)>,
    /// 还在等的定时唤醒。倒计时要用它的精确触发时刻。
    pub pending_crons: Vec<agent_core::wakeup::PendingCron>,
    /// 请求把聊天区滚到这次工具调用上并高亮一下。渲染时消费掉即清空。
    pub focus_tool_call: Option<String>,
    /// 被「后台任务」面板点名展开的那些工具调用（按调用 id 记）。
    /// 不能复用 `expanded_parts`：那个按「消息 id + 序号」拼 key，而同一条消息
    /// 在流式和落盘两条渲染路径下序号规则不同，跨路径拼不出同一个 key。
    pub expanded_calls: HashSet<String>,
    /// 正在闪烁高亮的那次工具调用。闪一下比直接滚过去更容易被眼睛抓到——
    /// 长对话里滚动之后，用户往往不知道该看哪一行。
    pub flash_tool_call: Option<String>,

    /// 正在预览的那条 Claude 对话（点列表里某一条后才有）。
    pub claude_preview: Option<std::rc::Rc<crate::core::ClaudePreview>>,

    /// 用户 Claude 目录下可以导入的对话（导入弹窗打开时才拉）。
    pub claude_importable: Vec<crate::core::ClaudeImportable>,
    /// 刚导出成功的那条 `claude --resume` 命令，等用户复制走。
    pub claude_exported: Option<String>,

    /// 待用户确认的破坏性操作。删除对话 / 删除项目都不可撤销，
    /// 所以照原 UI 一样问两遍——第一遍防误点，第二遍防手快。
    pub confirm: Option<Confirm>,

    /// 这个会话的 plan（新到旧）。
    pub plans: Vec<(String, String)>,

    /// 当前正在看的改动：文件相对路径 + 逐行 diff。
    pub diff: Option<(String, Vec<crate::diff::DiffLine>)>,

    /// 设置里几页共用的只读清单。
    pub extras: crate::core::Extras,

    /// 最近一份调度日志（名字 + 尾部若干行）。
    pub log_tail: (String, Vec<String>),

    /// 全局权限规则（设置里的「权限」页）。
    pub perm_allow: Vec<String>,
    pub perm_deny: Vec<String>,

    /// 可用 skill（`//` 命令面板）。
    pub skills: Vec<agent_core::tools::skill::Skill>,

    /// 工作目录的 git 状态。`None` = 还没读或不是仓库。
    pub git: Option<agent_core::git_scm::GitProjectStatus>,

    /// 每个打开文件「上次读盘/保存时」的内容，用来判断有没有未保存改动。
    pub file_baselines: HashMap<PathBuf, String>,

    /// 编辑区打开的文件列表（多标签），以及当前活动的那个。
    pub open_files: Vec<PathBuf>,
    pub active_file: Option<PathBuf>,

    /// 当前会话的运行模式。切会话时从 session 读，切模式时即时更新。
    pub run_mode: agent_core::run_mode::RunMode,

    /// 上下文占用（已用 / 预算 tokens）与缓存命中率。没算出来之前是 None——
    /// 宁可不显示，也不显示编出来的数字。
    pub context_usage: Option<(usize, usize, u32)>,

    /// 注册表里还活着的后台任务。每次渲染后台面板时现读，不缓存。
    pub live_tasks: Vec<LiveTask>,

    /// 这个会话改过的文件（按 run 分组，新的在前）。
    pub edits: Vec<agent_core::edits::metadata::RunEditEntry>,
    /// 从这个会话分叉出去的旁支。
    pub branches: Vec<(String, String)>,

    /// 当前会话的 todo 列表。由 `TodoListUpdated` 事件驱动，不落单独的盘。
    pub todos: Vec<protocol::WireTodoItem>,

    /// 展开着的工具卡片 / 思考块（按 message id + 序号定位）。
    pub expanded_parts: HashSet<String>,

    // ── 文件树 ──────────────────────────────────────────────────
    /// 已读过的目录：路径 → 这一层的条目。
    pub dirs: HashMap<PathBuf, Vec<DirEntry>>,
    /// 展开着的目录。
    pub expanded_dirs: HashSet<PathBuf>,

    /// 审批「记住」区已勾选的 pattern（默认勾选所有待决定段）。
    pub approval_picked: Vec<String>,

    /// 「编辑后重跑」待填进输入框的原文。
    pub edit_draft: Option<String>,

    /// 刚保存了哪个文件（编辑区状态条上一闪而过的提示）。
    pub saved_notice: Option<String>,

    /// 最近一次失败信息，渲染成顶部 toast。
    pub error: Option<String>,
}

impl AppState {
    pub fn new(core: Core) -> Self {
        Self {
            core,
            sessions: Vec::new(),
            projects: Vec::new(),
            providers: Vec::new(),
            settings: agent_core::storage::settings::Settings::default(),
            current: None,
            messages: Vec::new(),
            streaming: StreamingTurn::default(),
            running: HashSet::new(),
            unread: HashSet::new(),
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            tab: SidebarTab::Code,
            query: String::new(),
            search_case: false,
            search_regex: false,
            collapsed: HashSet::new(),
            expanded_task: None,
            task_output: None,
            pending_crons: Vec::new(),
            focus_tool_call: None,
            flash_tool_call: None,
            expanded_calls: HashSet::new(),
            claude_importable: Vec::new(),
            claude_preview: None,
            claude_exported: None,
            confirm: None,
            plans: Vec::new(),
            extras: crate::core::Extras::default(),
            log_tail: (String::new(), Vec::new()),
            perm_allow: Vec::new(),
            perm_deny: Vec::new(),
            diff: None,
            skills: Vec::new(),
            git: None,
            file_baselines: HashMap::new(),
            open_files: Vec::new(),
            active_file: None,
            context_usage: None,
            live_tasks: Vec::new(),
            run_mode: agent_core::run_mode::RunMode::Default,
            edits: Vec::new(),
            branches: Vec::new(),
            todos: Vec::new(),
            expanded_parts: HashSet::new(),
            dirs: HashMap::new(),
            expanded_dirs: HashSet::new(),
            approval_picked: Vec::new(),
            edit_draft: None,
            saved_notice: None,
            error: None,
        }
    }

    pub fn current_id(&self) -> Option<&str> {
        self.current.as_ref().map(|s| s.id.as_str())
    }

    pub fn is_running(&self) -> bool {
        self.current_id()
            .is_some_and(|id| self.running.contains(id))
    }

    /// 消费一条 core 更新。返回 true 表示需要重绘。
    pub fn apply(&mut self, update: CoreUpdate) -> bool {
        match update {
            CoreUpdate::Catalog { sessions, projects } => {
                self.sessions = sessions;
                self.projects = projects;
            }
            CoreUpdate::SessionLoaded(session) => {
                let session = *session;
                // 切会话即读过：清掉未读高亮。
                self.unread.remove(&session.id);
                self.messages = session
                    .messages
                    .iter()
                    .filter(|m| !matches!(m.role, Role::System))
                    .cloned()
                    .collect();
                self.streaming = StreamingTurn::default();
                self.run_mode = session.run_mode;
                self.todos.clear();
                self.plans.clear();
                self.diff = None;
                self.core
                    .refresh_plans(session.id.clone(), session.workdir.clone());
                self.edits.clear();
                self.branches.clear();
                // 后台任务的输出是按会话取的，切走了还留着就会把上一个会话的
                // 输出挂在新会话的面板上；同时把轮询也停掉。
                self.expanded_task = None;
                self.task_output = None;
                self.core.unwatch_task_output();
                self.core
                    .refresh_edits(session.id.clone(), session.workdir.clone());
                self.core.refresh_branches(session.id.clone());
                self.context_usage = None;
                self.core.refresh_context_usage(session.id.clone());
                if let Some(workdir) = session.workdir.clone() {
                    self.expanded_dirs.insert(workdir.clone());
                    self.core.list_dir(workdir.clone());
                    self.core.refresh_git(workdir.clone());
                    self.core.refresh_skills(workdir);
                }
                self.current = Some(session);
            }
            CoreUpdate::SessionCreated(id) => {
                self.core.open_session(id);
            }
            CoreUpdate::TaskOutput { task_id, text } => {
                self.task_output = Some((task_id, text));
            }
            CoreUpdate::ClaudeImportable(list) => {
                self.claude_importable = list;
            }
            CoreUpdate::ClaudePreview(preview) => {
                // 换一条预览就清掉工具卡片的展开态：key 里带消息下标，
                // 留着会让新对话里莫名其妙有几张卡是开的。
                self.expanded_parts.retain(|k| !k.starts_with("preview-"));
                self.claude_preview = Some(std::rc::Rc::from(preview));
            }
            CoreUpdate::ClaudeExported { resume_command } => {
                self.claude_exported = Some(resume_command);
            }
            CoreUpdate::Edits(edits) => {
                self.edits = edits;
            }
            CoreUpdate::Branches(branches) => {
                self.branches = branches;
            }
            CoreUpdate::Extras(extras) => {
                self.extras = *extras;
            }
            CoreUpdate::LogTail { name, lines } => {
                self.log_tail = (name, lines);
            }
            CoreUpdate::Permissions { allow, deny } => {
                self.perm_allow = allow;
                self.perm_deny = deny;
            }
            CoreUpdate::ContextUsage {
                used_tokens,
                budget_tokens,
                cache_hit_pct,
            } => {
                self.context_usage = Some((used_tokens, budget_tokens, cache_hit_pct));
            }
            CoreUpdate::LiveTasks {
                tasks,
                pending_crons,
            } => {
                self.pending_crons = pending_crons;
                self.live_tasks = tasks;
            }
            CoreUpdate::RunModeChanged(mode) => {
                self.run_mode = mode;
            }
            CoreUpdate::Settings(settings) => {
                self.settings = *settings;
            }
            CoreUpdate::Providers(providers) => {
                self.providers = providers;
            }
            CoreUpdate::Plans(plans) => {
                self.plans = plans;
            }
            CoreUpdate::DiffLoaded {
                rel_path,
                before,
                after,
            } => {
                self.diff = Some((rel_path, crate::diff::line_diff(&before, &after)));
            }
            CoreUpdate::Skills(skills) => {
                self.skills = skills;
            }
            CoreUpdate::GitStatus(status) => {
                self.git = status.map(|s| *s);
            }
            CoreUpdate::EditDraft(text) => {
                self.edit_draft = Some(text);
            }
            CoreUpdate::FileSaved { path, text } => {
                // 保存成功后基线跟着走，圆点随之消失。
                self.file_baselines.insert(path.clone(), text);
                self.saved_notice = path
                    .file_name()
                    .map(|n| format!("{} 已保存", n.to_string_lossy()));
            }
            CoreUpdate::FileLoaded { path, text } => {
                self.file_baselines.insert(path.clone(), text);
                // 已经开过就只切过去，不重复加标签。
                if !self.open_files.contains(&path) {
                    self.open_files.push(path.clone());
                }
                self.active_file = Some(path);
            }
            CoreUpdate::DirListed { path, entries } => {
                self.dirs.insert(path, entries);
            }
            CoreUpdate::Failed(message) => {
                self.error = Some(message);
            }
            CoreUpdate::Wire { session_id, event } => {
                return self.apply_wire(&session_id, event);
            }
        }
        true
    }

    fn apply_wire(&mut self, session_id: &str, event: WireEvent) -> bool {
        let foreground = self.current_id() == Some(session_id);
        match event {
            WireEvent::RunStarted { .. } => {
                self.running.insert(session_id.to_string());
                if foreground {
                    self.streaming = StreamingTurn::default();
                }
            }
            WireEvent::TextDelta { text, .. } => {
                if !foreground {
                    return false;
                }
                self.streaming.text.push_str(&text);
            }
            WireEvent::Reasoning { text, .. } => {
                if !foreground {
                    return false;
                }
                self.streaming.reasoning.push_str(&text);
            }
            WireEvent::ToolStart { id, name, .. } => {
                if !foreground {
                    return false;
                }
                self.streaming.tools.push(StreamingTool {
                    id,
                    name,
                    done: false,
                    is_error: false,
                });
            }
            WireEvent::ToolDone { id, is_error, .. } => {
                if !foreground {
                    return false;
                }
                if let Some(tool) = self.streaming.tools.iter_mut().find(|t| t.id == id) {
                    tool.done = true;
                    tool.is_error = is_error;
                }
            }
            WireEvent::PermissionRequested {
                request_id,
                tool_name,
                summary,
                segments,
                command_segments,
                refuse_remember,
                ..
            } => {
                // 新事件带 `segments`（含状态）；老事件只有 `command_segments`
                // 字符串数组，退化成「都待决定」。
                let segments = if segments.is_empty() {
                    command_segments
                        .into_iter()
                        .map(|fingerprint| protocol::ApprovalSegment {
                            fingerprint,
                            status: protocol::ApprovalSegmentStatus::NeedsApproval,
                        })
                        .collect()
                } else {
                    segments
                };
                self.approval_picked = segments
                    .iter()
                    .filter(|s| {
                        matches!(s.status, protocol::ApprovalSegmentStatus::NeedsApproval)
                    })
                    .map(|s| s.fingerprint.clone())
                    .collect();
                self.pending_approvals.insert(
                    session_id.to_string(),
                    PendingApproval {
                        request_id,
                        tool_name,
                        summary,
                        segments,
                        refuse_remember,
                    },
                );
            }
            WireEvent::UserQuestionRequested {
                request_id,
                question,
                options,
                multi,
                ..
            } => {
                self.pending_questions.insert(
                    session_id.to_string(),
                    PendingQuestion {
                        request_id,
                        question,
                        options,
                        multi,
                    },
                );
            }
            WireEvent::TodoListUpdated { todos, .. } => {
                if !foreground {
                    return false;
                }
                self.todos = todos;
            }
            WireEvent::RunFinished { .. } => {
                self.running.remove(session_id);
                self.pending_approvals.remove(session_id);
                self.pending_questions.remove(session_id);
                if foreground {
                    // 落盘的消息才是唯一真源：重新读一遍 transcript，让流式拼的文本退场。
                    self.core.open_session(session_id.to_string());
                    self.core.refresh_context_usage(session_id.to_string());
                } else {
                    self.unread.insert(session_id.to_string());
                }
                self.core.refresh_catalog();
            }
            WireEvent::Error { message } => {
                self.running.remove(session_id);
                self.error = Some(message);
            }
            _ => return false,
        }
        true
    }

    /// 用户点了「允许 / 拒绝」后本地先摘掉待办，再把决定投回 core。
    pub fn take_approval(&mut self, session_id: &str) -> Option<PendingApproval> {
        self.pending_approvals.remove(session_id)
    }

    pub fn take_question(&mut self, session_id: &str) -> Option<PendingQuestion> {
        self.pending_questions.remove(session_id)
    }
}

/// 注册表里还活着的后台任务（运行中的实时状态）。
/// transcript 只知道「启动成功了没」，真实状态与已跑秒数只有注册表知道。
#[derive(Debug, Clone)]
pub struct LiveTask {
    pub task_id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    /// 已经跑了多少秒。运行中的卡片显示它，也用来给「运行中」排序。
    pub elapsed_secs: u64,
}

/// 后台任务卡片的一条。三种来源合成同一种形状，面板才好按统一模板画。
#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub kind: BackgroundKind,
    /// 注册表 task_id。cron 恒为 None；Bash 在结果还没回来时也可能是 None。
    pub task_id: Option<String>,
    /// 这条卡片对应哪次工具调用——点卡片要跳到聊天区那张工具卡。
    pub tool_call_id: String,
    /// Bash 是命令原文，cron 是唤醒原因，subagent 是子 agent 类型。
    pub command: String,
    pub running: bool,
    pub is_error: bool,
    /// 工具结果原文。任务已经结束时，输出直接看它，不用再问注册表。
    pub result: Option<String>,
    pub duration_ms: Option<u64>,
    /// 注册表报上来的已运行秒数（只有活着的 Bash 有）。
    pub elapsed_secs: Option<u64>,
    pub cron: Option<CronInfo>,
}

#[derive(Debug, Clone)]
pub struct CronInfo {
    pub reason: String,
    pub fire_at_ms: i64,
    /// 还在等 = 时间没到；已唤醒 = scheduler 里查不到了。
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    Bash,
    Cron,
    Subagent,
}

/// 从 Bash 工具结果里抠出后台任务编号。
///
/// 两种文案都要认：现在的 `[bash_001] 已在后台启动` / `[bash_001] 60s 内未结束，已转后台`，
/// 以及旧版的 `task_id=bash_001 cmd=…`。**必须限定成 `bash_` 加数字**——
/// 早先我图省事只找一对方括号，那样任何以 `[xxx]` 开头的结果都会被认成任务编号，
/// 拿这种假编号去和注册表比对，会把一条毫不相干的活任务顶掉。
pub fn extract_bg_task_id(result: &str) -> Option<String> {
    extract_after(result, "task_id=", "bash_")
        .or_else(|| extract_after(result, "[", "bash_"))
}

/// 子 agent 的编号只有 `task_id=subagent-xxx` 一种写法。
pub fn extract_subagent_task_id(result: &str) -> Option<String> {
    let rest = result.split("task_id=").nth(1)?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    id.starts_with("subagent-").then_some(id)
}

/// 找 `marker` 之后紧跟着的、以 `prefix` 开头的编号。
fn extract_after(text: &str, marker: &str, prefix: &str) -> Option<String> {
    for chunk in text.split(marker).skip(1) {
        if !chunk.starts_with(prefix) {
            continue;
        }
        let id: String = chunk
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        // 前缀后面必须真的是数字，`bash_` 光秃秃一个不算。
        if id.len() > prefix.len() && id[prefix.len()..].chars().all(|c| c.is_ascii_digit()) {
            return Some(id);
        }
    }
    None
}

/// 把 transcript 里的工具调用和注册表里活着的任务合成一张卡片列表。
///
/// 三条来源规则，与原前端逐条对齐：
/// - **Bash**：显式 `run_in_background` 的，**以及**前台跑超时被转到后台的
///   （那种入参里没有 run_in_background，只能靠结果里有没有任务编号认出来）。
///   纯前台跑完的不算——它由聊天区的工具卡片自己管，列到这里只会让人分不清。
/// - **ScheduleWakeup**：按原因去 scheduler 的待唤醒表里查；查得到说明还在等，
///   用它的精确触发时刻；查不到说明已经唤醒过了，用「调用时刻 + 延时」倒推。
/// - **Task**：结果里解析出子 agent 编号才算数；完成与否看有没有收到对应的完成通知。
///
/// 最后把注册表里有、transcript 里还没记上的补进来（刚启动 / 结果还没回来 /
/// 上次进程留下的），并让运行中的排在最前——正在发生的事优先。
pub fn derive_background_tasks(
    messages: &[Message],
    shells: &[LiveTask],
    pending_crons: &[PendingCron],
) -> Vec<BackgroundTask> {
    let mut consumed: HashSet<String> = HashSet::new();
    // 子 agent 跑完会以系统通知的形式回到对话里，据此判定它结束了没有。
    let finished_subagents: HashSet<&str> = messages
        .iter()
        .filter_map(|m| match m.meta.as_ref() {
            Some(MessageMeta::SystemNotification {
                kind,
                task_id: Some(id),
                ..
            }) if kind == "bg_task_finished" => Some(id.as_str()),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for message in messages {
        for call in &message.tool_calls {
            match call.name.as_str() {
                "ScheduleWakeup" => {
                    let reason = arg_str(&call.input, "reason").unwrap_or("(无说明)").to_string();
                    let delay_secs = call
                        .input
                        .get("delay_secs")
                        .or_else(|| call.input.get("delaySeconds"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let pending = pending_crons.iter().find(|c| c.reason == reason);
                    let fire_at_ms = match pending {
                        Some(c) => c.fire_at_ms,
                        None => message.created_at + delay_secs * 1000,
                    };
                    out.push(BackgroundTask {
                        kind: BackgroundKind::Cron,
                        task_id: None,
                        tool_call_id: call.id.clone(),
                        command: reason.clone(),
                        running: pending.is_some(),
                        is_error: call.is_error,
                        result: call.result.clone(),
                        duration_ms: call.duration_ms,
                        elapsed_secs: None,
                        cron: Some(CronInfo {
                            reason,
                            fire_at_ms,
                            pending: pending.is_some(),
                        }),
                    });
                }
                "Task" => {
                    let Some(task_id) = call
                        .result
                        .as_deref()
                        .and_then(extract_subagent_task_id)
                    else {
                        continue;
                    };
                    out.push(BackgroundTask {
                        kind: BackgroundKind::Subagent,
                        running: !finished_subagents.contains(task_id.as_str()),
                        task_id: Some(task_id),
                        tool_call_id: call.id.clone(),
                        command: arg_str(&call.input, "subagent_type")
                            .unwrap_or("subagent")
                            .to_string(),
                        is_error: call.is_error,
                        result: call.result.clone(),
                        duration_ms: call.duration_ms,
                        elapsed_secs: None,
                        cron: None,
                    });
                }
                "Bash" => {
                    let explicit = call
                        .input
                        .get("run_in_background")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let task_id = call.result.as_deref().and_then(extract_bg_task_id);
                    // 前台正常跑完的 Bash：既没声明后台、结果里也没有任务编号。
                    if !explicit && task_id.is_none() {
                        continue;
                    }
                    let live = task_id
                        .as_ref()
                        .and_then(|id| shells.iter().find(|s| &s.task_id == id));
                    if let Some(id) = task_id.as_ref() {
                        consumed.insert(id.clone());
                    }
                    out.push(BackgroundTask {
                        kind: BackgroundKind::Bash,
                        // 状态以注册表为准；查不到就退回「有结果就是跑完了」。
                        // 反过来不行：后台 Bash 的结果只表示**启动成功**，
                        // 拿它当「跑完了」会让一个还在跑的任务显示成已完成。
                        running: match live {
                            Some(shell) => shell.running,
                            None => call.result.is_none(),
                        },
                        elapsed_secs: live.map(|s| s.elapsed_secs),
                        task_id,
                        tool_call_id: call.id.clone(),
                        command: arg_str(&call.input, "command").unwrap_or("(无命令)").to_string(),
                        is_error: call.is_error,
                        result: call.result.clone(),
                        duration_ms: call.duration_ms,
                        cron: None,
                    });
                }
                _ => {}
            }
        }
    }

    // 注册表里有、transcript 还没记上的：刚启动、结果还没回来、或上次进程留下的。
    for shell in shells {
        if consumed.contains(&shell.task_id) {
            continue;
        }
        out.push(BackgroundTask {
            kind: BackgroundKind::Bash,
            task_id: Some(shell.task_id.clone()),
            // 还没有对应的工具调用可跳，用一个占位 id 标记。
            tool_call_id: format!("pending-{}", shell.task_id),
            command: shell.command.clone(),
            running: shell.running,
            is_error: false,
            result: None,
            duration_ms: None,
            elapsed_secs: Some(shell.elapsed_secs),
            cron: None,
        });
    }

    // 运行中的排前面，其中跑得越久的越靠后（新起的更可能是用户正在等的那个）。
    let (mut running, done): (Vec<_>, Vec<_>) = out.into_iter().partition(|t| t.running);
    running.sort_by_key(|t| t.elapsed_secs.unwrap_or(0));
    running.into_iter().chain(done).collect()
}

fn arg_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 侧栏的一个项目分组。空项目也要显示（原前端同样保留空桶）。
#[derive(Debug, Clone)]
pub struct ProjectBucket {
    pub id: String,
    pub name: String,
    pub path: String,
    pub project_id: Option<String>,
    pub sessions: Vec<SessionMeta>,
}

/// 一个等着用户点「确认」的破坏性操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub action: ConfirmAction,
    /// 已经确认过几次。0 = 还没问过，1 = 问过一遍正在问第二遍。
    pub asked: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    DeleteSession { id: String, title: String },
    DeleteProject { id: String, name: String },
}

impl ConfirmAction {
    /// 两遍问话的正文。第二遍必须和第一遍不一样，否则用户会以为没点上，
    /// 无脑再点一次——那这道确认就白设了。
    pub fn body(&self, asked: u8) -> String {
        match self {
            ConfirmAction::DeleteSession { title, .. } => {
                if asked == 0 {
                    format!("删除对话「{title}」？")
                } else {
                    format!("再确认一次：删除对话「{title}」，删了就找不回来了。")
                }
            }
            ConfirmAction::DeleteProject { name, .. } => {
                if asked == 0 {
                    format!("删除项目「{name}」？项目下的对话不会被删掉。")
                } else {
                    format!("再确认一次：删除项目「{name}」。")
                }
            }
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            ConfirmAction::DeleteSession { .. } => "删除对话",
            ConfirmAction::DeleteProject { .. } => "删除项目",
        }
    }
}

/// 会话是否命中搜索。与原前端 `sessionMatchesQuery` 同语义：
/// 匹配「标题 + 模型名」，支持大小写敏感与正则两个开关。
pub fn session_matches(session: &SessionMeta, query: &str, case: bool, regex: bool) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let text = format!("{} {}", session.title, session.model);
    if regex {
        // 正则写坏时不炸 UI，直接判不匹配——与原前端 try/catch 一致。
        let built = if case {
            regex_lite_build(query)
        } else {
            regex_lite_build(&format!("(?i){query}"))
        };
        return built.map(|re| re.is_match(&text)).unwrap_or(false);
    }
    if case {
        text.contains(query)
    } else {
        text.to_lowercase().contains(&query.to_lowercase())
    }
}

fn regex_lite_build(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(pattern).ok()
}

/// 把会话按项目分桶。归属规则与原前端一致：先按 `project_id`，再按 workdir 兜底老会话；
/// 都不中的落「默认项目」。
pub fn build_buckets(
    projects: &[WorkspaceProject],
    sessions: &[SessionMeta],
    query: &str,
    case: bool,
    regex: bool,
) -> Vec<ProjectBucket> {
    let mut buckets: Vec<ProjectBucket> = projects
        .iter()
        .map(|project| ProjectBucket {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project
                .workdir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            project_id: Some(project.id.clone()),
            sessions: Vec::new(),
        })
        .collect();
    let mut default_bucket = ProjectBucket {
        id: "default".to_string(),
        name: "默认项目".to_string(),
        path: "未归入项目的对话".to_string(),
        project_id: None,
        sessions: Vec::new(),
    };

    for session in sessions {
        if !session_matches(session, query, case, regex) {
            continue;
        }
        let workdir = session.workdir.as_ref().map(|p| p.to_string_lossy().to_string());
        let index = buckets.iter().position(|b| {
            b.project_id == session.project_id
                || (!b.path.is_empty() && workdir.as_deref() == Some(b.path.as_str()))
        });
        match index {
            Some(i) => buckets[i].sessions.push(session.clone()),
            None => default_bucket.sessions.push(session.clone()),
        }
    }

    for bucket in &mut buckets {
        bucket.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    }
    default_bucket
        .sessions
        .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    buckets.push(default_bucket);
    buckets
}

/// 消息时间戳文案，与原前端 `formatTime` 一致：**当天只显示时分**，
/// 跨天才显示月/日。（我之前一律显示月/日，与原 UI 不符。）
pub fn format_message_time(ts_ms: i64, now_ms: i64) -> String {
    let Some(dt) = chrono::DateTime::from_timestamp_millis(ts_ms) else {
        return String::new();
    };
    let now = chrono::DateTime::from_timestamp_millis(now_ms).unwrap_or(dt);
    let same_day = dt.date_naive() == now.date_naive();
    if same_day {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%m/%d").to_string()
    }
}

/// 相对时间文案，与原前端 `relativeTime` 一一对应。
pub fn relative_time(ts_ms: i64, now_ms: i64) -> String {
    let diff = now_ms - ts_ms;
    const MIN: i64 = 60_000;
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;
    if diff < MIN {
        "刚刚".to_string()
    } else if diff < HOUR {
        format!("{}分钟前", diff / MIN)
    } else if diff < DAY {
        format!("{}小时前", diff / HOUR)
    } else if diff < 3 * DAY {
        format!("{}天前", diff / DAY)
    } else {
        chrono::DateTime::from_timestamp_millis(ts_ms)
            .map(|dt| dt.format("%m/%d").to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, title: &str, project: Option<&str>) -> SessionMeta {
        SessionMeta {
            id: id.to_string(),
            title: title.to_string(),
            provider_id: "p".to_string(),
            model: "deepseek-v4".to_string(),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            date: String::new(),
            source: None,
            project_id: project.map(|s| s.to_string()),
            workdir: None,
            path: None,
        }
    }

    #[test]
    fn sessions_without_project_land_in_default_bucket() {
        let buckets = build_buckets(&[], &[meta("a", "hi", None)], "", false, false);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].id, "default");
        assert_eq!(buckets[0].sessions.len(), 1);
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let session = meta("a", "优化指纹浏览器方案", None);
        assert!(session_matches(&session, "指纹", false, false));
        assert!(session_matches(&session, "DEEPSEEK", false, false));
        assert!(!session_matches(&session, "DEEPSEEK", true, false));
    }

    /// 正则写坏（未闭合括号）时必须判不匹配而不是 panic——搜索框逐字符输入时
    /// 中间态几乎总是非法正则，炸一次整个侧栏就没了。
    #[test]
    fn broken_regex_does_not_panic() {
        let session = meta("a", "hi", None);
        assert!(!session_matches(&session, "(unclosed", false, true));
    }

    fn tool_call(name: &str, input: serde_json::Value, result: Option<&str>) -> Message {
        Message {
            id: "m".into(),
            role: Role::Assistant,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: vec![agent_core::storage::sessions::MessageToolCall {
                id: "t".into(),
                name: name.into(),
                input,
                result: result.map(|s| s.to_string()),
                duration_ms: Some(10),
                is_error: false,
                nested: Vec::new(),
            }],
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        }
    }

    fn derive(msgs: &[Message]) -> Vec<BackgroundTask> {
        derive_background_tasks(msgs, &[], &[])
    }

    /// 前台 Bash 不该出现在后台任务列表里——它由聊天区的工具卡片管，
    /// 重复列出会让人分不清哪个才是真后台。
    #[test]
    fn foreground_bash_is_not_a_background_task() {
        let msgs = vec![tool_call(
            "Bash",
            serde_json::json!({"command": "ls"}),
            Some("ok"),
        )];
        assert!(derive(&msgs).is_empty());
    }

    /// 前台 Bash 跑超时会被转成后台任务：入参里**没有** run_in_background，
    /// 只有结果里那个编号能认出它。漏掉这条的话，最需要盯着的那种任务
    /// （跑太久的）反而不出现在后台任务面板里。
    #[test]
    fn promoted_foreground_bash_is_picked_up() {
        let msgs = vec![tool_call(
            "Bash",
            serde_json::json!({"command": "cargo build"}),
            Some("[bash_007] 60s 内未结束，已转后台"),
        )];
        let tasks = derive(&msgs);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id.as_deref(), Some("bash_007"));
    }

    /// 后台 Bash 的工具结果是 `[bash_001] 已在后台启动`——只说明**启动**成功。
    /// 必须把编号解析出来，否则面板没法和注册表里那份活记录对上号，
    /// 同一个任务会并排显示成一条「运行中」加一条「已完成」，自相矛盾。
    #[test]
    fn background_bash_task_id_is_parsed_both_formats() {
        for (result, want) in [
            ("[bash_001] 已在后台启动", "bash_001"),
            ("task_id=bash_042 cmd=sleep", "bash_042"),
        ] {
            let msgs = vec![tool_call(
                "Bash",
                serde_json::json!({"command": "sleep 300", "run_in_background": true}),
                Some(result),
            )];
            assert_eq!(derive(&msgs)[0].task_id.as_deref(), Some(want), "{result}");
        }

        // 还没返回结果时自然没有编号，这条会照常列出来。
        let msgs = vec![tool_call(
            "Bash",
            serde_json::json!({"command": "sleep 300", "run_in_background": true}),
            None,
        )];
        assert_eq!(derive(&msgs)[0].task_id, None);
    }

    /// 结果文本不是那个格式时不要瞎解析出一个假编号——假编号拿去和注册表比对，
    /// 会把一条毫不相干的活任务顶掉。`[nope]` 这种是重点：早先只找一对方括号，
    /// 任何带方括号的结果都会被当成编号。
    #[test]
    fn malformed_result_yields_no_task_id() {
        for bad in [
            "已在后台启动",
            "[] 空的",
            "no brackets here",
            "[nope] 不是任务编号",
            "[bash_] 没有数字",
        ] {
            let msgs = vec![tool_call(
                "Bash",
                serde_json::json!({"command": "x", "run_in_background": true}),
                Some(bad),
            )];
            assert_eq!(derive(&msgs)[0].task_id, None, "{bad}");
        }
    }

    /// 注册表说还在跑，就必须显示成还在跑——哪怕工具结果早就回来了。
    /// 后台 Bash 的结果只代表「启动成功」，拿它当「跑完了」会让面板睁眼说瞎话。
    #[test]
    fn registry_state_wins_over_tool_result() {
        let msgs = vec![tool_call(
            "Bash",
            serde_json::json!({"command": "sleep 300", "run_in_background": true}),
            Some("[bash_001] 已在后台启动"),
        )];
        let live = vec![LiveTask {
            task_id: "bash_001".into(),
            command: "sleep 300".into(),
            running: true,
            exit_code: None,
            elapsed_secs: 12,
        }];
        let tasks = derive_background_tasks(&msgs, &live, &[]);
        // 只有一条，不是「运行中」+「已结束」两条
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].running);
        assert_eq!(tasks[0].elapsed_secs, Some(12));
    }

    /// 注册表里有、transcript 里还没记上的（刚启动 / 上次进程留下的）也要列出来，
    /// 否则任务已经在跑了、面板却是空的。
    #[test]
    fn registry_only_task_still_shows_up() {
        let live = vec![LiveTask {
            task_id: "bash_009".into(),
            command: "npm run dev".into(),
            running: true,
            exit_code: None,
            elapsed_secs: 3,
        }];
        let tasks = derive_background_tasks(&[], &live, &[]);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "npm run dev");
        // 没有对应的工具调用可跳，用占位 id 标记
        assert!(tasks[0].tool_call_id.starts_with("pending-"));
    }

    /// 定时唤醒：还在等就用 scheduler 的精确时刻，已经唤醒过就用「调用时刻 + 延时」倒推。
    #[test]
    fn cron_uses_scheduler_time_while_pending() {
        let mut msg = tool_call(
            "ScheduleWakeup",
            serde_json::json!({"reason": "等 CI", "delay_secs": 600}),
            Some("已安排"),
        );
        msg.created_at = 1_000_000;

        // 还在等：用 scheduler 报的触发时刻
        let pending = vec![PendingCron {
            run_id: "r1".into(),
            fire_at_ms: 9_999_999,
            seconds_remaining: 300,
            reason: "等 CI".into(),
        }];
        let tasks = derive_background_tasks(std::slice::from_ref(&msg), &[], &pending);
        let cron = tasks[0].cron.as_ref().unwrap();
        assert!(cron.pending);
        assert_eq!(cron.fire_at_ms, 9_999_999);
        assert!(tasks[0].running);

        // 已经唤醒过：scheduler 里查不到，按调用时刻 + 延时倒推
        let tasks = derive_background_tasks(&[msg], &[], &[]);
        let cron = tasks[0].cron.as_ref().unwrap();
        assert!(!cron.pending);
        assert_eq!(cron.fire_at_ms, 1_000_000 + 600 * 1000);
        assert!(!tasks[0].running);
    }

    /// 子任务要能从结果里解出编号才算数；跑完与否看有没有收到完成通知。
    #[test]
    fn subagent_needs_task_id_and_finish_notice() {
        let msgs = vec![tool_call(
            "Task",
            serde_json::json!({"subagent_type": "Explore"}),
            Some("已启动 task_id=subagent-abc123"),
        )];
        let tasks = derive(&msgs);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "Explore");
        assert!(tasks[0].running, "没收到完成通知就还算在跑");

        // 解不出编号的（例如同步跑完的 Task）不进这个面板
        let msgs = vec![tool_call(
            "Task",
            serde_json::json!({"subagent_type": "Explore"}),
            Some("直接返回了结果"),
        )];
        assert!(derive(&msgs).is_empty());
    }

    /// 运行中的排在前面：面板最上面应该是「此刻正在发生的事」。
    #[test]
    fn running_tasks_sort_first() {
        let msgs = vec![
            tool_call(
                "Bash",
                serde_json::json!({"command": "done one", "run_in_background": true}),
                Some("[bash_001] 已在后台启动"),
            ),
            tool_call(
                "Bash",
                serde_json::json!({"command": "still going", "run_in_background": true}),
                Some("[bash_002] 已在后台启动"),
            ),
        ];
        let live = vec![LiveTask {
            task_id: "bash_002".into(),
            command: "still going".into(),
            running: true,
            exit_code: None,
            elapsed_secs: 5,
        }];
        let tasks = derive_background_tasks(&msgs, &live, &[]);
        assert_eq!(tasks[0].command, "still going");
        assert!(tasks[0].running);
    }

    #[test]
    fn message_time_shows_clock_today_and_date_otherwise() {
        let now = 1_786_000_000_000i64; // 某天正午附近
        // 同一天 → 时:分
        let today = format_message_time(now - 3_600_000, now);
        assert!(today.contains(':'), "同一天该显示时分，实际 {today}");
        // 跨天 → 月/日
        let older = format_message_time(now - 3 * 86_400_000, now);
        assert!(older.contains('/'), "跨天该显示月/日，实际 {older}");
    }

    #[test]
    fn relative_time_buckets() {
        let now = 1_000_000_000;
        assert_eq!(relative_time(now - 1_000, now), "刚刚");
        assert_eq!(relative_time(now - 120_000, now), "2分钟前");
        assert_eq!(relative_time(now - 7_200_000, now), "2小时前");
        assert_eq!(relative_time(now - 172_800_000, now), "2天前");
    }
}
