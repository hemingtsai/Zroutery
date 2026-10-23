//! A fake NewAPI panel, and the fixtures that run the adapter against it.

use super::helpers::*;
use super::types::*;
use super::wire::*;
use super::*;
use crate::account::provider::*;
use crate::account::types::*;
use crate::error::Error;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use serde_json::Value;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const BEARER: &str = "pat-123";

// ── fake panel ───────────────────────────────────────────────────────

fn envelope(data: Value) -> Value {
    json!({ "success": true, "message": "", "data": data })
}

fn failure(code: &str, message: &str) -> Value {
    json!({ "success": false, "code": code, "message": message })
}

fn self_payload() -> Value {
    envelope(json!({
        "id": 7,
        "username": "alice",
        "display_name": "Alice",
        "status": 1,
        "group": "default",
        "quota": 1_500_000,
        "used_quota": 500_000,
        "request_count": 42,
    }))
}

fn status_payload() -> Value {
    envelope(json!({
        "version": "v0.9.0",
        "system_name": "New API",
        "quota_per_unit": 500_000.0,
        "checkin_enabled": true,
        "turnstile_check": false,
        "self_use_mode_enabled": false,
        "display_in_currency": true,
        "usd_exchange_rate": 7.3,
    }))
}

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    bearer: Option<String>,
    cookie: Option<String>,
}

struct MockState {
    required_bearer: Mutex<String>,
    requests: Mutex<Vec<Recorded>>,
    status_body: Mutex<Value>,
    /// When set, `/api/status` answers with this raw body instead of JSON.
    status_raw: Mutex<Option<String>>,
    status_hits: AtomicU64,
    self_body: Mutex<Value>,
    self_http_status: Mutex<u16>,
    /// Number of upcoming `/api/user/self` calls to reject with an expired
    /// token before answering normally.
    self_expired: AtomicI64,
    stat_body: Mutex<Value>,
    stat_http_status: Mutex<u16>,
    logs: Mutex<Vec<Value>>,
    log_total_override: Mutex<Option<i64>>,
    refresh_body: Mutex<Value>,
    refresh_status: Mutex<u16>,
    /// When set, only this refresh-cookie value is accepted, and a
    /// successful exchange rotates it to `refresh_next_cookie`. Models a
    /// panel that invalidates the cookie as it is redeemed.
    refresh_cookie: Mutex<Option<String>>,
    /// The cookie value accepted after one successful exchange.
    refresh_next_cookie: Mutex<Option<String>>,
    /// The bearer the panel requires after a successful exchange.
    refresh_new_bearer: Mutex<Option<String>>,
    /// The `Set-Cookie` header a refresh answers with, when not the default.
    refresh_set_cookie: Mutex<Option<String>>,
    subscription_body: Mutex<Value>,
    subscription_status: Mutex<u16>,
    checkin_body: Mutex<Value>,
    checkin_post_body: Mutex<Value>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            required_bearer: Mutex::new(BEARER.into()),
            requests: Mutex::new(Vec::new()),
            status_body: Mutex::new(status_payload()),
            status_raw: Mutex::new(None),
            status_hits: AtomicU64::new(0),
            self_body: Mutex::new(self_payload()),
            self_http_status: Mutex::new(200),
            self_expired: AtomicI64::new(0),
            stat_body: Mutex::new(envelope(json!({ "quota": 0, "rpm": 0, "tpm": 0 }))),
            stat_http_status: Mutex::new(200),
            logs: Mutex::new(Vec::new()),
            log_total_override: Mutex::new(None),
            refresh_body: Mutex::new(envelope(json!({
                "access_token": "jwt-from-cookie",
                "token_type": "Bearer",
                "access_expires_at": 1_800_000_000,
                "user": {
                    "id": 7,
                    "username": "alice",
                    "display_name": "Alice",
                    "status": 1,
                    "quota": 1_500_000,
                    "used_quota": 500_000,
                    "request_count": 42,
                },
            }))),
            refresh_status: Mutex::new(200),
            refresh_cookie: Mutex::new(None),
            refresh_next_cookie: Mutex::new(None),
            refresh_new_bearer: Mutex::new(None),
            refresh_set_cookie: Mutex::new(None),
            subscription_body: Mutex::new(envelope(json!({
                "billing_preference": "wallet",
                "subscriptions": [],
            }))),
            subscription_status: Mutex::new(200),
            checkin_body: Mutex::new(envelope(json!({
                "enabled": true,
                "min_quota": 1000,
                "max_quota": 10000,
                "stats": { "checked_in_today": false, "checkin_count": 0, "total_checkins": 0, "total_quota": 0 },
            }))),
            checkin_post_body: Mutex::new(envelope(json!({
                "quota_awarded": 5000,
                "checkin_date": "2026-09-25",
            }))),
        }
    }
}

impl MockState {
    fn record(&self, method: &Method, uri: &Uri, headers: &HeaderMap) {
        let field = |name: HeaderName| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        self.requests.lock().unwrap().push(Recorded {
            method: method.to_string(),
            path: uri.path().to_string(),
            query: uri.query().unwrap_or("").to_string(),
            bearer: field(AUTHORIZATION),
            cookie: field(COOKIE),
        });
    }

    fn default_body(&self, slot: &Mutex<Value>) -> Response {
        axum::Json(slot.lock().unwrap().clone()).into_response()
    }

    fn last(&self) -> Recorded {
        self.requests
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("the mock panel recorded no request")
    }

    fn hits(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path == path)
            .count()
    }
}

/// Reject the request unless it carries the expected bearer.
fn authorize(state: &MockState, headers: &HeaderMap) -> Option<Response> {
    let raw = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let token = strip_bearer_prefix(raw);
    let expected = state.required_bearer.lock().unwrap().clone();
    if token.is_empty() || (!expected.is_empty() && token != expected) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(failure("AUTH_UNAUTHORIZED", "unauthorized")),
            )
                .into_response(),
        );
    }
    None
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

