//! 聊天滚动视图（架构 §8.4 草图）。
//!
//! 每条记录是一个 `ChatBlock`：User / Assistant / Tool / AutoJudged / SystemNote。
//! 渲染时全部转 `Vec<Line>` 后用 `Paragraph::scroll` 走线。
//!
//! 流式输出：streaming 状态下最后一条 Assistant block 的 text 字段持续追加，
//! 同时 reasoning 单独累计；TextDone / ToolCallStarted 之类的边界事件由 App 接事件后调
//! `append_assistant_text` / `start_assistant_block` 等方法管理。

use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::super::theme;

#[derive(Debug, Clone)]
pub enum ChatBlock {
    User(String),
    Assistant {
        text: String,
        reasoning: String,
    },
    ToolCall {
        name: String,
        brief: String,
        status: ToolStatus,
    },
    AutoJudged {
        tool: String,
        decision: AutoDecision,
        reason: Option<String>,
    },
    Note(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDecision {
    Allow,
    Deny,
    Route,
}

pub struct ChatView {
    blocks: Vec<ChatBlock>,
    /// 滚动偏移（行单位，0 = 顶部，越大越往下）。`u16::MAX` 视为"贴底自动跟随"。
    scroll: u16,
    follow: bool,
}

impl Default for ChatView {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            scroll: 0,
            follow: true,
        }
    }
}

impl ChatView {
    pub fn push(&mut self, block: ChatBlock) {
        self.blocks.push(block);
    }

    pub fn blocks_mut(&mut self) -> &mut Vec<ChatBlock> {
        &mut self.blocks
    }

    pub fn last_mut(&mut self) -> Option<&mut ChatBlock> {
        self.blocks.last_mut()
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
        self.follow = false;
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn follow_bottom(&mut self) {
        self.follow = true;
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        // 算 scroll 时先借不可变拿 lines 长度（用 owned Lines 形式避免重复 clone）。
        let lines = self.render_lines();
        let total = lines.len() as u16;
        let inner_h = area.height.saturating_sub(2);
        if self.follow {
            self.scroll = total.saturating_sub(inner_h);
        }
        let scroll = self.scroll;

        let block = Block::default()
            .borders(Borders::ALL)
            .title("对话")
            .style(Style::default());

        let para = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        frame.render_widget(para, area);
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for b in &self.blocks {
            match b {
                ChatBlock::User(text) => {
                    out.push(Line::from(vec![
                        Span::styled("› ", theme::user_prefix()),
                        Span::raw(text.clone()),
                    ]));
                    out.push(Line::from(Span::raw(String::new())));
                }
                ChatBlock::Assistant { text, reasoning } => {
                    if !reasoning.is_empty() {
                        for ln in reasoning.lines() {
                            out.push(Line::from(Span::styled(
                                format!("  · {ln}"),
                                theme::reasoning_text(),
                            )));
                        }
                    }
                    for ln in text.lines() {
                        out.push(Line::from(Span::styled(
                            ln.to_string(),
                            theme::assistant_text(),
                        )));
                    }
                    if text.is_empty() && reasoning.is_empty() {
                        out.push(Line::from(Span::styled(
                            "…".to_string(),
                            theme::hint(),
                        )));
                    }
                    out.push(Line::from(Span::raw(String::new())));
                }
                ChatBlock::ToolCall {
                    name,
                    brief,
                    status,
                } => {
                    let (mark, style) = match status {
                        ToolStatus::Running => ("…", theme::tool_call()),
                        ToolStatus::Ok => ("✓", theme::tool_call()),
                        ToolStatus::Failed => ("✗", theme::tool_failure()),
                    };
                    let line = if brief.is_empty() {
                        format!("> {name}() {mark}")
                    } else {
                        format!("> {name}({brief}) {mark}")
                    };
                    out.push(Line::from(Span::styled(line, style)));
                }
                ChatBlock::AutoJudged {
                    tool,
                    decision,
                    reason,
                } => {
                    let (mark, style) = match decision {
                        AutoDecision::Allow => ("✓", theme::auto_judged_allow()),
                        AutoDecision::Deny => ("✗", theme::auto_judged_deny()),
                        AutoDecision::Route => ("?", theme::auto_judged_route()),
                    };
                    let action = match decision {
                        AutoDecision::Allow => "自动放行",
                        AutoDecision::Deny => "自动拒绝",
                        AutoDecision::Route => "转人工",
                    };
                    let mut s = format!("{mark} AutoMode {action} [{tool}]");
                    if let Some(r) = reason {
                        s.push_str("：");
                        s.push_str(r);
                    }
                    out.push(Line::from(Span::styled(s, style)));
                }
                ChatBlock::Note(s) => {
                    out.push(Line::from(Span::styled(s.clone(), theme::hint())));
                }
            }
        }
        out
    }
}
