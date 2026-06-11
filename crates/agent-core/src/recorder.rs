//! 事件持久化：把 run 产生的 [`Event`] 流以 JSONL 格式异步追加写盘。
//!
//! Actor 模式：clone 成本只有一个 `Sender`，后台 writer task 异步落盘，
//! 主 loop 不被 IO 阻塞。
//!
//! 用法：
//! ```ignore
//! let recorder = Recorder::open(&path).await?;
//! session_config.recorder = Some(recorder);
//! // run 期间事件被双写：mpsc 给 surface + jsonl 给磁盘
//! ```
//!
//! 重放 / fork / rollback 直接读这份 jsonl 即可。

use std::path::{Path, PathBuf};

use protocol::Event;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

/// 内部命令。
enum RecorderCmd {
    Write(Event),
    Flush(oneshot::Sender<std::io::Result<()>>),
}

/// JSONL 事件持久化的句柄。Clone 是廉价的（只复制 `UnboundedSender`）。
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::UnboundedSender<RecorderCmd>,
    path: PathBuf,
}

impl Recorder {
    /// 打开（创建或追加）一份 jsonl 事件日志。父目录会自动创建。
    pub async fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let (tx, mut rx) = mpsc::unbounded_channel::<RecorderCmd>();
        let writer_path = path.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    RecorderCmd::Write(event) => match serde_json::to_string(&event) {
                        Ok(line) => {
                            if let Err(e) = file.write_all(line.as_bytes()).await {
                                warn!(error = %e, path = %writer_path.display(), "recorder write");
                                continue;
                            }
                            if let Err(e) = file.write_all(b"\n").await {
                                warn!(error = %e, path = %writer_path.display(), "recorder newline");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "recorder serialize");
                        }
                    },
                    RecorderCmd::Flush(reply) => {
                        let _ = reply.send(file.flush().await);
                    }
                }
            }
        });

        Ok(Self { tx, path })
    }

    /// 异步追加一个事件。失败被记入 trace，不向调用方传播——run loop 不应该
    /// 因为磁盘 IO 失败而崩。
    /// 使用 unbounded channel：落盘不应阻塞 run loop，但也不应丢事件——
    /// 后台 writer task 持文件锁追加写，背压由文件系统承担。
    pub fn write(&self, event: &Event) {
        if let Err(e) = self.tx.send(RecorderCmd::Write(event.clone())) {
            warn!(error = %e, "recorder channel closed, dropping event");
        }
    }

    /// 等待写队列排空到磁盘。run 结束时可选调用。
    pub async fn flush(&self) -> std::io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(RecorderCmd::Flush(tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "recorder closed"))?;
        rx.await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "recorder dropped"))?
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
