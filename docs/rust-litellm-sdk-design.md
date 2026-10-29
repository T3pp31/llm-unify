# Rust LiteLLM SDK design

Created: 2026-10-25

Implementation status: all roadmap stages below were implemented in a single crate. The crate is an initial development release, `0.1.0`. See the [README](../README.md) for actual API examples, constraints, and features. CI uses synthetic fixtures and mock HTTP without calling live APIs.

Update, 2026-10-26: an optional `gateway` feature was added for local LLM forwarding and persistent usage recording. It provides OpenAI Chat Completions-compatible HTTP/SSE forwarding, bearer authentication, a SQLite ledger, and daily/model summary APIs. It operates independently of the existing SDK’s native API conversions, router, and cache. HTTP forwarding does not translate other providers’ API formats or provide automatic retry, fallback, or caching. Unknown usage/cost is `null`; pricing is saved as a snapshot at recording time. This does not establish full LiteLLM feature parity.

## 1. Overview

This project provides an LLM SDK with Rust’s type safety and extensibility while retaining LiteLLM’s ease of use.

Core characteristics:

- Model selection with `provider/model`.
- Unified, type-safe requests and responses.
- Connections to each provider’s native API.
- Built-in streaming, tool calling, retry, and fallback.
- Minimal dependencies.
- Extensibility that preserves provider-specific features.

## 2. Scope

### 2.1 Included

Build an SDK with a LiteLLM-style public API that internally selects provider adapters.

Initial core features:

- Chat API
- Streaming
- Tool calling
- Retry
- Fallback
- Capability validation

### 2.2 Excluded

The following are outside the initial scope:

- Agent frameworks
- RAG
- Vector stores
- An HTTP proxy in the SDK core (see the opening update for the additional optional gateway).
- An administration UI

The library’s responsibility is limited to calling LLM APIs safely through a consistent interface. Routing and caching are optional features configured explicitly by users, rather than prerequisites for direct calls. The router provides in-process weighted round-robin, retry/fallback, and temporary exclusion of failing endpoints. Caching is disabled by default.

## 3. Differentiation

As of October 2026, Rust already has several LLM libraries.

| Library | Characteristics |
| --- | --- |
| genai | Unified API and native protocols for over 25 providers |
| Rig | Framework for LLM applications, agents, tools, and RAG |
| async-openai | Typed OpenAI client |

Calling OpenAI, Anthropic, and Gemini through the same API alone offers limited differentiation. This project focuses on three points:

1. **Provider agnostic**: retain LiteLLM-style `provider/model` selection.
2. **Rust native**: follow Rust’s types, errors, and asynchronous design naturally.
3. **Minimal core**: stabilize the smallest core needed for LLM API calls, without agents or RAG.

## 4. Architecture

Keep the public API in a LiteLLM style while using a general canonical data model internally.

```text
Public API
  Client / ChatRequest / ChatResponse

Canonical Data Model
  Message / ContentBlock / ToolCall / Usage

Provider Registry + Capability Validation
  OpenAI Adapter
  Anthropic Adapter
  Gemini Adapter
  OpenAI-compatible Adapter

Shared HTTP Transport
  reqwest / tokio / TLS / Timeout
```

Each adapter converts between the common data model, provider-specific JSON, and streaming events. This allows support for new fields in OpenAI Responses or Gemini Interactions without major changes to the entire public interface.

Preserve information that cannot be generalized. Keep metadata required for replaying history, such as Gemini-specific signatures, in content block metadata. Retain response safety information as read-only `ChatResponse.provider_metadata`, without automatically forwarding it to the next turn.

## 5. Public API

### 5.1 Example

```rust
use llm_unify::{Client, ChatRequest};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::from_env()?;

    let response = client
        .chat(
            ChatRequest::new("anthropic/claude-sonnet-5")
                .system("You are a helpful assistant.")
                .user("What makes Rust a compelling language?")
                .max_output_tokens(1024),
        )
        .await?;

    println!("{}", response.output_text());

    Ok(())
}
```

Switching models generally requires changing only the model identifier.

```text
openai/gpt-6-astra
anthropic/claude-sonnet-5
gemini/gemini-3.8-flash
ollama/my-local-model
```

Do not treat options unsupported by a model as compatible features. Unsupported features must return explicit errors.

## 6. Rust type design

### 6.1 ChatRequest

```rust
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub options: GenerationOptions,
    pub tools: Vec<ToolDefinition>,
}
```

### 6.2 ContentBlock

```rust
pub enum ContentBlock {
    Text { text: String, metadata: Vec<ProviderMetadata> },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Image(ImageContent),
    Provider(ProviderMetadata),
}
```

### 6.3 ChatResponse

```rust
pub struct ChatResponse {
    pub model: ModelId,
    pub content: Vec<ContentBlock>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: FinishReason,
    // The implementation also retains deployment, cache_hit, cost_usd, and request_id.
}
```

This design has three goals:

1. Avoid limiting message content to strings, allowing natural representations of tool calls and image input.
2. Preserve provider-specific information, passing Gemini signatures and other providers’ special fields to the next turn.
3. Allow future extension: mark enums `#[non_exhaustive]` so variants can be added compatibly.

`ProviderMetadata` carries the originating provider’s identifier rather than being arbitrary JSON alone. This prevents accidental forwarding to a different provider.

## 7. Provider trait

Define a trait for provider adapter implementations separately from the public API.

```rust
#[async_trait]
pub trait Provider {
    async fn chat(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatResponse, Error>;

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatStream, Error>;
}
```

