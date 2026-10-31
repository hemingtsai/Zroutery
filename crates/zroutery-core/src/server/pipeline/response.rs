//! Turning a settled request into the response the client gets, headers included.

use super::lifecycle::RequestLifecycle;
use super::sse::{sse_body, StreamContext};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::Response;
use serde_json::Value;
use tokio::sync::watch;

use super::{AdmissionGuard, AppState};
use crate::billing::Cost;
use crate::ir::{Dialect, Usage};
use crate::protocol::{self};
use crate::query::RequestKind;
use crate::router::Candidate;

/// Claim a Responses API id for an in-flight stream, if this dialect has one.
pub(super) fn register_in_flight(
    state: &Arc<AppState>,
    dialect: Dialect,
    storage: protocol::responses::StoragePolicy,
) -> (Option<String>, Option<watch::Receiver<bool>>) {
    if dialect == Dialect::OpenAIResponses {
        let id = format!("resp-{}", uuid::Uuid::new_v4().simple());
        let rx = state
            .response_store
            .register_in_flight(id.clone(), storage.retains());
        (Some(id), Some(rx))
    } else {
        (None, None)
    }
}

/// Hand an established upstream stream to the client as an SSE response.
///
/// The request's lifecycle moves into the body stream: from here the stream
/// owns the terminal transition, because a stream can end in four different
/// ways and only the stream knows which one happened.
#[allow(clippy::too_many_arguments)]
pub(super) fn stream_response(
    state: &Arc<AppState>,
    candidate: &Candidate,
    kind: RequestKind,
    dialect: Dialect,
    include_usage: bool,
    mut lifecycle: RequestLifecycle,
    events: crate::upstream::EventStream,
    storage: protocol::responses::StoragePolicy,
    response_id: Option<String>,
    cancel_rx: Option<watch::Receiver<bool>>,
    // The Responses input items, stored verbatim so a completed stream can be
    // retrieved and continued exactly like a buffered answer.
    input_items: Vec<Value>,
    previous_response_id: Option<String>,
    // Held by the stream state, not by this function: a stream outlives the
    // handler that built it, and the permit has to last exactly as long as the
    // request does. `SseState`'s terminal transition charges the ledger before
    // the field is dropped, and `Drop` runs for a client that disconnects too.
    admission: AdmissionGuard,
    classifier_severity: bool,
    reply_headers: &crate::ir::ReplyHeaders,
) -> Response {
    let mut encoder = protocol::stream_encoder(dialect, &candidate.exposed_id, include_usage);
    if let Some(ref id) = response_id {
        lifecycle.note_response_id(id);
        // The response was registered under this id before the handshake, so
        // this is the identity every frame has to publish — including the
        // failure frame, which can be the client's first and only sight of it.
        encoder.set_response_id(id);
    }
    let body = Body::from_stream(sse_body(
        Arc::clone(state),
        events,
        encoder,
        StreamContext {
            lifecycle,
            usage: Usage::default(),
            pricing: candidate.entry.pricing.clone(),
            provider_id: candidate.provider.id.clone(),
            tier: candidate.entry.tier,
            kind,
            classifier_severity,
            storage,
            response_id,
            cancel_rx,
            input_items,
            previous_response_id,
            admission,
        },
    ));
    // Built from a plain body and static headers, so nothing here can fail and
    // there is no reason to unwrap.
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    inject_routing_headers(response.headers_mut(), candidate, kind);
    reply_headers.apply(response.headers_mut());
    response
}

/// Report the estimated spend of a buffered answer.
///
/// Streaming answers cannot carry this: the headers are long gone by the time the
/// usage arrives, so a stream's cost shows up in the Activity tab instead.
pub(super) fn inject_cost_header(headers: &mut HeaderMap, cost: Option<&Cost>) {
    if let Some(cost) = cost {
        if let Ok(value) = HeaderValue::from_str(&format!("{} {:.6}", cost.currency, cost.amount)) {
            headers.insert("x-zroutery-cost", value);
        }
    }
}

pub(super) fn inject_routing_headers(
    headers: &mut HeaderMap,
    candidate: &Candidate,
    kind: RequestKind,
) {
    if let Ok(v) = HeaderValue::from_str(&candidate.exposed_id) {
        headers.insert("x-zroutery-model", v);
    }
    if let Ok(v) = HeaderValue::from_str(&candidate.provider.name) {
        headers.insert("x-zroutery-provider", v);
    }
    // The tier the answer actually came from, as a tier name rather than a
    // virtual id: naming style changes what `standard-class` is called, but the
    // client can compare this against what it asked for either way. A request
    // that was escalated up reports the tier it landed in, which is the fact a
    // caller billing against tiers cannot otherwise see.
    if let Some(tier) = candidate.entry.tier {
        if let Ok(v) = HeaderValue::from_str(tier.as_str()) {
            headers.insert("x-zroutery-tier", v);
        }
    }
    if candidate.degraded {
        headers.insert("x-zroutery-degraded", HeaderValue::from_static("1"));
    }
    if !kind.is_main() {
        headers.insert("x-zroutery-classifier", HeaderValue::from_static("1"));
    }
}
