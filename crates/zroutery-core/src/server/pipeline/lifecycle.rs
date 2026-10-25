//! One request from entry to exactly one terminal state: the settlement facts, and the single writer that owns them.

use std::sync::Arc;
use std::time::Instant;

use axum::http::HeaderMap;

use super::AppState;
use crate::billing::{Cost, Pricing};
use crate::config::{ModelTier, RoutingConfig};
use crate::error::Error;
use crate::failure::ClassifiedFailure;
#[cfg(feature = "ml")]
use crate::ir::ChatRequest;
use crate::ir::{Dialect, Usage};
#[cfg(feature = "ml")]
use crate::ml::ShadowInput;
use crate::outcome::{Attempt as OutcomeAttempt, CandidateIdentity, Outcome};
use crate::policy::RouteDecision;
#[cfg(feature = "ml")]
use crate::policy::TaskProfile;
use crate::query::RequestKind;
use crate::router::Candidate;
use crate::stats::{RecordBuilder, RequestRecord};

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
pub(super) struct Settlement {
    pub(super) usage: Usage,
    pub(super) pricing: Option<Pricing>,
    /// Who to bill. Empty means nothing is billable, which is the honest state
    /// of a request that never produced usage.
    pub(super) provider_id: String,
    pub(super) tier: Option<ModelTier>,
}

