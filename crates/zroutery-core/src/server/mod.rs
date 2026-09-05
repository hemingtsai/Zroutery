//! The local HTTP server.
//!
//! Exposes one endpoint per dialect (`POST /v1/messages` and
//! `POST /v1/chat/completions`) plus a merged `GET /v1/models` listing that
//! satisfies both Anthropic and OpenAI clients.
//!
//! Security: it binds loopback by default and requires a local token. Anything
//! that can reach this port can spend the configured API keys.

mod pipeline;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Local;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::billing::{Cost, Pricing};
use crate::budget::{self, Ledger, Verdict};
use crate::config::{AppConfig, ModelTier, ProviderConfig, SecretStore, ServerConfig};
use crate::election::{self, Election, Measurement};
use crate::error::{Error, Result};
use crate::ir::Dialect;
use crate::protocol;
use crate::registry::Registry;
use crate::router::Router;
use crate::stats::Stats;
use crate::upstream::Upstream;

use pipeline::handle_chat;

/// Everything a request handler needs.
///
/// Fields are private so nothing outside can leave the cached registry out of
/// step with the configuration it was built from.
pub struct AppState {
    /// Configuration plus its precomputed lookup tables, swapped together.
    registry: RwLock<Arc<Registry>>,
    router: Arc<Router>,
    stats: Arc<Stats>,
    upstream: Upstream,
    secrets: Arc<dyn SecretStore>,
    /// Spend so far, which the budgets are checked against. It lives here rather
    /// than in `Stats` because the request log is deliberately memory only, and a
    /// guardrail that forgets on restart is not a guardrail.
    ledger: RwLock<Ledger>,
    /// Set when the ledger has moved since it was last written out, so the desktop
    /// layer can flush on a timer instead of writing a file per request.
    ledger_dirty: AtomicBool,
}

impl AppState {
    pub fn new(config: AppConfig, secrets: Arc<dyn SecretStore>) -> Self {
        let stats = Arc::new(Stats::new(config.server.log_limit));
        let bypass_proxy = config.server.bypass_proxy;
        let connect_timeout_secs = config
            .providers
            .first()
            .map(|p| p.connect_timeout_secs)
            .unwrap_or(15);
        AppState {
            registry: RwLock::new(Arc::new(Registry::new(Arc::new(config)))),
            router: Arc::new(Router::new()),
            stats,
            upstream: Upstream::new(bypass_proxy, connect_timeout_secs),
            secrets,
            ledger: RwLock::new(Ledger::new()),
            ledger_dirty: AtomicBool::new(false),
        }
    }

    /// Adopt a ledger read from disk.
    pub fn set_ledger(&self, ledger: Ledger) {
        *crate::sync::write(&self.ledger) = ledger;
        self.ledger_dirty.store(false, Ordering::Relaxed);
    }

    pub fn ledger(&self) -> Ledger {
        crate::sync::read(&self.ledger).clone()
    }

    /// Take the ledger for writing out, clearing the dirty flag.
    ///
    /// Returns `None` when nothing has changed, so an idle proxy does not rewrite
    /// the same file every few seconds. The snapshot is pruned so long-running
    /// processes do not accumulate stale day/month buckets in `spend.json`
    /// (the in-memory ledger keeps its buckets; checks ignore stale ones).
    pub fn take_dirty_ledger(&self) -> Option<Ledger> {
        self.ledger_dirty.swap(false, Ordering::Relaxed).then(|| {
            let mut ledger = self.ledger();
            ledger.prune(Local::now());
            ledger
        })
    }

    /// Record what a finished request cost, against every scope that covers it.
    pub fn charge(&self, provider_id: &str, tier: Option<ModelTier>, cost: &Cost) {
        crate::sync::write(&self.ledger).charge(Local::now(), provider_id, tier, cost);
        self.ledger_dirty.store(true, Ordering::Relaxed);
    }

    /// What the budgets say about a request that is about to be routed.
    pub fn budget_verdict(&self, provider_ids: &[String], tier: Option<ModelTier>) -> Verdict {
        let config = self.config();
        if config.budgets.is_empty() {
            return Verdict::Allow;
        }
        budget::check(
            &config.budgets,
            &crate::sync::read(&self.ledger),
            Local::now(),
            provider_ids,
            tier,
        )
    }

    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    pub fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    /// Rebuild the upstream HTTP client (e.g. when bypass_proxy changes).
    pub fn rebuild_upstream(&self, bypass_proxy: bool) {
        self.upstream.rebuild_client(bypass_proxy);
    }

    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    pub fn config(&self) -> Arc<AppConfig> {
        self.registry().snapshot()
    }

