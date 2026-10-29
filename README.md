# LLM Unify

[Project website](https://t3pp31.github.io/llm-unify/) · [Source and documentation](https://github.com/T3pp31/llm-unify)

An LLM gateway and asynchronous Rust SDK. Configure upstream endpoints and public model names in YAML, then access them through an HTTP API.
The web console lets you search configured models, copy model names, and view API connection details.

## Run with Docker

```sh
cp deploy/.env.example deploy/.env
# Set the master key, upstream URL, and upstream key in deploy/.env, and the model in deploy/proxy.yaml.
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d --build
```

Open the console at `http://127.0.0.1:4000/ui/` with the default settings.
See the [installation, HTTPS, upgrade, and rollback guide](docs/operations.md).
Distribution images are built from source; no image has been published to a registry yet.

The MIT license permits commercial use, modification, and redistribution.
See [LICENSE](LICENSE) and [third-party source attribution](THIRD_PARTY_NOTICES.md).
The Python LiteLLM runtime and upstream administration UI are not bundled.

## Current scope

The proxy web console displays configuration. Per-user virtual keys, budget enforcement, usage summaries, and model editing through the UI are not yet available.
It is intended for operators who can share a single master key and cannot serve as a multi-tenant authorization or billing system.
**Full LiteLLM OSS parity and product acceptance remain incomplete.**
The baseline is LiteLLM 1.108.0. Support status and test requirements are tracked in the [parity verification record](docs/parity/README.md).

The Rust SDK below uses Rust 1.96 or later, Edition 2024, and Tokio.
The existing `llm-unify-gateway` HTTP relay with a SQLite ledger is a separate command from `llm-unify-proxy`.

## Usage

```rust,no_run
use llm_unify::{ChatRequest, Client};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::from_env()?;
let model = std::env::var("LLM_UNIFY_MODEL")?;
let response = client.chat(ChatRequest::new(model)
    .system("You are a helpful assistant.")
    .user("What makes Rust a compelling language?")
    .max_output_tokens(1024)).await?;
println!("{}", response.output_text());
# Ok(()) }
```

Run the repository examples as follows. Set credentials only for the provider you use.

```bash
export OPENAI_API_KEY='your-key'
export LLM_UNIFY_MODEL='openai/your-model'
cargo run --example chat
cargo run --example streaming
cargo run --example tools
```

The SDK does not execute tools. Handle each `ToolCall` in your application, then pass `response.into_message()` and `Message::tool(ToolResult)` to the next request. Multiple tool calls require a result for every call.

## Supported APIs

| Provider | API | Chat / Stream / Tools / Image input | Feature |
|---|---|---|---|
| `openai` | Responses (default), Chat Completions | Supported | Default |
| `anthropic` | Messages | Supported | Default |
| `gemini` | Interactions, steps format | Supported | Default |
| `ollama`, user-defined compatible endpoints | OpenAI Chat Completions-compatible | Depends on the endpoint | Default |
| `bedrock` | Converse / ConverseStream | Images must be byte arrays | `bedrock` |
| `vertex` | Vertex Gemini generateContent / streamGenerateContent | Supported | `vertex` |

Structured output and reasoning options are translated to each native API. Check whether the model itself supports them.
Embeddings are supported for OpenAI and OpenAI-compatible endpoints that support embeddings.
Audio, video, image generation, Bedrock InvokeModel, and third-party models on Vertex are outside the scope.

Cloud integrations and OpenTelemetry are optional features. Bedrock uses the AWS SDK credential chain; Vertex uses Google ADC. Official libraries handle credentials and token refresh.

## Configuration

Defaults in `config/defaults.toml`, known model capabilities in `config/capabilities.toml`, and optional prices in `config/pricing.toml` are bundled at build time. The model capability and price tables are currently empty and contain no inferred values.

Configuration precedence is explicit builder settings, environment variables, user TOML, then bundled defaults. `Client::builder()` starts with bundled defaults; `Client::from_env()` also reads `LLM_UNIFY_CONFIG` and configuration environment variables.

```bash
cp config/example.toml config/local.toml
export LLM_UNIFY_CONFIG=config/local.toml
```

TOML configuration can be partial. Keep API key values out of TOML: set an environment variable name in `api_key_env`, or pass the key with the builder’s `api_key()`. `.env` files are not loaded automatically.

For each provider, override TOML with `LLM_UNIFY_<UPPERCASE_ENDPOINT_NAME>_BASE_URL`, `_API`, `_API_VERSION`, `_PROJECT`, `_LOCATION`, and `_REGION`. Hyphens in endpoint names become underscores.
Vertex requires `project` and `location`; Bedrock requires a region in configuration or the AWS credential chain.

Explicit capabilities under `capabilities."provider/model"` take precedence. `false` rejects an option before sending; unspecified capabilities remain unknown and their options are preserved. Capabilities are not inferred from similar model names.
`ProviderMetadata` identifies both the provider and API format and rejects forwarding to a different provider or API.

Signatures, message IDs, and `phase` needed for conversation history are retained in content block metadata and can be resent through `into_message()`. Response safety information and raw usage are retained as read-only `response.provider_metadata` and are not automatically forwarded into history. When the API returns a model name, `response.model` identifies that actual model.

The caller maintains conversation history. Responses and Interactions requests send `store: false`. This controls server-side conversation state and does not change the service’s overall data retention policy.

## Routing, retry, and streaming

`config/example.toml` includes alias and route examples. A name such as `alias/chat` selects an endpoint from a group using weighted round-robin. Clones of the same client share load-balancing and failure state.

- Retry HTTP 429, configured 5xx responses, and selected transport failures. Honor `Retry-After`; otherwise use capped exponential backoff with jitter.
- Apply an attempt limit and total deadline across all endpoints. Invalid input and authentication failures are not retried automatically.
- Temporarily exclude endpoints after consecutive failures and probe recovery with one attempt after the cooldown. Fail immediately when all endpoints are excluded.
- If the initially selected endpoint fails, try the remaining group members and then explicit fallbacks. Do not send to candidates with incompatible capabilities or metadata.
- Disable streaming retry/fallback after any event has been delivered to the caller.

`ChatStream::collect_response()` builds the final response. Client streaming calculates `cost_usd` with the same price table as ordinary responses and retains an upstream HTTP request ID in `request_id` when available. Price lookup uses the actual response model first, then the requested model. Unknown usage or prices produce `None`. A disconnect without a normal terminal event produces `StreamInterrupted`, with generated content and incomplete tool arguments in `error.partial`. Dropping the stream cancels the connection.

Gemini Interactions `incomplete` is retained as `FinishReason::Unknown("incomplete")` when the API reports a normal terminal state. A token limit is not assumed when the API does not return a termination cause.

## Caching, extensions, and observability

Caching is disabled by default. When enabled, it retains successful non-streaming responses within TTL and capacity limits. **Responses containing tool calls are also cached.** Check `response.cache_hit` and decide in your application whether executing the tools again is appropriate. Tool call IDs are not regenerated.

Clones share the cache. Separately built clients have isolated key spaces, even when registered with the same cache. External cache errors do not turn a successful API response into a failure. Streaming and embeddings are not cached.

`Provider` and `Cache` are public async traits. Register custom implementations with `Client::builder().provider(name, Arc::new(...))` or `.cache(...)`. Dynamic library loading and a Redis implementation are not included in the SDK.

`Observer` receives the model, endpoint, timing, attempt count, usage, cache state, and error category. The `opentelemetry` feature uses the application’s global meter/tracer. It does not create exporters or record request/response bodies.

`cost_usd` is calculated only when a price table is available. It is an estimate at configured standard rates and excludes cache discounts, batch discounts, and similar adjustments. Without prices, it is `None`.

## Verification

```bash
cargo fmt -p llm-unify -p llm-unify-engine -p llm-unify-proxy -p llm-unify-storage -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features --all-targets
cargo test --locked --doc --all-features
cargo check --locked --no-default-features --all-targets
```

Ordinary tests use only mock HTTP and synthetic fixtures and require no API keys, cloud credentials, or paid requests. Use `examples` explicitly for live service checks. Fixture sources and verification dates are recorded in `tests/fixtures/README.md`.
In addition to Linux CI, manual `workflow_dispatch` runs test all features on macOS and Windows. The crate has not been published to crates.io.

## Local LLM forwarding and usage ledger

The `gateway` feature forwards ordinary responses and SSE streams to OpenAI-compatible servers such as Ollama. Use an `openai_compatible` endpoint or an `openai` endpoint configured with `api = "chat_completions"`. The API is `POST /v1/chat/completions`. Model names use the SDK’s `provider/model` format; the part after the first `/` is sent upstream. Other request fields and response JSON are preserved.

```bash
cp config/example.toml config/local.toml
export LLM_UNIFY_CONFIG=config/local.toml
export LLM_UNIFY_GATEWAY_API_KEY='replace-with-a-long-random-secret'
cargo run --features gateway --bin llm-unify-gateway
```

In `[gateway]` in `config/local.toml`, configure the listen address, SQLite path, authentication environment variable name, request size/concurrency/history limits, and SQLite busy timeout. Paths may be absolute or relative to the startup directory. The database’s parent directory and schema are created at startup. The example listens on loopback. Configure TLS through a reverse proxy or equivalent for external access.

Ensure Ollama is running and the target model is installed before making requests.

```bash
curl http://127.0.0.1:4000/v1/chat/completions \
  -H "Authorization: Bearer $LLM_UNIFY_GATEWAY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"ollama/your-installed-model","messages":[{"role":"user","content":"Hello"}]}'

curl http://127.0.0.1:4000/usage/requests \
  -H "Authorization: Bearer $LLM_UNIFY_GATEWAY_API_KEY"
curl http://127.0.0.1:4000/usage/summary \
  -H "Authorization: Bearer $LLM_UNIFY_GATEWAY_API_KEY"
```

All APIs require the same bearer authentication. The upstream API key is read from `api_key_env` in the existing provider configuration; the caller’s authorization header is not forwarded upstream. Prompts, generated text, and API keys are not stored in the database. Model names, timestamps, status, token counts, pricing snapshots, and estimated costs are stored. Authenticated chat requests receive a gateway-generated `x-request-id` on both successful and failed responses for ledger correlation. Caller and upstream IDs are not used as this correlation ID. Requests rejected by pre-send validation or concurrency limits, and requests whose initial persistence fails, have no ledger row.

- Usage comes from the upstream `usage` field and is not estimated with a local tokenizer. Unknown values are `null`; measured zero is `0`.
- Streaming sends `stream_options.include_usage = true`. The upstream must accept this option and return usage. Usage events are also forwarded to the client. A disconnect without `[DONE]` is recorded as failure and returns an SSE error.
- Calculate `estimated_cost_usd` at standard input/output rates only when `[prices."provider/model"]` exists. Each row retains the price table, source, and effective date at recording time; configuration changes do not rewrite historical prices. Configure your own assumed rates for local operating costs. Actual API invoices, GPU usage, electricity consumption, cache discounts, and tiered pricing are not measured or supported.
- Forwarding does not use automatic retry, fallback, or caching. Each accepted request sends upstream at most once. Client retries are recorded as separate requests.
- Persist a `pending` row before sending upstream, then update it to `completed` or `failed` before responding. Client disconnects become `cancelled`. Requests left unresolved by forced termination remain `pending` and are not treated as successful or free. Persistence failures return HTTP or SSE errors.
- `/usage/requests` returns newest first. Paginate with `limit` and the returned `next_before` as `before`. Both APIs support `model` and Unix millisecond filters from inclusive `from_ms` to exclusive `to_ms`. `/usage/summary` aggregates by UTC day and model and returns `truncated: true` when the retrieval limit is exceeded; narrow the period or model and retry. Always inspect unknown counts alongside totals; the sum of known costs is not the final cost of all requests.

This relay does not translate HTTP formats for Anthropic/Gemini, manage per-user keys, enforce budgets, or provide an administration UI. Native APIs remain available through the SDK.

References are `ARCHITECTURE.md`, `litellm/proxy/hooks/proxy_track_cost_callback.py`, `litellm/proxy/db/db_spend_update_writer.py`, and `litellm-rust/crates/cost/src/lib.rs` in `example_code/litellm-main.zip` (commit `dddbaa803d6e9fcc862ea7a5412f771cb1ae040e`, SHA-256 `b9f9058eb9141baaa7f072ca6be448f2e947d6b264a032a3579a613b63b45a99`). After verifying the MIT license for the non-enterprise portions, we implemented responsibility separation, unknown-price handling, and per-send recording independently using these references. No `enterprise/` code is used.

## Project website

The English introduction is published on [GitHub Pages](https://t3pp31.github.io/llm-unify/).
Edit `site/index.html`, `site/styles.css`, and `site/script.js`; configure URLs and metadata in `site/site.json` (`repository_url`, `site_url`, and `description` are required).
Run `python3 tools/build_site.py` to generate `_site/`, then preview it with `python3 -m http.server --directory _site 8000`.
The Pages workflow deploys site changes from `main` and the initial publication branch `codex/github-pages`. It requires no third-party build dependencies.
