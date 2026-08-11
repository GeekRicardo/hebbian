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
    pub search_open: bool,
    pub search_case: bool,
    pub search_regex: bool,
    pub collapsed: HashSet<String>,

    /// 可用 skill（`//` 命令面板）。
    pub skills: Vec<agent_core::tools::skill::Skill>,

    /// 工作目录的 git 状态。`None` = 还没读或不是仓库。
    pub git: Option<agent_core::git_scm::GitProjectStatus>,

    /// 编辑区当前打开的文件（路径 + 正文）。
    pub open_file: Option<(PathBuf, String)>,

    /// 当前会话的 todo 列表。由 `TodoListUpdated` 事件驱动，不落单独的盘。
    pub todos: Vec<protocol::WireTodoItem>,

    /// 展开着的工具卡片 / 思考块（按 message id + 序号定位）。
    pub expanded_parts: HashSet<String>,

    // ── 文件树 ──────────────────────────────────────────────────
    /// 已读过的目录：路径 → 这一层的条目。
    pub dirs: HashMap<PathBuf, Vec<DirEntry>>,
    /// 展开着的目录。
    pub expanded_dirs: HashSet<PathBuf>,

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
            search_open: false,
            search_case: false,
            search_regex: false,
            collapsed: HashSet::new(),
            skills: Vec::new(),
            git: None,
            open_file: None,
            todos: Vec::new(),
            expanded_parts: HashSet::new(),
            dirs: HashMap::new(),
            expanded_dirs: HashSet::new(),
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
                self.todos.clear();
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
            CoreUpdate::Settings(settings) => {
                self.settings = *settings;
            }
            CoreUpdate::Providers(providers) => {
                self.providers = providers;
            }
            CoreUpdate::Skills(skills) => {
                self.skills = skills;
            }
            CoreUpdate::GitStatus(status) => {
                self.git = status.map(|s| *s);
            }
            CoreUpdate::FileLoaded { path, text } => {
                self.open_file = Some((path, text));
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
                ..
            } => {
                self.pending_approvals.insert(
                    session_id.to_string(),
                    PendingApproval {
                        request_id,
                        tool_name,
                        summary,
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

/// 侧栏的一个项目分组。空项目也要显示（原前端同样保留空桶）。
#[derive(Debug, Clone)]
pub struct ProjectBucket {
    pub id: String,
    pub name: String,
    pub path: String,
    pub project_id: Option<String>,
    pub sessions: Vec<SessionMeta>,
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

    #[test]
    fn relative_time_buckets() {
        let now = 1_000_000_000;
        assert_eq!(relative_time(now - 1_000, now), "刚刚");
        assert_eq!(relative_time(now - 120_000, now), "2分钟前");
        assert_eq!(relative_time(now - 7_200_000, now), "2小时前");
        assert_eq!(relative_time(now - 172_800_000, now), "2天前");
    }
}
