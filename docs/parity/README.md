# LiteLLM OSS parity verification record

Full feature parity and product acceptance remain incomplete. Do not treat the current proxy as a production migration target.
The existing Rust SDK, TOML configuration, and SQLite ledger in `llm-unify-gateway` are separate from the compatibility proxy under development.
The existing gateway retains its contract of at most one upstream send per accepted request.

The baseline is LiteLLM **1.108.0**, commit `dddbaa803d6e9fcc862ea7a5412f771cb1ae040e`.
The reference ZIP, SHA-256, extraction scope, and original source manifest SHA-256 are pinned in `config/parity/baseline.json`.
MIT copyright notices are retained in `vendor/litellm/LICENSE` and related files. Only upstream Rust and its test materials are kept in the repository; the Python runtime and upstream web UI are not bundled.
Pinned comparison sources live in Git-ignored `target/reference/litellm/`. Enterprise directories are excluded.
Changes to upstream Rust dependency constraints are recorded in `config/parity/patches.json` with reasons, replacements, and before/after hashes.

## Current scope

| Area | Status |
|---|---|
| Upstream Rust reuse, licenses/hashes, separate external comparison sources | Implemented. Importing code does not count as evidence of feature implementation |
| Static extraction of HTTP, providers, provider/API pairs, configuration, public SDK names, router, cache, authentication, administration, guardrails, integrations, and UI | Inventory candidates generated. OSS/Enterprise classification, dynamic registration analysis, and contract review remain incomplete |
| OpenAI Chat ordinary responses, tool calls, tool-result round trips, SSE, upstream 429 | Differential cases implemented with the pinned Python version and a mock upstream |
| Upstream Rust Responses, Messages, OCR, Transcription, etc. | Routes connected. Per-feature Python comparisons remain incomplete |
| YAML | Only supported keys accepted. Unsupported budget/router/cache settings are not silently ignored |
| Connecting the existing SDK to the shared engine; all providers/APIs, pricing, router, cache backends | Incomplete |
| PostgreSQL attempt ledger and atomic reservation/settlement across multiple budgets; Redis shared rate limits/cache | Rust APIs and real-database tests implemented. Integration into all proxy send paths remains incomplete |
| Virtual keys, user/team/organization management, pricing integration, administration API | Not implemented |
| Web console `/ui/` | Served by Rust. Displays authenticated configured-model lists and runtime status. Administration operations are not implemented |
| All console operations and interaction E2E tests, database migration, backup/restore | Not implemented |
| Live APIs for seven representative endpoints; failure/soak tests with two instances, 100 concurrent requests, and one hour | Not run. Codex local-router tests are a separate live-connection category |

`inventory.json` is a static list of investigation candidates. Counts do not represent product features, OSS coverage, or implemented features.
Each candidate’s `scope`, input/output contract, implementation status, and verification cases are generated as unreviewed values.
Reviewed implementation decisions are saved separately in `config/parity/implementation.json` and are not overwritten by extraction.
OpenAI Chat records are `partial`. Passing five cases does not promote them to `implemented` until all provider/API contracts are covered.
Candidates include functions, types, and internal helpers, so each also needs review to determine whether it is an independent product feature.

## Pinned source and inventory

Python 3.12 or later is required. Upstream Python is parsed as an AST without importing it.

```bash
python3 tools/parity/source.py verify
python3 tools/parity/workspace.py check
python3 tools/parity/inventory.py check
python3 -m unittest discover -s tests/parity -v
```

Place the reference ZIP at `archive` in `config/parity/baseline.json`. Only an archive matching the pinned SHA-256 is accepted.
`python3 tools/parity/source.py import` imports only Rust materials; it does not restore Python into vendor.
Before comparison, `python3 tools/parity/source.py import --reference` extracts pinned sources to Git-ignored `target/reference/litellm/`.
Verify all comparison materials with `source.py verify --reference`.
Ordinary `inventory.py check` verifies the pinned inventory hash and baseline; `inventory.py check --reference` regenerates from external comparison sources and checks equality.
The full release gate also requires external comparison sources and fails if they are absent.
`SOURCE.json` retains original file hashes; only recorded patches are applied and verified.
Regenerate the inventory with `python3 tools/parity/inventory.py generate`.

