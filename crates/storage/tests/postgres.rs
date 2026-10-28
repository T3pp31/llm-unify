#![allow(dead_code)] // shared helpers also serve the Redis integration target.
mod common;

use common::*;
use llm_unify_storage::{AttemptState, Error, Outcome, Settlement, Usage};

fn settlement(actual: i64) -> Settlement {
    Settlement {
        actual: money(actual),
        outcome: Outcome::Succeeded,
        http_status: Some(200),
        usage: Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            raw: Some(
                serde_json::json!({"prompt_tokens":10,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":3},"provider_specific":{"units":2}}),
            ),
        },
        upstream_request_id: Some("upstream-fixture".into()),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn repeated_and_parallel_migrations_preserve_balances() {
    // Given: a migrated database and a persistent account.
    let first = ledger().await;
    let account = id();
    first
        .create_account(&account, Some(money(100)))
        .await
        .unwrap();
    let attempt = spec(std::slice::from_ref(&account), 10);
    first.prepare(attempt).await.unwrap();
    // When: another instance runs the same migrations concurrently.
    let second = ledger().await;
    let (a, b) = tokio::join!(first.migrate(), second.migrate());
    a.unwrap();
    b.unwrap();
    // Then: both remain healthy and existing balances are untouched.
    second.health().await.unwrap();
    assert_eq!(second.account(&account).await.unwrap().reserved, money(10));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn exact_budget_is_admitted_and_one_unit_over_is_denied() {
    // Given: a balance equal to one reservation.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, Some(money(10))).await.unwrap();
    let attempt = spec(std::slice::from_ref(&account), 10);
    // When: reserving the exact limit, then one more unit.
    db.prepare(attempt.clone()).await.unwrap();
    let denied = spec(std::slice::from_ref(&account), 1);
    assert_eq!(
        db.prepare(denied.clone()).await.unwrap_err(),
        Error::BudgetExceeded
    );
    // Then: the failed attempt is absent and releasing restores the exact balance.
    assert_eq!(db.attempt(&denied.id).await.unwrap_err(), Error::NotFound);
    db.release(&attempt.id).await.unwrap();
    db.release(&attempt.id).await.unwrap();
    assert_eq!(db.account(&account).await.unwrap().reserved, money(0));
    assert_eq!(
        db.attempt(&attempt.id).await.unwrap().state,
        AttemptState::Released
    );
    assert_eq!(db.dispatch(&attempt.id).await, Err(Error::State));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn failed_multi_account_reservation_rolls_back_every_account() {
    // Given: lexically first account has room; last account has no budget.
    let db = ledger().await;
    let prefix = id();
    let a = format!("{prefix}-a");
    let z = format!("{prefix}-z");
    db.create_account(&a, Some(money(100))).await.unwrap();
    db.create_account(&z, Some(money(0))).await.unwrap();
    // When: a request reserves both, deliberately reversing input lock order.
    let attempt = spec(&[z.clone(), a.clone()], 1);
    assert_eq!(
        db.prepare(attempt.clone()).await.unwrap_err(),
        Error::BudgetExceeded
    );
    // Then: all writes, including the new attempt, were rolled back.
    for account in [&a, &z] {
        assert_eq!(db.account(account).await.unwrap().reserved, money(0));
    }
    assert_eq!(db.attempt(&attempt.id).await.unwrap_err(), Error::NotFound);
    assert_eq!(
        db.prepare(spec(&[a.clone(), id()], 1)).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(db.account(&a).await.unwrap().reserved, money(0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn two_pools_cannot_overspend_a_shared_budget_under_concurrency() {
    // Given: two independent pools representing two Proxy instances.
    let a = ledger().await;
    let b = ledger().await;
    let account = id();
    let concurrency = settings()["concurrency"].as_u64().unwrap();
    let budget = (concurrency / 10) as i64;
    a.create_account(&account, Some(money(budget)))
        .await
        .unwrap();
    // When: all callers reserve against the same shared budget.
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..concurrency {
        let db = if index % 2 == 0 { a.clone() } else { b.clone() };
        let attempt = spec(std::slice::from_ref(&account), 1);
        tasks.spawn(async move { db.prepare(attempt).await });
    }
    let mut accepted = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(attempt) => accepted.push(attempt.id),
            Err(error) => assert_eq!(error, Error::BudgetExceeded),
        }
    }
    // Then: the exact budget was accepted and all releases are durable.
    assert_eq!(accepted.len() as i64, budget);
    assert_eq!(b.account(&account).await.unwrap().reserved, money(budget));
    for id in accepted {
        a.release(&id).await.unwrap();
    }
    assert_eq!(b.account(&account).await.unwrap().reserved, money(0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn concurrent_idempotent_prepare_has_one_reservation_and_one_send_claim() {
    // Given: one logical upstream attempt submitted through two instances.
    let a = ledger().await;
    let b = ledger().await;
    let account = id();
    a.create_account(&account, Some(money(10))).await.unwrap();
    let attempt = spec(std::slice::from_ref(&account), 10);
    // When: identical preparations and dispatch claims race.
    let (first, second) = tokio::join!(a.prepare(attempt.clone()), b.prepare(attempt.clone()));
    assert_eq!(first.unwrap(), second.unwrap());
    let (first, second) = tokio::join!(a.dispatch(&attempt.id), b.dispatch(&attempt.id));
    // Then: only one send is authorized and only one amount was reserved.
    assert_eq!(
        [first.is_ok(), second.is_ok()]
            .iter()
            .filter(|value| **value)
            .count(),
        1
    );
    assert!(first == Err(Error::State) || second == Err(Error::State));
    assert_eq!(a.account(&account).await.unwrap().reserved, money(10));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn settlement_retries_do_not_double_charge_and_changed_input_conflicts() {
    // Given: a dispatched attempt against key and team accounts.
    let db = ledger().await;
    let accounts = [id(), id()];
    for account in &accounts {
        db.create_account(account, None).await.unwrap();
    }
    let attempt = spec(&accounts, 100);
    db.prepare(attempt.clone()).await.unwrap();
    db.dispatch(&attempt.id).await.unwrap();
    // When: the same finalization is delivered more than once.
    let bill = settlement(25);
    let (a, b) = tokio::join!(
        db.settle(&attempt.id, bill.clone()),
        db.settle(&attempt.id, bill.clone())
    );
    a.unwrap();
    b.unwrap();
    // Then: spend is charged once per account, and changed bills are rejected.
    for account in &accounts {
        let account = db.account(account).await.unwrap();
        assert_eq!(account.spent, money(25));
        assert_eq!(account.reserved, money(0));
    }
    assert_eq!(
        db.settle(&attempt.id, settlement(26)).await,
        Err(Error::Conflict)
    );
    let saved = db.attempt(&attempt.id).await.unwrap();
    assert_eq!(saved.usage, bill.usage);
    assert_eq!(saved.actual, Some(money(25)));
    assert_eq!(db.release(&attempt.id).await, Err(Error::State));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn restart_keeps_unknown_dispatched_cost_reserved_until_reconciliation() {
    // Given: the first instance persisted its send claim then lost the response.
    let first = ledger().await;
    let account = id();
    first
        .create_account(&account, Some(money(10)))
        .await
        .unwrap();
    let attempt = spec(std::slice::from_ref(&account), 10);
    first.prepare(attempt.clone()).await.unwrap();
    first.dispatch(&attempt.id).await.unwrap();
    first.close().await;
    // When: a new process-level pool loads the incomplete attempt.
    let second = ledger().await;
    assert_eq!(
        second.attempt(&attempt.id).await.unwrap().state,
        AttemptState::Dispatched
    );
    second.mark_uncertain(&attempt.id).await.unwrap();
    // Then: no expiry frees its reservation or authorizes a repeated send.
    assert_eq!(second.release(&attempt.id).await, Err(Error::State));
    assert_eq!(second.dispatch(&attempt.id).await, Err(Error::State));
    assert_eq!(
        second
            .prepare(spec(std::slice::from_ref(&account), 1))
            .await
            .unwrap_err(),
        Error::BudgetExceeded
    );
    assert!(
        second
            .unresolved(i64::MAX)
            .await
            .unwrap()
            .contains(&attempt.id)
    );
    second.settle(&attempt.id, settlement(7)).await.unwrap();
    assert_eq!(second.account(&account).await.unwrap().spent, money(7));
    assert_eq!(second.account(&account).await.unwrap().reserved, money(0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn actual_charge_over_reservation_is_recorded_even_on_failed_upstream() {
    // Given: an upstream failure with a verified charge above the reservation.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, Some(money(10))).await.unwrap();
    let attempt = spec(std::slice::from_ref(&account), 5);
    db.prepare(attempt.clone()).await.unwrap();
    db.dispatch(&attempt.id).await.unwrap();
    // When: recording actual spend rather than capping it at the local budget.
    let mut bill = settlement(11);
    bill.outcome = Outcome::Failed;
    bill.http_status = Some(500);
    bill.usage = Usage {
        prompt_tokens: None,
        completion_tokens: None,
        raw: None,
    };
    db.settle(&attempt.id, bill).await.unwrap();
    // Then: accounting is accurate and new requests are denied.
    assert_eq!(db.account(&account).await.unwrap().spent, money(11));
    assert_eq!(db.account(&account).await.unwrap().reserved, money(0));
    assert_eq!(
        db.attempt(&attempt.id).await.unwrap().usage.prompt_tokens,
        None
    );
    assert_eq!(
        db.attempt(&attempt.id).await.unwrap().outcome,
        Some(Outcome::Failed)
    );
    assert_eq!(
        db.prepare(spec(std::slice::from_ref(&account), 0))
            .await
            .unwrap_err(),
        Error::BudgetExceeded
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn conflicting_request_identity_cannot_create_another_send() {
    // Given: one existing ordinal in a request.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, None).await.unwrap();
    let attempt = spec(std::slice::from_ref(&account), 1);
    db.prepare(attempt.clone()).await.unwrap();
    // When: reusing its attempt ID with changed content or its ordinal with another ID.
    let mut changed = attempt.clone();
    changed.model = "different".into();
    assert_eq!(db.prepare(changed).await.unwrap_err(), Error::Conflict);
    let mut changed = attempt.clone();
    changed.id = id();
    assert_eq!(db.prepare(changed).await.unwrap_err(), Error::Conflict);
    let mut changed = attempt.clone();
    changed.request_fingerprint = "1".repeat(64);
    assert_eq!(db.prepare(changed).await.unwrap_err(), Error::Conflict);
    // Then: the original reservation is unchanged.
    assert_eq!(db.account(&account).await.unwrap().reserved, money(1));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn invalid_input_and_invalid_state_never_change_balances() {
    // Given: a prepared reservation.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, None).await.unwrap();
    let attempt = spec(std::slice::from_ref(&account), 1);
    db.prepare(attempt.clone()).await.unwrap();
    // When: settling before dispatch, submitting invalid tokens, or duplicating accounts.
    assert_eq!(
        db.settle(&attempt.id, settlement(1)).await,
        Err(Error::State)
    );
    let mut bill = settlement(1);
    bill.usage.prompt_tokens = Some(-1);
    assert!(matches!(
        db.settle(&attempt.id, bill).await,
        Err(Error::Invalid(_))
    ));
    let mut bill = settlement(1);
    bill.usage.raw = Some(serde_json::json!(["invalid-usage"]));
    assert!(matches!(
        db.settle(&attempt.id, bill).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        db.prepare(spec(&[account.clone(), account.clone()], 1))
            .await,
        Err(Error::Invalid(_))
    ));
    let mut invalid = spec(std::slice::from_ref(&account), 1);
    invalid.ordinal = -1;
    assert!(matches!(db.prepare(invalid).await, Err(Error::Invalid(_))));
    let mut invalid = spec(std::slice::from_ref(&account), 1);
    invalid.id.clear();
    assert!(matches!(db.prepare(invalid).await, Err(Error::Invalid(_))));
    assert!(matches!(db.unresolved(0).await, Err(Error::Invalid(_))));
    assert_eq!(db.release("not-an-attempt").await, Err(Error::NotFound));
    // Then: the initial amount is still reserved and nothing was charged.
    assert_eq!(db.account(&account).await.unwrap().spent, money(0));
    assert_eq!(db.account(&account).await.unwrap().reserved, money(1));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn overflow_and_changed_account_creation_are_not_silently_accepted() {
    // Given: an unlimited account reserved to the maximum representable balance.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, None).await.unwrap();
    let attempt = spec(std::slice::from_ref(&account), i64::MAX);
    db.prepare(attempt.clone()).await.unwrap();
    // When: adding another unit, changing an idempotent create, or lowering its budget.
    assert!(matches!(
        db.prepare(spec(std::slice::from_ref(&account), 1)).await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        db.create_account(&account, Some(money(1)))
            .await
            .unwrap_err(),
        Error::Conflict
    );
    db.set_budget(&account, Some(money(0))).await.unwrap();
    // Then: the limit update preserves reservations; future requests are denied.
    assert_eq!(
        db.account(&account).await.unwrap().reserved,
        money(i64::MAX)
    );
    assert_eq!(
        db.prepare(spec(std::slice::from_ref(&account), 0))
            .await
            .unwrap_err(),
        Error::BudgetExceeded
    );
    db.release(&attempt.id).await.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn failed_settlement_overflow_keeps_reservations_and_can_be_reconciled() {
    // Given: concurrent reservations filling the representable aggregate balance.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, None).await.unwrap();
    let large = spec(std::slice::from_ref(&account), i64::MAX - 2);
    let small = spec(std::slice::from_ref(&account), 2);
    db.prepare(large.clone()).await.unwrap();
    db.prepare(small.clone()).await.unwrap();
    db.dispatch(&small.id).await.unwrap();
    // When: actual spend would overflow while another reservation is active.
    assert!(matches!(
        db.settle(&small.id, settlement(3)).await,
        Err(Error::Invalid(_))
    ));
    // Then: the transaction preserved the original balance and dispatched state.
    assert_eq!(db.account(&account).await.unwrap().spent, money(0));
    assert_eq!(
        db.account(&account).await.unwrap().reserved,
        money(i64::MAX)
    );
    assert_eq!(
        db.attempt(&small.id).await.unwrap().state,
        AttemptState::Dispatched
    );
    db.release(&large.id).await.unwrap();
    db.settle(&small.id, settlement(3)).await.unwrap();
    assert_eq!(db.account(&account).await.unwrap().spent, money(3));
    assert_eq!(db.account(&account).await.unwrap().reserved, money(0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run tools/parity/run_storage.py"]
async fn retry_ordinals_preserve_each_external_send_charge() {
    // Given: one logical request with two separately dispatched upstream sends.
    let db = ledger().await;
    let account = id();
    db.create_account(&account, Some(money(10))).await.unwrap();
    let first = spec(std::slice::from_ref(&account), 5);
    let second = llm_unify_storage::AttemptSpec {
        id: id(),
        ordinal: 1,
        ..first.clone()
    };
    db.prepare(first.clone()).await.unwrap();
    db.prepare(second.clone()).await.unwrap();
    db.dispatch(&first.id).await.unwrap();
    db.dispatch(&second.id).await.unwrap();
    // When: the failed send had a known charge, followed by a successful retry.
    let mut failure = settlement(2);
    failure.outcome = Outcome::Failed;
    failure.http_status = Some(500);
    db.settle(&first.id, failure).await.unwrap();
    db.settle(&second.id, settlement(3)).await.unwrap();
    // Then: neither failure nor retry disappears from spend or attempt history.
    assert_eq!(db.account(&account).await.unwrap().spent, money(5));
    assert_eq!(db.account(&account).await.unwrap().reserved, money(0));
    assert_eq!(
        db.attempt(&first.id).await.unwrap().outcome,
        Some(Outcome::Failed)
    );
    assert_eq!(db.attempt(&second.id).await.unwrap().ordinal, 1);
}
