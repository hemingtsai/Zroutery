//! The route table and one function per route, plus the auth layer they all sit behind.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use serde_json::{json, Value};

use crate::billing::Pricing;
use crate::error::{Error, Result};
use crate::ir::Dialect;
use crate::protocol;

use super::cors::cors_layer;
use super::pipeline::handle_chat;
use super::state::AppState;

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
    "/v1/responses/{response_id}",
    "/v1/responses/{response_id}/cancel",
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
            .route(
                &format!("{prefix}/responses/{{response_id}}"),
                get(get_response).delete(delete_response),
            )
            .route(
                &format!("{prefix}/responses/{{response_id}}/cancel"),
                post(cancel_response),
            )
            .route(&format!("{prefix}/generateContent"), post(gemini_generate))
            .route(&format!("{prefix}/models"), get(list_models))
            .route(&format!("{prefix}/models/{{id}}"), get(get_model))
            .route(&format!("{prefix}/status"), get(status))
            .route(&format!("{prefix}/ml/status"), get(ml_status))
            .route(&format!("{prefix}/ml/shadow"), get(ml_shadow))
            .route(&format!("{prefix}/ml/rollback"), post(ml_rollback))
            .route(&format!("{prefix}/ml/promote"), post(ml_promote))
            .route(
                &format!("{prefix}/ml/traces"),
                get(ml_traces).delete(ml_traces_clear),
            );
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
pub(crate) async fn unknown_route(method: Method, uri: Uri) -> Response {
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
pub(crate) async fn wrong_method(method: Method, uri: Uri) -> Response {
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

/// The optional bound on a replay, from the query string.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

#[cfg(feature = "ml")]
impl LimitQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_SHADOW_LIMIT)
    }
}

/// Records read by an on-demand replay when the caller does not say.
///
/// Sized to be comfortable to run from a UI click while still covering enough
/// traffic for the agreement and regret numbers to mean something. The caller
/// can ask for more; nobody should want to.
#[cfg(feature = "ml")]
const DEFAULT_SHADOW_LIMIT: usize = 5_000;

/// Read the learned model's status.
///
/// Read-only, and behind the same auth layer as everything else: an operator
/// surface that leaked which model is serving, and on whose authority, over an
/// unauthenticated port would be a disclosure.
///
/// Without the `ml` feature this reports `available: false` rather than 404, so
/// a client can tell "this build has no ML stack" from "this build does and the
/// request went somewhere wrong".
#[cfg(feature = "ml")]
async fn ml_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let status = state.ml_status();
    Json(json!({
        "available": true,
        "routing_with_a_model": status.is_routing_with_a_model(),
        "headline": status.headline(),
        "status": status,
    }))
}

#[cfg(not(feature = "ml"))]
async fn ml_status() -> Json<Value> {
    Json(json!({
        "available": false,
        "routing_with_a_model": false,
        "headline": "this build contains no ML stack; routing is deterministic",
    }))
}

/// Replay the serving model over recorded history, on demand.
///
/// Bounded by the `limit` query parameter, and capped again inside, because this
/// is a button in a desktop app and an unbounded replay is a hang.
#[cfg(feature = "ml")]
async fn ml_shadow(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(limit): axum::extract::Query<LimitQuery>,
) -> Json<Value> {
    Json(json!(state.ml_shadow_analysis(limit.limit())))
}

#[cfg(not(feature = "ml"))]
async fn ml_shadow() -> Json<Value> {
    Json(json!({
        "available": false,
        "traces_read": 0,
        "reason": "this build contains no ML stack",
    }))
}

/// Roll back to the previously promoted model, or to deterministic routing if
/// there was not one.
#[cfg(feature = "ml")]
async fn ml_rollback(State(state): State<Arc<AppState>>) -> Json<Value> {
    let outcome = state.rollback_active_model();
    Json(json!({
        "outcome": outcome,
        "status": state.ml_status(),
    }))
}

#[cfg(not(feature = "ml"))]
async fn ml_rollback() -> Json<Value> {
    Json(json!({
        "error": "this build contains no ML stack, so nothing can be rolled back",
    }))
}