async fn mock_status(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    state.status_hits.fetch_add(1, Ordering::SeqCst);
    if let Some(raw) = state.status_raw.lock().unwrap().clone() {
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(raw))
            .unwrap();
    }
    state.default_body(&state.status_body)
}

async fn mock_self(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    if state.self_expired.load(Ordering::SeqCst) > 0 {
        state.self_expired.fetch_sub(1, Ordering::SeqCst);
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(failure("AUTH_TOKEN_EXPIRED", "登录已过期")),
        )
            .into_response();
    }
    let status = StatusCode::from_u16(*state.self_http_status.lock().unwrap()).unwrap();
    let body = state.self_body.lock().unwrap().clone();
    (status, axum::Json(body)).into_response()
}

async fn mock_refresh(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    let status = *state.refresh_status.lock().unwrap();
    if status != 200 {
        return (
            StatusCode::from_u16(status).unwrap(),
            axum::Json(failure("AUTH_UNAUTHORIZED", "refresh token invalid")),
        )
            .into_response();
    }
    // Rotating-cookie semantics, when the test asks for them: a cookie that
    // was already exchanged is dead, which is what makes a double redeem
    // observable instead of harmless.
    let expected = state.refresh_cookie.lock().unwrap().clone();
    if let Some(expected) = expected {
        let presented = headers
            .get(COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(cookie_value);
        if presented.as_deref() != Some(expected.as_str()) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(failure(
                    "AUTH_UNAUTHORIZED",
                    "refresh cookie is no longer valid",
                )),
            )
                .into_response();
        }
        let next = state.refresh_next_cookie.lock().unwrap().clone();
        *state.refresh_cookie.lock().unwrap() = next;
    }
    if let Some(bearer) = state.refresh_new_bearer.lock().unwrap().clone() {
        *state.required_bearer.lock().unwrap() = bearer;
    }
    let mut response = state.default_body(&state.refresh_body);
    // NewAPI rotates the refresh cookie on every refresh.
    let set_cookie = state
        .refresh_set_cookie
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| {
            format!("{REFRESH_COOKIE_NAME}=rotated-cookie; Path=/api/user/auth; HttpOnly")
        });
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&set_cookie).expect("a valid Set-Cookie header"),
    );
    response
}

async fn mock_stat(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    let status = *state.stat_http_status.lock().unwrap();
    if status != 200 {
        return (
            StatusCode::from_u16(status).unwrap(),
            axum::Json(failure("INTERNAL_ERROR", "stat unavailable")),
        )
            .into_response();
    }
    state.default_body(&state.stat_body)
}

async fn mock_logs(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    let query = uri.query().unwrap_or_default();
    let page: u32 = query_param(query, "p")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
        .max(1);
    let page_size: u32 = query_param(query, "page_size")
        .and_then(|value| value.parse().ok())
        .unwrap_or(20)
        .min(LOG_PAGE_SIZE);
    let logs = state.logs.lock().unwrap().clone();
    let total = state
        .log_total_override
        .lock()
        .unwrap()
        .unwrap_or(logs.len() as i64);
    let skip = usize::try_from((page - 1) * page_size).unwrap_or(usize::MAX);
    let items: Vec<Value> = logs
        .into_iter()
        .skip(skip)
        .take(page_size as usize)
        .collect();
    axum::Json(json!({
        "success": true,
        "message": "",
        "data": { "page": page, "page_size": page_size, "total": total, "items": items },
    }))
    .into_response()
}

async fn mock_subscription(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    let status = *state.subscription_status.lock().unwrap();
    if status != 200 {
        // Older instances have no such route; gin answers plain text.
        return Response::builder()
            .status(StatusCode::from_u16(status).unwrap())
            .header("content-type", "text/plain")
            .body(Body::from("404 page not found"))
            .unwrap();
    }
    state.default_body(&state.subscription_body)
}

async fn mock_checkin(
    State(state): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record(&method, &uri, &headers);
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    match method {
        Method::POST => state.default_body(&state.checkin_post_body),
        _ => state.default_body(&state.checkin_body),
    }
}

struct MockPanel {
    base_url: String,
    state: Arc<MockState>,
    task: JoinHandle<()>,
}

impl MockPanel {
    async fn start() -> Self {
        Self::start_with(MockState::default()).await
    }

    async fn start_with(state: MockState) -> Self {
        let state = Arc::new(state);
        let app = Router::new()
            .route("/api/status", get(mock_status))
            .route("/api/user/self", get(mock_self))
            .route("/api/user/auth/refresh", post(mock_refresh))
            .route("/api/log/self/stat", get(mock_stat))
            .route("/api/log/self", get(mock_logs))
            .route("/api/subscription/self", get(mock_subscription))
            .route("/api/user/checkin", get(mock_checkin).post(mock_checkin))
            .fallback(|| async {
                (
                    StatusCode::NOT_FOUND,
                    axum::Json(failure("NOT_FOUND", "no such endpoint")),
                )
                    .into_response()
            })
            .with_state(state.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
            task,
        }
    }

    /// An adapter carrying the panel's personal access token.
    fn adapter(&self) -> NewApiAdapter {
        self.adapter_with(|_| {})
    }

    /// An adapter with one knob changed; callers that want to start
    /// unauthenticated clear `api_key` themselves.
    fn adapter_with(&self, tweak: impl FnOnce(&mut NewApiConfig)) -> NewApiAdapter {
        let mut config = NewApiConfig {
            base_url: self.base_url.clone(),
            api_key: BEARER.into(),
            ..Default::default()
        };
        tweak(&mut config);
        NewApiAdapter::new(config).expect("adapter builds")
    }
}

impl Drop for MockPanel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn account() -> AccountId {
    AccountId("acct-1".into())
}

// ── configuration ────────────────────────────────────────────────────

#[test]
fn provider_id_is_newapi() {
    let mock_base = NewApiConfig {
        base_url: "https://newapi.example.com".into(),
        ..Default::default()
    };
    let adapter = NewApiAdapter::new(mock_base).unwrap();
    assert_eq!(adapter.provider_id(), "newapi");
}

