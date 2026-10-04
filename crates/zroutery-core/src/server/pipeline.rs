//! The request pipeline: walk the router's plan, talk to a provider, and turn the
//! answer back into the client's dialect.
//!
//! Split from the routing table because the two change for different reasons. That
//! file is about which paths exist; this one is about what happens once a request
//! is on one of them.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use tokio::sync::watch;

use super::{error_response, AdmissionGuard, AppState};
use crate::billing::{Cost, Pricing};
use crate::budget::{Budget, BudgetScope, OnExceeded, Verdict};
use crate::config::{AppConfig, ModelTier, RoutingConfig};
use crate::error::{Error, Result};
use crate::failure::ClassifiedFailure;
use crate::ir::{
    ChatRequest, Dialect, Message, ResponseStatus, StoredResponse, StreamEvent, Usage,
};
#[cfg(feature = "ml")]
use crate::ml::ShadowInput;
use crate::outcome::{Attempt as OutcomeAttempt, CandidateIdentity, Outcome};
use crate::policy::{self, ClientContext, RouteDecision, RoutingPolicy};
use crate::protocol::{self, openai, SseFrame, StreamEncoder};
use crate::query::RequestKind;
use crate::rectifier::media_fallback::MediaFallbackRectifier;
use crate::rectifier::{self, Rectifier};
use crate::registry::{Registry, Resolution};
use crate::router::Candidate;
use crate::stats::{RecordBuilder, RequestRecord};
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

/// ml-ranking-helper-begin
/// Re-order one request's executable plan with the learned model, if one can rank
/// it.
///
/// Returns the plan and the decision unchanged when nothing can be ranked. That
/// is the normal case in a deployment that has never promoted a model, and the
/// case whenever the model is attached but unavailable, so it is written as an
/// ordinary return rather than as an error the caller has to remember to catch.
#[cfg(feature = "ml")]
fn apply_ml_ranking(
    state: &Arc<AppState>,
    plan: Vec<Candidate>,
    routing_decision: Option<RouteDecision>,
    snapshot: Option<crate::ml::ShadowInput>,
) -> (Vec<Candidate>, Option<RouteDecision>) {
    let untouched = || (plan.clone(), routing_decision.clone());

    let (Some(decision), Some(snapshot)) = (routing_decision.clone(), snapshot) else {
        // A direct-resolution or classifier request carries no routing decision,
        // so there is no policy evidence to rank against. Reordering it on
        // features alone would put the model in charge of a request the router
        // never decided, which is a different product decision.
        return untouched();
    };
    if plan.len() < 2 {
        return untouched();
    }
    let request_id = decision.decision_id.clone();
    if !state.ml_routing().is_attached() {
        // No model, but exploration still applies — and this is the only place it
        // can. A model cannot be trained on evidence the router never gathered,
        // so if exploring required a promoted model the dataset could only ever
        // contain candidates the incumbent plan already reached, and a provider
        // added at the bottom of the priority order would be invisible forever.
        return apply_exploration_without_a_model(state, plan, decision, &snapshot, &request_id);
    }
    let ranked = match state.ml_routing().rank(&snapshot, &request_id) {
        Ok(ranked) => ranked,
        Err(reason) => {
            tracing::debug!(
                decision_id = %decision.decision_id,
                reason = ?reason,
                "the learned model could not rank this request; the deterministic plan stands"
            );
            return untouched();
        }
    };

    let plan_order: Vec<String> = plan.iter().map(|c| c.exposed_id.clone()).collect();
    let applied = crate::ml::AppliedRanking::apply(&ranked, &plan_order);
    if !applied.changed {
        return untouched();
    }

    // Rebuild the plan in the model's order. Only identities present in the
    // original plan are looked up, so the set of servable candidates is
    // unchanged and only the order differs.
    let mut reordered: Vec<Candidate> = Vec::with_capacity(plan.len());
    for id in &applied.order {
        if let Some(candidate) = plan.iter().find(|c| &c.exposed_id == id) {
            reordered.push(candidate.clone());
        }
    }
    if reordered.len() != plan.len() {
        // The ranking named something the plan does not contain in a way that
        // lost a candidate. Refuse the whole ranking rather than serve a plan
        // the router never produced.
        tracing::warn!(
            decision_id = %decision.decision_id,
            planned = plan.len(),
            reordered = reordered.len(),
            "the learned ranking did not cover the executable plan; the deterministic plan stands"
        );
        return untouched();
    }

    // Amend the decision so the recorded planned identity is the one that will
    // actually be tried, and so the model's influence is visible in the decision
    // record rather than only in a log line.
    let amended = routing_decision.map(|mut decision| {
        decision.selected = Some(applied.selected.clone());
        decision.ml_ranking = Some(policy::MlRankingTrace {
            commit_id: applied.commit_id.clone(),
            selected: applied.selected.clone(),
            reason: applied.reason.clone(),
            explored: applied.explored,
            modelled: true,
            order: applied.order.clone(),
            baseline_order: plan_order.clone(),
        });
        decision
    });

    tracing::debug!(
        decision_id = %decision.decision_id,
        commit_id = %applied.commit_id,
        selected = %applied.selected,
        explored = applied.explored,
        reason = %applied.reason,
        "the learned model re-ordered the provider plan"
    );

    (reordered, amended)
}

/// Try an eligible candidate the plan would not have reached, with no model.
///
/// The counterpart to [`apply_ml_ranking`] for the state before anything has been
/// promoted. It reorders within the executable plan exactly as the model path
/// does, and it records `modelled: false` so nothing downstream can read a
/// deliberate experiment as a decision the model made.
#[cfg(feature = "ml")]
fn apply_exploration_without_a_model(
    state: &Arc<AppState>,
    plan: Vec<Candidate>,
    decision: RouteDecision,
    snapshot: &crate::ml::ShadowInput,
    request_id: &str,
) -> (Vec<Candidate>, Option<RouteDecision>) {
    let Some(explored) = state.ml_routing().explore_plan(snapshot, request_id) else {
        return (plan.clone(), Some(decision));
    };
    if !plan.iter().any(|c| c.exposed_id == explored.selected) {
        // The draw named something the executable plan does not contain. Refuse
        // it rather than serve a candidate the router never planned.
        tracing::warn!(
            decision_id = %request_id,
            selected = %explored.selected,
            "the exploration draw named a candidate outside the executable plan; \
             the deterministic plan stands"
        );
        return (plan, Some(decision));
    }

    let mut order: Vec<String> = plan
        .iter()
        .map(|candidate| candidate.exposed_id.clone())
        .collect();
    // Move the explored candidate to the front, keeping everything else's
    // relative order. The set of servable candidates is untouched.
    order.retain(|id| id != &explored.selected);
    order.insert(0, explored.selected.clone());
    let mut reordered: Vec<Candidate> = Vec::with_capacity(plan.len());
    for id in &order {
        if let Some(candidate) = plan.iter().find(|c| &c.exposed_id == id) {
            reordered.push(candidate.clone());
        }
    }
    if reordered.len() != plan.len() {
        return (plan.clone(), Some(decision));
    }

    let plan_order: Vec<String> = plan.iter().map(|c| c.exposed_id.clone()).collect();
    let amended = {
        let mut decision = decision;
        decision.selected = Some(explored.selected.clone());
        decision.ml_ranking = Some(policy::MlRankingTrace {
            // No model, so no commit. Empty rather than a placeholder string,
            // because a field that can hold a fake commit is a field that will.
            commit_id: String::new(),
            selected: explored.selected.clone(),
            reason: explored.reason.clone(),
            explored: true,
            modelled: false,
            order: order.clone(),
            baseline_order: plan_order,
        });
        decision
    };

    tracing::debug!(
        decision_id = %request_id,
        selected = %explored.selected,
        "explored a candidate with no model attached, to gather evidence about it"
    );

    (reordered, Some(amended))
}
// ml-ranking-helper-end

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

