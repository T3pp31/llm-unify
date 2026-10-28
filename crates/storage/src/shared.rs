//! Redis-backed fixed-window admission and tenant-scoped byte cache.
//! A Redis failure is an error, never a local fallback that bypasses shared limits.
use std::time::Duration;

use redis::aio::ConnectionManager;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Error, RedisSettings, config, identifier};

#[derive(Clone)]
pub struct Shared {
    connection: ConnectionManager,
    prefix: String,
    timeout: Duration,
    max_cache_entry_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct RateLimit {
    /// A tenant, key, team, model or other shared limit identity.
    pub id: String,
    pub window_ms: u64,
    pub limit: u64,
    pub amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    pub allowed: bool,
    pub retry_after_ms: u64,
}

/// The scope must identify the complete authorization/cache-sharing domain.
/// A bare model name cannot accidentally serve one tenant's cached prompt to another.
#[derive(Clone, Debug)]
pub struct CacheKey {
    digest: String,
}

impl CacheKey {
    pub fn new(scope: &str, request_fingerprint: &str) -> Result<Self, Error> {
        identifier(scope)?;
        if request_fingerprint.len() != 64
            || !request_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(Error::Invalid(
                "cache request fingerprint must be lowercase SHA-256 hex",
            ));
        }
        let bytes = serde_json::to_vec(&(scope, request_fingerprint))
            .map_err(|_| Error::Invalid("invalid cache key"))?;
        Ok(Self {
            digest: hash(bytes),
        })
    }
}

fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

// Lua numbers must represent all integers exactly. This is a protocol bound,
// not a deployment threshold.
const MAX_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

fn positive_integer(value: u64) -> Result<(), Error> {
    if value == 0 || value > MAX_EXACT_INTEGER {
        return Err(Error::Invalid(
            "Redis integer must be positive and exactly representable",
        ));
    }
    Ok(())
}

impl Shared {
    pub async fn connect(settings: &RedisSettings) -> Result<Self, Error> {
        let timeout = config::duration(settings.operation_timeout_ms)?;
        if settings.namespace.trim().is_empty() || settings.max_cache_entry_bytes == 0 {
            return Err(Error::Configuration(
                "Redis namespace and cache size must be nonempty",
            ));
        }
        let url = config::environment(&settings.url_environment)?;
        let client =
            redis::Client::open(url).map_err(|_| Error::Configuration("invalid Redis URL"))?;
        let connection = tokio::time::timeout(timeout, client.get_connection_manager())
            .await
            .map_err(|_| Error::Timeout)??;
        Ok(Self {
            connection,
            // All keys in one namespace share a Redis Cluster hash slot. Multi-key
            // admission is atomic; deployments can shard by independent budget domain.
            prefix: format!("llm-unify:{{{}}}", hash(&settings.namespace)),
            timeout,
            max_cache_entry_bytes: settings.max_cache_entry_bytes,
        })
    }

