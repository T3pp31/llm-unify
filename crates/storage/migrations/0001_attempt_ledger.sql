-- A separate schema avoids changing a source LiteLLM database during migration.
CREATE SCHEMA llm_unify;

CREATE TABLE llm_unify.accounts (
    id TEXT PRIMARY KEY CHECK (length(btrim(id)) > 0),
    budget_nanos BIGINT CHECK (budget_nanos >= 0),
    spent_nanos BIGINT NOT NULL DEFAULT 0 CHECK (spent_nanos >= 0),
    reserved_nanos BIGINT NOT NULL DEFAULT 0 CHECK (reserved_nanos >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE llm_unify.attempts (
    id TEXT PRIMARY KEY CHECK (length(btrim(id)) > 0),
    request_id TEXT NOT NULL CHECK (length(btrim(request_id)) > 0),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    model TEXT NOT NULL CHECK (length(btrim(model)) > 0),
    input_fingerprint TEXT NOT NULL CHECK (length(input_fingerprint) = 64),
    reserved_nanos BIGINT NOT NULL CHECK (reserved_nanos >= 0),
    state TEXT NOT NULL CHECK (state IN ('prepared', 'dispatched', 'uncertain', 'settled', 'released')),
    actual_nanos BIGINT CHECK (actual_nanos >= 0),
    outcome TEXT CHECK (outcome IN ('succeeded', 'failed', 'cancelled')),
    http_status INTEGER CHECK (http_status BETWEEN 100 AND 599),
    prompt_tokens BIGINT CHECK (prompt_tokens >= 0),
    completion_tokens BIGINT CHECK (completion_tokens >= 0),
    usage_json JSONB CHECK (usage_json IS NULL OR jsonb_typeof(usage_json) = 'object'),
    upstream_request_id TEXT,
    settlement_fingerprint TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    dispatched_at TIMESTAMPTZ,
    settled_at TIMESTAMPTZ,
    UNIQUE (request_id, ordinal),
    CHECK ((state = 'settled') = (actual_nanos IS NOT NULL)),
    CHECK ((state = 'settled') = (settlement_fingerprint IS NOT NULL)),
    CHECK ((state = 'settled') = (outcome IS NOT NULL))
);

CREATE TABLE llm_unify.attempt_accounts (
    attempt_id TEXT NOT NULL REFERENCES llm_unify.attempts(id),
    account_id TEXT NOT NULL REFERENCES llm_unify.accounts(id),
    PRIMARY KEY (attempt_id, account_id)
);

CREATE INDEX unresolved_attempts ON llm_unify.attempts(state, created_at)
    WHERE state IN ('prepared', 'dispatched', 'uncertain');
CREATE INDEX account_attempts ON llm_unify.attempt_accounts(account_id, attempt_id);