/// Apply the spending limits to a resolved request.
///
/// The check is "have I already spent it", not "will this request spend it", because
/// the cost is only known once a request has finished. What bounds the overshoot at
/// one request instead of at one request *per racing caller* is admission: every
/// scope with a configured limit that this request could spend against is taken as
/// a permit before the check, in canonical order, and returned with the resolution
/// so the caller can hold it until the request has been settled and charged. A
/// request that arrives while another is in flight for the same scope waits, then
/// reads the ledger the finished request already wrote.
///
/// A degrade is followed at most once per tier: the cheaper tier's own budget still
/// applies, so this cannot be used to route around a limit, and a cycle of degrades
/// ends in a refusal rather than a loop. The permit set is computed over the whole
/// degrade closure, so the tier a request is degraded to is already held.
async fn apply_budgets(
    state: &AppState,
    registry: &Registry,
    resolution: Resolution,
) -> Result<(Resolution, AdmissionGuard)> {
    let admission = state
        .admit(&main_admission_scopes(
            registry.config(),
            registry,
            &resolution,
        ))
        .await;
    let mut resolution = resolution;
    let mut visited: Vec<ModelTier> = Vec::new();

    loop {
        // The tier the request will be billed under. Direct ids participate
        // too: charging uses the target model's tier, so a tier budget must
        // also gate requests that reach the tier by exact id, or the limit
        // could be spent around while still being charged.
        let tier = match &resolution {
            Resolution::Tier(tier) => Some(*tier),
            Resolution::Direct(id) => registry.entry(id).ok().and_then(|m| m.tier),
        };
        // Every provider the request could land on, because a provider's budget has
        // to stop a request that might reach it.
        let provider_ids: Vec<String> = match &resolution {
            Resolution::Tier(tier) => registry
                .tier_members(*tier)
                .iter()
                .map(|m| m.provider_id.clone())
                .collect(),
            Resolution::Direct(id) => registry
                .entry(id)
                .map(|m| vec![m.provider_id.clone()])
                .unwrap_or_default(),
        };

        match state.budget_verdict(&provider_ids, tier) {
            Verdict::Allow => return Ok((resolution, admission)),
            Verdict::Reject { because } => return Err(Error::OverBudget(because)),
            Verdict::Degrade { to, because } => {
                if let Some(current) = tier {
                    visited.push(current);
                }
                if visited.contains(&to) {
                    // Following this would come back here, so refusing is the end.
                    return Err(Error::OverBudget(format!(
                        "{because}, and the tier it degrades to is over its own limit"
                    )));
                }
                tracing::info!("degrading to {}: {because}", to.virtual_id());
                resolution = Resolution::Tier(to);
            }
        }
    }
}

/// Every budgeted scope a main request could spend against, in canonical order.
///
/// That is the global scope, the providers the resolution it starts from could
/// land on, the tier it starts at, and — because a covering tier budget can
/// degrade the request to another tier whose own scope then applies — the
/// closure of those degrade targets. Taking the whole set before the first check
/// is what makes the acquisition order total: each request accumulates a subset
/// of the same ordered gates ascending, so two requests cannot deadlock.
///
/// Only scopes that actually have an enabled budget are listed. A scope with no
/// limit has no gate, so traffic no budget covers is never serialised.
fn main_admission_scopes(
    config: &AppConfig,
    registry: &Registry,
    resolution: &Resolution,
) -> Vec<BudgetScope> {
    let budgets: Vec<&Budget> = config.budgets.iter().filter(|b| b.enabled).collect();
    if budgets.is_empty() {
        return Vec::new();
    }
    let budgeted = |scope: &BudgetScope| budgets.iter().any(|b| &b.scope == scope);

    let mut scopes: BTreeSet<BudgetScope> = BTreeSet::new();
    if budgets
        .iter()
        .any(|b| matches!(b.scope, BudgetScope::Global))
    {
        scopes.insert(BudgetScope::Global);
    }

    let mut pending = vec![resolution.clone()];
    let mut seen_tiers: BTreeSet<ModelTier> = BTreeSet::new();
    let mut seen_direct: BTreeSet<String> = BTreeSet::new();
    while let Some(next) = pending.pop() {
        let (providers, tier) = match &next {
            Resolution::Tier(tier) => (
                registry
                    .tier_members(*tier)
                    .iter()
                    .map(|m| m.provider_id.clone())
                    .collect::<Vec<_>>(),
                Some(*tier),
            ),
            Resolution::Direct(id) => {
                if !seen_direct.insert(id.clone()) {
                    continue;
                }
                match registry.entry(id) {
                    Ok(entry) => (vec![entry.provider_id.clone()], entry.tier),
                    Err(_) => (Vec::new(), None),
                }
            }
        };
        for provider in providers {
            let scope = BudgetScope::Provider { id: provider };
            if budgeted(&scope) {
                scopes.insert(scope);
            }
        }
        let Some(tier) = tier else { continue };
        let tier_scope = BudgetScope::Tier { tier };
        if budgeted(&tier_scope) {
            scopes.insert(tier_scope.clone());
        }
        if !seen_tiers.insert(tier) {
            continue;
        }
        for budget in &budgets {
            if budget.scope == tier_scope {
                if let OnExceeded::Degrade { to } = &budget.on_exceeded {
                    pending.push(Resolution::Tier(*to));
                }
            }
        }
    }
    scopes.into_iter().collect()
}

