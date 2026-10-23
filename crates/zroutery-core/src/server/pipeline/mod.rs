//! The request pipeline: walk the router's plan, talk to a provider, and turn the
//! answer back into the client's dialect.
//!
//! Split from the routing table because the two change for different reasons. That
//! file is about which paths exist; this one is about what happens once a request
//! is on one of them.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`lifecycle`] | one request from entry to exactly one terminal state: `RequestLifecycle`, the `Settlement` it is given, and the `TerminalKind` that ends it |
//! | [`budget`] | spending limits and admission - take the permits, then check the ledger |
//! | [`ml_rank`] | re-ordering the plan with the learned model, and the exploration path when no model can rank it |
//! | [`client`] | working out which client sent this request |
//! | [`repair`] | repairing a request the upstream refused, and settling the attempt when the repair is refused too |
//! | [`response`] | turning a settled request into the response the client gets |
//! | [`sse`] | the streaming body and its drop guard |
//!
//! What stays here is the orchestration: `handle_chat`, the attempt loop
//! (`buffered_chat` and `stream_chat`), request preparation, policy resolution
//! and the early-reject path. Those are the parts that read all the others, and
//! splitting them further means deciding where the orchestration boundary
//! belongs rather than where a section marker happens to fall.
//!
//! Every child's surface is `pub(super)` and nothing is re-exported: the `use`
//! lines below name exactly what this file calls, and a child that is not named
//! there is a child this file does not depend on.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use super::{error_response, AdmissionGuard, AppState};
use crate::budget::Verdict;
use crate::config::ClientIdentity;
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, Dialect, Message, ResponseStatus, StoredResponse};
#[cfg(feature = "ml")]
use crate::ml::ShadowInput;
use crate::policy::{self, ClientContext, RouteDecision, RoutingPolicy};
use crate::protocol::{self, openai};
use crate::query::RequestKind;
use crate::registry::Resolution;
use crate::router::Candidate;
mod sse;

mod budget;
mod client;
mod lifecycle;
mod ml_rank;
mod repair;
mod response;

#[cfg(test)]
mod tests;

