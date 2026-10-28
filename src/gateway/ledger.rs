use super::{ApiError, Counts};
use crate::{GatewayConfig, Price};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub(super) struct Ledger(Arc<Mutex<Connection>>);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Filter {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub model: Option<String>,
    pub before: Option<i64>,
    pub limit: Option<usize>,
}

impl Filter {
    pub fn limit(&self, maximum: usize) -> Result<usize, ApiError> {
        let limit = self.limit.unwrap_or(maximum);
        if limit == 0
            || limit > maximum
            || self.from_ms.is_some_and(|n| n < 0)
            || self.to_ms.is_some_and(|n| n < 0)
            || self.before.is_some_and(|n| n <= 0)
            || matches!((self.from_ms, self.to_ms), (Some(a), Some(b)) if a >= b)
        {
            return Err(ApiError::bad_request("invalid usage filter"));
        }
        Ok(limit)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    pub sequence: i64,
    pub request_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub model: String,
    pub provider: String,
    pub status: String,
    pub upstream_status: Option<u16>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub pricing: Option<Price>,
    pub error_code: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageSummary {
    pub date_utc: String,
    pub model: String,
    pub requests: u64,
    pub completed_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub pending_requests: u64,
    pub unknown_usage_requests: u64,
    pub unknown_cost_requests: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Sum of known estimates; consult unknown_cost_requests before interpreting it.
    pub estimated_cost_usd: Option<f64>,
}

impl Ledger {
    pub async fn open(config: &GatewayConfig) -> Result<Self, ApiError> {
        let config = config.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = config.database_path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|_| ApiError::storage())?;
            }
            let connection =
                Connection::open(config.database_path).map_err(|_| ApiError::storage())?;
            connection
                .busy_timeout(Duration::from_millis(config.sqlite_busy_timeout_ms))
                .map_err(|_| ApiError::storage())?;
            let version: u32 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .map_err(|_| ApiError::storage())?;
            if version > 1 {
                return Err(ApiError::storage());
            }
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
                .and_then(|_| connection.execute_batch(include_str!("schema.sql")))
                .map_err(|_| ApiError::storage())?;
            Ok(Self(Arc::new(Mutex::new(connection))))
        })
        .await
        .map_err(|_| ApiError::storage())?
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, ApiError> {
        let connection = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection.lock().map_err(|_| ApiError::storage())?;
            operation(&connection).map_err(|_| ApiError::storage())
        })
        .await
        .map_err(|_| ApiError::storage())?
    }

    pub async fn begin(
        &self,
        request_id: String,
        model: String,
        provider: String,
        price: Option<Price>,
    ) -> Result<(), ApiError> {
        let pricing = price
            .map(|price| serde_json::to_string(&price))
            .transpose()
            .map_err(|_| ApiError::storage())?;
        let now = now_ms()?;
        self.run(move |connection| {
            connection.execute(
                "INSERT INTO gateway_usage(request_id, started_at_ms, model, provider, status, pricing_json)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                params![request_id, now, model, provider, pricing],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn finish(
        &self,
        id: String,
        status: &'static str,
        upstream_status: Option<u16>,
        counts: Counts,
        cost: Option<f64>,
        error_code: Option<&'static str>,
    ) -> Result<(), ApiError> {
        let now = now_ms()?;
        self.run(move |connection| {
            connection.execute(
                "UPDATE gateway_usage SET finished_at_ms=?2, status=?3, upstream_status=?4,
                 input_tokens=?5, output_tokens=?6, total_tokens=?7, estimated_cost_usd=?8,
                 error_code=?9 WHERE request_id=?1 AND status='pending'",
                params![
                    id,
                    now,
                    status,
                    upstream_status,
                    counts.input,
                    counts.output,
                    counts.total,
                    cost,
                    error_code
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn records(
        &self,
        filter: Filter,
        limit: usize,
    ) -> Result<Vec<UsageRecord>, ApiError> {
        self.run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT sequence, request_id, started_at_ms, finished_at_ms, model, provider,
                 status, upstream_status, input_tokens, output_tokens, total_tokens,
                 estimated_cost_usd, pricing_json, error_code FROM gateway_usage
                 WHERE (?1 IS NULL OR started_at_ms>=?1) AND (?2 IS NULL OR started_at_ms<?2)
                 AND (?3 IS NULL OR model=?3) AND (?4 IS NULL OR sequence<?4)
                 ORDER BY sequence DESC LIMIT ?5",
            )?;
            statement
                .query_map(
                    params![
                        filter.from_ms,
                        filter.to_ms,
                        filter.model,
                        filter.before,
                        limit as i64
                    ],
                    |row| {
                        let pricing: Option<String> = row.get(12)?;
                        let pricing = pricing
                            .map(|value| {
                                serde_json::from_str(&value).map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        12,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                })
                            })
                            .transpose()?;
                        Ok(UsageRecord {
                            sequence: row.get(0)?,
                            request_id: row.get(1)?,
                            started_at_ms: row.get(2)?,
                            finished_at_ms: row.get(3)?,
                            model: row.get(4)?,
                            provider: row.get(5)?,
                            status: row.get(6)?,
                            upstream_status: row.get(7)?,
                            input_tokens: row.get(8)?,
                            output_tokens: row.get(9)?,
                            total_tokens: row.get(10)?,
                            estimated_cost_usd: row.get(11)?,
                            pricing,
                            error_code: row.get(13)?,
                        })
                    },
                )?
                .collect()
        })
        .await
    }

    pub async fn summary(
        &self,
        filter: Filter,
        limit: usize,
    ) -> Result<Vec<UsageSummary>, ApiError> {
        self.run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT strftime('%Y-%m-%d', started_at_ms / 1000, 'unixepoch'), model,
                 COUNT(*), SUM(status='completed'), SUM(status='failed'), SUM(status='cancelled'),
                 SUM(status='pending'), SUM(input_tokens IS NULL OR output_tokens IS NULL),
                 SUM(estimated_cost_usd IS NULL), SUM(input_tokens), SUM(output_tokens),
                 SUM(total_tokens), SUM(estimated_cost_usd) FROM gateway_usage
                 WHERE (?1 IS NULL OR started_at_ms>=?1) AND (?2 IS NULL OR started_at_ms<?2)
                 AND (?3 IS NULL OR model=?3)
                 GROUP BY 1, model ORDER BY 1 DESC, model LIMIT ?4",
            )?;
            statement
                .query_map(
                    params![filter.from_ms, filter.to_ms, filter.model, limit as i64],
                    |row| {
                        Ok(UsageSummary {
                            date_utc: row.get(0)?,
                            model: row.get(1)?,
                            requests: row.get(2)?,
                            completed_requests: row.get(3)?,
                            failed_requests: row.get(4)?,
                            cancelled_requests: row.get(5)?,
                            pending_requests: row.get(6)?,
                            unknown_usage_requests: row.get(7)?,
                            unknown_cost_requests: row.get(8)?,
                            input_tokens: row.get(9)?,
                            output_tokens: row.get(10)?,
                            total_tokens: row.get(11)?,
                            estimated_cost_usd: row.get(12)?,
                        })
                    },
                )?
                .collect()
        })
        .await
    }
}

fn now_ms() -> Result<i64, ApiError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_millis()).ok())
        .ok_or_else(ApiError::storage)
}