The initial implementation uses `async_trait` to support dynamic dispatch easily. The internals can be optimized if static dispatch becomes necessary later.

## 8. Capabilities

Represent each model’s support as metadata.

| Capability | Meaning |
| --- | --- |
| Streaming | Whether streaming responses are supported |
| Tool Calling | Whether tool calling is supported |
| Image Input | Whether image input is supported |
| Structured Output | Whether structured output through JSON Schema or equivalent is supported |
| Reasoning Options | Whether reasoning options are supported |

Also validate per-model support for parameters such as `temperature` and `max_output_tokens`.

For unknown models, do not assume support or lack of support. Prefer explicitly supplied capabilities and do not silently discard options.

## 9. Streaming design

Streaming is a core SDK feature. Convert provider-specific events to shared `ChatEvent` values.

```rust
pub enum ChatEvent {
    TextDelta { index: usize, delta: String },
    ToolCallStart { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, delta: String },
    Metadata { index: usize, metadata: ProviderMetadata },
    ResponseMetadata(ProviderMetadata),
    Usage(TokenUsage),
    Completed { reason: FinishReason },
}
```

Separate two components:

```text
Stream Decoder
  Convert provider-specific events to ChatEvent

Stream Accumulator
  Accumulate events and construct the final ChatResponse
```

Tool arguments may be incomplete JSON while streaming. Do not parse each delta as a complete JSON value.

Do not treat an interrupted stream as success. Return an error informing the caller that partial output exists.

## 10. Errors and reliability

| Error | Meaning |
| --- | --- |
| InvalidRequest | Request violates common types or provider constraints |
| UnsupportedOption | Model or provider does not support the feature |
| AuthError | Missing API key or invalid credentials |
| RateLimitError | Rate-limit failure |
| ProviderError | Runtime error returned by the provider |
| TransportError | Network failure or timeout |
| StreamInterrupted | Stream terminated prematurely |

Handle retries near the shared HTTP layer. Eligible failures generally include:

- HTTP 429
- Selected HTTP 5xx responses
- Retryable transport failures

After any streaming output has been delivered to the caller, disable transparent automatic retries to prevent duplicate output or repeated tool execution.

The client shares HTTP connections internally and is designed for `Clone + Send + Sync`. API keys are scoped to each client’s configuration and are not logged.

## 11. Directory structure

Start with modules in a single crate.

```text
llm-unify/
  Cargo.toml / Cargo.lock
  config/          # defaults.toml, capabilities.toml, pricing.toml, example.toml
  src/
    lib.rs / client.rs / config.rs / error.rs
    types.rs / registry.rs / capabilities.rs / stream.rs
    router.rs / cache.rs / embeddings.rs / observability.rs
    transport.rs
    providers/     # openai, anthropic, gemini, bedrock, vertex
  tests/
    fixtures/      # Synthetic fixtures and sources
    contract/      # Shared contracts and mock transport
  examples/
  .github/workflows/ci.yml
```

## 12. Roadmap

### Stage 1 (originally v0.1)

| Area | Content |
| --- | --- |
| Core | Client and common message types |
| Provider | OpenAI Chat / Responses |
| Streaming | Text streaming |
| Error | Error conversion |

### Stage 2 (originally v0.2)

| Area | Content |
| --- | --- |
| Provider | Anthropic Messages, Gemini Interactions API |
| Tool Calling | Tool call and tool result round trips |
| Metadata | Preservation of provider-specific metadata |
| Structured Output | Structured output through JSON Schema |

### Stage 3 (originally v0.3)

| Area | Content |
| --- | --- |
| Router | Retry/fallback, weighted round-robin, failure management |
| Alias | Model aliases |
| Embeddings | Embeddings API |
| Observability | Token usage/cost and OpenTelemetry integration |

### Stage 4 (originally v0.4 and later)

| Area | Content |
| --- | --- |
| Multimodal | Image and other multimodal input |
| Provider | Optional Bedrock Converse / Vertex Gemini features |
| Plugin | Registration API for Rust provider traits; dynamic loading is outside the scope |
| Cache | Cache trait and in-memory cache with TTL/capacity limits, including tool-call responses and cache_hit reporting |

## 13. Testing strategy

At each stage, run synthetic fixtures based on each provider’s public specifications and shared contract tests. When adding real responses, remove secrets and record the capture date, API version, and source. Verify cloud connections with fake credentials and local HTTP.

Focus on:

1. Tool-call round trips.
2. Interrupted streams.
3. HTTP 429 retries.
4. Explicit errors for unsupported options.
5. Preventing secrets from entering logs.

## 14. Configuration and operations

- Target Rust 1.96 / Edition 2024 and Tokio’s asynchronous API.
- Bundled configuration lives in `config/`; user configuration is partial TOML selected by `LLM_UNIFY_CONFIG`. Precedence is builder, environment variables, user configuration, then defaults.
- Pass API keys through environment variables or ClientBuilder. `.env` is not loaded automatically.
- The caller retains conversation history. Responses / Interactions requests send `store: false`.
- Reject known unsupported features before sending; preserve options for unknown models.
- ProviderMetadata identifies the provider and API format and rejects forwarding to a different destination.
- Retry and fallback share attempt/time limits. Do not retry after delivering a streaming event to the caller.
- Cache successful non-streaming responses. Share caches between client clones and isolate key spaces between separately built clients.
- Do not infer or automatically fetch prices or model capabilities. Return `None` without prices; otherwise return an estimate at standard rates.
- Select Bedrock, Vertex, and OpenTelemetry dependencies through features. Live external API checks and crates.io publication are not automatic.
