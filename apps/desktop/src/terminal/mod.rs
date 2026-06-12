//! 内置终端：全局单例 PTY 管理（架构 §8 内置终端）。
//!
//! 与内置浏览器（§8.5，session-scoped）刻意不同——终端是「我这台机器上的活儿」，
//! 跨会话长存，整个 app 一份 [`TerminalState`]，不按 session 路由。
//!
//! PTY 进程是单一真理源：sidebar 内嵌视图与 popout 独立窗口都是连到某个
//! `term_*` 的 xterm 视图。同一时刻只有一个视图活跃（[`ViewOwner`]），另一个让位，
//! 避免两个 xterm 各自 `fit()` 来回 resize 同一 PTY。
//!
//! 通信：
//!   - 前端 → Rust：`terminal_write` / `terminal_resize` 等 invoke 命令。
//!   - Rust → 前端：reader 线程把 PTY 输出按读 base64 后 emit `terminal://output`
//!     （全窗口广播，仅活跃视图实际渲染）；退出 emit `terminal://exit`；
//!     活跃视图变更 emit `terminal://view`。
//!
//! 不进 agent-core：终端不是 agent 能力，是窗口部件，与 browser 模块同层。

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const POPOUT_LABEL: &str = "terminal-popout";
/// scrollback ring buffer 上限：1 MiB raw 字节，足够回放重建一屏 + 历史。
const SCROLLBACK_CAP: usize = 1 << 20;

/// 当前活跃视图归属。popout 打开时切到 `Popout`，内嵌让位显示占位。
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ViewOwner {
    Embedded,
    Popout,
}

/// 单个子终端：一份 PTY + reader 线程共享的 scrollback / alive。
struct TerminalInstance {
    /// portable-pty 的 master 只保证 `Send` 不保证 `Sync`，包 Mutex 才能放进
    /// 全局 `Arc<TerminalInstance>` 跨线程共享。resize 时短暂上锁。
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    /// 与 reader 线程共享：线程 append，`terminal_attach` 读取回放。
    scrollback: Arc<Mutex<VecDeque<u8>>>,
    /// reader 线程读到 EOF / 进程退出时置 false。
    alive: Arc<AtomicBool>,
    cwd: String,
}

impl Drop for TerminalInstance {
    fn drop(&mut self) {
        // app 退出 / 关 tab：杀子进程，避免 orphan。master 一并 drop 会向
        // 子进程发 SIGHUP，双保险。
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// app 全局唯一（tauri manage），不按 session 路由——见模块注释。
#[derive(Default)]
pub struct TerminalState {
    terminals: Mutex<HashMap<String, Arc<TerminalInstance>>>,
    /// 子终端 tab 顺序（全局一份，内嵌与 popout 共享同一份顺序）。
    order: Mutex<Vec<String>>,
    counter: AtomicU64,
    active_view: Mutex<ViewOwner>,
}

impl TerminalState {
    fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("term_{n:03}")
    }
}

// Mutex<ViewOwner> 默认值：Embedded。
impl Default for ViewOwner {
    fn default() -> Self {
        ViewOwner::Embedded
    }
}

#[derive(Serialize, Clone)]
struct OutputEvent {
    id: String,
    data_b64: String,
}

#[derive(Serialize, Clone)]
struct ExitEvent {
    id: String,
    exit_code: Option<u32>,
}

#[derive(Serialize, Clone)]
struct ViewEvent {
    owner: ViewOwner,
}

#[derive(Serialize, Clone)]
pub struct TerminalMeta {
    id: String,
    cwd: String,
    alive: bool,
}

#[derive(Serialize, Clone)]
pub struct TerminalListResult {
    terminals: Vec<TerminalMeta>,
    order: Vec<String>,
    active_view: ViewOwner,
}

/// 解析用户的交互式 shell：优先 `$SHELL`，回退 `/bin/zsh`。
fn resolve_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

/// 当前进程 locale 是否已是 UTF-8。GUI 启动的 app 不继承 shell 的 locale，
/// 若不是 UTF-8，中文路径会被 shell 按字节转义成 `\M-^@` 乱码——兜底强设。
fn locale_is_utf8() -> bool {
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = std::env::var(key) {
            if v.to_ascii_uppercase().contains("UTF-8") || v.to_ascii_uppercase().contains("UTF8") {
                return true;
            }
        }
    }
    false
}

