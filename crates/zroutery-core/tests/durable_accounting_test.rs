//! Settlement correctness against isolated SQLite stores, never live user data.

use std::sync::Arc;

use axum::response::IntoResponse;
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::accounting::{
    AttemptFact, AttemptState, PersistenceState, RequestAccounting, SettlementFact,
    SETTLEMENT_SCHEMA_VERSION,
};
use zroutery_core::billing::{Cost, Pricing};
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope, Ledger};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::{Dialect, Usage};
use zroutery_core::router::Candidate;
use zroutery_core::runtime_store::{RuntimeStore, SCHEMA_VERSION};
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::RecordBuilder;

fn candidate() -> Candidate {
    let mut model = ModelEntry::for_upstream("relay", "mock", Some(ModelTier::Standard));
    model.display_name = Some("Readable mock".into());
    model.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    Candidate {
        exposed_id: model.exposed_id(),
        entry: model,
        provider: ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible),
        degraded: false,
        credential: None,
        rate_quote: None,
    }
}

fn completed(id: &str, request: &str, candidate: &Candidate) -> AttemptFact {
    let mut attempt = AttemptFact::new(id.into(), Some(request.into()), candidate, true);
    attempt.observe_usage(Usage {
        input_tokens: 10_000,
        output_tokens: 200,
        ..Default::default()
    });
    attempt.finish(AttemptState::Served);
    attempt
}

fn settlement(request: &str, attempts: Vec<AttemptFact>) -> SettlementFact {
    let mut builder = RecordBuilder::new(Dialect::OpenAI, "standard-class", false);
    builder.resolved("relay-mock", "Relay").attempt();
    let mut record = builder.finish(10);
    record.id = request.into();
    record.error = Some("not persisted: secret or body".into());
    record.accounting = Some(RequestAccounting::from_attempts(
        &attempts,
        PersistenceState::Durable,
    ));
    SettlementFact {
        schema_version: SETTLEMENT_SCHEMA_VERSION,
        request: record,
        attempts,
    }
}

fn daily(ledger: &Ledger) -> f64 {
    ledger.spent(
        &Budget::new(BudgetScope::Global, BudgetPeriod::Day, "USD", 100.0),
        chrono::Local::now(),
    )
}

#[test]
fn schema_migrates_legacy_data_transactionally_and_refuses_future_writers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.sqlite3");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE wallet_holds(scope TEXT PRIMARY KEY, record TEXT NOT NULL);
            INSERT INTO wallet_holds VALUES ('kept', 'original');",
        )
        .unwrap();
    }
    let store = RuntimeStore::open(&path).unwrap();
    assert_eq!(store.wallet_holds().unwrap(), ["original"]);
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(RuntimeStore::open(&path)
        .unwrap_err()
        .contains("newer than supported"));
    assert_eq!(
        db.query_row("SELECT record FROM wallet_holds", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "original"
    );
}

#[test]
fn opening_spend_is_imported_once_and_history_never_changes_actual_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.sqlite3");
    let store = RuntimeStore::open(&path).unwrap();
    let mut opening = Ledger::new();
    opening.charge(
        chrono::Local::now(),
        "relay",
        None,
        &Cost {
            currency: "USD".into(),
            amount: 12.0,
        },
    );
    let history = settlement("legacy", vec![]).request;
    store.append_usage(&[history]).unwrap();
    assert_eq!(
        daily(
            &store
                .initialize_accounting(&opening, "legacy opening source")
                .unwrap()
        ),
        12.0
    );
    let fact = settlement("new", vec![completed("attempt-1", "new", &candidate())]);
    let cost = fact.attempts[0].cost.as_ref().unwrap().amount;
    let first = store.settle(&fact).unwrap();
    assert!(first.inserted);
    assert!((daily(&first.ledger) - 12.0 - cost).abs() < 1e-12);
    assert!(!store.settle(&fact).unwrap().inserted);
    assert!((daily(&store.accounting_ledger().unwrap().unwrap()) - 12.0 - cost).abs() < 1e-12);
    store.clear_usage(None).unwrap();
    assert_eq!(
        daily(&store.accounting_ledger().unwrap().unwrap()),
        daily(&first.ledger)
    );
    drop(store);
    let reopened = RuntimeStore::open(&path).unwrap();
    assert_eq!(
        reopened
            .initialize_accounting(&Ledger::new(), "must not replace")
            .unwrap(),
        first.ledger
    );
    assert!(reopened.usage().unwrap().is_empty());
    assert!(
        !reopened.settle(&fact).unwrap().inserted,
        "clearing history never removes settlement dedup"
    );
    assert!(
        reopened.usage().unwrap().is_empty(),
        "retry cannot republish explicitly cleared history"
    );
}

