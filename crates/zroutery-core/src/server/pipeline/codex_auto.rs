//! Responses staging and retry orchestration; all request-local state lives here.
//! Policy ported from Magpie Auto V10. See rectifier::codex_auto and its license.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use super::response::{inject_cost_header, inject_routing_headers, register_in_flight};
use super::session_binding;
use super::{
    client_for, console_key, prepare, shape_for_candidate, AdmissionGuard, AppState,
    RequestLifecycle, Settlement, TerminalKind,
};
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, Dialect, StoredResponse, Usage};
use crate::policy::RouteDecision;
use crate::protocol::{self, SseFrame, StreamEncoder};
use crate::query::RequestKind;
use crate::rectifier::{self, codex_auto as policy};
use crate::router::Candidate;
use crate::upstream::{EncodeMode, StagedResponse};

struct RequestGuard {
    lifecycle: RequestLifecycle,
    app: Arc<AppState>,
    response_id: Option<String>,
    _admission: AdmissionGuard,
    pending_settlement: Settlement,
    filter_outcome: Option<&'static str>,
    filter_active: bool,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.app
            .router
            .release_half_open_permit(&self.lifecycle.served_model());
        if let Some(id) = &self.response_id {
            self.app.response_store.complete_in_flight(id);
        }
        if !self.lifecycle.is_terminal() {
            let _ = self.lifecycle.finalize(
                TerminalKind::ClientDisconnected { emitted: false },
                std::mem::take(&mut self.pending_settlement),
            );
        }
        if self.filter_active {
            self.app.codex_filter.update(|stats| {
                stats.active_requests = stats.active_requests.saturating_sub(1);
                match self.filter_outcome {
                    Some("success") => stats.success += 1,
                    Some("failed") => stats.failed += 1,
                    _ => stats.cancelled += 1,
                }
            });
        }
    }
}

struct Completed {
    wire: Value,
    frames: Vec<SseFrame>,
    usage: Usage,
    candidate: Candidate,
    started: Instant,
    declared_model: Option<String>,
}

struct Delivery {
    frame: SseFrame,
    /// Ownership moves with the final frame. Its poll commits the terminal
    /// transaction before acknowledging completion; dropping queued frames
    /// still runs the cancellation guard, not a premature served transition.
    completion: Option<Box<DeliveryCompletion>>,
}

struct DeliveryCompletion {
    guard: RequestGuard,
    completed: Completed,
    input: Vec<Value>,
    previous: Option<String>,
    storage: protocol::responses::StoragePolicy,
}

/// Forward a native compact operation once, using the same routing and budget
/// admission as chat, but without history cleaning or retries.
pub(in crate::server) async fn compact(
    app: Arc<AppState>,
    headers: axum::http::HeaderMap,
    body: Value,
) -> Response {
    match compact_inner(&app, headers, body).await {
        Ok(body) => Json(body).into_response(),
        Err(error) => super::error_response(Dialect::OpenAIResponses, &error),
    }
}

