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
//   审批：{"msg_id":"perm-<id>","action":"allow|deny|...","checked":[...]}
//   问答：{"msg_id":"question-<id>","action":"submit","answer":{<UserAnswer>}}
//         {"msg_id":"question-<id>","action":"skip"}
//   其中 answer 直接是 protocol::UserAnswer 的 wire 形态，hitl 侧零翻译反序列化。
//
// 回传收到后调 `hitl::resolve_hitl_from_island` 落地审批决定，
// 或 `hitl::answer_question_from_island` 落地问答回答。

use serde::Serialize;
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

/// 一张推给 hebisland 的通知卡。
///
/// 用 serde 序列化而非手拼字符串：body / 选项 label 里只要带引号或换行，
/// 手拼就会破坏 JSON 让 Swift 端 JSONDecoder 整条丢弃（审批卡显示不出来的根因）。
#[derive(Serialize, Default)]
pub struct IslandCard {
    pub id: String,
    #[serde(rename = "cardType")]
    pub card_type: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 单题 question 卡的选项。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<IslandOption>,
    /// 单题 question 卡是否多选。
    #[serde(rename = "multiSelect", skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
    /// 多题 question 卡：每道子题。非空时 island 逐题渲染，body 留空。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<IslandQuestion>,
}

#[derive(Serialize)]
pub struct IslandOption {
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub desc: String,
}

#[derive(Serialize)]
pub struct IslandQuestion {
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub desc: String,
    pub options: Vec<IslandOption>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
}

impl IslandCard {
    /// 构造一张基础卡（id / 类型 / 标题 / 正文），可选字段走 `..Default::default()`。
    pub fn new(
        id: impl Into<String>,
        card_type: &str,
        title: &str,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            card_type: card_type.into(),
            title: title.into(),
            body: body.into(),
            ..Default::default()
        }
    }
}

/// 客户端句柄：clone 安全，内部只是一个 channel sender。
#[derive(Clone)]
pub struct HebislandClient {
    tx: mpsc::Sender<ClientMsg>,
}

impl HebislandClient {
    /// 推送一条通知。card.id 同时用作 msg_id（回传时映射回 request_id）。
    pub fn show(&self, card: IslandCard) {
        #[derive(Serialize)]
        struct Envelope<'a> {
            #[serde(rename = "type")]
            kind: &'a str,
            id: &'a str,
            card: &'a IslandCard,
        }
        let env = Envelope {
            kind: "show",
            id: &card.id,
            card: &card,
        };
        match serde_json::to_string(&env) {
            Ok(json) => {
                let _ = self.tx.send(ClientMsg::Show { json });
            }
            Err(e) => tracing::warn!(error = %e, "hebisland card 序列化失败，跳过推送"),
        }
    }

    /// 关闭一条通知。
    pub fn dismiss(&self, id: &str) {
        #[derive(Serialize)]
        struct Dismiss<'a> {
            #[serde(rename = "type")]
            kind: &'a str,
            id: &'a str,
        }
        if let Ok(json) = serde_json::to_string(&Dismiss {
            kind: "dismiss",
            id,
        }) {
            let _ = self.tx.send(ClientMsg::Dismiss { json });
        }
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

