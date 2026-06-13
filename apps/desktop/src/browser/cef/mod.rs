//! CEF 承载运行时（架构 §8.5 M2，feature `cef-preview`）。
//!
//! 把内置浏览器的内核从系统 WKWebView（wry）换成 CEF（真 Chromium + 自带 CDP），
//! 让旁支工具的截图 / matched-rules 连的是**用户正看着的同一实例**（不再是 M1 的
//! attach 镜像）。eval 操作通道（PreviewAct/Style/Mutate）继续走 inspector 注入。
//!
//! 进程模型（macOS）：CEF 是多进程架构，主进程之外有 GPU / Renderer 等 helper 子进程。
//! - 主进程：`init_cef()` 在 Tauri 启动**之前**调，先 `execute_process` 判定自己是不是
//!   被 CEF 拉起的子进程（是则直接返回让 CEF 接管，不进 Tauri）；否则 `initialize` 全局
//!   CEF runtime。
//! - helper 子进程：独立 bin `hebbian-cef-helper`（见 bin/cef_helper.rs），只跑
//!   `execute_process`，不链接 Tauri / agent_core。
//!
//! 事件循环：CEF 用 `external_message_pump`，由 Tauri 的 `RunEvent::MainEventsCleared`
//! 每轮调 `do_message_loop_work()` 驱动（与 PoC 阶段 3 一致）。
//!
//! dev 模式（`pnpm tauri dev` 跑 target/debug 裸二进制，旁无 .app/Frameworks）：
//! 用 `HEBBIAN_CEF_DIR` + `HEBBIAN_CEF_HELPER` 显式指 framework / helper 路径
//! （PoC embed_dev 已验证）。release 模式靠 .app bundle 约定路径。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use cef::{
    api_hash, args::Args, execute_process, initialize, sys, App, CefString, ImplCommandLine,
    Settings,
};

mod app_handler;
mod browser;
mod client;

pub use browser::{CefBrowser, CefHost};
pub use cef::Rect;

/// CEF 是否已成功初始化（承载层据此决定走 CEF 还是 wry）。
static CEF_READY: AtomicBool = AtomicBool::new(false);

pub fn cef_ready() -> bool {
    CEF_READY.load(Ordering::Relaxed)
}

/// CEF 内嵌实例的 CDP 端口（127.0.0.1 only）。CdpBridge 连它即连用户看的同一实例。
pub const CEF_CDP_PORT: u16 = 9222;

/// 解析 framework 目录：dev 用 HEBBIAN_CEF_DIR，release 用 .app/Contents/Frameworks。
fn framework_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HEBBIAN_CEF_DIR") {
        return Some(PathBuf::from(dir));
    }
    // release：可执行文件在 .app/Contents/MacOS/，framework 在 ../Frameworks/
    let exe = std::env::current_exe().ok()?;
    let fw = exe.parent()?.parent()?.join("Frameworks");
    fw.is_dir().then_some(fw)
}

/// helper 子进程可执行路径：dev 用 HEBBIAN_CEF_HELPER，release 用 bundle 内 helper.app。
/// release 的 helper.app 由 cef-bundle.sh 组装（主默认 helper 名 "Hebbian Helper"）。
fn helper_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HEBBIAN_CEF_HELPER") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let frameworks = exe.parent()?.parent()?.join("Frameworks");
    let helper = frameworks.join("Hebbian Helper.app/Contents/MacOS/Hebbian Helper");
    helper.is_file().then_some(helper)
}

/// 在 Tauri 启动前调用。返回 `true` 表示当前进程是 CEF 子进程（已被 execute_process
/// 接管），调用方应立即退出、**不要**启动 Tauri。返回 `false` 表示是主进程且 CEF
/// 已初始化（或初始化失败、降级 wry）。
#[cfg(target_os = "macos")]
pub fn init_cef() -> bool {
    let Some(fw_dir) = framework_dir() else {
        tracing::warn!("CEF framework 目录未找到，降级 wry 预览");
        return false;
    };
    // dev 模式裸二进制：framework 不在 ../Frameworks，用绝对路径加载（绕过 cef 默认
    // LibraryLoader 的相对路径假设，PoC 已验证）。
    let fw_lib = fw_dir.join("Chromium Embedded Framework.framework/Chromium Embedded Framework");
    if let Ok(c) = std::ffi::CString::new(fw_lib.to_string_lossy().as_bytes()) {
        let ok = unsafe { cef::load_library(Some(&*c.as_ptr().cast())) };
        if ok != 1 {
            tracing::warn!(path = %fw_lib.display(), "CEF framework 加载失败，降级 wry 预览");
            return false;
        }
    }

    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let cmd = match args.as_cmd_line() {
        Some(c) => c,
        None => return false,
    };
    let is_browser_process = cmd.has_switch(Some(&CefString::from("type"))) != 1;
    let ret = execute_process(Some(args.as_main_args()), None::<&mut App>, std::ptr::null_mut());
    if !is_browser_process {
        // 子进程：CEF 已接管，直接让调用方退出，绝不进 Tauri。
        std::process::exit(if ret >= 0 { ret } else { 0 });
    }
    assert_eq!(ret, -1, "browser 进程不应被 execute_process 接管");

    let Some(helper) = helper_path() else {
        tracing::warn!("CEF helper 未找到，降级 wry 预览");
        return false;
    };

    let fw_framework = fw_dir.join("Chromium Embedded Framework.framework");
    let cache = agent_core::storage::default_data_dir().join("cef-cache");
    let settings = Settings {
        no_sandbox: 1,
        external_message_pump: 1,
        remote_debugging_port: CEF_CDP_PORT as i32,
        framework_dir_path: CefString::from(fw_framework.to_string_lossy().as_ref()),
        browser_subprocess_path: CefString::from(helper.to_string_lossy().as_ref()),
        resources_dir_path: CefString::from(
            fw_framework.join("Resources").to_string_lossy().as_ref(),
        ),
        root_cache_path: CefString::from(cache.to_string_lossy().as_ref()),
        ..Default::default()
    };

    let mut app = app_handler::HebCefApp::new();
    let ok = initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if ok == 1 {
        CEF_READY.store(true, Ordering::Relaxed);
        tracing::info!(port = CEF_CDP_PORT, "CEF runtime 初始化成功，预览走 CEF");
        true
    } else {
        tracing::warn!("CEF initialize 失败，降级 wry 预览");
        false
    }
}

#[cfg(not(target_os = "macos"))]
pub fn init_cef() -> bool {
    false
}

/// Tauri RunEvent::MainEventsCleared 每轮调，驱动 CEF 消息循环（external pump）。
pub fn pump() {
    if cef_ready() {
        cef::do_message_loop_work();
    }
}
