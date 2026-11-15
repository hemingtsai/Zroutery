//! The reviewer uses the configured pool, its credentials and frozen prices.
//! No generator replay, gateway loopback, model tools, or guessed permission.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};

use super::{context::REVIEW_POLICY, ReviewContext, ToolUse, Unavailable};
use crate::accounting::AttemptFact;
use crate::budget::{BudgetScope, Verdict};
use crate::error::Error;
use crate::ir::{ChatRequest, ChatResponse, Dialect, Message, StopReason, SystemPart};
use crate::query::{RequestKind, SideQueryKind};
use crate::router::Candidate;
use crate::server::pipeline::{credentials, AppState, RequestLifecycle, Settlement, TerminalKind};

pub(super) enum ReviewOutcome {
    Unsupported,
    Unavailable(Unavailable),
    Verdicts(Map<String, Value>),
}

impl ReviewContext {
    pub(super) async fn classify(
        &self,
        state: Arc<AppState>,
        tools: &[ToolUse],
        parent: &str,
        pending: Option<AttemptFact>,
    ) -> ReviewOutcome {
        if !self.still_enabled(&state) {
            return ReviewOutcome::Unsupported;
        }
        let plan = match state
            .router
            .plan_classifier(&self.registry, &self.classifier)
        {
            Ok(plan) => plan,
            Err(Error::RateLimited { .. }) => {
                return ReviewOutcome::Unavailable(Unavailable::Error)
            }
            Err(_) => return ReviewOutcome::Unsupported,
        };
        let limit = if self.classifier.failover {
            self.classifier.max_attempts.max(1)
        } else {
            1
        } as usize;
        let plan = match credentials::expand_inference_keys(&state, plan, limit) {
            Ok(plan) if !plan.is_empty() => plan,
            Err(Error::RateLimited { .. }) => {
                return ReviewOutcome::Unavailable(Unavailable::Error)
            }
            _ => return ReviewOutcome::Unsupported,
        };
        let deadline = Instant::now()
            + Duration::from_secs(self.classifier.server_review_timeout_secs.clamp(1, 120));
        let mut remaining = tools.to_vec();
        let mut verdicts = Map::new();
        let mut last_reason = Unavailable::Error;
        let mut sends = 0usize;
        let mut unsupported_candidates = false;
        for candidate in &plan {
            if remaining.is_empty() || sends >= limit {
                break;
            }
            if !self.still_enabled(&state) {
                return ReviewOutcome::Unsupported;
            }
            if !self.budget_allows(&state, candidate, pending.as_ref()) {
                unsupported_candidates = true;
                continue;
            }
            if !state.router.credential_available(candidate) {
                continue;
            }
            // The SystemOne binary primitive judges one action. Never use one
            // shared probability to clear unrelated calls in a parallel batch.
            if candidate.provider.kind.is_decision() {
                for tool in remaining.clone() {
                    if Instant::now() >= deadline {
                        last_reason = Unavailable::Timeout;
                        break;
                    }
                    if !self.budget_allows(&state, candidate, pending.as_ref()) {
                        break;
                    }
                    match self
                        .send_review(
                            state.clone(),
                            candidate,
                            std::slice::from_ref(&tool),
                            parent,
                            deadline,
                            pending.as_ref(),
                        )
                        .await
                    {
                        Ok(results) => {
                            verdicts.extend(results);
                            remaining.retain(|pending| pending.id != tool.id);
                        }
                        Err(reason) => {
                            last_reason = reason;
                        }
                    }
                }
                sends += 1;
            } else {
                sends += 1;
                match self
                    .send_review(
                        state.clone(),
                        candidate,
                        &remaining,
                        parent,
                        deadline,
                        pending.as_ref(),
                    )
                    .await
                {
                    Ok(results) => {
                        // A parseable batch can omit an ID. Only explicit, typed
                        // verdicts survive; omissions stay unavailable below.
                        for (id, result) in results {
                            verdicts.insert(id, result);
                        }
                        remaining.retain(|tool| !verdicts.contains_key(&tool.id));
                        break;
                    }
                    Err(reason) => last_reason = reason,
                }
            }
        }
        if sends == 0 && unsupported_candidates {
            return ReviewOutcome::Unsupported;
        }
        for tool in remaining {
            verdicts.insert(tool.id, last_reason.wire());
        }
        ReviewOutcome::Verdicts(verdicts)
    }