/// Inspect the durable trace log, and let an operator discard it.
///
/// `GET` reports size and record count without reading the file into memory —
/// `count` streams, and it is the same number the promotion gate uses to decide
/// whether a body is trainable.
///
/// `DELETE` exists because the log is the operator's evidence and only the
/// operator should decide to destroy it. There is no automatic retention: the
/// promotion round trains from the whole log, so a retention policy that fired on
/// its own would change what the next model learns from without anyone asking.
/// The response says what was removed *and* what it affects, because the second
/// is the part that is not obvious.
///
/// It is a separate route from `status` rather than a parameter on it: this is
/// destructive and there is no reason it should be reachable by a typo'd query
/// string on a read.
#[cfg(feature = "ml")]
async fn ml_traces(State(state): State<Arc<AppState>>) -> Json<Value> {
    // The instance `AppState` holds, not a freshly opened one. Two `TraceLog`s on
    // one path would be two independent handles, and reporting on a second one
    // would describe a file nobody is appending to.
    let Some(log) = state.traces() else {
        return Json(json!({
            "error": "no durable state directory is configured, so there is no trace log",
        }));
    };
    match log.count() {
        Ok(records) => Json(json!({
            "records": records,
            "bytes_on_disk": std::fs::metadata(log.path()).map(|m| m.len()).unwrap_or(0),
            "path": log.path().display().to_string(),
            "counters": log.counters(),
        })),
        Err(error) => Json(json!({ "error": error.to_string() })),
    }
}

/// The no-ML twin. Present because the route is registered unconditionally, and
/// because a 404 on a read would be a worse answer than "this build keeps no
/// trace log" -- the caller asked a fair question of a build that genuinely has
/// nothing to report.
#[cfg(not(feature = "ml"))]
async fn ml_traces() -> Json<Value> {
    Json(json!({
        "available": false,
        "error": "this build contains no ML stack, so it keeps no trace log",
    }))
}

#[cfg(feature = "ml")]
async fn ml_traces_clear(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Through the live log, so `clear` drops the append handle this process is
    // actually holding. Clearing a second handle would truncate the file and then
    // leave the serving path holding a handle to a file it had already written.
    let outcome = match state.traces() {
        None => json!({
            "error": "no durable state directory is configured, so there is no trace log",
        }),
        Some(log) => match log.clear() {
            Ok(cleared) => json!({
                "cleared": cleared.cleared,
                "removed_bytes": cleared.removed_bytes,
                "note": "the durable trace log is now empty; the in-memory dataset still \
                         holds recent samples, and the next promotion round trains only \
                         on records written from here on",
            }),
            Err(error) => json!({ "error": error.to_string() }),
        },
    };
    Json(outcome)
}

#[cfg(not(feature = "ml"))]
async fn ml_traces_clear() -> Json<Value> {
    Json(json!({
        "error": "this build contains no ML stack, so it keeps no trace log to clear",
    }))
}

/// Run one promotion round over the durable history.
///
/// `install` is a query parameter rather than the default, because judging a model
/// and installing it are different acts and only the second changes what every
/// later request is served by. `POST /v1/ml/promote` with no `install` reports what
/// the gate would decide and changes nothing, which makes the endpoint safe to poll
/// and safe to point a dashboard at. `?install=true` asks for the model to go live,
/// and then it is on the live router before the response is written, because
/// `reload_active_model` attaches in-process rather than waiting for a restart.
///
/// Nothing here schedules. When a round happens is the caller's decision, which is
/// why the endpoint has no timer and no cadence of its own.
#[cfg(feature = "ml")]
async fn ml_promote(
    State(state): State<Arc<AppState>>,
    // Fully qualified, like `ml_shadow` above. The bare `Query` would need an
    // import that is only used from ml-gated handlers, so a `--no-default-features`
    // build would warn about an unused import on a row CI actually runs.
    axum::extract::Query(request): axum::extract::Query<PromoteRequest>,
) -> Json<Value> {
    // Only the baseline is the caller's to choose. The evidence floors — minimum
    // paired requests, required utility delta, permitted regressions — stay at their
    // shipped values, because relaxing those is not a statement about *what* to hold
    // a model to, it is a decision to stop requiring evidence at all.
    let mut gate = crate::ml::PromotionConfig::default();
    if let Some(baseline) = request.baseline.as_deref() {
        gate.required_baseline = baseline.to_string();
    }
    let config = crate::ml::RoundConfig {
        gate,
        ..crate::ml::RoundConfig::default()
    };
    let outcome = state.ml_run_promotion_round(config, request.install, request.revision);
    Json(json!({
        "round": outcome,
        "status": state.ml_status(),
    }))
}