    /// Swap in a new configuration, rebuilding the lookup tables once here
    /// instead of per request. In-flight requests keep their snapshot.
    pub fn set_config(&self, config: AppConfig) {
        self.stats.set_limit(config.server.log_limit);
        let known: std::collections::HashSet<String> =
            config.models.iter().map(|m| m.exposed_id()).collect();
        // Health rows for models that no longer exist would otherwise linger
        // forever and keep appearing in the GUI snapshot.
        self.router.retain_models(|id| known.contains(id));
        let registry = Arc::new(Registry::new(Arc::new(config)));
        *crate::sync::write(&self.registry) = registry;
    }

    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&crate::sync::read(&self.registry))
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

    /// Probe every tier member, rank the results, and pin the outcome.
    ///
    /// The runner lives here rather than in [`crate::election`] because it needs the
    /// registry, the HTTP client and the secret store, and keeping those out of the
    /// scoring module is what lets the scoring be a pure function with tests that
    /// never open a socket.
    ///
    /// Probes run one at a time on purpose: fired together they would queue behind
    /// each other on the same uplink and measure the queue instead of the providers.
    pub async fn hold_election(&self) -> Election {
        let registry = self.registry();
        let scoring = registry.config().routing.scoring.clone();
        let mut election = Election::new(scoring.clone());

        for tier in ModelTier::ALL {
            let members = registry.tier_members(tier);
            if members.is_empty() {
                continue;
            }

            let mut measurements = Vec::with_capacity(members.len());
            for entry in members {
                let exposed = entry.exposed_id();
                let mut measurement = Measurement::new(&exposed);
                measurement.priority = entry.priority;
                measurement.price = entry.pricing.as_ref().map(|p| scoring.reference_cost(p));

                measurement = match registry
                    .provider_of(entry)
                    .and_then(|provider| Ok((provider, self.api_key(provider)?)))
                {
                    Ok((provider, key)) => {
                        match self
                            .upstream
                            .probe(provider, key.as_deref(), &entry.upstream_model)
                            .await
                        {
                            Ok(latency) => {
                                // A probe is a real call, so it is also the freshest
                                // health signal there is.
                                let routing = &registry.config().routing;
                                self.router.report_success(&exposed, latency, routing);
                                measurement.answered(latency)
                            }
                            Err(e) => {
                                let routing = &registry.config().routing;
                                self.router.report_failure(&exposed, &e, routing);
                                measurement.failed(e.to_string())
                            }
                        }
                    }
                    Err(e) => measurement.failed(e.to_string()),
                };
                measurements.push(measurement);
            }

            election
                .tiers
                .insert(tier, election::rank(tier, &measurements, &scoring));
        }

        self.router.set_election(election.clone());
        election
    }
}

/// Build the axum application.
/// Every path the proxy answers on, in the order the docs list them.
///
/// Both prefixes are served. OpenAI clients are usually configured with a base
/// URL that already ends in `/v1` and append `/models` themselves, but plenty of
/// tools take the host on its own and do the same, so serving only one prefix
/// turns a working configuration into a bare 404.
const ENDPOINTS: &[&str] = &[
    "/v1/messages",
    "/v1/messages/count_tokens",
    "/v1/chat/completions",
    "/v1/responses",
    "/v1/generateContent",
    "/v1/models",
    "/v1/models/{id}",
    "/v1/status",
    "/health",
];

