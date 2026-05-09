//! 后台 shell 注册表：让 [`super::bash::BashTool`] 在命令超时（或用户显式
//! `run_in_background`）时把进程转后台继续运行，由 [`super::bash_output::BashOutputTool`]
//! 按 task_id 增量查询，由 [`super::kill_shell::KillShellTool`] 主动终止。
//!
//! 设计要点：
//! - `BackgroundShells` 是进程级单例，在 [`super::default_tools`] 里构造一次后
//!   注入给上述三个工具，跨 session 共享。注册表上限 [`MAX_BACKGROUND_SHELLS`]
//!   个，超过时踢最老的已退出条目。
//! - 每个 `BackgroundShell` 持有一个 tail buffer：合并 stdout/stderr 后只保留
//!   尾部 [`MAX_TAIL_BYTES`] 字节，避免长跑命令吃光内存。读取按 byte 游标增量
//!   返回，模型一次拿不超过 [`READ_CHUNK_BYTES`] 字节。
//! - 进程 stdout/stderr 由后台 task 持续抽到 buffer；waiter task 用 select! 同时
//!   等 child 退出和 kill 信号——SIGKILL 与正常 wait 不会争 child 的可变借用。

use std::collections::VecDeque;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{oneshot, Notify};

/// tail buffer 上限（256 KiB）。超过后丢弃最早字节。
pub const MAX_TAIL_BYTES: usize = 256 * 1024;
/// 单次 BashOutput 返回上限（32 KiB），避免一次塞满 model context。
pub const READ_CHUNK_BYTES: usize = 32 * 1024;
/// 注册表保留的活跃条目上限。
pub const MAX_BACKGROUND_SHELLS: usize = 16;

#[derive(Debug, Clone)]
pub enum ShellState {
    Running,
    Exited { code: Option<i32> },
    Killed,
    Failed { error: String },
}

impl ShellState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, ShellState::Running)
    }

    pub fn label(&self) -> &'static str {
        match self {
            ShellState::Running => "running",
            ShellState::Exited { .. } => "exited",
            ShellState::Killed => "killed",
            ShellState::Failed { .. } => "failed",
        }
    }
}

pub struct BackgroundShell {
    pub task_id: String,
    pub command: String,
    pub cwd: String,
    pub started_at: Instant,
    inner: Mutex<ShellInner>,
    /// 输出/状态变化时唤醒等待方（BashOutput 的 wait_ms 阻塞、KillShell 等终态）。
    notify: Notify,
}

struct ShellInner {
    state: ShellState,
    total_bytes: u64,
    tail: VecDeque<u8>,
    /// 用户已读到的字节游标（绝对值，对齐 total_bytes）。
    read_cursor: u64,
    finished_at: Option<Instant>,
    /// kill 信号：waiter task 内的 oneshot 接收端等它，KillShell 取出 sender 发送。
    /// 用 oneshot 是因为 `Notify::notify_waiters` 在没人 awaiting 时会丢消息。
    kill_tx: Option<oneshot::Sender<()>>,
}

impl BackgroundShell {
    fn new(
        task_id: String,
        command: String,
        cwd: String,
        kill_tx: oneshot::Sender<()>,
    ) -> Self {
        Self {
            task_id,
            command,
            cwd,
            started_at: Instant::now(),
            inner: Mutex::new(ShellInner {
                state: ShellState::Running,
                total_bytes: 0,
                tail: VecDeque::new(),
                read_cursor: 0,
                finished_at: None,
                kill_tx: Some(kill_tx),
            }),
            notify: Notify::new(),
        }
    }

    pub fn state(&self) -> ShellState {
        self.inner.lock().expect("background shell mutex").state.clone()
    }

    fn append(&self, prefix: Option<&str>, bytes: &[u8]) {
        {
            let mut inner = self.inner.lock().expect("background shell mutex");
            if let Some(p) = prefix {
                inner.push(p.as_bytes());
            }
            inner.push(bytes);
            if !bytes.ends_with(b"\n") {
                inner.push(b"\n");
            }
        }
        self.notify.notify_waiters();
    }