    fn still_enabled(&self, state: &AppState) -> bool {
        let config = state.config();
        config.classifier.enabled
            && config.classifier.server_review
            && config.classifier == self.classifier
            && config.budgets == self.budgets
    }

    fn budget_allows(
        &self,
        state: &AppState,
        candidate: &Candidate,
        pending: Option<&AttemptFact>,
    ) -> bool {
        if credentials::validate_budget_price(state, candidate, false).is_err() {
            return false;
        }
        let mut ledger = state.ledger();
        if let Some(pending) = pending {
            for cost in pending.known_costs() {
                ledger.charge_windows(
                    &pending.budget_day,
                    &pending.budget_month,
                    &pending.execution.provider_id,
                    pending.execution.tier,
                    &cost,
                );
            }
        }
        let budgets: Vec<_> = self
            .budgets
            .iter()
            .filter(|budget| !matches!(budget.scope, BudgetScope::Tier { .. }))
            .cloned()
            .collect();
        matches!(
            crate::budget::check(
                &budgets,
                &ledger,
                chrono::Local::now(),
                std::slice::from_ref(&candidate.provider.id),
                None
            ),
            Verdict::Allow
        )
    }

    fn request_for(&self, tools: &[ToolUse], decision: bool) -> ChatRequest {
        let mut payload = self.payload.clone();
        payload["pending_tool_calls"] = json!(tools.iter().map(ToolUse::wire).collect::<Vec<_>>());
        let policy = if decision {
            let common = REVIEW_POLICY
                .split("\nReturn exactly one JSON object:")
                .next()
                .unwrap_or(REVIEW_POLICY);
            format!("{common}\nDecide whether the one pending tool call must be blocked. Answer yes to block it, no only when it may proceed under this policy.")
        } else {
            REVIEW_POLICY.to_string()
        };
        let mut req = ChatRequest::new(&self.requested_model, Dialect::Anthropic);
        req.system = vec![SystemPart::new(policy)];
        req.messages = vec![Message::user_text(payload.to_string())];
        req.max_tokens = Some(4096);
        req.client_headers = self.headers.clone();
        req.client_headers.remove("anthropic-beta");
        req.client_identity = self.client_identity.clone();
        req.refresh_required_capabilities();
        req
    }

