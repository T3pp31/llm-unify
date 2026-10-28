use llm_unify_storage::{
    AttemptSpec, Ledger, Money, PostgresSettings, RedisSettings, shared::Shared,
};
use serde_json::Value;

pub fn settings() -> Value {
    serde_json::from_str(include_str!("../../../../config/parity/storage-test.json")).unwrap()
}

pub async fn ledger() -> Ledger {
    let settings: PostgresSettings =
        serde_json::from_value(settings()["postgres"].clone()).unwrap();
    let ledger = Ledger::connect(&settings)
        .await
        .expect("run tools/parity/run_storage.py or set the configured PostgreSQL test URL");
    ledger.migrate().await.unwrap();
    ledger
}

pub async fn shared() -> Shared {
    let settings: RedisSettings = serde_json::from_value(settings()["redis"].clone()).unwrap();
    Shared::connect(&settings)
        .await
        .expect("run tools/parity/run_storage.py or set the configured Redis test URL")
}

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn money(nanos: i64) -> Money {
    Money::from_nanos(nanos).unwrap()
}

pub fn spec(accounts: &[String], reserve: i64) -> AttemptSpec {
    AttemptSpec {
        id: id(),
        request_id: id(),
        ordinal: 0,
        model: "test-model".into(),
        request_fingerprint: "0".repeat(64),
        accounts: accounts.to_vec(),
        reserve: money(reserve),
    }
}