    fn finish(&self, state: ShellState) {
        {
            let mut inner = self.inner.lock().expect("background shell mutex");
            // 已经是终态就保留首次记录，避免 waiter / kill 互踩
            if inner.state.is_terminal() {
                return;
            }
            inner.state = state;
            inner.finished_at = Some(Instant::now());
        }
        self.notify.notify_waiters();
    }

    /// 读取自上次以来未读的输出。`max_bytes` 控制单次返回上限。
    pub fn read_incremental(&self, max_bytes: usize) -> ReadOutput {
        let mut inner = self.inner.lock().expect("background shell mutex");
        let unread = inner.total_bytes.saturating_sub(inner.read_cursor);
        if unread == 0 {
            return ReadOutput {
                content: String::new(),
                state: inner.state.clone(),
                bytes_dropped: 0,
                total_bytes: inner.total_bytes,
            };
        }
        let tail_start = inner.total_bytes.saturating_sub(inner.tail.len() as u64);
        let bytes_dropped = tail_start.saturating_sub(inner.read_cursor);
        let effective_start = inner.read_cursor.max(tail_start);
        let want = unread.min(max_bytes as u64) as usize;
        let skip = (effective_start - tail_start) as usize;
        let take = want.min(inner.tail.len().saturating_sub(skip));
        let bytes: Vec<u8> = inner.tail.iter().skip(skip).take(take).copied().collect();
        let content = String::from_utf8_lossy(&bytes).into_owned();
        inner.read_cursor = effective_start + take as u64;
        ReadOutput {
            content,
            state: inner.state.clone(),
            bytes_dropped,
            total_bytes: inner.total_bytes,
        }
    }

    /// 等待新输出或状态变化（最多 wait_ms 毫秒）。返回时调用方应再 `read_incremental`。
    pub async fn wait_for_change(&self, wait_ms: u64) {
        if wait_ms == 0 {
            return;
        }
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(wait_ms),
            self.notify.notified(),
        )
        .await;
    }

    /// 等到进入终态。
    pub async fn wait_terminal(&self) {
        loop {
            if self.state().is_terminal() {
                return;
            }
            self.notify.notified().await;
        }
    }
}

impl ShellInner {
    fn push(&mut self, bytes: &[u8]) {
        self.total_bytes += bytes.len() as u64;
        self.tail.extend(bytes.iter().copied());
        if self.tail.len() > MAX_TAIL_BYTES {
            let drop = self.tail.len() - MAX_TAIL_BYTES;
            self.tail.drain(..drop);
        }
    }
}

pub struct ReadOutput {
    pub content: String,
    pub state: ShellState,
    /// 因 tail 容量被永久丢弃的字节数（让模型知道输出有间断）。
    pub bytes_dropped: u64,
    pub total_bytes: u64,
}

/// 进程级注册表。Clone 等价于持 Arc。
#[derive(Clone, Default)]
pub struct BackgroundShells {
    inner: Arc<Mutex<Inner>>,
    counter: Arc<AtomicU64>,
}

#[derive(Default)]
struct Inner {
    shells: Vec<Arc<BackgroundShell>>,
}