#[test]
fn default_capabilities_cover_every_operation_the_adapter_implements() {
    let config = NewApiConfig {
        base_url: "https://newapi.example.com".into(),
        ..Default::default()
    };
    let adapter = NewApiAdapter::new(config).unwrap();
    let caps = adapter.capabilities();
    assert!(caps.supports_usage);
    assert!(caps.supports_quota);
    assert!(caps.supports_refresh);
    assert!(caps.supports_checkin);
    assert!(caps.supports_health_check);
}

#[test]
fn configured_capabilities_win() {
    let config = NewApiConfig {
        base_url: "https://newapi.example.com".into(),
        capabilities: AccountCapabilities {
            supports_checkin: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let adapter = NewApiAdapter::new(config).unwrap();
    assert!(!adapter.capabilities().supports_checkin);
    assert!(!adapter.capabilities().supports_usage);
}

#[test]
fn config_validate_accepts_panel_and_relay_shapes() {
    for base in [
        "https://newapi.example.com",
        "https://newapi.example.com/",
        "https://newapi.example.com/api",
        "https://newapi.example.com/api/",
        "https://newapi.example.com/v1",
        "http://127.0.0.1:3000",
    ] {
        let config = NewApiConfig {
            base_url: base.into(),
            ..Default::default()
        };
        assert!(config.validate().is_ok(), "rejected {base}");
    }
}

#[test]
fn config_validate_rejects_unusable_base_urls() {
    let cases = [
        ("", "base_url is empty"),
        ("   ", "base_url is empty"),
        ("newapi.example.com", "base_url is not a valid URL"),
        ("ftp://newapi.example.com", "must start with http://"),
        ("file:///etc/passwd", "must start with http://"),
        (
            "https://user:secret@newapi.example.com",
            "must not embed credentials",
        ),
        ("https://newapi.example.com?key=1", "must not carry a query"),
        ("https://newapi.example.com#frag", "must not carry a query"),
    ];
    for (base, expected) in cases {
        let config = NewApiConfig {
            base_url: base.into(),
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.contains(expected),
            "for `{base}` expected `{expected}`, got `{err}`"
        );
    }
}

#[test]
fn config_validate_rejects_bad_tuning_values() {
    let base = "https://newapi.example.com";
    let cases: [(NewApiConfig, &str); 4] = [
        (
            NewApiConfig {
                connect_timeout_secs: 0,
                ..Default::default()
            },
            "connect_timeout_secs",
        ),
        (
            NewApiConfig {
                request_timeout_secs: 0,
                ..Default::default()
            },
            "request_timeout_secs",
        ),
        (
            NewApiConfig {
                usage_page_limit: 0,
                ..Default::default()
            },
            "usage_page_limit",
        ),
        (
            NewApiConfig {
                quota_per_unit: Some(0.0),
                ..Default::default()
            },
            "quota_per_unit",
        ),
    ];
    for (mut config, expected) in cases {
        config.base_url = base.into();
        let err = config.validate().unwrap_err();
        assert!(err.contains(expected), "expected `{expected}`, got `{err}`");
    }
}

#[test]
fn config_validate_rejects_secrets_that_cannot_ride_in_a_header() {
    let config = NewApiConfig {
        base_url: "https://newapi.example.com".into(),
        api_key: "pat-123\r\nX-Injected: 1".into(),
        ..Default::default()
    };
    let err = config.validate().unwrap_err();
    assert!(err.contains("control characters"), "got `{err}`");
}

#[test]
fn normalize_base_url_strips_panel_and_relay_suffixes() {
    let cases = [
        ("https://host", "https://host"),
        ("https://host/", "https://host"),
        ("https://host/api", "https://host"),
        ("https://host/api/", "https://host"),
        ("https://host/v1", "https://host"),
        ("https://host/api/v1", "https://host"),
        ("https://host:8443/", "https://host:8443"),
        ("https://host/panel", "https://host/panel"),
        ("  https://host/v1  ", "https://host"),
    ];
    for (raw, expected) in cases {
        assert_eq!(normalize_base_url(raw).unwrap(), expected, "for `{raw}`");
    }
}

#[test]
fn new_rejects_an_invalid_base_url() {
    let config = NewApiConfig {
        base_url: "newapi.example.com".into(),
        ..Default::default()
    };
    match NewApiAdapter::new(config) {
        Ok(_) => panic!("an invalid base_url must be rejected"),
        Err(err) => assert!(matches!(err, Error::InvalidRequest(_)), "got {err:?}"),
    }
}

// ── authentication ───────────────────────────────────────────────────

#[tokio::test]
async fn authenticate_with_api_key_verifies_and_installs_the_session() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    assert!(!adapter.is_authenticated());

    let session = adapter
        .authenticate(NewApiAuth::ApiKey { key: BEARER.into() })
        .await
        .unwrap();

    assert_eq!(session.user_id.as_deref(), Some("7"));
    assert_eq!(session.username.as_deref(), Some("alice"));
    assert!(session.authenticated_at > 0);
    assert!(
        session.expires_at.is_none(),
        "personal tokens do not expire"
    );
    assert!(adapter.is_authenticated());

    let probe = panel.state.last();
    assert_eq!(probe.path, "/api/user/self");
    // The dashboard credential is sent verbatim, with no `Bearer ` prefix: the
    // panel compares the header value against the stored token as-is.
    assert_eq!(probe.bearer.as_deref(), Some(BEARER));
}

#[tokio::test]
async fn authenticate_strips_a_pasted_bearer_prefix() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    let session = adapter
        .authenticate(NewApiAuth::ApiKey {
            key: format!("Bearer {BEARER} "),
        })
        .await
        .unwrap();
    assert_eq!(session.username.as_deref(), Some("alice"));
}

#[tokio::test]
async fn authenticate_with_a_rejected_key_leaves_the_adapter_unauthenticated() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());

    let err = adapter
        .authenticate(NewApiAuth::ApiKey {
            key: "pat-wrong".into(),
        })
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Unauthorized), "got {err:?}");
    assert!(!adapter.is_authenticated());
}