pub fn build_app(state: Arc<AppState>) -> AxumRouter {
    let cfg = state.config();
    let mut api = AxumRouter::new();
    // `/x` and `/v1/x` reach the same handler, so a base URL with or without the
    // version prefix both work.
    for prefix in ["", "/v1"] {
        api = api
            .route(&format!("{prefix}/messages"), post(anthropic_messages))
            .route(
                &format!("{prefix}/messages/count_tokens"),
                post(count_tokens),
            )
            .route(&format!("{prefix}/chat/completions"), post(openai_chat))
            .route(&format!("{prefix}/responses"), post(responses_chat))
            .route(&format!("{prefix}/generateContent"), post(gemini_generate))
            .route(&format!("{prefix}/models"), get(list_models))
            .route(&format!("{prefix}/models/{{id}}"), get(get_model))
            .route(&format!("{prefix}/status"), get(status));
    }

    let mut app = api
        // Prompts with inline images are large, runaway bodies are not.
        .layer(DefaultBodyLimit::max(cfg.server.max_body_bytes()))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_layer,
        ))
        // Liveness only, and deliberately unauthenticated: it says nothing about
        // the configuration. `/v1/status` behind the token has the detail.
        .route("/health", get(health))
        // An unmatched path used to answer with an empty 404, which tells a user
        // nothing about whether the proxy is even running. A wrong method on a
        // real path is worth spelling out too.
        .fallback(unknown_route)
        .method_not_allowed_fallback(wrong_method)
        .with_state(Arc::clone(&state));

    if let Some(cors) = cors_layer(&cfg.server) {
        app = app.layer(cors);
    }
    app
}

/// Answer an unknown path with something a human can act on.
///
/// Only route names are listed, never configuration: this runs before
/// authentication, because an unmatched path never reaches the auth layer.
async fn unknown_route(method: Method, uri: Uri) -> Response {
    let path = uri.path().to_string();
    let error = Error::UnknownRoute(format!("{method} {path}"));
    // Anthropic clients parse a different error envelope, so answer in the shape
    // the caller is most likely to understand.
    let dialect = if path.contains("messages") || path.contains("count_tokens") {
        Dialect::Anthropic
    } else if path.contains("responses") {
        Dialect::OpenAIResponses
    } else if path.contains("generateContent") {
        Dialect::Gemini
    } else {
        Dialect::OpenAI
    };

    let mut hint = serde_json::Map::new();
    hint.insert("endpoints".into(), json!(ENDPOINTS));
    // The usual cause: a base URL that already ends in /v1 while the client
    // appends /v1 as well, which the Anthropic SDKs do.
    if let Some(rest) = path.strip_prefix("/v1/v1/") {
        hint.insert(
            "likely_cause".into(),
            json!(format!(
                "the base URL already ends in /v1 and the client added another; \
                 drop the /v1 from the base URL and this becomes /v1/{rest}"
            )),
        );
    }
    let mut body = error.to_wire(dialect);
    if let Some(object) = body.as_object_mut() {
        object.insert("zroutery".into(), Value::Object(hint));
    }
    (error.status(), Json(body)).into_response()
}

/// The path exists but not for this verb. Say which one it wants, since an empty
/// 405 reads exactly like a broken proxy.
///
/// Like the 404 handler this runs before authentication, so it names routes and
/// nothing else. That a well known path exists is already public.
async fn wrong_method(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    let wanted = if path.ends_with("/models") || path.ends_with("/status") {
        "GET"
    } else {
        "POST"
    };
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": {
                "message": format!("{path} is a {wanted} endpoint, not {method}"),
                "type": "invalid_request_error",
                "code": "method_not_allowed",
            },
            "zroutery": {"endpoints": ENDPOINTS},
        })),
    )
        .into_response()
}

/// Build the CORS layer for the configured origins.
///
/// Browsers are the only reason this exists, so the allowed methods and headers
/// are pinned to what the two APIs actually use instead of `Any`. An empty origin
/// list with CORS enabled means "any origin", which `AppConfig::validate` flags as
/// a warning and the dashboard shows in red.
fn cors_layer(server: &ServerConfig) -> Option<CorsLayer> {
    if !server.allow_cors {
        return None;
    }
    let mut layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            HeaderName::from_static("x-api-key"),
            HeaderName::from_static("anthropic-version"),
            HeaderName::from_static("anthropic-beta"),
        ])
        .max_age(Duration::from_secs(600));

    // Origins split into valid / invalid. An empty *configured* list means
    // "allow any", but if the user configured origins and none of them are
    // usable, deny everything rather than silently widening to `Any` — a
    // typo must never turn into an open CORS policy.
    match origin_policy(&server.cors_origins) {
        OriginPolicy::Any => layer = layer.allow_origin(Any),
        OriginPolicy::List(valid) => {
            let origins: Vec<HeaderValue> = valid
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect();
            layer = layer.allow_origin(AllowOrigin::list(origins));
        }
        OriginPolicy::DenyAll => layer = layer.allow_origin(AllowOrigin::list(Vec::new())),
    }
    Some(layer)
}

