//! Durable accounting primitives for the compatibility runtime.
//!
//! PostgreSQL is authoritative. A dispatched attempt with an unknown outcome keeps
//! its reservation across restarts; a lease timeout never implies a free request.
//! These primitives do not by themselves enable Proxy billing or virtual keys.

mod config;
mod ledger;
mod money;
pub mod shared;

pub use config::{PostgresSettings, RedisSettings};
pub use ledger::{Account, Attempt, AttemptSpec, AttemptState, Ledger, Outcome, Settlement, Usage};
pub use money::Money;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid storage configuration: {0}")]
    Configuration(&'static str),
    #[error("invalid accounting input: {0}")]
    Invalid(&'static str),
    #[error("account or attempt was not found")]
    NotFound,
    #[error("budget is exhausted")]
    BudgetExceeded,
    #[error("idempotency key was reused with different input")]
    Conflict,
    #[error("attempt is not in the required state")]
    State,
    #[error("PostgreSQL operation failed; accounting outcome may require reconciliation")]
    Database,
    #[error("Redis operation failed; shared limits must not be bypassed")]
    Redis,
    #[error("storage operation deadline exceeded")]
    Timeout,
}

// Database and Redis errors can contain statements, credentials or connection URLs.
// Keep those errors out of Display, Debug and HTTP error envelopes.
impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}

impl From<redis::RedisError> for Error {
    fn from(_: redis::RedisError) -> Self {
        Self::Redis
    }
}

pub(crate) fn identifier(value: &str) -> Result<(), Error> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(Error::Invalid(
            "identifier must be nonempty and contain no NUL",
        ));
    }
    Ok(())
}
