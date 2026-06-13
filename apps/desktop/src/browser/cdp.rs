//! 薄 CDP 客户端：实现 agent-core 的 `PreviewBridge`（架构 §8.5，spec：内置浏览器-CDP-能力）。
//!
//! 连接 Chromium 系预览实例的 DevTools 端口（M1：`HEBBIAN_PREVIEW_CDP` 指定的
//! attach 端口；M2：内嵌 CEF 自带端口）。只封装旁支工具用到的能力：
//! 截图（Page.captureScreenshot）、matched rules（CSS.getMatchedStylesForNode）、
//! eval（Runtime.evaluate）、元素包围盒（DOM.getBoxModel，局部截图用）。
//!
//! nodeId 只在产生它的连接内有效（CDP 语义），所以一次工具调用的全部命令
//! 必须走同一条 ws 连接——`CdpSession` 持连接、顺序发命令，调用结束即关。

use std::sync::atomic::{AtomicU64, Ordering};

use agent_core::preview_bridge::PreviewBridge;
use async_trait::async_trait;
use base64::Engine as _;
use common::{AppError, AppResult};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct CdpBridge {
    /// DevTools HTTP 端点，如 `http://127.0.0.1:9333`。
    endpoint: String,
}

/// 一次工具调用范围内的 CDP 会话：单 ws 连接，顺序请求-响应。
struct CdpSession {
    tx: SplitSink<Ws, WsMessage>,
    rx: SplitStream<Ws>,
}

impl CdpSession {
    async fn cmd(&mut self, method: &str, params: Value) -> AppResult<Value> {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "id": id, "method": method, "params": params });
        self.tx
            .send(WsMessage::Text(msg.to_string().into()))
            .await
            .map_err(|e| AppError::msg(format!("CDP 发送失败: {e}")))?;
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(10), self.rx.next())
                .await
                .map_err(|_| AppError::msg(format!("CDP {method} 超时")))?
                .ok_or_else(|| AppError::msg("CDP 连接中断"))?
                .map_err(|e| AppError::msg(format!("CDP 读取失败: {e}")))?;
            let WsMessage::Text(text) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                continue; // 跳过事件与他人响应
            }
            if let Some(err) = v.get("error") {
                return Err(AppError::msg(format!("CDP {method} 出错: {err}")));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// selector → nodeId（本连接内有效）。
    async fn query_node(&mut self, selector: &str) -> AppResult<i64> {
        self.cmd("DOM.enable", json!({})).await?;
        let doc = self.cmd("DOM.getDocument", json!({ "depth": 0 })).await?;
        let root = doc
            .pointer("/root/nodeId")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| AppError::msg("CDP getDocument 无 root"))?;
        let found = self
            .cmd(
                "DOM.querySelector",
                json!({ "nodeId": root, "selector": selector }),
            )
            .await?;
        found
            .get("nodeId")
            .and_then(|v| v.as_i64())
            .filter(|&id| id != 0)
            .ok_or_else(|| AppError::msg(format!("selector 没有匹配元素: {selector}")))
    }
}

impl CdpBridge {
    pub fn new(port: u16) -> Self {
        Self {
            endpoint: format!("http://127.0.0.1:{port}"),
        }
    }

    /// 从环境变量构造（M1 attach 模式）。未设置 → None，工具走降级提示。
    pub fn from_env() -> Option<Self> {
        std::env::var("HEBBIAN_PREVIEW_CDP")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .map(Self::new)
    }

    /// send_aside 注入用：Arc<dyn PreviewBridge> 形态。
    ///
    /// 优先级：CEF 内嵌端口（feature cef-preview 且 CEF 已就绪——此时 CDP 连的是
    /// 用户正看的同一实例，截图/查规则所见即所得）> 环境变量 HEBBIAN_PREVIEW_CDP
    /// （M1 attach 镜像模式）> None（工具走降级提示）。
    pub fn shared() -> Option<std::sync::Arc<dyn PreviewBridge>> {
        #[cfg(feature = "cef-preview")]
        {
            if super::cef::cef_ready() {
                return Some(std::sync::Arc::new(Self::new(super::cef::CEF_CDP_PORT))
                    as std::sync::Arc<dyn PreviewBridge>);
            }
        }
        Self::from_env().map(|b| std::sync::Arc::new(b) as std::sync::Arc<dyn PreviewBridge>)
    }

