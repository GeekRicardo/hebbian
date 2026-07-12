use async_trait::async_trait;
use std::sync::Arc;

use super::types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent, TranscriptEntry};
use common::CancelFlag;

#[async_trait]
pub trait ModelClient: Send + Sync {
    fn provider_id(&self) -> &str;

    fn supports_streaming_tools(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError>;

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError>;

    async fn compact_remote(
        &self,
        _req: ModelRequest,
        _before_tokens: usize,
        _cancel: CancelFlag,
        _on_progress: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<Option<Vec<TranscriptEntry>>, ModelError> {
        Ok(None)
    }
}

pub type DynModelClient = Arc<dyn ModelClient>;