    async fn send_review(
        &self,
        state: Arc<AppState>,
        candidate: &Candidate,
        tools: &[ToolUse],
        parent: &str,
        deadline: Instant,
        pending: Option<&AttemptFact>,
    ) -> Result<Map<String, Value>, Unavailable> {
        let kind = RequestKind::Side(SideQueryKind::AutoMode);
        let mut req = self.request_for(tools, candidate.provider.kind.is_decision());
        let mut lifecycle = RequestLifecycle::new(
            state.clone(),
            Dialect::Anthropic,
            false,
            kind,
            &req.model,
            &req.client_headers,
            #[cfg(feature = "ml")]
            None,
        );
        lifecycle.link_parent(parent);
        lifecycle.planned_from(None, std::slice::from_ref(candidate));
        lifecycle.attempted(candidate);
        let started = Instant::now();
        let preparation = (|| {
            credentials::validate_execution(&state, candidate)?;
            credentials::validate_budget_price(&state, candidate, false)?;
            if candidate.provider.configured_client_profile()
                == crate::config::ProviderClientProfile::OpenCodeCli
            {
                return Err(Error::NoCandidate(
                    "an agent-shaped upstream cannot review tool safety".into(),
                ));
            }
            // A character estimate is not a measured tokenizer count. Refuse
            // an obviously oversized review without clipping its authorization.
            if candidate
                .entry
                .context_window
                .is_some_and(|window| req.estimate_tokens().saturating_add(4096) > window)
            {
                return Err(Error::invalid("safety review input_too_long"));
            }
            super::super::prepare(
                &state,
                candidate,
                &req,
                crate::upstream::EncodeMode::Classifier,
            )
        })();
        let (key, body) = match preparation {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = lifecycle.begin_preparation(candidate);
                lifecycle.failed_attempt(started, &error);
                let _ = lifecycle.finalize(TerminalKind::failed(&error), Settlement::default());
                return Err(reason_for(&error));
            }
        };
        req.reply_headers = Default::default();
        let mut client = super::super::client_for(&req, &candidate.provider);
        client.native_anthropic = false;
        client.omit_server_review = true;
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(candidate.provider.timeout_secs));
        if wait.is_zero() {
            return Err(Unavailable::Timeout);
        }
        let result = tokio::time::timeout(wait, async {
            let key = super::super::console_key(&state, candidate, key, &mut client).await?;
            if !self.still_enabled(&state) || !self.budget_allows(&state, candidate, pending) {
                return Err(Error::NoCandidate(
                    "safety review configuration changed before execution".into(),
                ));
            }
            if !state.router.allow_candidate_request(candidate) {
                return Err(Error::NoCandidate("safety reviewer is unavailable".into()));
            }
            lifecycle.begin_send(candidate, false)?;
            lifecycle.bind_transport(&client.observation);
            super::super::buffered_upstream_once(
                &state,
                candidate,
                key.as_deref(),
                &body,
                &client,
                kind,
            )
            .await
        })
        .await
        .unwrap_or_else(|_| Err(Error::Timeout(wait.as_secs().max(1))));
        lifecycle.observe_transport(&client.observation);
        let parsed = match result {
            Ok(response) => {
                lifecycle.observed_attempt_usage(response.usage);
                lifecycle.note_declared_response(&response);
                parse_response(&response, tools, candidate.provider.kind.is_decision())
            }
            Err(error) => Err((reason_for(&error), error)),
        };
        match parsed {
            Ok(results) => {
                let latency = started.elapsed().as_millis() as u64;
                state
                    .router
                    .report_success(candidate.model_id(), latency, lifecycle.routing());
                state.router.record_classified_outcome(
                    candidate.model_id(),
                    &candidate.provider.id,
                    latency as f64,
                    None,
                    true,
                    None,
                );
                lifecycle.served_attempt(started);
                if lifecycle
                    .finalize(
                        TerminalKind::Served,
                        Settlement {
                            pricing: candidate.settled_pricing().cloned(),
                            provider_id: candidate.provider.id.clone(),
                            tier: candidate.entry.tier,
                            ..Settlement::default()
                        },
                    )
                    .is_err()
                {
                    return Err(Unavailable::Error);
                }
                Ok(results)
            }
            Err((reason, error)) => {
                lifecycle.failed_attempt(started, &error);
                state.router.note_candidate_retry_after(candidate, &error);
                state.router.record_candidate_failure(
                    candidate,
                    &error.classified(),
                    lifecycle.routing(),
                );
                let _ = lifecycle.finalize(TerminalKind::failed(&error), Settlement::default());
                // Machine-only diagnostics: never print the classifier's body,
                // transcript, tool input or refusal explanation.
                tracing::warn!(
                    model = candidate.model_id(),
                    parent_request_id = parent,
                    "server-side safety review produced no usable verdict"
                );
                Err(reason)
            }
        }
    }
}

fn reason_for(error: &Error) -> Unavailable {
    match error {
        Error::Timeout(_) => Unavailable::Timeout,
        Error::InterruptedStream { .. } => Unavailable::Truncated,
        Error::InvalidRequest(message) if message == "safety review input_too_long" => {
            Unavailable::InputTooLong
        }
        _ => Unavailable::Error,
    }
}