/// 连接 hebisland daemon；连不上则拉起随包内嵌的 daemon 再重试。
///
/// **必须用「能否 connect」判断 daemon 存活，而非 socket 文件是否存在**：
/// daemon 进程退出后 `~/.hebbian/island.sock` 文件会残留（stale），
/// 用 `path.exists()` 判断会误以为 daemon 还在，跳过拉起、connect 到死 socket 拿 ECONNREFUSED。
///
/// 拉起的是 `resource_dir/HebIsland.app/Contents/MacOS/hebisland`（daemon 自带单例，
/// 重复拉起安全）。找不到内嵌资源（如 dev 模式）或拉起后仍连不上 → 返回 None，通知静默跳过。
fn connect_or_spawn(app: &AppHandle, sock_path: &std::path::Path) -> Option<UnixStream> {
    if let Ok(s) = UnixStream::connect(sock_path) {
        return Some(s);
    }

    if !spawn_bundled_daemon(app) {
        return None;
    }

    // 轮询等 daemon 把 socket listen 起来，最多 ~2s。
    for _ in 0..40 {
        if let Ok(s) = UnixStream::connect(sock_path) {
            return Some(s);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// 拉起随 Hebbian.app 内嵌的 hebisland daemon。成功 spawn 返回 true。
///
/// release 包里 HebIsland.app 被 Tauri 放在 `resource_dir/HebIsland.app`，
/// 可执行文件在 `.../Contents/MacOS/hebisland`。找不到内嵌资源（如 dev 模式）返回 false。
fn spawn_bundled_daemon(app: &AppHandle) -> bool {
    use tauri::Manager;

    let Ok(resource_dir) = app.path().resource_dir() else {
        return false;
    };
    let bin = resource_dir
        .join("HebIsland.app")
        .join("Contents")
        .join("MacOS")
        .join("hebisland");
    if !bin.exists() {
        tracing::info!("未找到内嵌 hebisland（{}），跳过自动拉起", bin.display());
        return false;
    }

    match std::process::Command::new(&bin).arg("daemon").spawn() {
        Ok(_) => {
            tracing::info!("已拉起内嵌 hebisland daemon: {}", bin.display());
            true
        }
        Err(e) => {
            tracing::warn!("拉起 hebisland daemon 失败: {e}");
            false
        }
    }
}

fn client_loop(app: AppHandle, rx: mpsc::Receiver<ClientMsg>) {
    let sock_path = dirs::home_dir()
        .expect("无法获取 home 目录")
        .join(".hebbian")
        .join("island.sock");

    let mut stream = match connect_or_spawn(&app, &sock_path) {
        Some(s) => s,
        None => {
            tracing::warn!("hebisland daemon 未运行，通知将不弹出");
            for _ in rx {}
            return;
        }
    };

    tracing::info!("hebisland socket 已连接: {}", sock_path.display());

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

                        // 审批勾选的子命令索引（空 = 全选）。
                        let checked = v["checked"].as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_i64().map(|n| n as usize))
                                .collect::<Vec<_>>()
                        });

                        // msg_id 格式为 "perm-{request_id}" 或 "question-{request_id}"
                        let (prefix, request_id) = if let Some(rid) = msg_id.strip_prefix("perm-") {
                            ("perm", rid)
                        } else if let Some(rid) = msg_id.strip_prefix("question-") {
                            ("question", rid)
                        } else {
                            ("unknown", msg_id)
                        };

                        match prefix {
                            "perm" => {
                                crate::hitl::resolve_hitl_from_island(
                                    &reader_app,
                                    request_id,
                                    action,
                                    checked.as_deref(),
                                );
                            }
                            "question" => {
                                // answer 直接是 protocol::UserAnswer 的 wire JSON（island 自己
                                // 拼好真实 label / 多题 items），hitl 侧零翻译反序列化。
                                crate::hitl::answer_question_from_island(
                                    &reader_app,
                                    request_id,
                                    action,
                                    v.get("answer").cloned(),
                                );
                            }
                            _ => {
                                tracing::warn!(msg_id, action, "未知 msg_id 前缀");
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
        let mut buf = json.as_bytes().to_vec();
        buf.push(b'\n');
        if stream.write_all(&buf).is_err() {
            tracing::warn!("hebisland socket 写失败，停止推送 ({label})");
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::{UnixListener, UnixStream};

    /// 回归：daemon 退出后 socket 文件残留（stale），`path.exists()` 仍为真，
    /// 但 `connect` 必失败。这正是「审批不唤起 hebisland」的根因——
    /// 旧逻辑用 `!sock_path.exists()` 判断是否拉起 daemon，被 stale 文件骗过去跳过拉起，
    /// connect 到死 socket 拿 ECONNREFUSED 后永久躺平。必须用 connect 结果判断 daemon 存活。
    #[test]
    fn stale_socket_file_exists_but_connect_fails() {
        let sock = std::env::temp_dir().join(format!("heb-stale-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);

        // 起 listener 建出 socket 文件，再 drop 模拟 daemon 退出。
        let listener = UnixListener::bind(&sock).expect("bind");
        assert!(UnixStream::connect(&sock).is_ok(), "daemon 活着时应能连上");
        drop(listener);

        // socket 文件在 macOS 上不会随 listener drop 自动删除 → stale。
        assert!(sock.exists(), "stale socket 文件应残留（旧逻辑据此误判 daemon 还活着）");
        assert!(
            UnixStream::connect(&sock).is_err(),
            "stale socket connect 必须失败——connect 才是 daemon 存活的唯一可信判据"
        );

        let _ = std::fs::remove_file(&sock);
    }
}