// Every child's surface is used by "handle_chat" or the attempt loop, and both
// stay here, so nothing below is re-exported: these import lines are the whole
// interface, and each names exactly what the remaining code calls.
use budget::{apply_budgets, classifier_admission_scopes};
use client::{build_client_context, client_for, client_identity};
use lifecycle::{RequestLifecycle, Settlement, TerminalKind};
#[cfg(feature = "ml")]
use ml_rank::apply_ml_ranking;
use repair::{maybe_filter_stream, try_rectify_buffered, try_rectify_stream};
use response::{inject_cost_header, inject_routing_headers, register_in_flight, stream_response};
pub(super) async fn handle_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    headers: axum::http::HeaderMap,
    body: Value,
) -> Response {
    let include_usage = dialect == Dialect::OpenAI && openai::wants_stream_usage(&body);

    let mut req = match protocol::decode_request(dialect, body.clone()) {
        Ok(r) => r,
        Err(e) => return error_response(dialect, &e),
    };
    // Client identity is request context. It is carried alongside the IR so
    // every provider attempt, including retries and auxiliary calls, can use
    // the provider's resolved client profile, and so that the profile's
    // passthrough set can be resolved from the real inbound headers.
    req.client_identity = client_identity(&headers);
    req.client_headers = headers.clone();

    // Retention policy, read from the decoded request rather than the raw
    // body, and honoured by every path that could write a response snapshot.
    let storage = protocol::responses::StoragePolicy::from_request(&req);

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

    // The Responses API lifecycle fields. A continuation is either accepted —
    // the prior conversation is replayed into this request's messages — or the
    // request is rejected with the reason; `previous_response_id` is an
    // instruction to include history, and answering without it is the one
    // outcome that is never allowed. The input items kept for storage are the
    // items as they arrived, normalized so a string `input` is the user turn it
    // means rather than an empty list.
    let (input_items, previous_response_id) = if dialect == Dialect::OpenAIResponses {
        // Capability requirements are derived after history expansion, because
        // a continued conversation can carry images or tools the current turn
        // does not mention.
        match expand_continuation(&state.response_store, &body, &mut req) {
            Ok(fields) => fields,
            Err(e) => {
                reject(
                    Arc::clone(&state),
                    dialect,
                    req.stream,
                    kind,
                    &req.model,
                    &req.client_headers,
                    &e,
                );
                return error_response(dialect, &e);
            }
        }
    } else {
        (Vec::new(), None)
    };
    req.required_capabilities = req.compute_required_capabilities();

    // Build task profile and client context for policy resolution.
    let task_profile = policy::TaskProfile::from_request(&req);
    let header_pairs: Vec<(&str, &str)> = headers
        .iter()
        .filter_map(|(name, value)| {
            let val = value.to_str().ok()?;
            Some((name.as_str(), val))
        })
        .collect();
    let client_ctx = build_client_context(&headers, &req.model, &header_pairs);
    let policy_config = &config.routing.policies;
    let matched_policy = resolve_policy(policy_config, &client_ctx, &task_profile);

    // Admission permits for every budgeted scope this request occupies. Taken
    // before the budget check and held until the request settles: the streaming
    // path moves the guard into the body stream, because that response outlives
    // this function, and every other path drops it after the terminal
    // transition has charged the ledger.
    let admission: AdmissionGuard;

    let (plan, routing_decision) = match kind {
        RequestKind::Main => {
            // Fallback Contract:
            //
            // 1. POLICY FALLBACK (before any request is sent):
            //    - Escalation/degradation happens at the candidate selection level
            //    - No upstream request has been made yet
            //    - Safe to try any eligible candidate
            //
            // 2. TRANSPORT RETRY (after request sent, before streaming output):
            //    - Connection failure or timeout before first token
            //    - Can retry same candidate or failover to next
            //    - Rectifier cascade applies here
            //
            // 3. NO FALLBACK (after streaming output has been sent to client):
            //    - Once tokens have been sent to the client
            //    - Once tool calls have been executed
            //    - Once terminal events have been sent
            //    - The response cannot be transparently retried
            let result = match registry.resolve(&req.model) {
                Ok(resolution) => apply_budgets(&state, &registry, resolution).await.and_then(
                    |(resolution, guard)| {
                        // Resolution::Direct skips policy: the client named an exact
                        // model, so policy filtering would be surprising.
                        // Resolution::Tier uses policy-aware routing when a policy is
                        // resolved (via client profile, matchers, or default).
                        let planned = match &resolution {
                            Resolution::Direct(_) => state
                                .router
                                .plan(&registry, &resolution, &req.required_capabilities)
                                .map(|plan| (plan, None)),
                            Resolution::Tier(_) => match &matched_policy {
                                Some(policy) => {
                                    tracing::debug!(
                                        policy_id = %policy.id,
                                        "using policy-aware routing"
                                    );
                                    state
                                        .router
                                        .plan_with_policy(
                                            &registry,
                                            &resolution,
                                            &req.required_capabilities,
                                            &policy.requirements,
                                            &policy.preference,
                                            &policy.fallback,
                                            Some(&task_profile),
                                        )
                                        .map(|(plan, mut decision)| {
                                            decision.policy_id = policy.id.clone();
                                            decision.policy_revision.policy_id = policy.id.clone();
                                            decision.policy_revision.policy_enabled =
                                                policy.enabled;
                                            (plan, Some(decision))
                                        })
                                }
                                None => state
                                    .router
                                    .plan(&registry, &resolution, &req.required_capabilities)
                                    .map(|plan| (plan, None)),
                            },
                        };
                        planned.map(|(plan, decision)| (plan, decision, guard))
                    },
                ),
                Err(e) => Err(e),
            };
            match result {
                Ok((plan, decision, guard)) => {
                    admission = guard;
                    (plan, decision)
                }
                Err(e) => {
                    // Terminal before any candidate was tried: one record, one
                    // outcome, one canonical classification.
                    reject(
                        Arc::clone(&state),
                        dialect,
                        req.stream,
                        kind,
                        &req.model,
                        &req.client_headers,
                        &e,
                    );
                    return error_response(dialect, &e);
                }
            }
        }
        // The classifier pool is resolved directly: the model the client named
        // (e.g. `claude-opus-4-8[1m]`) is irrelevant to *which model judges*,
        // and may not even exist in the registry. The class (tier) budget is
        // deliberately not applied: the pool is not chosen by tier policy, so a
        // class budget neither degrades nor rejects a side request. That
        // exception is only for the class scope. Global and per-provider limits
        // are still spend limits, so every candidate whose provider (or the
        // global scope) is already used up is dropped before it can be sent.
        RequestKind::Side(_) => {
            let classifier = config.classifier.clone();
            match state.router.plan_classifier(&registry, &classifier) {
                Ok(plan) => {
                    // Admission before the check, exactly as for main traffic:
                    // a side request is real spend, and its permits cover every
                    // provider the pool could fail over to.
                    let guard = state
                        .admit(&classifier_admission_scopes(config, &plan))
                        .await;
                    let mut allowed = Vec::new();
                    let mut refusal: Option<String> = None;
                    for candidate in plan {
                        let provider_ids = [candidate.provider.id.clone()];
                        match state.classifier_budget_verdict(&provider_ids) {
                            Verdict::Allow => allowed.push(candidate),
                            Verdict::Reject { because } => {
                                tracing::warn!(
                                    requested_model = %req.model,
                                    candidate_model = candidate.model_id(),
                                    "classifier candidate skipped: {because}"
                                );
                                refusal.get_or_insert(because);
                            }
                            Verdict::Degrade { because, .. } => {
                                // The classifier pool has no cheaper tier to
                                // follow, so a degrade verdict can only mean
                                // "not this candidate".
                                tracing::warn!(
                                    requested_model = %req.model,
                                    candidate_model = candidate.model_id(),
                                    "classifier candidate skipped: {because}"
                                );
                                refusal.get_or_insert(because);
                            }
                        }
                    }
                    if allowed.is_empty() {
                        let e = Error::OverBudget(refusal.unwrap_or_else(|| {
                            "the classifier budget has no candidate left".to_string()
                        }));
                        reject(
                            Arc::clone(&state),
                            dialect,
                            req.stream,
                            kind,
                            &req.model,
                            &req.client_headers,
                            &e,
                        );
                        return error_response(dialect, &e);
                    }
                    admission = guard;
                    (allowed, None)
                }
                Err(e) => {
                    reject(
                        Arc::clone(&state),
                        dialect,
                        req.stream,
                        kind,
                        &req.model,
                        &req.client_headers,
                        &e,
                    );
                    return error_response(dialect, &e);
                }
            }
        }
    };

    // Log a summary of the routing decision for diagnostics.
    if let Some(ref decision) = routing_decision {
        tracing::debug!(
            decision_id = %decision.decision_id,
            policy_id = %decision.policy_id,
            selected = ?decision.selected,
            candidates = decision.candidates.len(),
            fallback_chain = ?decision.fallback_chain,
            reason = ?decision.reason,
            "routing decision recorded"
        );
    }

    // shadow-block-begin
    // The decision-time snapshot: what the ML stack saw when the decision was
    // made.
    //
    // Built once, here, and shared. Both the shadow record and the ML ranking
    // read this same value, which is not an optimisation — if the two each built
    // their own snapshot they could disagree about a candidate set, and every
    // comparison between what production served and what the model would have
    // served would then be comparing two different questions.
    //
    // Taken after routing and before any attempt, so no observation a request
    // itself produced can leak into the features that judged it.
    //
    // Direct-resolution and classifier requests carry no routing decision and
    // therefore have no policy evidence to snapshot, so they are out of scope by
    // construction.
    //
    // Built only when something will read it. Feature extraction walks every
    // candidate and every observation, and a deployment with neither the shadow
    // nor an active model has no use for the result, so paying for it on every
    // request would be a cost for a record nothing would keep.
    // Built only when something will read it, and in the non-`ml` build there is
    // nothing that can: the shadow and the ranking are both behind the feature,
    // so the snapshot is not taken at all and no feature vector is extracted.
    #[cfg(feature = "ml")]
    let decision_snapshot = if state.shadow().enabled() || state.ml_routing().is_attached() {
        routing_decision.as_ref().map(|decision| {
            ShadowInput::from_policy_plan(
                state.router().observations(),
                &state.router().stats_store,
                &plan,
                decision,
                &task_profile,
                req.messages.len(),
            )
        })
    } else {
        None
    };
    #[cfg(feature = "ml")]
    let shadow_input = decision_snapshot.clone();
    // shadow-block-end

    // ml-ranking-block-begin
    // The learned model re-orders the executable plan.
    //
    // Placement is deliberate. Eligibility, the circuit breaker and the attempt
    // cap have already narrowed `plan`, so what ML chooses between is a set of
    // candidates that are all legal to serve; ML never re-derives any of those
    // filters and cannot widen them. The plan is also still the router's own, so
    // when nothing can be ranked — no model, a prediction that is not finite, a
    // selection the plan does not contain — the order below is exactly the one
    // the router produced.
    //
    // The decision is amended as well as the plan. `RouteDecision.selected` is
    // what the outcome records as the planned identity, so leaving it alone
    // would produce traces in which the request says it planned one provider and
    // was served by another. The learning loop reads those traces, so a mismatch
    // here would be a systematic label error in the training data.
    #[cfg(feature = "ml")]
    let (plan, routing_decision) = {
        let ranked = apply_ml_ranking(&state, plan, routing_decision, decision_snapshot);
        (ranked.0, ranked.1)
    };
    // ml-ranking-block-end

    if req.stream {
        stream_chat(
            state,
            dialect,
            req,
            plan,
            kind,
            include_usage,
            input_items,
            previous_response_id,
            storage,
            routing_decision,
            admission,
            #[cfg(feature = "ml")]
            shadow_input,
        )
        .await
    } else {
        buffered_chat(
            state,
            dialect,
            req,
            plan,
            kind,
            input_items,
            previous_response_id,
            storage,
            routing_decision,
            admission,
            #[cfg(feature = "ml")]
            shadow_input,
        )
        .await
    }
}