#[test]
fn failed_attempt_known_cost_survives_and_late_finalization_cannot_rebind_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = RuntimeStore::open(&directory.path().join("runtime.sqlite3")).unwrap();
    store.initialize_accounting(&Ledger::new(), "test").unwrap();
    let candidate = candidate();
    let started = AttemptFact::new("attempt".into(), Some("request".into()), &candidate, true);
    store.record_attempt(&started).unwrap();
    assert_eq!(store.unresolved_attempts().unwrap().len(), 1);
    let mut failed = started.clone();
    failed.observe_usage(Usage {
        input_tokens: 200,
        ..Default::default()
    });
    failed.finish(AttemptState::Failed);
    let charged = store.record_attempt(&failed).unwrap();
    assert!(daily(&charged) > 0.0);
    let fact = settlement("request", vec![failed.clone()]);
    assert_eq!(
        store.settle(&fact).unwrap().ledger,
        charged,
        "known failure is not charged again"
    );
    let saved = store.settlement("request").unwrap().unwrap();
    assert!(saved.request.error.is_none());
    assert_eq!(saved.attempts[0].execution.model_name, "Readable mock");
    let mut wrong = failed;
    wrong.execution.provider_id = "different".into();
    assert!(store.record_attempt(&wrong).is_err());
    assert_eq!(store.accounting_ledger().unwrap().unwrap(), charged);
}

#[test]
fn invalid_attempt_rolls_back_request_usage_and_every_charge() {
    let directory = tempfile::tempdir().unwrap();
    let store = RuntimeStore::open(&directory.path().join("runtime.sqlite3")).unwrap();
    store.initialize_accounting(&Ledger::new(), "test").unwrap();
    let first = completed("first", "request", &candidate());
    let mut invalid = completed("invalid", "request", &candidate());
    invalid.cost.as_mut().unwrap().amount = -1.0;
    assert!(store
        .settle(&settlement("request", vec![first, invalid]))
        .is_err());
    assert!(store.usage().unwrap().is_empty());
    assert!(store.settlement("request").unwrap().is_none());
    assert_eq!(daily(&store.accounting_ledger().unwrap().unwrap()), 0.0);
}

#[tokio::test]
async fn terminal_response_has_committed_usage_and_budget_without_a_timer_flush() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let app = axum::Router::new().route("/v1/chat/completions", post(|Json(_body): Json<Value>| async {
            Json(json!({"id":"answer","model":"mock","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":10_000,"completion_tokens":200}})).into_response()
        }));
        axum::serve(listener, app).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.sqlite3");
    let store = Arc::new(RuntimeStore::open(&path).unwrap());
    let candidate = candidate();
    let mut config = AppConfig::default();
    config.server.require_auth = false;
    config.server.port = 0;
    config.server.autostart = false;
    let mut provider = candidate.provider.clone();
    provider.base_url = format!("http://{upstream}");
    provider.key_ref.clear();
    config.providers = vec![provider];
    config.models = vec![candidate.entry];
    let app = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    app.require_accounting();
    app.attach_accounting(store.clone(), &Ledger::new(), "isolated test")
        .unwrap();
    let handle = ServerHandle::start(app.clone()).await.unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", handle.addr))
        .json(&json!({"model":"standard-class","messages":[{"role":"user","content":"test"}]}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let _body: Value = response.json().await.unwrap();
    let disk = RuntimeStore::open(&path).unwrap();
    let usage = disk.usage().unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(
        usage[0].accounting.as_ref().unwrap().persistence,
        PersistenceState::Durable
    );
    assert!(disk.settlement(&usage[0].id).unwrap().is_some());
    assert!((daily(&disk.accounting_ledger().unwrap().unwrap()) - 0.033).abs() < 1e-12);
    assert_eq!(app.stats().summary().requests, 1);
    assert!(disk.unresolved_attempts().unwrap().is_empty());
    handle.stop().await;
    mock.abort();
}

#[tokio::test]
async fn required_storage_failure_never_silently_starts_volatile_serving() {
    let app = Arc::new(AppState::new(
        AppConfig::default(),
        Arc::new(MemorySecretStore::new()),
    ));
    app.require_accounting();
    assert!(ServerHandle::start(app.clone()).await.is_err());
    app.accounting_failed("cannot open runtime storage".into());
    assert!(!app.accounting_status().available);
    assert!(app.accounting_status().required);
    let error = zroutery_core::Error::Persistence;
    assert!(!error.is_retryable());
    assert!(!error.counts_against_health());
}
