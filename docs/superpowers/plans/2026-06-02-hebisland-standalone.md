# Hebisland 独立二进制实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 hebisland 从 desktop 嵌入模块重构为独立 Tauri 二进制 `apps/island`，通过 Unix socket IPC 接收通知/审批消息，每条通知一个无边框窗口。

**Architecture:** 独立 Tauri 应用，后端 Rust + tokio Unix socket 监听 `~/.hebbian/island.sock`，前端 React 单窗口单卡片。CLI 两个子命令：`hebisland daemon`（启动守护进程）和 `hebisland notify <json>`（发送通知）。审批决策通过 socket 回传给发起 surface，island 不直接读写 agent_core。

**Tech Stack:** Tauri 2.x, tokio (Unix socket + channels), React 19, Vite, TypeScript

**前置条件：** `docs/hebisland.md`（设计文档）已完成，`apps/desktop/frontend/src/desktop/ui/components/IslandApp.tsx` 和 `IslandCard.tsx`（前端样式）已完成。

---

## 文件结构

### Rust（`apps/island/src/`）
- `main.rs` — 二进制入口：解析 CLI，daemon 模式启动 Tauri app + socket，notify 模式发送消息
- `lib.rs` — Tauri 插件注册：manage 状态、注册 commands、setup socket listener
- `socket.rs` — Unix socket server：监听、解析、dispatch 到 channel
- `window.rs` — 窗口管理：创建/销毁通知窗口、屏幕定位
- `protocol.rs` — SocketMessage 枚举 + 序列化

### 前端（`apps/island/frontend/`）
- `index.html` — 入口 HTML
- `src/main.tsx` — React 入口
- `src/IslandApp.tsx` — 从 desktop 的 IslandApp 适配，单窗口单卡片模式
- `src/IslandCard.tsx` — 从 desktop 的 IslandCard 适配，Tauri invoke 改为本地命令
- `package.json` — 独立依赖
- `tsconfig.json`
- `vite.config.ts`

### 配置
- `apps/island/Cargo.toml` — Rust crate 配置
- `apps/island/tauri.conf.json` — Tauri 配置（transparent + decorations:false + 1x1 初始窗口）
- `apps/island/build.rs` — Tauri build script

---

## Task 1: 创建 crate 骨架

**Files:**
- Create: `apps/island/Cargo.toml`
- Create: `apps/island/build.rs`
- Create: `apps/island/tauri.conf.json`
- Create: `apps/island/src/main.rs`
- Create: `apps/island/src/lib.rs`
- Modify: `Cargo.toml`（workspace members）

- [ ] **Step 1.1: 创建 Cargo.toml**

```toml
[package]
name = "hebisland"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "hebisland"
path = "src/main.rs"

[lib]
name = "hebisland_lib"
crate-type = ["staticlib", "cdylib", "lib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["unstable"] }
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dirs = "6"
rand = "0.9"
chrono = { version = "0.4", features = ["serde"] }

[features]
custom-protocol = ["tauri/custom-protocol"]
```

- [ ] **Step 1.2: 创建 build.rs**

