use async_trait::async_trait;
use std::sync::Arc;

use super::types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent};
use platform::CancelFlag;

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
}

pub type DynModelClient = Arc<dyn ModelClient>;
