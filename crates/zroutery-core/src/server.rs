//! The local HTTP server.
//!
//! Exposes one endpoint per dialect (`POST /v1/messages` and
//! `POST /v1/chat/completions`) plus a merged `GET /v1/models` listing that
//! satisfies both Anthropic and OpenAI clients.
//!
//! Security: it binds loopback by default and requires a local token. Anything
//! that can reach this port can spend the configured API keys.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};

use crate::config::{AppConfig, ProviderConfig, SecretStore};
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, Dialect, StreamEvent, Usage};
use crate::protocol::{self, openai, SseFrame, StreamEncoder};
use crate::registry::Registry;
use crate::router::{Candidate, Router};
use crate::stats::{RecordBuilder, Stats};
use crate::upstream::Upstream;

/// Everything a request handler needs.
pub struct AppState {
    config: RwLock<Arc<AppConfig>>,
    pub router: Arc<Router>,
    pub stats: Arc<Stats>,
    pub upstream: Upstream,
    pub secrets: Arc<dyn SecretStore>,
}

impl AppState {
    pub fn new(config: AppConfig, secrets: Arc<dyn SecretStore>) -> Self {
        let stats = Arc::new(Stats::new(config.server.log_limit));
        AppState {
            config: RwLock::new(Arc::new(config)),
            router: Arc::new(Router::new()),
            stats,
            upstream: Upstream::new(),
            secrets,
        }
    }

    pub fn config(&self) -> Arc<AppConfig> {
        Arc::clone(&self.config.read().expect("config poisoned"))
    }

    /// Swap in a new configuration. In-flight requests keep their snapshot.
    pub fn set_config(&self, config: AppConfig) {
        self.stats.set_limit(config.server.log_limit);
        *self.config.write().expect("config poisoned") = Arc::new(config);
    }

    pub fn registry(&self) -> Registry {
        Registry::new(self.config())
    }

    /// Resolve a provider's secret, failing loudly when one is expected but absent.
    pub fn api_key(&self, provider: &ProviderConfig) -> Result<Option<String>> {
        if provider.key_ref.is_empty() {
            return Ok(None);
        }
        match self.secrets.get(&provider.key_ref) {
            Some(k) if !k.is_empty() => Ok(Some(k)),
            _ => Err(Error::MissingApiKey(provider.name.clone())),
        }
    }
}

/// Build the axum application.
pub fn build_app(state: Arc<AppState>) -> AxumRouter {
    let mut app = AxumRouter::new()
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/chat/completions", post(openai_chat))
        .route("/v1/models", get(list_models))
        .route("/v1/models/{id}", get(get_model))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_layer,
        ))
        // Health is intentionally unauthenticated so the GUI can poll it.
        .route("/health", get(health))
        .with_state(Arc::clone(&state));

    if state.config().server.allow_cors {
        app = app.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );
    }
    app
}

/// A running server plus its shutdown handle.
pub struct ServerHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    /// Bind and serve. Returns as soon as the socket is listening.
    pub async fn start(state: Arc<AppState>) -> Result<ServerHandle> {
        let cfg = state.config();
        let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("cannot bind {addr}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::internal(format!("cannot read local addr: {e}")))?;

        let app = build_app(state);
        let (tx, rx) = oneshot::channel();
        let join = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            if let Err(e) = served {
                tracing::error!("server stopped: {e}");
            }
        });

        Ok(ServerHandle {
            addr,
            shutdown: Some(tx),
            join,
        })
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

// ------------------------------------------------------------------- handlers

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config();
    let registry = state.registry();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "models": registry.list().len(),
        "providers": cfg.providers.iter().filter(|p| p.enabled).count(),
        "auth_required": cfg.server.require_auth,
    }))
}