```rust
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 1.3: 创建 tauri.conf.json**

```json
{
  "$schema": "https://raw.githubusercontent.com/tauri-apps/tauri/dev/crates/tauri-cli/schema.json",
  "productName": "Hebisland",
  "version": "0.1.0",
  "identifier": "com.hebbian.island",
  "build": {
    "frontendDist": "frontend/dist",
    "devUrl": "http://localhost:1421",
    "beforeDevCommand": "pnpm dev",
    "beforeBuildCommand": "pnpm build"
  },
  "app": {
    "windows": [
      {
        "label": "island-init",
        "url": "index.html",
        "width": 1,
        "height": 1,
        "visible": false,
        "decorations": false,
        "transparent": true,
        "skipTaskbar": true
      }
    ],
    "security": {
      "csp": null
    }
  }
}
```

- [ ] **Step 1.4: 创建 main.rs 骨架**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hebisland", about = "无边框通知 / 审批浮窗")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动通知守护进程
    Daemon,
    /// 发送一条通知到正在运行的守护进程
    Notify {
        /// JSON 格式的 SocketMessage
        #[arg(long)]
        msg: String,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Daemon => {
            // 启动 Tauri app，socket listener 在 setup 中启动
            hebisland_lib::run();
        }
        Commands::Notify { msg } => {
            // 同步发送到 socket
            notify_sync(&msg);
        }
    }
}

fn notify_sync(msg: &str) {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let sock_path = dirs::home_dir()
        .unwrap()
        .join(".hebbian")
        .join("island.sock");
    match UnixStream::connect(&sock_path) {
        Ok(mut stream) => {
            let _ = stream.write_all(msg.as_bytes());
            let _ = stream.write_all(b"\n");
            println!("ok");
        }
        Err(e) => {
            eprintln!("无法连接到 hebisland daemon: {e}");
            eprintln!("请先运行: hebisland daemon");
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 1.5: 创建 lib.rs 骨架**

```rust
mod protocol;
mod socket;
mod window;

use protocol::SocketMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc, RwLock};

pub struct IslandState {
    pub notifications: Arc<RwLock<HashMap<String, protocol::NotificationCard>>>,
    pub action_tx: mpsc::UnboundedSender<ActionEvent>,
}

pub struct ActionEvent {
    pub msg_id: String,
    pub action: String,
}

#[tauri::command]
fn island_get_card(
    state: tauri::State<'_, IslandState>,
    id: String,
) -> Option<protocol::NotificationCard> {
    // 阻塞读取在 command 上下文中用 tokio 的 block_in_place
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            state.notifications.read().await.get(&id).cloned()
        })
    })
}

#[tauri::command]
fn island_action(
    state: tauri::State<'_, IslandState>,
    id: String,
    action: String,
) {
    let _ = state.action_tx.send(ActionEvent { msg_id: id, action });
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let rt = tokio::runtime::Runtime::new().expect("创建 tokio runtime");
    let _guard = rt.enter();

    let (action_tx, action_rx) = mpsc::unbounded_channel::<ActionEvent>();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(IslandState {
            notifications: Arc::new(RwLock::new(HashMap::new())),
            action_tx,
        })
        .invoke_handler(tauri::generate_handler![island_get_card, island_action])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            // socket listener 和 action handler 在 setup 中启动
            tokio::spawn(async move {
                socket::run_socket_listener(app_handle.clone(), action_rx).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 hebisland 失败");
}
```

- [ ] **Step 1.6: 创建空的 protocol.rs / socket.rs / window.rs**

```rust
// protocol.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SocketMessage {
    #[serde(rename = "show")]
    Show { id: String, card: NotificationCard },
    #[serde(rename = "action")]
    Action { id: String, action: String },
    #[serde(rename = "dismiss")]
    Dismiss { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationCard {
    pub id: String,
    #[serde(rename = "cardType")]
    pub card_type: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
}
```

```rust
// socket.rs
use crate::protocol::SocketMessage;
use crate::{ActionEvent, IslandState};
use crate::window;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::io::AsyncBufReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};

pub async fn run_socket_listener(
    app: AppHandle,
    mut action_rx: mpsc::UnboundedReceiver<ActionEvent>,
) {
    let sock_path = dirs::home_dir()
        .unwrap()
        .join(".hebbian")
        .join("island.sock");

    let _ = std::fs::remove_file(&sock_path);
    if let Some(parent) = sock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("绑定 island.sock 失败: {e}");
            return;
        }
    };

    tracing::info!("hebisland daemon 监听: {}", sock_path.display());

    // action handler: 收到前端 action → 关闭窗口 → 回传给 socket client
    // 暂时只关闭窗口，Phase 2 再加 socket 回传
    let app_for_action = app.clone();
    tokio::spawn(async move {
        while let Some(action) = action_rx.recv().await {
            let label = format!("island-{}", action.msg_id);
            if let Some(win) = app_for_action.get_webview_window(&label) {
                let _ = win.close();
            }
        }
    });

    // accept loop
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let app_clone = app.clone();
                tokio::spawn(async move {
                    handle_client(stream, app_clone).await;
                });
            }
            Err(e) => {
                tracing::error!("accept 失败: {e}");
            }
        }
    }
}

