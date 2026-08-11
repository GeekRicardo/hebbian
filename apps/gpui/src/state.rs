//! UI 运行态：把 core 推来的 `CoreUpdate` 收敛成视图直接消费的数据。
//!
//! 与原 Web 前端 `useStore` 的分工一致——单一事实源在这里，视图只读不算。
//! 流式增量（TextDelta / Reasoning / 工具进度）落在 `streaming`，run 结束后由
//! 落盘消息接管，避免「实时拼的文本」与「jsonl 里的真消息」两份各自演化。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agent_core::storage::projects::WorkspaceProject;
use agent_core::storage::sessions::{Message, Role, Session, SessionMeta};
use protocol::{QuestionOption, WireEvent};

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
            claude_importable: Vec::new(),
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
            CoreUpdate::ClaudeImportable(list) => {
                self.claude_importable = list;
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
            CoreUpdate::LiveTasks(tasks) => {
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

/// 「后台任务」面板的一条。与原前端一样**从 `session.messages` 派生**——
/// 跑完的任务永远留在 transcript 里，不依赖任何运行期注册表，切会话/重启都还在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTask {
    pub kind: BackgroundKind,
    /// 命令原文 / 唤醒原因 / 子 agent 描述，取决于 kind。
    pub label: String,
    /// 有 result 就算跑完了；没有就是还在跑（或这轮没跑完就结束了）。
    pub finished: bool,
    pub is_error: bool,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    Bash,
    Cron,
    Subagent,
}

/// 注册表里还活着的后台任务（运行中的实时状态）。
/// transcript 派生只知道「有没有 result」，活的输出与真实退出码要从注册表拿。
#[derive(Debug, Clone)]
pub struct LiveTask {
    pub task_id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
}

/// 从消息里挑出后台任务。
///
/// 判定依据与原前端一致：后台 Bash（入参 `run_in_background: true`）、
/// `ScheduleWakeup`（定时唤醒）、`Task`（子 agent）。前台 Bash 不进这个列表——
/// 它由聊天区的工具卡片自己管，重复列出只会让人分不清哪个才是后台的。
pub fn derive_background_tasks(messages: &[Message]) -> Vec<BackgroundTask> {
    let mut out = Vec::new();
    for message in messages {
        for call in &message.tool_calls {
            let kind = match call.name.as_str() {
                "Bash" => {
                    let bg = call
                        .input
                        .get("run_in_background")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !bg {
                        continue;
                    }
                    BackgroundKind::Bash
                }
                "ScheduleWakeup" => BackgroundKind::Cron,
                "Task" => BackgroundKind::Subagent,
                _ => continue,
            };
            let label = match kind {
                BackgroundKind::Bash => call
                    .input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
                BackgroundKind::Cron => call
                    .input
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("定时唤醒"),
                BackgroundKind::Subagent => call
                    .input
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("子任务"),
            };
            out.push(BackgroundTask {
                kind,
                label: label.to_string(),
                finished: call.result.is_some(),
                is_error: call.is_error,
                duration_ms: call.duration_ms,
            });
        }
    }
    out
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

    /// 前台 Bash 不该出现在后台任务列表里——它由聊天区的工具卡片管，
    /// 重复列出会让人分不清哪个才是真后台。
    #[test]
    fn foreground_bash_is_not_a_background_task() {
        let msgs = vec![tool_call(
            "Bash",
            serde_json::json!({"command": "ls"}),
            Some("ok"),
        )];
        assert!(derive_background_tasks(&msgs).is_empty());
    }

    #[test]
    fn background_bash_cron_and_subagent_are_picked_up() {
        let msgs = vec![
            tool_call(
                "Bash",
                serde_json::json!({"command": "cargo build", "run_in_background": true}),
                None,
            ),
            tool_call(
                "ScheduleWakeup",
                serde_json::json!({"reason": "等 CI"}),
                Some("done"),
            ),
            tool_call("Task", serde_json::json!({"description": "查一下用法"}), None),
        ];
        let tasks = derive_background_tasks(&msgs);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].kind, BackgroundKind::Bash);
        assert_eq!(tasks[0].label, "cargo build");
        // 没有 result = 还在跑
        assert!(!tasks[0].finished);
        assert_eq!(tasks[1].kind, BackgroundKind::Cron);
        assert!(tasks[1].finished);
        assert_eq!(tasks[2].kind, BackgroundKind::Subagent);
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
