//! Claude 2.1.281 monitor requests reach the configured decision pool and leave
//! actual request records. No real API, secret, or executable shell command.

use axum::extract::State;
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use zroutery_core::budget::Ledger;
use zroutery_core::config::{
    AppConfig, ClassifierCandidate, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind,
};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone)]
struct Mock {
    received: Arc<Mutex<Vec<Value>>>,
    probability: f64,
}

async fn decide(State(mock): State<Mock>, Json(body): Json<Value>) -> Json<Value> {
    mock.received.lock().unwrap().push(body);
    Json(json!({"answers":{"decision":{"type":"noul","noul":mock.probability}}}))
}

#[tokio::test]
async fn a_rejected_systemone_reply_keeps_its_own_cache_and_reasoning_usage() {
    async fn rejected() -> (axum::http::StatusCode, Json<Value>) {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "model": "jev-failed-revision",
                "usage": {"input_tokens": 20, "output_tokens": 7,
                    "cache_read_tokens": 3, "cache_write_tokens": 2, "reasoning_tokens": 4},
                "error": {"message": "fixture refusal"}
            })),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = axum::Router::new().route("/v1/systemone", post(rejected));
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut provider = ProviderConfig::new("jev", "Jev fixture", ProviderKind::Decision);
    provider.base_url = format!("http://{address}");
    let identity = zroutery_core::config::resolve_client_identity(
        zroutery_core::config::ProviderClientProfile::Native,
        &axum::http::HeaderMap::new(),
    );
    let result = zroutery_core::upstream::Upstream::new(true, 5)
        .send_decision_with_identity(&provider, None, &json!({"model": "jev"}), &identity)
        .await;
    assert!(result.is_err());
    let observed = identity.observation.snapshot();
    assert_eq!(
        observed.declaration.observed_model(),
        Some("jev-failed-revision")
    );
    assert_eq!(
        observed.usage,
        Some(zroutery_core::ir::Usage {
            input_tokens: 20,
            output_tokens: 7,
            cache_read_tokens: 3,
            cache_write_tokens: 2,
            reasoning_tokens: 4,
        })
    );
    task.abort();
}

#[tokio::test]
async fn temperature_one_and_severity_requests_use_jev_not_the_sonnet_class_and_are_durably_recorded(
) {
    for probability in [0.02, 0.97, 0.5] {
        let mock = Mock {
            received: Arc::new(Mutex::new(Vec::new())),
            probability,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = axum::Router::new()
            .route("/v1/systemone", post(decide))
            .with_state(mock.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.require_auth = false;
        config.server.bypass_proxy = true;
        let mut provider = ProviderConfig::new("jev", "Jev fixture", ProviderKind::Decision);
        provider.base_url = format!("http://{address}");
        provider.key_ref.clear();
        provider.decision.path = "/v1/systemone".into();
        provider.decision.threshold = 0.6;
        config.providers.push(provider);
        config
            .models
            .push(ModelEntry::for_upstream("jev", "jev-1.13-free", None));
        config.classifier.enabled = true;
        config.classifier.candidates.push(ClassifierCandidate {
            model: "jev-jev-1.13-free".into(),
            priority: 0,
            enabled: true,
        });
        let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(RuntimeStore::open(&directory.path().join("runtime.sqlite3")).unwrap());
        state
            .attach_accounting(store.clone(), &Ledger::new(), "isolated fixture")
            .unwrap();
        let gateway = ServerHandle::start(state.clone()).await.unwrap();
        for stream in [false, true] {
            let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("http://{}/v1/messages",gateway.addr))
                .header("user-agent","claude-cli/2.1.281").json(&json!({"model":"standard-class","stream":stream,
                    "temperature":1,"max_tokens":256,"stop_sequences":["</severity>"],
                    "thinking":{"type":"enabled","budget_tokens":128},
                    "system":[{"type":"text","text":"You are a security monitor for autonomous AI coding agents.\n## Output Format\nOutput <severity>N</severity> where N is an integer 0-100."}],
                    "messages":[{"role":"user","content":"local non-executable fixture"}]})).send().await.unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["x-zroutery-classifier"], "1");
            assert_eq!(response.headers()["x-zroutery-model"], "jev-jev-1.13-free");
            let text = response.text().await.unwrap();
            let severity = if probability < 0.4 { 0 } else { 100 };
            assert!(
                text.contains(&format!("<severity>{severity}</severity>")),
                "{text}"
            );
            assert!(
                !text.contains("<severity>0</severity>") || probability < 0.4,
                "uncertain/blocked replies cannot approve"
            );
        }
        {
            let received = mock.received.lock().unwrap();
            assert_eq!(received.len(), 2);
            for body in received.iter() {
                assert_eq!(body["model"], "jev-1.13-free");
                assert_eq!(body["questions"]["decision"]["type"], "noul");
            }
        }
        let records = store.usage().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| !record.kind.is_main()
            && record.resolved_model.as_deref() == Some("jev-jev-1.13-free")
            && record.attempts == 1));
        gateway.stop().await;
        task.abort();
    }
}

