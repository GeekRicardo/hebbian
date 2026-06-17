//! 终端 surface：每条 user message 一个 turn，复用 [`agent_core::Session`]。

use std::path::PathBuf;
use std::sync::Arc;

use agent_core::{
    context::transcript::Transcript, definition::AgentDefinition, permissions::PermissionStore,
    workspace::Workspace, Harness, Session, SessionConfig, TurnObserver, TurnOutcome,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use colored::Colorize;
use model_gateway::client::ModelClient;
use agent_core::storage::sessions::{self, Message as StoredMessage, Role as StoredRole};
use protocol::{
    ApprovalDecision, Event as AgentEvent, PermissionKind, PermissionRequestId, PermissionScope,
    QuestionOption, UserAnswer,
};
use rustyline::{
    error::ReadlineError, Cmd, ConditionalEventHandler, DefaultEditor, Event, EventContext,
    EventHandler, KeyEvent, Movement, RepeatCount,
};

use crate::render::TurnRenderer;

/// 把每个 turn 落盘到 `<data_dir>/sessions/<date>/<id>.jsonl`。
pub struct SessionPersist {
    pub data_dir: PathBuf,
    pub session_id: String,
    /// `--history` 加载时的历史消息，构造 transcript 时填进去。
    pub seed_messages: Vec<StoredMessage>,
}

pub struct CliSession {
    inner: Session,
    auto_approve: bool,
    run_mode: agent_core::run_mode::RunMode,
    provider_display: String,
    persist: Option<PersistRef>,
}

/// `SessionPersist` 在构造完 transcript 后剩下的部分（不再持有 seed_messages）。
pub struct PersistRef {
    pub data_dir: PathBuf,
    pub session_id: String,
}

impl CliSession {
    pub fn new(
        harness: Harness,
        client: Arc<dyn ModelClient>,
        system: Option<String>,
        enabled_tools: Vec<String>,
        auto_approve: bool,
        workspace: Arc<Workspace>,
        provider_display: String,
        persist: Option<SessionPersist>,
        model_io_dump: Option<agent_core::ModelIoDump>,
        permission_store: Option<Arc<PermissionStore>>,
        run_mode: agent_core::run_mode::RunMode,
        model_name: String,
    ) -> Self {
        let definition = AgentDefinition::default();
        let (initial_transcript, persist_ref) = match persist {
            Some(p) => (
                Transcript::from_session(system, &p.seed_messages),
                Some(PersistRef {
                    data_dir: p.data_dir,
                    session_id: p.session_id,
                }),
            ),
            None => (Transcript::new(system), None),
        };
        // 给 PermissionStore 在该 session 下预热一个空规则视图（架构 §4.6.2）。
        // jsonl 回放 PermissionRule entry 当前未实现，所以传空 Vec。
        if let (Some(store), Some(p)) = (&permission_store, persist_ref.as_ref()) {
            store.load_session_rules(&p.session_id, Vec::new());
        }
        let inner = Session::new(
            Arc::new(harness),
            SessionConfig {
                definition,
                workspace,
                client,
                enabled_tools,
                initial_transcript,
                recorder: None,
                model_io_dump,
                permission_store,
                session_id: persist_ref.as_ref().map(|p| p.session_id.clone()),
                run_mode,
                model_id: Some(model_name.clone()),
                data_dir: persist_ref.as_ref().map(|p| p.data_dir.clone()),
            },
        );
        Self {
            inner,
            auto_approve,
            run_mode,
            provider_display,
            persist: persist_ref,
        }
    }

    fn persist_user(&self, content: &str) {
        if let Some(p) = &self.persist {
            let msg = StoredMessage {
                id: sessions::new_id(),
                role: StoredRole::User,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: chrono::Utc::now().timestamp_millis(),
                meta: None,
                subagent_call_id: None,
                run_duration_ms: None,
            };
            if let Err(e) = sessions::append_message(&p.data_dir, &p.session_id, msg) {
                eprintln!("{} 保存 user 消息失败：{e}", "warn:".yellow());
            }
        }
    }

    fn persist_assistant(&self, content: &str, run_duration_ms: Option<u64>) {
        if content.is_empty() {
            return;
        }
        if let Some(p) = &self.persist {
            let msg = StoredMessage {
                id: sessions::new_id(),
                role: StoredRole::Assistant,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: chrono::Utc::now().timestamp_millis(),
                meta: None,
                subagent_call_id: None,
                run_duration_ms,
            };
            if let Err(e) = sessions::append_message(&p.data_dir, &p.session_id, msg) {
                eprintln!("{} 保存 assistant 消息失败：{e}", "warn:".yellow());
            }
        }
    }

    /// 单次：发起一条 user message，渲染流式回复，结束后退出。
    pub async fn run_single(&mut self, user_input: String) -> Result<()> {
        self.run_one_turn(user_input).await
    }

    /// JSON 多轮：把历史 messages 压入 transcript，最后一条作为当前轮 user。
    pub async fn run_with_history(&mut self, mut messages: Vec<ConvoMessage>) -> Result<()> {
        if messages.is_empty() {
            return Err(anyhow!("messages 为空"));
        }
        let last = messages.pop().unwrap();
        if last.role != "user" {
            return Err(anyhow!(
                "messages 最后一条必须是 user，实际为 {}",
                last.role
            ));
        }
        let transcript = self.inner.transcript_mut();
        for m in messages {
            match m.role.as_str() {
                "user" => transcript.push_user(m.content, Vec::new()),
                "assistant" => transcript.push_assistant(m.content, Vec::new()),
                "system" => {
                    let cur = transcript.system.clone().unwrap_or_default();
                    transcript.system = Some(if cur.is_empty() {
                        m.content
                    } else {
                        format!("{cur}\n\n{}", m.content)
                    });
                }
                other => return Err(anyhow!("不支持的 role: {other}")),
            }
        }
        self.run_one_turn(last.content).await
    }

    /// 把 inner Session / provider / run_mode / persist 拆出来，交给 TUI 主循环。
    /// 调用后本对象不再可用。
    pub fn into_tui_parts(
        self,
    ) -> (
        agent_core::Session,
        String,
        agent_core::run_mode::RunMode,
        Option<PersistRef>,
    ) {
        (self.inner, self.provider_display, self.run_mode, self.persist)
    }

    /// loop 交互：rustyline 读输入，每行一个 turn，直到 Ctrl+D。
    pub async fn run_loop(&mut self) -> Result<()> {
        print_banner(&self.provider_display, self.inner.enabled_tools());

        let mut rl = DefaultEditor::new()?;
        let history_path = dirs::cache_dir()
            .map(|d| d.join("hebbian-cli-history.txt"))
            .filter(|p| p.parent().map(|d| d.exists()).unwrap_or(false));
        if let Some(path) = &history_path {
            let _ = rl.load_history(path);
        }
        rl.bind_sequence(
            KeyEvent::ctrl('C'),
            EventHandler::Conditional(Box::new(CtrlCHandler)),
        );

        loop {
            let usage = self.inner.context_usage();
            let pct = (usage.ratio() * 100.0).round() as u32;
            let pct_label = format!("[{pct}%]");
            let pct_colored = if pct >= 90 {
                pct_label.red().bold().to_string()
            } else if pct >= 70 {
                pct_label.yellow().to_string()
            } else {
                pct_label.dimmed().to_string()
            };
            let prompt = format!("{} {} ", pct_colored, "›".cyan().bold());
            match rl.readline(&prompt) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if matches!(trimmed, "/exit" | "/quit" | "/q") {
                        break;
                    }
                    if let Some(args) = trimmed.strip_prefix("/compact") {
                        let _ = rl.add_history_entry(line.as_str());
                        if let Err(e) = self.run_compact(args.trim()).await {
                            eprintln!("{} {e}", "错误:".red());
                        }
                        println!();
                        continue;
                    }
                    let _ = rl.add_history_entry(line.as_str());
                    if let Err(e) = self.run_one_turn(line).await {
                        eprintln!("{} {e}", "错误:".red());
                    }
                    println!();
                }
                Err(ReadlineError::Interrupted) => {
                    println!();
                    break;
                }
                Err(ReadlineError::Eof) => {
                    println!();
                    break;
                }
                Err(e) => return Err(anyhow!("readline error: {e}")),
            }
        }

        if let Some(path) = &history_path {
            let _ = rl.save_history(path);
        }
        Ok(())
    }

    /// /compact：调一次模型把整段 transcript 浓缩成摘要。
    async fn run_compact(&mut self, custom_instructions: &str) -> Result<()> {
        eprintln!("{}", "正在压缩上下文…".dimmed());
        let custom = if custom_instructions.is_empty() {
            None
        } else {
            Some(custom_instructions)
        };
        let result = self
            .inner
            .compact(custom)
            .await
            .map_err(|e| anyhow!("压缩失败：{e}"))?;
        eprintln!(
            "{} {} → {} tokens",
            "✔ 已压缩".green().bold(),
            result.before_tokens,
            result.after_tokens,
        );
        if !result.summary.is_empty() {
            eprintln!("{}", "──── 摘要 ────".dimmed());
            eprintln!("{}", result.summary);
            eprintln!("{}", "──────────────".dimmed());
        }
        Ok(())
    }

    /// 单 turn 核心：append user → run → 把事件循环交给 driver → commit assistant。
    async fn run_one_turn(&mut self, user_input: String) -> Result<()> {
        self.persist_user(&user_input);
        self.inner.append_user(user_input, Vec::new());
        let mut handle = self.inner.run();
        let mut observer = CliObserver {
            renderer: TurnRenderer::new(),
            auto_approve: self.auto_approve,
            run_mode: self.run_mode,
        };
        let summary = handle.drive(&mut observer).await;

        match summary.outcome {
            // 架构 §4.12.1：Suspended 与 Done 走同一段持久化——transcript 从 jsonl
            // 重建（§4.12.3），本轮 assistant 必须落盘；不报错让 cli 静静等 wakeup。
            TurnOutcome::Done | TurnOutcome::Suspended => {
                let text = observer.renderer.take_final_text();
                if !text.is_empty() {
                    self.inner.commit_assistant(text.clone(), Vec::new());
                    self.persist_assistant(&text, summary.duration_ms);
                }
                Ok(())
            }
            TurnOutcome::Failed(err) => Err(anyhow!(err)),
            TurnOutcome::Cancelled => Err(anyhow!("已取消")),
        }
    }
}