/// reader 线程：阻塞读 PTY，每次读到的 OS 缓冲块即时 base64 后 emit。
/// 阻塞 read 天然把高频小写汇聚成块（一次 read 最多 8 KiB），无需额外定时聚合；
/// 也避免「定时器没到、pending 卡在缓冲里」的竞态。
fn spawn_reader(
    app: AppHandle,
    id: String,
    mut reader: Box<dyn Read + Send>,
    scrollback: Arc<Mutex<VecDeque<u8>>>,
    alive: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF：shell 退出
                Ok(n) => {
                    let chunk = &buf[..n];
                    {
                        let mut sb = scrollback.lock().unwrap();
                        sb.extend(chunk.iter().copied());
                        if sb.len() > SCROLLBACK_CAP {
                            let drop = sb.len() - SCROLLBACK_CAP;
                            sb.drain(..drop);
                        }
                    }
                    let data_b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
                    let _ = app.emit(
                        "terminal://output",
                        OutputEvent {
                            id: id.clone(),
                            data_b64,
                        },
                    );
                }
                Err(_) => break,
            }
        }
        alive.store(false, Ordering::Relaxed);
        // 退出码此刻 child 可能尚未被 wait；前端只需知道「已退出」，code 尽力而为。
        let _ = app.emit(
            "terminal://exit",
            ExitEvent {
                id: id.clone(),
                exit_code: None,
            },
        );
    });
}

/// 新建子终端：openpty + spawn `$SHELL`，启动 reader 线程，返回 `term_*`。
#[tauri::command]
pub fn terminal_open(
    app: AppHandle,
    state: tauri::State<'_, TerminalState>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let pty_system = native_pty_system();
    let size = PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty_system.openpty(size).map_err(|e| e.to_string())?;

    let shell = resolve_shell();
    let workdir = cwd
        .filter(|c| !c.is_empty() && std::path::Path::new(c).is_dir())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".to_string());

    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(&workdir);
    cmd.env("TERM", "xterm-256color");
    if !locale_is_utf8() {
        cmd.env("LANG", "zh_CN.UTF-8");
    }

    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    // slave fd 留着会泄漏，spawn 后立即释放（master 保活即可）。
    drop(pair.slave);

    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;

    let id = state.next_id();
    let scrollback = Arc::new(Mutex::new(VecDeque::new()));
    let alive = Arc::new(AtomicBool::new(true));

    spawn_reader(
        app.clone(),
        id.clone(),
        reader,
        scrollback.clone(),
        alive.clone(),
    );

    let inst = Arc::new(TerminalInstance {
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        child: Mutex::new(child),
        scrollback,
        alive,
        cwd: workdir,
    });

    state.terminals.lock().unwrap().insert(id.clone(), inst);
    state.order.lock().unwrap().push(id.clone());
    Ok(id)
}

/// 把前端按键写进 PTY stdin。
#[tauri::command]
pub fn terminal_write(
    state: tauri::State<'_, TerminalState>,
    id: String,
    data: String,
) -> Result<(), String> {
    let inst = {
        let map = state.terminals.lock().unwrap();
        map.get(&id).cloned()
    };
    let inst = inst.ok_or_else(|| "终端不存在".to_string())?;
    let mut w = inst.writer.lock().unwrap();
    w.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

/// 同步 PTY 尺寸（前端 fit() 后调）。
#[tauri::command]
pub fn terminal_resize(
    state: tauri::State<'_, TerminalState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let inst = {
        let map = state.terminals.lock().unwrap();
        map.get(&id).cloned()
    };
    let inst = inst.ok_or_else(|| "终端不存在".to_string())?;
    let result = inst.master.lock().unwrap().resize(PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    });
    result.map_err(|e| e.to_string())
}