async fn handle_client(stream: UnixStream, app: AppHandle) {
    let reader = tokio::io::BufReader::new(stream);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line() {
        match serde_json::from_str::<SocketMessage>(&line) {
            Ok(SocketMessage::Show { id, card }) => {
                let state = app.state::<IslandState>();
                state.notifications.write().await.insert(id.clone(), card.clone());
                if let Err(e) = window::spawn_notification_window(&app, &id, &card) {
                    tracing::error!("创建窗口失败: {e}");
                }
            }
            Ok(SocketMessage::Dismiss { id }) => {
                let label = format!("island-{id}");
                if let Some(win) = app.get_webview_window(&label) {
                    let _ = win.close();
                }
            }
            Ok(SocketMessage::Action { id, action }) => {
                tracing::info!("收到 action: {id} -> {action}");
            }
            Err(e) => {
                tracing::warn!("解析 socket 消息失败: {e} | line: {line}");
            }
        }
    }
}
```

```rust
// window.rs
use crate::protocol::NotificationCard;
use rand::Rng;
use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder};

pub fn spawn_notification_window(
    app: &AppHandle,
    id: &str,
    card: &NotificationCard,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = format!("island-{id}");
    let title = format!("Hebisland - {}", card.title);

    // 临时写法：Phase 1 用固定位置，Phase 2 改为堆叠规则
    let mut rng = rand::rng();
    let x: f64 = rng.random_range(1200.0..1600.0);
    let y: f64 = rng.random_range(20.0..200.0);

    let _window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title(&title)
        .inner_size(360.0, 100.0)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .build()?;

    // 将 card 数据通过 eval 推送到前端
    let card_json = serde_json::to_string(card)?;
    let card_b64 = base64_encode(card_json.as_bytes());
    let js = format!(
        r#"(async () => {{
            const b = await (await fetch("data:application/octet-stream;base64,{}")).blob();
            const t = new TextDecoder().decode(await b.arrayBuffer());
            window.dispatchEvent(new CustomEvent("island-init", {{ detail: JSON.parse(t) }}));
        }})()"#,
        card_b64
    );
    // 等一小段时间让前端 mount
    let window = app.get_webview_window(&label).unwrap();
    let win_clone = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = win_clone.eval(&js);
    });

    Ok(())
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 63) as usize] as char);
        result.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 63) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