struct CliObserver {
    renderer: TurnRenderer,
    auto_approve: bool,
    run_mode: agent_core::run_mode::RunMode,
}

#[async_trait]
impl TurnObserver for CliObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        if std::env::var_os("HEBBIAN_DUMP_EVENTS").is_some() {
            // 只 dump 关键 payload 类型，不打印用户内容
            let tag = match &event.payload {
                protocol::EventPayload::TextDelta { text } => format!("TextDelta(len={})", text.len()),
                protocol::EventPayload::Reasoning { text } => format!("Reasoning(len={})", text.len()),
                protocol::EventPayload::TextDone { full_text } => format!("TextDone(len={})", full_text.len()),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            eprintln!("[event] {tag}");
        }
        self.renderer.on_event(event);
    }

    async fn on_permission_request(
        &mut self,
        _request_id: &PermissionRequestId,
        kind: &PermissionKind,
        summary: &str,
    ) -> Option<ApprovalDecision> {
        // AutoMode 下 LLM judge 是唯一决策者；observer 不参与（否则会和 judge race）。
        if matches!(self.run_mode, agent_core::run_mode::RunMode::AutoMode) {
            return None;
        }
        if self.auto_approve {
            return Some(ApprovalDecision::AllowOnce);
        }
        Some(prompt_approval_in_terminal(kind.clone(), summary.to_string()).await)
    }

    async fn on_question(
        &mut self,
        _request_id: &PermissionRequestId,
        question: &str,
        options: &[QuestionOption],
        multi: bool,
        questions: &[protocol::AskQuestion],
    ) -> Option<UserAnswer> {
        if questions.is_empty() {
            Some(ask_user_in_terminal(question.to_string(), options.to_vec(), multi).await)
        } else {
            Some(ask_questions_in_terminal(questions.to_vec()).await)
        }
    }
}