async fn auth_layer(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let cfg = state.config();
    if !cfg.server.require_auth {
        return next.run(request).await;
    }
    let expected = cfg.server.auth_token.as_bytes();
    let presented = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
        });

    let ok = match presented {
        Some(token) if !expected.is_empty() => constant_time_eq(token.as_bytes(), expected),
        _ => false,
    };
    if !ok {
        return error_response(Dialect::Anthropic, &Error::Unauthorized);
    }
    next.run(request).await
}

/// Length-independent comparison to avoid leaking the token via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

type JsonBody = std::result::Result<Json<Value>, JsonRejection>;

fn unwrap_body(body: JsonBody) -> Result<Value> {
    match body {
        Ok(Json(v)) => Ok(v),
        Err(e) => Err(Error::invalid(e.body_text())),
    }
}

async fn anthropic_messages(State(state): State<Arc<AppState>>, body: JsonBody) -> Response {
    let body = match unwrap_body(body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };
    handle_chat(state, Dialect::Anthropic, body).await
}

async fn openai_chat(State(state): State<Arc<AppState>>, body: JsonBody) -> Response {
    let body = match unwrap_body(body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::OpenAI, &e),
    };
    handle_chat(state, Dialect::OpenAI, body).await
}

async fn count_tokens(State(state): State<Arc<AppState>>, body: JsonBody) -> Response {
    let body = match unwrap_body(body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };
    match protocol::decode_request(Dialect::Anthropic, body) {
        Ok(req) => {
            // Best effort: providers do not expose a shared tokenizer, so this
            // is an estimate and is documented as such in the GUI.
            let _ = state;
            Json(json!({"input_tokens": req.estimate_tokens()})).into_response()
        }
        Err(e) => error_response(Dialect::Anthropic, &e),
    }
}

async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let registry = state.registry();
    let items: Vec<Value> = registry.list().iter().map(model_json).collect();
    let first = items.first().and_then(|m| m["id"].as_str()).unwrap_or("");
    let last = items.last().and_then(|m| m["id"].as_str()).unwrap_or("");
    Json(json!({
        "object": "list",
        "data": items,
        "has_more": false,
        "first_id": first,
        "last_id": last,
    }))
    .into_response()
}

async fn get_model(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let registry = state.registry();
    match registry.list().iter().find(|m| m.id == id) {
        Some(info) => Json(model_json(info)).into_response(),
        None => error_response(Dialect::OpenAI, &Error::UnknownModel(id)),
    }
}

/// One listing entry, carrying both the Anthropic and the OpenAI field names.
fn model_json(info: &crate::registry::ModelInfo) -> Value {
    let now = chrono::Utc::now();
    json!({
        "id": info.id,
        "object": "model",
        "type": "model",
        "created": now.timestamp(),
        "created_at": now.to_rfc3339(),
        "display_name": info.display_name,
        "owned_by": info.provider_name.clone().unwrap_or_else(|| "zroutery".into()),
        "zroutery": {
            "class": info.class,
            "virtual": info.virtual_model,
            "member_count": info.member_count,
            "provider": info.provider_name,
            "supports_tools": info.supports_tools,
            "supports_vision": info.supports_vision,
            "supports_thinking": info.supports_thinking,
        }
    })
}

fn error_response(dialect: Dialect, err: &Error) -> Response {
    (err.status(), Json(err.to_wire(dialect))).into_response()
}

// ------------------------------------------------------------------- pipeline

async fn handle_chat(state: Arc<AppState>, dialect: Dialect, body: Value) -> Response {
    let include_usage = dialect == Dialect::OpenAI && openai::wants_stream_usage(&body);

    let req = match protocol::decode_request(dialect, body) {
        Ok(r) => r,
        Err(e) => return error_response(dialect, &e),
    };

    let registry = state.registry();
    let plan = match registry
        .resolve(&req.model)
        .and_then(|res| state.router.plan(&registry, &res))
    {
        Ok(p) => p,
        Err(e) => {
            let mut rec = RecordBuilder::new(dialect, &req.model, req.stream);
            rec.fail(e.status().as_u16(), e.to_string());
            state.stats.record(rec.finish(0));
            return error_response(dialect, &e);
        }
    };

    if req.stream {
        stream_chat(state, dialect, req, plan, include_usage).await
    } else {
        buffered_chat(state, dialect, req, plan).await
    }
}

