use anyhow::{ensure, Context, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
#[cfg(target_os = "macos")]
use tauri::tray::{TrayIcon, TrayIconBuilder};
#[cfg(target_os = "macos")]
use tauri::ActivationPolicy;
use tauri::{AppHandle, Emitter, Manager, Window, WindowEvent};

const MAIN_WINDOW_LABEL: &str = "main";

#[cfg(target_os = "macos")]
const TRAY_ID: &str = "hebbian-status";
#[cfg(target_os = "macos")]
const TOGGLE_MAIN_WINDOW_MENU_ID: &str = "toggle_main_window";
#[cfg(target_os = "macos")]
const OPEN_SETTINGS_MENU_ID: &str = "open_settings";
#[cfg(target_os = "macos")]
const QUIT_MENU_ID: &str = "quit_app";
#[cfg(target_os = "macos")]
const TRAY_ANIMATION_FRAME_MILLIS: u64 = 120;
#[cfg(target_os = "macos")]
const TRAY_ANIMATION_FRAME_SIZE: u32 = 32;
#[cfg(target_os = "macos")]
const TRAY_ANIMATION_FRAME_BYTE_LEN: usize =
    (TRAY_ANIMATION_FRAME_SIZE as usize) * (TRAY_ANIMATION_FRAME_SIZE as usize) * 4;
#[cfg(target_os = "macos")]
const TRAY_ANIMATION_FRAMES: [&[u8]; 9] = [
    include_bytes!("../icons/tray-frame-01.rgba"),
    include_bytes!("../icons/tray-frame-02.rgba"),
    include_bytes!("../icons/tray-frame-03.rgba"),
    include_bytes!("../icons/tray-frame-04.rgba"),
    include_bytes!("../icons/tray-frame-05.rgba"),
    include_bytes!("../icons/tray-frame-06.rgba"),
    include_bytes!("../icons/tray-frame-07.rgba"),
    include_bytes!("../icons/tray-frame-08.rgba"),
    include_bytes!("../icons/tray-frame-09.rgba"),
];
#[cfg(target_os = "macos")]
const TRAY_ANIMATION_FRAME_COUNT: usize = TRAY_ANIMATION_FRAMES.len();

#[cfg(target_os = "macos")]
struct GlobalShortcutState {
    _manager: GlobalHotKeyManager,
    _hotkey: HotKey,
    _press_gate: Arc<Mutex<HotkeyPressGate>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainWindowAction {
    Show,
    Hide,
}

#[derive(Debug, Default)]
struct HotkeyPressGate {
    is_pressed: bool,
}

fn toggle_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::SUPER | Modifiers::CONTROL), Code::KeyJ)
}

pub fn toggle_action(is_visible: bool, is_minimized: bool, is_focused: bool) -> MainWindowAction {
    if !is_visible || is_minimized || !is_focused {
        MainWindowAction::Show
    } else {
        MainWindowAction::Hide
    }
}

/// 判断快捷键事件是否应触发窗口 toggle。
///
/// 只在首次 Pressed 时触发，防止长按重复触发。
/// Released 不触发——macOS 修饰键释放焦点问题由 show_main_window
/// 内部的 dispatch_after 延迟激活解决。
fn should_toggle_hotkey_event(gate: &mut HotkeyPressGate, event_id: u32, hotkey_id: u32, state: HotKeyState) -> bool {
    if event_id != hotkey_id {
        return false;
    }
    match state {
        HotKeyState::Pressed if !gate.is_pressed => {
            gate.is_pressed = true;
            true
        }
        HotKeyState::Pressed => false,
        HotKeyState::Released => {
            gate.is_pressed = false;
            false
        }
    }
}
pub fn initialize(app: &AppHandle) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        create_tray(app)?;
        if let Err(err) = register_global_shortcut(app) {
            eprintln!("Failed to register Cmd+Ctrl+J global shortcut: {err:#}");
        }
    }

    Ok(())
}

