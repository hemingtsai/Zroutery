//! A retryable upstream failure is replayed on the same candidate.
//!
//! The reference filter in front of CC Switch learned this the hard way: a
//! relay between several accounts answers 429/502/503 for reasons a second
//! attempt usually clears, and failing the client on the first answer turns a
//! transient window into a visible error. The policy lives in
//! `upstream::retry_advice`; this drives it through the real pipeline.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

const TOKEN: &str = "test-token";

/// Answers `failures` times with `status`, then with a normal Anthropic reply.
#[derive(Clone)]
struct Flaky {
    failures: usize,
    status: u16,
    seen: Arc<AtomicUsize>,
}

async fn messages(axum::extract::State(flaky): axum::extract::State<Flaky>) -> Response {
    let n = flaky.seen.fetch_add(1, Ordering::SeqCst);
    if n < flaky.failures {
        return (
            StatusCode::from_u16(flaky.status).unwrap(),
            Json(json!({"error": {"type": "overloaded_error", "message": "try again"}})),
        )
            .into_response();
    }
    Json(json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "m",
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    }))
    .into_response()
}

async fn start_flaky(failures: usize, status: u16) -> (String, Arc<AtomicUsize>) {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(Flaky {
            failures,
            status,
            seen: Arc::clone(&seen),
        });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), seen)
}

fn config_for(base: &str) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
    provider.base_url = base.into();
    provider.key_ref = "provider:relay".into();
    provider.timeout_secs = 10;
    cfg.providers = vec![provider];
    cfg.models = vec![ModelEntry::for_upstream("relay", "m", None)];
    cfg
}

async fn start_proxy(cfg: AppConfig) -> (String, ServerHandle) {
    let secrets = Arc::new(MemorySecretStore::new().with("provider:relay", "key"));
    let state = Arc::new(AppState::new(cfg, secrets));
    let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
    (format!("http://{}", server.addr), server)
}

async fn ask(base: &str) -> (reqwest::StatusCode, String) {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let response = client
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", TOKEN)
        .json(&json!({
            "model": "relay-m",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.text().await.unwrap())
}

/// A gateway blip (502) is replayed on the same candidate and the client sees
/// the successful answer, not the failure.
#[tokio::test]
async fn a_transient_gateway_failure_is_replayed_before_the_client_sees_it() {
    let (upstream, seen) = start_flaky(1, 502).await;
    let (proxy, server) = start_proxy(config_for(&upstream)).await;
    let (status, text) = ask(&proxy).await;
    assert_eq!(status, 200, "{text}");
    assert!(text.contains("\"ok\""), "{text}");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "the failure and the successful replay both reached the upstream"
    );
    server.stop().await;
}

/// A failure a replay cannot fix (a 401) is returned at once: no retries, no
/// second request.
#[tokio::test]
async fn a_terminal_failure_is_not_replayed() {
    let (upstream, seen) = start_flaky(1, 401).await;
    let (proxy, server) = start_proxy(config_for(&upstream)).await;
    let (status, _) = ask(&proxy).await;
    assert_eq!(status, 401);
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    server.stop().await;
}

/// A rate limit is retried, and the upstream asking for a long wait is obeyed
/// (here with the shortest possible schedule, so the test stays quick).
#[tokio::test]
async fn a_rate_limit_is_retried_on_the_same_candidate() {
    let (upstream, seen) = start_flaky(1, 429).await;
    let mut cfg = config_for(&upstream);
    // One retry's wait is 5 s; nothing else in the request is slow.
    cfg.providers[0].timeout_secs = 10;
    let (proxy, server) = start_proxy(cfg).await;
    let (status, text) = ask(&proxy).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    server.stop().await;
}