/// What to allow, derived from the configured `cors_origins`.
#[derive(Debug, PartialEq, Eq)]
enum OriginPolicy {
    /// No origins configured: allow any.
    Any,
    /// These (validated) origins are allowed.
    List(Vec<String>),
    /// Origins were configured but none are valid: allow none. A typo must
    /// never widen an explicit allow-list into an open policy.
    DenyAll,
}

fn origin_policy(configured: &[String]) -> OriginPolicy {
    let valid: Vec<String> = configured
        .iter()
        .map(|o| o.trim())
        .filter(|o| is_valid_origin(o))
        .map(str::to_string)
        .collect();
    if valid.is_empty() {
        let any_configured = configured.iter().any(|o| !o.trim().is_empty());
        if any_configured {
            tracing::warn!("every configured cors_origin is invalid; no origin will be allowed");
            OriginPolicy::DenyAll
        } else {
            OriginPolicy::Any
        }
    } else {
        if valid.len() != configured.len() {
            tracing::warn!("some cors_origins entries are invalid and were ignored");
        }
        OriginPolicy::List(valid)
    }
}

/// Whether `origin` is a well-formed `scheme://host[:port]` value usable as a
/// CORS allow-origin entry. Rejects paths, queries, fragments, whitespace,
/// userinfo, non-ASCII text and invalid ports — those are config mistakes, and
/// treating them as "no origins" would silently open the proxy to `Any`.
pub(crate) fn is_valid_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    if rest.is_empty() || !rest.is_ascii() {
        return false;
    }
    // No path / query / fragment / whitespace / userinfo after the authority.
    if rest.contains(['/', '?', '#', ' ', '\t', '@']) {
        return false;
    }

    let (host, port) = if let Some(stripped) = rest.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:8787.
        let Some(close) = stripped.find(']') else {
            return false;
        };
        let host = &stripped[..close];
        let after = &stripped[close + 1..];
        if host.is_empty() {
            return false;
        }
        if after.is_empty() {
            (host, None)
        } else if let Some(port) = after.strip_prefix(':') {
            (host, Some(port))
        } else {
            return false;
        }
    } else {
        match rest.rsplit_once(':') {
            None => (rest, None),
            Some((_, "")) => return false, // trailing colon, no port
            Some((host, _)) if host.contains(':') => return false, // bare IPv6 without []
            Some((host, port)) => (host, Some(port)),
        }
    };

    if host.is_empty() {
        return false;
    }
    if let Some(port) = port {
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if port.parse::<u16>().is_err() {
            return false;
        }
    }
    true
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

/// Liveness probe. Says nothing about the configuration on purpose: it is the
/// only route that does not require the token.
async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

/// The detail that `/health` used to leak, behind authentication.
async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let registry = state.registry();
    let cfg = registry.config();
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
        let path = request.uri().path();
        let dialect = if path.contains("messages") || path.contains("count_tokens") {
            Dialect::Anthropic
        } else if path.contains("responses") {
            Dialect::OpenAIResponses
        } else if path.contains("generateContent") {
            Dialect::Gemini
        } else {
            Dialect::OpenAI
        };
        return error_response(dialect, &Error::Unauthorized);
    }
    next.run(request).await
}

/// Length-independent comparison so timing does not leak how much of a
/// presented token was right, or how long the expected one is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    // Walk the longer side; zipping alone would stop at the shorter and give
    // length information back through runtime.
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

type JsonBody = std::result::Result<Json<Value>, JsonRejection>;

fn unwrap_body(state: &AppState, body: JsonBody) -> Result<Value> {
    match body {
        Ok(Json(v)) => Ok(v),
        // A body over the limit is a different problem from malformed JSON, and
        // clients back off differently for 413 than for 400.
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(Error::TooLarge {
            limit_mib: state.config().server.max_body_mib,
        }),
        Err(e) => Err(Error::invalid(e.body_text())),
    }
}

async fn anthropic_messages(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };
    handle_chat(state, Dialect::Anthropic, headers, body).await
}

async fn openai_chat(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::OpenAI, &e),
    };
    handle_chat(state, Dialect::OpenAI, headers, body).await
}

async fn responses_chat(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::OpenAIResponses, &e),
    };
    handle_chat(state, Dialect::OpenAIResponses, headers, body).await
}

async fn gemini_generate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Gemini, &e),
    };
    handle_chat(state, Dialect::Gemini, headers, body).await
}

