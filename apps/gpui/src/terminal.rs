//! 内置终端的后端。
//!
//! ANSI 解析、屏幕网格、滚动回看全部交给 `alacritty_terminal`——手写 ANSI 状态机
//! 一定会在颜色、光标移动、清屏这些地方出错，一个会显示乱码的终端比没有终端更糟。
//! 这里只负责三件事：起 PTY、把输出泵进 Term、把 Term 的网格读成可渲染的行。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Color as AnsiColor;
use anyhow::Result;

/// 终端网格里的一段同色文本。渲染层按段拼行，避免一个字符一个元素。
#[derive(Debug, Clone)]
pub struct TermSpan {
    pub text: String,
    /// 前景色的 RGB。`None` = 用主题默认前景色。
    pub fg: Option<(u8, u8, u8)>,
    pub bold: bool,
}

/// 事件回调：alacritty 有输出 / 要响铃 / 退出时通知我们重绘。
#[derive(Clone)]
struct Proxy {
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Exit => self.exited.store(true, Ordering::Relaxed),
            // Wakeup / ColorRequest / Title 等都意味着屏幕可能变了。
            _ => self.dirty.store(true, Ordering::Relaxed),
        }
    }
}

/// 一个活着的终端会话。
pub struct TerminalSession {
    term: Arc<FairMutex<Term<Proxy>>>,
    sender: EventLoopSender,
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    /// 当前网格尺寸。用 Cell 是因为 resize 要能在渲染路径（只有 &self）里调——
    /// 面板宽度一变就得跟着改，不然回车换行的位置和看到的不一样。
    rows: std::cell::Cell<u16>,
    cols: std::cell::Cell<u16>,
}

impl TerminalSession {
    /// 在 `cwd` 起一个 shell。shell 取 `$SHELL`，没有就退回 `/bin/sh`。
    pub fn spawn(cwd: Option<std::path::PathBuf>, cols: u16, rows: u16) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut options = tty::Options {
            shell: Some(tty::Shell::new(shell, Vec::new())),
            ..Default::default()
        };
        options.working_directory = cwd;

        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 8,
            cell_height: 16,
        };

        let dirty = Arc::new(AtomicBool::new(true));
        let exited = Arc::new(AtomicBool::new(false));
        let proxy = Proxy {
            dirty: dirty.clone(),
            exited: exited.clone(),
        };

        let pty = tty::new(&options, window_size, 0)?;
        let config = Config::default();
        let term = Term::new(config, &TermSize { cols, rows }, proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)?;
        let sender = event_loop.channel();
        // spawn 后线程自己泵 PTY，我们只在渲染时读网格。
        let _ = event_loop.spawn();

        Ok(Self {
            term,
            sender,
            dirty,
            exited,
            rows: std::cell::Cell::new(rows),
            cols: std::cell::Cell::new(cols),
        })
    }

    pub fn cols(&self) -> u16 {
        self.cols.get()
    }

    pub fn rows(&self) -> u16 {
        self.rows.get()
    }

    /// 按面板实际大小调整网格。尺寸没变就什么都不做——
    /// 每帧无脑 resize 会让 shell 反复收到 SIGWINCH。
    pub fn resize(&self, cols: u16, rows: u16) {
        let cols = cols.max(20);
        let rows = rows.max(4);
        if self.cols.get() == cols && self.rows.get() == rows {
            return;
        }
        self.cols.set(cols);
        self.rows.set(rows);

        self.term
            .lock()
            .resize(TermSize { cols, rows });
        let _ = self.sender.send(Msg::Resize(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 8,
            cell_height: 16,
        }));
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// 当前选中的文本。没有选区时返回 None。
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// 有没有新输出。渲染循环用它决定要不要重绘。
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// 把用户输入写进 PTY。
    pub fn write(&self, bytes: impl Into<std::borrow::Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    /// 读出当前可见网格，按行、按同色段返回。
    pub fn visible_lines(&self) -> Vec<Vec<TermSpan>> {
        let term = self.term.lock();
        let grid = term.grid();
        let mut out = Vec::with_capacity(grid.screen_lines());

        for line in 0..grid.screen_lines() {
            let row = &grid[alacritty_terminal::index::Line(line as i32)];
            let mut spans: Vec<TermSpan> = Vec::new();
            for col in 0..grid.columns() {
                let cell = &row[alacritty_terminal::index::Column(col)];
                let fg = rgb_of(cell.fg);
                let bold = cell
                    .flags
                    .contains(alacritty_terminal::term::cell::Flags::BOLD);
                let ch = cell.c;
                match spans.last_mut() {
                    // 同色同粗细就接着上一段拼，避免一个字符一个元素。
                    Some(last) if last.fg == fg && last.bold == bold => last.text.push(ch),
                    _ => spans.push(TermSpan {
                        text: ch.to_string(),
                        fg,
                        bold,
                    }),
                }
            }
            // 行尾空白去掉，省得每行都拖一串空格。
            if let Some(last) = spans.last_mut() {
                let trimmed = last.text.trim_end().to_string();
                last.text = trimmed;
            }
            spans.retain(|s| !s.text.is_empty());
            out.push(spans);
        }
        out
    }
}

/// alacritty 的颜色枚举 → RGB。命名色与索引色都映射到 xterm 256 色板，
/// 与终端里 `ls --color` 之类的输出对得上。
fn rgb_of(color: AnsiColor) -> Option<(u8, u8, u8)> {
    match color {
        AnsiColor::Spec(rgb) => Some((rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) => Some(xterm_256(i)),
        AnsiColor::Named(named) => {
            let index = named as usize;
            if index < 16 {
                Some(xterm_256(index as u8))
            } else {
                // Foreground / Background / Cursor 这些语义色交给主题决定。
                None
            }
        }
    }
}

/// xterm 256 色板。0–15 是标准色，16–231 是 6×6×6 立方，232–255 是灰阶。
fn xterm_256(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
            (step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let v = (i - 232) * 10 + 8;
            (v, v, v)
        }
    }
}

/// 给 `Term::new` / `resize` 用的尺寸。
#[derive(Clone, Copy)]
struct TermSize {
    cols: u16,
    rows: u16,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_grayscale_are_in_range() {
        // 立方体起点与终点
        assert_eq!(xterm_256(16), (0, 0, 0));
        assert_eq!(xterm_256(231), (255, 255, 255));
        // 灰阶两端
        assert_eq!(xterm_256(232), (8, 8, 8));
        assert_eq!(xterm_256(255), (238, 238, 238));
    }

    #[test]
    fn named_semantic_colors_defer_to_theme() {
        // Foreground/Background 这类语义色不该硬编码成某个 RGB，
        // 否则深浅主题切换时终端文字会和背景撞色。
        assert_eq!(rgb_of(AnsiColor::Named(NamedColor::Foreground)), None);
        assert!(rgb_of(AnsiColor::Named(NamedColor::Red)).is_some());
    }

    use alacritty_terminal::vte::ansi::NamedColor;
}
