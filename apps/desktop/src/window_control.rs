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
use tauri::{AppHandle, Manager, Window, WindowEvent};

const MAIN_WINDOW_LABEL: &str = "main";

#[cfg(target_os = "macos")]
const TRAY_ID: &str = "hebbian-status";
#[cfg(target_os = "macos")]
const TOGGLE_MAIN_WINDOW_MENU_ID: &str = "toggle_main_window";
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

pub fn global_toggle_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::SUPER | Modifiers::CONTROL), Code::KeyJ)
}

pub fn toggle_action(is_visible: bool, is_minimized: bool, is_focused: bool) -> MainWindowAction {
    if !is_visible || is_minimized || !is_focused {
        MainWindowAction::Show
    } else {
        MainWindowAction::Hide
    }
}

fn should_toggle_hotkey_event(
    gate: &mut HotkeyPressGate,
    event_id: u32,
    hotkey_id: u32,
    state: HotKeyState,
) -> bool {
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
    let window = main_window(app)?;
    let is_visible = window.is_visible()?;
    let is_minimized = window.is_minimized()?;
    let is_focused = is_visible && !is_minimized && window.is_focused()?;
    let action = toggle_action(is_visible, is_minimized, is_focused);

    match action {
        MainWindowAction::Show => show_main_window(app),
        MainWindowAction::Hide => hide_main_window(app),
    }
}

fn main_window(app: &AppHandle) -> Result<tauri::WebviewWindow> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .context("main window not found")
}

fn show_main_window(app: &AppHandle) -> Result<()> {
    let window = main_window(app)?;

    #[cfg(target_os = "macos")]
    apply_macos_tray_policy(app, true)?;

    window.unminimize()?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

fn hide_main_window(app: &AppHandle) -> Result<()> {
    let window = main_window(app)?;
    window.hide()?;

    #[cfg(target_os = "macos")]
    apply_macos_tray_policy(app, false)?;

    Ok(())
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
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, QUIT_MENU_ID, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle_item, &separator, &quit_item])?;

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
        QUIT_MENU_ID => app.exit(0),
        _ => {}
    }
}

#[cfg(target_os = "macos")]
fn register_global_shortcut(app: &AppHandle) -> Result<()> {
    let manager = GlobalHotKeyManager::new().context("failed to create global hotkey manager")?;
    let hotkey = global_toggle_hotkey();
    manager
        .register(hotkey)
        .with_context(|| format!("failed to register {}", hotkey.into_string()))?;

    let hotkey_id = hotkey.id();
    let app_handle = app.clone();
    let press_gate = Arc::new(Mutex::new(HotkeyPressGate::default()));
    let handler_press_gate = Arc::clone(&press_gate);
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let should_toggle = handler_press_gate
            .lock()
            .map(|mut gate| should_toggle_hotkey_event(&mut gate, event.id, hotkey_id, event.state))
            .unwrap_or(false);

        if should_toggle {
            let app_handle = app_handle.clone();
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
        let hotkey = global_toggle_hotkey();

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
    fn only_matching_pressed_event_toggles() {
        let mut gate = HotkeyPressGate::default();

        assert!(should_toggle_hotkey_event(
            &mut gate,
            42,
            42,
            HotKeyState::Pressed
        ));
        assert!(!should_toggle_hotkey_event(
            &mut gate,
            42,
            7,
            HotKeyState::Pressed
        ));
        assert!(!should_toggle_hotkey_event(
            &mut gate,
            42,
            42,
            HotKeyState::Released
        ));
    }

    #[test]
    fn repeated_pressed_event_before_release_toggles_once() {
        let mut gate = HotkeyPressGate::default();

        assert!(should_toggle_hotkey_event(
            &mut gate,
            42,
            42,
            HotKeyState::Pressed
        ));
        assert!(!should_toggle_hotkey_event(
            &mut gate,
            42,
            42,
            HotKeyState::Pressed
        ));
        assert!(!should_toggle_hotkey_event(
            &mut gate,
            42,
            42,
            HotKeyState::Released
        ));
        assert!(should_toggle_hotkey_event(
            &mut gate,
            42,
            42,
            HotKeyState::Pressed
        ));
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