#[tokio::test]
async fn authenticate_with_cookie_exchanges_it_for_an_access_token() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    // The exchanged token, not the cookie, is what the panel will see next.
    *panel.state.required_bearer.lock().unwrap() = "jwt-from-cookie".into();

    let session = adapter
        .authenticate(NewApiAuth::Cookie {
            session_cookie: "raw-refresh-cookie".into(),
        })
        .await
        .unwrap();

    assert_eq!(session.user_id.as_deref(), Some("7"));
    assert_eq!(session.username.as_deref(), Some("alice"));
    assert_eq!(session.expires_at, Some(1_800_000_000));

    let refresh = panel.state.last();
    assert_eq!(refresh.path, "/api/user/auth/refresh");
    assert_eq!(refresh.method, "POST");
    assert_eq!(
        refresh.cookie.as_deref(),
        Some("new_api_refresh=raw-refresh-cookie")
    );
    assert!(refresh.bearer.is_none(), "refresh carries no bearer");

    // Later calls use the exchanged token, not the cookie.
    let quota = adapter.fetch_quota(&account()).await.unwrap();
    assert!(quota.remaining > 0.0);
    assert_eq!(
        panel.state.last().bearer.as_deref(),
        Some("jwt-from-cookie")
    );
}

#[tokio::test]
async fn authenticate_with_cookie_accepts_a_pasted_cookie_pair() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    adapter
        .authenticate(NewApiAuth::Cookie {
            session_cookie: " new_api_refresh=raw-refresh-cookie ".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        panel.state.last().cookie.as_deref(),
        Some("new_api_refresh=raw-refresh-cookie")
    );
}

#[tokio::test]
async fn authenticate_with_oauth2_uses_the_access_token_as_the_bearer() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    *panel.state.required_bearer.lock().unwrap() = "bearer-jwt".into();

    let session = adapter
        .authenticate(NewApiAuth::OAuth2 {
            access_token: "bearer-jwt".into(),
            refresh_token: Some("new_api_refresh=cookie-jwt".into()),
        })
        .await
        .unwrap();

    assert_eq!(session.username.as_deref(), Some("alice"));
    let probe = panel.state.last();
    assert_eq!(probe.bearer.as_deref(), Some("bearer-jwt"));
}

/// A relay key authenticates `/v1`, never the dashboard; storing one as an
/// account credential must be refused at construction with the reason, not at
/// request time as a 401 that reads as an expired token.
#[test]
fn a_relay_key_is_refused_as_a_dashboard_credential() {
    let build = |api_key: &str| {
        NewApiAdapter::new(NewApiConfig {
            base_url: "https://panel.example.com".into(),
            api_key: api_key.into(),
            ..Default::default()
        })
    };
    let err = build("sk-relay-key")
        .err()
        .expect("an sk- credential must not build an adapter");
    assert!(
        err.to_string().contains("relay API key"),
        "the refusal names the credential kind: {err}"
    );

    // Also refused when pasted with a bearer prefix on top.
    let err = build("Bearer sk-relay-key")
        .err()
        .expect("a prefixed sk- credential must not build an adapter either");
    assert!(err.to_string().contains("relay API key"), "{err}");
}

#[tokio::test]
async fn authenticate_rejects_empty_credentials() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();

    let cases = [
        (
            NewApiAuth::Cookie {
                session_cookie: String::new(),
            },
            "empty session cookie",
        ),
        (NewApiAuth::ApiKey { key: "  ".into() }, "empty API key"),
        (
            NewApiAuth::OAuth2 {
                access_token: String::new(),
                refresh_token: None,
            },
            "empty access token",
        ),
    ];
    for (auth, expected) in cases {
        let err = adapter.authenticate(auth).await.unwrap_err();
        assert!(
            err.to_string().contains(expected),
            "expected `{expected}`, got `{err}`"
        );
    }
}

#[tokio::test]
async fn authenticate_rejects_a_cookie_that_tries_to_add_another_one() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    let err = adapter
        .authenticate(NewApiAuth::Cookie {
            session_cookie: "good; admin=1".into(),
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("single cookie value"),
        "got `{err}`"
    );
}

#[tokio::test]
async fn calls_without_a_credential_fail_before_any_request() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    let err = adapter.fetch_quota(&account()).await.unwrap_err();
    assert!(matches!(err, Error::MissingApiKey(_)), "got {err:?}");
    assert_eq!(panel.state.hits("/api/user/self"), 0);
}

#[tokio::test]
async fn an_expired_access_token_is_renewed_once_and_the_call_retried() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    // Accept the stale token for authentication itself, then expire it.
    panel.state.required_bearer.lock().unwrap().clear();
    adapter
        .authenticate(NewApiAuth::OAuth2 {
            access_token: "stale-jwt".into(),
            refresh_token: Some("cookie-jwt".into()),
        })
        .await
        .unwrap();
    panel.state.self_expired.store(1, Ordering::SeqCst);

    let quota = adapter.fetch_quota(&account()).await.unwrap();

    assert!((quota.remaining - 3.0).abs() < 1e-9);
    assert_eq!(panel.state.hits("/api/user/auth/refresh"), 1);
    assert_eq!(
        panel.state.last().bearer.as_deref(),
        Some("jwt-from-cookie"),
        "the retry uses the renewed token"
    );
}

