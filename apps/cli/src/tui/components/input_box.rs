//! 多行输入框。
//!
//! 简化实现：用 `String` 存当前 buffer，单光标位置（按 byte index）。Enter 提交，
//! Shift+Enter 在行末插入 `\n` 多行。Ctrl+U 清空。

use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};

use super::super::theme;

pub struct InputBox {
    buffer: String,
}

impl Default for InputBox {
    fn default() -> Self {
        Self {
            buffer: String::new(),
        }
    }
}

impl InputBox {
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn push_char(&mut self, c: char) {
        self.buffer.push(c);
    }

    pub fn pop_char(&mut self) {
        self.buffer.pop();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" 输入（Enter 发送，Shift+Enter 换行，Ctrl+U 清空，{}） ", "F2 切模式·F3 历史·Ctrl+C 退出");
        let block = Block::default().borders(Borders::ALL).title(title);
        let prompt = if self.buffer.is_empty() {
            Line::from(Span::styled("›", theme::user_prefix()))
        } else {
            Line::from(vec![
                Span::styled("› ", theme::user_prefix()),
                Span::raw(self.buffer.as_str()),
            ])
        };
        let para = Paragraph::new(prompt).block(block);
        frame.render_widget(para, area);
    }
}
