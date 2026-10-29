# Changelog

## Unreleased

- Translate the repository documentation, web console, and examples into English. Keep multibyte UTF-8 coverage in streaming tests.
- Remove Japanese text from upstream test data and generators, regenerate tokenizer/Python expectations, and record reproducible byte-range patches while preserving Unicode coverage.
- Add pinned LiteLLM 1.108.0 source, upstream Rust/UI, patch history, and hash verification. Resolve dependency conflicts through recorded patches.
- Add automatic feature inventory generation and a release gate that rejects unclassified or unimplemented features, missing comparisons, untested live APIs, and insufficient load tests.
- Add `llm-unify-engine` / `llm-unify-proxy` to the workspace. Add OpenAI Chat HTTP/SSE, tool history, usage, and 429 differential tests. Full LiteLLM OSS parity remains incomplete.
- Add a PostgreSQL per-attempt ledger and atomic reservation/settlement across multiple budgets, plus Redis shared rate limits and tenant-scoped caching in `llm-unify-storage`. Add real-database concurrency and forced-stop/recovery tests. Integration with proxy billing remains incomplete.
- Validate comparison case names, suites, endpoints, and current source identity; register partial evidence and record reference runtimes. Partial tests cannot establish full acceptance.

- Count streaming startup failures and Chat / Embeddings total-timeout failures toward endpoint failure thresholds, fixing temporary exclusion and recovery.
- Preserve estimated cost and available upstream HTTP request IDs in final responses collected through client streaming.
- Attach the ledger’s `x-request-id` to failed gateway responses and verify correlation across HTTP, transport, and SSE errors.
- Add the optional `gateway` feature and `llm-unify-gateway` binary with OpenAI Chat Completions-compatible HTTP/SSE forwarding, bearer authentication, a SQLite usage ledger, pricing snapshots, and daily/model summaries.
- Add gateway tests for unknown usage, missing prices, interrupted streams, and persistence failures; add a gateway-only CI configuration.

- Add the client, shared request/response types, capabilities, and provider metadata.
- Add native connections to OpenAI, Anthropic, Gemini, OpenAI-compatible endpoints, and Ollama.
- Add streaming, tool calling, structured output, reasoning options, and image input.
- Add retry/fallback, aliases, weighted load balancing, and temporary exclusion of failing endpoints.
- Add embeddings, optional cost estimation, observers, and OpenTelemetry.
- Add Bedrock Converse and Vertex Gemini as optional features.
- Add public provider/cache traits and an in-memory cache with TTL and capacity limits.
- Add contract tests and CI without live service calls.
