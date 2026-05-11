//! Ask 工具的提问弹窗：题目 + 选项 + 自由输入。
//!
//! 按 1-9 数字键选选项；按 Tab 切到「自由输入」模式，再 Enter 提交；Esc 取消。

use protocol::{PermissionRequestId, QuestionOption, UserAnswer};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::super::theme;

pub struct QuestionPopupState {
    pub request_id: PermissionRequestId,
    pub question: String,
    pub options: Vec<QuestionOption>,
    /// `true` 时按 1-9 数字键叠加勾选，Enter 提交 SelectedMulti。
    pub multi: bool,
    /// 自由输入 buffer + 是否处于输入模式（Tab 切换）。
    pub input_buffer: String,
    pub free_input_mode: bool,
    /// 多选时已勾选的 label 列表。
    pub picked: Vec<String>,
}

pub fn render(state: &QuestionPopupState, frame: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        state.question.as_str(),
        theme::popup_title(),
    )));
    lines.push(Line::raw(""));
    for (i, opt) in state.options.iter().enumerate().take(9) {
        let mark = if state.multi && state.picked.contains(&opt.label) {
            "[x]"
        } else if state.multi {
            "[ ]"
        } else {
            ""
        };
        let line = format!("  ({}) {} {}", i + 1, mark, opt.label);
        let style = if state.free_input_mode {
            theme::hint()
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(line, style)));
        if !opt.description.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("      {}", opt.description),
                theme::hint(),
            )));
        }
    }
    lines.push(Line::raw(""));
    let input_label = if state.free_input_mode {
        format!("[自由输入] {}_", state.input_buffer)
    } else {
        format!("[Tab 切到自由输入] {}", state.input_buffer)
    };
    let input_style = if state.free_input_mode {
        theme::auto_judged_allow()
    } else {
        theme::hint()
    };
    lines.push(Line::from(Span::styled(input_label, input_style)));
    lines.push(Line::raw(""));
    let hint = if state.multi {
        "1-9 切换勾选，Enter 提交，Tab 自由输入，Esc 取消"
    } else {
        "1-9 选项，Enter 提交自由输入，Tab 切换模式，Esc 取消"
    };
    lines.push(Line::from(Span::styled(hint, theme::hint())));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::popup_border())
        .title(Span::styled("Agent 提问", theme::popup_title()));
    let para = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    frame.render_widget(para, popup);
}

/// 数字键 → 选项 index。
pub fn option_index_for_key(c: char) -> Option<usize> {
    c.to_digit(10).and_then(|d| {
        if (1..=9).contains(&d) {
            Some(d as usize - 1)
        } else {
            None
        }
    })
}

/// 提交逻辑：根据当前 popup state 形成 UserAnswer。free_input 优先。
pub fn build_answer(state: &QuestionPopupState) -> UserAnswer {
    if state.free_input_mode && !state.input_buffer.trim().is_empty() {
        return UserAnswer::Custom {
            text: state.input_buffer.trim().to_string(),
        };
    }
    if state.multi {
        if state.picked.is_empty() {
            UserAnswer::Cancelled
        } else {
            UserAnswer::SelectedMulti {
                labels: state.picked.clone(),
            }
        }
    } else if let Some(first) = state.picked.first() {
        UserAnswer::Selected {
            label: first.clone(),
        }
    } else {
        UserAnswer::Cancelled
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