/// Every budgeted scope a classifier side request could spend against.
///
/// A side request is billed like any other, so global and provider limits admit
/// it, and the permits cover every provider in the pool it could fail over to so
/// a failover cannot escape the set. The class (tier) scope is deliberately
/// excluded, matching [`AppState::classifier_budget_verdict`]: the pool is not
/// chosen by tier policy, so a class budget neither degrades nor rejects a side
/// request.
fn classifier_admission_scopes(config: &AppConfig, plan: &[Candidate]) -> Vec<BudgetScope> {
    let mut scopes = BTreeSet::new();
    for budget in config.budgets.iter().filter(|b| b.enabled) {
        match &budget.scope {
            BudgetScope::Global => {
                scopes.insert(BudgetScope::Global);
            }
            BudgetScope::Provider { id } if plan.iter().any(|c| &c.provider.id == id) => {
                scopes.insert(BudgetScope::Provider { id: id.clone() });
            }
            _ => {}
        }
    }
    scopes.into_iter().collect()
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
        let outcome =
            crate::media::vision::describe(&state.upstream, &target, key.as_deref(), source).await;
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

/// Try every enabled rectifier, allowing each rectifier up to three repair
/// rounds. A rectifier retry is deliberately not reported to the circuit
/// breaker: it is the same provider and same model, and the failure was a
/// fixable request-shape problem.
///
/// A media rejection is upgraded before the ordinary cascade runs: instead of
/// a placeholder, the images get described by the vision model (falling back
/// to the placeholder when it fails), and the repaired request retries the
/// same provider — the upstream rejected the *image*, not the question.
///
/// Every actual upstream send opens its own attempt before it leaves and
/// settles that attempt if it fails. A send that succeeds is left open
/// deliberately: the caller owns the response that follows (a classifier
/// verdict still has to be checked), so it settles the attempt with what the
/// answer turned out to be. Nothing is reported to router health here.
async fn try_rectify_buffered(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    req: &mut ChatRequest,
    error: &Error,
    lifecycle: &mut RequestLifecycle,
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
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
            match state
                .upstream
                .send(&candidate.provider, key_owned.as_deref(), &body)
                .await
            {
                Ok(resp) => return Ok(Some(resp)),
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
                    tracing::warn!(
                        model = candidate.model_id(),
                        "vision-repaired retry also failed: {e}"
                    );
                }
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
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
            match state
                .upstream
                .send(&candidate.provider, key, &modified)
                .await
            {
                Ok(resp) => return Ok(Some(resp)),
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
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

/// The streaming counterpart of [`try_rectify_buffered`].
///
/// Each repair send opens its own attempt before the handshake leaves. A
/// handshake that fails settles that attempt here; a handshake that succeeds
/// leaves it open, because for a stream the attempt's verdict is the stream's
/// terminal state and only the body stream knows it. Nothing is reported to
/// router health: the repair is the same provider and model, and the original
/// failure was a fixable request shape rather than evidence about the model.
async fn try_rectify_stream(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    req: &mut ChatRequest,
    error: &Error,
    lifecycle: &mut RequestLifecycle,
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
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
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
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
                    tracing::warn!(
                        model = candidate.model_id(),
                        "vision-repaired stream retry also failed: {e}"
                    );
                }
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
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
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
                    lifecycle.failed_attempt(started, &e);
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
    let mut lifecycle = RequestLifecycle::new(Arc::clone(&state), dialect, false, kind, &req.model);
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

    for candidate in &plan {
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
                            let unusable = Error::BadUpstreamPayload(format!(
                                "classifier response from `{}` carried no <block> verdict",
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
                                "classifier response had no <block> verdict; failing over"
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
                            && crate::classifier::parse_verdict(&resp.text())
                                == crate::classifier::ClassifierVerdict::Unparseable
                        {
                            tracing::warn!(
                                kind = kind.as_str(),
                                candidate_model = candidate.model_id(),
                                "rectified classifier response still had no <block> verdict; failing over"
                            );
                            let verdict = Error::BadUpstreamPayload(format!(
                                "classifier response from `{}` carried no <block> verdict",
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
    let mut lifecycle = RequestLifecycle::new(Arc::clone(&state), dialect, true, kind, &req.model);
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

    for candidate in &plan {
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
                    events,
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

/// Claim a Responses API id for an in-flight stream, if this dialect has one.
fn register_in_flight(
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
fn stream_response(
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
    response
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

/// Build a [`ClientContext`] from the request headers and model.
///
/// Extracts the User-Agent, any `x-zroutery-client` header, and uses the
/// pre-collected header pairs for policy matching.
fn build_client_context<'a>(
    headers: &'a HeaderMap,
    model: &'a str,
    header_pairs: &'a [(&'a str, &'a str)],
) -> ClientContext<'a> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());

    let client_id = headers
        .get("x-zroutery-client")
        .and_then(|v| v.to_str().ok());

    ClientContext {
        client_id,
        user_agent,
        api_key_prefix: None,
        application: headers
            .get("x-zroutery-application")
            .and_then(|v| v.to_str().ok()),
        headers: header_pairs,
        model,
    }
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

/// The status an activity record carries for a request whose client went away.
///
/// 499 is the conventional code for "the client closed the request": there is no
/// HTTP response to quote, and the terminal state is exactly the point.
const CLIENT_CLOSED_REQUEST: u16 = 499;
const DROPPED_MID_STREAM: &str = "client disconnected before the stream completed";
const DROPPED_BEFORE_OUTPUT: &str = "client disconnected before any output was produced";
const CANCELLED_BY_CLIENT: &str = "cancelled by the client";

/// The usage and price facts known when a request reaches its terminal state.
///
/// A buffered answer has both from the moment the response arrives; a stream
/// only learns its usage at the end, which is why this is settled at the
/// terminal transition rather than during the attempt.
#[derive(Default)]
struct Settlement {
    usage: Usage,
    pricing: Option<Pricing>,
    /// Who to bill. Empty means nothing is billable, which is the honest state
    /// of a request that never produced usage.
    provider_id: String,
    tier: Option<ModelTier>,
}

/// Why a request reached its terminal state.
///
/// Chosen once, by whichever part of the pipeline actually observed the end, and
/// never re-derived from a message or a status code: every classification here
/// comes from [`Error::classified`], and the two states an `Error` cannot
/// express — a client that vanished, and a client that cancelled — are named
/// explicitly rather than smuggled in as a success.
enum TerminalKind {
    /// The upstream answered, and that answer was delivered to the client.
    Served,
    /// The request failed. The class is the canonical one.
    Failed {
        failure: ClassifiedFailure,
        /// The message for the activity log, taken from the error's safe form.
        /// `Error::to_string()` is deliberately not used: it embeds the
        /// provider's response body, whose shape — HTML, JSON, plain text — no
        /// string rule can be trusted to recognise.
        message: String,
    },
    /// The client went away before the answer was finished. `emitted` says
    /// whether any answer byte had already been produced: a drop after output
    /// truncated the answer (interrupted), a drop before the first byte simply
    /// cancelled it.
    ClientDisconnected { emitted: bool },
    /// The client asked for this request to be cancelled.
    ClientCancelled,
}

impl TerminalKind {
    /// A request that failed, with the canonical classification of `error`.
    ///
    /// The activity message comes from [`Error::safe_message`], the structural
    /// redaction, rather than the `Display` text: the latter carries whatever
    /// the provider answered with, and that body can be HTML, JSON or plain
    /// text indifferently.
    fn failed(error: &Error) -> Self {
        TerminalKind::Failed {
            failure: error.classified(),
            message: error.safe_message(),
        }
    }

    /// The canonical classification of this terminal state.
    ///
    /// `None` only for a served request: a delivered answer is not a failure to
    /// classify. The cancellation and interruption facts come from the accepted
    /// constructors, so the impact table — not this file — decides what they
    /// mean for health, retry and stats.
    fn classified(&self) -> Option<ClassifiedFailure> {
        match self {
            TerminalKind::Served => None,
            TerminalKind::Failed { failure, .. } => Some(failure.clone()),
            TerminalKind::ClientDisconnected { emitted: true } => {
                Some(ClassifiedFailure::interrupted(DROPPED_MID_STREAM))
            }
            TerminalKind::ClientDisconnected { emitted: false } => {
                Some(ClassifiedFailure::cancelled(DROPPED_BEFORE_OUTPUT))
            }
            TerminalKind::ClientCancelled => {
                Some(ClassifiedFailure::cancelled(CANCELLED_BY_CLIENT))
            }
        }
    }

    fn is_served(&self) -> bool {
        matches!(self, TerminalKind::Served)
    }

    /// The status the activity record shows for this terminal state.
    fn record_status(&self) -> u16 {
        match self {
            TerminalKind::Served => 200,
            TerminalKind::Failed { failure, .. } => failure
                .status
                .unwrap_or_else(|| axum::http::StatusCode::INTERNAL_SERVER_ERROR.as_u16()),
            TerminalKind::ClientDisconnected { .. } | TerminalKind::ClientCancelled => {
                CLIENT_CLOSED_REQUEST
            }
        }
    }

    /// The message the activity record shows for this terminal state.
    fn record_message(&self) -> String {
        match self {
            TerminalKind::Served => String::new(),
            TerminalKind::Failed { message, .. } => message.clone(),
            other => other
                .classified()
                .map(|failure| failure.message)
                .unwrap_or_default(),
        }
    }
}

/// The identity of a candidate, as the outcome records it.
fn identity_of(candidate: &Candidate) -> CandidateIdentity {
    CandidateIdentity::new(candidate.model_id(), candidate.provider.id.clone())
}

/// The identity the router selected before any attempt, resolved in one place.
///
/// A policy-routed request carries the router's own evidence, so its decision
/// is the authority. A direct or classifier request has no decision object, and
/// the router's selection is then the head of the plan it returned. Nothing
/// downstream re-derives this: a served identity is never a stand-in for the
/// planned one, and vice versa.
fn planned_identity(
    decision: Option<&RouteDecision>,
    plan: &[Candidate],
) -> Option<CandidateIdentity> {
    if let Some(planned) = decision.and_then(RouteDecision::planned_identity) {
        return Some(CandidateIdentity::new(
            planned.model_id,
            planned.provider_id,
        ));
    }
    plan.first().map(identity_of)
}

/// The wire name of the dialect the client spoke, as the outcome records it.
fn dialect_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Anthropic => "anthropic",
        Dialect::OpenAI => "openai",
        Dialect::OpenAIResponses => "openai_responses",
        Dialect::Gemini => "gemini",
    }
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
    error: &Error,
) {
    let mut lifecycle = RequestLifecycle::new(state, dialect, streaming, kind, requested_model);
    lifecycle.finalize(TerminalKind::failed(error), Settlement::default());
}

/// Everything one client request needs to reach exactly one terminal state.
///
/// This is the lifecycle's single writer. It owns the activity record, the
/// attempt evidence and the router accounting for a request, and it builds that
/// request's one [`Outcome`]. Attempt-level health is still reported where the
/// attempt happens, because that is the signal a routing decision can act on;
/// but the terminal transition itself is recorded here, once, whichever path
/// reached it: a delivered answer, an upstream error, a client disconnect, or a
/// cancellation.
struct RequestLifecycle {
    state: Arc<AppState>,
    record: RecordBuilder,
    routing: RoutingConfig,
    dialect: Dialect,
    streaming: bool,
    started: Instant,
    /// The decision trace, kept for the record, the stored response and the
    /// outcome's decision id.
    decision: Option<RouteDecision>,
    /// Selected before any attempt, from the router's own evidence.
    planned: Option<CandidateIdentity>,
    /// One entry per send the pipeline observed, in order.
    attempts: Vec<OutcomeAttempt>,
    /// The candidate whose response was actually delivered, if any.
    served: Option<CandidateIdentity>,
    /// Response store id, for the Responses API lifecycle.
    response_id: Option<String>,
    /// Set once an attempt has failed inside the loop, where the router adapters
    /// accounted for that failure at the attempt. The terminal transition must
    /// not report the same failure a second time: the loop's last error usually
    /// *is* the terminal error.
    attempt_failure_accounted: bool,
    /// Identity of the shadow record this request produced at decision time.
    /// The record itself — and with it the exact decision-time input it was
    /// computed from — stays in the store; only the key travels, so the
    /// terminal transition attaches the served identity to *that* record
    /// instead of rebuilding one from whatever the request looks like later.
    #[cfg(feature = "ml")]
    shadow_id: Option<String>,
    /// The decision-time input this request's shadow record was accepted with.
    ///
    /// A sample's features have to be the ones captured at decision time, and
    /// they are only captured at all when a record was accepted for this
    /// request. The hook therefore retains the snapshot *only* on the path
    /// where `evaluate` returned a record, so the dataset can never be a second
    /// source of features: with no retained record there is no input here either.
    ///
    /// The stronger form of this correlation — re-reading the record out of the
    /// shadow store by request id at the terminal transition — needs a
    /// read accessor this crate does not expose yet.
    #[cfg(feature = "ml")]
    decision_time: Option<ShadowInput>,
    /// The exactly-once guard for the whole request.
    terminal: bool,
    /// The exactly-once guard for dataset ingestion. It is redundant with
    /// `terminal` on purpose: the dataset's contract is "one ingestion per
    /// request", and a redundant guard is cheaper than discovering otherwise.
    #[cfg(feature = "ml")]
    ingested: bool,
    /// The exactly-once guard for the durable trace write, on the same terms as
    /// `ingested`.
    #[cfg(feature = "ml")]
    trace_written: bool,
    /// The exactly-once guard for the observability projection, for the same
    /// reason and on the same terms as `ingested`.
    projected: bool,
}

impl RequestLifecycle {
    fn new(
        state: Arc<AppState>,
        dialect: Dialect,
        streaming: bool,
        kind: RequestKind,
        requested_model: &str,
    ) -> Self {
        let mut record = RecordBuilder::new(dialect, requested_model, streaming);
        record.kind(kind);
        let routing = state.config().routing.clone();
        RequestLifecycle {
            state,
            record,
            routing,
            dialect,
            streaming,
            started: Instant::now(),
            decision: None,
            planned: None,
            attempts: Vec::new(),
            served: None,
            response_id: None,
            attempt_failure_accounted: false,
            #[cfg(feature = "ml")]
            shadow_id: None,
            #[cfg(feature = "ml")]
            decision_time: None,
            terminal: false,
            #[cfg(feature = "ml")]
            ingested: false,
            #[cfg(feature = "ml")]
            trace_written: false,
            projected: false,
        }
    }

    /// The request id the activity record, the outcome and the shadow
    /// evaluation all correlate on.
    #[cfg_attr(not(feature = "ml"), allow(dead_code))]
    fn id(&self) -> &str {
        self.record.id()
    }

    /// Whether this request already reached its terminal transition.
    fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// How long this request has been running, for the record's own fields.
    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Stamp time to first token, once.
    fn ttft_ms(&mut self, ms: u64) {
        self.record.ttft(ms);
    }

    /// The routing config the router's health adapters need.
    fn routing(&self) -> &RoutingConfig {
        &self.routing
    }

    /// The decision trace, for the Responses API record.
    fn decision(&self) -> Option<RouteDecision> {
        self.decision.clone()
    }

    /// Note the response store id this request is served under.
    fn note_response_id(&mut self, id: &str) {
        self.response_id = Some(id.to_string());
    }

    /// Fix the planned identity and the decision trace, before any attempt.
    fn planned_from(&mut self, decision: Option<&RouteDecision>, plan: &[Candidate]) {
        self.planned = planned_identity(decision, plan);
        if let Some(decision) = decision {
            self.decision = Some(decision.clone());
            self.record.routing_decision(decision.clone());
        }
    }

    /// Evaluate this request's shadow counterfactual over the decision-time
    /// snapshot and remember which record it produced.
    ///
    /// Strictly record-only. The engine absorbs its own faults and reports
    /// them through its own counter, so there is nothing here to handle: a
    /// disabled engine, a fault, or a refused record all simply mean this
    /// request has no shadow record to correlate later. The verdict is never
    /// read back, and the snapshot is not retained here — the record holds it.
    ///
    /// Which predictor runs is the one question this hook now answers, and the
    /// answer is recorded in the record itself: `ShadowVerdict::model_commit`
    /// names the commit that produced it. When a verified candidate is
    /// attached, every record therefore names that candidate; when the
    /// candidate is withdrawn, every record names the engine's own cold-start
    /// predictor, which is the pre-7F behaviour. Either way the return value is
    /// discarded and nothing downstream of this line can reach a response.
    #[cfg(feature = "ml")]
    fn shadow_evaluated(&mut self, input: &ShadowInput) {
        if self.shadow_id.is_some() {
            return;
        }
        let decision = self.state.shadow_evaluated(self.id(), input);
        let Some(decision) = decision else {
            return;
        };
        tracing::debug!(
            request_id = %self.id(),
            shadow_id = %decision.shadow_id,
            model_commit = %decision.shadow.model_commit,
            selected = %decision.shadow.selected,
            "shadow counterfactual recorded"
        );
        // A returned record is the proof that the store accepted this input, so
        // the same snapshot can be handed to dataset ingestion later. Nothing is
        // re-derived here: these are the very values the record holds.
        self.shadow_id = Some(decision.shadow_id);
        self.decision_time = Some(input.clone());
    }

    /// Attach what actually served to this request's shadow record.
    ///
    /// The served identity is read off the request's one terminal outcome and
    /// from nowhere else: not the plan, not the store, not a re-run of the
    /// router. That is what makes the correlation trustworthy in both
    /// directions — a failover records the candidate that really answered, and a
    /// request that served nothing (failed, cancelled, or abandoned part-way)
    /// leaves the record uncorrelated, because the outcome has no served
    /// identity for those terminal states.
    #[cfg(feature = "ml")]
    fn shadow_correlated(&self, outcome: &Outcome, validated: bool) {
        let Some(shadow_id) = self.shadow_id.as_deref() else {
            return;
        };
        // An outcome the schema rejected carries no trustworthy identity
        // evidence, so it correlates nothing rather than something guessed.
        let served = if validated {
            outcome
                .served_identity()
                .map(|identity| identity.model().to_string())
        } else {
            None
        };
        self.state
            .shadow()
            .correlate_served(shadow_id, served.as_deref());
    }

    /// Ingest this request's terminal outcome into the canonical dataset.
    ///
    /// Strictly one-way and strictly last: it runs after the request's single
    /// validated Outcome exists, it feeds the store, and nothing reads the
    /// result back. A request whose Outcome the schema rejected contributes
    /// nothing — an outcome with untrustworthy identity evidence cannot be
    /// labelled, and guessing would be worse than having no sample.
    ///
    /// Three outcomes, never conflated: the samples were stored, the request
    /// retained no decision-time input so it has no features at all, or the
    /// sample was refused with a reason. Every one of the three is counted, so
    /// "no decision-time input" can never be mistaken for "collected".
    ///
    /// The ingestion is contained: it returns nothing, propagates no error, and
    /// catches its own panics, so no dataset condition can fail a request.
    #[cfg(feature = "ml")]
    fn dataset_ingested(&mut self, outcome: &Outcome, validated: bool) {
        if self.ingested {
            return;
        }
        if !validated {
            self.ingested = true;
            return;
        }
        let ingestion = crate::ml::dataset::contained_ingest(
            self.state.dataset(),
            self.id(),
            outcome,
            self.decision_time.as_ref(),
        );
        self.ingested = true;
        match ingestion {
            crate::ml::dataset::Ingestion::Ingested { sample_ids } => {
                tracing::debug!(
                    request_id = %self.id(),
                    samples = sample_ids.len(),
                    "training samples collected"
                );
            }
            crate::ml::dataset::Ingestion::NoDecisionTimeInput => {
                tracing::debug!(
                    request_id = %self.id(),
                    "no retained decision-time input; no training sample collected"
                );
            }
            crate::ml::dataset::Ingestion::Rejected { reason } => {
                tracing::warn!(
                    request_id = %self.id(),
                    reason,
                    "training sample refused at the dataset boundary"
                );
            }
        }
    }

    /// Persist this request to the durable trace log.
    ///
    /// The same three facts as the dataset boundary, in the same terminal
    /// transition, from the same validated outcome: which candidates were on the
    /// table and eligible, and what each attempt that was actually made
    /// produced. The dataset holds the samples and dies with the process; this is
    /// what makes the history a *learning* history rather than a log line.
    ///
    /// Exactly once per request, and only when the same decision-time input the
    /// dataset was given is available. Writing a trace without features would
    /// produce a record that looks like counterfactual evidence and cannot support
    /// a counterfactual claim.
    ///
    /// Contained exactly as the dataset is: no return value, no error, no panic
    /// escapes, and a refusal is counted on the log rather than propagated.
    #[cfg(feature = "ml")]
    fn trace_persisted(&mut self) {
        if self.trace_written {
            return;
        }
        // No configured state directory means no durable history at all. The
        // in-memory dataset still collected the samples; nothing persists them.
        let Some(log) = self.state.traces() else {
            self.trace_written = true;
            return;
        };
        let Some(input) = self.decision_time.as_ref() else {
            return;
        };
        let samples = self.state.dataset().training_slice();
        let samples = samples
            .into_iter()
            .filter(|sample| sample.outcome_id == self.id() || sample.request_id == self.id())
            .collect::<Vec<_>>();
        if samples.is_empty() {
            // Nothing was collected for this request, so there is nothing to
            // record. Counted by the log on its own terms when it is told.
            self.trace_written = true;
            let _ = crate::ml::traces::contained_append(
                log,
                &input.decision_id,
                input,
                Vec::new(),
                chrono::Utc::now().timestamp(),
            );
            return;
        }
        self.trace_written = true;
        let verdict = crate::ml::traces::contained_append(
            log,
            &input.decision_id,
            input,
            samples,
            chrono::Utc::now().timestamp(),
        );
        if let crate::ml::traces::TraceIngestion::Refused { reason } = verdict {
            tracing::warn!(
                request_id = %self.id(),
                reason,
                "routing trace was not persisted"
            );
        }
    }

    /// Project this request's terminal outcome for correlation.
    ///
    /// The third consumer of the one terminal transition, and the last thing it
    /// does with the outcome. It is invoked unconditionally — including for
    /// requests that were never policy-routed and therefore have no decision id
    /// at all — because that is the case the projection is built to report
    /// rather than paper over: such a request projects as *refused, decision id
    /// absent*, and this hook counts and logs the refusal instead of skipping
    /// the request so the run looks clean. A refusal is a finding.
    ///
    /// Contained for the same reason the dataset hook is: it returns nothing,
    /// propagates no error, and catches its own panics, so no projection
    /// condition can fail a request. By this point the response, the spend, the
    /// activity record and the outcome are all already decided.
    fn observed(&mut self, outcome: &Outcome, validated: bool) {
        if self.projected {
            return;
        }
        self.projected = true;
        // Only a validated outcome is worth projecting; the projection would
        // refuse it anyway on its own schema check, and handing it unvalidated
        // input would mean the refusal it reports is not the interesting one.
        if !validated {
            return;
        }
        let decision = self.decision.clone();
        let projected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.state
                .projections()
                .record(outcome.clone(), decision.clone());
            crate::observability::project_request(outcome, decision.as_ref())
        }));
        match projected {
            Ok(crate::observability::Projection::Projected(record)) => {
                tracing::debug!(
                    request_id = %self.id(),
                    outcome_id = %record.outcome_id(),
                    decision_id = %record.decision_id(),
                    served = ?record.served_identity.as_ref().map(|label| (
                        label.provider.as_str(),
                        label.model.as_str()
                    )),
                    "request projected for correlation"
                );
            }
            Ok(crate::observability::Projection::Refused(refusal)) => {
                tracing::debug!(
                    request_id = %self.id(),
                    outcome_id = ?refusal.record.outcome_id,
                    reason = ?refusal.reason(),
                    consequence = refusal.consequence(),
                    "request refused projection"
                );
            }
            Err(_) => {
                tracing::warn!(
                    request_id = %self.id(),
                    "the observability projection panicked; the request is unaffected"
                );
            }
        }
    }

    /// The loop is about to try `candidate`: the activity record counts the
    /// attempt and names what it resolved to.
    fn attempted(&mut self, candidate: &Candidate) {
        self.record.attempt();
        self.record
            .resolved(candidate.model_id(), &candidate.provider.name);
    }

    /// Open an attempt: the request is now committed to this candidate.
    ///
    /// A rectifier repair is a second send in its own right, so it is opened
    /// with `rectified` set rather than hidden inside the attempt it replaces.
    /// Attempts are strictly sequential — the loop sends, settles, and only then
    /// moves on — so at most one is ever open. A buffered attempt is settled
    /// immediately; a stream's is settled by the terminal transition, because a
    /// handshake is all that is known once the body belongs to the client.
    fn begin_attempt(&mut self, candidate: &Candidate, rectified: bool) {
        // The accounting dedup is per attempt, not per request: a new send has
        // recorded nothing yet, so a failure observed while *this* attempt is
        // open must still reach the router even when an earlier attempt on
        // another candidate already settled one.
        self.attempt_failure_accounted = false;
        let now = chrono::Utc::now().timestamp();
        self.attempts.push(OutcomeAttempt {
            attempt_id: format!("att_{}", uuid::Uuid::new_v4().simple()),
            candidate_model: candidate.model_id().to_string(),
            candidate_provider: candidate.provider.id.clone(),
            started_at: now,
            completed_at: now,
            latency_ms: 0.0,
            ttft_ms: None,
            success: false,
            failure_class: None,
            failure_message: None,
            http_status: None,
            rectified,
            // Filled in when the request settles, because the price depends on
            // what the provider reported back and is not knowable before.
            cost: None,
        });
    }

    /// Price the most recent attempt, once its usage is known.
    ///
    /// Separate from settling the attempt because a *failed* call can be billed
    /// too, and a router that prices only its successes cannot tell a plan that
    /// failed cheaply from one that failed expensively.
    ///
    /// Called from the terminal transition rather than from the attempt loop,
    /// because that is where the settlement — and therefore the usage — is known.
    /// It prices the last attempt, which is the one the settlement describes; an
    /// earlier failed attempt in the same chain keeps whatever usage its own
    /// failure reported.
    fn price_last_attempt(&mut self, pricing: Option<&Pricing>, usage: &Usage) {
        let Some(pricing) = pricing else {
            return;
        };
        if *usage == Usage::default() {
            // No usage means no basis for a figure. Leaving it unattributed is
            // the honest answer; zero would claim the call was free.
            return;
        }
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.cost = Some(pricing.cost_of(usage).amount);
        }
    }

    /// The attempt that has no verdict yet, if one is open.
    fn open_attempt(&self) -> Option<usize> {
        let index = self.attempts.len().checked_sub(1)?;
        let attempt = self.attempts.get(index)?;
        (!attempt.success && attempt.failure_class.is_none()).then_some(index)
    }

    /// The open attempt produced the answer the client received, so it is the
    /// identity that served the request.
    fn served_attempt(&mut self, since: Instant) {
        let Some(index) = self.open_attempt() else {
            tracing::warn!("a served attempt had no open attempt to settle");
            return;
        };
        let Some(attempt) = self.attempts.get_mut(index) else {
            return;
        };
        attempt.success = true;
        attempt.completed_at = chrono::Utc::now().timestamp();
        attempt.latency_ms = since.elapsed().as_secs_f64() * 1000.0;
        let identity = CandidateIdentity::new(
            attempt.candidate_model.clone(),
            attempt.candidate_provider.clone(),
        );
        self.served = Some(identity);
    }

    /// The open attempt failed, with the canonical classification of `error`.
    ///
    /// A failure here is reported to the router adapters by the loop that owns
    /// the attempt, because that is where a routing decision can still act on
    /// it. The terminal transition then leaves it alone — but only for the
    /// attempt that actually recorded this failure, which is why the dedup flag
    /// is set here rather than at the request level.
    fn failed_attempt(&mut self, since: Instant, error: &Error) {
        let Some(index) = self.open_attempt() else {
            tracing::warn!(error = %error, "a failed attempt had no open attempt to settle");
            return;
        };
        self.attempt_failure_accounted = true;
        let failure = error.classified();
        let Some(attempt) = self.attempts.get_mut(index) else {
            return;
        };
        attempt.completed_at = chrono::Utc::now().timestamp();
        attempt.latency_ms = since.elapsed().as_secs_f64() * 1000.0;
        attempt.failure_class = Some(failure.class);
        attempt.failure_message = Some(failure.message);
        attempt.http_status = failure.status;
    }

    /// The identity of the final attempt actually made, if any.
    fn last_attempted_identity(&self) -> Option<CandidateIdentity> {
        self.attempts.last().map(|attempt| {
            CandidateIdentity::new(&attempt.candidate_model, &attempt.candidate_provider)
        })
    }

    /// The model name to report for this request, used by the placeholder a
    /// cancelled Responses API request leaves behind.
    fn served_model(&self) -> String {
        self.served
            .clone()
            .or_else(|| self.last_attempted_identity())
            .map(|identity| identity.model)
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Settle the attempt a stream left open, from the terminal state.
    ///
    /// A stream hands its body to the client before the answer is finished, so
    /// the handshake proved reachability and nothing more. An answer that
    /// reached the client in full is the only successful attempt: a broken,
    /// abandoned or cancelled stream is not, and saying otherwise would be
    /// exactly the "usage exists, so it must have worked" reading this lifecycle
    /// exists to remove.
    fn settle_open_attempt(&mut self, terminal: &TerminalKind) {
        let Some(index) = self.open_attempt() else {
            return;
        };
        let Some(attempt) = self.attempts.get_mut(index) else {
            return;
        };
        let failure = terminal.classified();
        let identity = CandidateIdentity::new(
            attempt.candidate_model.clone(),
            attempt.candidate_provider.clone(),
        );
        attempt.completed_at = chrono::Utc::now().timestamp();
        match failure {
            None => attempt.success = true,
            Some(ref failure) => {
                attempt.failure_class = Some(failure.class);
                attempt.failure_message = Some(failure.message.clone());
                attempt.http_status = failure.status;
            }
        }
        if terminal.is_served() {
            self.served = Some(identity);
        }
    }

    /// The one terminal transition of this request.
    ///
    /// Every path that can end a request arrives here and only the first one
    /// counts. The attempt verdict, the classified router accounting, the
    /// activity record, the charge and the request's single `Outcome` are each
    /// written exactly once, in that order, from one place.
    fn finalize(&mut self, terminal: TerminalKind, settlement: Settlement) {
        if self.terminal {
            return;
        }
        self.terminal = true;

        // 1. Anything still in flight is settled by the terminal state. A
        //    buffered request has nothing open here; a stream does.
        self.settle_open_attempt(&terminal);

        // 2. Router accounting, once, through the classified adapter. A failure
        //    the loop already reported at its attempt is not reported again, and
        //    a served request already had its attempt health reported where the
        //    attempt happened. What is left is a terminal failure that belongs to
        //    no attempt — a stream that broke or was abandoned after its
        //    handshake — and the impact table keeps a client that vanished out of
        //    provider health entirely.
        if !self.attempt_failure_accounted {
            if let (Some(failure), Some(identity)) =
                (terminal.classified(), self.last_attempted_identity())
            {
                self.state.router.record_classified_attempt(
                    identity.model(),
                    identity.provider(),
                    &failure,
                    &self.routing,
                );
            }
        }

        // 3. Spend. A stream only reports its usage at the end, so this is the
        //    one moment its cost can be charged.
        let cost = settlement
            .pricing
            .as_ref()
            .map(|pricing| pricing.cost_of(&settlement.usage));
        if let Some(cost) = &cost {
            if !settlement.provider_id.is_empty() {
                // Booked against the model that answered, so a failover spends
                // from the provider it actually reached.
                self.state
                    .charge(&settlement.provider_id, settlement.tier, cost);
            }
        }
        // And attributed to the attempt that incurred it, so a chain's spend is
        // per-attempt rather than only in total. Without this the attempt-scoped
        // training samples carry no cost target at all, and the routing
        // comparison — which reads only attempt samples — compares cost as a
        // structural constant.
        self.price_last_attempt(settlement.pricing.as_ref(), &settlement.usage);

        // 4. The activity record, once.
        let latency_ms = self.elapsed_ms();
        let record = self
            .record
            .usage(settlement.usage)
            .priced_with(settlement.pricing.as_ref());
        if terminal.is_served() {
            record.ok();
        } else {
            record.fail(terminal.record_status(), terminal.record_message());
        }
        let record = record.finish(latency_ms);
        self.state.stats.record(record.clone());

        // 5. Exactly one outcome per request, from the accepted schema.
        let outcome = self.build_outcome(&record, &terminal, cost.as_ref());
        let validation = outcome.validate();
        if let Err(reason) = &validation {
            tracing::error!(
                request_id = %record.id,
                reason,
                "constructed an outcome the schema rejected"
            );
        }
        // shadow-block-begin
        // Shadow correlation is the last step of the same terminal transition,
        // because that is the only moment the served identity exists. It reads
        // the outcome built above and touches nothing else: a fault or an
        // uncorrelated record can never affect the response, which has already
        // been decided by this point in every path.
        #[cfg(feature = "ml")]
        self.shadow_correlated(&outcome, validation.is_ok());
        // shadow-block-end
        // The log takes its own copy of the outcome; this one is the dataset's
        // to read, and neither can see the other's.
        self.state.outcomes().record(outcome.clone());
        // dataset-block-begin
        // Dataset ingestion is the very last step of the same terminal
        // transition, and it is one-way: the request's own validated Outcome
        // plus the decision-time input this request's record was accepted with,
        // and nothing else. It is not a routing input, it returns nothing, and
        // it cannot fail the request — by this point the response, the spend,
        // the activity record and the outcome are all already decided.
        #[cfg(feature = "ml")]
        self.dataset_ingested(&outcome, validation.is_ok());
        // dataset-block-end
        // trace-block-begin
        // The durable trace is written from the same terminal transition, on the
        // same validated outcome and the same retained decision-time input, so a
        // trace and the dataset samples can never disagree about a request.
        #[cfg(feature = "ml")]
        self.trace_persisted();
        // trace-block-end
        // projection-block-begin
        // The observability projection runs last, on the same validated outcome,
        // with the request's own decision supplied so the record can be joined
        // on its decision id. It is invoked for every validated request,
        // routed or not, and a refusal is counted and logged rather than
        // skipped — the absence of a decision id is exactly the fact the
        // projection exists to report accurately. One-way, returns nothing, and
        // cannot fail a request.
        self.observed(&outcome, validation.is_ok());
        // projection-block-end
    }

    /// Build this request's outcome from the accepted schema.
    ///
    /// Identity comes from exactly one place: the router's planned selection,
    /// the final attempt made, and the candidate whose response was actually
    /// delivered. A non-success outcome never claims a served identity, and
    /// every other field is evidence the terminal transition established rather
    /// than a second opinion formed here.
    fn build_outcome(
        &self,
        record: &RequestRecord,
        terminal: &TerminalKind,
        cost: Option<&Cost>,
    ) -> Outcome {
        let mut builder = Outcome::builder(record.id.clone())
            .streaming(self.streaming)
            .dialect(dialect_name(self.dialect))
            .total_latency_ms(record.latency_ms as f64)
            .timestamp(record.at.timestamp());
        if let Some(ref decision) = self.decision {
            builder = builder.decision_id(decision.decision_id.clone());
        }
        if let Some(ref response_id) = self.response_id {
            builder = builder.response_id(response_id.clone());
        }
        if let Some(ref planned) = self.planned {
            builder = builder.planned(planned.model.clone(), planned.provider.clone());
        }
        for attempt in &self.attempts {
            builder = builder.attempt(attempt.clone());
        }
        if terminal.is_served() || record.usage != Usage::default() {
            builder = builder.usage(record.usage);
        }
        if let Some(ttft_ms) = record.ttft_ms {
            builder = builder.ttft_ms(ttft_ms as f64);
        }
        if let Some(cost) = cost {
            // The per-currency split stays in the activity record; the outcome
            // carries the amount that was charged.
            builder = builder.cost(None, Some(cost.amount));
        }
        match terminal {
            TerminalKind::Served => {
                if let Some(served) = self.served.clone() {
                    builder = builder.served_candidate(served.model, served.provider);
                }
            }
            other => {
                if let Some(failure) = other.classified() {
                    builder = builder.classified_failure(failure);
                }
            }
        }
        builder.build()
    }
}

/// Why a stream stopped producing events.
///
/// A stream has four genuinely different endings, and only the stream knows
/// which one happened. Collapsing them — as a success-defaulted drop path does
/// — is what turns an abandoned answer into a served one.
enum StreamTerminal {
    /// The upstream closed the stream after its end.
    Completed,
    /// The upstream reported an error part way through.
    UpstreamError(Error),
    /// The client asked for this request to be cancelled.
    Cancelled,
    /// The client went away before the stream finished.
    ClientDisconnected,
}

struct SseState {
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    pending: VecDeque<SseFrame>,
    lifecycle: RequestLifecycle,
    state: Arc<AppState>,
    usage: Usage,
    pricing: Option<Pricing>,
    /// Who to bill, once the stream reports what it used.
    provider_id: String,
    tier: Option<ModelTier>,
    /// Set for classifier streams, which accumulate their text so the verdict
    /// can be checked once the stream ends. Main streams never pay for this.
    classifier_text: Option<String>,
    finished: bool,
    /// Whether any answer byte has been produced for this client yet. A client
    /// that disappears after that truncated the answer it was reading.
    emitted: bool,
    /// Pre-generated response ID for in-flight tracking (Responses API).
    response_id: Option<String>,
    /// Cancel receiver for cancellation detection (Responses API).
    cancel_rx: Option<watch::Receiver<bool>>,
    /// What the client asked the proxy to retain for this response.
    storage: protocol::responses::StoragePolicy,
    /// The Responses input items, kept so a completed stream can be retrieved
    /// and continued exactly like a buffered answer.
    input_items: Vec<Value>,
    /// The response this stream continues, when the client sent one.
    previous_response_id: Option<String>,
    /// The admission permits for this request's budgeted scopes.
    ///
    /// Nothing reads it: it is held for its drop. `Drop for SseState` runs the
    /// terminal transition — which charges the ledger — before any field is
    /// released, so the next request for the scope can never be admitted against
    /// a total this stream has not written yet, and a client that walks away
    /// releases the scope through the same path.
    _admission: AdmissionGuard,
}

impl SseState {
    /// How long this stream has been running, for the record's own fields.
    fn elapsed_ms(&self) -> u64 {
        self.lifecycle.elapsed_ms()
    }

    /// Keep the finished stream, so its id answers a later `GET`.
    ///
    /// Only a confirmed normal terminal is stored, and only when the client
    /// asked for retention: a truncated, failed, cancelled or `store: false`
    /// stream has nothing to replay, and a truncated one must never be handed
    /// back as a completed answer. The in-flight entry is gone by the time this
    /// runs, so a cancellation arriving now finds the stored response instead
    /// of overwriting it with a placeholder.
    fn store_completed(&self) {
        if !self.storage.retains() {
            return;
        }
        let Some(ref id) = self.response_id else {
            return;
        };
        let Some(output) = self.encoder.response_output() else {
            return;
        };
        self.state.response_store.put(StoredResponse::completed(
            id.clone(),
            self.lifecycle.served_model(),
            self.input_items.clone(),
            output,
            self.usage,
            self.previous_response_id.clone(),
            self.lifecycle.decision(),
        ));
    }

    /// The stream's one terminal transition.
    ///
    /// Idempotent, so [`Drop`] can call it for a client that walked away
    /// without disturbing a stream that already ended normally.
    fn finalize(&mut self, terminal: StreamTerminal) {
        if self.lifecycle.is_terminal() {
            return;
        }
        // Clean up in-flight tracking for Responses API streams.
        if let Some(ref id) = self.response_id {
            self.state.response_store.complete_in_flight(id);
        }
        if let StreamTerminal::Completed = terminal {
            self.store_completed();
        }
        // A classifier stream that reached its end without producing a verdict
        // is worth a warning. The bytes have already gone out, so there is
        // nothing to fail over to — but the client will fail closed on its own
        // parse, and this line is how that shows up in the proxy's log.
        if let StreamTerminal::Completed = terminal {
            if let Some(text) = self.classifier_text.take() {
                if crate::classifier::parse_verdict(&text)
                    == crate::classifier::ClassifierVerdict::Unparseable
                {
                    tracing::warn!(
                        model = %self.lifecycle.served_model(),
                        "streamed classifier response carried no <block> verdict"
                    );
                }
            }
        }
        let kind = match terminal {
            StreamTerminal::Completed => TerminalKind::Served,
            StreamTerminal::UpstreamError(error) => TerminalKind::failed(&error),
            StreamTerminal::Cancelled => TerminalKind::ClientCancelled,
            StreamTerminal::ClientDisconnected => TerminalKind::ClientDisconnected {
                emitted: self.emitted,
            },
        };
        let settlement = Settlement {
            usage: self.usage,
            pricing: self.pricing.clone(),
            provider_id: self.provider_id.clone(),
            tier: self.tier,
        };
        self.lifecycle.finalize(kind, settlement);
    }
}

impl Drop for SseState {
    fn drop(&mut self) {
        // A client disconnect drops the body stream without the unfold loop ever
        // reaching the end of it. That is a terminal state in its own right:
        // the answer was abandoned, and an abandoned answer is never a served
        // one. Activity, the ledger and the outcome still record what happened,
        // exactly once, and the provider's health is left alone.
        self.finalize(StreamTerminal::ClientDisconnected);
    }
}

/// What the SSE pipeline needs to know about the request it is serving.
struct StreamContext {
    lifecycle: RequestLifecycle,
    usage: Usage,
    pricing: Option<Pricing>,
    provider_id: String,
    tier: Option<ModelTier>,
    kind: RequestKind,
    /// Pre-generated response ID for in-flight tracking (Responses API).
    response_id: Option<String>,
    /// Cancel receiver for cancellation detection (Responses API).
    cancel_rx: Option<watch::Receiver<bool>>,
    /// What the client asked the proxy to retain for this response.
    storage: protocol::responses::StoragePolicy,
    /// The Responses input items, replayed by a later `GET`.
    input_items: Vec<Value>,
    /// The response this stream continues, when the client sent one.
    previous_response_id: Option<String>,
    /// Handed to the stream state, which holds it until the stream is over.
    admission: AdmissionGuard,
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
        lifecycle: context.lifecycle,
        state: app,
        usage: context.usage,
        pricing: context.pricing,
        provider_id: context.provider_id,
        tier: context.tier,
        classifier_text,
        finished: false,
        emitted: false,
        response_id: context.response_id,
        cancel_rx: context.cancel_rx,
        storage: context.storage,
        input_items: context.input_items,
        previous_response_id: context.previous_response_id,
        _admission: context.admission,
    };

    futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(frame) = st.pending.pop_front() {
                return Some((Ok(bytes::Bytes::from(frame.to_wire())), st));
            }
            if st.finished {
                return None;
            }
            // Race upstream events against cancellation signal for immediate abort.
            let next_event = if let Some(ref mut rx) = st.cancel_rx {
                tokio::select! {
                    event = st.events.next() => event,
                    result = rx.changed() => {
                        if result.is_ok() && *rx.borrow() {
                            st.finished = true;
                            // The cancelled placeholder carries no content, but
                            // it is still a retained response: a client that
                            // asked for `store: false` gets none.
                            if st.storage.retains() {
                                if let Some(ref id) = st.response_id {
                                    let model = st.lifecycle.served_model();
                                    st.state.response_store.mark_cancelled(id, model);
                                }
                            }
                            // An explicit cancellation is a terminal state of its
                            // own, never a success.
                            st.finalize(StreamTerminal::Cancelled);
                            return None;
                        }
                        // Spurious wakeup or channel closed — continue
                        st.events.next().await
                    }
                }
            } else {
                st.events.next().await
            };
            match next_event {
                Some(Ok(event)) => {
                    // The response's public id is the one the proxy registered
                    // as in-flight before the first byte went out. It is not a
                    // substitute for an empty upstream id: an upstream id the
                    // client could only repeat back to a cancel endpoint that
                    // never heard of it is not an identity, so the upstream id
                    // is never published.
                    let event = match (&st.response_id, &event) {
                        (Some(pregen_id), StreamEvent::Start { model, usage, .. }) => {
                            StreamEvent::Start {
                                id: pregen_id.clone(),
                                model: model.clone(),
                                usage: *usage,
                            }
                        }
                        _ => event,
                    };
                    match &event {
                        StreamEvent::ThinkingDelta { .. } => {
                            let elapsed = st.elapsed_ms();
                            st.lifecycle.ttft_ms(elapsed);
                        }
                        StreamEvent::TextDelta { text, .. } => {
                            let elapsed = st.elapsed_ms();
                            st.lifecycle.ttft_ms(elapsed);
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
                    // From here the client holds part of the answer, so a
                    // disconnect that happens later truncates it.
                    st.emitted |= !frames.is_empty();
                    st.pending.extend(frames);
                }
                Some(Err(err)) => {
                    st.finished = true;
                    // The upstream may have ended a truncated body after
                    // reporting usage: keep that accounting even though the
                    // request itself is failing, so the partial answer's real
                    // spend is not silently dropped.
                    if let Error::InterruptedStream { usage } = &err {
                        st.usage = *usage;
                    }
                    let frames = st.encoder.error(&err);
                    st.emitted |= !frames.is_empty();
                    st.pending.extend(frames);
                    st.finalize(StreamTerminal::UpstreamError(err));
                }
                None => {
                    st.finished = true;
                    let frames = st.encoder.finish();
                    st.emitted |= !frames.is_empty();
                    st.pending.extend(frames);
                    st.finalize(StreamTerminal::Completed);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Activity message for a failed request, through the same two steps the
    /// terminal transition uses: the error becomes a terminal state, and that
    /// state's message becomes the record's error.
    fn activity_error(error: &Error) -> String {
        let terminal = TerminalKind::failed(error);
        let mut builder = RecordBuilder::new(Dialect::OpenAI, "sonnet-class", false);
        builder.fail(terminal.record_status(), terminal.record_message());
        builder.finish(0).error.unwrap_or_default()
    }

    #[test]
    fn no_upstream_body_shape_reaches_the_activity_record() {
        // Bodies that the old `": <"` rule let through, plus the one it caught.
        let bodies = [
            "{\"error\":{\"message\":\"invalid key sk-SYNTHETIC-TEST-ONLY\"}}",
            "invalid key sk-SYNTHETIC-TEST-ONLY",
            "\n\t  <html><body>openresty</body></html>",
            "<html><body>openresty</body></html>",
        ];
        for body in bodies {
            let error = Error::Upstream {
                provider: "test-provider".into(),
                status: 401,
                body: body.into(),
            };
            let shown = activity_error(&error);
            assert!(
                !shown.contains("sk-SYNTHETIC-TEST-ONLY") && !shown.contains("openresty"),
                "the provider body reached Activity: {shown}"
            );
            assert_eq!(shown, "upstream test-provider returned 401");
        }

        // A malformed payload is a body too, and its text can echo credentials.
        let error =
            Error::BadUpstreamPayload("{\"access_token\":\"sk-SYNTHETIC-TEST-ONLY\"}".into());
        let shown = activity_error(&error);
        assert!(
            !shown.contains("sk-SYNTHETIC-TEST-ONLY"),
            "the malformed payload reached Activity: {shown}"
        );
        assert_eq!(shown, "upstream returned malformed data");

        // A refused request is still explained, not blanked.
        let error = Error::invalid("model sonnet-class is not configured");
        assert_eq!(
            activity_error(&error),
            "invalid request: model sonnet-class is not configured"
        );
    }

    /// Two providers and one model per tier, so a resolution's provider and tier
    /// scopes are both non-trivial.
    fn scope_registry(budgets: Vec<Budget>) -> Registry {
        use crate::config::{ModelEntry, ProviderConfig, ProviderKind};

        let config = AppConfig {
            providers: vec![
                ProviderConfig::new("deepseek", "DeepSeek", ProviderKind::OpenAICompatible),
                ProviderConfig::new("openai", "OpenAI", ProviderKind::OpenAICompatible),
            ],
            models: vec![
                ModelEntry::for_upstream("deepseek", "flash", Some(ModelTier::Fast)),
                ModelEntry::for_upstream("deepseek", "pro", Some(ModelTier::Standard)),
                ModelEntry::for_upstream("openai", "sol", Some(ModelTier::Reasoning)),
            ],
            budgets,
            ..Default::default()
        };
        Registry::new(Arc::new(config))
    }

    fn daily(scope: BudgetScope) -> Budget {
        Budget::new(scope, crate::budget::BudgetPeriod::Day, "USD", 1.0)
    }

    /// The permits cover exactly the scopes that have a configured limit: no
    /// budget, no gate.
    #[test]
    fn admission_scopes_are_exactly_the_budgeted_ones() {
        let registry = scope_registry(Vec::new());
        assert!(
            main_admission_scopes(
                registry.config(),
                &registry,
                &Resolution::Tier(ModelTier::Standard)
            )
            .is_empty(),
            "no budgets at all means no admission gates"
        );

        // A budget for a provider this request cannot reach is not a permit it
        // has to hold, or a proxy would serialise traffic it does not have to.
        let registry = scope_registry(vec![daily(BudgetScope::Provider {
            id: "openai".into(),
        })]);
        assert!(main_admission_scopes(
            registry.config(),
            &registry,
            &Resolution::Tier(ModelTier::Standard)
        )
        .is_empty());

        // A disabled limit is not a limit.
        let mut disabled = daily(BudgetScope::Global);
        disabled.enabled = false;
        let registry = scope_registry(vec![disabled]);
        assert!(main_admission_scopes(
            registry.config(),
            &registry,
            &Resolution::Tier(ModelTier::Standard)
        )
        .is_empty());
    }

    /// Global, the candidate provider and the tier, in canonical order.
    #[test]
    fn admission_scopes_bundle_global_provider_and_tier() {
        let registry = scope_registry(vec![
            daily(BudgetScope::Global),
            daily(BudgetScope::Provider {
                id: "deepseek".into(),
            }),
            daily(BudgetScope::Tier {
                tier: ModelTier::Standard,
            }),
        ]);
        let scopes = main_admission_scopes(
            registry.config(),
            &registry,
            &Resolution::Tier(ModelTier::Standard),
        );
        assert_eq!(
            scopes,
            vec![
                BudgetScope::Global,
                BudgetScope::Provider {
                    id: "deepseek".into()
                },
                BudgetScope::Tier {
                    tier: ModelTier::Standard
                },
            ]
        );

        // A direct id occupies the provider it names and the tier it is billed
        // under, exactly as the check does — and neither is taken when the
        // configured limits do not cover them.
        let scopes = main_admission_scopes(
            registry.config(),
            &registry,
            &Resolution::Direct("openai-sol".into()),
        );
        assert_eq!(
            scopes,
            vec![BudgetScope::Global],
            "the deepseek provider budget and the standard tier budget do not \
             cover an openai reasoning model"
        );
    }

    /// A degrade target is part of the set before the first check, so the
    /// degrade cannot leave the permit set.
    #[test]
    fn admission_scopes_include_the_degrade_closure() {
        let registry = scope_registry(vec![
            daily(BudgetScope::Provider {
                id: "deepseek".into(),
            }),
            daily(BudgetScope::Tier {
                tier: ModelTier::Fast,
            }),
            daily(BudgetScope::Tier {
                tier: ModelTier::Reasoning,
            })
            .degrading_to(ModelTier::Fast),
        ]);
        let scopes = main_admission_scopes(
            registry.config(),
            &registry,
            &Resolution::Tier(ModelTier::Reasoning),
        );
        assert_eq!(
            scopes,
            vec![
                BudgetScope::Provider {
                    id: "deepseek".into()
                },
                BudgetScope::Tier {
                    tier: ModelTier::Fast
                },
                BudgetScope::Tier {
                    tier: ModelTier::Reasoning
                },
            ],
            "the tier the request can degrade to — and the providers it could \
             then reach — are held from the start"
        );
    }

    /// A classifier side request is gated by global and provider limits over
    /// every candidate the pool could fail over to, and by no class limit.
    #[test]
    fn classifier_admission_scopes_cover_the_pool_but_not_the_class() {
        let registry = scope_registry(vec![
            daily(BudgetScope::Global),
            daily(BudgetScope::Provider {
                id: "deepseek".into(),
            }),
            daily(BudgetScope::Provider {
                id: "openai".into(),
            }),
            daily(BudgetScope::Tier {
                tier: ModelTier::Fast,
            }),
        ]);

        let candidate = |id: &str| {
            let entry = registry.entry(id).unwrap().clone();
            let provider = registry.provider_of(&entry).unwrap().clone();
            Candidate {
                exposed_id: id.to_string(),
                entry,
                provider,
                degraded: false,
            }
        };
        let plan = vec![candidate("deepseek-flash"), candidate("openai-sol")];
        assert_eq!(
            classifier_admission_scopes(registry.config(), &plan),
            vec![
                BudgetScope::Global,
                BudgetScope::Provider {
                    id: "deepseek".into()
                },
                BudgetScope::Provider {
                    id: "openai".into()
                },
            ],
            "every candidate provider in the pool is held, and no class scope is"
        );

        // A pool that no budget covers takes no permits at all.
        assert!(classifier_admission_scopes(scope_registry(Vec::new()).config(), &plan).is_empty());
    }
}