    pub async fn health(&self) -> Result<(), Error> {
        let _: String = tokio::time::timeout(
            self.timeout,
            redis::cmd("PING").query_async(&mut self.connection.clone()),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        Ok(())
    }

    /// Atomically consume all limits, or none. Repeating an admitted operation ID
    /// is idempotent for the longest configured window. Use one ID per admission.
    /// A denied operation may be tried again after retry_after_ms.
    pub async fn admit(
        &self,
        operation_id: &str,
        limits: &[RateLimit],
    ) -> Result<Admission, Error> {
        identifier(operation_id)?;
        if limits.is_empty() {
            return Err(Error::Invalid("at least one rate limit is required"));
        }
        let mut sorted: Vec<_> = limits.iter().collect();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        if sorted.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(Error::Invalid("duplicate rate limit identity"));
        }
        let script = redis::Script::new(include_str!("../scripts/admit.lua"));
        let mut invocation = script.prepare_invoke();
        invocation.key(format!("{}:admission:{}", self.prefix, hash(operation_id)));
        let mut ttl = 0;
        for limit in &sorted {
            identifier(&limit.id)?;
            for value in [limit.limit, limit.amount, limit.window_ms] {
                positive_integer(value)?;
            }
            ttl = ttl.max(limit.window_ms);
            let key = serde_json::to_vec(&(&limit.id, limit.window_ms))
                .map_err(|_| Error::Invalid("invalid rate limit"))?;
            invocation.key(format!("{}:limit:{}", self.prefix, hash(key)));
        }
        let fingerprint =
            hash(serde_json::to_vec(&sorted).map_err(|_| Error::Invalid("invalid rate limit"))?);
        invocation.arg(fingerprint).arg(ttl);
        for limit in sorted {
            invocation
                .arg(limit.limit)
                .arg(limit.amount)
                .arg(limit.window_ms);
        }
        let (code, retry): (i64, u64) = tokio::time::timeout(
            self.timeout,
            invocation.invoke_async(&mut self.connection.clone()),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        match code {
            0 => Ok(Admission {
                allowed: false,
                retry_after_ms: retry,
            }),
            1 => Ok(Admission {
                allowed: true,
                retry_after_ms: 0,
            }),
            2 => Err(Error::Conflict),
            _ => Err(Error::Redis),
        }
    }

    pub async fn cache_put(&self, key: &CacheKey, bytes: &[u8], ttl_ms: u64) -> Result<(), Error> {
        positive_integer(ttl_ms)?;
        if bytes.len() > self.max_cache_entry_bytes {
            return Err(Error::Invalid("cache entry exceeds the configured limit"));
        }
        let _: () = tokio::time::timeout(
            self.timeout,
            redis::cmd("SET")
                .arg(format!("{}:cache:{}", self.prefix, key.digest))
                .arg(bytes)
                .arg("PX")
                .arg(ttl_ms)
                .query_async(&mut self.connection.clone()),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        Ok(())
    }

    pub async fn cache_get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>, Error> {
        let script = redis::Script::new(include_str!("../scripts/cache_get.lua"));
        Ok(tokio::time::timeout(
            self.timeout,
            script
                .key(format!("{}:cache:{}", self.prefix, key.digest))
                .arg(self.max_cache_entry_bytes)
                .invoke_async(&mut self.connection.clone()),
        )
        .await
        .map_err(|_| Error::Timeout)??)
    }

    pub async fn cache_delete(&self, key: &CacheKey) -> Result<(), Error> {
        let _: i64 = tokio::time::timeout(
            self.timeout,
            redis::cmd("DEL")
                .arg(format!("{}:cache:{}", self.prefix, key.digest))
                .query_async(&mut self.connection.clone()),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_scope_and_request_both_partition_cache_keys() {
        // Given: identical requests from different authorization domains.
        // When: constructing cache keys.
        // Then: both scope and request affect the key without exposing either.
        let a = CacheKey::new("tenant-a", &"0".repeat(64)).unwrap();
        let b = CacheKey::new("tenant-b", &"0".repeat(64)).unwrap();
        let c = CacheKey::new("tenant-a", &"1".repeat(64)).unwrap();
        assert_ne!(a.digest, b.digest);
        assert_ne!(a.digest, c.digest);
        assert!(!a.digest.contains("tenant"));
    }

    #[test]
    fn ambiguous_keys_and_inexact_lua_numbers_are_rejected() {
        // Given: empty scopes, malformed fingerprints and nonrepresentable counters.
        // When: constructing cache keys or checking Lua integers.
        // Then: invalid inputs cannot reach Redis.
        assert!(CacheKey::new("", &"0".repeat(64)).is_err());
        assert!(CacheKey::new("tenant", "bad").is_err());
        assert!(positive_integer(0).is_err());
        assert!(positive_integer(MAX_EXACT_INTEGER + 1).is_err());
        assert!(positive_integer(MAX_EXACT_INTEGER).is_ok());
    }
}
