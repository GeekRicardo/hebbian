use async_trait::async_trait;
use serde_json::Value;

use crate::config::{AuthMode, Provider};
use crate::{
    client::ModelClient,
    protocols::anthropic as proto,
    providers::apply_auth,
    types::{
        has_image_generation_tool, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, Usage,
    },
};
use platform::{runtime as cancellation, CancelFlag};

pub struct AnthropicClient {
    provider: Provider,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(provider: Provider) -> Result<Self, ModelError> {
        let http = super::build_http_client()?;
        Ok(Self { provider, http })
    }

    fn messages_url(&self) -> String {
        format!(
            "{}/v1/messages",
            self.provider.base_url.trim_end_matches('/')
        )
    }

    fn is_claude_code_oauth(&self) -> bool {
        matches!(self.provider.auth_mode, AuthMode::OauthClaudeCode)
    }
}

#[async_trait]
impl ModelClient for AnthropicClient {
    fn provider_id(&self) -> &str {
        &self.provider.id
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        reject_image_generation_tool(&req)?;
        let body = proto::build_body(&req, false, self.is_claude_code_oauth());

        let future = async {
            let resp = apply_auth(self.http.post(self.messages_url()), &self.provider)
                .json(&body)
                .send()
                .await?;
            let status = resp.status().as_u16();
            let text = resp.text().await?;
            if status >= 400 {
                return Err(ModelError::Http { status, body: text });
            }
            let v: Value = serde_json::from_str(&text)?;
            Ok(proto::parse_response(&v))
        };

        wait_or_cancel(future, cancel).await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        reject_image_generation_tool(&req)?;
        let body = proto::build_body(&req, true, self.is_claude_code_oauth());

        let resp = wait_or_cancel(
            async {
                let r = apply_auth(self.http.post(self.messages_url()), &self.provider)
                    .json(&body)
                    .send()
                    .await?;
                Ok::<_, ModelError>(r)
            },
            cancel.clone(),
        )
        .await?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await?;
            return Err(ModelError::Http { status, body });
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();
        let mut current_event_type = String::new();

        while let Some(chunk) = super::next_stream_chunk_or_cancel(&mut stream, &cancel).await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Anthropic SSE: event: <type>\ndata: <json>\n\n
            while let Some(pos) = buf.find("\n\n").or_else(|| buf.find("\r\n\r\n")) {
                let skip = if buf[pos..].starts_with("\r\n\r\n") {
                    4
                } else {
                    2
                };
                let frame = buf[..pos].to_string();
                buf = buf[pos + skip..].to_string();

                for line in frame.lines() {
                    let line = line.trim_end_matches('\r');
                    if let Some(event) = line.strip_prefix("event:") {
                        current_event_type = event.trim().to_string();
                    } else if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        if let Some(delta) = proto::parse_stream_delta(&current_event_type, data) {
                            on_event(ModelStreamEvent::TextDelta {
                                text: delta.clone(),
                            });
                            full.push_str(&delta);
                        }
                    }
                }
            }
        }

        Ok(ModelResponse::Done {
            text: full,
            attachments: Vec::new(),
            usage: Usage::default(),
        })
    }
}

fn reject_image_generation_tool(req: &ModelRequest) -> Result<(), ModelError> {
    if has_image_generation_tool(&req.tools) {
        return Err(ModelError::Other(
            "image_generation 工具只支持 OpenAI Responses；请切换到 OpenAI provider，或关闭生图工具。"
                .to_string(),
        ));
    }
    Ok(())
}

async fn wait_or_cancel<T, F>(fut: F, cancel: CancelFlag) -> Result<T, ModelError>
where
    F: std::future::Future<Output = Result<T, ModelError>>,
{
    tokio::select! {
        res = fut => res,
        _ = async {
            while !cancellation::is_cancelled(&cancel) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        } => Err(ModelError::Cancelled),
    }
}
