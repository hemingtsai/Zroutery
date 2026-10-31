//! Native @google/genai paths and credential headers against a loopback upstream.

use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, NamingStyle, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

#[tokio::test]
async fn sdk_routes_use_url_model_streaming_and_google_key_without_counting_probes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = observed.clone();
    let mock = tokio::spawn(async move {
        let app = axum::Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body.clone());
                if body["stream"] == true {
                    ([ ("content-type", "text/event-stream") ],
                        "data: {\"id\":\"test\",\"model\":\"mock\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n").into_response()
                } else {
                    Json(json!({"id":"test", "model":"mock", "choices":[{"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":3}})).into_response()
                }
            }
        }));
        axum::serve(listener, app).await.unwrap();
    });
    let mut provider = ProviderConfig::new("p", "Mock", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}");
    provider.key_ref.clear();
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.auth_token = "zr-gemini-native-test".into();
    config.server.require_auth = true;
    config.server.autostart = false;
    config.server.allow_cors = true;
    config.server.cors_origins = vec!["http://localhost:3000".into()];
    config.routing.naming_style = NamingStyle::Anthropic;
    config.providers.push(provider);
    config.models.push(ModelEntry::for_upstream(
        "p",
        "mock",
        Some(ModelTier::Standard),
    ));
    let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let server = ServerHandle::start(state.clone()).await.unwrap();
    let base = format!("http://{}/v1beta/models", server.addr);
    let client = reqwest::Client::new();
    let preflight = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base}/standard-class:generateContent"),
        )
        .header("origin", "http://localhost:3000")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "content-type,x-goog-api-key",
        )
        .send()
        .await
        .unwrap();
    assert!(preflight.status().is_success());
    assert!(preflight.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .contains("x-goog-api-key"));
    let started = chrono::Utc::now() - chrono::Duration::seconds(1);
    let headers = || {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-goog-api-key", "zr-gemini-native-test".parse().unwrap());
        headers.insert(
            "user-agent",
            "GeminiCLI-tui/0.66.0/standard-class (win32)"
                .parse()
                .unwrap(),
        );
        headers
    };
    let list = client.get(&base).headers(headers()).send().await.unwrap();
    assert!(list.status().is_success());
    let list: Value = list.json().await.unwrap();
    assert!(list["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["name"] == "models/standard-class"));
    assert!(list.get("data").is_none());
    let body = json!({"contents":[{"role":"user","parts":[{"text":"Hello from fixture"}]}]});
    let count = client
        .post(format!("{base}/standard-class:countTokens"))
        .headers(headers())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(count.status().is_success());
    let count: Value = count.json().await.unwrap();
    assert!(count["totalTokens"].as_u64().unwrap() > 0);
    assert_eq!(count["zroutery"]["estimated"], true);
    assert_eq!(state.stats().summary().requests, 0);
    assert!(!state.stats().client_seen_since("gemini", started));
    let refused = client
        .post(format!("{base}/standard-class:generateContent"))
        .header("x-goog-api-key", "wrong")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    assert!(refused.json::<Value>().await.unwrap()["error"].is_object());
    assert!(observed.lock().unwrap().is_empty());

    let mut conflicting_body = body.clone();
    let mut explicit_effort = body.clone();
    explicit_effort["generationConfig"] = json!({"thinkingConfig":{"thinkingLevel":"high"}});
    let unsupported = client
        .post(format!("{base}/standard-class:generateContent"))
        .headers(headers())
        .json(&explicit_effort)
        .send()
        .await
        .unwrap();
    assert_eq!(
        unsupported.status(),
        422,
        "unknown cross-protocol effort cannot be dropped"
    );
    assert!(observed.lock().unwrap().is_empty());
    conflicting_body["model"] = json!("not-the-url-model");
    conflicting_body["stream"] = json!(true);
    let response = client
        .post(format!("{base}/standard-class:generateContent"))
        .headers(headers())
        .json(&conflicting_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response: Value = response.json().await.unwrap();
    assert_eq!(
        response["candidates"][0]["content"]["parts"][0]["text"],
        "hello"
    );
    let response = client
        .post(format!(
            "{base}/standard-class:streamGenerateContent?alt=sse"
        ))
        .headers(headers())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .contains("text/event-stream"));
    let sse = response.text().await.unwrap();
    assert!(
        sse.contains("candidates") && sse.contains("hello") && sse.contains("usageMetadata"),
        "{sse}"
    );
    assert!(state.stats().client_seen_since("gemini", started));
    assert_eq!(state.stats().summary().requests, 3);
    assert!(state
        .stats()
        .recent(2)
        .iter()
        .all(|record| record.client.client_id.as_str() == "gemini"));
    let sent = observed.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["model"], "mock");
    assert_ne!(sent[0]["stream"], true);
    assert_eq!(sent[1]["stream"], true);
    server.stop().await;
    mock.abort();
}

#[test]
fn every_documented_gemini_entrypoint_has_the_same_stable_attribution() {
    for entry in [
        "GeminiCLI",
        "GeminiCLI-tui",
        "GeminiCLI-cli-command",
        "GeminiCLI-acp",
    ] {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "user-agent",
            format!("{entry}/0.66.0/standard-class (win32)")
                .parse()
                .unwrap(),
        );
        let client = zroutery_core::client::identify(&headers, None);
        assert_eq!(client.client_id.as_str(), "gemini");
        assert_eq!(client.client_name, "Gemini CLI");
    }
}
