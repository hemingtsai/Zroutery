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
