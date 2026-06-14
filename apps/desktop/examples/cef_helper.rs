//! CEF helper 子进程（架构 §8.5 M2，feature `cef-preview`）。
//!
//! CEF 多进程架构：browser 主进程之外的 GPU / Renderer / Plugin 等子进程都由这个
//! helper 可执行承担。它只跑 `execute_process` 把控制权交给 CEF，**不链接** Tauri /
//! agent_core——子进程跑业务逻辑会崩。主进程经 `Settings.browser_subprocess_path`
//! 指向本 bin（打成 helper.app bundle，macOS 要求子进程是 .app）。
//!
//! 无 cef-preview feature 时编译成一个空 main（占位，不会被调用）。

#[cfg(all(feature = "cef-preview", target_os = "macos"))]
fn main() {
    use cef::{api_hash, args::Args, execute_process, library_loader::LibraryLoader, sys, App};

    let args = Args::new();

    // framework 加载：dev 模式（裸 helper，旁无 .app/Frameworks）用 HEBBIAN_CEF_DIR
    // 绝对路径；release 模式 helper 在 helper.app/Contents/MacOS，framework 在
    // Hebbian.app/Contents/Frameworks（相对 ../../../.. ），用 LibraryLoader(helper=true)。
    if let Ok(dir) = std::env::var("HEBBIAN_CEF_DIR") {
        let fw = format!("{dir}/Chromium Embedded Framework.framework/Chromium Embedded Framework");
        if let Ok(c) = std::ffi::CString::new(fw) {
            let ok = unsafe { cef::load_library(Some(&*c.as_ptr().cast())) };
            assert_eq!(ok, 1, "CEF helper(dev) 加载 framework 失败");
        }
    } else {
        let loader = LibraryLoader::new(&std::env::current_exe().unwrap(), true);
        assert!(loader.load(), "CEF helper 加载 framework 失败");
    }

    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );
}

#[cfg(not(all(feature = "cef-preview", target_os = "macos")))]
fn main() {}