pub fn handle_window_event(window: &Window, event: &WindowEvent) {
    #[cfg(target_os = "macos")]
    {
        if window.label() != MAIN_WINDOW_LABEL {
            return;
        }

        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(err) = hide_main_window(window.app_handle()) {
                eprintln!("Failed to hide main window to tray: {err:#}");
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = (window, event);
}

fn toggle_main_window(app: &AppHandle) -> Result<()> {
    let action = toggle_action_for_app(app);
    match action {
        MainWindowAction::Show => show_main_window(app),
        MainWindowAction::Hide => hide_main_window(app),
    }
}

/// 根据窗口当前可见/聚焦状态判断应该 Show 还是 Hide。
fn toggle_action_for_app(app: &AppHandle) -> MainWindowAction {
    let window = match main_window(app) {
        Ok(w) => w,
        Err(_) => return MainWindowAction::Show,
    };
    let is_visible = window.is_visible().unwrap_or(false);
    let is_minimized = window.is_minimized().unwrap_or(false);
    let is_focused = is_visible && !is_minimized && window.is_focused().unwrap_or(false);
    toggle_action(is_visible, is_minimized, is_focused)
}

fn main_window(app: &AppHandle) -> Result<tauri::WebviewWindow> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .context("main window not found")
}

/// 从最小化 / 隐藏状态恢复到前台。
///
/// macOS 全局快捷键释放修饰键时，系统会把焦点还给之前的前台 app。
/// 即使在 Released 事件回调里执行激活，macOS 仍可能在回调之后处理修饰键释放
/// 并抢回焦点。解法：立即激活一次（窗口尽快出现），然后 dispatch_after 延迟
/// 150ms 再激活一次，确保在修饰键释放事件被完全消化后重新抢占焦点。
fn show_main_window(app: &AppHandle) -> Result<()> {
    let window = main_window(app)?;

    #[cfg(target_os = "macos")]
    apply_macos_tray_policy(app, true)?;

    window.unminimize()?;
    window.show()?;
    app.show()?;
    window.set_focus()?;

    // 通知前端把焦点切到 chat 输入框
    let _ = window.emit("hebbian://focus-chat-input", ());

    // macOS：延迟再次激活，确保修饰键释放后焦点不会被抢走。
    // dispatch_after_f 本身会 dispatch 到主线程，无需再包 run_on_main_thread���
    #[cfg(target_os = "macos")]
    activate_macos_app_delayed(app, std::time::Duration::from_millis(150));

    Ok(())
}

fn hide_main_window(app: &AppHandle) -> Result<()> {
    let window = main_window(app)?;
    window.hide()?;

    #[cfg(target_os = "macos")]
    apply_macos_tray_policy(app, false)?;

    Ok(())
}

/// macOS：延迟后激活 app 到前台并设置窗口焦点。
///
/// 使用 `dispatch_after` 在主线程 RunLoop 上延迟执行，
/// 确保在 macOS 修饰键释放事件被完全处理后再激活。
#[cfg(target_os = "macos")]
fn activate_macos_app_delayed(app: &AppHandle, delay: std::time::Duration) {
    let app_ptr = app as *const AppHandle as *mut std::ffi::c_void;
    let delay_nanos = (delay.as_secs_f64() * 1_000_000_000.0) as i64;

    unsafe {
        let when = dispatch_time(std::ptr::null(), delay_nanos);
        let queue = dispatch_get_main_queue();
        dispatch_after_f(when, queue, app_ptr, dispatch_after_activate);
    }
}

/// dispatch_after_f 的回调：激活 app 并聚焦窗口。
///
/// Safety: context 必须是有效的 *const AppHandle 转换而来。
#[cfg(target_os = "macos")]
extern "C" fn dispatch_after_activate(ctx: *mut std::ffi::c_void) {
    let app = unsafe { &*(ctx as *const AppHandle) };
    let _ = app.show();
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.set_focus();
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn dispatch_time(when: *const std::ffi::c_void, nsec: i64) -> *const std::ffi::c_void;
    fn dispatch_get_main_queue() -> *const std::ffi::c_void;
    fn dispatch_after_f(
        when: *const std::ffi::c_void,
        queue: *const std::ffi::c_void,
        context: *mut std::ffi::c_void,
        work: extern "C" fn(*mut std::ffi::c_void),
    );
}
#[cfg(target_os = "macos")]
fn create_tray(app: &AppHandle) -> Result<()> {
    let toggle_item = MenuItem::with_id(
        app,
        TOGGLE_MAIN_WINDOW_MENU_ID,
        "显示/隐藏窗口",
        true,
        None::<&str>,
    )?;
    let settings_item = MenuItem::with_id(
        app,
        OPEN_SETTINGS_MENU_ID,
        "设置…",
        true,
        Some("CmdOrCtrl+,"),
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, QUIT_MENU_ID, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle_item,
            &settings_item,
            &separator,
            &separator2,
            &quit_item,
        ],
    )?;

    let icon = tray_animation_frame(0).or_else(|err| {
        eprintln!("Failed to load animated tray icon, falling back to default: {err:#}");
        app.default_window_icon()
            .cloned()
            .context("default window icon unavailable")
    })?;

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Hebbian")
        .menu(&menu)
        .on_menu_event(|app, event| handle_tray_menu_event(app, &event.id.0))
        .show_menu_on_left_click(true)
        .build(app)?;
    if let Err(err) = start_tray_icon_animation(tray) {
        eprintln!("Failed to start animated tray icon: {err:#}");
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn tray_animation_frame_index(tick: usize) -> usize {
    tick % TRAY_ANIMATION_FRAME_COUNT
}

#[cfg(target_os = "macos")]
fn tray_animation_frame(tick: usize) -> Result<tauri::image::Image<'static>> {
    let frame = TRAY_ANIMATION_FRAMES[tray_animation_frame_index(tick)];
    ensure!(
        frame.len() == TRAY_ANIMATION_FRAME_BYTE_LEN,
        "tray animation frame has {} bytes, expected {}",
        frame.len(),
        TRAY_ANIMATION_FRAME_BYTE_LEN
    );
    Ok(
        tauri::image::Image::new(frame, TRAY_ANIMATION_FRAME_SIZE, TRAY_ANIMATION_FRAME_SIZE)
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn start_tray_icon_animation(tray: TrayIcon) -> Result<()> {
    let app = tray.app_handle().clone();
    std::thread::Builder::new()
        .name("hebbian-tray-icon-animation".to_string())
        .spawn(move || {
            let mut tick = 1usize;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(
                    TRAY_ANIMATION_FRAME_MILLIS,
                ));

                let icon = match tray_animation_frame(tick) {
                    Ok(icon) => icon,
                    Err(err) => {
                        eprintln!("Failed to decode tray animation frame: {err:#}");
                        tick = tick.wrapping_add(1);
                        continue;
                    }
                };
                let tray = tray.clone();
                if let Err(err) = app.run_on_main_thread(move || {
                    if let Err(err) = tray.set_icon(Some(icon)) {
                        eprintln!("Failed to update tray icon frame: {err:#}");
                    }
                }) {
                    eprintln!("Failed to schedule tray icon frame update: {err:#}");
                }

                tick = tick.wrapping_add(1);
            }
        })
        .context("failed to spawn tray icon animation thread")?;

    Ok(())
}

#[cfg(target_os = "macos")]
fn handle_tray_menu_event(app: &AppHandle, event_id: &str) {
    match event_id {
        TOGGLE_MAIN_WINDOW_MENU_ID => {
            if let Err(err) = toggle_main_window(app) {
                eprintln!("Failed to toggle main window from tray: {err:#}");
            }
        }
        OPEN_SETTINGS_MENU_ID => {
            // 显示主窗口并通知前端打开设置面板
            if let Err(err) = show_main_window(app) {
                eprintln!("Failed to show main window: {err:#}");
            }
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                if let Err(err) = window.emit("hebbian://open-settings", ()) {
                    eprintln!("Failed to emit open-settings event: {err:#}");
                }
            }
        }
        QUIT_MENU_ID => app.exit(0),
        _ => {}
    }
}

