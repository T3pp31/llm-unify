use crate::{ModelId, TokenUsage};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: ModelId,
    pub input: Vec<String>,
    pub dimensions: Option<u32>,
}
impl EmbeddingRequest {
    pub fn new(model: ModelId, input: Vec<String>) -> Self {
        Self {
            model,
            input,
            dimensions: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Embedding {
    pub index: usize,
    pub vector: Vec<f32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub model: ModelId,
    pub deployment: String,
    pub embeddings: Vec<Embedding>,
    pub usage: Option<TokenUsage>,
}
