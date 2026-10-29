# CI design

Updated: 2026-10-25

## Purpose and triggers

Verify that existing SDK contracts remain intact without live APIs, secrets, or paid requests.
The workflow is defined in [ci.yml](../.github/workflows/ci.yml).

- Run on pull requests and pushes to main.
- Detect changes to `src/**`, `tests/**`, `examples/**`, `config/**`, Cargo files, and the workflow itself.
- Documentation-only changes under `docs/**` or the README are outside the original trigger scope.
- `workflow_dispatch` also runs macOS and Windows checks.

## Verification jobs

The regular test job runs the following on Ubuntu / Rust stable:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features --all-targets
cargo test --locked --doc --all-features
```

The `features` job uses the MSRV, Rust 1.96.0, and checks no additional features, bedrock only, vertex only, opentelemetry only, and gateway only with `cargo check --locked --no-default-features --features ... --all-targets`. Together with the all-features checks, this detects compilation gaps in feature isolation.

The manually triggered portability job tests all features and targets on stable Rust on macOS and Windows.

## Test coverage

- Per-provider request/response conversion, tool calls/results, and preservation of signatures and provider-specific information.
- Text streaming, fragmented tool arguments, UTF-8/SSE boundaries, and interrupted streams.
- HTTP 429 retry, attempt limits, fallback, weighted load balancing, failure state, and recovery.
- Capabilities, configuration, missing authentication, and secret non-disclosure.
- Cache TTL/capacity, isolation between clients, and reuse of tool-call responses.
- Bedrock AWS signing and binary EventStream, Vertex authentication headers and conversation history.
- Gateway HTTP/SSE forwarding, authentication, SQLite persistence, pricing snapshots, daily/model summaries, unknown usage, upstream failures, interruptions, and persistence failures.

Fixtures are synthetic data based on official documentation; sources are recorded in the [fixture README](../tests/fixtures/README.md). Live service checks are run explicitly with examples and are not added to normal CI.

## Permissions and caching

The workflow has only `contents: read` permission and uses no secrets.
Cache the Cargo registry, Git dependencies, and target directory, separating keys by OS, toolchain/feature, and Cargo.lock hash. Commit Cargo.lock and use `--locked` to pin dependency resolution.

## Failure handling

Check the failed step and test name, then reproduce locally with the same command.
If an external provider API change is suspected, check official documentation, update the verification date, and revise the fixture and adapter.

## Local gateway verification

```bash
cargo test --locked --features gateway --test gateway
cargo test --locked --all-features --all-targets
cargo test --locked --doc --all-features
```

Gateway tests use a local mock upstream and temporary SQLite databases to verify persistence across reconnections, price changes, unknown values, HTTP/SSE failures, client disconnects, concurrency limits, and persistence failures. They do not start Ollama or perform actual inference.

Coverage measurement is not integrated into CI. After installing `cargo-llvm-cov` and the toolchain’s `llvm-tools-preview`, run `cargo llvm-cov --all-features --html` to generate an HTML report. Coverage percentages were not measured during this verification.
