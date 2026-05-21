//! IPC 客户端：向 daemon 的 Unix socket 发送一条命令并打印响应。

use anyhow::{anyhow, Result};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::ipc::{IpcCommand, IpcResponse};

pub fn sockets_dir() -> std::path::PathBuf {
    agent_core::storage::default_data_dir().join("cli-sockets")
}

pub fn socket_path(session_id: &str) -> std::path::PathBuf {
    sockets_dir().join(format!("{session_id}.sock"))
}

/// 扫 `~/.hebbian/cli-sockets/*.sock`，ping 每个 socket 测活，输出存活 session 列表。
///
/// 残留 socket（daemon 已死但文件未清）自动删除。
pub async fn list_sessions() -> Result<()> {
    let dir = sockets_dir();
    let mut alive = Vec::<serde_json::Value>::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("{}", serde_json::to_string_pretty(&json!({"sessions": []}))?);
            return Ok(());
        }
        Err(e) => return Err(anyhow!("读取 {} 失败：{e}", dir.display())),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        let Some(sid) = name.strip_suffix(".sock") else { continue };

        match ping_socket(sid).await {
            Ok(()) => alive.push(json!({"session_id": sid, "socket": path.display().to_string()})),
            Err(_) => {
                // 死 socket，清掉
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"sessions": alive}))?
    );
    Ok(())
}

async fn ping_socket(session_id: &str) -> Result<()> {
    let path = socket_path(session_id);
    let stream = UnixStream::connect(&path).await?;
    let (reader, mut writer) = stream.into_split();
    let line = serde_json::to_string(&IpcCommand::Ping)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    let mut buf = BufReader::new(reader);
    let mut resp_line = String::new();
    buf.read_line(&mut resp_line).await?;
    let resp: IpcResponse = serde_json::from_str(resp_line.trim())?;
    if !resp.ok {
        return Err(anyhow!("ping failed"));
    }
    Ok(())
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
