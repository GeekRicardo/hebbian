// hebisland socket client
//
// 启动时建立一条到 `~/.hebbian/island.sock` 的持久连接，
// 后续所有通知推送和 action 回传都走这一条连接。
//
// 推送方向（Desktop → hebisland）：
//   {"type":"show","id":"...","card":{...}}
//   {"type":"dismiss","id":"..."}
//
// 回传方向（hebisland → Desktop）：
//   {"msg_id":"...","action":"allow|deny|open|dismiss"}
//
// 回传收到后直接调 `hitl::resolve_hitl_from_island` 落地审批决定。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use tauri::AppHandle;

enum ClientMsg {
    /// 推送一条通知到 hebisland。json 是完整序列化后的 {"type":"show",...}
    Show { json: String },
    /// 关闭一条通知
    Dismiss { json: String },
}

/// 客户端句柄：clone 安全，内部只是一个 channel sender。
#[derive(Clone)]
pub struct HebislandClient {
    tx: mpsc::Sender<ClientMsg>,
}

impl HebislandClient {
    /// 推送一条通知。id 同时用作 msg_id（回传时映射回 request_id）。
    /// card_type: "approval" | "info" | "question"
    pub fn push(
        &self,
        id: String,
        card_type: &str,
        title: &str,
        body: &str,
        session_id: Option<&str>,
    ) {
        let sid = match session_id {
            Some(s) => format!(r#","sessionId":"{}""#, s),
            None => String::new(),
        };
        let json = format!(
            r#"{{"type":"show","id":"{id}","card":{{"id":"{id}","cardType":"{card_type}","title":"{title}","body":"{body}"{sid}}}}}"#
        );
        let _ = self.tx.send(ClientMsg::Show { json });
    }

    /// 关闭一条通知。
    pub fn dismiss(&self, id: &str) {
        let json = format!(r#"{{"type":"dismiss","id":"{id}"}}"#);
        let _ = self.tx.send(ClientMsg::Dismiss { json });
    }
}

/// 启动时调用一次，建立 socket 连接并启动后台 IO 线程。
/// 返回 client handle 供 chat.rs 等调用。
pub fn init_hebisland_client(app: AppHandle) -> HebislandClient {
    let (tx, rx) = mpsc::channel::<ClientMsg>();

    let app_for_reader = app.clone();
    std::thread::spawn(move || {
        client_loop(app_for_reader, rx);
    });

    HebislandClient { tx }
}

fn client_loop(app: AppHandle, rx: mpsc::Receiver<ClientMsg>) {
    let sock_path = dirs::home_dir()
        .expect("无法获取 home 目录")
        .join(".hebbian")
        .join("island.sock");

    let mut stream = match UnixStream::connect(&sock_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("hebisland daemon 未运行 ({e})，通知将不弹出");
            // drain channel silently
            for _ in rx {}
            return;
        }
    };

    tracing::info!("hebisland socket 已连接: {}", sock_path.display());

    // 克隆 reader 端
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("hebisland try_clone 失败: {e}");
            return;
        }
    };

    // reader 线程：接收 action 回传 → 落地 HITL
    let reader_app = app.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(reader_stream);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                        let msg_id = v["msg_id"].as_str().unwrap_or("");
                        let action = v["action"].as_str().unwrap_or("");
                        tracing::info!(msg_id, action, "hebisland action 回传");
                        match action {
                            "allow" | "deny" => {
                                // msg_id 格式为 "perm-{request_id}"，取出 request_id
                                let request_id =
                                    msg_id.strip_prefix("perm-").unwrap_or(msg_id);
                                crate::hitl::resolve_hitl_from_island(
                                    &reader_app,
                                    request_id,
                                    action,
                                );
                            }
                            "dismiss" | "open" => {
                                // open/dismiss 不需要落地 HITL；用户手动关闭卡片或
                                // 点击打开主窗口，这里不发任何动作。
                            }
                            _ => {
                                tracing::warn!(action, "未知 action");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("hebisland socket 读错误: {e}");
                    break;
                }
            }
        }
    });

    // writer 循环
    for msg in rx {
        let (json, label) = match &msg {
            ClientMsg::Show { json } => (json.as_str(), "show"),
            ClientMsg::Dismiss { json } => (json.as_str(), "dismiss"),
        };
        // 每次写前先检查，socket 断了就退出
        let mut buf = json.as_bytes().to_vec();
        buf.push(b'\n');
        if stream.write_all(&buf).is_err() {
            tracing::warn!("hebisland socket 写失败，停止推送 ({label})");
            break;
        }
    }
}
