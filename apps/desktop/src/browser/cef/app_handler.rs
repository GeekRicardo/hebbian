//! CEF App handler（架构 §8.5 M2）：命令行开关注入 + 进程级回调。
//!
//! `on_before_command_line_processing` 加 `no-startup-window`——external pump 模式下
//! Chrome runtime 默认开启动窗口会让 initialize 死锁（PoC 验证的核心坑）。

use cef::*;

wrap_app! {
    pub struct HebCefApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            command_line.append_switch(Some(&"no-startup-window".into()));
            command_line.append_switch(Some(&"noerrdialogs".into()));
            command_line.append_switch(Some(&"hide-crash-restore-bubble".into()));
            command_line.append_switch(Some(&"use-mock-keychain".into()));
        }
    }
}
