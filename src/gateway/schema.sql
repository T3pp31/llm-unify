CREATE TABLE IF NOT EXISTS gateway_usage (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL UNIQUE,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    model TEXT NOT NULL,
    provider TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'completed', 'failed', 'cancelled')),
    upstream_status INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    total_tokens INTEGER,
    estimated_cost_usd REAL,
    pricing_json TEXT,
    error_code TEXT
);
CREATE INDEX IF NOT EXISTS gateway_usage_time_model ON gateway_usage(started_at_ms, model);
PRAGMA user_version = 1;
