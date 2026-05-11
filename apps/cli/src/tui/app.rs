//! TUI 主应用（架构 §8）。
//!
//! 三层 race：
//! - crossterm 终端事件（按键 / resize）
//! - run event 通道：当前 RunHandle 流过来的 protocol::Event（无 run 时 None）
//! - tick 200ms：定时驱动重绘 + 检查 cancel
//!
//! 主循环用 `tokio::select!` 三路 race；当一个 run 在跑时，输入框继续接收按键
//! 但 Enter 不再立即提交（用户得先等当前 run 结束——MVP 简化，不做 pending_inputs 队列）。

use agent_core::{run_mode::RunMode, RunHandle, Session};
use anyhow::Result;
use crossterm::{
    event::{Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use protocol::{ApprovalDecision, Event as AgentEvent, EventPayload, UserAnswer};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};

use super::components::{
    chat_view::{AutoDecision, ChatBlock, ChatView, ToolStatus},
    input_box::InputBox,
    permission_popup::{self, PermissionPopupState},
    question_popup::{self, QuestionPopupState},
    status_bar::{self, StatusBarState},
};

/// TUI 启动入口（被 main.rs 调用）。
pub async fn run(
    mut session: Session,
    provider_display: String,
    run_mode: RunMode,
    persist: Option<crate::session::PersistRef>,
) -> Result<()> {
    // 进入 alternate screen + raw mode。
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app_result = App::new(provider_display, run_mode, persist).run_loop(&mut terminal, &mut session).await;

    // 离开 alternate screen + 恢复 raw mode。
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);
    let _ = disable_raw_mode();

    app_result
}

/// 当前 run 的运行时上下文。
struct ActiveRun {
    handle: RunHandle,
    // 累积流式 reasoning，与 assistant text 分开存。
    streaming_text: String,
    streaming_reasoning: String,
    // 在 chat_view.blocks 里的 assistant block 索引（streaming 期间持续追加）。
    assistant_block_idx: Option<usize>,
}

struct App {
    chat: ChatView,
    input: InputBox,
    status: StatusBarState,
    permission_popup: Option<PermissionPopupState>,
    question_popup: Option<QuestionPopupState>,
    active_run: Option<ActiveRun>,
    persist: Option<crate::session::PersistRef>,
    should_quit: bool,
    // 模式快速切换：F2 在四种 RunMode 之间循环（仅本地状态，不重新构造 Session）。
    run_mode: RunMode,
}

impl App {
    fn new(
        provider_display: String,
        run_mode: RunMode,
        persist: Option<crate::session::PersistRef>,
    ) -> Self {
        Self {
            chat: ChatView::default(),
            input: InputBox::default(),
            status: StatusBarState {
                provider_display,
                used_tokens: 0,
                budget_tokens: 200_000,
                run_mode: run_mode.as_str().to_string(),
                model_step: 0,
                tool_step: 0,
            },
            permission_popup: None,
            question_popup: None,
            active_run: None,
            persist,
            should_quit: false,
            run_mode,
        }
    }