/// 关闭子终端：杀进程 + 从 map / order 摘除。
#[tauri::command]
pub fn terminal_close(state: tauri::State<'_, TerminalState>, id: String) -> Result<(), String> {
    // 从 map 取出后 drop → Drop impl 杀子进程。
    let _removed = state.terminals.lock().unwrap().remove(&id);
    state.order.lock().unwrap().retain(|x| x != &id);
    Ok(())
}

#[derive(Serialize, Clone)]
pub struct AttachResult {
    scrollback_b64: String,
    alive: bool,
}

/// 视图（重）挂载时拉 scrollback 回放（内嵌切回 / popout 新窗口 / webview reload 共用）。
#[tauri::command]
pub fn terminal_attach(
    state: tauri::State<'_, TerminalState>,
    id: String,
) -> Result<AttachResult, String> {
    let inst = {
        let map = state.terminals.lock().unwrap();
        map.get(&id).cloned()
    };
    let inst = inst.ok_or_else(|| "终端不存在".to_string())?;
    let bytes: Vec<u8> = inst.scrollback.lock().unwrap().iter().copied().collect();
    Ok(AttachResult {
        scrollback_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        alive: inst.alive.load(Ordering::Relaxed),
    })
}

/// 列出所有子终端 + 顺序 + 当前活跃视图（任一视图初始化 / reload 时重建状态）。
#[tauri::command]
pub fn terminal_list(state: tauri::State<'_, TerminalState>) -> TerminalListResult {
    let map = state.terminals.lock().unwrap();
    let order = state.order.lock().unwrap().clone();
    let terminals = order
        .iter()
        .filter_map(|id| {
            map.get(id).map(|inst| TerminalMeta {
                id: id.clone(),
                cwd: inst.cwd.clone(),
                alive: inst.alive.load(Ordering::Relaxed),
            })
        })
        .collect();
    TerminalListResult {
        terminals,
        order,
        active_view: *state.active_view.lock().unwrap(),
    }
}

/// 把终端弹成独立可缩放窗口：加载 hebbian 前端的 `?terminal-popout` surface，
/// 在新窗口里渲染 xterm 并 attach 现有 PTY（PTY 不重启、不丢 scrollback）。
#[tauri::command]
pub fn terminal_popout(
    app: AppHandle,
    state: tauri::State<'_, TerminalState>,
) -> Result<(), String> {
    // 已有先聚焦
    if let Some(w) = app.get_webview_window(POPOUT_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }

    let win = WebviewWindowBuilder::new(
        &app,
        POPOUT_LABEL,
        WebviewUrl::App("/?terminal-popout=1".into()),
    )
    .title("终端")
    .inner_size(900.0, 560.0)
    .min_inner_size(480.0, 240.0)
    .resizable(true)
    .build()
    .map_err(|e| e.to_string())?;

    *state.active_view.lock().unwrap() = ViewOwner::Popout;
    emit_view(&app, ViewOwner::Popout);

    // 窗口被 OS 关闭 → 活跃视图交还内嵌。
    let app_for_close = app.clone();
    win.on_window_event(move |event| {
        if matches!(
            event,
            WindowEvent::Destroyed | WindowEvent::CloseRequested { .. }
        ) {
            if let Some(st) = app_for_close.try_state::<TerminalState>() {
                *st.active_view.lock().unwrap() = ViewOwner::Embedded;
            }
            emit_view(&app_for_close, ViewOwner::Embedded);
        }
    });
    Ok(())
}

/// 收回 popout：关窗 + 活跃视图交还内嵌（窗口事件兜底同样会触发）。
#[tauri::command]
pub fn terminal_close_popout(
    app: AppHandle,
    state: tauri::State<'_, TerminalState>,
) -> Result<(), String> {
    *state.active_view.lock().unwrap() = ViewOwner::Embedded;
    if let Some(w) = app.get_webview_window(POPOUT_LABEL) {
        let _ = w.close();
    }
    emit_view(&app, ViewOwner::Embedded);
    Ok(())
}

fn emit_view(app: &AppHandle, owner: ViewOwner) {
    let _ = app.emit("terminal://view", ViewEvent { owner });
}