fn parse_response(
    response: &ChatResponse,
    tools: &[ToolUse],
    decision: bool,
) -> Result<Map<String, Value>, (Unavailable, Error)> {
    if response
        .passthrough
        .get("zroutery.internal.responses_status")
        .is_some_and(|status| status.as_str() != Some("completed"))
    {
        return Err((
            Unavailable::Error,
            Error::BadUpstreamPayload("safety review Responses status is not completed".into()),
        ));
    }
    if response.stop_reason == StopReason::Refusal {
        return Err((
            Unavailable::Refused,
            Error::BadUpstreamPayload("safety reviewer refused".into()),
        ));
    }
    if matches!(
        response.stop_reason,
        StopReason::MaxTokens | StopReason::ModelContextWindowExceeded
    ) {
        return Err((
            Unavailable::Truncated,
            Error::BadUpstreamPayload("safety review was truncated".into()),
        ));
    }
    if !matches!(
        response.stop_reason,
        StopReason::EndTurn | StopReason::StopSequence
    ) {
        return Err((
            Unavailable::Error,
            Error::BadUpstreamPayload("safety review has no confirmed text terminal".into()),
        ));
    }
    if decision {
        let verdict = response
            .passthrough
            .get("zroutery.internal.decision_verdict")
            .and_then(|value| {
                serde_json::from_value::<crate::classifier::ClassifierVerdict>(value.clone()).ok()
            });
        let outcome = match verdict {
            Some(crate::classifier::ClassifierVerdict::Allow) => {
                json!({"type":"evaluated", "outcome":"not_flagged"})
            }
            Some(crate::classifier::ClassifierVerdict::Block) => {
                json!({"type":"evaluated", "outcome":"flagged", "explanation":"The safety reviewer blocked this action under the supplied policy."})
            }
            _ => {
                return Err((
                    Unavailable::Error,
                    Error::BadUpstreamPayload("safety reviewer was not confident".into()),
                ))
            }
        };
        if tools.len() != 1 {
            return Err((
                Unavailable::Error,
                Error::BadUpstreamPayload("a binary review cannot approve a batch".into()),
            ));
        }
        return Ok(Map::from_iter([(tools[0].id.clone(), outcome)]));
    }
    let parsed: ClassifierOutput = serde_json::from_str(response.text().trim()).map_err(|_| {
        (
            Unavailable::Error,
            Error::BadUpstreamPayload("safety review verdict JSON is invalid".into()),
        )
    })?;
    if parsed
        .verdicts
        .0
        .keys()
        .any(|id| !tools.iter().any(|tool| &tool.id == id))
    {
        return Err((
            Unavailable::Error,
            Error::BadUpstreamPayload("safety reviewer answered for an unknown tool ID".into()),
        ));
    }
    Ok(parsed
        .verdicts
        .0
        .into_iter()
        .map(|(id, verdict)| {
            let result = if verdict.flagged {
                let explanation: String = verdict
                    .explanation
                    .unwrap_or_else(|| "The safety reviewer blocked this action.".into())
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(360)
                    .collect();
                json!({"type":"evaluated", "outcome":"flagged", "explanation":explanation})
            } else {
                json!({"type":"evaluated", "outcome":"not_flagged"})
            };
            (id, result)
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassifierOutput {
    verdicts: UniqueVerdicts,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerdictInput {
    flagged: bool,
    explanation: Option<String>,
}

struct UniqueVerdicts(BTreeMap<String, VerdictInput>);

impl<'de> Deserialize<'de> for UniqueVerdicts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueVerdicts;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with one verdict per unique tool ID")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut verdicts = BTreeMap::new();
                while let Some((id, verdict)) = access.next_entry::<String, VerdictInput>()? {
                    if verdicts.insert(id, verdict).is_some() {
                        return Err(serde::de::Error::custom("duplicate safety verdict ID"));
                    }
                }
                Ok(UniqueVerdicts(verdicts))
            }
        }
        deserializer.deserialize_map(UniqueVisitor)
    }
}