/// Prepare one attempt: resolve the key and encode the upstream body.
fn prepare(
    state: &AppState,
    candidate: &Candidate,
    req: &ChatRequest,
) -> Result<(Option<String>, Value)> {
    let key = state.api_key(&candidate.provider)?;
    let body = crate::upstream::encode_for(
        &candidate.provider,
        req,
        &candidate.entry.upstream_model,
        candidate.entry.max_output_tokens,
    )?;
    Ok((key, body))
}

async fn buffered_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    req: ChatRequest,
    plan: Vec<Candidate>,
) -> Response {
    let started = Instant::now();
    let mut rec = RecordBuilder::new(dialect, &req.model, false);
    let routing = state.config().routing.clone();
    let mut last_error = Error::NoCandidate(req.model.clone());

    for candidate in &plan {
        rec.attempt();
        rec.resolved(candidate.model_id(), &candidate.provider.name);
        let attempt_start = Instant::now();

        let (key, body) = match prepare(&state, candidate, &req) {
            Ok(v) => v,
            Err(e) => {
                state
                    .router
                    .report_failure(candidate.model_id(), &e, &routing);
                last_error = e;
                continue;
            }
        };

        match state
            .upstream
            .send(&candidate.provider, key.as_deref(), &body)
            .await
        {
            Ok(mut resp) => {
                state.router.report_success(
                    candidate.model_id(),
                    attempt_start.elapsed().as_millis() as u64,
                );
                rec.usage(resp.usage);
                state
                    .stats
                    .record(rec.finish(started.elapsed().as_millis() as u64));
                // Report the model that actually answered, not the virtual id.
                resp.model = candidate.exposed_id.clone();
                let mut response = Json(protocol::encode_response(dialect, &resp)).into_response();
                inject_routing_headers(response.headers_mut(), candidate);
                return response;
            }
            Err(e) => {
                state
                    .router
                    .report_failure(candidate.model_id(), &e, &routing);
                tracing::warn!(
                    model = candidate.model_id(),
                    provider = candidate.provider.name.as_str(),
                    "upstream attempt failed: {e}"
                );
                let retryable = e.is_retryable();
                last_error = e;
                if !retryable {
                    break;
                }
            }
        }
    }

    rec.fail(last_error.status().as_u16(), last_error.to_string());
    state
        .stats
        .record(rec.finish(started.elapsed().as_millis() as u64));
    error_response(dialect, &last_error)
}

async fn stream_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    req: ChatRequest,
    plan: Vec<Candidate>,
    include_usage: bool,
) -> Response {
    let started = Instant::now();
    let mut rec = RecordBuilder::new(dialect, &req.model, true);
    let routing = state.config().routing.clone();
    let mut last_error = Error::NoCandidate(req.model.clone());

    for candidate in &plan {
        rec.attempt();
        rec.resolved(candidate.model_id(), &candidate.provider.name);

        let (key, body) = match prepare(&state, candidate, &req) {
            Ok(v) => v,
            Err(e) => {
                state
                    .router
                    .report_failure(candidate.model_id(), &e, &routing);
                last_error = e;
                continue;
            }
        };

        // Only the handshake can be retried; once bytes are flowing the client
        // has already seen part of the answer.
        match state
            .upstream
            .stream(
                &candidate.provider,
                key.as_deref(),
                &body,
                &candidate.entry.upstream_model,
            )
            .await
        {
            Ok(events) => {
                state
                    .router
                    .report_success(candidate.model_id(), started.elapsed().as_millis() as u64);
                let encoder =
                    protocol::stream_encoder(dialect, &candidate.exposed_id, include_usage);
                let body = Body::from_stream(sse_body(
                    Arc::clone(&state),
                    events,
                    encoder,
                    rec,
                    started,
                    candidate.model_id().to_string(),
                    routing.clone(),
                ));
                let mut response = Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .header("x-accel-buffering", "no")
                    .body(body)
                    .expect("valid response");
                inject_routing_headers(response.headers_mut(), candidate);
                return response;
            }
            Err(e) => {
                state
                    .router
                    .report_failure(candidate.model_id(), &e, &routing);
                tracing::warn!(
                    model = candidate.model_id(),
                    "upstream stream handshake failed: {e}"
                );
                let retryable = e.is_retryable();
                last_error = e;
                if !retryable {
                    break;
                }
            }
        }
    }

    rec.fail(last_error.status().as_u16(), last_error.to_string());
    state
        .stats
        .record(rec.finish(started.elapsed().as_millis() as u64));
    error_response(dialect, &last_error)
}