struct CtrlCHandler;

impl ConditionalEventHandler for CtrlCHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        ctx: &EventContext,
    ) -> Option<Cmd> {
        if ctx.line().is_empty() {
            Some(Cmd::Interrupt)
        } else {
            Some(Cmd::Kill(Movement::WholeBuffer))
        }
    }
}

/// 用 inquire 弹一个 select / multi-select + 自由输入。
///
/// - 单选：方向键选择，Enter 确认
/// - 多选（`multi = true`）：Space 勾选，Enter 确认
/// - **ESC 取消**（返回 `UserAnswer::Cancelled`）
/// - 单选模式选「其他（自由输入）」会继续弹 Text 输入框；多选模式不提供该项
async fn ask_user_in_terminal(
    question: String,
    options: Vec<QuestionOption>,
    multi: bool,
) -> UserAnswer {
    println!();
    println!("{} {}", "🤔".cyan(), question.bold());

    let labels: Vec<String> = options.iter().map(|o| o.label.clone()).collect();
    let display_items: Vec<String> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let prefix = format!("{}.", i + 1);
            if opt.description.is_empty() {
                format!("{prefix} {}", opt.label)
            } else {
                format!("{prefix} {} — {}", opt.label, opt.description)
            }
        })
        .collect();

    if multi {
        let display_items_for_task = display_items.clone();
        let labels_for_task = labels.clone();
        return tokio::task::spawn_blocking(move || {
            let prompt = inquire::MultiSelect::new(
                "勾选（Space 选中，Enter 确认，ESC 取消）：",
                display_items_for_task.clone(),
            )
            .with_help_message("↑↓ 移动，Space 勾选，Enter 确认，ESC 取消");
            match prompt.prompt() {
                Ok(choices) => {
                    let picked: Vec<String> = choices
                        .iter()
                        .filter_map(|c| {
                            display_items_for_task
                                .iter()
                                .position(|d| d == c)
                                .and_then(|idx| labels_for_task.get(idx).cloned())
                        })
                        .collect();
                    if picked.is_empty() {
                        UserAnswer::Cancelled
                    } else {
                        UserAnswer::SelectedMulti { labels: picked }
                    }
                }
                Err(_) => UserAnswer::Cancelled,
            }
        })
        .await
        .unwrap_or(UserAnswer::Cancelled);
    }

    const OTHER_LABEL: &str = "其他（自由输入）";
    let mut single_items = display_items.clone();
    single_items.push(OTHER_LABEL.to_string());

    tokio::task::spawn_blocking(move || {
        let select = inquire::Select::new("选择一项（ESC 取消）：", single_items.clone())
            .with_help_message("↑↓ 选择，Enter 确认，ESC 取消");
        match select.prompt() {
            Ok(choice) => {
                if choice == OTHER_LABEL {
                    match inquire::Text::new("请输入：").prompt() {
                        Ok(text) if !text.trim().is_empty() => UserAnswer::Custom { text },
                        _ => UserAnswer::Cancelled,
                    }
                } else if let Some(idx) = single_items.iter().position(|d| d == &choice) {
                    let label = labels.get(idx).cloned().unwrap_or(choice);
                    UserAnswer::Selected { label }
                } else {
                    UserAnswer::Selected { label: choice }
                }
            }
            Err(_) => UserAnswer::Cancelled,
        }
    })
    .await
    .unwrap_or(UserAnswer::Cancelled)
}

