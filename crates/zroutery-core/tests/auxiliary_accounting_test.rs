//! Durable auxiliary-attempt accounting through the real gateway.
//!
//! One mock provider plays the blind main model, the vision model and a
//! failing candidate; a second provider entry receives the descriptions so
//! their ledger rows are readable on a provider the main request never
//! touches. Every scenario here runs with a file-backed runtime store and
//! asserts against what is actually on the disk, not against in-memory
//! projections:
//!
//! * one description send is one immutable attempt identity, quote and charge;
//! * a failed description with reported usage keeps its known cost, once;
//! * a description without a usage report stays unknown — never a zero;
//! * a main-candidate failover charges each description it actually sent,
//!   exactly once each, and reopening the storage reads the same facts;
//! * the storage carries neither the description body, the user's question,
//!   the image reference nor any key bytes.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::accounting::{AttemptFact, AttemptState};
use zroutery_core::billing::Pricing;
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope, Ledger};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind, VisionConfig,
};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};

// ------------------------------------------------------------------ mock upstream

#[derive(Default)]
struct MockInner {
    received: Vec<Value>,
}

#[derive(Clone, Default)]
struct Mock {
    inner: Arc<Mutex<MockInner>>,
}

impl Mock {
    fn bodies(&self) -> Vec<Value> {
        self.inner.lock().unwrap().received.clone()
    }
}

/// Dispatch on the upstream model name:
/// * `eyes*`            -> the vision model: describes the image, reports usage;
/// * `nousage-eyes*`    -> describes the image but reports no usage at all;
/// * `fail-usage-eyes*` -> a 500 whose body still reports usage it consumed;
/// * `blind*`           -> rejects images (400), answers text requests;
/// * `flaky*`           -> always 500, so the main request has to fail over.
async fn mock_openai_chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.inner.lock().unwrap().received.push(body.clone());

    let model = body["model"].as_str().unwrap_or("").to_string();
    let has_image = body["messages"]
        .as_array()
        .map(|messages| {
            messages.iter().any(|m| {
                m.get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b.get("type").and_then(Value::as_str) == Some("image_url"))
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if model.starts_with("blind") && has_image {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message":
                "this model does not support image input; images are unsupported",
                "type": "invalid_request_error"}})),
        )
            .into_response();
    }
    if model.starts_with("flaky") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "flaky model exploded"}})),
        )
            .into_response();
    }
    if model.starts_with("fail-usage-eyes") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {"message": "vision model exploded"},
                // A relay that consumed work before failing can still report it.
                "usage": {"prompt_tokens": 5, "completion_tokens": 3},
            })),
        )
            .into_response();
    }

    let content = if model.starts_with("eyes") {
        "A chart with a rising line, titled \"Revenue\"."
    } else {
        "text answer"
    };
    let mut answer = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7},
    });
    if model.starts_with("nousage-eyes") {
        answer.as_object_mut().unwrap().remove("usage");
    }
    Json(answer).into_response()
}

async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock::default();
    let app = axum::Router::new()
        .route("/chat/completions", post(mock_openai_chat))
        .route("/v1/chat/completions", post(mock_openai_chat))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

// ------------------------------------------------------------------ fixtures

const TOKEN: &str = "zr-test-token";

/// 11 prompt + 7 completion tokens per description at 100 USD/Mtok.
const ONE_DESCRIPTION_USD: f64 = 18.0 * 100.0 / 1_000_000.0;
/// The failed description's 5 + 3 tokens at the same tariff.
const FAILED_DESCRIPTION_USD: f64 = 8.0 * 100.0 / 1_000_000.0;

fn image_request() -> Value {
    json!({
        "model": "sonnet-class",
        "max_tokens": 128,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.com/chart.png"}},
                {"type": "text", "text": "what does this chart show"}
            ]
        }]
    })
}

fn two_image_request() -> Value {
    json!({
        "model": "sonnet-class",
        "max_tokens": 128,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.com/one.png"}},
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.com/two.png"}},
                {"type": "text", "text": "compare these two charts"}
            ]
        }]
    })
}

