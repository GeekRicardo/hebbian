//! 把 `protocol::Event` 流渲染成有色彩的终端输出。
//!
//! 设计原则：流式可读、信息密度适中、不抢占用户视线。

use std::io::{self, Write};

use colored::Colorize;
use protocol::{Event, EventPayload, PermissionRequestId, QuestionOption};

/// 累积一次 turn 的渲染状态（跨 event）
pub struct TurnRenderer {
    streaming_text: bool,
    accumulated_text: String,
}

impl TurnRenderer {
    pub fn new() -> Self {
        Self {
            streaming_text: false,
            accumulated_text: String::new(),
        }
    }

    /// 处理一个事件，返回 session 应做的下一步动作。
    pub fn on_event(&mut self, event: &Event) -> RendererAction {
        match &event.payload {
            EventPayload::RunStarted { .. } => RendererAction::Continue,
            EventPayload::TurnStarted { turn, .. } => {
                if *turn > 0 {
                    println!();
                }
                RendererAction::Continue
            }
            EventPayload::TextDelta { text } => {
                print!("{}", text);
                io::stdout().flush().ok();
                self.accumulated_text.push_str(text);
                self.streaming_text = true;
                RendererAction::Continue
            }
            EventPayload::TextDone { full_text } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                if !full_text.is_empty() && self.accumulated_text.is_empty() {
                    println!("{}", full_text);
                    self.accumulated_text = full_text.clone();
                }
                RendererAction::Continue
            }
            EventPayload::ToolCallStarted { name, input, .. } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                let summary = summarize_input(input);
                println!("{} {}{}", "🔧".yellow(), name.yellow().bold(), summary.dimmed());
                RendererAction::Continue
            }
            EventPayload::ToolCallFinished {
                result,
                duration_ms,
                truncated,
                ..
            } => {
                let preview = preview_result(result);
                let suffix = if *truncated { " (截断)" } else { "" };
                println!(
                    "  {} {}ms · {}{}",
                    "↳".dimmed(),
                    duration_ms.to_string().dimmed(),
                    preview.dimmed(),
                    suffix.dimmed(),
                );
                RendererAction::Continue
            }
            EventPayload::PermissionRequested { summary, .. } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                println!("  {} {}", "⏸".yellow(), summary.yellow());
                RendererAction::Continue
            }
            EventPayload::PermissionResolved { decision, .. } => {
                let label = match decision {
                    protocol::ApprovalDecision::AllowOnce => "✓ 允许".green().to_string(),
                    protocol::ApprovalDecision::AllowAndRemember { .. } => {
                        "✓ 允许并记住".green().to_string()
                    }
                    protocol::ApprovalDecision::Deny => "✗ 拒绝".red().to_string(),
                    protocol::ApprovalDecision::DenyWithFeedback { .. } => {
                        "✗ 拒绝（含反馈）".red().to_string()
                    }
                };
                println!("  {label}");
                RendererAction::Continue
            }
            EventPayload::UserQuestionRequested {
                request_id,
                question,
                options,
            } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                RendererAction::AwaitQuestion {
                    request_id: request_id.clone(),
                    question: question.clone(),
                    options: options.clone(),
                }
            }
            EventPayload::UserQuestionAnswered { answer, .. } => {
                let label = match answer {
                    protocol::UserAnswer::Selected { label } => format!("✓ {label}"),
                    protocol::UserAnswer::Custom { text } => format!("✓ 自由输入：{text}"),
                    protocol::UserAnswer::Cancelled => "✗ 用户取消".to_string(),
                };
                println!("  {}", label.green());
                RendererAction::Continue
            }
            EventPayload::ContextCompacted {
                before_tokens,
                after_tokens,
            } => {
                println!(
                    "  {}",
                    format!("[上下文压缩 {before_tokens}→{after_tokens} tokens]").dimmed()
                );
                RendererAction::Continue
            }
            EventPayload::RunFinished {
                total_input_tokens,
                total_output_tokens,
                duration_ms,
            } => {
                if self.streaming_text {
                    println!();
                }
                if *total_input_tokens > 0 || *total_output_tokens > 0 {
                    eprintln!(
                        "{}",
                        format!(
                            "  [完成 · {duration_ms}ms · in={total_input_tokens} out={total_output_tokens}]"
                        )
                        .dimmed()
                    );
                }
                RendererAction::Done(std::mem::take(&mut self.accumulated_text))
            }
            EventPayload::RunFailed { error } => {
                if self.streaming_text {
                    println!();
                }
                eprintln!("{} {}", "错误:".red().bold(), error.message.red());
                RendererAction::Failed(error.message.clone())
            }
            EventPayload::RunCancelled => {
                if self.streaming_text {
                    println!();
                }
                eprintln!("{}", "[已取消]".dimmed());
                RendererAction::Cancelled
            }
            _ => RendererAction::Continue,
        }
    }
}

/// session 主循环消费 `RendererAction` 决定下一步：继续、问用户、或终止。
pub enum RendererAction {
    Continue,
    AwaitQuestion {
        request_id: PermissionRequestId,
        question: String,
        options: Vec<QuestionOption>,
    },
    Done(String),
    Failed(String),
    Cancelled,
}

fn summarize_input(input: &serde_json::Value) -> String {
    let s = input.to_string();
    if s.chars().count() > 80 {
        let snippet: String = s.chars().take(80).collect();
        format!("({snippet}…)")
    } else {
        format!("({s})")
    }
}

fn preview_result(result: &str) -> String {
    let single_line = result.replace('\n', " ");
    let trimmed = single_line.trim();
    if trimmed.chars().count() > 100 {
        let snippet: String = trimmed.chars().take(100).collect();
        format!("{snippet}…")
    } else {
        trimmed.to_string()
    }
}