    async fn open(&self) -> AppResult<CdpSession> {
        let list: Value = get_json_local(&format!("{}/json/list", self.endpoint)).await?;
        let ws_url = list
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            })
            .and_then(|t| t.get("webSocketDebuggerUrl").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .ok_or_else(|| AppError::msg("预览 CDP 端口上没有可用页面"))?;
        let (ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| AppError::msg(format!("CDP 连接失败: {e}")))?;
        let (tx, rx) = ws.split();
        Ok(CdpSession { tx, rx })
    }
}

/// 极简 HTTP GET JSON（仅本地回环 DevTools 发现端点，不引 reqwest——
/// 架构红线：desktop 不直接依赖 reqwest）。
async fn get_json_local(url: &str) -> AppResult<Value> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| AppError::msg("仅支持 http 本地端点"))?;
        let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
        let mut stream = std::net::TcpStream::connect(hostport)
            .map_err(|e| AppError::msg(format!("CDP 端口连不上: {e}")))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        use std::io::{Read, Write};
        write!(
            stream,
            "GET /{path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n"
        )
        .map_err(|e| AppError::msg(format!("CDP 发现请求失败: {e}")))?;
        // Chrome 的 DevTools 端点不理会 Connection: close（连接保持开），
        // read_to_end 会一直等 EOF——必须按 Content-Length 精确读 body。
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let (headers_end, content_len) = loop {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| AppError::msg(format!("CDP 发现响应读取失败: {e}")))?;
            if n == 0 {
                return Err(AppError::msg("CDP 发现响应被截断"));
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..pos]);
                let len = headers
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .ok_or_else(|| AppError::msg("CDP 发现响应缺 Content-Length"))?;
                break (pos + 4, len);
            }
            if buf.len() > 64 * 1024 {
                return Err(AppError::msg("CDP 发现响应头超长"));
            }
        };
        while buf.len() < headers_end + content_len {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| AppError::msg(format!("CDP 发现响应读取失败: {e}")))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = &buf[headers_end..(headers_end + content_len).min(buf.len())];
        serde_json::from_slice(body)
            .map_err(|e| AppError::msg(format!("CDP 发现响应非 JSON: {e}")))
    })
    .await
    .map_err(|e| AppError::msg(format!("CDP 发现任务失败: {e}")))?
}

