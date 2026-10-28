//! Controlled by run_storage.py: use only its disposable database and Redis.
use std::io::{BufRead, Write};

use llm_unify_storage::{
    AttemptSpec, AttemptState, Ledger, Money, Outcome, PostgresSettings, RedisSettings, Settlement,
    Usage,
    shared::{RateLimit, Shared},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Settings {
    postgres: PostgresSettings,
    redis: RedisSettings,
    rate_window_ms: u64,
}

fn signal(expected: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim() != expected {
        return Err("unexpected controller signal".into());
    }
    Ok(())
}

fn ready(message: &str) {
    println!("{message}");
    std::io::stdout().flush().expect("flush controller signal");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings: Settings =
        serde_json::from_str(include_str!("../../../config/parity/storage-test.json"))?;
    let ledger = Ledger::connect(&settings.postgres).await?;
    ledger.migrate().await?;
    let shared = Shared::connect(&settings.redis).await?;
    let account = uuid::Uuid::new_v4().to_string();
    ledger
        .create_account(&account, Some(Money::from_nanos(3)?))
        .await?;
    let attempt = AttemptSpec {
        id: uuid::Uuid::new_v4().to_string(),
        request_id: uuid::Uuid::new_v4().to_string(),
        ordinal: 0,
        model: "fault-test".into(),
        request_fingerprint: "0".repeat(64),
        accounts: vec![account.clone()],
        reserve: Money::from_nanos(3)?,
    };
    ledger.prepare(attempt.clone()).await?;
    ledger.dispatch(&attempt.id).await?;
    let limits = [RateLimit {
        id: account.clone(),
        window_ms: settings.rate_window_ms,
        limit: 1,
        amount: 1,
    }];
    assert!(
        shared
            .admit(&uuid::Uuid::new_v4().to_string(), &limits)
            .await?
            .allowed
    );
    ready("ready");
    signal("offline")?;
    // Given: established clients and a durable dispatched attempt.
    // When: the controller stops both services.
    // Then: operations fail; neither a local ledger nor a local rate limit is substituted.
    assert!(ledger.health().await.is_err());
    assert!(
        shared
            .admit(&uuid::Uuid::new_v4().to_string(), &limits)
            .await
            .is_err()
    );
    ready("offline-verified");
    signal("recovered")?;
    // Given: the same service storage restarted by the controller.
    // When: new clients reconnect and inspect the interrupted attempt.
    // Then: the reservation and persisted rate counter survive and settle exactly once.
    let recovered = Ledger::connect(&settings.postgres)
        .await
        .map_err(|_| "recovery PostgreSQL connection failed")?;
    recovered
        .health()
        .await
        .map_err(|_| "recovery schema health check failed")?;
    assert_eq!(
        recovered
            .attempt(&attempt.id)
            .await
            .map_err(|_| "recovery attempt read failed")?
            .state,
        AttemptState::Dispatched
    );
    assert_eq!(
        recovered.account(&account).await?.reserved,
        Money::from_nanos(3)?
    );
    let redis = Shared::connect(&settings.redis).await?;
    ledger
        .health()
        .await
        .map_err(|_| "original PostgreSQL pool did not recover")?;
    shared
        .health()
        .await
        .map_err(|_| "original Redis connection manager did not recover")?;
    assert!(
        !redis
            .admit(&uuid::Uuid::new_v4().to_string(), &limits)
            .await?
            .allowed
    );
    let bill = Settlement {
        actual: Money::from_nanos(2)?,
        outcome: Outcome::Succeeded,
        http_status: Some(200),
        usage: Usage {
            prompt_tokens: None,
            completion_tokens: None,
            raw: None,
        },
        upstream_request_id: None,
    };
    recovered.settle(&attempt.id, bill.clone()).await?;
    recovered.settle(&attempt.id, bill).await?;
    assert_eq!(
        recovered.account(&account).await?.spent,
        Money::from_nanos(2)?
    );
    assert_eq!(recovered.account(&account).await?.reserved, Money::ZERO);
    ready("recovery-verified");
    Ok(())
}