#[cfg(not(feature = "ml"))]
async fn ml_promote() -> Json<Value> {
    Json(json!({
        "error": "this build contains no ML stack, so there is nothing to promote",
    }))
}

/// What to do about the round a `POST /v1/ml/promote` just ran.
///
/// Gated with the handler that deserialises it. Ungated it is dead code in a
/// `--no-default-features` build, and that is a row CI runs.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct PromoteRequest {
    /// Install the model if — and only if — the gate authorised it.
    ///
    /// Defaults to false. A promotion is the most consequential thing this process
    /// does on request, so it is asked for rather than inferred.
    #[serde(default)]
    install: bool,
    /// Recorded on the gate decision, so a promotion or refusal can be tied back to
    /// the round that produced it.
    #[serde(default)]
    revision: Option<String>,
    /// Which baseline the model must beat.
    ///
    /// "Beats a baseline" is a claim about a *specific* baseline, so the endpoint
    /// names one rather than letting the promotion quietly pick whichever it
    /// happened to win against. The shipped default is `baseline.lowest_latency`;
    /// `baseline.priority` is the strategy Zroutery routes by, and is what most
    /// operators mean. This changes only which comparison is required — the evidence
    /// floors are not caller-controlled.
    #[serde(default)]
    baseline: Option<String>,
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
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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

pub(crate) type JsonBody = std::result::Result<Json<Value>, JsonRejection>;

pub(crate) fn unwrap_body(state: &AppState, body: JsonBody) -> Result<Value> {
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
    let plan = state.router().plan(&registry, &resolution, &[]).ok()?;
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
        // Additive, and omitted by JSON as `null` when undeclared. The OpenAI
        // listing has no context field, so this is the extra that lets a client
        // that reads it avoid inventing one; a client that does not ignores it.
        "context_window": info.context_window,
        "zroutery": {
            "tier": info.tier,
            "virtual": info.virtual_model,
            "member_count": info.member_count,
            "provider": info.provider_name,
            "context_window": info.context_window,
            "supports_tools": info.capabilities.tools,
            "supports_vision": info.capabilities.vision,
            "supports_thinking": info.capabilities.thinking,
            "capabilities": info.capabilities,
        }
    })
}

async fn get_response(
    State(state): State<Arc<AppState>>,
    Path(response_id): Path<String>,
) -> Response {
    match state.response_store.get(&response_id) {
        Some(resp) => Json(serde_json::to_value(&resp).unwrap()).into_response(),
        None => error_response(
            Dialect::OpenAIResponses,
            &Error::invalid("response not found"),
        ),
    }
}

async fn delete_response(
    State(state): State<Arc<AppState>>,
    Path(response_id): Path<String>,
) -> Response {
    if state.response_store.delete(&response_id) {
        Json(json!({"id": response_id, "object": "response", "deleted": true})).into_response()
    } else {
        error_response(
            Dialect::OpenAIResponses,
            &Error::invalid("response not found"),
        )
    }
}

async fn cancel_response(
    State(state): State<Arc<AppState>>,
    Path(response_id): Path<String>,
) -> Response {
    // Cancellation stores a content-free placeholder itself, but only when the
    // request asked for retention; a `store: false` client gets the status
    // without anything being kept.
    if state.response_store.cancel(&response_id, "unknown") {
        // Return the cancelled response.
        match state.response_store.get(&response_id) {
            Some(resp) => Json(serde_json::to_value(&resp).unwrap()).into_response(),
            None => Json(json!({
                "id": response_id, "object": "response", "status": "cancelled"
            }))
            .into_response(),
        }
    } else {
        // Not in-flight — check if it is a completed response.
        match state.response_store.get(&response_id) {
            Some(resp) => Json(serde_json::to_value(&resp).unwrap()).into_response(),
            None => error_response(
                Dialect::OpenAIResponses,
                &Error::invalid("response not found"),
            ),
        }
    }
}

pub(crate) fn error_response(dialect: Dialect, err: &Error) -> Response {
    (err.status(), Json(err.to_wire(dialect))).into_response()
}