async fn ask_questions_in_terminal(questions: Vec<protocol::AskQuestion>) -> UserAnswer {
    let mut items = Vec::new();
    for q in questions {
        if !q.description.is_empty() {
            println!("{}", q.description.dimmed());
        }
        let answer = ask_user_in_terminal(q.title.clone(), q.options, q.multi).await;
        let answer = match answer {
            UserAnswer::Selected { label } => protocol::SingleAnswer::Selected { label },
            UserAnswer::SelectedMulti { labels } => protocol::SingleAnswer::SelectedMulti { labels },
            UserAnswer::Custom { text } => protocol::SingleAnswer::Custom { text },
            UserAnswer::Cancelled => return UserAnswer::Cancelled,
            UserAnswer::Multi { .. } => protocol::SingleAnswer::Cancelled,
        };
        items.push(protocol::MultiQuestionAnswer { title: q.title, answer });
    }
    UserAnswer::Multi { items }
}

/// 终端里弹审批选单，用 inquire + spawn_blocking。
///
/// - `Bash` 类带 `fingerprint` 的工具会多一档「始终允许 `<前缀>`」/「始终允许 `<root>`
///   所有子命令」，与 desktop UI 行为一致；记住的粒度由 [`HitlGate`] 用前缀 token 匹配。
/// - 其它工具（无 fingerprint）只有「始终允许工具 X（会话级）」一档。
/// - 路径越界 / 计划 / 长 run 续跑只给「允许一次 / 拒绝」。
/// - ESC 或选「拒绝」都返回 [`ApprovalDecision::Deny`]。
async fn prompt_approval_in_terminal(
    kind: PermissionKind,
    summary: String,
) -> ApprovalDecision {
    enum Choice {
        Once,
        RememberPattern(String),
        RememberTool,
        Deny,
    }

    println!();
    let header = match &kind {
        PermissionKind::ToolCall { tool_name, .. } => format!("🔒 审批：调用 {tool_name}"),
        PermissionKind::PathAccess { tool_name, paths } => {
            format!("🔒 审批：{tool_name} 越界访问 {} 个路径", paths.len())
        }
        PermissionKind::Plan { .. } => "🔒 审批：执行计划".to_string(),
        PermissionKind::ContinueLongRun { iterations_used } => {
            format!("🔒 审批：已运行 {iterations_used} 轮，是否继续")
        }
    };
    println!("{}", header.yellow().bold());
    if !summary.is_empty() {
        println!("   {}", summary.dimmed());
    }

    let mut choices: Vec<(String, Choice)> =
        vec![("✓ 允许一次".to_string(), Choice::Once)];

    if let PermissionKind::ToolCall {
        tool_name,
        fingerprint,
        ..
    } = &kind
    {
        match fingerprint {
            Some(fp) if !fp.trim().is_empty() => {
                let fp = fp.trim().to_string();
                choices.push((
                    format!("✓ 始终允许 `{fp}`（项目级）"),
                    Choice::RememberPattern(fp.clone()),
                ));
                let tokens: Vec<&str> = fp.split_whitespace().collect();
                if tokens.len() >= 2 {
                    let root = tokens[0].to_string();
                    choices.push((
                        format!("✓ 始终允许 `{root}` 所有子命令（项目级）"),
                        Choice::RememberPattern(root),
                    ));
                }
            }
            _ => {
                choices.push((
                    format!("✓ 始终允许工具 {tool_name}（会话级）"),
                    Choice::RememberTool,
                ));
            }
        }
    }
    choices.push(("✗ 拒绝".to_string(), Choice::Deny));

    tokio::task::spawn_blocking(move || {
        let display: Vec<String> = choices.iter().map(|(s, _)| s.clone()).collect();
        let pick = inquire::Select::new("请选择（ESC = 拒绝）：", display.clone())
            .with_help_message("↑↓ 选择，Enter 确认，ESC 拒绝")
            .prompt();
        let idx = pick.ok().and_then(|c| display.iter().position(|d| d == &c));
        let choice = idx
            .and_then(|i| choices.into_iter().nth(i).map(|(_, c)| c))
            .unwrap_or(Choice::Deny);
        match choice {
            Choice::Once => ApprovalDecision::AllowOnce,
            Choice::RememberPattern(pattern) => ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: Some(pattern),
            extra_patterns: Vec::new(),
        },
            Choice::RememberTool => ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: None,
            extra_patterns: Vec::new(),
        },
            Choice::Deny => ApprovalDecision::Deny,
        }
    })
    .await
    .unwrap_or(ApprovalDecision::Deny)
}

fn print_banner(provider_display: &str, tools: &[String]) {
    let tool_str = if tools.is_empty() {
        "built-in".cyan().to_string()
    } else {
        format!("built-in + {}", tools.join(", "))
            .cyan()
            .to_string()
    };
    eprintln!("{}", "Hebbian CLI".bold());
    eprintln!(
        "  provider: {} · tools: {}",
        provider_display.cyan(),
        tool_str,
    );
    eprintln!(
        "  {}",
        "/compact [指令] 主动压缩上下文 · Ctrl+C / Ctrl+D / /exit 退出".dimmed()
    );
    eprintln!();
}

/// JSON 多轮上下文输入格式。
#[derive(serde::Deserialize)]
pub struct ConvoInput {
    pub messages: Vec<ConvoMessage>,
}

#[derive(serde::Deserialize)]
pub struct ConvoMessage {
    pub role: String,
    pub content: String,
}
