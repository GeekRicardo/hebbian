use std::sync::Arc;

use agent_core::{
    permissions::PermissionStore,
    storage::{sessions, sessions_dir},
};
use anyhow::{anyhow, Result};
use eframe::egui;
use protocol::{ApprovalDecision, PermissionScope, UserAnswer, WireEvent};
use surface_session::{RuntimeRegistry, TurnInput};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

fn main() -> eframe::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Hebbian Native POC",
        native_options,
        Box::new(|cc| Ok(Box::new(NativeApp::new(cc)))),
    )
}

#[derive(Debug)]
enum UiEvent {
    Wire(WireEvent),
    Error(String),
}

#[derive(Debug, Clone)]
struct PendingPermission {
    request_id: String,
    summary: String,
    tool_name: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingQuestion {
    request_id: String,
    question: String,
    options: Vec<protocol::QuestionOption>,
    multi: bool,
}

struct NativeApp {
    rt: Runtime,
    data_dir: std::path::PathBuf,
    permission_store: Option<Arc<PermissionStore>>,
    runtimes: RuntimeRegistry,
    events_tx: mpsc::UnboundedSender<UiEvent>,
    events_rx: mpsc::UnboundedReceiver<UiEvent>,
    sessions: Vec<sessions::SessionMeta>,
    active_session: Option<sessions::Session>,
    transcript: Vec<String>,
    input: String,
    busy: bool,
    status: String,
    pending_permission: Option<PendingPermission>,
    pending_question: Option<PendingQuestion>,
}

impl NativeApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let rt = Runtime::new().expect("tokio runtime");
        let data_dir = agent_core::storage::default_data_dir();
        let permission_store = PermissionStore::open(data_dir.clone()).ok().map(Arc::new);
        let runtimes = RuntimeRegistry::new();
        surface_session::register_wakeup_resume_handler(
            data_dir.clone(),
            permission_store.clone(),
            runtimes.clone(),
        );
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let mut app = Self {
            rt,
            data_dir,
            permission_store,
            runtimes,
            events_tx,
            events_rx,
            sessions: Vec::new(),
            active_session: None,
            transcript: Vec::new(),
            input: String::new(),
            busy: false,
            status: "准备就绪".to_string(),
            pending_permission: None,
            pending_question: None,
        };
        app.reload_sessions();
        app
    }

    fn reload_sessions(&mut self) {
        match sessions::list(&self.data_dir) {
            Ok(items) => self.sessions = items,
            Err(err) => self.status = format!("读取对话失败：{err}"),
        }
    }

    fn load_session(&mut self, session_id: String) {
        match sessions::load_with_partial_recovery(&self.data_dir, &session_id) {
            Ok(session) => {
                self.transcript = session
                    .messages
                    .iter()
                    .filter_map(|message| match message.role {
                        sessions::Role::User => Some(format!("你：{}", message.content)),
                        sessions::Role::Assistant => Some(format!("Hebbian：{}", message.content)),
                        _ => None,
                    })
                    .collect();
                self.active_session = Some(session);
                self.status = "已打开对话".to_string();
            }
            Err(err) => self.status = format!("打开对话失败：{err}"),
        }
    }

    fn create_session(&mut self) -> Result<()> {
        let providers_file = model_gateway::config::load(&self.data_dir)?;
        let provider = providers_file
            .providers
            .iter()
            .find(|provider| provider.enabled)
            .or_else(|| providers_file.providers.first())
            .ok_or_else(|| anyhow!("还没有配置模型供应商"))?;
        let model = provider
            .default_model
            .clone()
            .or_else(|| provider.models.first().cloned())
            .ok_or_else(|| anyhow!("供应商 {} 还没有可用模型", provider.name))?;

        let session = sessions::create_with_source(
            &self.data_dir,
            provider.id.clone(),
            model,
            None,
            None,
            "native".to_string(),
        )?;
        sessions_dir::ensure_session_dirs(&self.data_dir, &session.id)?;
        self.reload_sessions();
        self.load_session(session.id);
        Ok(())
    }

    fn send_current_input(&mut self) {
        let Some(session) = self.active_session.as_ref() else {
            self.status = "先新建或选择一个对话".to_string();
            return;
        };
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear();
        self.transcript.push(format!("你：{text}"));
        self.busy = true;
        self.status = "运行中".to_string();

        let data_dir = self.data_dir.clone();
        let permission_store = self.permission_store.clone();
        let runtimes = self.runtimes.clone();
        let tx = self.events_tx.clone();
        let session_id = session.id.clone();
        self.rt.spawn(async move {
            if let Err(err) = send_message(data_dir, permission_store, runtimes, session_id, text, tx.clone()).await {
                let _ = tx.send(UiEvent::Error(err.to_string()));
            }
        });
    }

    fn approve_permission(&mut self, decision: ApprovalDecision) {
        let Some(session) = self.active_session.as_ref() else {
            return;
        };
        let Some(pending) = self.pending_permission.take() else {
            return;
        };
        let data_dir = self.data_dir.clone();
        let permission_store = self.permission_store.clone();
        let runtimes = self.runtimes.clone();
        let tx = self.events_tx.clone();
        let session_id = session.id.clone();
        self.rt.spawn(async move {
            match runtimes.ensure(&data_dir, permission_store, &session_id).await {
                Ok(runtime) => {
                    runtime.state.resolve_approval(&pending.request_id, decision);
                }
                Err(err) => {
                    let _ = tx.send(UiEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn answer_question(&mut self, answer: UserAnswer) {
        let Some(session) = self.active_session.as_ref() else {
            return;
        };
        let Some(pending) = self.pending_question.take() else {
            return;
        };
        let data_dir = self.data_dir.clone();
        let permission_store = self.permission_store.clone();
        let runtimes = self.runtimes.clone();
        let tx = self.events_tx.clone();
        let session_id = session.id.clone();
        self.rt.spawn(async move {
            match runtimes.ensure(&data_dir, permission_store, &session_id).await {
                Ok(runtime) => {
                    runtime.state.answer_question(&pending.request_id, answer);
                }
                Err(err) => {
                    let _ = tx.send(UiEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                UiEvent::Wire(event) => self.apply_wire_event(event),
                UiEvent::Error(message) => {
                    self.busy = false;
                    self.status = message;
                }
            }
        }
    }

    fn apply_wire_event(&mut self, event: WireEvent) {
        match event {
            WireEvent::TextDelta { text, .. } => {
                if let Some(last) = self.transcript.last_mut().filter(|line| line.starts_with("Hebbian：")) {
                    last.push_str(&text);
                } else {
                    self.transcript.push(format!("Hebbian：{text}"));
                }
            }
            WireEvent::Reasoning { text, .. } => {
                self.status = format!("思考中：{}", text.chars().take(40).collect::<String>());
            }
            WireEvent::ToolStart { name, .. } => self.transcript.push(format!("工具：{name} 开始")),
            WireEvent::ToolDone { id, is_error, .. } => {
                let state = if is_error { "失败" } else { "完成" };
                self.transcript.push(format!("工具：{id} {state}"));
            }
            WireEvent::PermissionRequested {
                request_id,
                tool_name,
                summary,
                ..
            } => {
                self.pending_permission = Some(PendingPermission {
                    request_id,
                    summary,
                    tool_name: Some(tool_name),
                });
                self.status = "等待审批".to_string();
            }
            WireEvent::UserQuestionRequested {
                request_id,
                question,
                options,
                multi,
                ..
            } => {
                self.pending_question = Some(PendingQuestion {
                    request_id,
                    question,
                    options,
                    multi,
                });
                self.status = "等待回答".to_string();
            }
            WireEvent::RunFinished { .. } => {
                self.busy = false;
                self.status = "回答完成".to_string();
                self.reload_sessions();
            }
            WireEvent::Error { message } => {
                self.busy = false;
                self.status = message;
            }
            WireEvent::Notice { message, .. } => self.status = message,
            _ => {}
        }
    }
}

impl eframe::App for NativeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        egui::SidePanel::left("sessions").resizable(true).show(ctx, |ui| {
            ui.heading("Hebbian Native");
            if ui.button("新建对话").clicked() {
                if let Err(err) = self.create_session() {
                    self.status = err.to_string();
                }
            }
            if ui.button("刷新").clicked() {
                self.reload_sessions();
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let items: Vec<_> = self.sessions.clone();
                for session in items {
                    let selected = self
                        .active_session
                        .as_ref()
                        .is_some_and(|active| active.id == session.id);
                    if ui.selectable_label(selected, session.title).clicked() {
                        self.load_session(session.id);
                    }
                }
            });
        });

        egui::TopBottomPanel::bottom("input").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if self.busy {
                    ui.spinner();
                }
            });
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .hint_text("输入消息，回车发送")
                        .desired_width(f32::INFINITY),
                );
                let enter = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if ui.button("发送").clicked() || enter {
                    self.send_current_input();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(pending) = self.pending_permission.clone() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.heading("需要确认");
                    if let Some(tool_name) = &pending.tool_name {
                        ui.label(format!("工具：{tool_name}"));
                    }
                    ui.label(pending.summary);
                    ui.horizontal(|ui| {
                        if ui.button("允许一次").clicked() {
                            self.approve_permission(ApprovalDecision::AllowOnce);
                        }
                        if ui.button("本对话允许").clicked() {
                            self.approve_permission(ApprovalDecision::AllowAndRemember {
                                scope: PermissionScope::Session,
                                pattern: None,
                                extra_patterns: Vec::new(),
                            });
                        }
                        if ui.button("拒绝").clicked() {
                            self.approve_permission(ApprovalDecision::Deny);
                        }
                    });
                });
            }

            if let Some(pending) = self.pending_question.clone() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.heading("需要回答");
                    ui.label(pending.question);
                    if pending.multi {
                        ui.label("这个原型暂时只支持单选回答");
                    }
                    for option in pending.options {
                        if ui.button(&option.label).clicked() {
                            self.answer_question(UserAnswer::Selected { label: option.label });
                        }
                    }
                    if ui.button("取消").clicked() {
                        self.answer_question(UserAnswer::Cancelled);
                    }
                });
            }

            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.transcript {
                        ui.label(line);
                        ui.add_space(6.0);
                    }
                });
        });
    }
}

async fn send_message(
    data_dir: std::path::PathBuf,
    permission_store: Option<Arc<PermissionStore>>,
    runtimes: RuntimeRegistry,
    session_id: String,
    text: String,
    tx: mpsc::UnboundedSender<UiEvent>,
) -> Result<()> {
    let runtime = runtimes.ensure(&data_dir, permission_store, &session_id).await?;
    let mut events = runtime.state.subscribe();
    let tx_for_events = tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            let finished = matches!(event, WireEvent::RunFinished { .. } | WireEvent::Error { .. });
            let _ = tx_for_events.send(UiEvent::Wire(event));
            if finished {
                break;
            }
        }
    });

    if runtime.is_active() {
        if !runtime.inject(TurnInput::text(text)) {
            return Err(anyhow!("当前对话正在运行，插队失败"));
        }
        return Ok(());
    }
    runtime
        .input_tx
        .send(TurnInput::text(text))
        .map_err(|_| anyhow!("运行通道已关闭"))?;
    Ok(())
}
