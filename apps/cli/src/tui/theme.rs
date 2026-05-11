//! TUI 配色与样式（架构 §8.2）。
//!
//! 集中放在一处便于以后接 cli-settings.json 的 `tui.theme` 字段做热替换。

use ratatui::style::{Color, Modifier, Style};

pub fn user_prefix() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

pub fn assistant_text() -> Style {
    Style::default().fg(Color::White)
}

pub fn reasoning_text() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC)
}

pub fn tool_call() -> Style {
    Style::default().fg(Color::Green)
}

pub fn tool_failure() -> Style {
    Style::default().fg(Color::Red)
}

pub fn auto_judged_allow() -> Style {
    Style::default().fg(Color::Green)
}

pub fn auto_judged_deny() -> Style {
    Style::default().fg(Color::Red)
}

pub fn auto_judged_route() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn status_bar() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

pub fn popup_border() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn popup_title() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub fn hint() -> Style {
    Style::default().fg(Color::DarkGray)
}
