//! The parts of token capture that do not need a window: the report's life and
//! the verification call, driven against a panel on a real socket.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

use super::*;

fn view(provider_id: &str, account_id: &str, phase: CapturePhase) -> TokenCaptureView {
    TokenCaptureView::new(provider_id, account_id, phase)
}

#[test]
fn a_report_survives_the_session_and_says_when_it_is_live() {
    let runtime = TokenCaptureRuntime::default();
    assert!(runtime.report("relay", "alice").is_none());
    assert!(!runtime.is_live("relay", "alice"));

    runtime.record(view("relay", "alice", CapturePhase::Waiting));
    assert_eq!(
        runtime.report("relay", "alice").unwrap().phase,
        CapturePhase::Waiting
    );
    assert!(runtime.is_live("relay", "alice"));

    // The terminal phases stay readable after the browser is gone, and none of
    // them owns one any more.
    for phase in [
        CapturePhase::Stored,
        CapturePhase::Captured,
        CapturePhase::Failed,
        CapturePhase::Cancelled,
    ] {
        runtime.record(view("relay", "alice", phase));
        assert_eq!(runtime.report("relay", "alice").unwrap().phase, phase);
        assert!(!runtime.is_live("relay", "alice"), "{phase:?}");
    }
}

#[test]
fn a_report_carries_no_token() {
    // The type has no field that could hold one, and the serialised view is what
    // the webview receives. This pins the shape rather than trusting review.
    let json = serde_json::to_value(view("relay", "alice", CapturePhase::Stored)).unwrap();
    assert!(json.get("token").is_none(), "{json}");
    assert!(json.get("secret").is_none(), "{json}");
}

/// A panel that accepts one bearer and describes the user behind it.
#[derive(Clone)]
struct Panel {
    token: &'static str,
}

async fn whoami(
    axum::extract::State(panel): axum::extract::State<Panel>,
    headers: HeaderMap,
) -> Response {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    // The adapter and the page probe both send the bare token; a Bearer prefix
    // is part of the compared string on older panels.
    if bearer == panel.token {
        Json(json!({
            "success": true,
            "message": "",
            "data": {
                "id": 7,
                "username": "alice",
                "status": 1,
                "quota": 0,
                "used_quota": 0,
                "request_count": 0
            }
        }))
        .into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"success": false, "message": "无权进行此操作"})),
        )
            .into_response()
    }
}

async fn start_panel(token: &'static str) -> String {
    let app = Router::new()
        .route("/api/user/self", get(whoami))
        .with_state(Panel { token });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_minted_token_is_verified_against_the_panel() {
    let base = start_panel("panel-token").await;
    // The panel root is joined the way the account's base url is: `/v1` is the
    // relay endpoint, not part of the dashboard.
    let root = format!("{base}/v1");
    assert_eq!(
        verify_token(&root, "panel-token", None).await.unwrap(),
        Some("alice".to_string())
    );
    let rejected = verify_token(&root, "wrong-token", None).await;
    assert!(rejected.is_err(), "a rejected token is not a session");
    // A relay key is refused before any request is sent, with the reason the
    // operator needs.
    let relay = verify_token(&root, "sk-relay-key", None).await.unwrap_err();
    assert!(relay.contains("relay"), "{relay}");
}

#[test]
fn a_cancelled_capture_is_not_live_even_while_the_browser_closes() {
    let runtime = TokenCaptureRuntime::default();
    runtime.record(view("relay", "alice", CapturePhase::NeedsVerification));
    assert!(runtime.is_live("relay", "alice"));
    runtime.record(view("relay", "alice", CapturePhase::Cancelled));
    assert!(!runtime.is_live("relay", "alice"));
    // Another account is unaffected.
    assert!(runtime.report("relay", "bob").is_none());
    assert!(!runtime.is_live("relay", "bob"));
}
