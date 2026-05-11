//! 把 `protocol::Event` 流渲染成有色彩的终端输出。
//!
//! 设计原则：流式可读、信息密度适中、不抢占用户视线。

use std::{
    collections::HashMap,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use colored::Colorize;
use protocol::{Event, EventPayload};
use terminal_spinners::{SpinnerData, STAR, STAR2};

/// 累积一次 turn 的渲染状态（跨 event）
pub struct TurnRenderer {
    streaming_text: bool,
    streaming_reasoning: bool,
    accumulated_text: String,
    run_spinner: Option<ToolSpinner>,
    tool_spinners: HashMap<String, ToolSpinner>,
    output_lock: Arc<Mutex<()>>,
}

impl TurnRenderer {
    pub fn new() -> Self {
        Self {
            streaming_text: false,
            streaming_reasoning: false,
            accumulated_text: String::new(),
            run_spinner: None,
            tool_spinners: HashMap::new(),
            output_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 渲染一个事件。终止事件（RunFinished / Failed / Cancelled）也由这里渲染。
    pub fn on_event(&mut self, event: &Event) {
        match &event.payload {
            EventPayload::RunStarted { .. } => self.start_run_spinner(),
            EventPayload::TurnStarted { turn, .. } => {
                if *turn > 0 {
                    println!();
                }
                self.start_run_spinner();
            }
            EventPayload::TextDelta { text } => {
                self.stop_run_spinner();
                if self.streaming_reasoning {
                    let _guard = self.output_lock.lock().ok();
                    println!();
                    self.streaming_reasoning = false;
                }
                let _guard = self.output_lock.lock().ok();
                print!("{}", text);
                io::stdout().flush().ok();
                self.accumulated_text.push_str(text);
                self.streaming_text = true;
            }
            EventPayload::Reasoning { text } => {
                self.stop_run_spinner();
                self.stop_tool_spinners();
                let _guard = self.output_lock.lock().ok();
                if !self.streaming_reasoning {
                    // 推理开始：先把 spinner 行清干净，再起一行
                    print!("\r\x1b[2K{}", "💭 ".dimmed());
                    self.streaming_reasoning = true;
                }
                // 推理段以淡色斜体展示，与正文留出视觉差异
                print!("{}", text.dimmed().italic());
                io::stdout().flush().ok();
            }
            EventPayload::TextDone { full_text } => {
                self.stop_run_spinner();
                self.flush_streaming_line();
                if !full_text.is_empty() && self.accumulated_text.is_empty() {
                    let _guard = self.output_lock.lock().ok();
                    println!("{}", full_text);
                    self.accumulated_text = full_text.clone();
                }
            }
            EventPayload::ToolCallDelta { index, name, .. } => {
                self.stop_run_spinner();
                let key = stream_tool_spinner_key(*index);
                if !self.tool_spinners.contains_key(&key) {
                    self.flush_streaming_line();
                    let tool_name = name.as_deref().unwrap_or("tool_call").to_string();
                    let spinner = ToolSpinner::start(
                        tool_name,
                        " preparing".to_string(),
                        Arc::clone(&self.output_lock),
                    );
                    self.tool_spinners.insert(key, spinner);
                }
            }
            EventPayload::ToolCallStarted {
                index,
                call_id,
                name,
                input,
                ..
            } => {
                self.stop_run_spinner();
                self.flush_streaming_line();
                let summary = summarize_input(input);
                if let Some(spinner) = self.tool_spinners.remove(&stream_tool_spinner_key(*index)) {
                    spinner.stop();
                }
                if let Some(spinner) = self.tool_spinners.remove(call_id) {
                    spinner.stop();
                }
                let spinner =
                    ToolSpinner::start(name.clone(), summary, Arc::clone(&self.output_lock));
                self.tool_spinners.insert(call_id.clone(), spinner);
            }
            EventPayload::ToolCallFinished {
                call_id,
                result,
                duration_ms,
                truncated,
                ..
            } => {
                if let Some(spinner) = self.tool_spinners.remove(call_id) {
                    spinner.stop();
                }
                let preview = preview_result(result);
                let suffix = if *truncated { " (截断)" } else { "" };
                let _guard = self.output_lock.lock().ok();
                println!(
                    "  {} {}ms · {}{}",
                    "↳".dimmed(),
                    duration_ms.to_string().dimmed(),
                    preview.dimmed(),
                    suffix.dimmed(),
                );
                drop(_guard);
                if self.tool_spinners.is_empty() {
                    self.start_run_spinner();
                }
            }
            EventPayload::PermissionRequested { summary, .. } => {
                self.stop_run_spinner();
                self.flush_streaming_line();
                let _guard = self.output_lock.lock().ok();
                println!("  {} {}", "⏸".yellow(), summary.yellow());
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
                let _guard = self.output_lock.lock().ok();
                println!("  {label}");
                drop(_guard);
                self.start_run_spinner();
            }
            EventPayload::PermissionAutoJudged {
                tool_name,
                decision,
                reason,
            } => {
                self.stop_run_spinner();
                self.flush_streaming_line();
                let _guard = self.output_lock.lock().ok();
                let dec_label = match decision.as_str() {
                    "allow" => "✓ AutoMode 自动放行".green().to_string(),
                    "deny" => "✗ AutoMode 拒绝".red().to_string(),
                    "ask" => "? AutoMode 转人工".yellow().to_string(),
                    other => format!("? AutoMode {other}").yellow().to_string(),
                };
                if let Some(r) = reason {
                    println!("  {dec_label}  [{tool_name}] {r}");
                } else {
                    println!("  {dec_label}  [{tool_name}]");
                }
                drop(_guard);
                self.start_run_spinner();
            }
            EventPayload::UserQuestionRequested { .. } => {
                self.flush_streaming_line();
            }
            EventPayload::UserQuestionAnswered { answer, .. } => {
                let label = match answer {
                    protocol::UserAnswer::Selected { label } => format!("✓ {label}"),
                    protocol::UserAnswer::SelectedMulti { labels } => {
                        format!("✓ 多选：{}", labels.join("、"))
                    }
                    protocol::UserAnswer::Custom { text } => format!("✓ 自由输入：{text}"),
                    protocol::UserAnswer::Cancelled => "✗ 用户取消".to_string(),
                };
                let _guard = self.output_lock.lock().ok();
                println!("  {}", label.green());
                drop(_guard);
                self.start_run_spinner();
            }
            EventPayload::ContextCompacted {
                before_tokens,
                after_tokens,
            } => {
                let _guard = self.output_lock.lock().ok();
                println!(
                    "  {}",
                    format!("[上下文压缩 {before_tokens}→{after_tokens} tokens]").dimmed()
                );
            }
            EventPayload::RunFinished {
                total_input_tokens,
                total_output_tokens,
                duration_ms,
                ..
            } => {
                self.stop_all_spinners();
                self.flush_streaming_line();
                if *total_input_tokens > 0 || *total_output_tokens > 0 {
                    let _guard = self.output_lock.lock().ok();
                    eprintln!(
                        "{}",
                        format!(
                            "  [完成 · {duration_ms}ms · in={total_input_tokens} out={total_output_tokens}]"
                        )
                        .dimmed()
                    );
                }
            }
            EventPayload::RunFailed { error } => {
                self.stop_all_spinners();
                self.flush_streaming_line();
                let _guard = self.output_lock.lock().ok();
                eprintln!("{} {}", "错误:".red().bold(), error.message.red());
            }
            EventPayload::RunCancelled => {
                self.stop_all_spinners();
                self.flush_streaming_line();
                let _guard = self.output_lock.lock().ok();
                eprintln!("{}", "[已取消]".dimmed());
            }
            _ => {}
        }
    }

    /// run 完成后取出累计的 assistant 文本。
    pub fn take_final_text(&mut self) -> String {
        std::mem::take(&mut self.accumulated_text)
    }

    fn flush_streaming_line(&mut self) {
        if self.streaming_text || self.streaming_reasoning {
            let _guard = self.output_lock.lock().ok();
            println!();
            self.streaming_text = false;
            self.streaming_reasoning = false;
        }
    }

    fn stop_all_spinners(&mut self) {
        self.stop_run_spinner();
        self.stop_tool_spinners();
    }

    fn stop_tool_spinners(&mut self) {
        for (_, spinner) in self.tool_spinners.drain() {
            spinner.stop();
        }
    }

    fn start_run_spinner(&mut self) {
        if self.run_spinner.is_none() {
            self.run_spinner = Some(ToolSpinner::start(
                "agent".to_string(),
                " thinking".to_string(),
                Arc::clone(&self.output_lock),
            ));
        }
    }

    fn stop_run_spinner(&mut self) {
        if let Some(spinner) = self.run_spinner.take() {
            spinner.stop();
        }
    }
}

impl Drop for TurnRenderer {
    fn drop(&mut self) {
        self.stop_run_spinner();
        self.stop_tool_spinners();
    }
}

struct ToolSpinner {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl ToolSpinner {
    fn start(name: String, summary: String, output_lock: Arc<Mutex<()>>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let spinner_stop = Arc::clone(&stop);
        let spinner = random_tool_spinner();
        let interval_ms = random_tool_interval_ms();

        let handle = thread::spawn(move || {
            let mut frame_index = 0usize;
            while !spinner_stop.load(Ordering::Relaxed) {
                {
                    let _guard = output_lock.lock().ok();
                    let frame = spinner.frames[frame_index % spinner.frames.len()];
                    print!(
                        "\r\x1b[2K{} {}{}",
                        frame.yellow(),
                        name.yellow().bold(),
                        summary.dimmed(),
                    );
                    io::stdout().flush().ok();
                }

                frame_index += 1;
                thread::sleep(Duration::from_millis(interval_ms));
            }

            let _guard = output_lock.lock().ok();
            print!("\r\x1b[2K");
            io::stdout().flush().ok();
        });

        Self { stop, handle }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

static RANDOM_COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_tool_spinner() -> &'static SpinnerData<'static> {
    if random_u64() % 2 == 0 {
        &STAR
    } else {
        &STAR2
    }
}

fn random_tool_interval_ms() -> u64 {
    100 + (random_u64() % 51)
}

fn random_u64() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let counter = RANDOM_COUNTER.fetch_add(1, Ordering::Relaxed);
    now ^ counter.rotate_left(17) ^ now.rotate_right(7)
}

fn stream_tool_spinner_key(index: usize) -> String {
    format!("stream:{index}")
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