async fn compact_inner(
    app: &Arc<AppState>,
    headers: axum::http::HeaderMap,
    mut body: Value,
) -> Result<Value> {
    app.accounting_ready()?;
    let mut req = protocol::responses::decode_request(body.clone())?;
    req.client_identity = super::client_identity(&headers);
    req.client_headers = headers;
    let registry = super::effort::registry_for(&req, &app.registry());
    let resolution = registry.resolve(&req.model)?;
    let (resolution, admission) = super::apply_budgets(app, &registry, resolution).await?;
    let plan = app
        .router
        .plan(&registry, &resolution, &req.required_capabilities)?;
    let plan = super::effort::filter(&req, plan)?;
    let candidate = plan
        .into_iter()
        .find(|candidate| {
            candidate.provider.dialect() == Dialect::OpenAIResponses
                && app.router.credential_available(candidate)
        })
        .ok_or_else(|| {
            Error::invalid(
                "encrypted_compaction_disabled: compact requires a native Responses upstream",
            )
        })?;
    let mut identity = client_for(&req, &candidate.provider);
    let key = app.api_key(&candidate.provider)?;
    let key = console_key(app, &candidate, key, &mut identity).await?;
    body["model"] = json!(candidate.entry.upstream_model);
    app.upstream_sessions
        .resolve(&candidate, &mut body, &mut identity);
    let mut lifecycle = RequestLifecycle::new(
        Arc::clone(app),
        Dialect::OpenAIResponses,
        false,
        RequestKind::Main,
        &req.model,
        &req.client_headers,
        #[cfg(feature = "ml")]
        Some(req.clone()),
    );
    lifecycle.planned_from(None, std::slice::from_ref(&candidate));
    let mut guard = RequestGuard {
        lifecycle,
        app: Arc::clone(app),
        response_id: None,
        _admission: admission,
        pending_settlement: Settlement::default(),
        filter_outcome: None,
        filter_active: false,
    };
    guard.lifecycle.attempted(&candidate);
    guard.lifecycle.begin_attempt(&candidate, false)?;
    let started = Instant::now();
    match app
        .upstream
        .compact_codex(&candidate.provider, key.as_deref(), &body, &identity)
        .await
    {
        Ok(mut response) => {
            session_binding::annotate_response(&mut response, &req.reply_headers);
            let usage = protocol::responses::response_usage(&response);
            response["model"] = json!(candidate.exposed_id);
            guard.lifecycle.served_attempt(started);
            guard.lifecycle.finalize(
                TerminalKind::Served,
                Settlement {
                    usage,
                    pricing: candidate.entry.pricing.clone(),
                    provider_id: candidate.provider.id.clone(),
                    tier: candidate.entry.tier,
                },
            )?;
            Ok(response)
        }
        Err(error) => {
            guard.lifecycle.failed_attempt(started, &error);
            app.router.note_candidate_retry_after(&candidate, &error);
            app.router.record_candidate_failure(
                &candidate,
                &error.classified(),
                guard.lifecycle.routing(),
            );
            let _ = guard
                .lifecycle
                .finalize(TerminalKind::failed(&error), Settlement::default());
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle(
    app: Arc<AppState>,
    req: ChatRequest,
    plan: Vec<Candidate>,
    input: Vec<Value>,
    previous: Option<String>,
    storage: protocol::responses::StoragePolicy,
    decision: Option<RouteDecision>,
    admission: AdmissionGuard,
    #[cfg(feature = "ml")] shadow: Option<crate::ml::ShadowInput>,
) -> Response {
    let mut lifecycle = RequestLifecycle::new(
        Arc::clone(&app),
        Dialect::OpenAIResponses,
        req.stream,
        RequestKind::Main,
        &req.model,
        &req.client_headers,
        #[cfg(feature = "ml")]
        Some(req.clone()),
    );
    lifecycle.planned_from(decision.as_ref(), &plan);
    #[cfg(feature = "ml")]
    if let Some(shadow) = &shadow {
        lifecycle.shadow_evaluated(shadow);
    }
    let (response_id, cancel_rx) = if req.stream {
        register_in_flight(&app, Dialect::OpenAIResponses, storage)
    } else {
        (
            Some(format!("resp-{}", uuid::Uuid::new_v4().simple())),
            None,
        )
    };
    if let Some(id) = &response_id {
        lifecycle.note_response_id(id);
    }
    let mut guard = RequestGuard {
        lifecycle,
        app: Arc::clone(&app),
        response_id: response_id.clone(),
        _admission: admission,
        pending_settlement: Settlement::default(),
        filter_outcome: None,
        filter_active: true,
    };
    app.codex_filter.update(|stats| {
        stats.requests += 1;
        stats.active_requests += 1;
    });
    if !req.stream {
        let mut response = match run(
            &app,
            &req,
            &plan,
            &mut guard.lifecycle,
            response_id.as_deref(),
        )
        .await
        {
            Ok(completed) => {
                let mut response = Json(completed.wire.clone()).into_response();
                inject_routing_headers(
                    response.headers_mut(),
                    &completed.candidate,
                    RequestKind::Main,
                );
                let cost = completed
                    .candidate
                    .entry
                    .pricing
                    .as_ref()
                    .map(|p| p.cost_of(&completed.usage));
                inject_cost_header(response.headers_mut(), cost.as_ref());
                match finish(&mut guard, &completed, input, previous, storage) {
                    Ok(()) => response,
                    Err(error) => super::error_response(Dialect::OpenAIResponses, &error),
                }
            }
            Err(error) => {
                guard.filter_outcome = Some("failed");
                let _ = guard
                    .lifecycle
                    .finalize(TerminalKind::failed(&error), Settlement::default());
                super::error_response(Dialect::OpenAIResponses, &error)
            }
        };
        req.reply_headers.apply(response.headers_mut());
        return response;
    }
    let (tx, rx) = mpsc::channel::<Delivery>(8);
    let public_response_id = response_id.clone();
    tokio::spawn(async move {
        let mut cancel_rx = cancel_rx;
        let result = {
            let work = run(
                &app,
                &req,
                &plan,
                &mut guard.lifecycle,
                response_id.as_deref(),
            );
            tokio::pin!(work);
            let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
            heartbeat.tick().await; // Do not send a fake model result on entry.
            loop {
                tokio::select! {
                    result = &mut work => break Some(result),
                    _ = tx.closed() => break None,
                    _ = cancelled(&mut cancel_rx) => break None,
                    _ = heartbeat.tick() => {
                        let mut progress = SseFrame { event: Some("response.in_progress".into()), data: json!({
                            "type":"response.in_progress", "response":{"id":response_id,
                                "object":"response", "status":"in_progress", "model":req.model, "output":[]}
                        }).to_string() };
                        session_binding::annotate_frames(std::slice::from_mut(&mut progress), &req.reply_headers);
                        if matches!(tx.try_send(Delivery { frame: progress, completion: None }), Err(mpsc::error::TrySendError::Closed(_))) { break None; }
                    }
                }
            }
        };
        match result {
            Some(Ok(mut completed)) => {
                guard.pending_settlement = Settlement {
                    usage: completed.usage,
                    pricing: completed.candidate.entry.pricing.clone(),
                    provider_id: completed.candidate.provider.id.clone(),
                    tier: completed.candidate.entry.tier,
                };
                let frames = std::mem::take(&mut completed.frames);
                let last = frames.len().saturating_sub(1);
                let mut completion = Some(Box::new(DeliveryCompletion {
                    guard,
                    completed,
                    input,
                    previous,
                    storage,
                }));
                for (index, frame) in frames.into_iter().enumerate() {
                    let settle = if index == last {
                        completion.take()
                    } else {
                        None
                    };
                    tokio::select! {
                        sent = tx.send(Delivery { frame, completion: settle }) => if sent.is_err() { return; },
                        _ = cancelled(&mut cancel_rx) => return,
                    }
                }
            }
            Some(Err(error)) => {
                guard.filter_outcome = Some("failed");
                let mut encoder = protocol::responses::ResponsesStreamEncoder::new(&req.model);
                if let Some(id) = &response_id {
                    encoder.set_response_id(id);
                }
                let committed = guard
                    .lifecycle
                    .finalize(TerminalKind::failed(&error), Settlement::default());
                let mut frames = encoder.error(committed.as_ref().err().unwrap_or(&error));
                session_binding::annotate_frames(&mut frames, &req.reply_headers);
                for frame in frames {
                    tokio::select! {
                        sent = tx.send(Delivery {frame, completion: None}) => if sent.is_err() {return;},
                        _ = cancelled(&mut cancel_rx) => return,
                    }
                }
            }
            None => {
                let error = Error::BadUpstreamPayload("cancelled by the client".into());
                let mut encoder = protocol::responses::ResponsesStreamEncoder::new(&req.model);
                if let Some(id) = &response_id {
                    encoder.set_response_id(id);
                }
                let mut frames = encoder.error(&error);
                session_binding::annotate_frames(&mut frames, &req.reply_headers);
                for frame in frames {
                    let _ = tx.try_send(Delivery {
                        frame,
                        completion: None,
                    });
                }
                let _ = guard
                    .lifecycle
                    .finalize(TerminalKind::ClientCancelled, Settlement::default());
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|mut delivery| {
            if let Some(completion) = delivery.completion.take() {
                let DeliveryCompletion {
                    mut guard,
                    completed,
                    input,
                    previous,
                    storage,
                } = *completion;
                if let Err(error) = finish(&mut guard, &completed, input, previous, storage) {
                    let mut encoder = protocol::responses::ResponsesStreamEncoder::new(
                        &completed.candidate.exposed_id,
                    );
                    if let Some(id) = &guard.response_id {
                        encoder.set_response_id(id);
                    }
                    if let Some(frame) = encoder.error(&error).into_iter().last() {
                        delivery.frame = frame;
                    }
                }
            }
            (
                Ok::<_, std::io::Error>(bytes::Bytes::from(delivery.frame.to_wire())),
                rx,
            )
        })
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    if let Some(id) = public_response_id.and_then(|id| HeaderValue::from_str(&id).ok()) {
        response.headers_mut().insert("x-zroutery-response-id", id);
    }
    response
}

async fn cancelled(rx: &mut Option<watch::Receiver<bool>>) {
    match rx {
        Some(rx) => {
            if *rx.borrow() {
                return;
            }
            let _ = rx.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

fn finish(
    guard: &mut RequestGuard,
    completed: &Completed,
    input: Vec<Value>,
    previous: Option<String>,
    storage: protocol::responses::StoragePolicy,
) -> Result<()> {
    let candidate = &completed.candidate;
    guard.filter_outcome = Some(if completed.wire["status"] == "incomplete" {
        "failed"
    } else {
        "success"
    });
    guard.lifecycle.ttft_ms(guard.lifecycle.elapsed_ms());
    #[cfg(feature = "ml")]
    guard
        .lifecycle
        .note_declared_model(completed.declared_model.as_deref());
    #[cfg(not(feature = "ml"))]
    let _ = &completed.declared_model;
    guard.lifecycle.served_attempt(completed.started);
    guard.app.router.record_classified_outcome(
        candidate.model_id(),
        &candidate.provider.id,
        completed.started.elapsed().as_secs_f64() * 1000.0,
        None,
        true,
        None,
    );
    guard.lifecycle.finalize(
        TerminalKind::Served,
        Settlement {
            usage: completed.usage,
            pricing: candidate.entry.pricing.clone(),
            provider_id: candidate.provider.id.clone(),
            tier: candidate.entry.tier,
        },
    )?;
    if storage.retains() && completed.wire["status"] != "incomplete" {
        guard.app.response_store.put(StoredResponse::completed(
            completed.wire["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            candidate.exposed_id.clone(),
            input,
            completed.wire["output"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            completed.usage,
            previous,
            guard.lifecycle.decision(),
        ));
    }
    Ok(())
}

async fn run(
    app: &AppState,
    req: &ChatRequest,
    plan: &[Candidate],
    lifecycle: &mut RequestLifecycle,
    id: Option<&str>,
) -> Result<Completed> {
    let deadline = tokio::time::Instant::now() + policy::DEADLINE;
    let mut budget = policy::RetryBudget::default();
    let auto_enabled =
        lifecycle.routing().rectifier.enabled && lifecycle.routing().rectifier.codex_auto;
    let mut last = Error::NoCandidate(req.model.clone());
    let limit = if lifecycle.routing().failover {
        lifecycle.routing().max_attempts.max(1)
    } else {
        1
    } as usize;
    tracing::info!(
        request_id = lifecycle.id(),
        "staging codex response before delivery"
    );
    let mut candidates_sent = 0;
    for candidate in plan {
        if candidates_sent >= limit {
            break;
        }
        if !app.router.credential_available(candidate) {
            continue;
        }
        let shaped = shape_for_candidate(app, candidate, req).await;
        // Auto cleans history only after a targeted error, but an explicitly
        // enabled provider portability quirk must still apply before every send,
        // including tool-only staging when Auto is disabled.
        let (key, mut body) = match prepare(app, candidate, &shaped, EncodeMode::Normal) {
            Ok(prepared) => prepared,
            Err(error) => {
                last = error;
                continue;
            }
        };
        if budget.cleaned && candidate.provider.dialect() == Dialect::OpenAIResponses {
            policy::clean_history(&mut body)?;
        }
        if !app.router.allow_candidate_request(candidate) {
            continue;
        }
        candidates_sent += 1;
        let mut identity = client_for(&shaped, &candidate.provider);
        let key = match console_key(app, candidate, key, &mut identity).await {
            Ok(key) => key,
            Err(error) => {
                app.router.release_half_open_permit(candidate.model_id());
                last = error;
                continue;
            }
        };
        let mut tried_rectifiers = std::collections::HashSet::new();
        let mut binding = app
            .upstream_sessions
            .resolve(candidate, &mut body, &mut identity);
        loop {
            if tokio::time::Instant::now() >= deadline {
                app.router.release_half_open_permit(candidate.model_id());
                return Err(Error::Timeout(policy::DEADLINE.as_secs()));
            }
            let started = Instant::now();
            lifecycle.attempted(candidate);
            lifecycle.begin_attempt(candidate, budget.cleaned || !tried_rectifiers.is_empty())?;
            let result = tokio::time::timeout_at(
                deadline,
                app.upstream.stage_codex(
                    &candidate.provider,
                    key.as_deref(),
                    &body,
                    &candidate.entry.upstream_model,
                    &identity,
                ),
            )
            .await;
            let result = result.unwrap_or_else(|_| Err(Error::Timeout(policy::DEADLINE.as_secs())));
            match result {
                Ok(staged) => {
                    let mut completed = match normalize(staged, &shaped, candidate, id) {
                        Ok(completed) => completed,
                        Err(error) => {
                            lifecycle.failed_attempt(started, &error);
                            app.router.release_half_open_permit(candidate.model_id());
                            return Err(error);
                        }
                    };
                    completed.started = started;
                    app.router.report_success(
                        candidate.model_id(),
                        started.elapsed().as_millis() as u64,
                        lifecycle.routing(),
                    );
                    tracing::info!(
                        request_id = lifecycle.id(),
                        retries = budget.total,
                        "codex staged response accepted"
                    );
                    app.codex_filter.update(|stats| {
                        for (index, count) in budget.by_kind.iter().enumerate() {
                            if *count > 0 {
                                stats.retries_accepted[index] += 1;
                            }
                        }
                    });
                    return Ok(completed);
                }
                Err(error) => {
                    lifecycle.failed_attempt(started, &error);
                    if lifecycle.routing().rectifier.enabled
                        && error.is_session_binding_invalidated()
                        && budget.total < policy::MAX_RETRIES
                        && binding.refresh(&app.upstream_sessions, &mut body, &mut identity)
                    {
                        app.accounting_ready()?;
                        budget.total += 1;
                        tried_rectifiers.insert("session_binding");
                        tracing::info!(
                            request_id = lifecycle.id(),
                            "renewed upstream session binding"
                        );
                        continue;
                    }
                    let failure = error.classified();
                    if failure.class == crate::failure::FailureClass::RateLimit {
                        // Staging makes replay possible, not appropriate: a
                        // confirmed rate window closes this model/Key now. Keep
                        // independent siblings available and never sleep/replay
                        // twenty times under the filter's generic allowance.
                        app.router.note_candidate_retry_after(candidate, &error);
                        app.router.record_candidate_failure(
                            candidate,
                            &failure,
                            lifecycle.routing(),
                        );
                        last = error;
                        break;
                    }
                    if let Some(decision) = policy::classify_error(&error).filter(|_| auto_enabled)
                    {
                        let floor = match &error {
                            Error::Upstream { retry_after, .. } => retry_after.unwrap_or_default(),
                            _ => Duration::ZERO,
                        };
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if let Some(delay) = budget.schedule(
                            decision,
                            floor,
                            remaining,
                            rand::random::<u64>() % 1000,
                        ) {
                            if decision.cleanup && !budget.cleaned {
                                let report = match policy::clean_history(&mut body) {
                                    Ok(report) => report,
                                    Err(error) => {
                                        app.codex_filter.update(|stats| stats.cleanup_refused += 1);
                                        app.router.release_half_open_permit(candidate.model_id());
                                        return Err(error);
                                    }
                                };
                                budget.cleaned = true;
                                app.codex_filter.update(|stats| {
                                    stats.cleanup_requests += 1;
                                    stats.removed_reasoning_items += report.reasoning as u64;
                                    stats.removed_item_ids += report.ids as u64;
                                });
                                tracing::info!(
                                    request_id = lifecycle.id(),
                                    error_code = decision.code,
                                    removed_reasoning = report.reasoning,
                                    removed_ids = report.ids,
                                    "codex history cleanup"
                                );
                            }
                            tracing::info!(
                                request_id = lifecycle.id(),
                                kind = decision.kind.label(),
                                code = decision.code,
                                retry = budget.total,
                                delay_ms = delay.as_millis(),
                                "codex retry scheduled"
                            );
                            tokio::time::sleep(delay).await;
                            app.codex_filter
                                .update(|stats| stats.retries[decision.kind.index()] += 1);
                            continue;
                        }
                        app.codex_filter
                            .update(|stats| stats.retries_exhausted[decision.kind.index()] += 1);
                    } else if auto_enabled {
                        // Preserve existing named repairs, but never multiply V10's
                        // total retry allowance or replay an unmodified body.
                        let rectifiers = rectifier::from_config(&lifecycle.routing().rectifier);
                        let mut repaired = false;
                        for rectifier in rectifiers {
                            if req.reasoning.is_some() && rectifier.name() == "thinking_budget" {
                                continue;
                            }
                            if !tried_rectifiers.contains(rectifier.name())
                                && rectifier.should_apply(&error, &body)
                                && budget.total < policy::MAX_RETRIES
                                && tokio::time::Instant::now() < deadline
                            {
                                let result = rectifier.rectify(&mut body);
                                tried_rectifiers.insert(rectifier.name());
                                if result.applied {
                                    budget.total += 1;
                                    repaired = true;
                                    break;
                                }
                            }
                        }
                        if repaired {
                            continue;
                        }
                    }
                    app.router.record_candidate_failure(
                        candidate,
                        &error.classified(),
                        lifecycle.routing(),
                    );
                    app.router.note_candidate_retry_after(candidate, &error);
                    let fallback = app
                        .router
                        .should_fallback_candidate(candidate, &error.classified());
                    last = error;
                    if !fallback || tokio::time::Instant::now() >= deadline {
                        return Err(last);
                    }
                    break;
                }
            }
        }
    }
    Err(last)
}

fn normalize(
    staged: StagedResponse,
    req: &ChatRequest,
    candidate: &Candidate,
    id: Option<&str>,
) -> Result<Completed> {
    let mut frames = Vec::new();
    let mut declared_model = None;
    let mut wire = match staged {
        StagedResponse::Responses(raw) => {
            frames = raw;
            frames
                .last()
                .and_then(|frame| frame.json().ok())
                .and_then(|value| value.get("response").cloned())
                .ok_or_else(|| {
                    Error::BadUpstreamPayload("terminal Responses event is missing response".into())
                })?
        }
        StagedResponse::Json(value) if candidate.provider.dialect() == Dialect::OpenAIResponses => {
            value
        }
        staged => {
            let response = match staged {
                StagedResponse::Json(value) => {
                    protocol::decode_response(candidate.provider.dialect(), value)?
                }
                StagedResponse::Events(events) => {
                    if req.stream {
                        let mut encoder =
                            protocol::responses::ResponsesStreamEncoder::new(&candidate.exposed_id);
                        for event in &events {
                            frames.extend(encoder.encode(event));
                        }
                        frames.extend(encoder.finish());
                    }
                    protocol::collect_stream(events)
                }
                StagedResponse::Responses(_) => unreachable!(),
            };
            let mut wire = protocol::responses::encode_response(&response);
            declared_model = (!response.model.trim().is_empty()).then(|| response.model.clone());
            protocol::responses::restore_response_tools(&mut wire, req);
            wire
        }
    };
    if !wire.is_object() {
        return Err(Error::BadUpstreamPayload(
            "Responses response must be an object".into(),
        ));
    }
    if declared_model.is_none() {
        declared_model = wire
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .map(str::to_string);
    }
    if let Some(id) = id {
        wire["id"] = json!(id);
    }
    wire["model"] = json!(candidate.exposed_id);
    if req.stream && frames.is_empty() {
        frames.push(SseFrame {
            event: Some("response.created".into()),
            data: json!({
            "type":"response.created", "response":{"id":wire["id"],"object":"response",
                "model":candidate.exposed_id,"status":"in_progress","output":[]}})
            .to_string(),
        });
        for (index, item) in wire["output"].as_array().into_iter().flatten().enumerate() {
            frames.push(SseFrame {
                event: Some("response.output_item.done".into()),
                data: json!({
                "type":"response.output_item.done","output_index":index,"item":item})
                .to_string(),
            });
        }
        frames.push(SseFrame { event: Some(if wire["status"] == "incomplete" { "response.incomplete" } else { "response.completed" }.into()),
            data: json!({"type":if wire["status"] == "incomplete" { "response.incomplete" } else { "response.completed" },"response":wire}).to_string() });
    }
    for frame in &mut frames {
        if let Ok(mut value) = frame.json() {
            if let Some(response) = value.get_mut("response") {
                if let Some(id) = id {
                    response["id"] = json!(id);
                }
                response["model"] = json!(candidate.exposed_id);
            }
            frame.data = value.to_string();
        }
    }
    if candidate.provider.dialect() != Dialect::OpenAIResponses {
        protocol::responses::restore_stream_frames(&mut frames, req);
    }
    session_binding::annotate_response(&mut wire, &req.reply_headers);
    session_binding::annotate_frames(&mut frames, &req.reply_headers);
    let usage = protocol::responses::response_usage(&wire);
    Ok(Completed {
        wire,
        frames,
        usage,
        candidate: candidate.clone(),
        started: Instant::now(),
        declared_model,
    })
}