#[cfg(target_os = "macos")]
fn register_global_shortcut(app: &AppHandle) -> Result<()> {
    let manager = GlobalHotKeyManager::new().context("failed to create global hotkey manager")?;
    let hotkey = toggle_hotkey();
    manager
        .register(hotkey)
        .with_context(|| format!("failed to register {}", hotkey.into_string()))?;

    let hotkey_id = hotkey.id();
    let app_handle = app.clone();
    let press_gate = Arc::new(Mutex::new(HotkeyPressGate::default()));
    let handler_press_gate = Arc::clone(&press_gate);
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if handler_press_gate
            .lock()
            .map(|mut gate| should_toggle_hotkey_event(&mut gate, event.id, hotkey_id, event.state))
            .unwrap_or(false)
        {
            let toggle_handle = app_handle.clone();
            let _ = app_handle.run_on_main_thread(move || {
                if let Err(err) = toggle_main_window(&toggle_handle) {
                    eprintln!("Failed to toggle main window from hotkey: {err:#}");
                }
            });
        }
    }));

    app.manage(GlobalShortcutState {
        _manager: manager,
        _hotkey: hotkey,
        _press_gate: press_gate,
    });

    Ok(())
}

#[cfg(target_os = "macos")]
fn apply_macos_tray_policy(app: &AppHandle, dock_visible: bool) -> Result<()> {
    let policy = if dock_visible {
        ActivationPolicy::Regular
    } else {
        ActivationPolicy::Accessory
    };

    // 必须先设置 activation policy，再切换 dock 可见性。
    // 否则 macOS 在 Accessory→Regular 切换期间会短暂显示 exec 图标。
    app.set_activation_policy(policy)?;
    app.set_dock_visibility(dock_visible)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_ctrl_j_is_the_toggle_shortcut() {
        let hotkey = toggle_hotkey();

        assert_eq!(hotkey.key, Code::KeyJ);
        assert!(hotkey.mods.contains(Modifiers::SUPER));
        assert!(hotkey.mods.contains(Modifiers::CONTROL));
    }

    #[test]
    fn visible_focused_window_toggles_to_hide() {
        assert_eq!(toggle_action(true, false, true), MainWindowAction::Hide);
    }

    #[test]
    fn visible_unfocused_window_toggles_to_show() {
        assert_eq!(toggle_action(true, false, false), MainWindowAction::Show);
    }

    #[test]
    fn hidden_window_toggles_to_show() {
        assert_eq!(toggle_action(false, false, false), MainWindowAction::Show);
    }

    #[test]
    fn minimized_window_toggles_to_show() {
        assert_eq!(toggle_action(true, true, false), MainWindowAction::Show);
    }

    #[test]
    fn pressed_triggers_toggle() {
        let mut gate = HotkeyPressGate::default();
        assert!(should_toggle_hotkey_event(&mut gate, 42, 42, HotKeyState::Pressed));
    }

    #[test]
    fn released_does_not_trigger() {
        let mut gate = HotkeyPressGate::default();
        assert!(!should_toggle_hotkey_event(&mut gate, 42, 42, HotKeyState::Released));
    }

    #[test]
    fn mismatched_event_id_is_ignored() {
        let mut gate = HotkeyPressGate::default();
        assert!(!should_toggle_hotkey_event(&mut gate, 42, 7, HotKeyState::Pressed));
    }

    #[test]
    fn repeated_pressed_does_not_double_trigger() {
        let mut gate = HotkeyPressGate::default();
        assert!(should_toggle_hotkey_event(&mut gate, 42, 42, HotKeyState::Pressed));
        assert!(!should_toggle_hotkey_event(&mut gate, 42, 42, HotKeyState::Pressed));
        assert!(!should_toggle_hotkey_event(&mut gate, 42, 42, HotKeyState::Released));
        // 新 cycle
        assert!(should_toggle_hotkey_event(&mut gate, 42, 42, HotKeyState::Pressed));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn tray_animation_frame_index_wraps() {
        assert_eq!(tray_animation_frame_index(0), 0);
        assert_eq!(tray_animation_frame_index(TRAY_ANIMATION_FRAME_COUNT), 0);
        assert_eq!(
            tray_animation_frame_index(TRAY_ANIMATION_FRAME_COUNT + 1),
            1
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn tray_animation_frames_are_32px_rgba() {
        assert!(TRAY_ANIMATION_FRAMES
            .iter()
            .all(|frame| frame.len() == TRAY_ANIMATION_FRAME_BYTE_LEN));
    }
}
