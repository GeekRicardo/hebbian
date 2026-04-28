//! 把 `protocol::Event` 流渲染成有色彩的终端输出。
//!
//! 设计原则：流式可读、信息密度适中、不抢占用户视线。

use std::io::{self, Write};

use colored::Colorize;
use protocol::{Event, EventPayload};

/// 累积一次 turn 的渲染状态（跨 event）
pub struct TurnRenderer {
    /// 当前是否处在文本流（用于决定要不要在 ToolCall 前加换行）
    streaming_text: bool,
    /// 当前 turn 的最终文本（来自 TextDone 或累积的 TextDelta）
    accumulated_text: String,
}

impl TurnRenderer {
    pub fn new() -> Self {
        Self {
            streaming_text: false,
            accumulated_text: String::new(),
        }
    }

    /// 处理一个事件。返回 `Some(text)` 表示 turn 已结束，附带最终 assistant 文本。
    pub fn on_event(&mut self, event: &Event) -> Option<TurnEnd> {
        match &event.payload {
            EventPayload::RunStarted { .. } => {
                // 不显式打印开始；让首个 TextDelta 自己开场
                None
            }
            EventPayload::TurnStarted { turn, .. } => {
                if *turn > 0 {
                    // 多轮 tool call 之间的视觉分隔
                    println!();
                }
                None
            }
            EventPayload::TextDelta { text } => {
                print!("{}", text);
                io::stdout().flush().ok();
                self.accumulated_text.push_str(text);
                self.streaming_text = true;
                None
            }
            EventPayload::TextDone { full_text } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                // TextDone 通常只有非流式 provider 才发，stream 模式下 full_text 会与累积一致
                if !full_text.is_empty() && self.accumulated_text.is_empty() {
                    println!("{}", full_text);
                    self.accumulated_text = full_text.clone();
                }
                None
            }
            EventPayload::ToolCallStarted { name, input, .. } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                let summary = summarize_input(input);
                println!("{} {}{}", "🔧".yellow(), name.yellow().bold(), summary.dimmed());
                None
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
                None
            }
            EventPayload::PermissionRequested { summary, .. } => {
                if self.streaming_text {
                    println!();
                    self.streaming_text = false;
                }
                // 仅打印一行通知；具体审批由 main loop 自动处理或交互处理
                println!("  {} {}", "⏸".yellow(), summary.yellow());
                None
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
                None
            }
            EventPayload::ContextCompacted {
                before_tokens,
                after_tokens,
            } => {
                println!(
                    "  {}",
                    format!("[上下文压缩 {before_tokens}→{after_tokens} tokens]").dimmed()
                );
                None
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
                Some(TurnEnd::Done(std::mem::take(&mut self.accumulated_text)))
            }
            EventPayload::RunFailed { error } => {
                if self.streaming_text {
                    println!();
                }
                eprintln!("{} {}", "错误:".red().bold(), error.message.red());
                Some(TurnEnd::Failed(error.message.clone()))
            }
            EventPayload::RunCancelled => {
                if self.streaming_text {
                    println!();
                }
                eprintln!("{}", "[已取消]".dimmed());
                Some(TurnEnd::Cancelled)
            }
            _ => None,
        }
    }
}

pub enum TurnEnd {
    Done(String),
    Failed(String),
    Cancelled,
}

fn summarize_input(input: &serde_json::Value) -> String {
    let s = input.to_string();
    if s.len() > 80 {
        format!("({}…)", &s[..80])
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
