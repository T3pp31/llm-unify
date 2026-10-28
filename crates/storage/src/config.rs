use std::time::Duration;

use serde::Deserialize;

use crate::Error;

/// Connection URL is supplied only through the named environment variable.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresSettings {
    pub url_environment: String,
    pub max_connections: u32,
    pub acquire_timeout_ms: u64,
    pub statement_timeout_ms: u32,
    pub lock_timeout_ms: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisSettings {
    pub url_environment: String,
    pub namespace: String,
    pub operation_timeout_ms: u64,
    pub max_cache_entry_bytes: usize,
}

pub(crate) fn environment(name: &str) -> Result<String, Error> {
    if name.is_empty() {
        return Err(Error::Configuration(
            "connection URL environment variable is required",
        ));
    }
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(Error::Configuration(
            "connection URL environment variable is unset or empty",
        ))
}

pub(crate) fn duration(milliseconds: u64) -> Result<Duration, Error> {
    let duration = Duration::from_millis(milliseconds);
    if milliseconds == 0 || std::time::Instant::now().checked_add(duration).is_none() {
        return Err(Error::Configuration(
            "operation timeout must be positive and representable",
        ));
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_environment_is_an_explicit_redacted_error() {
        // Given: a missing connection variable.
        // When: resolving the database URL.
        // Then: no accidental default connection or secret-bearing error is returned.
        let error = environment("LLM_UNIFY_MISSING_STORAGE_TEST_VARIABLE").unwrap_err();
        assert!(matches!(error, Error::Configuration(_)));
        assert!(!format!("{error:?}").contains("LLM_UNIFY_MISSING"));
    }

    #[test]
    fn zero_timeout_and_unknown_configuration_are_rejected() {
        // Given: disabled deadlines and an unrecognized field.
        // When: validating settings.
        // Then: neither configuration is silently accepted.
        assert!(duration(0).is_err());
        assert!(serde_json::from_str::<PostgresSettings>(r#"{"url_environment":"X","max_connections":1,"acquire_timeout_ms":1,"statement_timeout_ms":1,"lock_timeout_ms":1,"ignored":true}"#).is_err());
    }
}
