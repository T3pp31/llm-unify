# Installing and operating LLM Unify

## Current scope

LLM Unify is an MIT-licensed gateway and Rust SDK. The web console displays gateway
status and configured models, supports model search, and copies model names and the API Base URL.
Models are managed in YAML. The console does not use the upstream LiteLLM UI.

This version is intended for operators who can share one master key. It does not provide
per-user virtual keys, tenant isolation, budget enforcement, or per-user usage summaries.
The prototype PostgreSQL/Redis APIs are not connected to HTTP proxy billing or budget enforcement.
Full LiteLLM compatibility, administration operations, database migration, and a one-hour multi-instance load test remain incomplete.
Production deployment requirements for environments that need these features have not been met.

## Start with Docker

Run from the repository root. Docker Engine/Desktop and Compose v2 are required.
No image has been published to a registry yet. Build locally from the pinned lockfile.

```sh
cp deploy/.env.example deploy/.env
openssl rand -hex 32
```

Set the following values in `deploy/.env`. Do not add this file to Git.

- `LLM_UNIFY_MASTER_KEY`: a sufficiently random generated master key.
- `UPSTREAM_API_BASE`: the upstream API Base URL, usually ending in `/v1`.
- `UPSTREAM_API_KEY`: the upstream key. For a local endpoint without authentication, use a nonempty, non-secret placeholder.
- `PROXY_BIND_ADDRESS` and `PROXY_PORT`: the exposed address and port; localhost only by default.
- Other limits, build concurrency, and health-check settings are collected in the same file.

Change `litellm_params.model` in `deploy/proxy.yaml` to your upstream model.
`model_name: main` is the public name used in the client’s model field.
Unsupported or unknown YAML settings cause startup to fail.
When a local LLM runs on the host with Docker Desktop, use `host.docker.internal`
as the connection hostname. On Linux, configure routing to the host separately.

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml config --quiet
docker compose --env-file deploy/.env -f deploy/compose.yaml build
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d
docker compose --env-file deploy/.env -f deploy/compose.yaml ps
```

With the default port, open `http://127.0.0.1:4000/ui/` and sign in with the master key.
The UI is embedded in the image; no Node.js or Python production runtime is required.
The container runs as a non-root user with a read-only filesystem, dropped capabilities, and PID/memory limits.
YAML is mounted read-only; restart to apply changes.
Create `deploy/.env` and configuration files with readable permissions before startup.

## External access

Terminate TLS at a trusted reverse proxy or load balancer.
The browser’s origin becomes the API Base URL. Forward `/ui/`, `/ui/api/`, `/v1/`, and `/health/`
on the same origin without adding a path prefix, and preserve the Authorization header.
Send master keys and inference requests over HTTPS.
To restrict the console to operators, add network/authentication restrictions at the entry point.
Do not expose the gateway assuming budget or per-user limits are implemented.

The UI uses the key only in memory and discards it on sign-out or page exit.
Reloading requires signing in again. The key is not saved in browser storage.

## Health checks and logs

- `/health/liveliness`: the process responds over HTTP.
- `/health/readiness`: configuration and authentication initialization are complete and requests can be accepted. This does not guarantee upstream health.
- Verify upstream/model health with actual requests to an API supported by that endpoint.

Compose rotates logs. Environment variables and expanded Compose configuration contain secrets.
Do not paste full `docker compose config` output or environment variables from `docker inspect` into public logs.

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml logs --tail 100 proxy
```

## Upgrades and rollback

1. Save the current image tag or digest, YAML, and `deploy/.env` in an access-restricted location.
2. Build new source in a separate checkout and verify the API and UI on a separate port with a dedicated test key.
3. Change `LLM_UNIFY_IMAGE` to the new tag and run `up -d`. Configure time for in-flight requests to finish with `PROXY_STOP_GRACE_PERIOD`.
4. If problems occur, restore the previous image tag and configuration and recreate with `up -d --no-build`.

The current proxy does not use a usage database. Include configuration, secrets, and images in your recovery procedure.
The SQLite ledger backup for the separate `llm-unify-gateway` command is distinct from this proxy setup.
Migration commands from LiteLLM PostgreSQL/SQLite are not available.

Stop and restart:

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml stop
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d
```

## Verification and releases

Run `python3 tools/check_container.py --image llm-unify:0.1.0` for an isolated smoke test
using the actual Compose configuration. It checks authentication, secret non-disclosure, UI,
readiness, non-root/read-only execution, and restart recovery, and removes only its own test containers.
It does not run upstream inference. See `docs/parity/README.md` for full acceptance requirements,
including differential, live API, migration, and load tests.
Passing a Docker smoke test alone does not establish product acceptance for all features.

## License

See the [MIT License](../LICENSE) and [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)
for upstream Rust attribution, modification history, and licenses.