    async fn run_loop(
        mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
        session: &mut Session,
    ) -> Result<()> {
        // 初始 banner：作为一条 Note 渲染。
        self.chat
            .push(ChatBlock::Note(format!(
                "Hebbian TUI · {} · RunMode {} · F2 切换模式 · Ctrl+C 退出",
                self.status.provider_display,
                self.run_mode.as_str(),
            )));

        let mut event_stream = crossterm::event::EventStream::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));

        // 渲染一次初始帧。
        self.draw(terminal)?;

        while !self.should_quit {
            // 把 active_run 拆出来避免 select 借用冲突。
            let next_run_event = async {
                if let Some(run) = self.active_run.as_mut() {
                    run.handle.recv().await
                } else {
                    futures_util::future::pending::<Option<AgentEvent>>().await
                }
            };

            tokio::select! {
                maybe_event = event_stream.next() => {
                    match maybe_event {
                        Some(Ok(CtEvent::Key(key))) if key.kind == KeyEventKind::Press => {
                            self.handle_key(key, session).await?;
                        }
                        Some(Ok(CtEvent::Resize(_, _))) => {
                            // 下一次 draw 自适应；不动 state。
                        }
                        Some(Err(_)) | None => {
                            self.should_quit = true;
                        }
                        _ => {}
                    }
                }
                Some(event) = next_run_event => {
                    self.on_run_event(event, session).await?;
                }
                _ = tick.tick() => {
                    // 周期性刷一下 status_bar（token usage 等）。
                    let usage = session.context_usage();
                    self.status.used_tokens = usage.used_tokens as u64;
                    self.status.budget_tokens = usage.budget_tokens as u64;
                }
            }

            // 如果 active_run 跑完（handle.recv 返回 None / Done）就要清掉。
            // 这里的清理由 on_run_event 收到 RunFinished/Failed/Cancelled 处理；select 收到
            // None 的情况下直接抹掉 handle。
            if let Some(run) = &mut self.active_run {
                if run.handle.id_is_finished() {
                    self.active_run = None;
                }
            }

            self.draw(terminal)?;
        }
        Ok(())
    }

    fn draw(&mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
        terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(3),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(area);

            self.chat.render(frame, chunks[0]);
            self.input.render(frame, chunks[1]);
            status_bar::render(&self.status, frame, chunks[2]);

            if let Some(popup) = &self.permission_popup {
                permission_popup::render(popup, frame, area);
            } else if let Some(popup) = &self.question_popup {
                question_popup::render(popup, frame, area);
            }
        })?;
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent, session: &mut Session) -> Result<()> {
        // popup 优先消费按键。
        if self.permission_popup.is_some() {
            return self.handle_permission_key(key);
        }
        if self.question_popup.is_some() {
            return self.handle_question_key(key);
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if let Some(run) = &self.active_run {
                    run.handle.interrupt();
                    self.chat
                        .push(ChatBlock::Note("⏸ 已请求取消当前 run".to_string()));
                } else {
                    self.should_quit = true;
                }
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.input.clear();
            }
            (KeyCode::F(2), _) => {
                self.cycle_run_mode();
            }
            (KeyCode::PageUp, _) => self.chat.scroll_up(5),
            (KeyCode::PageDown, _) => {
                self.chat.scroll_down(5);
                self.chat.follow_bottom();
            }
            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                self.input.push_char('\n');
            }
            (KeyCode::Enter, _) => {
                let buf = self.input.take();
                let buf = buf.trim().to_string();
                if buf.is_empty() {
                    return Ok(());
                }
                if self.active_run.is_some() {
                    self.chat
                        .push(ChatBlock::Note("(当前 run 进行中，请等待或 Ctrl+C 取消)".into()));
                    return Ok(());
                }
                self.submit_user(buf, session);
            }
            (KeyCode::Backspace, _) => {
                self.input.pop_char();
            }
            (KeyCode::Char(c), _) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push_char(c);
            }
            _ => {}
        }
        Ok(())
    }

    fn cycle_run_mode(&mut self) {
        let next = match self.run_mode {
            RunMode::AskBeforeEdits => RunMode::EditAutomatically,
            RunMode::EditAutomatically => RunMode::PlanMode,
            RunMode::PlanMode => RunMode::AutoMode,
            RunMode::AutoMode => RunMode::AskBeforeEdits,
        };
        self.run_mode = next;
        self.status.run_mode = next.as_str().to_string();
        self.chat
            .push(ChatBlock::Note(format!("切到模式：{}", next.as_str())));
    }

    fn submit_user(&mut self, text: String, session: &mut Session) {
        self.persist_user(&text);
        self.chat.push(ChatBlock::User(text.clone()));
        // 占位 assistant block（streaming 期间往里追加）。
        let idx = self.chat.blocks_mut().len();
        self.chat.push(ChatBlock::Assistant {
            text: String::new(),
            reasoning: String::new(),
        });
        session.append_user(text, Vec::new());
        let handle = session.run();
        self.active_run = Some(ActiveRun {
            handle,
            streaming_text: String::new(),
            streaming_reasoning: String::new(),
            assistant_block_idx: Some(idx),
        });
    }

    async fn on_run_event(
        &mut self,
        event: AgentEvent,
        session: &mut Session,
    ) -> Result<()> {
        match &event.payload {
            EventPayload::TextDelta { text } => {
                if let Some(run) = &mut self.active_run {
                    run.streaming_text.push_str(text);
                    self.refresh_assistant_block();
                }
            }
            EventPayload::Reasoning { text } => {
                if let Some(run) = &mut self.active_run {
                    run.streaming_reasoning.push_str(text);
                    self.refresh_assistant_block();
                }
            }
            EventPayload::TextDone { full_text } => {
                if let Some(run) = &mut self.active_run {
                    run.streaming_text = full_text.clone();
                    self.refresh_assistant_block();
                }
            }
            EventPayload::ToolCallStarted { name, call_id, .. } => {
                self.chat.push(ChatBlock::ToolCall {
                    name: name.clone(),
                    brief: call_id.clone(),
                    status: ToolStatus::Running,
                });
                self.status.tool_step = self.status.tool_step.saturating_add(1);
            }
            EventPayload::ToolCallFinished {
                call_id, result, ..
            } => {
                // 找最近一条 brief == call_id 的 ToolCall block 标记完成。result 以
                // "ERROR" / "error" 前缀粗略判定失败（dispatcher 把错误信息直接进 result）。
                let is_failed = result.to_ascii_lowercase().contains("error");
                for b in self.chat.blocks_mut().iter_mut().rev() {
                    if let ChatBlock::ToolCall { brief, status, .. } = b {
                        if brief == call_id && *status == ToolStatus::Running {
                            *status = if is_failed {
                                ToolStatus::Failed
                            } else {
                                ToolStatus::Ok
                            };
                            break;
                        }
                    }
                }
            }
            EventPayload::PermissionRequested {
                request_id,
                kind,
                summary,
                ..
            } => {
                // AutoMode 下 judge 自己拍板，observer 不弹窗（与 CliObserver 同语义）。
                if matches!(self.run_mode, RunMode::AutoMode) {
                    // 无操作——actor 内部的 judge 会处理。
                } else {
                    self.permission_popup = Some(PermissionPopupState {
                        request_id: request_id.clone(),
                        kind: kind.clone(),
                        summary: summary.clone(),
                    });
                }
            }
            EventPayload::UserQuestionRequested {
                request_id,
                question,
                options,
                multi,
            } => {
                self.question_popup = Some(QuestionPopupState {
                    request_id: request_id.clone(),
                    question: question.clone(),
                    options: options.clone(),
                    multi: *multi,
                    input_buffer: String::new(),
                    free_input_mode: false,
                    picked: Vec::new(),
                });
            }
            EventPayload::PermissionAutoJudged {
                tool_name,
                decision,
                reason,
            } => {
                let dec = match decision.as_str() {
                    "allow" => AutoDecision::Allow,
                    "deny" => AutoDecision::Deny,
                    _ => AutoDecision::Route,
                };
                self.chat.push(ChatBlock::AutoJudged {
                    tool: tool_name.clone(),
                    decision: dec,
                    reason: reason.clone(),
                });
            }
            EventPayload::RunModeChanged { from: _, to } => {
                self.status.run_mode = to.clone();
                self.chat
                    .push(ChatBlock::Note(format!("RunMode → {to}")));
            }
            EventPayload::StepStarted { step_kind, .. } => {
                if matches!(step_kind, protocol::StepKind::Model) {
                    self.status.model_step = self.status.model_step.saturating_add(1);
                }
            }
            EventPayload::RunFinished {
                total_input_tokens,
                total_output_tokens,
                ..
            } => {
                if let Some(run) = self.active_run.as_mut() {
                    let final_text = std::mem::take(&mut run.streaming_text);
                    if !final_text.is_empty() {
                        session.commit_assistant(final_text.clone(), Vec::new());
                        self.persist_assistant(&final_text);
                    }
                }
                self.status.used_tokens =
                    self.status.used_tokens.saturating_add(*total_input_tokens + *total_output_tokens);
                self.active_run = None;
            }
            EventPayload::RunFailed { error } => {
                self.chat
                    .push(ChatBlock::Note(format!("✗ run 失败：{}", error.message)));
                self.active_run = None;
            }
            EventPayload::RunCancelled => {
                self.chat.push(ChatBlock::Note("⏸ 已取消".into()));
                self.active_run = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn refresh_assistant_block(&mut self) {
        let (text, reasoning) = match &self.active_run {
            Some(r) => (r.streaming_text.clone(), r.streaming_reasoning.clone()),
            None => return,
        };
        let idx = match self.active_run.as_ref().and_then(|r| r.assistant_block_idx) {
            Some(i) => i,
            None => return,
        };
        if let Some(b) = self.chat.blocks_mut().get_mut(idx) {
            if let ChatBlock::Assistant {
                text: t,
                reasoning: r,
            } = b
            {
                *t = text;
                *r = reasoning;
            }
        }
    }

    fn handle_permission_key(&mut self, key: KeyEvent) -> Result<()> {
        let popup = match self.permission_popup.as_ref() {
            Some(p) => p,
            None => return Ok(()),
        };
        let decision: Option<ApprovalDecision> = match (key.code, key.modifiers) {
            (KeyCode::Char(c), _) => permission_popup::decision_for_key(c),
            (KeyCode::Esc, _) => Some(ApprovalDecision::Deny),
            _ => None,
        };
        if let Some(dec) = decision {
            if let Some(run) = &self.active_run {
                run.handle.resolve_permission(&popup.request_id, dec);
            }
            // 简短反馈：在 chat 流插一行——具体决定的副作用由 dispatcher 走。
            self.chat
                .push(ChatBlock::Note("(已处理审批决定)".into()));
            self.permission_popup = None;
        }
        Ok(())
    }

    fn handle_question_key(&mut self, key: KeyEvent) -> Result<()> {
        let popup = match self.question_popup.as_mut() {
            Some(p) => p,
            None => return Ok(()),
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                if let Some(run) = &self.active_run {
                    run.handle.answer_question(&popup.request_id, UserAnswer::Cancelled);
                }
                self.question_popup = None;
            }
            (KeyCode::Tab, _) => {
                popup.free_input_mode = !popup.free_input_mode;
            }
            (KeyCode::Enter, _) => {
                let answer = question_popup::build_answer(popup);
                if let Some(run) = &self.active_run {
                    run.handle.answer_question(&popup.request_id, answer);
                }
                self.question_popup = None;
            }
            (KeyCode::Backspace, _) if popup.free_input_mode => {
                popup.input_buffer.pop();
            }
            (KeyCode::Char(c), _) if popup.free_input_mode => {
                popup.input_buffer.push(c);
            }
            (KeyCode::Char(c), _) => {
                // 选项数字键
                if let Some(idx) = question_popup::option_index_for_key(c) {
                    if let Some(opt) = popup.options.get(idx).cloned() {
                        if popup.multi {
                            if let Some(pos) = popup.picked.iter().position(|l| l == &opt.label) {
                                popup.picked.remove(pos);
                            } else {
                                popup.picked.push(opt.label);
                            }
                        } else {
                            popup.picked.clear();
                            popup.picked.push(opt.label);
                            // 单选立即提交
                            let answer = question_popup::build_answer(popup);
                            if let Some(run) = &self.active_run {
                                run.handle.answer_question(&popup.request_id, answer);
                            }
                            self.question_popup = None;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn persist_user(&self, content: &str) {
        if let Some(p) = &self.persist {
            use agent_core::storage::sessions::{self, Message as M, Role};
            let msg = M {
                id: sessions::new_id(),
                role: Role::User,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: chrono::Utc::now().timestamp_millis(),
                meta: None,
            };
            let _ = sessions::append_message(&p.data_dir, &p.session_id, msg);
        }
    }

    fn persist_assistant(&self, content: &str) {
        if content.is_empty() {
            return;
        }
        if let Some(p) = &self.persist {
            use agent_core::storage::sessions::{self, Message as M, Role};
            let msg = M {
                id: sessions::new_id(),
                role: Role::Assistant,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: chrono::Utc::now().timestamp_millis(),
                meta: None,
            };
            let _ = sessions::append_message(&p.data_dir, &p.session_id, msg);
        }
    }
}

// RunHandle 没暴露 `id_is_finished`；这里用 trait extension 兜底——它实际上靠
// recv 返回 None 来识别 finished，但我们已经在 select 上消化了事件，select 借用
// 期间不能 peek。简单实现：把 active_run 在 RunFinished/Failed/Cancelled 时直接清掉，
// 这里只对"通道意外关闭"的兜底做一个 no-op。
trait RunHandleExt {
    fn id_is_finished(&self) -> bool;
}

impl RunHandleExt for RunHandle {
    fn id_is_finished(&self) -> bool {
        false
    }
}

