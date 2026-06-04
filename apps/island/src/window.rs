use crate::protocol::NotificationCard;
use crate::IslandState;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tracing::warn;

const CARD_W: f64 = 320.0;
const CARD_H: f64 = 100.0;
const MARGIN_TOP: f64 = 20.0;
const MARGIN_RIGHT: f64 = 20.0;
const GAP: f64 = 10.0;

/// 在屏幕右上角创建一个无边框通知窗口，按堆叠规则计算位置
pub fn spawn_notification_window(
    app: &AppHandle,
    id: &str,
    card: &NotificationCard,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = format!("island-{id}");
    let title = format!("Hebisland - {}", card.title);

    // 获取屏幕尺寸
    let (screen_w, _screen_h) = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let size = m.size();
            (size.width as f64, size.height as f64)
        })
        .unwrap_or((1920.0, 1080.0));

    // 右上角 x：屏幕右边距
    let x = screen_w - MARGIN_RIGHT - CARD_W;

    // y：根据当前堆叠中已有窗口数量计算
    let state = app.state::<IslandState>();
    let stack = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            state.window_stack.read().await.clone()
        })
    });
    let index = stack.len() as f64;
    let y = MARGIN_TOP + index * (CARD_H + GAP);

    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title(&title)
        .inner_size(CARD_W, CARD_H)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .build()?;

    // 将 id 加入堆叠
    let state = app.state::<IslandState>();
    let id_owned = id.to_string();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            state.window_stack.write().await.push(id_owned);
        })
    });

    // 将 card 数据通过 eval 推送到前端 CustomEvent
    let card_json = serde_json::to_string(card)?;
    let card_b64 = base64_encode(card_json.as_bytes());
    let js = format!(
        r#"(async () => {{
            const r = await fetch("data:application/octet-stream;base64,{card_b64}");
            const b = await r.blob();
            const t = new TextDecoder().decode(await b.arrayBuffer());
            window.dispatchEvent(new CustomEvent("island-init", {{ detail: JSON.parse(t) }}));
        }})()"#,
    );

    // 延迟推送，等前端 mount 完成
    let win = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Err(e) = win.eval(&js) {
            warn!("推送 card 数据失败: {e}");
        }
    });

    Ok(())
}

/// 关闭指定 id 的通知窗口，并重新排列剩余窗口
pub fn close_notification_window(app: &AppHandle, id: &str) {
    let label = format!("island-{id}");
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }

    // 从堆叠中移除并重新排列
    let state = app.state::<IslandState>();
    let id_owned = id.to_string();
    let app_clone = app.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let mut stack = state.window_stack.write().await;
            stack.retain(|x| x != &id_owned);
            // 重新排列剩余窗口的 y 坐标
            for (i, wid) in stack.iter().enumerate() {
                let lbl = format!("island-{wid}");
                if let Some(w) = app_clone.get_webview_window(&lbl) {
                    let y = MARGIN_TOP + i as f64 * (CARD_H + GAP);
                    let _ = w.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                        x: 0, // x 不变，仅调整 y
                        y: y as i32,
                    }));
                }
            }
        });
    });
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}