//! Native asynchronous LLM access with explicit provider selection.
//!
//! ```no_run
//! use llm_unify::{Client, ChatRequest};
//! # async fn example() -> Result<(), llm_unify::Error> {
//! let client = Client::from_env()?;
//! let response = client.chat(ChatRequest::new("openai/my-model").user("Hello")).await?;
//! println!("{}", response.output_text());
//! # Ok(()) }
//! ```
mod cache;
mod capabilities;
mod client;
mod config;
mod embeddings;
mod error;
mod observability;
pub mod providers;
mod registry;
mod router;
mod stream;
mod transport;
mod types;

pub use cache::{Cache, MemoryCache};
pub use capabilities::Capabilities;
pub use client::{Client, ClientBuilder};
pub use config::{CacheConfig, Config, HttpConfig, ProviderConfig, RetryConfig, RouterConfig};
pub use embeddings::{Embedding, EmbeddingRequest, EmbeddingResponse};
pub use error::{Error, ErrorKind};
pub use observability::{Observation, Observer, Price};
pub use registry::Provider;
pub use router::{Deployment, Route};
pub use stream::{ChatEvent, ChatStream, StreamAccumulator};
pub use types::*;
