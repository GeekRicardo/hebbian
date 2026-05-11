//! 审批弹窗（HITL）。
//!
//! 用法：App 收到 `PermissionRequested` 后构造 `PermissionPopupState` 并存在 App.state；
//! 主循环把它当 popup 渲染。按键 a/b/c/d 选 + Esc 取消，App 把决定通过 `RunHandle.resolve_permission`
//! 回写 → popup 清空。

use protocol::{ApprovalDecision, PermissionKind, PermissionRequestId, PermissionScope};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::super::theme;

pub struct PermissionPopupState {
    pub request_id: PermissionRequestId,
    pub kind: PermissionKind,
    pub summary: String,
}

pub fn render(state: &PermissionPopupState, frame: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 35, area);
    frame.render_widget(Clear, popup);
    let title = match &state.kind {
        PermissionKind::ToolCall { tool_name, .. } => format!("权限审批：{tool_name}"),
        PermissionKind::PathAccess { tool_name, paths } => {
            format!("权限审批：{tool_name} 越界访问 {} 个路径", paths.len())
        }
        PermissionKind::Plan { .. } => "权限审批：执行计划".to_string(),
        PermissionKind::ContinueLongRun { iterations_used } => {
            format!("已运行 {iterations_used} 轮，是否继续")
        }
    };
    let mut lines: Vec<Line> = Vec::new();
    if !state.summary.is_empty() {
        for ln in state.summary.lines() {
            lines.push(Line::from(Span::raw(ln.to_string())));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(
        "(a) 仅本次允许",
        theme::auto_judged_allow(),
    )));
    lines.push(Line::from(Span::styled(
        "(b) 本对话不再询问（Session 级）",
        theme::auto_judged_allow(),
    )));
    lines.push(Line::from(Span::styled(
        "(c) 始终允许（Global 级）",
        theme::auto_judged_allow(),
    )));
    lines.push(Line::from(Span::styled(
        "(d) 拒绝",
        theme::auto_judged_deny(),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Esc 取消（视同拒绝）",
        theme::hint(),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::popup_border())
        .title(Span::styled(title, theme::popup_title()));
    let para = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    frame.render_widget(para, popup);
}

/// 按键 → 决策。
pub fn decision_for_key(c: char) -> Option<ApprovalDecision> {
    match c {
        'a' | 'A' => Some(ApprovalDecision::AllowOnce),
        'b' | 'B' => Some(ApprovalDecision::AllowAndRemember {
            scope: PermissionScope::Session,
            pattern: None,
        }),
        'c' | 'C' => Some(ApprovalDecision::AllowAndRemember {
            scope: PermissionScope::Global,
            pattern: None,
        }),
        'd' | 'D' => Some(ApprovalDecision::Deny),
        _ => None,
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
