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

pub use browser::{create_keepalive, CefBrowser, CefHost};
pub use cef::Rect;
pub use client::{NavCb, NavUpdate};

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
    let ret = execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );
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
        // CDP 端口：用 settings 字段（PoC embed_dev 验证过 Playwright 可连的方式）。
        // 命令行开关在 app_handler 也加了（双保险，只对 browser 进程）。
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
        // 诊断：CDP server 异步绑端口，延迟自检 9222 是否真的在监听 + 能否拿到 page。
        // 日志 target=cef，build 后看控制台/日志即可定位「检查通道连不上」卡在哪。
        std::thread::spawn(|| {
            for delay in [2u64, 5, 10] {
                std::thread::sleep(std::time::Duration::from_secs(delay));
                let addr = format!("127.0.0.1:{CEF_CDP_PORT}");
                match std::net::TcpStream::connect(&addr) {
                    Ok(_) => {
                        tracing::info!(target: "cef", port = CEF_CDP_PORT, "CDP 端口自检：TCP 可连 ✓");
                        return;
                    }
                    Err(e) => tracing::warn!(
                        target: "cef", port = CEF_CDP_PORT, delay, error = %e,
                        "CDP 端口自检：连不上（{delay}s 后重试）"
                    ),
                }
            }
            tracing::error!(
                target: "cef", port = CEF_CDP_PORT,
                "CDP 端口自检：10s 内始终连不上 → 检查/截图工具会降级。\
                 排查：是否子进程抢端口 / settings 与命令行开关冲突 / 端口被占"
            );
        });
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

/// Tauri RunEvent 每轮调，驱动 CEF 消息循环（external pump）。但 RunEvent 只在有
/// UI 事件时触发，app 空闲时不调 → CEF 消息循环停转、DevTools server 僵死、create
/// 回调不来。故另用 start_pump_loop 定时泵兜底，本函数保留给 RunEvent 顺带多泵几次。
pub fn pump() {
    if cef_ready() {
        cef::do_message_loop_work();
    }
}

/// 启动定时泵循环：后台线程每 ~8ms 经 run_on_main_thread 调度一次 do_message_loop_work
/// （CEF 要求在 UI 主线程泵）。不依赖 Tauri RunEvent 频率，保证 app 空闲时 CEF 消息
/// 循环仍持续转——DevTools server 才能响应、browser create 回调才会触发。
///
/// 首次泵几轮后（事件循环确认转起来），在主线程建 CDP keep-alive page——DevTools /json
/// server 需至少一个 target 才响应，预览懒创建启动无 page，靠它保活。`make_keepalive`
/// 由 mod 外提供（在主窗口 contentView 上建 1×1 隐藏 about:blank browser）。
#[cfg(target_os = "macos")]
pub fn start_pump_loop(app: tauri::AppHandle, make_keepalive: impl Fn() + Send + 'static) {
    if !cef_ready() {
        return;
    }
    std::thread::spawn(move || {
        let mut ticks: u32 = 0;
        let keepalive = std::sync::Arc::new(std::sync::Mutex::new(Some(make_keepalive)));
        tracing::info!(target: "cef", "pump loop 已启动");
        loop {
            std::thread::sleep(std::time::Duration::from_millis(8));
            ticks += 1;
            // 第 ~60 轮（~0.5s，事件循环已稳定转）在主线程建一次 keep-alive
            let ka = if ticks == 60 {
                keepalive.lock().unwrap().take()
            } else {
                None
            };
            if ticks == 60 {
                tracing::info!(target: "cef", "pump loop 第 60 轮，触发 keep-alive");
            }
            let _ = app.run_on_main_thread(move || {
                cef::do_message_loop_work();
                if let Some(f) = ka {
                    tracing::info!(target: "cef", "keep-alive 闭包在主线程执行");
                    f();
                }
            });
        }
    });
}

/// 在 Tauri RunEvent 回调里（主线程）直接泵 CEF + 适时建 keep-alive。
/// RunEvent 闭包本就在主线程跑，比 run_on_main_thread 投递可靠（不依赖队列消费）。
/// 缺点是 RunEvent 只在有事件时触发，空闲时不转——但 CEF DevTools server / 已建
/// browser 的渲染会持续产生事件，足够维持泵。keep-alive 在第 N 次 RunEvent 建（此时
/// 主窗口已就绪）。
#[cfg(target_os = "macos")]
pub fn pump_on_run_event(app: &tauri::AppHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    if !cef_ready() {
        return;
    }
    cef::do_message_loop_work();
    static TICKS: AtomicU32 = AtomicU32::new(0);
    let n = TICKS.fetch_add(1, Ordering::Relaxed);
    if n == 30 {
        // 第 30 次 RunEvent（主窗口 + 事件循环已稳定）建 keep-alive page
        tracing::info!(target: "cef", "RunEvent 第 30 次，建 keep-alive");
        use tauri::Manager;
        if let Some(window) = app.get_window("main") {
            if let Ok(view) = crate::browser::main_window_content_view_pub(&window) {
                create_keepalive(view);
            }
        }
    }
}