```

- [ ] **Step 1.7: 注册 workspace member**

修改 `Cargo.toml`，在 `[workspace] members` 中加入 `"apps/island"`。

- [ ] **Step 1.8: 验证编译**

```bash
cargo check -p hebisland
```

---

## Task 2: 创建前端

**Files:**
- Create: `apps/island/frontend/index.html`
- Create: `apps/island/frontend/package.json`
- Create: `apps/island/frontend/tsconfig.json`
- Create: `apps/island/frontend/vite.config.ts`
- Create: `apps/island/frontend/src/main.tsx`
- Create: `apps/island/frontend/src/IslandApp.tsx`
- Create: `apps/island/frontend/src/IslandCard.tsx`
- Create: `apps/island/frontend/src/island.css`
- Create: `apps/island/frontend/src/vite-env.d.ts`

- [ ] **Step 2.1: 创建 package.json**

```json
{
  "name": "hebisland-frontend",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "preview": "vite preview"
  },
  "dependencies": {
    "@tauri-apps/api": "^2",
    "react": "^19",
    "react-dom": "^19"
  },
  "devDependencies": {
    "@types/react": "^19",
    "@types/react-dom": "^19",
    "@vitejs/plugin-react": "^4",
    "typescript": "~5.8",
    "vite": "^7"
  }
}
```

- [ ] **Step 2.2: 创建 tsconfig.json**

```json
{
  "compilerOptions": {
    "target": "ES2021",
    "useDefineForClassFields": true,
    "lib": ["ES2021", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "isolatedModules": true,
    "moduleDetection": "force",
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "noUncheckedSideEffectImports": true
  },
  "include": ["src"]
}
```

- [ ] **Step 2.3: 创建 vite.config.ts**

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
  },
}));
```

- [ ] **Step 2.4: 创建 index.html**

```html
<!doctype html>
<html lang="zh">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Hebisland</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 2.5: 创建 vite-env.d.ts**

```ts
/// <reference types="vite/client" />
```

- [ ] **Step 2.6: 创建 island.css**

从 `docs/hebisland-design.html` 提取 CSS，做成独立的 island.css。核心内容：
- `:root` 变量（accent、surface、card radius、shadow 等）
- 全局 reset（transparent bg、no margin）
- `.island-card` 样式（compact / expanded 两态）
- `.glyph` 圆形 glyph
- 审批按钮（allow / deny）
- 过渡动画（mount / leave）

- [ ] **Step 2.7: 创建 IslandCard.tsx**

从 desktop 的 `IslandCard.tsx` 适配：
- 去掉 `onDismiss`/`onClick` props，改为调用 `invoke("island_action", { id, action })`
- `card` 数据结构适配 `protocol::NotificationCard`（id, cardType, title, body, sessionId）
- 审批（cardType=approval）：Allow / Deny / Open 三个按钮 → `invoke("island_action", { id, action: "allow"|"deny"|"open" })`
- 问题（cardType=question）：点击卡片 → `invoke("island_action", { id, action: "open" })`
- 完成（cardType=info）：点击 → `invoke("island_action", { id, action: "dismiss" })`
- compact 模式：100px 高，只显示 glyph + title；expanded 模式：显示 body + 按钮

- [ ] **Step 2.8: 创建 IslandApp.tsx**

从 desktop 的 `IslandApp.tsx` 简化：
- 去掉多卡片管理、`layoutMap`、`mounted`/`leaving` 状态
- 去掉 `invoke("get_window_type")` 和 `invoke("island_set_ready")`
- 初始化透明背景
- 监听 `island-init` 事件获取 card 数据
- 渲染单个 `<IslandCard />`
- compact 模式下点击展开，expanded 模式下点击外部折叠（保留 desktop 版的核心交互）

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import IslandCard from "./IslandCard";

export default function IslandApp() {
  const [card, setCard] = useState<any>(null);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.documentElement.style.background = "transparent";

    const handler = (e: Event) => {
      setCard((e as CustomEvent).detail);
    };
    window.addEventListener("island-init", handler);
    return () => window.removeEventListener("island-init", handler);
  }, []);

  if (!card) return null;

  return (
    <div
      style={{
        width: "100vw",
        height: "100vh",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "flex-end",
        background: "transparent",
      }}
    >
      <IslandCard
        card={card}
        expanded={expanded}
        onToggle={() => setExpanded((e) => !e)}
        onAction={(action: string) => {
          invoke("island_action", { id: card.id, action });
        }}
      />
    </div>
  );
}
```

- [ ] **Step 2.9: 创建 main.tsx**

```tsx
import React from "react";
import ReactDOM from "react-dom/client";
import IslandApp from "./IslandApp";
import "./island.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <IslandApp />
  </React.StrictMode>,
);
```

- [ ] **Step 2.10: 安装依赖并构建**

```bash
cd apps/island/frontend && pnpm install && pnpm build
```

---

## Task 3: 实现完整的 socket 回传

**Files:**
- Modify: `apps/island/src/socket.rs`
- Modify: `apps/island/src/lib.rs`

在 Phase 1 基础上，action 事件需要通过 socket 回传给发起方。实现方案：

1. `show` 消息携带一个可选的 `reply_to` 文件路径（phase 2 才需要，Phase 1 先忽略）
2. action 回传：通过 `tokio::sync::broadcast` 将 action 事件广播给所有连接的 client
3. client 在连接时保持长连接，持续监听 action 回传

- [ ] **Step 3.1: 扩展 IslandState**

加入 `action_broadcaster: broadcast::Sender<ActionEvent>`。

- [ ] **Step 3.2: 修改 handle_client**

client 连接后：
1. 读取入站消息行（show/action/dismiss）
2. 同时订阅 action broadcast
3. 收到 broadcast 的 action 时，写回给 client

- [ ] **Step 3.3: 验证端到端**

启动 daemon，用 `hebisland notify` 发一条 show 消息，确认窗口出现；点击审批按钮，确认 action 通过 socket 回传。

---

## Task 4: 堆叠规则与窗口生命周期

**Files:**
- Modify: `apps/island/src/window.rs`

- [ ] **Step 4.1: 实现屏幕定位**

参考 `hebisland.md §5`：
- 获取屏幕尺寸
- right_x = screen_w - margin_right - card_w
- 根据已有窗口列表计算下一个 y 坐标
- `top-right` 和 `bottom-right` 两区独立

- [ ] **Step 4.2: 窗口关闭后重新排列**

用 `app.get_webview_window(label)` 检查窗口存活状态，关闭后重新计算剩余窗口的 y 坐标。

- [ ] **Step 4.3: 窗口大小自适应**

初始创建用 compact 高度（100px），expanded 时调整窗口高度。

---

## Task 5: 清理 desktop 嵌入代码

**Files:**
- Modify: `apps/desktop/src/lib.rs` — 删除 island 相关 manage / invoke_handler / setup
- Modify: `apps/desktop/src/chat.rs` — 删除 island emit 调用，改为 socket client 发送
- Delete: `apps/desktop/src/island.rs`
- Modify: `apps/desktop/frontend/src/main.tsx` — 删除 `?island` 路由
- Modify: `apps/desktop/frontend/src/App.tsx` — 删除 IslandApp 导入
- Delete: `apps/desktop/frontend/src/desktop/ui/components/IslandApp.tsx`
- Delete: `apps/desktop/frontend/src/desktop/ui/components/IslandCard.tsx`

- [ ] **Step 5.1: desktop chat.rs 改用 socket client**

在 `spawn_engine_task` 中，原来调用 `island::emit_notification` 的地方改为通过 Unix socket 发送 `SocketMessage::Show`。

- [ ] **Step 5.2: 删除 island.rs**

- [ ] **Step 5.3: 清理 lib.rs 中的 island 注册**

- [ ] **Step 5.4: 清理前端 island 相关代码**

- [ ] **Step 5.5: 验证 desktop 编译**

```bash
cargo check -p hebbian-desktop
pnpm --filter hebbian-desktop exec tsc --noEmit
```

---

## Task 6: 更新文档

**Files:**
- Modify: `docs/hebisland.md` — 更新为实际实现的架构描述
- Modify: `docs/changelog.md` — 追加一条
- Modify: `docs/架构.md` — §13 决策表追加 hebisland 独立进程决策

- [ ] **Step 6.1: 更新 hebisland.md**

将文档中的架构描述从 "独立二进制 + socket IPC" 更新为与实现一致的细节：
- 文件结构（`apps/island/`）
- Socket 协议实际消息格式
- 窗口管理实际实现方式
- CLI 命令用法

- [ ] **Step 6.2: 更新 changelog.md**

- [ ] **Step 6.3: 更新架构.md §13**

---

## 验证清单

```bash
# 1. hebisland 编译
cargo check -p hebisland

# 2. 前端构建
cd apps/island/frontend && pnpm build

# 3. 启动 daemon
cargo run -p hebisland -- daemon &

# 4. 发送测试通知
cargo run -p hebisland -- notify --msg '{"type":"show","id":"test-1","card":{"id":"test-1","cardType":"info","title":"完成","body":"测试通知"}}'

# 5. 发送审批通知
cargo run -p hebisland -- notify --msg '{"type":"show","id":"test-2","card":{"id":"test-2","cardType":"approval","title":"工具审批","body":"允许执行 rm？","session_id":"abc"}}'

# 6. desktop 编译
cargo check -p hebbian-desktop
```