/// How many prior turns a `previous_response_id` chain may replay.
/// The chain is walked newest to oldest, so this is also the bound on how much
/// work a single continuation request can create before any upstream call.
const MAX_CONTINUATION_TURNS: usize = 32;

/// The byte budget for one continuation's replayed history.
///
/// Measured over the stored input and output items that are about to become
/// messages, so an oversized chain is refused before it reaches a provider.
const MAX_CONTINUATION_BYTES: usize = 1 << 20;

/// Size of one stored item list, as the bytes a request would carry.
fn items_bytes(items: &[Value]) -> usize {
    items.iter().map(|item| item.to_string().len()).sum()
}

/// The wire name of a stored response's status, for a refusal message.
fn response_status_name(status: ResponseStatus) -> &'static str {
    match status {
        ResponseStatus::Queued => "queued",
        ResponseStatus::InProgress => "in progress",
        ResponseStatus::Completed => "completed",
        ResponseStatus::Failed => "failed",
        ResponseStatus::Cancelled => "cancelled",
    }
}

/// Resolve a Responses API continuation, expanding the prior conversation.
///
/// Returns the input items to store for *this* request (normalized from the raw
/// body, never the expanded history — a chain must not duplicate itself) and the
/// prior id when one was accepted.
///
/// `previous_response_id` is only accepted when the prior response exists and
/// may actually be continued, and its conversation is then replayed into the
/// request's messages ahead of the current turn. Every way that can fail is an
/// explicit error: an unknown or deleted prior, one that was never retained, a
/// non-completed prior, a chain longer than [`MAX_CONTINUATION_TURNS`] or
/// larger than [`MAX_CONTINUATION_BYTES`], and a chain that loops. Silently
/// ignoring the field would answer the wrong question with the right status.
fn expand_continuation(
    store: &crate::ir::ResponseStore,
    body: &Value,
    req: &mut ChatRequest,
) -> Result<(Vec<Value>, Option<String>)> {
    let input_items = protocol::responses::input_items(body);
    let Some(previous) = body.get("previous_response_id").and_then(Value::as_str) else {
        return Ok((input_items, None));
    };
    let previous = previous.to_string();

    let mut chain: Vec<StoredResponse> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut cursor = Some(previous.clone());
    let mut bytes = 0usize;
    while let Some(id) = cursor {
        if !visited.insert(id.clone()) {
            return Err(Error::invalid(format!(
                "`previous_response_id` chain loops back to `{id}`"
            )));
        }
        if chain.len() >= MAX_CONTINUATION_TURNS {
            return Err(Error::invalid(format!(
                "`previous_response_id` chain is longer than {MAX_CONTINUATION_TURNS} turns"
            )));
        }
        let stored = store.get(&id).ok_or_else(|| {
            Error::invalid(format!(
                "`previous_response_id` `{id}` is not available to continue"
            ))
        })?;
        if stored.status != ResponseStatus::Completed {
            return Err(Error::invalid(format!(
                "`previous_response_id` `{id}` is {} and cannot be continued",
                response_status_name(stored.status)
            )));
        }
        bytes += items_bytes(&stored.input) + items_bytes(&stored.output);
        if bytes > MAX_CONTINUATION_BYTES {
            return Err(Error::invalid(format!(
                "`previous_response_id` history is larger than {MAX_CONTINUATION_BYTES} bytes"
            )));
        }
        cursor = stored.previous_response_id.clone();
        chain.push(stored);
    }

    // Oldest turn first, so the replayed messages read in conversation order.
    chain.reverse();
    let mut history: Vec<Message> = Vec::new();
    for stored in &chain {
        history.extend(protocol::responses::decode_history(&stored.input)?);
        history.extend(protocol::responses::decode_history(&stored.output)?);
    }
    let current = std::mem::take(&mut req.messages);
    req.messages = history;
    req.messages.extend(current);
    Ok((input_items, Some(previous)))
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
    // A decision endpoint is not asked a chat. It is asked a question, in its
    // own protocol, and the body is the decision shape: the state the client
    // sent, and the one typed question the Auto Mode's question becomes.
    if candidate.provider.kind.is_decision() {
        let (state_text, instructions) = crate::upstream::decision_question(req);
        // The model travels in the body, not the URL: a decision endpoint serves
        // several models at one path, and the one the operator configured is the
        // one that must answer.
        let body = crate::upstream::build_decision_body(
            &state_text,
            &instructions,
            &candidate.entry.upstream_model,
        );
        return Ok((key, body));
    }
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
async fn apply_vision_fallback(state: &AppState, req: &mut ChatRequest, reason: &str) {
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
async fn apply_vision_fallback_inner(state: &AppState, req: &mut ChatRequest, reason: &str) {
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
    let key = state.api_key(&target.provider).ok().flatten();

    let mut described = 0;
    let mut placeholders = 0;
    let mut refused_by_budget = 0;
    for (slot, source) in &images {
        // Every description is its own paid call to the vision provider, so
        // every one is gated like any other request before it leaves: the
        // shared verdict covers global, the provider it will actually reach,
        // and that provider's tier. A fixed vision target cannot follow a
        // degrade to another tier, so anything short of an allowance means
        // "not this call" — and because a charge lands in the ledger
        // immediately, the image that crosses the line completes while the
        // next one is already stopped.
        //
        // Admission residual, deliberately accepted: this auxiliary call does
        // not take a permit of its own. The request that made it already holds
        // the permits for its own resolution, and acquiring the vision
        // provider's scope here — possibly ordered before one already held —
        // could invert the canonical order and deadlock two requests that
        // crossed. So the vision provider's own budget is checked and charged
        // but not serialised by its own permit: two requests that both call the
        // same vision provider, with no shared global or provider scope in
        // their resolution, can overshoot that one budget by more than one
        // call. A global limit still serialises them, because the calling
        // request holds the global permit across this loop.
        let verdict =
            state.budget_verdict(std::slice::from_ref(&target.provider.id), target.entry.tier);
        if !matches!(verdict, Verdict::Allow) {
            refused_by_budget += 1;
            tracing::warn!(
                reason,
                vision_model = target.entry.exposed_id(),
                vision_provider = target.provider.id.as_str(),
                verdict = ?verdict,
                "vision fallback skipped by budget; image replaced with the placeholder"
            );
            crate::media::transform::replace(
                req,
                slot,
                &crate::media::transform::Replacement::Placeholder(
                    config.vision.placeholder.clone(),
                ),
            );
            continue;
        }

        let started = std::time::Instant::now();
        let outcome = crate::media::vision::describe(
            &state.upstream,
            &target,
            key.as_deref(),
            source,
            req.client_identity.as_deref(),
            &req.client_headers,
        )
        .await;
        let latency_ms = started.elapsed().as_millis() as f64;
        let model_id = target.entry.exposed_id();
        let replacement = match outcome {
            Ok(resp) => {
                described += 1;
                // The vision model is real traffic to a real provider, so
                // its attempt is reported through the same canonical
                // adapters as any other: a success keeps the health view
                // from being failure-only, which would otherwise open the
                // breaker for a model that answers far more often than it
                // fails.
                state
                    .router
                    .report_success(&model_id, latency_ms as u64, &state.config().routing);
                state.router.record_classified_outcome(
                    &model_id,
                    &target.provider.id,
                    latency_ms,
                    None,
                    true,
                    None,
                );
                // Charge the auxiliary call immediately, against the
                // provider that answered and the tier it is billed under,
                // so a later failure in this loop cannot lose spend that
                // was already incurred.
                if let Some(pricing) = target.entry.pricing.as_ref() {
                    let cost = pricing.cost_of(&resp.usage);
                    state.charge_auxiliary(&target.provider.id, target.entry.tier, &cost);
                }
                crate::media::transform::Replacement::Description(
                    crate::media::vision::description_text(&resp),
                )
            }
            Err(e) => {
                placeholders += 1;
                // An auxiliary failure is still an observed failure of the
                // model that was asked, reported through the same canonical
                // adapter as any other attempt.
                state.router.record_classified_attempt(
                    &model_id,
                    &target.provider.id,
                    &e.classified(),
                    &state.config().routing,
                );
                tracing::warn!(
                    vision_model = model_id.as_str(),
                    error = %e,
                    "vision description failed; using the placeholder for this image"
                );
                crate::media::transform::Replacement::Placeholder(config.vision.placeholder.clone())
            }
        };
        crate::media::transform::replace(req, slot, &replacement);
    }
    tracing::info!(
        reason,
        vision_model = target.entry.exposed_id(),
        vision_provider = target.provider.id.as_str(),
        described,
        placeholders,
        refused_by_budget,
        "vision fallback applied"
    );
}
/// Replace a request's key with the resolved OpenCode Console access token and
/// set the billing org on the identity. A no-op for every other provider, so the
/// key the secret store produced is used unchanged.
#[allow(unused_variables, unused_mut)] // used only under the `opencode` feature
async fn console_key(
    state: &AppState,
    candidate: &Candidate,
    key: Option<String>,
    client: &mut ClientIdentity,
) -> Result<Option<String>> {
    #[cfg(feature = "opencode")]
    if candidate.provider.kind == crate::config::ProviderKind::OpenCodeConsole {
        let (token, org) = state.console_credential(&candidate.provider).await?;
        client.org_id = org;
        return Ok(Some(token));
    }
    Ok(key)
}

/// Send one buffered call, retrying the same candidate when the failure is
/// one a replay can fix.
///
/// The candidate is not changed: a rate limit or a gateway blip is the same
/// upstream asking for a moment, and the next candidate is the fallback's job.
/// The policy lives in `upstream::retry_advice` so the streaming handshake and
/// this path cannot disagree about what is retryable.
async fn send_with_retry<F, Fut, T>(
    provider: &crate::config::ProviderConfig,
    can_fail_over: bool,
    mut send: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut retries = 0u32;
    loop {
        match send().await {
            Ok(value) => return Ok(value),
            Err(err) => match crate::upstream::retry_advice(&err, retries, can_fail_over, 1) {
                Some(delay) => {
                    tracing::warn!(
                        provider = %provider.name,
                        retry = retries + 1,
                        wait_ms = delay.as_millis(),
                        error = %err,
                        "retryable upstream failure, retrying the same candidate"
                    );
                    tokio::time::sleep(delay).await;
                    retries += 1;
                }
                None => return Err(err),
            },
        }
    }
}

/// Send one buffered request, folding an SSE-only upstream back into a response.
///
/// OpenCode's free tier forces `stream: true` (its gate rejects a non-streaming
/// request), so a non-streaming client's answer arrives as SSE. That case is
/// collected back into one [`crate::ir::ChatResponse`] here; every other provider
/// uses the plain buffered send.
async fn buffered_upstream(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    body: &Value,
    client: &ClientIdentity,
    kind: RequestKind,
    can_fail_over: bool,
) -> Result<crate::ir::ChatResponse> {
    // A decision endpoint answers with a probability rather than a message, so
    // the answer is read as a probability and then *spoken back* in the
    // client's own protocol. Nothing downstream has to know which kind of
    // model answered, and the client sees exactly what a language model's
    // verdict would have looked like.
    if candidate.provider.kind.is_decision() {
        let decision = send_with_retry(&candidate.provider, can_fail_over, || {
            state.upstream.send_decision(&candidate.provider, key, body)
        })
        .await?;
        let threshold = candidate.provider.decision.threshold;
        let verdict = crate::classifier::verdict_from_decision(Some(decision), threshold);
        return Ok(decision_response(
            kind,
            verdict,
            &candidate.entry.upstream_model,
        ));
    }
    #[cfg(feature = "opencode")]
    if client.profile == crate::config::ProviderClientProfile::OpenCodeCli {
        use futures_util::StreamExt as _;
        let mut stream = state
            .upstream
            .stream_with_identity_policy(
                &candidate.provider,
                key,
                body,
                &candidate.entry.upstream_model,
                client,
                can_fail_over,
            )
            .await?;
        let mut collector = protocol::StreamCollector::default();
        while let Some(event) = stream.next().await {
            // A mid-stream error is a real failure, not an empty answer.
            collector.push(&event?);
        }
        return Ok(collector.finish());
    }
    send_with_retry(&candidate.provider, can_fail_over, || {
        state
            .upstream
            .send_with_identity(&candidate.provider, key, body, client)
    })
    .await
}

/// The answer a decision model's probability is spoken back as.
///
/// A decision endpoint has no message of its own — it has a probability — and
/// the client that asked the question expects a verdict in its own protocol.
/// So the verdict is written as the text a language model's answer would have
/// been, and the rest of the pipeline carries it unchanged.
fn decision_response(
    kind: RequestKind,
    verdict: crate::classifier::ClassifierVerdict,
    model: &str,
) -> crate::ir::ChatResponse {
    crate::ir::ChatResponse {
        // An id of its own, and not a constant: the decision protocol has no
        // id to carry, and two answers that shared one would be two answers a
        // client could not tell apart.
        id: format!(
            "zroutery-decision-{}",
            chrono::Utc::now().timestamp_millis()
        ),
        model: model.to_string(),
        content: vec![crate::ir::ContentBlock::text(
            crate::classifier::verdict_text_for(kind, verdict),
        )],
        stop_reason: crate::ir::StopReason::EndTurn,
        stop_sequence: None,
        usage: crate::ir::Usage::default(),
        passthrough: Default::default(),
    }
}

// Eleven arguments, and each one is a distinct piece of the request that this
// function would otherwise have to re-derive: the assembled body, the plan it
// executes, the admission guard that must outlive the call, and the shadow
// input held for the ML journal. Bundling them into a struct would move the
// same eleven fields one level down and add a type whose only purpose is to
// satisfy a lint, so the count is allowed here and named.
#[allow(clippy::too_many_arguments)]
async fn buffered_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    req: ChatRequest,
    plan: Vec<Candidate>,
    kind: RequestKind,
    input_items: Vec<Value>,
    previous_response_id: Option<String>,
    storage: protocol::responses::StoragePolicy,
    routing_decision: Option<RouteDecision>,
    // Held for the whole call, which is longer than the budget check and ends
    // only after the terminal transition has charged the ledger: the drop is
    // the release, so no early return in here can leak a scope.
    _admission: AdmissionGuard,
    #[cfg(feature = "ml")] shadow_input: Option<ShadowInput>,
) -> Response {
    let mut lifecycle = RequestLifecycle::new(
        Arc::clone(&state),
        dialect,
        false,
        kind,
        &req.model,
        &req.client_headers,
        #[cfg(feature = "ml")]
        Some(req.clone()),
    );
    // The router's own decision supplies the planned identity, before any
    // attempt is made and before anything can fail.
    lifecycle.planned_from(routing_decision.as_ref(), &plan);
    // shadow-block-begin
    // Shadow evaluation runs before the attempt loop, over the snapshot taken
    // while ranking state was untouched. It is record-only: the engine contains
    // its own faults (catch_unwind + fault counter) and the store is the record
    // — the verdict is deliberately discarded here and never feeds back into
    // this request. Only the record's identity is retained, so the terminal
    // transition can attach what actually served.
    #[cfg(feature = "ml")]
    if let Some(input) = &shadow_input {
        lifecycle.shadow_evaluated(input);
    }
    // shadow-block-end
    let mode = encode_mode_for(kind);
    let mut last_error = Error::NoCandidate(req.model.clone());

    for (index, candidate) in plan.iter().enumerate() {
        // A later candidate means this failure is failed over, not waited on.
        let can_fail_over = index + 1 < plan.len();
        lifecycle.attempted(candidate);
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
        if state.config().vision.enabled && !candidate.entry.capabilities.vision {
            let needs_vision = crate::media::collect::collect(&req);
            if !needs_vision.is_empty() {
                apply_vision_fallback(&state, &mut req, "preflight").await;
            }
        }

        let (key, body) = match prepare(&state, candidate, &req, mode) {
            Ok(v) => v,
            Err(e) => {
                // The candidate was never reached, but the loop did try it, so
                // the attempt evidence records the same canonical failure the
                // router adapter accounts for.
                lifecycle.begin_attempt(candidate, false);
                lifecycle.failed_attempt(attempt_start, &e);
                state.router.record_classified_attempt(
                    candidate.model_id(),
                    &candidate.provider.id,
                    &e.classified(),
                    lifecycle.routing(),
                );
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
            // No attempt is opened: nothing was sent and nothing failed, so
            // there is no attempt evidence to record.
            continue;
        }

        lifecycle.begin_attempt(candidate, false);
        let mut client = client_for(&req, &candidate.provider);
        let key = match console_key(&state, candidate, key, &mut client).await {
            Ok(key) => key,
            Err(e) => {
                lifecycle.failed_attempt(attempt_start, &e);
                state.router.record_classified_attempt(
                    candidate.model_id(),
                    &candidate.provider.id,
                    &e.classified(),
                    lifecycle.routing(),
                );
                last_error = e;
                continue;
            }
        };
        match buffered_upstream(
            &state,
            candidate,
            key.as_deref(),
            &body,
            &client,
            kind,
            can_fail_over,
        )
        .await
        {
            Ok(mut resp) => {
                // A placeholder we injected only to satisfy the gate must not
                // reach a client that never declared it.
                #[cfg(feature = "opencode")]
                if client.profile == crate::config::ProviderClientProfile::OpenCodeCli {
                    let declared: Vec<String> =
                        req.tools.iter().map(|tool| tool.name.clone()).collect();
                    crate::opencode::strip_injected_tool_calls(&mut resp, &declared);
                }
                // A classifier answer is only a success when it carries a
                // verdict. HTTP 200 without `<block>…</block>` is not an
                // approval — the model failed to do its job, so the attempt
                // is reported, the failure is recorded and the next candidate
                // is tried. There is no "looks safe" fallback.
                if !kind.is_main() {
                    match crate::classifier::parse_verdict_for(kind, &resp.text()) {
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
                            let unusable = Error::BadUpstreamPayload(format!(
                                "classifier response from `{}` carried no verdict",
                                candidate.model_id()
                            ));
                            lifecycle.failed_attempt(attempt_start, &unusable);
                            state.router.record_classified_attempt(
                                candidate.model_id(),
                                &candidate.provider.id,
                                &unusable.classified(),
                                lifecycle.routing(),
                            );
                            tracing::warn!(
                                kind = kind.as_str(),
                                candidate_model = candidate.model_id(),
                                upstream_model = %candidate.entry.upstream_model,
                                "classifier response had no verdict; failing over"
                            );
                            last_error = unusable;
                            continue;
                        }
                    }
                }
                let latency_ms = attempt_start.elapsed().as_millis() as u64;
                state
                    .router
                    .report_success(candidate.model_id(), latency_ms, lifecycle.routing());
                state.router.record_classified_outcome(
                    candidate.model_id(),
                    &candidate.provider.id,
                    latency_ms as f64,
                    None,
                    true,
                    None,
                );
                lifecycle.served_attempt(attempt_start);
                lifecycle.note_response_id(&resp.id);
                let cost = candidate
                    .entry
                    .pricing
                    .as_ref()
                    .map(|p| p.cost_of(&resp.usage));
                // Capture the upstream's own declaration before anything that can
                // consume it. Two things happen to it, and the order matters:
                //
                // * `finalize` below is the terminal transition, and the durable
                //   trace is written *inside* it. Capturing after `finalize` records
                //   an identity that was already persisted, which is how this was
                //   silently wrong at first: the streaming path kept the right order
                //   only because its terminal transition happens later, when the body
                //   ends, so the two paths disagreed.
                // * `resp.model = exposed_id` below discards the value outright.
                #[cfg(feature = "ml")]
                lifecycle.note_declared_model(Some(resp.model.as_str()));
                // Judge the task while both the request's demands and the response's
                // content are still in hand. Only the verdict is kept — the response
                // body is never persisted — so this is the one place a task check can
                // honestly run. It must also precede `finalize`, which persists.
                #[cfg(feature = "ml")]
                lifecycle.note_task_evidence(&req, &resp, candidate);
                // The terminal transition below owns the charge, the record and
                // the outcome: booking here as well would spend twice.
                lifecycle.finalize(
                    TerminalKind::Served,
                    Settlement {
                        usage: resp.usage,
                        pricing: candidate.entry.pricing.clone(),
                        provider_id: candidate.provider.id.clone(),
                        tier: candidate.entry.tier,
                    },
                );
                // Report the model that actually answered, not the virtual id.
                resp.model = candidate.exposed_id.clone();
                let wire = protocol::encode_response(dialect, &resp);
                if dialect == Dialect::OpenAIResponses && storage.retains() {
                    let output = wire
                        .get("output")
                        .cloned()
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default();
                    let stored = StoredResponse::completed(
                        resp.id.clone(),
                        resp.model.clone(),
                        input_items.clone(),
                        output,
                        resp.usage,
                        previous_response_id.clone(),
                        lifecycle.decision(),
                    );
                    state.response_store.put(stored);
                }
                let mut response = Json(wire).into_response();
                inject_routing_headers(response.headers_mut(), candidate, kind);
                inject_cost_header(response.headers_mut(), cost.as_ref());
                return response;
            }
            Err(e) => {
                // The send that just failed is over: settle its attempt before
                // any repair send opens one of its own, so the request's
                // attempt list is exactly one complete entry per upstream
                // send. The repair retry itself is not a fresh probe, so the
                // half-open permit is given back and no health is recorded.
                lifecycle.failed_attempt(attempt_start, &e);
                // Rectifier cascade: try to repair the request and retry the
                // same provider without touching circuit-breaker health.
                let repair_start = Instant::now();
                match try_rectify_buffered(
                    &state,
                    candidate,
                    key.as_deref(),
                    &mut req,
                    &e,
                    &mut lifecycle,
                )
                .await
                {
                    Ok(Some(mut resp)) => {
                        // The repaired retry is not a fresh probe: give the
                        // half-open permit back without recording health.
                        state.router.release_half_open_permit(candidate.model_id());
                        // A repaired classifier answer still has to carry a
                        // verdict; the same fail-closed rule as the direct
                        // path applies, minus the health report (this was a
                        // repaired retry, which never records health).
                        if !kind.is_main()
                            && crate::classifier::parse_verdict_for(kind, &resp.text())
                                == crate::classifier::ClassifierVerdict::Unparseable
                        {
                            tracing::warn!(
                                kind = kind.as_str(),
                                candidate_model = candidate.model_id(),
                                "rectified classifier response still had no verdict; failing over"
                            );
                            let verdict = Error::BadUpstreamPayload(format!(
                                "classifier response from `{}` carried no verdict",
                                candidate.model_id()
                            ));
                            lifecycle.failed_attempt(repair_start, &verdict);
                            last_error = verdict;
                            continue;
                        }
                        lifecycle.served_attempt(repair_start);
                        lifecycle.note_response_id(&resp.id);
                        let cost = candidate
                            .entry
                            .pricing
                            .as_ref()
                            .map(|p| p.cost_of(&resp.usage));
                        // Before `finalize`, which is the terminal transition that
                        // persists the trace. See the buffered path for why.
                        #[cfg(feature = "ml")]
                        lifecycle.note_declared_model(Some(resp.model.as_str()));
                        lifecycle.finalize(
                            TerminalKind::Served,
                            Settlement {
                                usage: resp.usage,
                                pricing: candidate.entry.pricing.clone(),
                                provider_id: candidate.provider.id.clone(),
                                tier: candidate.entry.tier,
                            },
                        );
                        resp.model = candidate.exposed_id.clone();
                        let wire = protocol::encode_response(dialect, &resp);
                        if dialect == Dialect::OpenAIResponses && storage.retains() {
                            let output = wire
                                .get("output")
                                .cloned()
                                .and_then(|v| v.as_array().cloned())
                                .unwrap_or_default();
                            let stored = StoredResponse::completed(
                                resp.id.clone(),
                                resp.model.clone(),
                                input_items.clone(),
                                output,
                                resp.usage,
                                previous_response_id.clone(),
                                lifecycle.decision(),
                            );
                            state.response_store.put(stored);
                        }
                        let mut response = Json(wire).into_response();
                        inject_routing_headers(response.headers_mut(), candidate, kind);
                        inject_cost_header(response.headers_mut(), cost.as_ref());
                        return response;
                    }
                    Ok(None) => {
                        // No repair was applied; the initial attempt is the
                        // failure this candidate contributes.
                        let failure = e.classified();
                        state.router.record_classified_attempt(
                            candidate.model_id(),
                            &candidate.provider.id,
                            &failure,
                            lifecycle.routing(),
                        );
                        tracing::warn!(
                            model = candidate.model_id(),
                            provider = candidate.provider.name.as_str(),
                            "upstream attempt failed: {e}"
                        );
                        // Whether to keep going is the classified impact
                        // table's answer, asked through the router adapter.
                        let keep_going = state.router.should_fallback_failure(&failure);
                        last_error = e;
                        if !keep_going {
                            break;
                        }
                    }
                    Err(rectified_err) => {
                        // The repair was its own attempt and it failed; that
                        // attempt was settled where the send happened. The
                        // canonical classification of that failure is what the
                        // router and the outcome both record.
                        let failure = rectified_err.classified();
                        state.router.record_classified_attempt(
                            candidate.model_id(),
                            &candidate.provider.id,
                            &failure,
                            lifecycle.routing(),
                        );
                        tracing::warn!(
                            model = candidate.model_id(),
                            provider = candidate.provider.name.as_str(),
                            "rectifier retry failed: {rectified_err}"
                        );
                        let keep_going = state.router.should_fallback_failure(&failure);
                        last_error = rectified_err;
                        if !keep_going {
                            break;
                        }
                    }
                }
            }
        }
    }

    lifecycle.finalize(TerminalKind::failed(&last_error), Settlement::default());
    error_response(dialect, &last_error)
}

#[allow(clippy::too_many_arguments)]
async fn stream_chat(
    state: Arc<AppState>,
    dialect: Dialect,
    req: ChatRequest,
    plan: Vec<Candidate>,
    kind: RequestKind,
    include_usage: bool,
    input_items: Vec<Value>,
    previous_response_id: Option<String>,
    storage: protocol::responses::StoragePolicy,
    routing_decision: Option<RouteDecision>,
    admission: AdmissionGuard,
    #[cfg(feature = "ml")] shadow_input: Option<ShadowInput>,
) -> Response {
    let mut lifecycle = RequestLifecycle::new(
        Arc::clone(&state),
        dialect,
        true,
        kind,
        &req.model,
        &req.client_headers,
        #[cfg(feature = "ml")]
        Some(req.clone()),
    );
    // Same correlation as the buffered path: the router's decision fixes the
    // planned identity before the first handshake.
    lifecycle.planned_from(routing_decision.as_ref(), &plan);
    // shadow-block-begin
    // Same record-only shadow hook as the buffered path: before the attempt
    // loop, over the pre-attempt snapshot, verdict discarded, record identity
    // retained for the terminal transition.
    #[cfg(feature = "ml")]
    if let Some(input) = &shadow_input {
        lifecycle.shadow_evaluated(input);
    }
    // shadow-block-end
    let mode = encode_mode_for(kind);
    let mut last_error = Error::NoCandidate(req.model.clone());

    for (index, candidate) in plan.iter().enumerate() {
        // A later candidate means this failure is failed over, not waited on.
        let can_fail_over = index + 1 < plan.len();
        lifecycle.attempted(candidate);
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
        if state.config().vision.enabled && !candidate.entry.capabilities.vision {
            let needs_vision = crate::media::collect::collect(&req);
            if !needs_vision.is_empty() {
                apply_vision_fallback(&state, &mut req, "preflight").await;
            }
        }

        let (key, body) = match prepare(&state, candidate, &req, mode) {
            Ok(v) => v,
            Err(e) => {
                lifecycle.begin_attempt(candidate, false);
                lifecycle.failed_attempt(attempt_start, &e);
                state.router.record_classified_attempt(
                    candidate.model_id(),
                    &candidate.provider.id,
                    &e.classified(),
                    lifecycle.routing(),
                );
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
            // Nothing was sent and nothing failed: no attempt evidence.
            continue;
        }

        // Only the handshake can be retried; once bytes are flowing the client
        // has already seen part of the answer. The attempt stays open: its
        // verdict is the stream's terminal state, not the handshake.
        lifecycle.begin_attempt(candidate, false);
        let mut client = client_for(&req, &candidate.provider);
        let key = match console_key(&state, candidate, key, &mut client).await {
            Ok(key) => key,
            Err(e) => {
                lifecycle.failed_attempt(attempt_start, &e);
                state.router.record_classified_attempt(
                    candidate.model_id(),
                    &candidate.provider.id,
                    &e.classified(),
                    lifecycle.routing(),
                );
                last_error = e;
                continue;
            }
        };
        match state
            .upstream
            .stream_with_identity_policy(
                &candidate.provider,
                key.as_deref(),
                &body,
                &candidate.entry.upstream_model,
                &client,
                can_fail_over,
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
                    lifecycle.routing(),
                );
                // For streaming, handshake time approximates TTFT.
                let handshake_ms = attempt_start.elapsed().as_millis() as f64;
                state.router.record_classified_outcome(
                    candidate.model_id(),
                    &candidate.provider.id,
                    handshake_ms,
                    Some(handshake_ms),
                    true,
                    None,
                );
                let (response_id, cancel_rx) = register_in_flight(&state, dialect, storage);
                return stream_response(
                    &state,
                    candidate,
                    kind,
                    dialect,
                    include_usage,
                    lifecycle,
                    maybe_filter_stream(events, &client, &req),
                    storage,
                    response_id,
                    cancel_rx,
                    input_items.clone(),
                    previous_response_id.clone(),
                    admission,
                );
            }
            Err(e) => {
                // The handshake that just failed is over: settle its attempt
                // before any repair handshake opens one of its own, so every
                // upstream send has exactly one complete attempt entry.
                lifecycle.failed_attempt(attempt_start, &e);
                // Rectifier cascade for handshake failures. A successful repaired
                // stream is served to the client without reporting health.
                match try_rectify_stream(
                    &state,
                    candidate,
                    key.as_deref(),
                    &mut req,
                    &e,
                    &mut lifecycle,
                )
                .await
                {
                    Ok(Some(events)) => {
                        // The repaired stream is not a fresh half-open probe.
                        state.router.release_half_open_permit(candidate.model_id());
                        // The repair handshake opened its own attempt inside the
                        // cascade; the stream's terminal state settles it.
                        let (response_id, cancel_rx) = register_in_flight(&state, dialect, storage);
                        return stream_response(
                            &state,
                            candidate,
                            kind,
                            dialect,
                            include_usage,
                            lifecycle,
                            events,
                            storage,
                            response_id,
                            cancel_rx,
                            input_items,
                            previous_response_id,
                            admission,
                        );
                    }
                    Ok(None) => {
                        // No repair was applied; the settled attempt above is
                        // this candidate's contribution.
                        let failure = e.classified();
                        state.router.record_classified_attempt(
                            candidate.model_id(),
                            &candidate.provider.id,
                            &failure,
                            lifecycle.routing(),
                        );
                        tracing::warn!(
                            model = candidate.model_id(),
                            "upstream stream handshake failed: {e}"
                        );
                        let keep_going = state.router.should_fallback_failure(&failure);
                        last_error = e;
                        if !keep_going {
                            break;
                        }
                    }
                    Err(rectified_err) => {
                        // Every repair handshake was its own attempt and each
                        // failure was settled where the send happened.
                        let failure = rectified_err.classified();
                        state.router.record_classified_attempt(
                            candidate.model_id(),
                            &candidate.provider.id,
                            &failure,
                            lifecycle.routing(),
                        );
                        tracing::warn!(
                            model = candidate.model_id(),
                            "rectifier stream retry failed: {rectified_err}"
                        );
                        let keep_going = state.router.should_fallback_failure(&failure);
                        last_error = rectified_err;
                        if !keep_going {
                            break;
                        }
                    }
                }
            }
        }
    }

    lifecycle.finalize(TerminalKind::failed(&last_error), Settlement::default());
    error_response(dialect, &last_error)
}