#[tokio::test]
async fn concurrent_401s_redeem_the_rotating_cookie_once() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    // The old credential authenticates, and is then retired.
    panel.state.required_bearer.lock().unwrap().clear();
    adapter
        .authenticate(NewApiAuth::OAuth2 {
            access_token: "access-old".into(),
            refresh_token: Some("cookie0".into()),
        })
        .await
        .unwrap();

    // From here the panel accepts only the renewed bearer, and invalidates
    // `cookie0` the moment it is exchanged for `cookie1`.
    *panel.state.required_bearer.lock().unwrap() = "access-new".into();
    *panel.state.refresh_body.lock().unwrap() = envelope(json!({
        "access_token": "access-new",
        "token_type": "Bearer",
        "user": { "id": 7, "username": "alice", "display_name": "Alice", "status": 1 },
    }));
    *panel.state.refresh_cookie.lock().unwrap() = Some("cookie0".into());
    *panel.state.refresh_next_cookie.lock().unwrap() = Some("cookie1".into());
    *panel.state.refresh_new_bearer.lock().unwrap() = Some("access-new".into());
    *panel.state.refresh_set_cookie.lock().unwrap() = Some(format!(
        "{REFRESH_COOKIE_NAME}=cookie1; Path=/api/user/auth; HttpOnly"
    ));

    // Both calls snapshot the same bearer and are both rejected before
    // either can refresh.
    let (first, second) = tokio::join!(
        adapter.request_json(reqwest::Method::GET, "/api/user/self", &[]),
        adapter.request_json(reqwest::Method::GET, "/api/user/self", &[]),
    );

    assert!(first.is_ok(), "first concurrent call failed: {first:?}");
    assert!(second.is_ok(), "second concurrent call failed: {second:?}");
    assert_eq!(
        panel.state.hits("/api/user/auth/refresh"),
        1,
        "the rotating refresh cookie must be redeemed exactly once"
    );
    let refreshes: Vec<Recorded> = panel
        .state
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.path == "/api/user/auth/refresh")
        .cloned()
        .collect();
    assert_eq!(refreshes.len(), 1);
    assert_eq!(
        refreshes[0].cookie.as_deref(),
        Some("new_api_refresh=cookie0"),
        "the stale cookie is presented once, not once per waiter"
    );
}

#[tokio::test]
async fn a_refresh_result_never_overwrites_a_newer_login() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    panel.state.required_bearer.lock().unwrap().clear();
    adapter
        .authenticate(NewApiAuth::OAuth2 {
            access_token: "access-old".into(),
            refresh_token: Some("cookie0".into()),
        })
        .await
        .unwrap();

    // A refresh that started from the old credential is in flight...
    let stale = adapter.session_snapshot().expect("a session is installed");
    let renewed = adapter.renew_session(&stale.session).await.unwrap();
    // ...while an explicit re-login installs a newer credential.
    let newer = Session {
        bearer: "access-even-newer".into(),
        refresh_cookie: Some("cookie-new".into()),
        user_id: Some("7".into()),
        username: Some("alice".into()),
        expires_at: None,
    };
    let installed = adapter.install_session(newer.clone());
    assert!(installed.generation > stale.generation);

    // Committing the refresh must not roll the newer login back.
    let after = adapter.install_session_if_current(stale.generation, renewed);
    assert_eq!(after.generation, installed.generation);
    assert_eq!(after.session.bearer, newer.bearer);
    assert_eq!(
        adapter.session_snapshot().unwrap().session.bearer,
        newer.bearer
    );
}

#[tokio::test]
async fn a_personal_token_is_never_renewed_because_it_cannot_be() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();
    panel.state.self_expired.store(1, Ordering::SeqCst);

    let err = adapter.fetch_quota(&account()).await.unwrap_err();

    assert!(matches!(err, Error::Unauthorized), "got {err:?}");
    assert_eq!(panel.state.hits("/api/user/auth/refresh"), 0);
}

// ── status and quota ─────────────────────────────────────────────────

#[tokio::test]
async fn probe_status_reads_the_public_endpoint_without_a_bearer() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());

    let status = adapter.probe_status().await.unwrap();

    assert_eq!(status.version.as_deref(), Some("v0.9.0"));
    assert_eq!(status.system_name.as_deref(), Some("New API"));
    assert_eq!(status.quota_per_unit_or_default(), 500_000.0);
    assert_eq!(status.checkin_enabled, Some(true));
    assert!(panel.state.last().bearer.is_none());
}

#[tokio::test]
async fn refresh_maps_a_healthy_account_into_usd() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.account_id, account());
    assert_eq!(runtime.provider_id, "newapi");
    assert_eq!(runtime.status, AccountStatus::Active);
    let quota = runtime.quota.unwrap();
    assert!((quota.remaining - 3.0).abs() < 1e-9, "{quota:?}");
    assert!((quota.used - 1.0).abs() < 1e-9, "{quota:?}");
    assert!((quota.total - 4.0).abs() < 1e-9, "{quota:?}");
    assert_eq!(quota.unit, "USD");
    assert!(
        runtime.usage.is_none(),
        "usage costs a log sweep; refresh skips it"
    );
    assert!(runtime.last_sync.unwrap() > 0);

    assert_eq!(runtime.metadata.get("newapi.username").unwrap(), "alice");
    assert_eq!(runtime.metadata.get("newapi.quota_raw").unwrap(), "1500000");
    assert_eq!(
        runtime.metadata.get("newapi.used_quota_raw").unwrap(),
        "500000"
    );
    assert_eq!(runtime.metadata.get("newapi.request_count").unwrap(), "42");
    assert_eq!(runtime.metadata.get("newapi.group").unwrap(), "default");
    assert_eq!(runtime.metadata.get("newapi.version").unwrap(), "v0.9.0");
}

#[tokio::test]
async fn refresh_fills_rpm_and_tpm_from_the_rate_probe() {
    let state = MockState::default();
    *state.stat_body.lock().unwrap() = envelope(json!({ "quota": 1234, "rpm": 12, "tpm": 3456 }));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    let rate = runtime.rate_limit.unwrap();
    assert_eq!(rate.rpm_used, Some(12));
    assert_eq!(rate.tpm_used, Some(3456));
    assert_eq!(
        rate.rpm_limit, None,
        "newapi exposes no per-account ceiling"
    );
    assert_eq!(rate.pressure(), 0.0);
    let probe = panel.state.last();
    assert_eq!(probe.path, "/api/log/self/stat");
    assert!(probe.query.contains("type=2"));
}

#[tokio::test]
async fn refresh_survives_a_failing_rate_probe() {
    let state = MockState::default();
    // The account state is readable; only the optional rate probe breaks.
    *state.stat_http_status.lock().unwrap() = 500;
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.status, AccountStatus::Active);
    assert!(runtime.rate_limit.is_none());
    assert_eq!(panel.state.hits("/api/user/self"), 1);
}