async fn count_tokens(State(state): State<Arc<AppState>>, body: JsonBody) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };
    let req = match protocol::decode_request(Dialect::Anthropic, body) {
        Ok(req) => req,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };

    // Best effort: providers do not expose a shared tokenizer, so this is an
    // estimate, and the price beside it covers the prompt only.
    let estimate = req.estimate_tokens();
    let mut extra = serde_json::Map::new();
    extra.insert("estimated".into(), json!(true));
    if let Some((model_id, pricing)) = first_candidate_pricing(&state, &req.model) {
        extra.insert("model".into(), json!(model_id));
        if let Some(pricing) = pricing {
            let cost = pricing.estimate_input(estimate);
            extra.insert(
                "estimated_input_cost".into(),
                json!({"currency": cost.currency, "amount": cost.amount}),
            );
            extra.insert("input_per_mtok".into(), json!(pricing.input_per_mtok));
            extra.insert("output_per_mtok".into(), json!(pricing.output_per_mtok));
        }
    }

    Json(json!({
        "input_tokens": estimate,
        // Namespaced, so Anthropic clients that only read `input_tokens` ignore it.
        "zroutery": Value::Object(extra),
    }))
    .into_response()
}

/// Which model would answer this request, and what it charges.
fn first_candidate_pricing(state: &AppState, requested: &str) -> Option<(String, Option<Pricing>)> {
    let registry = state.registry();
    let resolution = registry.resolve(requested).ok()?;
    let plan = state.router().plan(&registry, &resolution).ok()?;
    let candidate = plan.into_iter().next()?;
    Some((candidate.exposed_id, candidate.entry.pricing))
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
            "tier": info.tier,
            "virtual": info.virtual_model,
            "member_count": info.member_count,
            "provider": info.provider_name,
            "supports_tools": info.capabilities.tools,
            "supports_vision": info.capabilities.vision,
            "supports_thinking": info.capabilities.thinking,
            "capabilities": info.capabilities,
        }
    })
}

