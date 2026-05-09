use async_trait::async_trait;
use serde_json::Value;

use crate::config::{AuthMode, Provider};
use crate::{
    client::ModelClient,
    protocols::gemini as proto,
    providers::apply_auth,
    types::{
        has_image_generation_tool, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, Usage,
    },
};
use platform::CancelFlag;

pub struct GeminiClient {
    provider: Provider,
    http: reqwest::Client,
}

impl GeminiClient {
    pub fn new(provider: Provider) -> Result<Self, ModelError> {
        let http = super::build_http_client()?;
        Ok(Self { provider, http })
    }

    fn url(&self, model: &str, stream: bool) -> String {
        let base = self.provider.base_url.trim_end_matches('/');
        let endpoint = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };

        if matches!(self.provider.auth_mode, AuthMode::OauthGeminiCli) {
            let suffix = if stream { "?alt=sse" } else { "" };
            format!("{}/v1beta/models/{}:{}{}", base, model, endpoint, suffix)
        } else {
            let sep = if stream { "alt=sse&" } else { "" };
            format!(
                "{}/v1beta/models/{}:{}?{}key={}",
                base, model, endpoint, sep, self.provider.api_key
            )
        }
    }
}

#[async_trait]
impl ModelClient for GeminiClient {
    fn provider_id(&self) -> &str {
        &self.provider.id
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        reject_image_generation_tool(&req)?;
        let url = self.url(&req.model, false);
        let body = proto::build_body(&req);

        super::retry_request(cancel, || {
            let body = body.clone();
            let url = url.clone();
            async move {
                let resp = apply_auth(self.http.post(&url), &self.provider)
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
            }
        })
        .await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        reject_image_generation_tool(&req)?;
        let url = self.url(&req.model, true);
        let body = proto::build_body(&req);

        let resp = super::retry_request(cancel.clone(), || {
            let body = body.clone();
            let url = url.clone();
            async move {
                let r = apply_auth(self.http.post(&url), &self.provider)
                    .json(&body)
                    .send()
                    .await?;
                let status = r.status().as_u16();
                if status >= 400 {
                    let body = r.text().await?;
                    return Err(ModelError::Http { status, body });
                }
                Ok(r)
            }
        })
        .await?;

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();
        // Gemini 把 usageMetadata 跨多帧推增量，留最新一份即可。
        let mut usage = Usage::default();

        while let Some(chunk) = super::next_stream_chunk_or_cancel(&mut stream, &cancel).await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));

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
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        if let Some(delta) = proto::parse_stream_delta(data) {
                            on_event(ModelStreamEvent::TextDelta {
                                text: delta.clone(),
                            });
                            full.push_str(&delta);
                        }
                        if let Some(u) = proto::parse_stream_usage(data) {
                            usage = u;
                        }
                    }
                }
            }
        }

        Ok(ModelResponse::Done {
            text: full,
            reasoning: String::new(),
            attachments: Vec::new(),
            usage,
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
