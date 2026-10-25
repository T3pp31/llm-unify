use crate::{
    Capabilities, ChatRequest, ChatResponse, ChatStream, EmbeddingRequest, EmbeddingResponse, Error,
};
use async_trait::async_trait;
#[async_trait]
pub trait Provider: Send + Sync {
    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::default()
    }
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, Error>;
    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatStream, Error>;
    async fn embed(&self, _request: &EmbeddingRequest) -> Result<EmbeddingResponse, Error> {
        Err(Error::unsupported("embeddings"))
    }
}