fn error_response(dialect: Dialect, err: &Error) -> Response {
    (err.status(), Json(err.to_wire(dialect))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderKind;

    /// Read a handler's JSON body back out.
    async fn body_json(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[test]
    fn token_comparison_is_length_safe_and_exact() {
        assert!(constant_time_eq(b"zr-abc", b"zr-abc"));
        assert!(!constant_time_eq(b"zr-abc", b"zr-abd"));
        // A prefix must not pass, which is what a naive loop would allow.
        assert!(!constant_time_eq(b"zr-ab", b"zr-abc"));
        assert!(!constant_time_eq(b"", b"zr-abc"));
        assert!(constant_time_eq(b"", b""));
        // Length differences that alias through a narrow accumulator must not
        // sneak past either.
        let zeros = vec![0u8; 256];
        assert!(!constant_time_eq(b"", &zeros));
        assert!(constant_time_eq(&zeros, &zeros));
    }

    #[tokio::test]
    async fn an_unknown_path_answers_in_the_likely_dialect() {
        // A models path is an OpenAI client's, so use its envelope.
        let response = unknown_route(Method::GET, "/v2/models".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "not_found_error");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("GET /v2/models"));
        assert!(body["zroutery"]["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "/v1/models"));

        // Anything that mentions messages is an Anthropic client's.
        let response = unknown_route(Method::POST, "/v1/messages/typo".parse().unwrap()).await;
        let body = body_json(response).await;
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "not_found_error");
    }

    #[tokio::test]
    async fn a_doubled_version_prefix_is_named_as_the_cause() {
        let response = unknown_route(Method::POST, "/v1/v1/messages".parse().unwrap()).await;
        let body = body_json(response).await;
        let cause = body["zroutery"]["likely_cause"].as_str().unwrap();
        assert!(cause.contains("base URL already ends in /v1"), "{cause}");
        assert!(cause.contains("/v1/messages"), "{cause}");

        // A single prefix is normal and gets no lecture.
        let response = unknown_route(Method::POST, "/v1/nope".parse().unwrap()).await;
        assert!(body_json(response).await["zroutery"]["likely_cause"].is_null());
    }

    #[tokio::test]
    async fn a_wrong_verb_names_the_verb_it_wants() {
        let response = wrong_method(Method::GET, "/v1/chat/completions".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let message = body_json(response).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("POST endpoint"), "{message}");
        assert!(message.contains("not GET"), "{message}");

        // Listings are the other way round.
        let response = wrong_method(Method::POST, "/v1/models".parse().unwrap()).await;
        let message = body_json(response).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("GET endpoint"), "{message}");
    }

    #[test]
    fn cors_is_off_unless_asked_for() {
        let mut server = ServerConfig::default();
        assert!(cors_layer(&server).is_none());
        server.allow_cors = true;
        assert!(cors_layer(&server).is_some());
        // An empty list still builds a layer, and `validate` is what complains.
        server.cors_origins = vec!["http://localhost:3000".into()];
        assert!(cors_layer(&server).is_some());
        // A malformed origin is dropped rather than rejected: the rest still works.
        server.cors_origins = vec!["not a header value\n".into()];
        assert!(cors_layer(&server).is_some());
    }

    #[test]
    fn is_valid_origin_rejects_paths_scheme_less_and_bad_ports() {
        assert!(is_valid_origin("http://localhost:3000"));
        assert!(is_valid_origin("https://app.example.com"));
        assert!(is_valid_origin("tauri://localhost"));
        assert!(is_valid_origin("http://[::1]:8787"));
        assert!(is_valid_origin("http://[::1]"));
        assert!(!is_valid_origin("https://app.example.com/path"));
        assert!(!is_valid_origin("https://app.example.com?q=1"));
        assert!(!is_valid_origin("app.example.com"));
        assert!(!is_valid_origin(""));
        assert!(!is_valid_origin("has space://x"));
        assert!(!is_valid_origin("https://app.example.com:99999"));
        assert!(!is_valid_origin("https://app.example.com:abc"));
        assert!(!is_valid_origin("https://app.example.com:"));
        assert!(!is_valid_origin("https://user@example.com"));
        assert!(!is_valid_origin("https://例子.测试"));
    }

    #[test]
    fn origin_policy_never_widens_on_invalid_entries() {
        use OriginPolicy::*;
        // Nothing configured: documented Any.
        assert_eq!(origin_policy(&[]), Any);
        // Valid entries: a list.
        assert_eq!(
            origin_policy(&["http://a".into(), "https://b".into()]),
            List(vec!["http://a".to_string(), "https://b".to_string()])
        );
        // Mixed: invalid entries are dropped, valid ones survive.
        assert_eq!(
            origin_policy(&["https://ok".into(), "garbage".into()]),
            List(vec!["https://ok".into()])
        );
        // All invalid (or whitespace-only): deny — never widen to Any.
        assert_eq!(origin_policy(&["garbage".into()]), DenyAll);
        assert_eq!(origin_policy(&["   ".into()]), Any);
    }

    #[test]
    fn a_missing_key_is_a_precondition_not_a_silent_none() {
        let secrets =
            Arc::new(crate::config::MemorySecretStore::new().with("provider:has", "sk-1"));
        let state = AppState::new(AppConfig::default(), secrets);

        let mut provider = ProviderConfig::new("has", "Has", ProviderKind::OpenAICompatible);
        provider.key_ref = "provider:has".into();
        assert_eq!(state.api_key(&provider).unwrap().as_deref(), Some("sk-1"));

        provider.key_ref = "provider:missing".into();
        assert!(matches!(
            state.api_key(&provider),
            Err(Error::MissingApiKey(_))
        ));

        // A provider that needs no credential says so explicitly.
        provider.key_ref = String::new();
        assert!(state.api_key(&provider).unwrap().is_none());
    }

    #[test]
    fn swapping_the_config_rebuilds_the_registry_with_it() {
        let secrets = Arc::new(crate::config::MemorySecretStore::new());
        let state = AppState::new(AppConfig::default(), secrets);
        assert!(state.registry().list().is_empty());

        let mut next = AppConfig::default();
        next.providers.push(ProviderConfig::new(
            "p",
            "P",
            ProviderKind::OpenAICompatible,
        ));
        next.models.push(crate::config::ModelEntry::for_upstream(
            "p",
            "m",
            Some(crate::config::ModelTier::Standard),
        ));
        state.set_config(next);

        // The cached index has to move with the document, not lag behind it.
        assert_eq!(state.config().models.len(), 1);
        assert!(state.registry().list().iter().any(|m| m.id == "p-m"));
        assert!(state
            .registry()
            .resolve("standard-class")
            .is_ok_and(|r| matches!(r, crate::registry::Resolution::Tier(_))));
    }
}