Because of Cargo’s nested-workspace inheritance issue ([Cargo #12154](https://github.com/rust-lang/cargo/issues/12154)),
the generated section in the root Cargo.toml retains upstream package/dependency definitions with the same specifications.
Generate with `python3 tools/parity/workspace.py generate`; use `check` to verify there are no differences except paths.
Select this project’s crates with `-p`; this is distinct from running the entire upstream Rust test suite.

## Running the proxy

```bash
cargo build --locked -p llm-unify-proxy
cargo run --locked -p llm-unify-proxy -- \
  --config /absolute/path/to/config.yaml --listen 127.0.0.1:4000 --check
cargo run --locked -p llm-unify-proxy -- \
  --config /absolute/path/to/config.yaml --listen 127.0.0.1:4000
```

Accepted keys are listed in `config/parity/native-capabilities.json`.
Supported settings include `model_name` and `litellm_params` under `model_list`, `general_settings.master_key`, and `environment_variables`.
Endpoints and keys can use `os.environ/VARIABLE_NAME`. Missing or empty master keys cause startup errors.
Custom Python authentication settings are rejected with an explanation that migration to Rust is required.
Multiple deployments with the same `model_name` are rejected until routing is implemented, rather than collapsed to one entry.

Deadlines, limits, and the default OpenAI URL are loaded from `config/parity/native-defaults.json` at build time.
Replace them with external JSON using the same schema via `--runtime-config /absolute/path/to/runtime.json`.
HTTP bodies and API keys are not logged. The current proxy has no persistent usage ledger.
OpenAI Chat retries operate independently of the existing gateway, with at most two retries by default for HTTP 429 and 5xx.
This does not implement all routing strategies, fallback, or consistency with billing records.

## Web console

Start the proxy and visit `http://<listen-address>/ui/`. HTML, CSS, and JavaScript are
embedded in the `llm-unify-proxy` binary and served by the same Rust process. No Node.js or Python server is required.
Entering this proxy’s master key in the console fetches `/ui/api/status` with bearer authentication.
It displays public model names, the product version, and API connection paths. It does not return upstream URLs, API keys, or the full configuration.
The key is used only in page memory and discarded on disconnect or page exit.
UI responses use `no-store` and a same-origin content security policy.
`config/proxy-ui.json` defines displayed API information and the UI request timeout, loaded at build time.
See `docs/operations.md` for distribution and operations instructions.
The console does not establish completion of the administration API. Configuration editing, virtual key management, usage, and billing totals are not yet provided.

## Persistent ledger and shared limits foundation

The Rust API in `crates/storage` is exposed through `llm_unify_engine::storage`.
This does not enable billing, budgets, or caching in the HTTP proxy. Verification currently covers the foundation in isolation.
The existing gateway’s SQLite ledger is preserved.

PostgreSQL is the source of truth. Reservations and settlements across multiple budget accounts
representing keys, users, teams, organizations, and similar entities run in one transaction.
The caller supplies account associations; virtual-key authentication and resolution of all applicable budgets still need separate implementation.
Redis is not the budget source of truth.

Attempt states are `prepared → dispatched → settled`; unknown send outcomes can become `uncertain`.
Only a caller whose `dispatch` succeeds may begin sending upstream. Retries require a new attempt ID and ordinal.
Only pre-send `prepared` attempts can become `released`. After dispatch, deadlines or proxy restarts alone do not release reservations;
reservations remain until actual cost is established. Interrupted responses and unknown settlement commit outcomes require reconciliation as uncertain results.
Actual cost is recorded even when it exceeds the reservation, and exceeded budgets reject subsequent reservations.
Unknown money/token values are distinct from zero. Provider usage details such as cache/reasoning usage are retained as original JSON.
The current API represents money as integer billionths of a USD and does not silently round finer decimal amounts.
Compatibility with every provider’s pricing and conversion remains incomplete.

`Ledger::connect` uses a URL from an environment variable. Explicitly run `Ledger::migrate` before serving traffic
to create the `llm_unify` schema in a new target database. This initializes an internal schema;
it is not a data migration command from LiteLLM PostgreSQL or SQLite.
Do not run this migration against the source database. Configure connection counts, acquisition deadlines, SQL execution deadlines,
and lock deadlines through `PostgresSettings`. Errors do not expose secrets from URLs or the database.

Redis evaluates all applicable limits together through Lua and consumes nothing if any limit is insufficient.
The same admission ID with the same input avoids duplicate consumption for the longest window; different input is rejected as a conflict.
Windows use Redis TTL rather than the client clock. These are fixed-window consumption limits.
Concurrency leases, settlement of actual TPM usage, and the full set of LiteLLM router limits are not implemented.
The cache API requires a tenant authorization scope and request fingerprint.
Redis failures return errors. Redis Cluster places a namespace in the same hash slot to preserve atomicity.
Preserving limits across restarts also requires Redis persistence; tests use AOF with `appendfsync always`.

Standalone unit tests:

```bash
cargo test --locked -p llm-unify-storage --lib
```

Tests with real databases, concurrency of 100, two independent connection pools, and forced PostgreSQL/Redis stop and recovery:

```bash
python3 tools/parity/run_storage.py
```

Python 3.12 or later, running Docker, and the images in `config/parity/storage-test.json` are required.
If images are absent, `docker pull` the file’s `postgres_image` and `redis_image` values.
The runner creates dedicated test credentials, databases, Redis, and available ports; it removes only containers and volumes it created.
It checks errors during service downtime, preservation of reservations and rate limits after recovery, and no duplicates on settlement retries.
Configuration and thresholds come from that JSON. Results and image IDs are saved to `target/parity/storage.json`,
with details in `storage-*.log`. This does not replace tests of two HTTP proxy instances or a one-hour load test.
Real-database tests are ignored by ordinary `cargo test`; the runner executes all of them with `--ignored`.
Unexecuted, zero-test, or partially ignored database runs are not recorded as successful.
A coverage tool has not been selected. After installing `cargo-llvm-cov`, set the same isolated database environment variables
and measure with `cargo llvm-cov -p llm-unify-storage --test postgres --test redis -- --ignored`.

## Python differential tests

`config/parity/cases.json` specifies identical input and mock upstream responses.
Comparisons cover HTTP status, JSON fields, ordered SSE events, usage, and upstream request contents/counts.
The current five Chat cases do not verify prices, all APIs, or all providers.
Only JSON pointers for generated IDs/timestamps explicitly listed per case can be excluded.
Incomplete environments or zero cases are not recorded as success. Logs and results are saved to `target/parity/`.

Create a local Python environment with pinned dependencies:

```bash
uv export --frozen --no-dev --extra proxy --no-emit-workspace \
  --project target/reference/litellm --output-file target/parity-requirements.txt --quiet
uv venv --python 3.12 target/parity-reference-venv
uv pip sync --python target/parity-reference-venv/bin/python \
  --require-hashes target/parity-requirements.txt
python3 tools/parity/run_local.py --python target/parity-reference-venv/bin/python
```

Run only the Python reference in a container and compare against the same Rust binary:

```bash
python3 tools/parity/run_local.py \
  --reference-image python:3.12-slim --uv-version 0.11.17 --startup-seconds 180
```

This command uses a local Docker image and synchronizes dependencies from the pinned lockfile with hashes inside it.
The reference container and mock upstream stop after testing. No live external API keys are used.
`deploy/parity.compose.yaml` runs native/reference/mock/compare entirely in containers.
It uses `config/parity/containers.env.example` and runtime `PARITY_MASTER_KEY` / `PARITY_SUBJECT` values.
Get `PARITY_SUBJECT` with `python3 tools/parity/gate.py --subject`; generate a random master key dedicated to testing.
Start Compose with `docker compose --env-file config/parity/containers.env.example -f deploy/parity.compose.yaml up --build --abort-on-container-exit --exit-code-from compare`; stop it with `down` using the same options.
This is not a production Compose deployment with PostgreSQL/Redis. Acceptance remains blocked until the complete container setup has been built and tested.

## Completion criteria and evidence

### Live connections through Codex’s local LLM

```bash
python3 tools/parity/run_live.py --codex-config /absolute/path/to/.codex/config.toml
```

Read the active provider’s `base_url` and model, then verify ordinary responses,
streaming, forced tool calls, and tool-result round trips through the Rust proxy’s `/v1/responses`. Override the model with `--model`.
The runner does not read Codex `auth.json` or OAuth tokens. It uses the provider’s `env_key` environment variable when specified,
or a non-secret placeholder for an unauthenticated loopback router. Non-loopback connections require HTTPS and an explicit API key environment variable.
Providers with additional authentication headers or query settings are rejected rather than having those settings silently ignored.

Input/output and time limits are configured in `config/parity/live-settings.json`.
Results are saved to `target/parity/live-local-router.json`, checking requested and actual response model names, usage,
stream ordering/reconstruction, and round trips of tool JSON arguments and call IDs.
Reports do not store API keys, Authorization headers, generated text, actual tool argument data, or raw proxy logs.
Returned model names are recorded unchanged even when different from the requested alias. Model identity or hardware placement is not inferred or certified.

Local-router results are partial tests named `local_router_live`.
They do not replace live API acceptance for OpenAI, Anthropic, Gemini, Azure, Bedrock, Vertex, and Ollama, or full comparisons against the pinned Python version.
Register partial evidence with `evidence.py --component-report target/parity/live-local-router.json`.
Rerun after source changes, as with other evidence. The CLI does not pass failures, incomplete responses, or zero tests.

### Release gate

```bash
python3 tools/parity/gate.py
```

Results are saved to `target/parity/release.json`. Exit code 1 is expected while implementation remains incomplete.
Besides matching the pinned version and inventory, the gate requires scope classification, input/output contracts, implementation, and per-case evidence for every candidate.
Exclusions are limited to Enterprise-specific features, Python SDK syntax, and direct execution of custom Python; each requires a reason and upstream source evidence.

Implementation records are keyed by candidate ID and have this shape:

```json
{
  "scope": "oss",
  "status": "implemented",
  "contract": {"input": "Reviewed input contract", "output": "Reviewed output contract"},
  "verification_cases": ["case-id"]
}
```

The execution evidence manifest is saved to `target/parity/evidence.json` with `schema_version: 1`,
`baseline` (version/commit/archive_sha256), `subject` (current source SHA-256), `cases`, `suites`, `live`, and `load`.
Each entry in `cases` has `status: "passed"`, an array of verified `features` IDs, a relative local `report` path, and the file’s `sha256`.
`suites` and `live` use the same report/hash/status format. Each report must also contain matching `subject` and `status`,
a positive `passed` count, `failed: 0`, and `skipped: 0`. Stale source or modified reports are rejected.
The source digest includes uncommitted and untracked implementation files; vendor is verified separately with the pinned manifest and patches.

See `config/parity/baseline.json` for required suites, live API endpoints, and load conditions.
Measured load results must include instance count, concurrency, duration, warmup, RSS growth rate, and zero counts for unintended failures, duplicate accounting, authorization leaks, and reservation leaks.
Do not register a report that passes only the five differential cases as evidence of full feature acceptance.

Register only the current cases and foundation tests:

```bash
python3 tools/parity/evidence.py \
  --cases-report target/parity/differential.json \
  --component-report target/parity/storage.json
python3 tools/parity/gate.py
```

Both reports must match the same current source digest. Registration does not change implementation status in the inventory.
The report body is checked for case ID presence, uniqueness, and comparison success; partial tests are saved under `components`.
`suites`, `live`, and `load` remain empty, so full acceptance still fails.
The gate also checks suite names, live API endpoints, and measured load results, preventing reuse of successful reports from unrelated tests.
The local comparison runner records the reference execution method, container image ID/uv version or Python version, and SHA-256 hashes of the Rust binary and lockfile.
