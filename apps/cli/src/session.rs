//! 终端 surface：每条 user message 一个 turn，复用 [`agent_core::Session`]。

use std::sync::Arc;

use agent_core::{
    context::transcript::Transcript, definition::AgentDefinition, workspace::Workspace, Harness,
    Recorder, Session, SessionConfig, TurnObserver, TurnOutcome,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use colored::Colorize;
use model_gateway::client::ModelClient;
use protocol::{
    ApprovalDecision, Event as AgentEvent, PermissionKind, PermissionRequestId, QuestionOption,
    UserAnswer,
};
use rustyline::{
    error::ReadlineError, Cmd, ConditionalEventHandler, DefaultEditor, Event, EventContext,
    EventHandler, KeyEvent, Movement, RepeatCount,
};

use crate::render::TurnRenderer;

pub struct CliSession {
    inner: Session,
    auto_approve: bool,
    provider_display: String,
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
        recorder: Option<Recorder>,
    ) -> Self {
        let definition = AgentDefinition::default();
        let inner = Session::new(
            Arc::new(harness),
            SessionConfig {
                definition,
                workspace,
                client,
                enabled_tools,
                initial_transcript: Transcript::new(system),
                recorder,
            },
        );
        Self {
            inner,
            auto_approve,
            provider_display,
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
            let prompt = format!("{} ", "›".cyan().bold());
            match rl.readline(&prompt) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if matches!(trimmed, "/exit" | "/quit" | "/q") {
                        break;
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

    /// 单 turn 核心：append user → run → 把事件循环交给 driver → commit assistant。
    async fn run_one_turn(&mut self, user_input: String) -> Result<()> {
        self.inner.append_user(user_input, Vec::new());
        let mut handle = self.inner.run();
        let mut observer = CliObserver {
            renderer: TurnRenderer::new(),
            auto_approve: self.auto_approve,
        };
        let summary = handle.drive(&mut observer).await;

        match summary.outcome {
            TurnOutcome::Done => {
                let text = observer.renderer.take_final_text();
                if !text.is_empty() {
                    self.inner.commit_assistant(text, Vec::new());
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
        _kind: &PermissionKind,
        _summary: &str,
    ) -> Option<ApprovalDecision> {
        self.auto_approve.then_some(ApprovalDecision::AllowOnce)
    }

    async fn on_question(
        &mut self,
        _request_id: &PermissionRequestId,
        question: &str,
        options: &[QuestionOption],
    ) -> Option<UserAnswer> {
        Some(ask_user_in_terminal(question.to_string(), options.to_vec()).await)
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

/// 用 inquire 弹一个 select + 自由输入。
///
/// - 方向键选择，Enter 确认
/// - **ESC 取消**（返回 `UserAnswer::Cancelled`）
/// - 选「其他（自由输入）」会继续弹 Text 输入框
async fn ask_user_in_terminal(question: String, options: Vec<QuestionOption>) -> UserAnswer {
    println!();
    println!("{} {}", "🤔".cyan(), question.bold());

    const OTHER_LABEL: &str = "其他（自由输入）";
    let display_items: Vec<String> = options
        .iter()
        .map(|opt| {
            if opt.description.is_empty() {
                opt.label.clone()
            } else {
                format!("{} — {}", opt.label, opt.description)
            }
        })
        .chain(std::iter::once(OTHER_LABEL.to_string()))
        .collect();

    let labels: Vec<String> = options.iter().map(|o| o.label.clone()).collect();

    tokio::task::spawn_blocking(move || {
        let select = inquire::Select::new("选择一项（ESC 取消）：", display_items.clone())
            .with_help_message("↑↓ 选择，Enter 确认，ESC 取消");
        match select.prompt() {
            Ok(choice) => {
                if choice == OTHER_LABEL {
                    match inquire::Text::new("请输入：").prompt() {
                        Ok(text) if !text.trim().is_empty() => UserAnswer::Custom { text },
                        _ => UserAnswer::Cancelled,
                    }
                } else if let Some(idx) = display_items.iter().position(|d| d == &choice) {
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
    eprintln!("  {}", "Ctrl+C / Ctrl+D / /exit 退出".dimmed());
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