/// Why a request reached its terminal state.
///
/// Chosen once, by whichever part of the pipeline actually observed the end, and
/// never re-derived from a message or a status code: every classification here
/// comes from [`Error::classified`], and the two states an `Error` cannot
/// express — a client that vanished, and a client that cancelled — are named
/// explicitly rather than smuggled in as a success.
pub(super) enum TerminalKind {
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
    pub(super) fn failed(error: &Error) -> Self {
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
    pub(super) fn record_status(&self) -> u16 {
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
    pub(super) fn record_message(&self) -> String {
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
/// Everything one client request needs to reach exactly one terminal state.
///
/// This is the lifecycle's single writer. It owns the activity record, the
/// attempt evidence and the router accounting for a request, and it builds that
/// request's one [`Outcome`]. Attempt-level health is still reported where the
/// attempt happens, because that is the signal a routing decision can act on;
/// but the terminal transition itself is recorded here, once, whichever path
/// reached it: a delivered answer, an upstream error, a client disconnect, or a
/// cancellation.
pub(super) struct RequestLifecycle {
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
    last_candidate: Option<Candidate>,
    /// What the upstream actually declared it ran, kept separate from the routing
    /// identity in `served` and captured *before* the response's model field is
    /// rewritten to the client-facing `exposed_id`.
    ///
    /// Unobserved values stay unknown; they are never inferred from the requested
    /// model, because a relay that fronts several backends behind one name would
    /// otherwise report a confident identity it cannot support.
    #[cfg(feature = "ml")]
    execution_identity: crate::ml::ExecutionIdentity,
    /// The request being served, retained for the stream verifier.
    ///
    /// A stream outlives the handler that owned it, so the request is kept where the
    /// stream body can still reach it. Held only under `ml` because nothing else
    /// needs it after parsing.
    #[cfg(feature = "ml")]
    request: Option<ChatRequest>,
    /// Verdicts from the task verifier for this request, kept so the durable
    /// trace records task outcomes alongside API outcomes. Empty when no check
    /// applied, which is the common case and is not a failure.
    #[cfg(feature = "ml")]
    task_evidence: Vec<crate::ml::TaskOutcomeEvidence>,
}

impl RequestLifecycle {
    /// Start one lifecycle for one client request.
    ///
    /// `headers` is the inbound set, and it is a parameter rather than
    /// something read later because the client fingerprint has to be decided
    /// *once*, at the moment the request enters, and then carried. Deciding it
    /// per attempt would mean a failover could re-attribute the same request to
    /// a different client, and the Activity log would show one client for a
    /// request that another log line called something else.
    pub(super) fn new(
        state: Arc<AppState>,
        dialect: Dialect,
        streaming: bool,
        kind: RequestKind,
        requested_model: &str,
        headers: &HeaderMap,
        #[cfg(feature = "ml")] request: Option<ChatRequest>,
    ) -> Self {
        let mut record = RecordBuilder::new(dialect, requested_model, streaming);
        record.kind(kind);
        // Attribution is separate from the routing client profile resolved
        // later per attempt. This one answers "who is calling", and is fixed at
        // ingress; that one answers "who should the upstream see", and may
        // differ when a profile is pinned in configuration.
        record.client(crate::client::identify(headers, None));
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
            #[cfg(feature = "ml")]
            execution_identity: crate::ml::ExecutionIdentity::unrouted(requested_model),
            #[cfg(feature = "ml")]
            request,
            #[cfg(feature = "ml")]
            task_evidence: Vec::new(),
            projected: false,
            last_candidate: None,
        }
    }

    /// The request id the activity record, the outcome and the shadow
    /// evaluation all correlate on.
    #[cfg_attr(not(feature = "ml"), allow(dead_code))]
    pub(super) fn id(&self) -> &str {
        self.record.id()
    }

    /// Whether this request already reached its terminal transition.
    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// How long this request has been running, for the record's own fields.
    pub(super) fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Stamp time to first token, once.
    pub(super) fn ttft_ms(&mut self, ms: u64) {
        self.record.ttft(ms);
    }

    /// The routing config the router's health adapters need.
    pub(super) fn routing(&self) -> &RoutingConfig {
        &self.routing
    }

    /// The decision trace, for the Responses API record.
    pub(super) fn decision(&self) -> Option<RouteDecision> {
        self.decision.clone()
    }

    /// Note the response store id this request is served under.
    pub(super) fn note_response_id(&mut self, id: &str) {
        self.response_id = Some(id.to_string());
    }

    /// Record what the upstream said it ran, before the wire value is normalised.
    ///
    /// Must be called at the point the upstream `model` field is still intact —
    /// immediately before `resp.model = candidate.exposed_id`, which replaces it with
    /// the client-facing id. That rewrite is correct for the wire: a client that asks
    /// for a Zroutery model id should not learn which relay served it. The cost is
    /// that the declared value would otherwise be gone, leaving `served_model` able to
    /// claim a backend identity the provider never stated.
    ///
    /// Copying it here changes nothing the client observes. A provider that declares
    /// nothing is recorded as having declared nothing, and is never back-filled from
    /// the requested model — see [`crate::ml::execution_identity`].
    #[cfg(feature = "ml")]
    pub(super) fn note_declared_model(&mut self, declared: Option<&str>) {
        if declared.is_some_and(|value| !value.trim().is_empty()) {
            self.execution_identity = self
                .execution_identity
                .clone()
                .with_declared_model(declared);
            return;
        }
        // Only downgrade to "not declared" if nothing better was already recorded, so
        // a rectified retry cannot erase a real observation from the first attempt.
        if !self
            .execution_identity
            .provider_declared_model
            .is_observed()
        {
            self.execution_identity = self.execution_identity.clone().with_declared_model(None);
        }
    }

    /// The request this lifecycle is serving, for the stream verifier.
    ///
    /// Held on the lifecycle because a stream outlives the handler that owned it.
    #[cfg(feature = "ml")]
    pub(super) fn request(&self) -> Option<&ChatRequest> {
        self.request.as_ref()
    }

    /// The id of the attempt currently open, for attributing a task verdict.
    #[cfg(feature = "ml")]
    pub(super) fn last_attempt_id(&self) -> Option<String> {
        self.attempts
            .last()
            .map(|attempt| attempt.attempt_id.clone())
    }

    /// The provider that served, for attributing a task verdict.
    #[cfg(feature = "ml")]
    pub(super) fn served_provider(&self) -> String {
        self.attempts
            .last()
            .map(|attempt| attempt.candidate_provider.clone())
            .unwrap_or_default()
    }

    /// Add verdicts produced after this request's terminal transition was set up.
    #[cfg(feature = "ml")]
    pub(super) fn extend_task_evidence(&mut self, verdicts: Vec<crate::ml::TaskOutcomeEvidence>) {
        self.task_evidence.extend(verdicts);
    }

    /// Fix the planned identity and the decision trace, before any attempt.
    pub(super) fn planned_from(&mut self, decision: Option<&RouteDecision>, plan: &[Candidate]) {
        self.planned = planned_identity(decision, plan);
        if let Some(decision) = decision {
            self.decision = Some(decision.clone());
            self.record.routing_decision(decision.clone());
        }
    }

    /// Note which candidate an attempt is running against.
    ///
    /// Routing provenance only: this is what Zroutery chose, never evidence of what
    /// the provider then executed. It is kept in its own field precisely so it cannot
    /// be mistaken for a declared identity.
    #[cfg(feature = "ml")]
    pub(super) fn note_routed_candidate(&mut self, candidate: &Candidate) {
        self.execution_identity = self.execution_identity.clone().with_routed(
            Some(candidate.exposed_id.as_str()),
            Some(candidate.provider.id.as_str()),
        );
    }

    /// Run the task verifier over a served response and keep only the verdicts.
    ///
    /// Contained for the same reason every other hook here is: it returns nothing,
    /// propagates no error, and cannot fail a request. A verifier that panicked is a
    /// bug, not a verdict, and must not cost the user their answer.
    ///
    /// This is the only place a task check can honestly run. It needs both the
    /// request's *demands* (tools, tool_choice, structured_output) and the
    /// response's *content*, and the persisted trace deliberately keeps neither the
    /// prompt nor the answer. Only the verdict is stored.
    ///
    /// Attributed to the attempt that actually served, so a fallback chain keeps each
    /// candidate's own task result rather than crediting the first failure with the
    /// last one's success.
    #[cfg(feature = "ml")]
    pub(super) fn note_task_evidence(
        &mut self,
        req: &ChatRequest,
        resp: &crate::ir::ChatResponse,
        candidate: &Candidate,
    ) {
        let context = crate::ml::VerdictContext {
            request_id: self.id().to_string(),
            outcome_id: self.id().to_string(),
            attempt_id: self
                .attempts
                .last()
                .map(|attempt| attempt.attempt_id.clone()),
            served_model: Some(candidate.exposed_id.clone()),
            served_provider: Some(candidate.provider.id.clone()),
            // Derived from the request rather than carried on the lifecycle: the
            // lifecycle does not hold the task profile, and re-deriving it here is
            // the same computation `TaskProfile::from_request` performs at ingress.
            task_type: TaskProfile::from_request(req).task_type,
            observed_at: chrono::Utc::now().timestamp(),
        };
        let verdicts = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::ml::verify_task_outcome(req, resp, &context, crate::ml::VerifierConfig::All)
        }))
        .unwrap_or_else(|_| {
            tracing::warn!(
                request_id = %self.id(),
                "task verification panicked and was contained"
            );
            Vec::new()
        });
        self.task_evidence.extend(verdicts);
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
    pub(super) fn shadow_evaluated(&mut self, input: &ShadowInput) {
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
        let verdict = crate::ml::traces::contained_append_with_evidence(
            log,
            &input.decision_id,
            input,
            samples,
            chrono::Utc::now().timestamp(),
            self.execution_identity.clone(),
            self.task_evidence.clone(),
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
    pub(super) fn attempted(&mut self, candidate: &Candidate) {
        self.record
            .inference_credential(candidate.credential.as_ref());
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
    pub(super) fn begin_attempt(&mut self, candidate: &Candidate, rectified: bool) {
        self.last_candidate = Some(candidate.clone());
        self.record
            .inference_credential(candidate.credential.as_ref());
        tracing::debug!(model = candidate.model_id(), provider = %candidate.provider.id,
            account = candidate.credential.as_ref().map(|key| key.account_id.as_str()),
            inference_key = candidate.credential.as_ref().map(|key| key.key_id.as_str()),
            rectified, "opening upstream attempt");
        // Routing provenance for the execution identity. Recorded here because this
        // runs once per send, so a failover chain leaves the identity of the
        // candidate that actually ran.
        #[cfg(feature = "ml")]
        self.note_routed_candidate(candidate);
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
    pub(super) fn served_attempt(&mut self, since: Instant) {
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
    pub(super) fn failed_attempt(&mut self, since: Instant, error: &Error) {
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
    pub(super) fn served_model(&self) -> String {
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
    pub(super) fn finalize(&mut self, terminal: TerminalKind, settlement: Settlement) {
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
            if let (Some(failure), Some(candidate)) =
                (terminal.classified(), self.last_candidate.as_ref())
            {
                self.state
                    .router
                    .record_candidate_failure(candidate, &failure, &self.routing);
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