#[tokio::test]
async fn a_disabled_user_maps_to_suspended() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = envelope(json!({
        "id": 7, "username": "alice", "status": 2, "quota": 0, "used_quota": 0, "request_count": 0,
    }));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.status, AccountStatus::Suspended);
    assert_eq!(panel.state.hits("/api/subscription/self"), 0);
}

#[tokio::test]
async fn an_empty_wallet_without_a_subscription_maps_to_quota_exhausted() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = envelope(json!({
        "id": 7, "username": "alice", "status": 1, "quota": 0, "used_quota": 900_000, "request_count": 9,
    }));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.status, AccountStatus::QuotaExhausted);
    assert_eq!(panel.state.hits("/api/subscription/self"), 1);
    assert!((runtime.quota.unwrap().remaining).abs() < 1e-9);
}

#[tokio::test]
async fn an_empty_wallet_funded_by_a_subscription_stays_active() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = envelope(json!({
        "id": 7, "username": "alice", "status": 1, "quota": 0, "used_quota": 900_000, "request_count": 9,
    }));
    *state.subscription_body.lock().unwrap() = envelope(json!({
        "billing_preference": "subscription",
        "subscriptions": [{ "id": 3, "plan_title": "Pro", "amount_total": 100 }],
    }));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.status, AccountStatus::Active);
}

#[tokio::test]
async fn an_instance_without_the_subscription_api_treats_the_wallet_as_the_only_funding() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = envelope(json!({
        "id": 7, "username": "alice", "status": 1, "quota": 0, "used_quota": 1, "request_count": 1,
    }));
    // An older panel answers the unknown route with plain-text 404, which
    // is not the dashboard envelope at all.
    *state.subscription_status.lock().unwrap() = 404;
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.status, AccountStatus::QuotaExhausted);
    assert_eq!(panel.state.hits("/api/subscription/self"), 1);
}

#[tokio::test]
async fn self_use_mode_does_not_report_an_empty_wallet_as_exhausted() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = envelope(json!({
        "id": 7, "username": "alice", "status": 1, "quota": -5, "used_quota": 900_000, "request_count": 9,
    }));
    let mut status = status_payload();
    status["data"]["self_use_mode_enabled"] = json!(true);
    *state.status_body.lock().unwrap() = status;
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let runtime = adapter.refresh(&account()).await.unwrap();

    assert_eq!(runtime.status, AccountStatus::Active);
    assert_eq!(panel.state.hits("/api/subscription/self"), 0);
}

#[tokio::test]
async fn quota_per_unit_can_be_overridden_by_configuration() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.quota_per_unit = Some(1_000_000.0));

    let quota = adapter.fetch_quota(&account()).await.unwrap();

    assert!((quota.remaining - 1.5).abs() < 1e-9, "{quota:?}");
    assert!((quota.total - 2.0).abs() < 1e-9, "{quota:?}");
    assert_eq!(
        adapter
            .refresh(&account())
            .await
            .unwrap()
            .metadata
            .get("newapi.quota_per_unit")
            .unwrap(),
        "1000000"
    );
}

#[tokio::test]
async fn status_is_cached_across_refreshes() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();

    adapter.refresh(&account()).await.unwrap();
    adapter.refresh(&account()).await.unwrap();

    assert_eq!(panel.state.hits("/api/status"), 1);
    assert_eq!(panel.state.hits("/api/user/self"), 2);
}

#[tokio::test]
async fn a_status_endpoint_without_quota_per_unit_falls_back_to_the_newapi_default() {
    let state = MockState::default();
    *state.status_body.lock().unwrap() = envelope(json!({ "version": "v0.8.0" }));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let quota = adapter.fetch_quota(&account()).await.unwrap();

    assert!((quota.remaining - 3.0).abs() < 1e-9, "{quota:?}");
}

// ── usage ────────────────────────────────────────────────────────────

fn consume_log(tokens: i64) -> Value {
    json!({
        "prompt_tokens": tokens,
        "completion_tokens": 1,
        "quota": 100,
        "model_name": "gpt-4o",
        "created_at": 1_760_000_000,
        "type": 2,
    })
}

#[tokio::test]
async fn fetch_usage_sums_every_page_of_consumption_logs() {
    let state = MockState::default();
    *state.stat_body.lock().unwrap() = envelope(json!({ "quota": 250_000, "rpm": 3, "tpm": 777 }));
    *state.logs.lock().unwrap() = (0..150).map(|_| consume_log(9)).collect();
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let usage = adapter.fetch_usage(&account()).await.unwrap();

    assert_eq!(usage.total_requests, 150);
    assert_eq!(usage.total_tokens, 150 * 10);
    assert!((usage.total_cost - 0.5).abs() < 1e-9, "{usage:?}");
    assert_eq!(usage.currency, "USD");
    let start = usage.period_start.unwrap();
    let end = usage.period_end.unwrap();
    assert_eq!(end - start, DEFAULT_USAGE_WINDOW_SECS);
    assert_eq!(
        panel.state.hits("/api/log/self"),
        2,
        "150 entries span 2 pages"
    );
}

#[tokio::test]
async fn usage_report_reports_truncation_instead_of_guessing_totals() {
    let state = MockState::default();
    state
        .logs
        .lock()
        .unwrap()
        .extend((0..250).map(|_| consume_log(9)));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter_with(|config| config.usage_page_limit = 2);

    let report = adapter
        .usage_report(Some(1_000), Some(2_000))
        .await
        .unwrap();

    assert!(report.truncated);
    assert_eq!(report.total_requests, 250, "the count comes from the panel");
    assert_eq!(
        report.total_tokens,
        200 * 10,
        "tokens cover the pages fetched"
    );
    assert_eq!(report.period_start, 1_000);
    assert_eq!(report.period_end, 2_000);
    assert_eq!(report.rpm_used, None, "a zero rate is reported as unknown");
    assert_eq!(panel.state.hits("/api/log/self"), 2);
}

