use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Postgres, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::{Error, Money, PostgresSettings, config, identifier};

#[derive(Clone)]
pub struct Ledger {
    pool: PgPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub budget: Option<Money>,
    pub spent: Money,
    pub reserved: Money,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptSpec {
    pub id: String,
    pub request_id: String,
    /// Zero-based ordinal of the actual external send, including retries.
    pub ordinal: i32,
    pub model: String,
    /// SHA-256 of the canonical upstream request; never store prompt or credentials.
    pub request_fingerprint: String,
    /// All applicable key, user, team, organization and global budget account IDs.
    pub accounts: Vec<String>,
    /// A conservative upper bound for this send; every applicable account reserves it.
    pub reserve: Money,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Prepared,
    Dispatched,
    Uncertain,
    Settled,
    Released,
}

impl FromStr for AttemptState {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "dispatched" => Ok(Self::Dispatched),
            "uncertain" => Ok(Self::Uncertain),
            "settled" => Ok(Self::Settled),
            "released" => Ok(Self::Released),
            _ => Err(Error::Database),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl FromStr for Outcome {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value {
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(Error::Database),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// None means unknown, rather than an invented zero.
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    /// Original provider usage fields, including cache/reasoning/audio details.
    /// None preserves the distinction between absent and an empty usage object.
    pub raw: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settlement {
    pub actual: Money,
    pub outcome: Outcome,
    pub http_status: Option<i32>,
    pub usage: Usage,
    pub upstream_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub id: String,
    pub request_id: String,
    pub ordinal: i32,
    pub model: String,
    pub accounts: Vec<String>,
    pub state: AttemptState,
    pub reserved: Money,
    pub actual: Option<Money>,
    pub outcome: Option<Outcome>,
    pub usage: Usage,
    pub http_status: Option<i32>,
    pub upstream_request_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: String,
    budget_nanos: Option<i64>,
    spent_nanos: i64,
    reserved_nanos: i64,
}

impl TryFrom<AccountRow> for Account {
    type Error = Error;
    fn try_from(row: AccountRow) -> Result<Self, Error> {
        Ok(Self {
            id: row.id,
            budget: row.budget_nanos.map(Money::from_nanos).transpose()?,
            spent: Money::from_nanos(row.spent_nanos)?,
            reserved: Money::from_nanos(row.reserved_nanos)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct AttemptRow {
    id: String,
    request_id: String,
    ordinal: i32,
    model: String,
    state: String,
    input_fingerprint: String,
    settlement_fingerprint: Option<String>,
    reserved_nanos: i64,
    actual_nanos: Option<i64>,
    outcome: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    usage_json: Option<serde_json::Value>,
    http_status: Option<i32>,
    upstream_request_id: Option<String>,
}

impl AttemptRow {
    fn attempt(self, accounts: Vec<String>) -> Result<Attempt, Error> {
        Ok(Attempt {
            id: self.id,
            request_id: self.request_id,
            ordinal: self.ordinal,
            model: self.model,
            accounts,
            state: self.state.parse()?,
            reserved: Money::from_nanos(self.reserved_nanos)?,
            actual: self.actual_nanos.map(Money::from_nanos).transpose()?,
            outcome: self.outcome.as_deref().map(str::parse).transpose()?,
            usage: Usage {
                prompt_tokens: self.prompt_tokens,
                completion_tokens: self.completion_tokens,
                raw: self.usage_json,
            },
            http_status: self.http_status,
            upstream_request_id: self.upstream_request_id,
        })
    }
}

fn fingerprint(value: &impl Serialize) -> Result<String, Error> {
    let mut value =
        serde_json::to_value(value).map_err(|_| Error::Invalid("invalid accounting input"))?;
    value.sort_all_objects();
    let bytes =
        serde_json::to_vec(&value).map_err(|_| Error::Invalid("invalid accounting input"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

impl Ledger {
    /// Connect without running migrations. Run `migrate` explicitly before serving traffic.
    pub async fn connect(settings: &PostgresSettings) -> Result<Self, Error> {
        let acquire_timeout = config::duration(settings.acquire_timeout_ms)?;
        if settings.max_connections == 0
            || settings.statement_timeout_ms == 0
            || settings.lock_timeout_ms == 0
        {
            return Err(Error::Configuration(
                "pool size and PostgreSQL deadlines must be positive",
            ));
        }
        let url = config::environment(&settings.url_environment)?;
        let options = PgConnectOptions::from_str(&url)
            .map_err(|_| Error::Configuration("invalid PostgreSQL URL"))?
            .options([
                (
                    "statement_timeout",
                    settings.statement_timeout_ms.to_string(),
                ),
                ("lock_timeout", settings.lock_timeout_ms.to_string()),
                (
                    "idle_in_transaction_session_timeout",
                    settings.statement_timeout_ms.to_string(),
                ),
            ]);
        let pool = PgPoolOptions::new()
            .max_connections(settings.max_connections)
            .acquire_timeout(acquire_timeout)
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    /// SQLx serializes migration runners and verifies applied migration checksums.
    pub async fn migrate(&self) -> Result<(), Error> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|_| Error::Database)
    }

    pub async fn health(&self) -> Result<(), Error> {
        sqlx::query("SELECT 1 FROM llm_unify.accounts LIMIT 1")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub async fn create_account(&self, id: &str, budget: Option<Money>) -> Result<Account, Error> {
        identifier(id)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO llm_unify.accounts(id,budget_nanos) VALUES ($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(budget.map(Money::nanos))
        .execute(&mut *tx)
        .await?;
        let row: AccountRow =
            sqlx::query_as("SELECT * FROM llm_unify.accounts WHERE id=$1 FOR UPDATE")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        let account: Account = row.try_into()?;
        if account.budget != budget {
            return Err(Error::Conflict);
        }
        tx.commit().await?;
        Ok(account)
    }

    pub async fn account(&self, id: &str) -> Result<Account, Error> {
        identifier(id)?;
        sqlx::query_as::<_, AccountRow>("SELECT * FROM llm_unify.accounts WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(Error::NotFound)?
            .try_into()
    }

    /// Changing a limit never discards already incurred spend or active reservations.
    pub async fn set_budget(&self, id: &str, budget: Option<Money>) -> Result<Account, Error> {
        identifier(id)?;
        let row: AccountRow = sqlx::query_as("UPDATE llm_unify.accounts SET budget_nanos=$2, updated_at=clock_timestamp() WHERE id=$1 RETURNING *")
            .bind(id).bind(budget.map(Money::nanos)).fetch_optional(&self.pool).await?.ok_or(Error::NotFound)?;
        row.try_into()
    }

    /// Atomically reserve every applicable account, in stable lock order.
    /// Idempotent preparation is not permission to send again; `dispatch` is the claim.
    pub async fn prepare(&self, mut spec: AttemptSpec) -> Result<Attempt, Error> {
        for value in [&spec.id, &spec.request_id, &spec.model] {
            identifier(value)?;
        }
        if spec.ordinal < 0 || spec.accounts.is_empty() {
            return Err(Error::Invalid(
                "attempt ordinal must be nonnegative and accounts nonempty",
            ));
        }
        if spec.request_fingerprint.len() != 64
            || !spec
                .request_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(Error::Invalid(
                "request fingerprint must be lowercase SHA-256 hex",
            ));
        }
        for account in &spec.accounts {
            identifier(account)?;
        }
        spec.accounts.sort();
        if spec.accounts.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::Invalid("duplicate budget account"));
        }
        let input_fingerprint = fingerprint(&spec)?;
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query("INSERT INTO llm_unify.attempts(id,request_id,ordinal,model,input_fingerprint,reserved_nanos,state) VALUES ($1,$2,$3,$4,$5,$6,'prepared') ON CONFLICT DO NOTHING")
            .bind(&spec.id).bind(&spec.request_id).bind(spec.ordinal).bind(&spec.model)
            .bind(&input_fingerprint).bind(spec.reserve.nanos()).execute(&mut *tx).await?.rows_affected();
        let row = sqlx::query_as::<_, AttemptRow>(
            "SELECT * FROM llm_unify.attempts WHERE id=$1 FOR UPDATE",
        )
        .bind(&spec.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::Conflict)?;
        if row.input_fingerprint != input_fingerprint {
            return Err(Error::Conflict);
        }
        if inserted != 0 {
            let accounts: Vec<AccountRow> = sqlx::query_as(
                "SELECT * FROM llm_unify.accounts WHERE id=ANY($1) ORDER BY id FOR UPDATE",
            )
            .bind(&spec.accounts)
            .fetch_all(&mut *tx)
            .await?;
            if accounts.len() != spec.accounts.len() {
                return Err(Error::NotFound);
            }
            for row in accounts {
                let account: Account = row.try_into()?;
                let total = account
                    .spent
                    .checked_add(account.reserved)?
                    .checked_add(spec.reserve)?;
                if account.budget.is_some_and(|budget| total > budget) {
                    return Err(Error::BudgetExceeded);
                }
                sqlx::query("UPDATE llm_unify.accounts SET reserved_nanos=reserved_nanos+$2,updated_at=clock_timestamp() WHERE id=$1")
                    .bind(&account.id).bind(spec.reserve.nanos()).execute(&mut *tx).await?;
                sqlx::query(
                    "INSERT INTO llm_unify.attempt_accounts(attempt_id,account_id) VALUES ($1,$2)",
                )
                .bind(&spec.id)
                .bind(&account.id)
                .execute(&mut *tx)
                .await?;
            }
        }
        let attempt = row.attempt(spec.accounts)?;
        tx.commit().await?;
        Ok(attempt)
    }

    /// Persist the send claim before touching the network. Only one caller succeeds.
    /// If commit returns an error, the caller must not send; reconcile via `attempt`.
    pub async fn dispatch(&self, id: &str) -> Result<(), Error> {
        identifier(id)?;
        let changed = sqlx::query("UPDATE llm_unify.attempts SET state='dispatched', dispatched_at=clock_timestamp() WHERE id=$1 AND state='prepared'")
            .bind(id).execute(&self.pool).await?.rows_affected();
        if changed == 0 {
            return Err(Error::State);
        }
        Ok(())
    }

    pub async fn mark_uncertain(&self, id: &str) -> Result<(), Error> {
        identifier(id)?;
        let changed = sqlx::query("UPDATE llm_unify.attempts SET state='uncertain' WHERE id=$1 AND state IN ('dispatched','uncertain')")
            .bind(id).execute(&self.pool).await?.rows_affected();
        if changed == 0 {
            return Err(Error::State);
        }
        Ok(())
    }

    /// A prepared attempt is known not to have sent. Dispatched attempts can only
    /// be settled using a known charge, including an explicitly verified zero.
    pub async fn release(&self, id: &str) -> Result<(), Error> {
        identifier(id)?;
        let mut tx = self.pool.begin().await?;
        let row = lock_attempt(&mut tx, id).await?;
        if row.state == "released" {
            tx.commit().await?;
            return Ok(());
        }
        if row.state != "prepared" {
            return Err(Error::State);
        }
        let accounts = lock_accounts(&mut tx, id).await?;
        for account in accounts {
            sqlx::query("UPDATE llm_unify.accounts SET reserved_nanos=reserved_nanos-$2,updated_at=clock_timestamp() WHERE id=$1")
                .bind(account.id).bind(row.reserved_nanos).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE llm_unify.attempts SET state='released',settled_at=clock_timestamp() WHERE id=$1")
            .bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Record the actual charge even if the provider exceeded the reserved bound.
    /// An exceeded budget blocks subsequent preparation, without hiding incurred cost.
    pub async fn settle(&self, id: &str, settlement: Settlement) -> Result<(), Error> {
        identifier(id)?;
        if settlement
            .http_status
            .is_some_and(|status| !(100..=599).contains(&status))
            || settlement
                .usage
                .prompt_tokens
                .is_some_and(|tokens| tokens < 0)
            || settlement
                .usage
                .completion_tokens
                .is_some_and(|tokens| tokens < 0)
            || settlement
                .usage
                .raw
                .as_ref()
                .is_some_and(|raw| !raw.is_object())
            || settlement
                .upstream_request_id
                .as_ref()
                .is_some_and(|id| id.contains('\0'))
        {
            return Err(Error::Invalid(
                "invalid status, token count, usage object or upstream request ID",
            ));
        }
        let settlement_fingerprint = fingerprint(&settlement)?;
        let mut tx = self.pool.begin().await?;
        let row = lock_attempt(&mut tx, id).await?;
        if row.state == "settled" {
            if row.settlement_fingerprint.as_deref() != Some(&settlement_fingerprint) {
                return Err(Error::Conflict);
            }
            tx.commit().await?;
            return Ok(());
        }
        if row.state != "dispatched" && row.state != "uncertain" {
            return Err(Error::State);
        }
        let accounts = lock_accounts(&mut tx, id).await?;
        for account_row in accounts {
            let account: Account = account_row.try_into()?;
            let spent = account.spent.checked_add(settlement.actual)?;
            // Keep the aggregate representable, including other active reservations.
            let remaining_reserved = account
                .reserved
                .nanos()
                .checked_sub(row.reserved_nanos)
                .ok_or(Error::Database)?;
            spent.checked_add(Money::from_nanos(remaining_reserved)?)?;
            sqlx::query("UPDATE llm_unify.accounts SET reserved_nanos=reserved_nanos-$2,spent_nanos=$3,updated_at=clock_timestamp() WHERE id=$1")
                .bind(account.id).bind(row.reserved_nanos).bind(spent.nanos()).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE llm_unify.attempts SET state='settled',actual_nanos=$2,outcome=$3,http_status=$4,prompt_tokens=$5,completion_tokens=$6,upstream_request_id=$7,settlement_fingerprint=$8,usage_json=$9,settled_at=clock_timestamp() WHERE id=$1")
            .bind(id).bind(settlement.actual.nanos()).bind(settlement.outcome.as_str()).bind(settlement.http_status)
            .bind(settlement.usage.prompt_tokens).bind(settlement.usage.completion_tokens).bind(settlement.upstream_request_id)
            .bind(settlement_fingerprint).bind(settlement.usage.raw).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn attempt(&self, id: &str) -> Result<Attempt, Error> {
        identifier(id)?;
        let mut tx = self.pool.begin().await?;
        let row = lock_attempt(&mut tx, id).await?;
        let accounts = sqlx::query_scalar("SELECT account_id FROM llm_unify.attempt_accounts WHERE attempt_id=$1 ORDER BY account_id")
            .bind(id).fetch_all(&mut *tx).await?;
        let attempt = row.attempt(accounts)?;
        tx.commit().await?;
        Ok(attempt)
    }

    /// IDs requiring recovery; no automatic expiration or release after dispatch.
    pub async fn unresolved(&self, limit: i64) -> Result<Vec<String>, Error> {
        if limit <= 0 {
            return Err(Error::Invalid("recovery page size must be positive"));
        }
        Ok(sqlx::query_scalar("SELECT id FROM llm_unify.attempts WHERE state IN ('prepared','dispatched','uncertain') ORDER BY created_at,id LIMIT $1")
            .bind(limit).fetch_all(&self.pool).await?)
    }
}

async fn lock_attempt(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<AttemptRow, Error> {
    sqlx::query_as("SELECT * FROM llm_unify.attempts WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)
}

async fn lock_accounts(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<Vec<AccountRow>, Error> {
    Ok(sqlx::query_as("SELECT a.* FROM llm_unify.accounts a JOIN llm_unify.attempt_accounts r ON r.account_id=a.id WHERE r.attempt_id=$1 ORDER BY a.id FOR UPDATE OF a")
        .bind(id).fetch_all(&mut **tx).await?)
}
