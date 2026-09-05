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
use crate::query::RequestKind;
use crate::rectifier::{self, Rectifier};
use crate::rectifier::media_fallback::MediaFallbackRectifier;
use crate::registry::{Registry, Resolution};
use crate::router::Candidate;
use crate::stats::RecordBuilder;
pub(super) async fn handle_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    headers: axum::http::HeaderMap,
    body: Value,
) -> Response {
    let include_usage = dialect == Dialect::OpenAI && openai::wants_stream_usage(&body);

    let req = match protocol::decode_request(dialect, body.clone()) {
        Ok(r) => r,
        Err(e) => return error_response(dialect, &e),
    };

    let registry = state.registry();
    let config = registry.config();

    // Routing intent: is this the client's main conversation, or a side query
    // (today: Claude Code's Auto Mode classifier) issued alongside it? The
    // classifier arrives with the same model string as main traffic, so the
    // answer comes from the request's shape, never from the model name — and
    // when classifier routing is off, everything is a main request.
    let kind = if config.classifier.enabled {
        let detection = crate::classifier::detect(&headers, &body, &config.classifier.detection);
        if !detection.kind.is_main() {
            tracing::info!(
                requested_model = %req.model,
                signature = detection.matched.as_deref().unwrap_or_default(),
                confidence = %format!("{:.2}", detection.confidence),
                "classifier request detected"
            );
        }
        detection.kind
    } else {
        RequestKind::Main
    };

    let plan = match kind {
        RequestKind::Main => registry
            .resolve(&req.model)
            .and_then(|resolution| apply_budgets(&state, &registry, resolution))
            .and_then(|resolution| state.router.plan(&registry, &resolution)),
        // The classifier pool is resolved directly: the model the client named
        // (e.g. `claude-opus-4-8[1m]`) is irrelevant to *which model judges*,
        // and may not even exist in the registry. Budgets are main-path
        // policy; classifier requests are billed under the model that answers
        // but are never degraded or rejected by a class budget.
        RequestKind::Side(_) => {
            let classifier = config.classifier.clone();
            state.router.plan_classifier(&registry, &classifier)
        }
    };
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            let mut rec = RecordBuilder::new(dialect, &req.model, req.stream);
            rec.kind(kind);
            rec.fail(e.status().as_u16(), e.to_string());
            state.stats.record(rec.finish(0));
            return error_response(dialect, &e);
        }
    };

    if req.stream {
        stream_chat(state, dialect, req, plan, kind, include_usage).await
    } else {
        buffered_chat(state, dialect, req, plan, kind).await
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
        // The class the request will be billed under. Direct ids participate
        // too: charging uses the target model's class, so a class budget must
        // also gate requests that reach the class by exact id, or the limit
        // could be spent around while still being charged.
        let class = match &resolution {
            Resolution::Class(class) => Some(*class),
            Resolution::Direct(id) => registry.entry(id).ok().and_then(|m| m.class),
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
///
/// `mode` is derived from the request kind: classifier queries are encoded in
/// fidelity mode so the verdict protocol survives providers whose quirks would
/// drop the stop sequence or the frozen temperature.
fn prepare(
    state: &AppState,
    candidate: &Candidate,
    req: &ChatRequest,
    mode: crate::upstream::EncodeMode,
) -> Result<(Option<String>, Value)> {
    let key = state.api_key(&candidate.provider)?;
    let body = crate::upstream::encode_for_mode(
        &candidate.provider,
        req,
        &candidate.entry.upstream_model,
        candidate.entry.max_output_tokens,
        mode,
    )?;
    Ok((key, body))
}

/// The encoding fidelity a request kind needs.
fn encode_mode_for(kind: RequestKind) -> crate::upstream::EncodeMode {
    match kind {
        RequestKind::Main => crate::upstream::EncodeMode::Normal,
        RequestKind::Side(_) => crate::upstream::EncodeMode::Classifier,
    }
}

/// Describe the images in a request so a model that cannot see still
/// receives their content.
///
/// Called from two places with one body: the preflight (the target model is
/// known not to support vision, so describe before sending) and the
/// media-fallback retry (the upstream rejected the image, so describe and
/// retry the same candidate). Each image is described independently — one
/// blind image must not sink the others.
///
/// Images that cannot be described get the placeholder, never a silent drop:
/// a text model that receives nothing where an image was has no way to know
/// it is missing something.
///
/// The entire operation is bounded by a 30-second timeout to prevent
/// unbounded latency when the vision model is slow. On timeout, any
/// remaining images are replaced with the placeholder.
async fn apply_vision_fallback(
    state: &AppState,
    req: &mut ChatRequest,
    reason: &str,
) {
    let timeout = std::time::Duration::from_secs(30);
    if tokio::time::timeout(timeout, apply_vision_fallback_inner(state, req, reason))
        .await
        .is_err()
    {
        // On timeout, apply placeholders to any images the inner loop
        // did not reach yet so the request can still proceed.
        let config = state.config();
        let remaining = crate::media::collect::collect(req);
        for (slot, _) in &remaining {
            crate::media::transform::replace(
                req,
                slot,
                &crate::media::transform::Replacement::Placeholder(
                    config.vision.placeholder.clone(),
                ),
            );
        }
        tracing::warn!(
            reason,
            images = remaining.len(),
            "vision fallback timed out after {}s; remaining images replaced with placeholder",
            timeout.as_secs()
        );
    }
}

/// Inner implementation of the vision fallback, extracted so it can be
/// wrapped in an aggregate timeout.
async fn apply_vision_fallback_inner(
    state: &AppState,
    req: &mut ChatRequest,
    reason: &str,
) {
    let config = state.config();
    let images = crate::media::collect::collect(req);
    if images.is_empty() {
        return;
    }
    let target = crate::media::vision::resolve(&config);
    if target.is_none() {
        // No vision model configured: every image becomes the honest
        // placeholder rather than an error, so the request can still go.
        for (slot, _) in &images {
            crate::media::transform::replace(
                req,
                slot,
                &crate::media::transform::Replacement::Placeholder(
                    config.vision.placeholder.clone(),
                ),
            );
        }
        tracing::warn!(
            images = images.len(),
            reason,
            "no vision model configured; images replaced with the placeholder"
        );
        return;
    }
    let target = target.unwrap();
    let key = state
        .api_key(&target.provider)
        .ok()
        .flatten();

    let mut described = 0;
    let mut placeholders = 0;
    for (slot, source) in &images {
        let replacement = match crate::media::vision::describe(
            &state.upstream,
            &target,
            key.as_deref(),
            source,
        )
        .await
        {
            Ok(resp) => {
                described += 1;
                crate::media::transform::Replacement::Description(
                    crate::media::vision::description_text(&resp),
                )
            }
            Err(e) => {
                placeholders += 1;
                tracing::warn!(
                    vision_model = target.entry.exposed_id(),
                    error = %e,
                    "vision description failed; using the placeholder for this image"
                );
                crate::media::transform::Replacement::Placeholder(
                    config.vision.placeholder.clone(),
                )
            }
        };
        crate::media::transform::replace(req, slot, &replacement);
    }
    tracing::info!(
        reason,
        vision_model = target.entry.exposed_id(),
        described,
        placeholders,
        "vision fallback applied"
    );
}

/// Try every enabled rectifier, allowing each rectifier up to three repair
/// rounds. A rectifier retry is deliberately not reported to the circuit
/// breaker: it is the same provider and same model, and the failure was a
/// fixable request-shape problem.
///
/// A media rejection is upgraded before the ordinary cascade runs: instead of
/// a placeholder, the images get described by the vision model (falling back
/// to the placeholder when it fails), and the repaired request retries the
/// same provider — the upstream rejected the *image*, not the question.
async fn try_rectify_buffered(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    req: &mut ChatRequest,
    error: &Error,
) -> std::result::Result<Option<crate::ir::ChatResponse>, Error> {
    // The reactive vision path: only when the upstream said "no images" and
    // the request still carries them.
    if state.config().vision.enabled
        && MediaFallbackRectifier.should_apply(error, &serde_json::Value::Null)
        && !crate::media::collect::collect(req).is_empty()
    {
        apply_vision_fallback(state, req, "upstream rejected media").await;
        let key_owned = key.map(str::to_string);
        if let Some(body) = reencode(candidate, req) {
            match state
                .upstream
                .send(&candidate.provider, key_owned.as_deref(), &body)
                .await
            {
                Ok(resp) => return Ok(Some(resp)),
                Err(e) => tracing::warn!(
                    model = candidate.model_id(),
                    "vision-repaired retry also failed: {e}"
                ),
            }
        }
    }

    let rectifiers = rectifier::from_config(&state.config().routing.rectifier);
    if rectifiers.is_empty() {
        return Ok(None);
    }

    // Rectifiers operate on the encoded body; re-encode the (possibly
    // vision-repaired) request for them.
    let base_body = match reencode(candidate, req) {
        Some(body) => body,
        None => return Ok(None),
    };
    let mut current_body = base_body.clone();
    let mut last_error: Option<Error> = None;
    let mut current_error: &Error = error;

    for rectifier in rectifiers {
        let mut rounds = 0;
        loop {
            if !rectifier.should_apply(current_error, &current_body) {
                break;
            }
            let mut modified = current_body.clone();
            let result = rectifier.rectify(&mut modified);
            if !result.applied {
                break;
            }
            tracing::info!(
                model = candidate.model_id(),
                provider = candidate.provider.name.as_str(),
                rectifier = rectifier.name(),
                "request repaired, retrying same provider without health accounting"
            );
            match state
                .upstream
                .send(&candidate.provider, key, &modified)
                .await
            {
                Ok(resp) => return Ok(Some(resp)),
                Err(e) => {
                    tracing::warn!(
                        model = candidate.model_id(),
                        rectifier = rectifier.name(),
                        "rectifier retry also failed: {e}"
                    );
                    last_error = Some(e);
                    current_body = modified;
                    current_error = last_error.as_ref().expect("just set");
                    rounds += 1;
                    if rounds >= 3 {
                        break;
                    }
                }
            }
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// Re-encode a (possibly repaired) IR request for one candidate.
fn reencode(candidate: &Candidate, req: &ChatRequest) -> Option<Value> {
    crate::upstream::encode_for_mode(
        &candidate.provider,
        req,
        &candidate.entry.upstream_model,
        candidate.entry.max_output_tokens,
        encode_mode_for(crate::query::RequestKind::Main),
    )
    .ok()
}

async fn try_rectify_stream(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    req: &mut ChatRequest,
    error: &Error,
) -> std::result::Result<Option<crate::upstream::EventStream>, Error> {
    // The reactive vision path: only when the upstream said "no images" and
    // the request still carries them.
    if state.config().vision.enabled
        && MediaFallbackRectifier.should_apply(error, &serde_json::Value::Null)
        && !crate::media::collect::collect(req).is_empty()
    {
        apply_vision_fallback(state, req, "upstream rejected media").await;
        let key_owned = key.map(str::to_string);
        if let Some(body) = reencode(candidate, req) {
            match state
                .upstream
                .stream(
                    &candidate.provider,
                    key_owned.as_deref(),
                    &body,
                    &candidate.entry.upstream_model,
                )
                .await
            {
                Ok(events) => return Ok(Some(events)),
                Err(e) => tracing::warn!(
                    model = candidate.model_id(),
                    "vision-repaired stream retry also failed: {e}"
                ),
            }
        }
    }

    let rectifiers = rectifier::from_config(&state.config().routing.rectifier);
    if rectifiers.is_empty() {
        return Ok(None);
    }

    // Rectifiers operate on the encoded body; re-encode the (possibly
    // vision-repaired) request for them.
    let base_body = match reencode(candidate, req) {
        Some(body) => body,
        None => return Ok(None),
    };
    let mut current_body = base_body.clone();
    let mut last_error: Option<Error> = None;
    let mut current_error: &Error = error;

    for rectifier in rectifiers {
        let mut rounds = 0;
        loop {
            if !rectifier.should_apply(current_error, &current_body) {
                break;
            }
            let mut modified = current_body.clone();
            let result = rectifier.rectify(&mut modified);
            if !result.applied {
                break;
            }
            tracing::info!(
                model = candidate.model_id(),
                provider = candidate.provider.name.as_str(),
                rectifier = rectifier.name(),
                "stream request repaired, retrying same provider without health accounting"
            );
            match state
                .upstream
                .stream(
                    &candidate.provider,
                    key,
                    &modified,
                    &candidate.entry.upstream_model,
                )
                .await
            {
                Ok(events) => return Ok(Some(events)),
                Err(e) => {
                    tracing::warn!(
                        model = candidate.model_id(),
                        rectifier = rectifier.name(),
                        "rectifier stream retry also failed: {e}"
                    );
                    last_error = Some(e);
                    current_body = modified;
                    current_error = last_error.as_ref().expect("just set");
                    rounds += 1;
                    if rounds >= 3 {
                        break;
                    }
                }
            }
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

async fn buffered_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    req: ChatRequest,
    plan: Vec<Candidate>,
    kind: RequestKind,
) -> Response {
    let started = Instant::now();
    let mut rec = RecordBuilder::new(dialect, &req.model, false);
    rec.kind(kind);
    let routing = state.config().routing.clone();
    let mode = encode_mode_for(kind);
    let mut last_error = Error::NoCandidate(req.model.clone());

    for candidate in &plan {
        rec.attempt();
        rec.resolved(candidate.model_id(), &candidate.provider.name);
        if !kind.is_main() {
            // The three names the debugging session needs: what the client
            // asked for, which pool member was chosen, and what the provider
            // actually receives.
            tracing::debug!(
                kind = kind.as_str(),
                requested_model = %req.model,
                candidate_model = candidate.model_id(),
                upstream_model = %candidate.entry.upstream_model,
                "classifier attempt"
            );
        }
        let attempt_start = Instant::now();

        // Preflight: a target known not to see gets the images described
        // before the first byte leaves, so the attempt is not wasted on a
        // rejection we could predict. Unknown capability is deliberately
        // left alone — the upstream decides, and the rectifier reacts.
        // And with vision fallback off entirely, nothing happens here: off
        // means the request goes out as it came, promise kept.
        let mut req = req.clone();
        if state.config().vision.enabled && !candidate.entry.supports_vision {
            let needs_vision = crate::media::collect::collect(&req);
            if !needs_vision.is_empty() {
                apply_vision_fallback(&state, &mut req, "preflight").await;
            }
        }

        let (key, body) = match prepare(&state, candidate, &req, mode) {
            Ok(v) => v,
            Err(e) => {
                state
                    .router
                    .report_failure(candidate.model_id(), &e, &routing);
                last_error = e;
                continue;
            }
        };

        // Enforce the half-open single-probe rule at the point of send.
        if !state.router.allow_request(candidate.model_id()) {
            tracing::debug!(
                model = candidate.model_id(),
                "half-open probe already in flight; skipping candidate"
            );
            continue;
        }

        match state
            .upstream
            .send(&candidate.provider, key.as_deref(), &body)
            .await
        {
            Ok(mut resp) => {
                // A classifier answer is only a success when it carries a
                // verdict. HTTP 200 without `<block>…</block>` is not an
                // approval — the model failed to do its job, so the attempt
                // is reported, the failure is recorded and the next candidate
                // is tried. There is no "looks safe" fallback.
                if !kind.is_main() {
                    match crate::classifier::parse_verdict(&resp.text()) {
                        crate::classifier::ClassifierVerdict::Allow
                        | crate::classifier::ClassifierVerdict::Block => {
                            tracing::debug!(
                                kind = kind.as_str(),
                                candidate_model = candidate.model_id(),
                                verdict = "parsed",
                                "classifier response validated"
                            );
                        }
                        crate::classifier::ClassifierVerdict::Unparseable => {
                            let e = Error::BadUpstreamPayload(format!(
                                "classifier response from `{}` carried no <block> verdict",
                                candidate.model_id()
                            ));
                            state
                                .router
                                .report_failure(candidate.model_id(), &e, &routing);
                            tracing::warn!(
                                kind = kind.as_str(),
                                candidate_model = candidate.model_id(),
                                upstream_model = %candidate.entry.upstream_model,
                                "classifier response had no <block> verdict; failing over"
                            );
                            last_error = e;
                            continue;
                        }
                    }
                }
                state.router.report_success(
                    candidate.model_id(),
                    attempt_start.elapsed().as_millis() as u64,
                    &routing,
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
                inject_routing_headers(response.headers_mut(), candidate, kind);
                inject_cost_header(response.headers_mut(), cost.as_ref());
                return response;
            }
            Err(e) => {
                // Rectifier cascade: try to repair the request and retry the
                // same provider without touching circuit-breaker health.
                match try_rectify_buffered(&state, candidate, key.as_deref(), &mut req, &e).await {
                    Ok(Some(mut resp)) => {
                        // The repaired retry is not a fresh probe: give the
                        // half-open permit back without recording health.
                        state.router.release_half_open_permit(candidate.model_id());
                        // A repaired classifier answer still has to carry a
                        // verdict; the same fail-closed rule as the direct
                        // path applies, minus the health report (this was a
                        // repaired retry, which never records health).
                        if !kind.is_main()
                            && crate::classifier::parse_verdict(&resp.text())
                                == crate::classifier::ClassifierVerdict::Unparseable
                        {
                            tracing::warn!(
                                kind = kind.as_str(),
                                candidate_model = candidate.model_id(),
                                "rectified classifier response still had no <block> verdict; failing over"
                            );
                            last_error = Error::BadUpstreamPayload(format!(
                                "classifier response from `{}` carried no <block> verdict",
                                candidate.model_id()
                            ));
                            continue;
                        }
                        rec.usage(resp.usage)
                            .priced_with(candidate.entry.pricing.as_ref());
                        let cost = candidate
                            .entry
                            .pricing
                            .as_ref()
                            .map(|p| p.cost_of(&resp.usage));
                        if let Some(cost) = &cost {
                            state.charge(&candidate.provider.id, candidate.entry.class, cost);
                        }
                        state
                            .stats
                            .record(rec.finish(started.elapsed().as_millis() as u64));
                        resp.model = candidate.exposed_id.clone();
                        let mut response =
                            Json(protocol::encode_response(dialect, &resp)).into_response();
                        inject_routing_headers(response.headers_mut(), candidate, kind);
                        inject_cost_header(response.headers_mut(), cost.as_ref());
                        return response;
                    }
                    Ok(None) => {
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
                    Err(rectified_err) => {
                        state
                            .router
                            .report_failure(candidate.model_id(), &rectified_err, &routing);
                        tracing::warn!(
                            model = candidate.model_id(),
                            provider = candidate.provider.name.as_str(),
                            "rectifier retry failed: {rectified_err}"
                        );
                        let retryable = rectified_err.is_retryable();
                        last_error = rectified_err;
                        if !retryable {
                            break;
                        }
                    }
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
    kind: RequestKind,
    include_usage: bool,
) -> Response {
    let started = Instant::now();
    let mut rec = RecordBuilder::new(dialect, &req.model, true);
    rec.kind(kind);
    let routing = state.config().routing.clone();
    let mode = encode_mode_for(kind);
    let mut last_error = Error::NoCandidate(req.model.clone());

    for candidate in &plan {
        rec.attempt();
        rec.resolved(candidate.model_id(), &candidate.provider.name);
        if !kind.is_main() {
            tracing::debug!(
                kind = kind.as_str(),
                requested_model = %req.model,
                candidate_model = candidate.model_id(),
                upstream_model = %candidate.entry.upstream_model,
                "classifier stream attempt"
            );
        }
        let attempt_start = Instant::now();

        // Preflight: a target known not to see gets the images described
        // before the first byte leaves, so the attempt is not wasted on a
        // rejection we could predict. Unknown capability is deliberately
        // left alone — the upstream decides, and the rectifier reacts.
        // And with vision fallback off entirely, nothing happens here: off
        // means the request goes out as it came, promise kept.
        let mut req = req.clone();
        if state.config().vision.enabled && !candidate.entry.supports_vision {
            let needs_vision = crate::media::collect::collect(&req);
            if !needs_vision.is_empty() {
                apply_vision_fallback(&state, &mut req, "preflight").await;
            }
        }

        let (key, body) = match prepare(&state, candidate, &req, mode) {
            Ok(v) => v,
            Err(e) => {
                state
                    .router
                    .report_failure(candidate.model_id(), &e, &routing);
                last_error = e;
                continue;
            }
        };

        // Enforce the half-open single-probe rule at the point of send.
        if !state.router.allow_request(candidate.model_id()) {
            tracing::debug!(
                model = candidate.model_id(),
                "half-open probe already in flight; skipping candidate"
            );
            continue;
        }

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
                    &routing,
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
                        kind,
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
                inject_routing_headers(response.headers_mut(), candidate, kind);
                return response;
            }
            Err(e) => {
                // Rectifier cascade for handshake failures. A successful repaired
                // stream is served to the client without reporting health.
                match try_rectify_stream(&state, candidate, key.as_deref(), &mut req, &e).await {
                    Ok(Some(events)) => {
                        // The repaired stream is not a fresh half-open probe.
                        state.router.release_half_open_permit(candidate.model_id());
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
                                kind,
                            },
                        ));
                        let mut response = Response::new(body);
                        let headers = response.headers_mut();
                        headers.insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/event-stream"),
                        );
                        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
                        inject_routing_headers(response.headers_mut(), candidate, kind);
                        return response;
                    }
                    Ok(None) => {
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
                    Err(rectified_err) => {
                        state
                            .router
                            .report_failure(candidate.model_id(), &rectified_err, &routing);
                        tracing::warn!(
                            model = candidate.model_id(),
                            "rectifier stream retry failed: {rectified_err}"
                        );
                        let retryable = rectified_err.is_retryable();
                        last_error = rectified_err;
                        if !retryable {
                            break;
                        }
                    }
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

fn inject_routing_headers(headers: &mut HeaderMap, candidate: &Candidate, kind: RequestKind) {
    if let Ok(v) = HeaderValue::from_str(&candidate.exposed_id) {
        headers.insert("x-zroutery-model", v);
    }
    if let Ok(v) = HeaderValue::from_str(&candidate.provider.name) {
        headers.insert("x-zroutery-provider", v);
    }
    if candidate.degraded {
        headers.insert("x-zroutery-degraded", HeaderValue::from_static("1"));
    }
    if !kind.is_main() {
        headers.insert("x-zroutery-classifier", HeaderValue::from_static("1"));
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
    /// Set for classifier streams, which accumulate their text so the verdict
    /// can be checked once the stream ends. Main streams never pay for this.
    classifier_text: Option<String>,
    finished: bool,
}

impl SseState {
    /// Record the request exactly once, when the stream ends for any reason.
    fn finalize(&mut self, error: Option<&Error>) {
        // A classifier stream that reached its end without producing a verdict
        // is worth a warning. The bytes have already gone out, so there is
        // nothing to fail over to — but the client will fail closed on its own
        // parse, and this line is how that shows up in the proxy's log.
        if error.is_none() {
            if let Some(text) = self.classifier_text.take() {
                if crate::classifier::parse_verdict(&text)
                    == crate::classifier::ClassifierVerdict::Unparseable
                {
                    tracing::warn!(
                        model = %self.model_id,
                        "streamed classifier response carried no <block> verdict"
                    );
                }
            }
        }
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

impl Drop for SseState {
    fn drop(&mut self) {
        // A client disconnect drops the body stream without the unfold loop
        // reaching `None`/`Some(Err)`. Record the request anyway so Activity,
        // stats and the budget ledger see whatever was consumed so far.
        // `finalize` is idempotent (`rec.take()`), so streams that ended
        // normally are unaffected.
        self.finalize(None);
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
    kind: RequestKind,
}

/// Pipe canonical events through the egress encoder into an SSE byte stream.
fn sse_body(
    app: Arc<AppState>,
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    context: StreamContext,
) -> impl futures_util::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
    use futures_util::StreamExt;

    let classifier_text = (!context.kind.is_main()).then(String::new);
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
        classifier_text,
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
                        StreamEvent::ThinkingDelta { .. } => {
                            if let Some(rec) = st.rec.as_mut() {
                                rec.ttft(st.started.elapsed().as_millis() as u64);
                            }
                        }
                        StreamEvent::TextDelta { text, .. } => {
                            if let Some(rec) = st.rec.as_mut() {
                                rec.ttft(st.started.elapsed().as_millis() as u64);
                            }
                            // Classifier streams keep their text so the verdict
                            // can be checked once the stream ends.
                            if let Some(acc) = st.classifier_text.as_mut() {
                                acc.push_str(text);
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
