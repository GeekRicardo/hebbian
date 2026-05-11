//! 底部状态栏：provider·model / token usage / RunMode / step counter。

use ratatui::{prelude::*, widgets::Paragraph};

use super::super::theme;

pub struct StatusBarState {
    pub provider_display: String,
    pub used_tokens: u64,
    pub budget_tokens: u64,
    pub run_mode: String,
    pub model_step: u32,
    pub tool_step: u32,
}

pub fn render(state: &StatusBarState, frame: &mut Frame, area: Rect) {
    let used = state.used_tokens;
    let budget = state.budget_tokens.max(1);
    let pct = (used as f64 / budget as f64 * 100.0).clamp(0.0, 999.0) as u32;
    let text = format!(
        " {provider} ─ {used}/{budget} tokens ({pct}%) ─ {mode} ─ step m{ms}/t{ts} ",
        provider = state.provider_display,
        used = format_count(used),
        budget = format_count(budget),
        pct = pct,
        mode = state.run_mode,
        ms = state.model_step,
        ts = state.tool_step,
    );
    let para = Paragraph::new(Line::from(Span::styled(text, theme::status_bar())));
    frame.render_widget(para, area);
}

fn format_count(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}
