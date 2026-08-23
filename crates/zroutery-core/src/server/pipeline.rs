//! The request pipeline: walk the router's plan, talk to a provider, and turn the
//! answer back into the client's dialect.
//!
//! Split from the routing table because the two change for different reasons. That
//! file is about which paths exist; this one is about what happens once a request
//! is on one of them.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use super::{error_response, AppState};
use crate::billing::{Cost, Pricing};
use crate::budget::Verdict;
use crate::config::ModelClass;
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, Dialect, StreamEvent, Usage};
use crate::protocol::{self, openai, SseFrame, StreamEncoder};
use crate::registry::{Registry, Resolution};
use crate::router::Candidate;
use crate::stats::RecordBuilder;
pub(super) async fn handle_chat(state: Arc<AppState>, dialect: Dialect, body: Value) -> Response {
    let include_usage = dialect == Dialect::OpenAI && openai::wants_stream_usage(&body);

    let req = match protocol::decode_request(dialect, body) {
        Ok(r) => r,
        Err(e) => return error_response(dialect, &e),
    };

    let registry = state.registry();
    let plan = match registry
        .resolve(&req.model)
        .and_then(|resolution| apply_budgets(&state, &registry, resolution))
        .and_then(|resolution| state.router.plan(&registry, &resolution))
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

/// Apply the spending limits to a resolved request.
///
/// The check is "have I already spent it", not "will this request spend it", because
/// the cost is only known once a request has finished. The one that crosses the line
/// completes and the next is stopped, which bounds the overshoot at one request.
///
/// A degrade is followed at most once per class: the cheaper class's own budget still
/// applies, so this cannot be used to route around a limit, and a cycle of degrades
/// ends in a refusal rather than a loop.
fn apply_budgets(
    state: &AppState,
    registry: &Registry,
    resolution: Resolution,
) -> Result<Resolution> {
    let mut resolution = resolution;
    let mut visited: Vec<ModelClass> = Vec::new();

    loop {
        let class = match &resolution {
            Resolution::Class(class) => Some(*class),
            Resolution::Direct(_) => None,
        };
        // Every provider the request could land on, because a provider's budget has
        // to stop a request that might reach it.
        let provider_ids: Vec<String> = match &resolution {
            Resolution::Class(class) => registry
                .class_members(*class)
                .iter()
                .map(|m| m.provider_id.clone())
                .collect(),
            Resolution::Direct(id) => registry
                .entry(id)
                .map(|m| vec![m.provider_id.clone()])
                .unwrap_or_default(),
        };

        match state.budget_verdict(&provider_ids, class) {
            Verdict::Allow => return Ok(resolution),
            Verdict::Reject { because } => return Err(Error::OverBudget(because)),
            Verdict::Degrade { to, because } => {
                if let Some(current) = class {
                    visited.push(current);
                }
                if visited.contains(&to) {
                    // Following this would come back here, so refusing is the end.
                    return Err(Error::OverBudget(format!(
                        "{because}, and the class it degrades to is over its own limit"
                    )));
                }
                tracing::info!("degrading to {}: {because}", to.virtual_id());
                resolution = Resolution::Class(to);
            }
        }
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
                rec.usage(resp.usage)
                    .priced_with(candidate.entry.pricing.as_ref());
                let cost = candidate
                    .entry
                    .pricing
                    .as_ref()
                    .map(|p| p.cost_of(&resp.usage));
                if let Some(cost) = &cost {
                    // Booked against the model that answered, so a failover spends
                    // from the provider it actually reached.
                    state.charge(&candidate.provider.id, candidate.entry.class, cost);
                }
                state
                    .stats
                    .record(rec.finish(started.elapsed().as_millis() as u64));
                // Report the model that actually answered, not the virtual id.
                resp.model = candidate.exposed_id.clone();
                let mut response = Json(protocol::encode_response(dialect, &resp)).into_response();
                inject_routing_headers(response.headers_mut(), candidate);
                inject_cost_header(response.headers_mut(), cost.as_ref());
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
                // Health is reported once, here: the handshake is the part a
                // routing decision can act on (responsiveness and reachability),
                // while total stream duration mostly measures how long the answer
                // was. Reporting again when the stream ends would double-count
                // every streaming request in the EWMA.
                state.router.report_success(
                    candidate.model_id(),
                    attempt_start.elapsed().as_millis() as u64,
                );
                let encoder =
                    protocol::stream_encoder(dialect, &candidate.exposed_id, include_usage);
                let body = Body::from_stream(sse_body(
                    Arc::clone(&state),
                    events,
                    encoder,
                    StreamContext {
                        record: rec,
                        started,
                        model_id: candidate.model_id().to_string(),
                        routing: routing.clone(),
                        pricing: candidate.entry.pricing.clone(),
                        provider_id: candidate.provider.id.clone(),
                        class: candidate.entry.class,
                    },
                ));
                // Built from a plain body and static headers, so nothing here can
                // fail and there is no reason to unwrap.
                let mut response = Response::new(body);
                let headers = response.headers_mut();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
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

/// Report the estimated spend of a buffered answer.
///
/// Streaming answers cannot carry this: the headers are long gone by the time the
/// usage arrives, so a stream's cost shows up in the Activity tab instead.
fn inject_cost_header(headers: &mut HeaderMap, cost: Option<&Cost>) {
    if let Some(cost) = cost {
        if let Ok(value) = HeaderValue::from_str(&format!("{} {:.6}", cost.currency, cost.amount)) {
            headers.insert("x-zroutery-cost", value);
        }
    }
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
    pricing: Option<Pricing>,
    /// Who to bill, once the stream reports what it used.
    provider_id: String,
    class: Option<ModelClass>,
    finished: bool,
}

impl SseState {
    /// Record the request exactly once, when the stream ends for any reason.
    fn finalize(&mut self, error: Option<&Error>) {
        if let Some(mut rec) = self.rec.take() {
            rec.usage(self.usage).priced_with(self.pricing.as_ref());
            // A stream only reports its usage at the end, so this is the first
            // moment its cost can be charged.
            if let Some(cost) = self.pricing.as_ref().map(|p| p.cost_of(&self.usage)) {
                self.state.charge(&self.provider_id, self.class, &cost);
            }
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

/// What the SSE pipeline needs to know about the request it is serving.
struct StreamContext {
    record: RecordBuilder,
    started: Instant,
    model_id: String,
    routing: crate::config::RoutingConfig,
    pricing: Option<Pricing>,
    provider_id: String,
    class: Option<ModelClass>,
}

/// Pipe canonical events through the egress encoder into an SSE byte stream.
fn sse_body(
    app: Arc<AppState>,
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    context: StreamContext,
) -> impl futures_util::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
    use futures_util::StreamExt;

    let state = SseState {
        events,
        encoder,
        pending: VecDeque::new(),
        rec: Some(context.record),
        state: app,
        started: context.started,
        model_id: context.model_id,
        routing: context.routing,
        usage: Usage::default(),
        pricing: context.pricing,
        provider_id: context.provider_id,
        class: context.class,
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
                    st.finalize(None);
                }
            }
        }
    })
}