impl BackgroundShells {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("bash_{n:03}")
    }

    /// 注册一条后台 shell：拿走 child 的 stdout/stderr 流式读，
    /// 用 waiter task 等退出 / kill 信号。
    pub fn register(&self, command: String, cwd: String, mut child: Child) -> Arc<BackgroundShell> {
        let task_id = self.next_id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (kill_tx, kill_rx) = oneshot::channel();
        let shell = Arc::new(BackgroundShell::new(task_id, command, cwd, kill_tx));

        {
            let mut inner = self.inner.lock().expect("background shells mutex");
            if inner.shells.len() >= MAX_BACKGROUND_SHELLS {
                if let Some(idx) = inner
                    .shells
                    .iter()
                    .position(|s| s.state().is_terminal())
                {
                    inner.shells.remove(idx);
                }
            }
            inner.shells.push(shell.clone());
        }

        if let Some(stdout) = stdout {
            spawn_reader(shell.clone(), stdout, None);
        }
        if let Some(stderr) = stderr {
            spawn_reader(shell.clone(), stderr, Some("[stderr] "));
        }
        spawn_waiter(shell.clone(), child, kill_rx);

        shell
    }

    pub fn get(&self, task_id: &str) -> Option<Arc<BackgroundShell>> {
        self.inner
            .lock()
            .expect("background shells mutex")
            .shells
            .iter()
            .find(|s| s.task_id == task_id)
            .cloned()
    }

    /// 列出所有条目（最近的在前）。
    pub fn list(&self) -> Vec<Arc<BackgroundShell>> {
        let mut v = self
            .inner
            .lock()
            .expect("background shells mutex")
            .shells
            .clone();
        v.reverse();
        v
    }

    /// 发出 kill 信号；等到 shell 进入终态。
    pub async fn kill(&self, task_id: &str) -> Option<ShellState> {
        let shell = self.get(task_id)?;
        if shell.state().is_terminal() {
            return Some(shell.state());
        }
        let kill_tx = {
            let mut inner = shell.inner.lock().expect("background shell mutex");
            inner.kill_tx.take()
        };
        if let Some(tx) = kill_tx {
            let _ = tx.send(());
        }
        shell.wait_terminal().await;
        Some(shell.state())
    }
}

fn spawn_reader<R>(shell: Arc<BackgroundShell>, reader: R, prefix: Option<&'static str>)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut buf = BufReader::new(reader);
        let mut line = Vec::with_capacity(256);
        loop {
            line.clear();
            match buf.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.strip_suffix(b"\n").unwrap_or(&line[..]);
                    shell.append(prefix, trimmed);
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_waiter(shell: Arc<BackgroundShell>, mut child: Child, kill_rx: oneshot::Receiver<()>) {
    tokio::spawn(async move {
        tokio::select! {
            result = child.wait() => {
                let state = match result {
                    Ok(status) => exit_state(status),
                    Err(e) => ShellState::Failed { error: e.to_string() },
                };
                shell.finish(state);
            }
            _ = kill_rx => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                shell.finish(ShellState::Killed);
            }
        }
    });
}

fn exit_state(status: ExitStatus) -> ShellState {
    ShellState::Exited {
        code: status.code(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::process::Command;

    fn spawn_bash(cmd: &str) -> Child {
        Command::new("bash")
            .arg("-lc")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn captures_output_and_exits() {
        let shells = BackgroundShells::new();
        let child = spawn_bash("echo hello && echo world");
        let shell = shells.register("echo hello && echo world".into(), "/".into(), child);
        shell.wait_terminal().await;
        let out = shell.read_incremental(READ_CHUNK_BYTES);
        assert!(out.content.contains("hello"));
        assert!(out.content.contains("world"));
        assert!(matches!(out.state, ShellState::Exited { code: Some(0) }));
    }

    #[tokio::test]
    async fn read_is_incremental() {
        let shells = BackgroundShells::new();
        let child = spawn_bash("echo a; sleep 0.1; echo b");
        let shell = shells.register("...".into(), "/".into(), child);

        // 先等到至少有 a 出现
        for _ in 0..50 {
            let snapshot = shell.read_incremental(READ_CHUNK_BYTES);
            if snapshot.content.contains('a') {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // 再读一次拿到 b（不会重复返回 a）
        shell.wait_terminal().await;
        let after = shell.read_incremental(READ_CHUNK_BYTES);
        assert!(!after.content.contains('a'));
        assert!(after.content.contains('b'));
    }

    #[tokio::test]
    async fn kill_marks_killed() {
        let shells = BackgroundShells::new();
        let child = spawn_bash("sleep 30");
        let shell = shells.register("sleep 30".into(), "/".into(), child);
        let id = shell.task_id.clone();
        let state = shells.kill(&id).await.unwrap();
        assert!(matches!(state, ShellState::Killed));
    }

    #[tokio::test]
    async fn registry_caps_to_max_when_terminal() {
        let shells = BackgroundShells::new();
        for _ in 0..(MAX_BACKGROUND_SHELLS + 4) {
            let child = spawn_bash("true");
            let s = shells.register("true".into(), "/".into(), child);
            s.wait_terminal().await;
        }
        assert!(shells.list().len() <= MAX_BACKGROUND_SHELLS);
    }
}