/// The main model on provider `p`, the vision model on provider `v`, both
/// served by the same mock: the descriptions settle on a provider the main
/// request never touches, so their rows cannot be confused with anything else.
fn config_for(mock: SocketAddr, vision_upstream: &str) -> AppConfig {
    let mut main = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    main.base_url = format!("http://{mock}");
    main.key_ref = "provider:p".into();
    main.timeout_secs = 10;

    let mut vision = ProviderConfig::new("v", "V", ProviderKind::OpenAICompatible);
    vision.base_url = format!("http://{mock}");
    vision.key_ref = "provider:v".into();
    vision.timeout_secs = 10;

    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![main, vision];
    cfg.models = vec![
        ModelEntry::for_upstream("p", "blind-model", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("v", vision_upstream, Some(ModelTier::Fast)),
    ];
    // The main tariff is explicit and separate, so the two attributions
    // cannot be folded into one number by accident.
    cfg.models[0].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.models[1].pricing = Some(Pricing::new("USD", 100.0, 100.0));
    cfg.models[1].capabilities.vision = true;
    let vision_model = cfg.models[1].exposed_id();
    cfg.vision = VisionConfig {
        enabled: true,
        model: Some(vision_model),
        ..VisionConfig::default()
    };
    cfg
}

fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(
        MemorySecretStore::new()
            .with("provider:p", "sk-p")
            .with("provider:v", "sk-v"),
    )
}

fn calls_to(bodies: &[Value], prefix: &str) -> usize {
    bodies
        .iter()
        .filter(|body| {
            body["model"]
                .as_str()
                .is_some_and(|model| model.starts_with(prefix))
        })
        .count()
}

fn bytes_contain(haystack: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    client: reqwest::Client,
    mock: Mock,
    store: Arc<RuntimeStore>,
    db_path: PathBuf,
    /// Declared last so the directory outlives the server on drop.
    _directory: tempfile::TempDir,
}

impl Harness {
    async fn durable(cfg: AppConfig, mock: Mock) -> Harness {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("runtime.sqlite3");
        let store = Arc::new(RuntimeStore::open(&db_path).unwrap());
        let state = Arc::new(AppState::new(cfg, secrets()));
        state
            .attach_accounting(
                Arc::clone(&store),
                &Ledger::new(),
                "auxiliary accounting test",
            )
            .unwrap();
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        let base = format!("http://{}", server.addr);
        Harness {
            base,
            server: Some(server),
            client: reqwest::Client::new(),
            mock,
            store,
            db_path,
            _directory: directory,
        }
    }