#[tokio::test]
async fn usage_report_rejects_an_inverted_window() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();
    let err = adapter
        .usage_report(Some(2_000), Some(1_000))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("starts after it ends"),
        "got `{err}`"
    );
}

#[tokio::test]
async fn usage_report_sends_the_consume_filter_and_the_window() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();

    adapter.usage_report(Some(100), Some(200)).await.unwrap();

    for request in panel.state.requests.lock().unwrap().iter() {
        if request.path.starts_with("/api/log/self") {
            assert!(request.query.contains("type=2"), "{request:?}");
            assert!(request.query.contains("start_timestamp=100"), "{request:?}");
            assert!(request.query.contains("end_timestamp=200"), "{request:?}");
        }
    }
}

#[tokio::test]
async fn the_page_cap_bounds_a_window_with_more_logs_than_the_cap() {
    let state = MockState::default();
    // 300 logs while the caller only allows two pages: the sweep must stop
    // at the cap and admit that the token total is partial.
    *state.logs.lock().unwrap() = (0..300).map(|_| consume_log(9)).collect();
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter_with(|config| config.usage_page_limit = 2);

    let report = adapter.usage_report(None, None).await.unwrap();

    assert!(report.truncated);
    assert_eq!(report.total_requests, 300);
    assert_eq!(report.total_tokens, 200 * 10);
    assert_eq!(
        panel.state.hits("/api/log/self"),
        2,
        "the page cap bounds the sweep"
    );
}

#[tokio::test]
async fn a_short_page_ends_the_sweep_even_when_the_total_looks_larger() {
    let state = MockState::default();
    *state.logs.lock().unwrap() = vec![consume_log(1)];
    *state.log_total_override.lock().unwrap() = Some(9_999);
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter_with(|config| config.usage_page_limit = 5);

    let report = adapter.usage_report(None, None).await.unwrap();

    assert_eq!(report.total_requests, 9_999, "the panel owns the count");
    assert_eq!(report.total_tokens, 2);
    assert!(
        !report.truncated,
        "an exhausted page list is not truncation"
    );
    assert_eq!(panel.state.hits("/api/log/self"), 2);
}

// ── health ───────────────────────────────────────────────────────────

#[tokio::test]
async fn health_check_without_a_credential_pings_the_public_status() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());

    let status = adapter.health_check(&account()).await.unwrap();

    assert_eq!(status, AccountStatus::Active);
    assert_eq!(panel.state.hits("/api/status"), 1);
    assert_eq!(panel.state.hits("/api/user/self"), 0);
}

#[tokio::test]
async fn health_check_maps_tokens_and_users_to_account_states() {
    // Disabled user: the panel rejects the call with AUTH_USER_DISABLED.
    let state = MockState::default();
    state.required_bearer.lock().unwrap().clear();
    *state.self_http_status.lock().unwrap() = 401;
    *state.self_body.lock().unwrap() = failure("AUTH_USER_DISABLED", "banned");
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();
    assert_eq!(
        adapter.health_check(&account()).await.unwrap(),
        AccountStatus::Suspended
    );

    // Expired token: any other 401 means the credential needs renewing.
    let state = MockState::default();
    state.required_bearer.lock().unwrap().clear();
    *state.self_http_status.lock().unwrap() = 401;
    *state.self_body.lock().unwrap() = failure("AUTH_TOKEN_EXPIRED", "expired");
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();
    assert_eq!(
        adapter.health_check(&account()).await.unwrap(),
        AccountStatus::AuthenticationExpired
    );
}

#[tokio::test]
async fn health_check_reports_the_panel_user_status() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter();
    assert_eq!(
        adapter.health_check(&account()).await.unwrap(),
        AccountStatus::Active
    );
    assert_eq!(panel.state.hits("/api/user/self"), 1);
}

// ── check-in ─────────────────────────────────────────────────────────

#[tokio::test]
async fn checkin_is_not_supported_when_the_instance_disables_it() {
    let state = MockState::default();
    let mut status = status_payload();
    status["data"]["checkin_enabled"] = json!(false);
    *state.status_body.lock().unwrap() = status;
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let result = adapter.checkin(&account()).await.unwrap();

    assert_eq!(result, AccountOpResult::NotSupported);
    assert_eq!(panel.state.hits("/api/user/checkin"), 0);
}

#[tokio::test]
async fn checkin_is_not_supported_when_a_turnstile_challenge_is_required() {
    let state = MockState::default();
    let mut status = status_payload();
    status["data"]["turnstile_check"] = json!(true);
    *state.status_body.lock().unwrap() = status;
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    assert_eq!(
        adapter.checkin(&account()).await.unwrap(),
        AccountOpResult::NotSupported
    );
    assert_eq!(panel.state.hits("/api/user/checkin"), 0);
}

#[tokio::test]
async fn checkin_passes_a_turnstile_response_when_the_caller_has_one() {
    let state = MockState::default();
    let mut status = status_payload();
    status["data"]["turnstile_check"] = json!(true);
    *state.status_body.lock().unwrap() = status;
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let result = adapter
        .checkin_with_turnstile(Some("challenge-response"))
        .await
        .unwrap();

    assert_eq!(result, AccountOpResult::Success);
    let post = panel.state.last();
    assert_eq!(post.method, "POST");
    assert_eq!(post.path, "/api/user/checkin");
    assert!(
        post.query.contains("turnstile=challenge-response"),
        "{post:?}"
    );
}

#[tokio::test]
async fn checkin_is_a_no_op_when_today_was_already_claimed() {
    let state = MockState::default();
    *state.checkin_body.lock().unwrap() = envelope(json!({
        "enabled": true,
        "stats": { "checked_in_today": true },
    }));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let result = adapter.checkin(&account()).await.unwrap();

    assert_eq!(result, AccountOpResult::Success);
    assert_eq!(
        panel.state.hits("/api/user/checkin"),
        1,
        "only the status read"
    );
    assert_eq!(panel.state.last().method, "GET");
}