#[tokio::test]
async fn configured_systemone_urls_preserve_tool_state_usage_and_distinct_protocols() {
    use axum::http::Uri;
    type Calls = Arc<Mutex<Vec<(String, Value)>>>;
    async fn capture(State(calls): State<Calls>, uri: Uri, Json(body): Json<Value>) -> Json<Value> {
        calls.lock().unwrap().push((uri.path().into(), body));
        Json(json!({"model":"jev-revision-17","modelVersion":"17.2",
            "usage":{"input_tokens":120,"output_tokens":2},
            "answers":{"decision":{"type":"noul","noul":0.97}}}))
    }
    for (base_suffix, path, expected) in [
        ("/v1", "/v1/systemone", "/v1/systemone"),
        ("/v1/systemone", "/v1/systemone", "/v1/systemone"),
        (
            "/inference/systemone",
            "/v1/systemone",
            "/inference/systemone/v1/systemone",
        ),
    ] {
        let calls = Calls::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = axum::Router::new()
            .fallback(post(capture))
            .with_state(calls.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.require_auth = false;
        config.server.bypass_proxy = true;
        let mut provider = ProviderConfig::new("jev", "Decision fixture", ProviderKind::Decision);
        provider.base_url = format!("http://{address}{base_suffix}");
        provider.decision.path = path.into();
        provider.key_ref.clear();
        config.providers.push(provider);
        config
            .models
            .push(ModelEntry::for_upstream("jev", "jev-latest", None));
        config.classifier.enabled = true;
        config.classifier.candidates.push(ClassifierCandidate {
            model: "jev-jev-latest".into(),
            priority: 0,
            enabled: true,
        });
        let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(RuntimeStore::open(&dir.path().join("runtime.db")).unwrap());
        state
            .attach_accounting(store.clone(), &Ledger::new(), "decision protocol fixture")
            .unwrap();
        let gateway = ServerHandle::start(state).await.unwrap();
        for stream in [false, true] {
            let response = reqwest::Client::builder().no_proxy().build().unwrap()
                .post(format!("http://{}/v1/messages", gateway.addr))
                .json(&json!({"model":"standard-class","stream":stream,"max_tokens":64,"temperature":0,
                    "stop_sequences":["</block>"],
                    "system":"You are a security monitor for autonomous AI coding agents.",
                    "messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"fixture-tool","content":"tool result evidence"}]}]}))
                .send().await.unwrap();
            assert_eq!(response.status(), 200);
            let text = response.text().await.unwrap();
            assert!(text.contains("<block>yes</block>"));
        }
        for (url, body) in calls.lock().unwrap().iter() {
            assert_eq!(url, expected);
            assert!(body.get("messages").is_none());
            assert!(body.get("stream").is_none());
            assert!(body.get("thinking").is_none());
            assert_eq!(body["model"], "jev-latest");
            assert_eq!(body["questions"]["decision"]["type"], "noul");
            assert!(body["state"]
                .as_str()
                .unwrap()
                .contains("tool result evidence"));
        }
        let records = store.usage().unwrap();
        assert_eq!(records.len(), 2);
        for record in records {
            assert_eq!(record.ingress, zroutery_core::ir::Dialect::Anthropic);
            assert_eq!(record.usage.input_tokens, 120);
            let attempts = store.usage_attempts(&record.id).unwrap();
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].execution.upstream_model, "jev-latest");
            assert_eq!(
                attempts[0].execution.upstream_protocol.as_deref(),
                Some("systemone")
            );
            assert_eq!(
                attempts[0].declared.as_ref().unwrap().observed_model(),
                Some("jev-revision-17")
            );
        }
        gateway.stop().await;
        task.abort();
    }
}