/// 把 getMatchedStylesForNode 的结果整理成模型可读文本：
/// 列出每条项目级规则的 selector + 声明（UA 默认样式略去）。
fn format_matched_rules(result: &Value, selector: &str) -> String {
    let mut out = vec![format!(
        "元素 {selector} 的生效 CSS 规则（后列的优先级更高）："
    )];
    if let Some(rules) = result.get("matchedCSSRules").and_then(|v| v.as_array()) {
        for entry in rules {
            let Some(rule) = entry.get("rule") else { continue };
            if rule.get("origin").and_then(|v| v.as_str()) == Some("user-agent") {
                continue; // UA 默认样式噪音大，模型要的是项目自己的规则
            }
            let sel = rule
                .pointer("/selectorList/text")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let decls: Vec<String> = rule
                .pointer("/style/cssProperties")
                .and_then(|v| v.as_array())
                .map(|props| {
                    props
                        .iter()
                        // text 字段仅源码声明有；省略它可滤掉 longhand 展开噪音
                        .filter(|p| p.get("text").is_some())
                        .filter_map(|p| {
                            let name = p.get("name")?.as_str()?;
                            let value = p.get("value")?.as_str()?;
                            Some(format!("{name}: {value}"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(format!("  {sel} {{ {} }}", decls.join("; ")));
        }
    }
    if let Some(inline) = result
        .pointer("/inlineStyle/cssText")
        .and_then(|v| v.as_str())
    {
        if !inline.trim().is_empty() {
            out.push(format!("  [inline style] {{ {} }}", inline.trim()));
        }
    }
    if out.len() == 1 {
        out.push("  （没有项目级规则命中，样式可能全部来自继承或 UA 默认）".to_string());
    }
    out.join("\n")
}

#[async_trait]
impl PreviewBridge for CdpBridge {
    async fn capture(&self, selector: Option<&str>) -> AppResult<Vec<u8>> {
        let mut s = self.open().await?;
        let mut params = json!({ "format": "png" });
        if let Some(sel) = selector {
            let node_id = s.query_node(sel).await?;
            // box model 取不到（display:none 等）就退整页截图
            if let Ok(bm) = s.cmd("DOM.getBoxModel", json!({ "nodeId": node_id })).await {
                if let Some(quad) = bm.pointer("/model/border").and_then(|v| v.as_array()) {
                    let xs: Vec<f64> = quad.iter().step_by(2).filter_map(|v| v.as_f64()).collect();
                    let ys: Vec<f64> =
                        quad.iter().skip(1).step_by(2).filter_map(|v| v.as_f64()).collect();
                    let x = xs.iter().cloned().fold(f64::MAX, f64::min);
                    let y = ys.iter().cloned().fold(f64::MAX, f64::min);
                    let w = xs.iter().cloned().fold(f64::MIN, f64::max) - x;
                    let h = ys.iter().cloned().fold(f64::MIN, f64::max) - y;
                    if w > 0.0 && h > 0.0 {
                        params["clip"] =
                            json!({ "x": x, "y": y, "width": w, "height": h, "scale": 1 });
                    }
                }
            }
        }
        let shot = s.cmd("Page.captureScreenshot", params).await?;
        let b64 = shot
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::msg("截图无数据"))?;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| AppError::msg(format!("截图解码失败: {e}")))
    }

    async fn matched_rules(&self, selector: &str) -> AppResult<String> {
        let mut s = self.open().await?;
        // CSS.enable 依赖 DOM agent 已启用，query_node 内部先 DOM.enable
        let node_id = s.query_node(selector).await?;
        s.cmd("CSS.enable", json!({})).await?;
        let matched = s
            .cmd("CSS.getMatchedStylesForNode", json!({ "nodeId": node_id }))
            .await?;
        Ok(format_matched_rules(&matched, selector))
    }

    async fn eval(&self, expression: &str) -> AppResult<String> {
        let mut s = self.open().await?;
        let r = s
            .cmd(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true }),
            )
            .await?;
        if let Some(desc) = r.pointer("/exceptionDetails/exception/description") {
            return Err(AppError::msg(format!("页面 JS 执行出错: {desc}")));
        }
        let value = r.pointer("/result/value").cloned().unwrap_or(Value::Null);
        Ok(match value {
            Value::String(s) => s,
            other => other.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_rules_formats_and_skips_ua() {
        let result = json!({
            "matchedCSSRules": [
                { "rule": { "origin": "user-agent", "selectorList": { "text": "div" },
                    "style": { "cssProperties": [] } } },
                { "rule": { "origin": "regular", "selectorList": { "text": ".btn" },
                    "style": { "cssProperties": [
                        { "name": "color", "value": "red", "text": "color: red;" },
                        { "name": "color", "value": "rgb(255,0,0)" }
                    ] } } }
            ],
            "inlineStyle": { "cssText": "font-size: 12px;" }
        });
        let s = format_matched_rules(&result, ".btn");
        assert!(s.contains(".btn { color: red }"), "实际: {s}");
        assert!(!s.contains("div"), "UA 规则应被过滤: {s}");
        assert!(s.contains("[inline style]"));
    }

    #[test]
    fn from_env_absent_is_none() {
        std::env::remove_var("HEBBIAN_PREVIEW_CDP");
        assert!(CdpBridge::from_env().is_none());
    }

    /// 现象级验证（需真实 Chromium）：
    /// chrome --headless --remote-debugging-port=9444 --user-data-dir=/tmp/x \
    ///   "data:text/html,<ul class='list'><li class='item'>A</li>...<style>.item{color:blue}</style>"
    /// 然后 cargo test -p hebbian --lib cdp -- --ignored
    #[tokio::test]
    #[ignore = "需要本机 9444 端口跑着测试页"]
    async fn live_bridge_against_real_chromium() {
        let bridge = CdpBridge::new(9444);
        let count = bridge
            .eval("document.querySelectorAll('.item').length")
            .await
            .unwrap();
        assert_eq!(count, "3", "应有 3 个 .item");
        let rules = bridge.matched_rules(".item").await.unwrap();
        assert!(rules.contains("color: blue"), "应看到 color: blue: {rules}");
        let full = bridge.capture(None).await.unwrap();
        assert!(full.len() > 1000, "整页截图太小: {}", full.len());
        // 局部截图：成功拿到 PNG 即可（尺寸关系受 headless 视口影响不稳定，不强求 < 整页）
        let part = bridge.capture(Some(".card-list")).await.unwrap();
        assert!(part.len() > 100, "局部截图应非空: {}", part.len());
        assert_eq!(&part[1..4], b"PNG", "应是合法 PNG");
        eprintln!("LIVE OK: rules {} chars, full {} B, part {} B", rules.len(), full.len(), part.len());
    }

    /// 端到端现象级验证（需真实 Chromium，同上测试页）：旁支工具经 PreviewBridge
    /// 拿到的是「真实回执」而非降级提示——这是 P1/P3 痛点真正被解的证据。
    /// 测试页含 .item ×3、.item.highlight 用 color:red !important 覆盖 .item 的 blue。
    #[tokio::test]
    #[ignore = "需要本机 9444 端口跑着测试页"]
    async fn live_tools_through_bridge() {
        use agent_core::tools::preview_capture::PreviewCaptureTool;
        use agent_core::tools::preview_inspect::PreviewInspectTool;
        use agent_core::tools::{Tool, ToolCtx};

        let bridge: std::sync::Arc<dyn PreviewBridge> = std::sync::Arc::new(CdpBridge::new(9444));

        // P1：查 .item.highlight 的生效规则，必须能看到 !important 覆盖关系——
        // 这正是"样式没生效"类问题模型过去瞎猜、现在能直接看到根因的能力。
        let inspect = PreviewInspectTool::new(Some(bridge.clone()));
        let rules = inspect
            .execute(serde_json::json!({ "selector": ".item.highlight", "what": "rules" }))
            .await
            .unwrap();
        assert!(rules.contains("color"), "应含 color 规则: {rules}");
        assert!(
            rules.contains("!important") || rules.contains("red"),
            "应看到 highlight 的覆盖规则: {rules}"
        );

        // P3：siblings 识别同构——sameStructureCount 应数出 .item 系列是一类，
        // 模型据此判断"改一个"应泛化成"改一类"。
        let sibs = inspect
            .execute(serde_json::json!({ "selector": ".item", "what": "siblings" }))
            .await
            .unwrap();
        assert!(sibs.contains("sameStructureCount"), "应含同构计数: {sibs}");
        assert!(sibs.contains("\"total\""), "应含兄弟总数: {sibs}");

        // 眼睛：截图工具产出真实图片附件（非降级提示）
        let capture = PreviewCaptureTool::new(Some(bridge));
        let out = capture
            .execute_rich(ToolCtx::noop(), serde_json::json!({ "selector": ".card-list" }))
            .await
            .unwrap();
        assert!(!out.is_error, "不应降级: {}", out.text);
        assert_eq!(out.attachments.len(), 1, "应有一张截图");

        eprintln!("LIVE TOOLS OK:\n--- rules ---\n{rules}\n--- siblings ---\n{sibs}");
    }
}