/// Resolve the routing policy for this request.
///
/// Resolution order:
/// 1. Client profile match (first matching profile's `policy_id`)
/// 2. Policy matchers (first policy whose matchers all pass)
/// 3. Default policy (by `default_policy` id, or the built-in default)
fn resolve_policy<'a>(
    config: &'a policy::PolicyConfig,
    client_ctx: &ClientContext,
    task: &policy::TaskProfile,
) -> Option<Cow<'a, RoutingPolicy>> {
    // Step 1: Try client profiles.
    if let Some(profile) = policy::resolve_client(&config.clients, client_ctx) {
        if let Some(policy) = config
            .policies
            .iter()
            .find(|p| p.id == profile.policy_id && p.enabled)
        {
            tracing::debug!(
                client_profile = %profile.id,
                policy_id = %policy.id,
                "policy resolved via client profile"
            );
            return Some(Cow::Borrowed(policy));
        }
    }

    // Step 2: Try policy matchers.
    let match_ctx = policy::MatchContext {
        client_id: client_ctx.client_id,
        application: client_ctx.application,
        model: client_ctx.model,
        streaming: task.streaming,
        has_tools: task.has_tools,
        has_vision: task.has_vision,
        task: Some(task),
    };
    for policy in &config.policies {
        if policy.matches(&match_ctx) {
            tracing::debug!(
                policy_id = %policy.id,
                "policy resolved via matchers"
            );
            return Some(Cow::Borrowed(policy));
        }
    }

    // Step 3: Fall back to default policy.
    if let Some(ref default_id) = config.default_policy {
        if let Some(policy) = config
            .policies
            .iter()
            .find(|p| p.id == *default_id && p.enabled)
        {
            tracing::debug!(
                policy_id = %policy.id,
                "policy resolved via default_policy config"
            );
            return Some(Cow::Borrowed(policy));
        }
    }

    // Built-in default when nothing is configured.
    Some(Cow::Owned(policy::default_policy()))
}
/// Record a request that ended before any candidate was tried.
///
/// A budget denial, an unresolvable model and a pool with no candidate all land
/// here, and each of them is still a real request: one activity record, one
/// outcome, one canonical classification — and no router accounting, because no
/// provider was involved.
fn reject(
    state: Arc<AppState>,
    dialect: Dialect,
    streaming: bool,
    kind: RequestKind,
    requested_model: &str,
    headers: &HeaderMap,
    error: &Error,
) {
    let mut lifecycle = RequestLifecycle::new(
        state,
        dialect,
        streaming,
        kind,
        requested_model,
        headers,
        #[cfg(feature = "ml")]
        None,
    );
    lifecycle.finalize(TerminalKind::failed(error), Settlement::default());
}