#[tokio::test]
async fn checkin_reports_the_panel_message_when_it_refuses() {
    let state = MockState::default();
    *state.checkin_post_body.lock().unwrap() = failure("CHECKIN_FAILED", "今日已签到");
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let result = adapter.checkin(&account()).await.unwrap();

    assert_eq!(
        result,
        AccountOpResult::Failed("CHECKIN_FAILED: 今日已签到".into())
    );
}

// ── failure handling ─────────────────────────────────────────────────

#[tokio::test]
async fn transport_failures_surface_as_transport_errors() {
    // Port 0 is not a service: nothing can listen on it, so a connection
    // there is refused. The obvious version — bind a free port, read its
    // number, drop the listener, then connect to it — additionally races
    // against the rest of the parallel lib binary, which can take that port
    // in the gap and turn this into a test that fails for a reason that is
    // not the adapter's.
    //
    // The second way this fails is environmental rather than a defect:
    // `reqwest` honours `HTTP_PROXY`, so behind a proxy the connect is
    // answered by the proxy and arrives as `Upstream` carrying the proxy's
    // status. That was observed here, and the same test passed with
    // `NO_PROXY=127.0.0.1`. Making the adapter ignore proxies for loopback is
    // a decision about what a local control panel may be reached through, so
    // it is recorded rather than taken.
    let config = NewApiConfig {
        base_url: "http://127.0.0.1:0".into(),
        api_key: BEARER.into(),
        connect_timeout_secs: 2,
        ..Default::default()
    };
    let adapter = NewApiAdapter::new(config).unwrap();

    let err = adapter.probe_status().await.unwrap_err();
    assert!(matches!(err, Error::Transport { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_body_that_is_not_the_dashboard_envelope_is_rejected() {
    let state = MockState::default();
    *state.status_raw.lock().unwrap() = Some("<html>502 Bad Gateway</html>".into());
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let err = adapter.probe_status().await.unwrap_err();

    assert!(matches!(err, Error::BadUpstreamPayload(_)), "got {err:?}");
    assert!(
        !err.to_string().contains("Bad Gateway"),
        "body must not leak"
    );
}

#[tokio::test]
async fn a_data_field_of_the_wrong_shape_is_rejected() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = envelope(json!("not an object"));
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let err = adapter.fetch_quota(&account()).await.unwrap_err();

    assert!(matches!(err, Error::BadUpstreamPayload(_)), "got {err:?}");
}

#[tokio::test]
async fn a_business_failure_keeps_its_code_and_message_for_the_log() {
    let state = MockState::default();
    *state.self_body.lock().unwrap() = failure("AUTH_USER_DISABLED", "用户已被封禁");
    let panel = MockPanel::start_with(state).await;
    let adapter = panel.adapter();

    let err = adapter.fetch_quota(&account()).await.unwrap_err();

    match &err {
        Error::Upstream {
            provider,
            status,
            body,
            ..
        } => {
            assert_eq!(provider, "newapi");
            assert_eq!(*status, 200);
            assert_eq!(body, "AUTH_USER_DISABLED: 用户已被封禁");
        }
        other => panic!("expected an upstream error, got {other:?}"),
    }
    assert_eq!(
        err.safe_message(),
        "upstream newapi returned 200",
        "the client-facing view stays redacted"
    );
}

#[tokio::test]
async fn a_rejected_refresh_cookie_reads_as_unauthorized() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    *panel.state.refresh_status.lock().unwrap() = 401;

    let err = adapter
        .authenticate(NewApiAuth::Cookie {
            session_cookie: "stale".into(),
        })
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Unauthorized), "got {err:?}");
    assert!(!adapter.is_authenticated());
}

#[tokio::test]
async fn a_rotated_refresh_cookie_is_kept_for_the_next_renewal() {
    let panel = MockPanel::start().await;
    let adapter = panel.adapter_with(|config| config.api_key = String::new());
    panel.state.required_bearer.lock().unwrap().clear();
    adapter
        .authenticate(NewApiAuth::OAuth2 {
            access_token: "stale-jwt".into(),
            refresh_token: Some("first-cookie".into()),
        })
        .await
        .unwrap();

    panel.state.self_expired.store(1, Ordering::SeqCst);
    adapter.fetch_quota(&account()).await.unwrap();

    // The panel answered the refresh with a rotated cookie; the next
    // renewal must present that one, not the original.
    panel.state.self_expired.store(1, Ordering::SeqCst);
    adapter.fetch_quota(&account()).await.unwrap();

    let refreshes: Vec<Recorded> = panel
        .state
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.path == "/api/user/auth/refresh")
        .cloned()
        .collect();
    assert_eq!(refreshes.len(), 2);
    assert_eq!(
        refreshes[0].cookie.as_deref(),
        Some("new_api_refresh=first-cookie")
    );
    assert_eq!(
        refreshes[1].cookie.as_deref(),
        Some("new_api_refresh=rotated-cookie")
    );
}

// ── public helpers ───────────────────────────────────────────────────

#[test]
fn quota_conversion_never_divides_by_zero() {
    assert_eq!(quota_to_usd(1_000, 0.0), 0.0);
    assert_eq!(quota_to_usd(1_000, f64::NAN), 0.0);
    assert!((quota_to_usd(1_000_000, 500_000.0) - 2.0).abs() < 1e-9);
}

#[test]
fn newapi_auth_serialises_with_a_type_tag() {
    let value = serde_json::to_value(NewApiAuth::ApiKey { key: "k".into() }).unwrap();
    assert_eq!(value["type"], "api_key");
    assert_eq!(value["key"], "k");
    let parsed: NewApiAuth = serde_json::from_value(value).unwrap();
    assert!(matches!(parsed, NewApiAuth::ApiKey { .. }));
}

#[test]
fn newapi_status_defaults_quota_per_unit_when_absent() {
    let status = NewApiStatus::default();
    assert_eq!(status.quota_per_unit_or_default(), DEFAULT_QUOTA_PER_UNIT);
    let status = NewApiStatus {
        quota_per_unit: Some(-1.0),
        ..Default::default()
    };
    assert_eq!(status.quota_per_unit_or_default(), DEFAULT_QUOTA_PER_UNIT);
}
