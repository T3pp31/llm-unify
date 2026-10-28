#![allow(dead_code)] // shared helpers also serve the PostgreSQL integration target.
mod common;

use std::time::Duration;

use common::*;
use llm_unify_storage::{
    Error,
    shared::{CacheKey, RateLimit},
};

fn limit(id: &str, maximum: u64) -> RateLimit {
    RateLimit {
        id: id.into(),
        window_ms: settings()["rate_window_ms"].as_u64().unwrap(),
        limit: maximum,
        amount: 1,
    }
}

#[tokio::test]
#[ignore = "requires isolated Redis; run tools/parity/run_storage.py"]
async fn independent_connections_share_atomic_multi_limit_admission() {
    // Given: independent Proxy connections sharing one key and one team limit.
    let a = shared().await;
    let b = shared().await;
    let key = id();
    let team = id();
    let concurrency = settings()["concurrency"].as_u64().unwrap();
    let maximum = concurrency / 10;
    let mut tasks = tokio::task::JoinSet::new();
    // When: all callers race to consume both limits.
    for index in 0..concurrency {
        let shared = if index % 2 == 0 { a.clone() } else { b.clone() };
        let limits = vec![limit(&key, concurrency), limit(&team, maximum)];
        tasks.spawn(async move { shared.admit(&id(), &limits).await });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        if result.unwrap().unwrap().allowed {
            accepted += 1;
        }
    }
    // Then: exactly the shared team budget is admitted; denials did not consume key capacity.
    assert_eq!(accepted, maximum);
    let mut remaining = limit(&key, concurrency);
    remaining.amount = concurrency - maximum;
    assert!(a.admit(&id(), &[remaining]).await.unwrap().allowed);
    assert!(
        !b.admit(&id(), &[limit(&key, concurrency)])
            .await
            .unwrap()
            .allowed
    );
}

#[tokio::test]
#[ignore = "requires isolated Redis; run tools/parity/run_storage.py"]
async fn successful_operation_retry_is_idempotent_but_changed_input_conflicts() {
    // Given: an admitted operation that used all available capacity.
    let shared = shared().await;
    let operation = id();
    let key = id();
    let limits = [limit(&key, 1)];
    assert!(shared.admit(&operation, &limits).await.unwrap().allowed);
    // When: retrying identical input, then changed input with the same receipt ID.
    assert!(shared.admit(&operation, &limits).await.unwrap().allowed);
    let mut changed = limits[0].clone();
    changed.limit = 2;
    // Then: identical input is not charged twice and changed input is explicit conflict.
    assert_eq!(
        shared.admit(&operation, &[changed]).await,
        Err(Error::Conflict)
    );
    let denied = shared.admit(&id(), &limits).await.unwrap();
    assert!(!denied.allowed);
    assert!(denied.retry_after_ms > 0);
}