    async fn post(&self, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    /// The auxiliary attempts on the disk: request-less facts are exactly the
    /// descriptions, never the settled main request's attempts.
    fn auxiliary_facts(&self) -> Vec<AttemptFact> {
        let db = rusqlite::Connection::open(&self.db_path).unwrap();
        let mut statement = db
            .prepare("SELECT record FROM attempt_facts WHERE request_id IS NULL ORDER BY id")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        rows.map(|row| serde_json::from_str::<AttemptFact>(&row.unwrap()).unwrap())
            .collect()
    }

    /// Charge rows that belong to the auxiliary facts — the main request's
    /// own attempts have charge rows too and must not be counted here.
    fn auxiliary_charge_count(&self) -> usize {
        let db = rusqlite::Connection::open(&self.db_path).unwrap();
        db.query_row(
            "SELECT count(*) FROM attempt_charges WHERE id IN
             (SELECT id FROM attempt_facts WHERE request_id IS NULL)",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize
    }

    fn spent_today(&self, scope: BudgetScope) -> f64 {
        let ledger = self.store.accounting_ledger().unwrap().unwrap();
        ledger.spent(
            &Budget::new(scope, BudgetPeriod::Day, "USD", f64::MAX),
            chrono::Local::now(),
        )
    }

    /// Every byte the runtime storage owns, WAL included, so a leak cannot
    /// hide in a page that has not been checkpointed yet.
    fn storage_bytes(&self) -> Vec<u8> {
        let mut bytes = std::fs::read(&self.db_path).unwrap();
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", self.db_path.display(), suffix));
            if let Ok(part) = std::fs::read(sidecar) {
                bytes.extend_from_slice(&part);
            }
        }
        bytes
    }

    async fn shutdown(mut self) {
        if let Some(s) = self.server.take() {
            s.stop().await;
        }
    }
}

// ------------------------------------------------------------------ the tests

/// One description send: one request-less attempt fact bound to the concrete
/// execution and quote, one charge row, the ledger charged exactly once — and
/// a reopened storage that reads the same facts back.
#[tokio::test]
async fn one_description_is_one_durable_identity_charged_once_and_reopens_readably() {
    let (addr, mock) = start_mock().await;
    let h = Harness::durable(config_for(addr, "eyes-model"), mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "text answer");

    let attempts = h.auxiliary_facts();
    assert_eq!(attempts.len(), 1, "exactly one auxiliary attempt fact");
    let fact = &attempts[0];
    assert_eq!(fact.state, AttemptState::Served);
    assert!(fact.sent);
    assert_eq!(
        fact.request_id, None,
        "an auxiliary call invents no client request"
    );
    assert_eq!(fact.execution.provider_id, "v");
    assert_eq!(fact.execution.upstream_model, "eyes-model");
    let pricing = fact.execution.pricing.as_ref().unwrap();
    assert_eq!(pricing.currency, "USD");
    let charged = fact.cost.as_ref().unwrap().amount;
    assert!(
        (charged - ONE_DESCRIPTION_USD).abs() < 1e-12,
        "the bound quote priced the reported usage: {charged}"
    );
    assert_eq!(fact.usage.as_ref().unwrap().input_tokens, 11);
    assert_eq!(fact.usage.as_ref().unwrap().output_tokens, 7);

    assert_eq!(
        h.auxiliary_charge_count(),
        1,
        "one execution, one charge row"
    );
    let spent = h.spent_today(BudgetScope::Provider { id: "v".into() });
    assert!(
        (spent - ONE_DESCRIPTION_USD).abs() < 1e-12,
        "ledger says {spent}"
    );
    assert!(h.spent_today(BudgetScope::Provider { id: "p".into() }) > 0.0);
    assert!(h.store.unresolved_attempts().unwrap().is_empty());

    // Reopening the storage reads the same settled ledger.
    let durable = h.store.accounting_ledger().unwrap().unwrap();
    let reopened = RuntimeStore::open(&h.db_path).unwrap();
    assert_eq!(reopened.accounting_ledger().unwrap().unwrap(), durable);
    assert!(reopened.unresolved_attempts().unwrap().is_empty());

    // Nothing that was said, asked or kept secret is on the disk.
    let bytes = h.storage_bytes();
    for leak in [
        "rising line",
        "Revenue",
        "what does this chart show",
        "chart.png",
        "sk-v",
        "sk-p",
    ] {
        assert!(
            !bytes_contain(&bytes, leak),
            "the runtime storage must not contain {leak:?}"
        );
    }

    h.shutdown().await;
}

/// The description failed with an HTTP error, but the relay reported the
/// usage it had already consumed: the attempt is Failed with that known cost,
/// charged once, and the main request still succeeded on the placeholder.
#[tokio::test]
async fn a_failed_description_keeps_its_reported_usage_and_charges_once() {
    let (addr, mock) = start_mock().await;
    let h = Harness::durable(config_for(addr, "fail-usage-eyes-model"), mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200, "the main request survives the failure");
    assert_eq!(calls_to(&h.mock.bodies(), "fail-usage-eyes"), 1);

    let attempts = h.auxiliary_facts();
    assert_eq!(attempts.len(), 1);
    let fact = &attempts[0];
    assert_eq!(fact.state, AttemptState::Failed);
    assert!(fact.sent, "the request really reached the upstream");
    assert_eq!(fact.usage.as_ref().unwrap().input_tokens, 5);
    assert_eq!(fact.usage.as_ref().unwrap().output_tokens, 3);
    let known = fact.cost.as_ref().unwrap().amount;
    assert!(
        (known - FAILED_DESCRIPTION_USD).abs() < 1e-12,
        "the reported usage priced by the bound quote: {known}"
    );
    assert!(fact.failure_class.is_some());

    assert_eq!(h.auxiliary_charge_count(), 1);
    let spent = h.spent_today(BudgetScope::Provider { id: "v".into() });
    assert!(
        (spent - FAILED_DESCRIPTION_USD).abs() < 1e-12,
        "a known failed cost is charged exactly once, never zero and never twice: {spent}"
    );

    h.shutdown().await;
}

/// The description succeeded but the upstream named no usage: the tokens and
/// the charge stay unknown. The gateway must not invent a zero, and the
/// main request still carries the description it paid a query for.
#[tokio::test]
async fn an_unreported_auxiliary_usage_stays_unknown() {
    let (addr, mock) = start_mock().await;
    let h = Harness::durable(config_for(addr, "nousage-eyes-model"), mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "text answer");
    // The description itself reached the blind model as prompt text.
    let blind = h
        .mock
        .bodies()
        .into_iter()
        .find(|body| {
            body["model"]
                .as_str()
                .is_some_and(|m| m.starts_with("blind"))
        })
        .unwrap();
    let prompt = serde_json::to_string(&blind["messages"][0]).unwrap();
    assert!(
        prompt.contains("[Image description:"),
        "the paid description was used: {prompt}"
    );

    let attempts = h.auxiliary_facts();
    assert_eq!(attempts.len(), 1);
    let fact = &attempts[0];
    assert_eq!(fact.state, AttemptState::Served);
    assert!(fact.sent);
    assert_eq!(
        fact.usage, None,
        "an absent report is unknown, not a measured zero"
    );
    assert_eq!(fact.cost, None);
    assert_eq!(h.auxiliary_charge_count(), 0);
    assert_eq!(
        h.spent_today(BudgetScope::Provider { id: "v".into() }),
        0.0,
        "nothing was proven chargeable, so nothing is booked"
    );

    h.shutdown().await;
}

/// Two images across a main-candidate failover: every description that was
/// actually sent is charged once, on its own immutable identity, and the
/// totals survive a storage reopen.
#[tokio::test]
async fn descriptions_across_a_main_failover_are_charged_once_each_durably() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, "eyes-model");
    cfg.models.push(
        ModelEntry::for_upstream("p", "flaky-model", Some(ModelTier::Standard)).with_priority(0),
    );
    cfg.models[0].priority = 1;
    let h = Harness::durable(cfg, mock).await;

    let resp = h.post(two_image_request()).await;
    assert_eq!(resp.status(), 200, "the second candidate answers");
    assert_eq!(calls_to(&h.mock.bodies(), "flaky-model"), 1);
    assert_eq!(calls_to(&h.mock.bodies(), "blind-model"), 1);
    assert_eq!(
        calls_to(&h.mock.bodies(), "eyes-model"),
        4,
        "two images per attempt"
    );

    let attempts = h.auxiliary_facts();
    assert_eq!(attempts.len(), 4, "one fact per sent description");
    let mut ids: Vec<_> = attempts.iter().map(|fact| fact.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "every send has its own immutable identity");
    for fact in &attempts {
        assert_eq!(fact.state, AttemptState::Served);
        assert!(fact.sent);
        assert!(fact.usage.is_some());
        assert!(fact.cost.is_some());
    }

    assert_eq!(
        h.auxiliary_charge_count(),
        4,
        "each sent description charged once"
    );
    let expected = 4.0 * ONE_DESCRIPTION_USD;
    let durable = h.spent_today(BudgetScope::Provider { id: "v".into() });
    assert!(
        (durable - expected).abs() < 1e-12,
        "{durable} vs {expected}"
    );

    let ledger = h.store.accounting_ledger().unwrap().unwrap();
    let reopened = RuntimeStore::open(&h.db_path).unwrap();
    assert_eq!(reopened.accounting_ledger().unwrap().unwrap(), ledger);

    h.shutdown().await;
}