fn inject_routing_headers(headers: &mut HeaderMap, candidate: &Candidate) {
    if let Ok(v) = HeaderValue::from_str(&candidate.exposed_id) {
        headers.insert("x-zroutery-model", v);
    }
    if let Ok(v) = HeaderValue::from_str(&candidate.provider.name) {
        headers.insert("x-zroutery-provider", v);
    }
    if candidate.degraded {
        headers.insert("x-zroutery-degraded", HeaderValue::from_static("1"));
    }
}

struct SseState {
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    pending: VecDeque<SseFrame>,
    rec: Option<RecordBuilder>,
    state: Arc<AppState>,
    started: Instant,
    model_id: String,
    routing: crate::config::RoutingConfig,
    usage: Usage,
    finished: bool,
}

impl SseState {
    /// Record the request exactly once, when the stream ends for any reason.
    fn finalize(&mut self, error: Option<&Error>) {
        if let Some(mut rec) = self.rec.take() {
            rec.usage(self.usage);
            if let Some(e) = error {
                rec.fail(e.status().as_u16(), e.to_string());
                self.state
                    .router
                    .report_failure(&self.model_id, e, &self.routing);
            }
            self.state
                .stats
                .record(rec.finish(self.started.elapsed().as_millis() as u64));
        }
    }
}

/// Pipe canonical events through the egress encoder into an SSE byte stream.
fn sse_body(
    app: Arc<AppState>,
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    rec: RecordBuilder,
    started: Instant,
    model_id: String,
    routing: crate::config::RoutingConfig,
) -> impl futures_util::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
    use futures_util::StreamExt;

    let state = SseState {
        events,
        encoder,
        pending: VecDeque::new(),
        rec: Some(rec),
        state: app,
        started,
        model_id,
        routing,
        usage: Usage::default(),
        finished: false,
    };

    futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(frame) = st.pending.pop_front() {
                return Some((Ok(bytes::Bytes::from(frame.to_wire())), st));
            }
            if st.finished {
                return None;
            }
            match st.events.next().await {
                Some(Ok(event)) => {
                    match &event {
                        StreamEvent::TextDelta { .. } | StreamEvent::ThinkingDelta { .. } => {
                            if let Some(rec) = st.rec.as_mut() {
                                rec.ttft(st.started.elapsed().as_millis() as u64);
                            }
                        }
                        StreamEvent::Start { usage, .. } => st.usage = *usage,
                        StreamEvent::Stop { usage, .. } => st.usage = *usage,
                        _ => {}
                    }
                    let frames = st.encoder.encode(&event);
                    st.pending.extend(frames);
                }
                Some(Err(err)) => {
                    st.finished = true;
                    st.finalize(Some(&err));
                    let frames = st.encoder.error(&err);
                    st.pending.extend(frames);
                }
                None => {
                    st.finished = true;
                    let frames = st.encoder.finish();
                    st.pending.extend(frames);
                    st.state
                        .router
                        .report_success(&st.model_id, st.started.elapsed().as_millis() as u64);
                    st.finalize(None);
                }
            }
        }
    })
}
