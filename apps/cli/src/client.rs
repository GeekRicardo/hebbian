//! IPC 客户端：向 daemon 的 Unix socket 发送一条命令并打印响应。

use anyhow::{anyhow, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::ipc::{IpcCommand, IpcResponse};

pub fn socket_path(session_id: &str) -> std::path::PathBuf {
    agent_core::storage::default_data_dir()
        .join("cli-sockets")
        .join(format!("{session_id}.sock"))
}

pub async fn send_command(session_id: &str, cmd: IpcCommand) -> Result<()> {
    let path = socket_path(session_id);
    let stream = UnixStream::connect(&path).await.map_err(|e| {
        anyhow!(
            "无法连接 daemon（session {session_id}）：{e}\n提示：先用 `heb new` 启动 daemon"
        )
    })?;

    let (reader, mut writer) = stream.into_split();
    let line = serde_json::to_string(&cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    let mut buf = BufReader::new(reader);
    let mut resp_line = String::new();
    buf.read_line(&mut resp_line).await?;

    let resp: IpcResponse = serde_json::from_str(resp_line.trim())
        .map_err(|e| anyhow!("daemon 响应解析失败：{e}"))?;

    if !resp.ok {
        return Err(anyhow!(
            "{}",
            resp.error.unwrap_or_else(|| "daemon 返回错误".into())
        ));
    }
    if let Some(data) = resp.data {
        println!("{}", serde_json::to_string_pretty(&data)?);
    }
    Ok(())
}