#[tokio::test]
#[ignore = "requires isolated Redis; run tools/parity/run_storage.py"]
async fn counter_and_cache_expiry_restore_capacity_without_client_clock() {
    // Given: a short server-expiring rate limit and cache entry.
    let shared = shared().await;
    let key = id();
    let ttl = settings()["expiry_test_ttl_ms"].as_u64().unwrap();
    let limit = RateLimit {
        window_ms: ttl,
        ..limit(&key, 1)
    };
    let cache = CacheKey::new(&key, &"0".repeat(64)).unwrap();
    shared.cache_put(&cache, b"fixture", ttl).await.unwrap();
    assert!(
        shared
            .admit(&id(), std::slice::from_ref(&limit))
            .await
            .unwrap()
            .allowed
    );
    // When: server TTL expires, without sending any client timestamp.
    tokio::time::sleep(Duration::from_millis(
        settings()["expiry_test_wait_ms"].as_u64().unwrap(),
    ))
    .await;
    // Then: capacity returns and the old cached content is absent.
    assert!(shared.admit(&id(), &[limit]).await.unwrap().allowed);
    assert_eq!(shared.cache_get(&cache).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires isolated Redis; run tools/parity/run_storage.py"]
async fn tenant_cache_is_shared_only_within_scope_and_invalidation_is_durable() {
    // Given: equal request fingerprints in two authorization domains.
    let a = shared().await;
    let b = shared().await;
    let tenant = id();
    let other = id();
    let fingerprint = "0".repeat(64);
    let key = CacheKey::new(&tenant, &fingerprint).unwrap();
    let isolated = CacheKey::new(&other, &fingerprint).unwrap();
    let ttl = settings()["rate_window_ms"].as_u64().unwrap();
    // When: storing a binary entry through one connection.
    a.cache_put(&key, &[0, 255, 10], ttl).await.unwrap();
    // Then: the same scope can read it and another tenant cannot.
    assert_eq!(b.cache_get(&key).await.unwrap(), Some(vec![0, 255, 10]));
    assert_eq!(b.cache_get(&isolated).await.unwrap(), None);
    b.cache_delete(&key).await.unwrap();
    assert_eq!(a.cache_get(&key).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires isolated Redis; run tools/parity/run_storage.py"]
async fn oversized_cache_and_invalid_limits_fail_without_mutations() {
    // Given: cache size and fixed-window policies from external settings.
    let shared = shared().await;
    let identity = id();
    let cache = CacheKey::new(&identity, &"0".repeat(64)).unwrap();
    let oversized = vec![
        0;
        settings()["redis"]["max_cache_entry_bytes"]
            .as_u64()
            .unwrap() as usize
            + 1
    ];
    // When: invalid cache writes, zero TTL and ambiguous rate policies are submitted.
    assert!(matches!(
        shared.cache_put(&cache, &oversized, 1).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        shared.cache_put(&cache, b"x", 0).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        shared.admit(&id(), &[]).await,
        Err(Error::Invalid(_))
    ));
    let rate = limit(&identity, 1);
    assert!(matches!(
        shared.admit(&id(), &[rate.clone(), rate.clone()]).await,
        Err(Error::Invalid(_))
    ));
    let invalid = RateLimit {
        amount: 0,
        ..rate.clone()
    };
    assert!(matches!(
        shared.admit(&id(), &[invalid]).await,
        Err(Error::Invalid(_))
    ));
    // Then: no cache was stored and all capacity remains.
    assert_eq!(shared.cache_get(&cache).await.unwrap(), None);
    assert!(shared.admit(&id(), &[rate]).await.unwrap().allowed);
}

#[tokio::test]
#[ignore = "requires isolated Redis; run tools/parity/run_storage.py"]
async fn corrupt_shared_counter_does_not_partially_consume_other_limits() {
    use sha2::{Digest, Sha256};
    // Given: one healthy bucket and a later bucket with noncanonical Redis integer data.
    let shared = shared().await;
    let config = settings();
    let prefix = id();
    let a = format!("{prefix}-a");
    let z = format!("{prefix}-z");
    let window = config["rate_window_ms"].as_u64().unwrap();
    let namespace = config["redis"]["namespace"].as_str().unwrap();
    let identity = serde_json::to_vec(&(&z, window)).unwrap();
    let key = format!(
        "llm-unify:{{{:x}}}:limit:{:x}",
        Sha256::digest(namespace),
        Sha256::digest(identity)
    );
    let url = std::env::var(config["redis"]["url_environment"].as_str().unwrap()).unwrap();
    let mut connection = redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg("01")
        .arg("PX")
        .arg(window)
        .query_async(&mut connection)
        .await
        .unwrap();
    // When: admitting a multi-bucket request, including the corrupt bucket.
    assert_eq!(
        shared.admit(&id(), &[limit(&a, 1), limit(&z, 1)]).await,
        Err(Error::Redis)
    );
    // Then: the healthy bucket still has all capacity; no write preceded validation.
    let _: i64 = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(
        shared
            .admit(&id(), &[limit(&a, 1), limit(&z, 1)])
            .await
            .unwrap()
            .allowed
    );
}
