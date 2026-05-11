//! 远端 CoreClient（架构 §7.2）。
//!
//! **占位实现**——本期不打算实施。设计预期是 surface 通过 HTTP/SSE 与跨进程的 core
//! daemon 通信：
//!
//! - `submit` → `POST /api/op`
//! - `subscribe` → `GET /api/events?run_id=...` (SSE)
//! - 同步 API → `GET /api/<resource>`、`PUT /api/<resource>`
//!
//! 目前仅留类型占位，避免编译期引入 reqwest client 配置 / TLS / SSE 解析栈。
//! 真正实施时考虑：(1) 鉴权 token；(2) optimistic concurrency（settings ETag）；
//! (3) SSE 断线重连 + `since_seq`（见 `Op::Subscribe`）。

#[allow(dead_code)]
pub struct HttpCoreClient {
    /// `https://localhost:7777/api` 之类的根 URL。
    base_url: String,
    // TODO Step 4 远程：reqwest::Client + auth token + SSE 解码器。
}

impl HttpCoreClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
