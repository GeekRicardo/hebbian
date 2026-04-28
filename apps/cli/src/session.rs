//! 终端会话上下文：维持 transcript、跑 turn、loop 交互。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    harness::RunParams,
    tools::permissions::PermissionGate,
    Harness,
};
use anyhow::{anyhow, Result};
use model_gateway::client::ModelClient;
use colored::Colorize;
use protocol::{AgentRef, ApprovalDecision, EventPayload};
use rustyline::{error::ReadlineError, DefaultEditor};

use crate::render::{TurnEnd, TurnRenderer};

pub struct Session {
    harness: Arc<Harness>,
    client: Arc<dyn ModelClient>,
    definition: AgentDefinition,
    transcript: Transcript,
    enabled_tools: Vec<String>,
    auto_approve: bool,
}

impl Session {
    pub fn new(
        harness: Harness,
        client: Arc<dyn ModelClient>,
        system: Option<String>,
        enabled_tools: Vec<String>,
        auto_approve: bool,
    ) -> Self {
        Self {
            harness: Arc::new(harness),
            client,
            definition: AgentDefinition::default(),
            transcript: Transcript::new(system),
            enabled_tools,
            auto_approve,
        }
    }

    /// 单次：发起一条 user message，渲染流式回复，结束后退出
    pub async fn run_single(&mut self, user_input: String) -> Result<()> {
        self.run_one_turn(user_input).await?;
        Ok(())
    }

    /// JSON 多轮：把历史 messages 压入 transcript，最后一条作为当前轮 user
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
        for m in messages {
            match m.role.as_str() {
                "user" => self.transcript.push_user(m.content, Vec::new()),
                "assistant" => self.transcript.push_assistant(m.content, Vec::new()),
                "system" => {
                    // 把 system 拼接到 transcript.system
                    let cur = self.transcript.system.clone().unwrap_or_default();
                    let next = if cur.is_empty() {
                        m.content
                    } else {
                        format!("{cur}\n\n{}", m.content)
                    };
                    self.transcript.system = Some(next);
                }
                other => return Err(anyhow!("不支持的 role: {other}")),
            }
        }
        self.run_one_turn(last.content).await?;
        Ok(())
    }

    /// loop 交互：rustyline 读输入，每行一个 turn，直到 Ctrl+D
    pub async fn run_loop(&mut self) -> Result<()> {
        print_banner(&self.client, &self.enabled_tools);

        let mut rl = DefaultEditor::new()?;
        let history_path = dirs::cache_dir()
            .map(|d| d.join("hebbian-cli-history.txt"))
            .filter(|p| p.parent().map(|d| d.exists()).unwrap_or(false));
        if let Some(path) = &history_path {
            let _ = rl.load_history(path);
        }

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
                    // Ctrl+C：清空当前行，继续
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    // Ctrl+D：退出
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

    /// 单 turn 核心：push user → spawn run → 渲染事件 → push assistant
    async fn run_one_turn(&mut self, user_input: String) -> Result<()> {
        self.transcript.push_user(user_input, Vec::new());

        // 必须在 spawn_run 之前订阅
        let mut events = self.harness.subscribe();
        let cancel: platform::CancelFlag = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(PermissionGate::new(self.definition.permission_policy.clone()));

        let run_id = self.harness.spawn_run(
            self.client.clone(),
            RunParams {
                agent: AgentRef::new(&self.definition.id),
                gate: gate.clone(),
                transcript: self.transcript.clone(),
                enabled_tools: self.enabled_tools.clone(),
                compaction_policy: self.definition.compaction_policy.clone(),
                stream: true,
                cancel,
                parent: None,
            },
        );

        let mut renderer = TurnRenderer::new();
        let outcome = loop {
            let event = match events.recv().await {
                Ok(e) => e,
                Err(_) => break TurnEnd::Failed("事件流意外关闭".into()),
            };
            if event.run_id != run_id {
                continue;
            }

            // HITL 自动批准（如果启用）
            if self.auto_approve {
                if let EventPayload::PermissionRequested { request_id, .. } = &event.payload {
                    let _ = self.harness.resolve_permission(
                        &event.run_id,
                        request_id,
                        ApprovalDecision::AllowOnce,
                    );
                }
            }

            if let Some(end) = renderer.on_event(&event) {
                break end;
            }
        };

        match outcome {
            TurnEnd::Done(text) => {
                if !text.is_empty() {
                    self.transcript.push_assistant(text, Vec::new());
                }
                Ok(())
            }
            TurnEnd::Failed(err) => Err(anyhow!(err)),
            TurnEnd::Cancelled => Err(anyhow!("已取消")),
        }
    }
}

fn print_banner(client: &Arc<dyn ModelClient>, tools: &[String]) {
    let provider = client.provider_id();
    let tool_str = if tools.is_empty() {
        "none".dimmed().to_string()
    } else {
        tools.join(", ").cyan().to_string()
    };
    eprintln!("{}", "Hebbian CLI".bold());
    eprintln!(
        "  provider: {} · tools: {}",
        provider.cyan(),
        tool_str,
    );
    eprintln!(
        "  {}",
        "Ctrl+D 退出 · /exit 退出 · 上下文跨 turn 自动累积".dimmed()
    );
    eprintln!();
}

/// JSON 多轮上下文输入格式
#[derive(serde::Deserialize)]
pub struct ConvoInput {
    pub messages: Vec<ConvoMessage>,
}

#[derive(serde::Deserialize)]
pub struct ConvoMessage {
    pub role: String,
    pub content: String,
}
