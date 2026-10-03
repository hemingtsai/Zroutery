//! Unified outcome model for Zroutery.
//!
//! Captures the complete result of a single client request, including every
//! routing attempt, failure classification, timing, and usage. Bridges the
//! gap between [`RequestRecord`](crate::stats::RequestRecord) (log-oriented),
//! [`RuntimeObservation`](crate::observation::RuntimeObservation) (health-oriented),
//! [`ProviderModelStats`](crate::stats_ext::ProviderModelStats) (latency-oriented),
//! and [`StoredResponse`](crate::ir::response::StoredResponse) (lifecycle-oriented).
//!
//! The [`Outcome`] type is the single source of truth from which all downstream
//! stores can be updated via [`Outcome::record_to_observation`].

use crate::failure::{ClassifiedFailure, FailureClass};
use crate::ir::Usage;
use crate::observation::ObservationStore;
use crate::stats_ext::StatsStore;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identity and failure evidence
// ---------------------------------------------------------------------------

/// The provider/model pair involved in one routing position.
///
/// This is deliberately a small value type rather than a route plan.  An
/// identity can be planned before an attempt, observed on the last attempt, or
/// present only when a successful terminal response was actually served.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateIdentity {
    pub model: String,
    pub provider: String,
}

impl CandidateIdentity {
    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn from_model_provider(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self::new(model, provider)
    }

    pub fn from_provider_model(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(model, provider)
    }
}

impl From<(&str, &str)> for CandidateIdentity {
    fn from((model, provider): (&str, &str)) -> Self {
        Self::new(model, provider)
    }
}

/// The three routing identities that must never be collapsed into one field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutcomeIdentity {
    /// Selected before attempts were made.  `None` means no candidate was
    /// available (for example, a budget denial or no-candidate rejection).
    pub planned: Option<CandidateIdentity>,
    /// Identity of the final attempt made, if any.
    pub last_attempted: Option<CandidateIdentity>,
    /// Identity that produced a successful terminal response, if any.
    pub served: Option<CandidateIdentity>,
}

/// The accepted failure vocabulary plus the error facts captured for one
/// terminal failure.
///
/// `FailureClass` remains the sole classification authority.  This type does
/// not inspect messages or HTTP status codes to reclassify anything; it only
/// carries the facts already attached to an attempt/outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureFacts {
    #[serde(alias = "failure_class")]
    pub class: FailureClass,
    #[serde(alias = "failure_message")]
    pub message: Option<String>,
    #[serde(alias = "status")]
    pub http_status: Option<u16>,
}

impl FailureFacts {
    pub fn new(class: FailureClass, message: Option<String>, http_status: Option<u16>) -> Self {
        Self {
            class,
            message,
            http_status,
        }
    }

    pub fn class(&self) -> FailureClass {
        self.class
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn status(&self) -> Option<u16> {
        self.http_status
    }

    /// Rehydrate the accepted classification context without classifying again.
    pub fn classified(&self) -> ClassifiedFailure {
        ClassifiedFailure {
            class: self.class,
            status: self.http_status,
            message: self.message.clone().unwrap_or_default(),
            impact: self.class.impact(),
        }
    }
}

/// Backwards/forwards-friendly spelling for callers that refer to error facts.
pub type ErrorFacts = FailureFacts;

/// Stable, lossless wire spelling for the accepted failure vocabulary.
pub const fn failure_class_wire_name(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Transport => "transport",
        FailureClass::Timeout => "timeout",
        FailureClass::RateLimit => "rate_limit",
        FailureClass::Authentication => "authentication",
        FailureClass::ProviderUnavailable => "provider_unavailable",
        FailureClass::ProviderRejected => "provider_rejected",
        FailureClass::Protocol => "protocol",
        FailureClass::Capability => "capability",
        FailureClass::InvalidRequest => "invalid_request",
        FailureClass::ClientCancelled => "client_cancelled",
        FailureClass::MissingApiKey => "missing_api_key",
        FailureClass::OverBudget => "over_budget",
        FailureClass::NoCandidate => "no_candidate",
        FailureClass::Configuration => "configuration",
        FailureClass::Interrupted => "interrupted",
        FailureClass::Unknown => "unknown",
    }
}

// ---------------------------------------------------------------------------
// FinalStatus
// ---------------------------------------------------------------------------

/// The terminal status of a request after all attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalStatus {
    /// Request completed successfully.
    Success,
    /// Request failed after all attempts exhausted.
    Failed,
    /// Client cancelled the request.
    Cancelled,
    /// Stream was interrupted after partial output.
    Interrupted,
}

impl FinalStatus {
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    pub const fn is_failure(self) -> bool {
        !self.is_success()
    }
}

// ---------------------------------------------------------------------------
// Attempt
// ---------------------------------------------------------------------------

/// A single attempt at routing a request to a candidate model.
///
/// When failover occurs, each candidate in the chain produces one `Attempt`.
/// When the rectifier repairs a request and retries the same candidate, the
/// retry is a separate attempt with `rectified = true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// Unique id for this attempt (e.g. `att_<uuid>`).
    pub attempt_id: String,
    /// The model that was tried.
    pub candidate_model: String,
    /// The provider that was tried.
    pub candidate_provider: String,
    /// Unix timestamp (seconds) when the attempt started.
    pub started_at: i64,
    /// Unix timestamp (seconds) when the attempt completed.
    pub completed_at: i64,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: f64,
    /// Time to first token (streaming only).
    pub ttft_ms: Option<f64>,
    /// Whether this attempt succeeded.
    pub success: bool,
    /// Classification of the failure (None if success).
    pub failure_class: Option<FailureClass>,
    /// Human-readable failure message (None if success).
    pub failure_message: Option<String>,
    /// HTTP status code returned by the provider, if any.
    pub http_status: Option<u16>,
    /// Whether this attempt was a rectifier retry (same candidate, repaired request).
    pub rectified: bool,
}

impl Attempt {
    /// Whether this attempt can be treated as a successful terminal response.
    pub fn is_terminal_success(&self) -> bool {
        self.success
            && self.failure_class.is_none()
            && self.failure_message.is_none()
            && self
                .http_status
                .is_none_or(|status| (200..=299).contains(&status))
    }

    /// Return the captured failure facts without deriving a new class.
    pub fn failure_facts(&self) -> Option<FailureFacts> {
        self.failure_class
            .map(|class| FailureFacts::new(class, self.failure_message.clone(), self.http_status))
    }

    /// Return the accepted classified failure context for this attempt.
    pub fn classified_failure(&self) -> Option<ClassifiedFailure> {
        self.failure_facts().map(|facts| facts.classified())
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// The complete outcome of a single client request.
///
/// Aggregates every attempt, links to the route decision and response store,
/// and carries timing, usage, and cost information. Call
/// [`Outcome::record_to_observation`] to fan out to the observation and stats
/// stores in one shot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    // --- Identity ---
    /// Unique id for this outcome (e.g. `out_<uuid>`).
    pub outcome_id: String,
    /// Link to the [`RouteDecision`](crate::policy::RouteDecision) that planned
    /// this request. None for requests that bypass routing (e.g. passthrough).
    pub decision_id: Option<String>,
    /// Request-scoped id from the stats system.
    pub request_id: String,
    /// Response store id (for the Responses API lifecycle). None when the
    /// Responses API is not in use.
    pub response_id: Option<String>,

    // --- Result ---
    /// Whether the request ultimately succeeded.
    pub success: bool,
    /// Classified terminal status.
    pub final_status: FinalStatus,

    // --- Candidate info ---
    /// The first model that was tried.
    pub initial_model: String,
    /// The provider of the first model.
    pub initial_provider: String,
    /// The model that ultimately served the response (same as initial if no fallback).
    pub final_model: String,
    /// The provider of the final model.
    pub final_provider: String,

    // --- Explicit identity evidence -----------------------------------------
    // These fields are the lossless planned/last-attempted/served distinction.
    // The legacy `initial_*` and `final_*` fields above remain as compatibility
    // projections; they must not be used to infer a served identity on failure.
    /// Model selected before attempts, if a candidate was planned.
    #[serde(default, alias = "planned")]
    pub planned_model: Option<String>,
    /// Provider selected before attempts, if a candidate was planned.
    #[serde(default, alias = "planned_provider_id")]
    pub planned_provider: Option<String>,
    /// Model of the final attempt made, if any.
    #[serde(default, alias = "last_attempted")]
    pub last_attempted_model: Option<String>,
    /// Provider of the final attempt made, if any.
    #[serde(default, alias = "last_attempted_provider_id")]
    pub last_attempted_provider: Option<String>,
    /// Model that produced a successful terminal response, if any.
    #[serde(default, alias = "served", alias = "final_served_model")]
    pub served_model: Option<String>,
    /// Provider that produced a successful terminal response, if any.
    #[serde(default, alias = "final_served_provider")]
    pub served_provider: Option<String>,
    /// Captured terminal error facts for failures that did not have a provider
    /// attempt (budget, missing key, no candidate, configuration, etc.).
    #[serde(default, alias = "terminal_failure", alias = "error_facts")]
    pub terminal_error: Option<FailureFacts>,

    // --- Attempts ---
    /// Ordered list of attempts (at least one for a non-cancelled request).
    pub attempts: Vec<Attempt>,
    /// Number of fallback transitions (len(attempts) - 1 when all are distinct candidates).
    pub fallback_count: u32,

    // --- Timing ---
    /// Wall-clock total latency in milliseconds (first attempt start to last attempt end).
    pub total_latency_ms: f64,
    /// Time to first token from the *successful* attempt, if streaming.
    pub ttft_ms: Option<f64>,

    // --- Usage ---
    /// Token usage from the successful attempt (None if all attempts failed).
    pub usage: Option<Usage>,
    /// Pre-request cost estimate.
    pub estimated_cost: Option<f64>,
    /// Actual cost after completion.
    pub actual_cost: Option<f64>,

    // --- Context ---
    /// Whether the client requested streaming.
    pub streaming: bool,
    /// The dialect (API flavour) the client spoke.
    pub dialect: String,
    /// Unix timestamp (seconds) when the request was first received.
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// OutcomeBuilder
// ---------------------------------------------------------------------------

/// Builder for [`Outcome`].
///
/// Create via [`Outcome::builder`]. All identity fields are set at construction;
/// remaining fields have sensible defaults and can be overridden before calling
/// [`build`](OutcomeBuilder::build).
pub struct OutcomeBuilder {
    outcome: Outcome,
    /// An explicit terminal state is kept outside the value until build so a
    /// cancellation/interruption cannot be overwritten by attempt heuristics.
    terminal_override: Option<FinalStatus>,
}

impl Outcome {
    /// Start building an `Outcome` for a request with the given id.
    pub fn builder(request_id: impl Into<String>) -> OutcomeBuilder {
        let request_id = request_id.into();
        OutcomeBuilder {
            outcome: Outcome {
                outcome_id: format!("out_{}", uuid::Uuid::new_v4().simple()),
                decision_id: None,
                request_id,
                response_id: None,
                success: false,
                final_status: FinalStatus::Failed,
                initial_model: String::new(),
                initial_provider: String::new(),
                final_model: String::new(),
                final_provider: String::new(),
                planned_model: None,
                planned_provider: None,
                last_attempted_model: None,
                last_attempted_provider: None,
                served_model: None,
                served_provider: None,
                terminal_error: None,
                attempts: Vec::new(),
                fallback_count: 0,
                total_latency_ms: 0.0,
                ttft_ms: None,
                usage: None,
                estimated_cost: None,
                actual_cost: None,
                streaming: false,
                dialect: String::new(),
                timestamp: chrono::Utc::now().timestamp(),
            },
            terminal_override: None,
        }
    }

    /// Determine the terminal status from the last observed attempt and an
    /// explicit cancellation flag.
    ///
    /// A prior successful attempt is not enough: the final attempt is the
    /// terminal observation.  Likewise, a successful-looking usage or cost
    /// record cannot upgrade a cancellation, interruption, or failure.
    pub fn classify_final(attempts: &[Attempt], cancelled: bool) -> FinalStatus {
        if cancelled {
            return FinalStatus::Cancelled;
        }
        let Some(last) = attempts.last() else {
            return FinalStatus::Failed;
        };

        // Failure vocabulary wins over the success bit.  This is important for
        // malformed/partial records: an attempt cannot be terminal success if
        // it carries an explicit cancellation or interruption class.
        match last.failure_class {
            Some(FailureClass::ClientCancelled) => return FinalStatus::Cancelled,
            Some(FailureClass::Interrupted) => return FinalStatus::Interrupted,
            _ => {}
        }
        if last.is_terminal_success() {
            FinalStatus::Success
        } else {
            FinalStatus::Failed
        }
    }

    /// Return the three identity roles as one lossless view.
    pub fn identity(&self) -> OutcomeIdentity {
        OutcomeIdentity {
            planned: self.planned_identity(),
            last_attempted: self.last_attempted_identity(),
            served: self.served_identity(),
        }
    }

    pub fn canonical_identity(&self) -> OutcomeIdentity {
        self.identity()
    }

    /// The candidate selected before attempts began.
    pub fn planned_identity(&self) -> Option<CandidateIdentity> {
        identity_from_parts(
            self.planned_model.as_deref(),
            self.planned_provider.as_deref(),
        )
        .or_else(|| legacy_identity(&self.initial_model, &self.initial_provider))
        .or_else(|| {
            self.attempts.first().map(|attempt| {
                CandidateIdentity::new(&attempt.candidate_model, &attempt.candidate_provider)
            })
        })
        .or_else(|| legacy_identity(&self.final_model, &self.final_provider))
    }

    /// The identity of the final attempt made, if any.
    pub fn last_attempted_identity(&self) -> Option<CandidateIdentity> {
        identity_from_parts(
            self.last_attempted_model.as_deref(),
            self.last_attempted_provider.as_deref(),
        )
        .or_else(|| {
            self.attempts.last().map(|attempt| {
                CandidateIdentity::new(&attempt.candidate_model, &attempt.candidate_provider)
            })
        })
    }

    /// The provider/model that actually produced a successful terminal
    /// response.  This is `None` for every non-success terminal state.
    pub fn served_identity(&self) -> Option<CandidateIdentity> {
        if self.final_status != FinalStatus::Success {
            return None;
        }
        identity_from_parts(
            self.served_model.as_deref(),
            self.served_provider.as_deref(),
        )
        .or_else(|| {
            self.attempts
                .iter()
                .rev()
                .find(|attempt| attempt.is_terminal_success())
                .map(|attempt| {
                    CandidateIdentity::new(&attempt.candidate_model, &attempt.candidate_provider)
                })
        })
    }

    /// Alias using the ADR's explicit "final served" terminology.
    pub fn final_served_identity(&self) -> Option<CandidateIdentity> {
        self.served_identity()
    }

    pub fn planned(&self) -> Option<CandidateIdentity> {
        self.planned_identity()
    }

    pub fn last_attempted(&self) -> Option<CandidateIdentity> {
        self.last_attempted_identity()
    }

    pub fn served(&self) -> Option<CandidateIdentity> {
        self.served_identity()
    }

    pub fn planned_candidate(&self) -> Option<CandidateIdentity> {
        self.planned_identity()
    }

    pub fn last_attempted_candidate(&self) -> Option<CandidateIdentity> {
        self.last_attempted_identity()
    }

    pub fn served_candidate(&self) -> Option<CandidateIdentity> {
        self.served_identity()
    }

    pub fn planned_model(&self) -> Option<&str> {
        self.planned_model
            .as_deref()
            .or_else(|| {
                (!self.initial_model.trim().is_empty()).then_some(self.initial_model.as_str())
            })
            .or_else(|| {
                self.attempts
                    .first()
                    .map(|attempt| attempt.candidate_model.as_str())
            })
            .or_else(|| (!self.final_model.trim().is_empty()).then_some(self.final_model.as_str()))
    }

    pub fn last_attempted_model(&self) -> Option<&str> {
        self.last_attempted_model.as_deref().or_else(|| {
            self.attempts
                .last()
                .map(|attempt| attempt.candidate_model.as_str())
        })
    }

    pub fn served_model(&self) -> Option<&str> {
        if self.final_status != FinalStatus::Success {
            return None;
        }
        self.served_model.as_deref().or_else(|| {
            self.attempts
                .iter()
                .rev()
                .find(|attempt| attempt.is_terminal_success())
                .map(|attempt| attempt.candidate_model.as_str())
        })
    }

    pub fn planned_provider(&self) -> Option<&str> {
        self.planned_provider
            .as_deref()
            .or_else(|| {
                (!self.initial_provider.trim().is_empty()).then_some(self.initial_provider.as_str())
            })
            .or_else(|| {
                self.attempts
                    .first()
                    .map(|attempt| attempt.candidate_provider.as_str())
            })
            .or_else(|| {
                (!self.final_provider.trim().is_empty()).then_some(self.final_provider.as_str())
            })
    }

    pub fn last_attempted_provider(&self) -> Option<&str> {
        self.last_attempted_provider.as_deref().or_else(|| {
            self.attempts
                .last()
                .map(|attempt| attempt.candidate_provider.as_str())
        })
    }

    pub fn served_provider(&self) -> Option<&str> {
        if self.final_status != FinalStatus::Success {
            return None;
        }
        self.served_provider.as_deref().or_else(|| {
            self.attempts
                .iter()
                .rev()
                .find(|attempt| attempt.is_terminal_success())
                .map(|attempt| attempt.candidate_provider.as_str())
        })
    }

    pub fn final_served_model(&self) -> Option<&str> {
        self.served_model()
    }

    pub fn final_served_provider(&self) -> Option<&str> {
        self.served_provider()
    }

    /// Return the captured terminal error facts, if this outcome has one.
    pub fn terminal_failure_facts(&self) -> Option<FailureFacts> {
        if let Some(facts) = &self.terminal_error {
            return Some(facts.clone());
        }
        if self.final_status == FinalStatus::Success {
            return None;
        }
        self.attempts.last().and_then(Attempt::failure_facts)
    }

    /// Alias for callers that use the shorter error-facts terminology.
    pub fn error_facts(&self) -> Option<FailureFacts> {
        self.terminal_failure_facts()
    }

    pub fn failure_class(&self) -> Option<FailureClass> {
        self.terminal_failure_facts().map(|facts| facts.class)
    }

    /// Return the accepted classified terminal failure without reclassification.
    pub fn classified_terminal_failure(&self) -> Option<ClassifiedFailure> {
        self.terminal_failure_facts()
            .map(|facts| facts.classified())
    }

    /// Whether the value represents a successful terminal response.
    pub fn is_terminal_success(&self) -> bool {
        self.final_status == FinalStatus::Success && self.success
    }

    pub fn terminal_status(&self) -> FinalStatus {
        self.final_status
    }

    /// Validate identity, terminal-state, timing, usage, and error evidence.
    ///
    /// This is intentionally fallible and side-effect free.  New fields are
    /// optional on the wire so older records remain readable; when present they
    /// must agree with the authoritative attempt evidence.
    pub fn validate(&self) -> Result<(), String> {
        if self.outcome_id.trim().is_empty() {
            return Err("outcome_id must not be empty".to_string());
        }
        if self.request_id.trim().is_empty() {
            return Err("request_id must not be empty".to_string());
        }
        if self
            .decision_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("decision_id must not be empty when present".to_string());
        }
        if self
            .response_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("response_id must not be empty when present".to_string());
        }
        if self.success != (self.final_status == FinalStatus::Success) {
            return Err("success must agree with final_status".to_string());
        }
        validate_non_negative_finite("total_latency_ms", self.total_latency_ms)?;
        validate_optional_non_negative_finite("ttft_ms", self.ttft_ms)?;
        validate_optional_non_negative_finite("estimated_cost", self.estimated_cost)?;
        validate_optional_non_negative_finite("actual_cost", self.actual_cost)?;
        validate_identity_pair("initial", &self.initial_model, &self.initial_provider)?;
        validate_identity_pair("final", &self.final_model, &self.final_provider)?;
        validate_optional_identity_pair(
            "planned",
            self.planned_model.as_deref(),
            self.planned_provider.as_deref(),
        )?;
        validate_optional_identity_pair(
            "last_attempted",
            self.last_attempted_model.as_deref(),
            self.last_attempted_provider.as_deref(),
        )?;
        validate_optional_identity_pair(
            "served",
            self.served_model.as_deref(),
            self.served_provider.as_deref(),
        )?;

        if self.attempts.is_empty()
            && self.final_status == FinalStatus::Failed
            && self.terminal_error.is_none()
            && self.planned_identity().is_none()
        {
            return Err(
                "failed outcome without attempts requires terminal_error facts".to_string(),
            );
        }
        if self.attempts.is_empty()
            && (self.last_attempted_model.is_some() || self.last_attempted_provider.is_some())
        {
            return Err("last_attempted identity requires an attempt".to_string());
        }

        let mut attempt_ids = std::collections::HashSet::new();
        let mut saw_success = false;
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.attempt_id.trim().is_empty() {
                return Err(format!("attempt[{index}].attempt_id must not be empty"));
            }
            if !attempt_ids.insert(attempt.attempt_id.as_str()) {
                return Err(format!("duplicate attempt_id: {}", attempt.attempt_id));
            }
            validate_identity_pair(
                &format!("attempt[{index}]"),
                &attempt.candidate_model,
                &attempt.candidate_provider,
            )?;
            validate_non_negative_finite(
                &format!("attempt[{index}].latency_ms"),
                attempt.latency_ms,
            )?;
            validate_optional_non_negative_finite(
                &format!("attempt[{index}].ttft_ms"),
                attempt.ttft_ms,
            )?;
            if attempt.completed_at < attempt.started_at {
                return Err(format!("attempt[{index}] completed before it started"));
            }
            if attempt.success {
                if !attempt.is_terminal_success() {
                    return Err(format!("attempt[{index}] has contradictory success facts"));
                }
                if saw_success {
                    return Err("an attempt follows a successful terminal attempt".to_string());
                }
                saw_success = true;
            } else if attempt.failure_class.is_none() {
                return Err(format!(
                    "attempt[{index}] is unsuccessful but has no FailureClass"
                ));
            }
            if saw_success && !attempt.success {
                return Err("an unsuccessful attempt follows a successful attempt".to_string());
            }
        }

        let expected_status = Outcome::classify_final(&self.attempts, false);
        if !self.attempts.is_empty() && expected_status != self.final_status {
            // An explicit cancellation/interruption is authoritative even when
            // an upstream attempt had already recorded a success-looking
            // result.  The attempt evidence is retained, but the request is
            // never promoted to success or served identity.
            let explicit_non_success = matches!(
                self.final_status,
                FinalStatus::Cancelled | FinalStatus::Interrupted
            ) && !self.success
                && self.served_model.is_none()
                && self.served_provider.is_none();
            if !explicit_non_success {
                return Err(format!(
                    "final_status {:?} disagrees with terminal attempt ({:?})",
                    self.final_status, expected_status
                ));
            }
        }

        if let Some(facts) = &self.terminal_error {
            if self.final_status == FinalStatus::Success {
                return Err("successful outcome cannot carry terminal_error".to_string());
            }
            if facts
                .message
                .as_ref()
                .is_some_and(|message| message.is_empty())
            {
                return Err("terminal_error.message must not be empty when present".to_string());
            }
            if let Some(last) = self.attempts.last() {
                if last.failure_class.is_some() && last.failure_class != Some(facts.class) {
                    return Err("terminal_error class disagrees with final attempt".to_string());
                }
            }
        }
        if self.final_status == FinalStatus::Cancelled
            && self
                .terminal_failure_facts()
                .is_some_and(|facts| facts.class != FailureClass::ClientCancelled)
        {
            return Err(
                "cancelled outcome must carry ClientCancelled or no error facts".to_string(),
            );
        }
        if self.final_status == FinalStatus::Interrupted
            && self
                .terminal_failure_facts()
                .is_some_and(|facts| facts.class != FailureClass::Interrupted)
        {
            return Err("interrupted outcome must carry Interrupted or no error facts".to_string());
        }
        if self.final_status != FinalStatus::Success
            && (self.served_model.is_some() || self.served_provider.is_some())
        {
            return Err("non-success outcome cannot claim a served identity".to_string());
        }
        if self.final_status == FinalStatus::Success {
            if self.attempts.is_empty() {
                return Err("successful outcome requires a successful attempt".to_string());
            }
            if self.served_identity().is_none() {
                return Err("successful outcome requires served identity evidence".to_string());
            }
        }

        if let Some(last) = self.attempts.last() {
            if let Some(identity) = self.last_attempted_identity() {
                if identity.model != last.candidate_model
                    || identity.provider != last.candidate_provider
                {
                    return Err("last_attempted identity does not match final attempt".to_string());
                }
            }
            if self.final_status == FinalStatus::Success {
                if let Some(identity) = self.served_identity() {
                    let served_attempt = self
                        .attempts
                        .iter()
                        .rev()
                        .find(|attempt| attempt.is_terminal_success())
                        .ok_or_else(|| {
                            "successful outcome has no successful attempt".to_string()
                        })?;
                    if identity.model != served_attempt.candidate_model
                        || identity.provider != served_attempt.candidate_provider
                    {
                        return Err("served identity does not match successful attempt".to_string());
                    }
                }
                if let (Some(legacy), Some(served)) = (
                    legacy_identity(&self.final_model, &self.final_provider),
                    self.served_identity(),
                ) {
                    if legacy != served {
                        return Err(
                            "final identity projection disagrees with served identity".to_string()
                        );
                    }
                }
            }
        }
        if let Some(identity) = self.planned_identity() {
            if let Some(initial) = legacy_identity(&self.initial_model, &self.initial_provider) {
                if identity != initial {
                    return Err(
                        "planned identity does not match legacy initial identity".to_string()
                    );
                }
            }
        }

        Ok(())
    }

    /// Short alias for callers that use validation terminology.
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }

    /// Return a validated, canonical copy with compatibility projections
    /// filled from the attempt evidence.  No runtime store is touched.
    pub fn canonicalized(&self) -> Result<Self, String> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical.fill_identity_fields();
        Ok(canonical)
    }

    fn fill_identity_fields(&mut self) {
        let planned = self.planned_identity();
        if let Some(identity) = planned {
            self.planned_model = Some(identity.model);
            self.planned_provider = Some(identity.provider);
            if self.initial_model.trim().is_empty() {
                self.initial_model = self.planned_model.clone().unwrap_or_default();
            }
            if self.initial_provider.trim().is_empty() {
                self.initial_provider = self.planned_provider.clone().unwrap_or_default();
            }
        } else {
            self.planned_model = None;
            self.planned_provider = None;
        }

        if let Some(attempt) = self.attempts.last() {
            self.last_attempted_model = Some(attempt.candidate_model.clone());
            self.last_attempted_provider = Some(attempt.candidate_provider.clone());
        } else {
            self.last_attempted_model = None;
            self.last_attempted_provider = None;
        }

        if self.final_status != FinalStatus::Success && self.terminal_error.is_none() {
            self.terminal_error = self.attempts.last().and_then(Attempt::failure_facts);
        }

        if self.final_status == FinalStatus::Success {
            if let Some(identity) = self.served_identity() {
                self.served_model = Some(identity.model);
                self.served_provider = Some(identity.provider);
                if self.final_model.trim().is_empty() {
                    self.final_model = self.served_model.clone().unwrap_or_default();
                }
                if self.final_provider.trim().is_empty() {
                    self.final_provider = self.served_provider.clone().unwrap_or_default();
                }
            }
        } else {
            self.served_model = None;
            self.served_provider = None;
        }
    }

    /// Pure Outcome-to-Feedback bridge.  `None` means no signal was supplied;
    /// outcome success/failure never creates a rating.
    pub fn to_feedback(
        &self,
        signals: Option<Vec<crate::feedback::FeedbackSignal>>,
        timestamp: i64,
        source: crate::feedback::FeedbackSource,
        data_origin: crate::feedback::DataOrigin,
    ) -> Option<crate::feedback::Feedback> {
        crate::feedback::feedback_from_outcome(self, signals, timestamp, source, data_origin)
    }

    /// Checked variant of [`Outcome::to_feedback`].
    pub fn try_to_feedback(
        &self,
        signals: Option<Vec<crate::feedback::FeedbackSignal>>,
        timestamp: i64,
        source: crate::feedback::FeedbackSource,
        data_origin: crate::feedback::DataOrigin,
    ) -> Result<Option<crate::feedback::Feedback>, String> {
        crate::feedback::try_feedback_from_outcome(self, signals, timestamp, source, data_origin)
    }

    /// Convenience form for a concrete signal list.
    pub fn to_feedback_with_signals(
        &self,
        signals: Vec<crate::feedback::FeedbackSignal>,
        timestamp: i64,
        source: crate::feedback::FeedbackSource,
        data_origin: crate::feedback::DataOrigin,
    ) -> Option<crate::feedback::Feedback> {
        self.to_feedback(Some(signals), timestamp, source, data_origin)
    }

    /// Write outcome data to the observation store and stats store.
    ///
    /// For each attempt, records success or classified failure against the
    /// corresponding (model, provider) pair. This is the single fan-out point
    /// that keeps the two stores in sync.
    pub fn record_to_observation(&self, obs_store: &ObservationStore, stats_store: &StatsStore) {
        for attempt in &self.attempts {
            if attempt.is_terminal_success() {
                obs_store.record_success(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    attempt.latency_ms,
                    attempt.ttft_ms,
                );
                stats_store.record_success(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    attempt.latency_ms,
                    attempt.ttft_ms,
                );
            } else {
                let class = attempt.failure_class.unwrap_or(FailureClass::Unknown);
                obs_store.record_classified_failure(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    &crate::failure::ClassifiedFailure {
                        class,
                        status: attempt.http_status,
                        message: attempt.failure_message.clone().unwrap_or_default(),
                        impact: class.impact(),
                    },
                );
                stats_store.record_classified_failure(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    class,
                );
            }
        }
    }
}

impl OutcomeBuilder {
    /// Set the decision id (link to route planning).
    pub fn decision_id(mut self, id: impl Into<String>) -> Self {
        self.outcome.decision_id = Some(id.into());
        self
    }

    /// Set the response store id.
    pub fn response_id(mut self, id: impl Into<String>) -> Self {
        self.outcome.response_id = Some(id.into());
        self
    }

    /// Set the planned candidate (the first model/provider selected).
    pub fn initial(mut self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        let model = model.into();
        let provider = provider.into();
        self.outcome.initial_model = model.clone();
        self.outcome.initial_provider = provider.clone();
        self.outcome.planned_model = Some(model);
        self.outcome.planned_provider = Some(provider);
        self
    }

    /// Explicit spelling for the planned identity.
    pub fn planned(self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        self.initial(model, provider)
    }

    /// Explicit spelling for the planned identity used by adapters.
    pub fn planned_candidate(self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        self.initial(model, provider)
    }

    /// Set the final candidate (model/provider that served the response).
    pub fn final_candidate(
        mut self,
        model: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        self.outcome.final_model = model.into();
        self.outcome.final_provider = provider.into();
        self
    }

    /// Set the final served identity as explicit evidence.  The builder still
    /// refuses to retain it on a non-success terminal state.
    pub fn served_candidate(
        mut self,
        model: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        self.outcome.served_model = Some(model.into());
        self.outcome.served_provider = Some(provider.into());
        self
    }

    /// Alias for the ADR terminology.
    pub fn final_served(self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        self.served_candidate(model, provider)
    }

    /// Capture accepted terminal failure facts.  The class is supplied by the
    /// caller; this method never reclassifies a message or status code.
    pub fn failure(
        mut self,
        class: FailureClass,
        message: impl Into<String>,
        http_status: Option<u16>,
    ) -> Self {
        self.outcome.terminal_error =
            Some(FailureFacts::new(class, Some(message.into()), http_status));
        self.terminal_override = Some(match class {
            FailureClass::ClientCancelled => FinalStatus::Cancelled,
            FailureClass::Interrupted => FinalStatus::Interrupted,
            _ => FinalStatus::Failed,
        });
        self
    }

    /// Convenience form for a terminal failure without an HTTP status.
    pub fn error(self, class: FailureClass, message: impl Into<String>) -> Self {
        self.failure(class, message, None)
    }

    /// Explicit alias for adapters that call the field terminal failure facts.
    pub fn terminal_failure(
        self,
        class: FailureClass,
        message: impl Into<String>,
        http_status: Option<u16>,
    ) -> Self {
        self.failure(class, message, http_status)
    }

    /// Explicit alias for failure-class-oriented callers.
    pub fn failure_class(
        self,
        class: FailureClass,
        message: impl Into<String>,
        http_status: Option<u16>,
    ) -> Self {
        self.failure(class, message, http_status)
    }

    /// Capture an already-classified failure without running classification
    /// again.
    pub fn classified_failure(self, failure: ClassifiedFailure) -> Self {
        self.failure(failure.class, failure.message, failure.status)
    }

    pub fn failure_facts(mut self, facts: FailureFacts) -> Self {
        self.terminal_override = Some(match facts.class {
            FailureClass::ClientCancelled => FinalStatus::Cancelled,
            FailureClass::Interrupted => FinalStatus::Interrupted,
            _ => FinalStatus::Failed,
        });
        self.outcome.terminal_error = Some(facts);
        self
    }

    /// Set an explicit terminal status without inventing a failure class.
    pub fn terminal_status(mut self, status: FinalStatus) -> Self {
        self.terminal_override = Some(status);
        self.outcome.success = status == FinalStatus::Success;
        self
    }

    /// Set both initial and final to the same candidate (no fallback).
    pub fn single_candidate(
        mut self,
        model: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        let model = model.into();
        let provider = provider.into();
        self.outcome.initial_model = model.clone();
        self.outcome.initial_provider = provider.clone();
        self.outcome.planned_model = Some(model.clone());
        self.outcome.planned_provider = Some(provider.clone());
        self.outcome.final_model = model;
        self.outcome.final_provider = provider;
        self
    }

    /// Add an attempt to the outcome.
    pub fn attempt(mut self, attempt: Attempt) -> Self {
        self.outcome.attempts.push(attempt);
        self
    }

    /// Set the fallback count explicitly.
    pub fn fallback_count(mut self, count: u32) -> Self {
        self.outcome.fallback_count = count;
        self
    }

    /// Set total latency in milliseconds.
    pub fn total_latency_ms(mut self, ms: f64) -> Self {
        self.outcome.total_latency_ms = ms;
        self
    }

    /// Set time to first token (from the successful attempt).
    pub fn ttft_ms(mut self, ms: f64) -> Self {
        self.outcome.ttft_ms = Some(ms);
        self
    }

    /// Set token usage.
    pub fn usage(mut self, usage: Usage) -> Self {
        self.outcome.usage = Some(usage);
        self
    }

    /// Set cost estimates.
    pub fn cost(mut self, estimated: Option<f64>, actual: Option<f64>) -> Self {
        self.outcome.estimated_cost = estimated;
        self.outcome.actual_cost = actual;
        self
    }

    /// Mark whether the request was streaming.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.outcome.streaming = streaming;
        self
    }

    /// Set the dialect (API flavour).
    pub fn dialect(mut self, dialect: impl Into<String>) -> Self {
        self.outcome.dialect = dialect.into();
        self
    }

    /// Set the request timestamp (unix seconds).
    pub fn timestamp(mut self, ts: i64) -> Self {
        self.outcome.timestamp = ts;
        self
    }

    /// Mark the outcome as cancelled.
    pub fn cancelled(mut self) -> Self {
        self.outcome.success = false;
        self.outcome.final_status = FinalStatus::Cancelled;
        self.terminal_override = Some(FinalStatus::Cancelled);
        self
    }

    /// Alias for [`OutcomeBuilder::cancelled`].
    pub fn cancel(self) -> Self {
        self.cancelled()
    }

    /// Mark the outcome as an interrupted stream.  Partial usage/cost facts do
    /// not change this terminal state.
    pub fn interrupted(mut self) -> Self {
        self.outcome.success = false;
        self.outcome.final_status = FinalStatus::Interrupted;
        self.terminal_override = Some(FinalStatus::Interrupted);
        self
    }

    /// Finalize the outcome.
    ///
    /// Automatically computes `success`, `final_status` (via [`Outcome::classify_final`]),
    /// identity evidence, and `fallback_count` if not explicitly set.
    pub fn build(mut self) -> Outcome {
        let status = self.terminal_override.unwrap_or_else(|| {
            let cancelled = self.outcome.final_status == FinalStatus::Cancelled
                || self
                    .outcome
                    .terminal_error
                    .as_ref()
                    .is_some_and(|facts| facts.class == FailureClass::ClientCancelled);
            Outcome::classify_final(&self.outcome.attempts, cancelled)
        });
        self.outcome.final_status = status;
        self.outcome.success = status == FinalStatus::Success;
        if status != FinalStatus::Success {
            self.outcome.served_model = None;
            self.outcome.served_provider = None;
        }

        // Auto-compute fallback count from attempts if not explicitly set and
        // the value is still the default (0).
        if self.outcome.fallback_count == 0 && self.outcome.attempts.len() > 1 {
            // Count transitions between distinct (model, provider) pairs.
            let mut count = 0u32;
            for window in self.outcome.attempts.windows(2) {
                if window[0].candidate_model != window[1].candidate_model
                    || window[0].candidate_provider != window[1].candidate_provider
                {
                    count += 1;
                }
            }
            self.outcome.fallback_count = count;
        }
        self.outcome.fill_identity_fields();
        self.outcome
    }

    /// Build and validate an outcome at the schema boundary.
    pub fn try_build(self) -> Result<Outcome, String> {
        let outcome = self.build();
        outcome.validate()?;
        Ok(outcome)
    }
}

fn legacy_identity(model: &str, provider: &str) -> Option<CandidateIdentity> {
    if model.trim().is_empty() && provider.trim().is_empty() {
        None
    } else {
        Some(CandidateIdentity::new(model, provider))
    }
}

fn identity_from_parts(model: Option<&str>, provider: Option<&str>) -> Option<CandidateIdentity> {
    match (model, provider) {
        (Some(model), Some(provider))
            if !model.trim().is_empty() && !provider.trim().is_empty() =>
        {
            Some(CandidateIdentity::new(model, provider))
        }
        _ => None,
    }
}

fn validate_identity_pair(label: &str, model: &str, provider: &str) -> Result<(), String> {
    if model.trim().is_empty() != provider.trim().is_empty() {
        return Err(format!(
            "{label} model/provider must be both present or both absent"
        ));
    }
    Ok(())
}

fn validate_optional_identity_pair(
    label: &str,
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<(), String> {
    if model.is_some() != provider.is_some() {
        return Err(format!(
            "{label} model/provider must be both present or both absent"
        ));
    }
    if let (Some(model), Some(provider)) = (model, provider) {
        if model.trim().is_empty() || provider.trim().is_empty() {
            return Err(format!("{label} identity must not contain empty values"));
        }
    }
    Ok(())
}

fn validate_non_negative_finite(label: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{label} must be finite and non-negative: {value}"));
    }
    Ok(())
}

fn validate_optional_non_negative_finite(label: &str, value: Option<f64>) -> Result<(), String> {
    if let Some(value) = value {
        validate_non_negative_finite(label, value)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_attempt(
        model: &str,
        provider: &str,
        success: bool,
        latency_ms: f64,
        failure_class: Option<FailureClass>,
    ) -> Attempt {
        Attempt {
            attempt_id: format!("att_{}", uuid::Uuid::new_v4().simple()),
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            started_at: 1_700_000_000,
            completed_at: 1_700_000_001,
            latency_ms,
            ttft_ms: if success {
                Some(latency_ms * 0.3)
            } else {
                None
            },
            success,
            failure_class,
            failure_message: if success {
                None
            } else {
                Some("test failure".to_string())
            },
            http_status: if success { Some(200) } else { Some(500) },
            rectified: false,
        }
    }

    // -- Success outcome construction --

    #[test]
    fn success_outcome_single_attempt() {
        let outcome = Outcome::builder("req_1")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(true)
            .attempt(make_attempt("gpt-4", "openai", true, 350.0, None))
            .total_latency_ms(350.0)
            .ttft_ms(105.0)
            .usage(Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Usage::default()
            })
            .build();

        assert!(outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Success);
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(outcome.fallback_count, 0);
        assert_eq!(outcome.initial_model, "gpt-4");
        assert_eq!(outcome.final_model, "gpt-4");
        assert_eq!(outcome.total_latency_ms, 350.0);
        assert_eq!(outcome.ttft_ms, Some(105.0));
        assert!(outcome.usage.is_some());
        assert_eq!(outcome.usage.as_ref().unwrap().output_tokens, 50);
        assert!(outcome.outcome_id.starts_with("out_"));
    }

    // -- Failure outcome with single attempt --

    #[test]
    fn failure_outcome_single_attempt() {
        let outcome = Outcome::builder("req_2")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(false)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                120.0,
                Some(FailureClass::RateLimit),
            ))
            .total_latency_ms(120.0)
            .build();

        assert!(!outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Failed);
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(outcome.fallback_count, 0);
        assert!(outcome.usage.is_none());
        assert_eq!(
            outcome.attempts[0].failure_class,
            Some(FailureClass::RateLimit)
        );
    }

    // -- Outcome with multiple attempts (fallback) --

    #[test]
    fn fallback_outcome_two_attempts() {
        let outcome = Outcome::builder("req_3")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("anthropic")
            .streaming(true)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                200.0,
                Some(FailureClass::ProviderUnavailable),
            ))
            .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
            .total_latency_ms(600.0)
            .ttft_ms(120.0)
            .usage(Usage {
                input_tokens: 200,
                output_tokens: 100,
                ..Usage::default()
            })
            .build();

        assert!(outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Success);
        assert_eq!(outcome.attempts.len(), 2);
        assert_eq!(outcome.fallback_count, 1);
        assert_eq!(outcome.initial_model, "gpt-4");
        assert_eq!(outcome.final_model, "claude-3");
        assert_eq!(outcome.total_latency_ms, 600.0);
    }

    #[test]
    fn fallback_all_fail() {
        let outcome = Outcome::builder("req_4")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("openai")
            .streaming(false)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                100.0,
                Some(FailureClass::Transport),
            ))
            .attempt(make_attempt(
                "claude-3",
                "anthropic",
                false,
                150.0,
                Some(FailureClass::Timeout),
            ))
            .total_latency_ms(250.0)
            .build();

        assert!(!outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Failed);
        assert_eq!(outcome.fallback_count, 1);
        assert_eq!(
            outcome.attempts[0].failure_class,
            Some(FailureClass::Transport)
        );
        assert_eq!(
            outcome.attempts[1].failure_class,
            Some(FailureClass::Timeout)
        );
    }

    // -- FinalStatus classification --

    #[test]
    fn classify_final_success() {
        let attempts = vec![
            make_attempt("m", "p", false, 100.0, Some(FailureClass::Timeout)),
            make_attempt("m2", "p2", true, 200.0, None),
        ];
        assert_eq!(
            Outcome::classify_final(&attempts, false),
            FinalStatus::Success
        );
    }

    #[test]
    fn classify_final_failed() {
        let attempts = vec![make_attempt(
            "m",
            "p",
            false,
            100.0,
            Some(FailureClass::Transport),
        )];
        assert_eq!(
            Outcome::classify_final(&attempts, false),
            FinalStatus::Failed
        );
    }

    #[test]
    fn classify_final_cancelled_flag() {
        let attempts = vec![make_attempt(
            "m",
            "p",
            false,
            100.0,
            Some(FailureClass::Timeout),
        )];
        assert_eq!(
            Outcome::classify_final(&attempts, true),
            FinalStatus::Cancelled
        );
    }

    #[test]
    fn classify_final_client_cancelled_in_attempt() {
        let attempts = vec![make_attempt(
            "m",
            "p",
            false,
            50.0,
            Some(FailureClass::ClientCancelled),
        )];
        assert_eq!(
            Outcome::classify_final(&attempts, false),
            FinalStatus::Cancelled
        );
    }

    #[test]
    fn classify_final_empty_attempts() {
        assert_eq!(Outcome::classify_final(&[], false), FinalStatus::Failed);
    }

    // -- Outcome serde round-trip --

    #[test]
    fn serde_round_trip() {
        let outcome = Outcome::builder("req_rt")
            .decision_id("dec_123")
            .response_id("resp_456")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(true)
            .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
            .total_latency_ms(300.0)
            .ttft_ms(90.0)
            .usage(Usage {
                input_tokens: 50,
                output_tokens: 30,
                ..Usage::default()
            })
            .cost(Some(0.01), Some(0.009))
            .build();

        let json = serde_json::to_string(&outcome).expect("serialize");
        let restored: Outcome = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.outcome_id, outcome.outcome_id);
        assert_eq!(restored.decision_id, Some("dec_123".to_string()));
        assert_eq!(restored.response_id, Some("resp_456".to_string()));
        assert_eq!(restored.request_id, "req_rt");
        assert!(restored.success);
        assert_eq!(restored.final_status, FinalStatus::Success);
        assert_eq!(restored.attempts.len(), 1);
        assert_eq!(restored.dialect, "openai");
        assert!(restored.streaming);
        assert_eq!(restored.estimated_cost, Some(0.01));
        assert_eq!(restored.actual_cost, Some(0.009));
    }

    #[test]
    fn final_status_serde_variants() {
        let variants = [
            FinalStatus::Success,
            FinalStatus::Failed,
            FinalStatus::Cancelled,
            FinalStatus::Interrupted,
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            let restored: FinalStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, restored);
        }
        // Check wire format.
        assert_eq!(
            serde_json::to_string(&FinalStatus::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::to_string(&FinalStatus::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&FinalStatus::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::to_string(&FinalStatus::Interrupted).unwrap(),
            "\"interrupted\""
        );
    }

    // -- record_to_observation writes to both stores --

    #[test]
    fn record_to_observation_writes_success() {
        let obs = ObservationStore::new();
        let stats = StatsStore::new();

        let outcome = Outcome::builder("req_obs1")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
            .total_latency_ms(300.0)
            .build();

        outcome.record_to_observation(&obs, &stats);

        let obs_entry = obs.get("gpt-4", "openai");
        assert_eq!(obs_entry.health.total_requests, 1);
        assert_eq!(obs_entry.health.total_failures, 0);
        assert!(obs_entry.latency.total_ms.is_known());

        let stats_entry = stats.get("gpt-4", "openai");
        assert_eq!(stats_entry.total_requests, 1);
        assert_eq!(stats_entry.total_successes, 1);
        assert_eq!(stats_entry.total_failures, 0);
    }

    #[test]
    fn record_to_observation_writes_failure() {
        let obs = ObservationStore::new();
        let stats = StatsStore::new();

        let outcome = Outcome::builder("req_obs2")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                100.0,
                Some(FailureClass::RateLimit),
            ))
            .total_latency_ms(100.0)
            .build();

        outcome.record_to_observation(&obs, &stats);

        let obs_entry = obs.get("gpt-4", "openai");
        assert_eq!(obs_entry.health.total_requests, 1);
        assert_eq!(obs_entry.health.total_failures, 1);

        let stats_entry = stats.get("gpt-4", "openai");
        assert_eq!(stats_entry.total_requests, 1);
        assert_eq!(stats_entry.total_successes, 0);
        assert_eq!(stats_entry.total_failures, 1);
        assert_eq!(stats_entry.failures.count(FailureClass::RateLimit), 1);
    }

    #[test]
    fn record_to_observation_writes_fallback_chain() {
        let obs = ObservationStore::new();
        let stats = StatsStore::new();

        let outcome = Outcome::builder("req_obs3")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("openai")
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                200.0,
                Some(FailureClass::ProviderUnavailable),
            ))
            .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
            .total_latency_ms(600.0)
            .build();

        outcome.record_to_observation(&obs, &stats);

        // OpenAI: 1 failure
        let obs_openai = obs.get("gpt-4", "openai");
        assert_eq!(obs_openai.health.total_failures, 1);
        let stats_openai = stats.get("gpt-4", "openai");
        assert_eq!(stats_openai.total_failures, 1);

        // Anthropic: 1 success
        let obs_anthropic = obs.get("claude-3", "anthropic");
        assert_eq!(obs_anthropic.health.total_requests, 1);
        assert_eq!(obs_anthropic.health.total_failures, 0);
        let stats_anthropic = stats.get("claude-3", "anthropic");
        assert_eq!(stats_anthropic.total_successes, 1);
        assert!(stats_anthropic.total_latency.sample_count() > 0);
    }

    // -- Edge cases --

    #[test]
    fn builder_sets_timestamp() {
        let before = chrono::Utc::now().timestamp();
        let outcome = Outcome::builder("req_ts")
            .single_candidate("m", "p")
            .dialect("openai")
            .build();
        let after = chrono::Utc::now().timestamp();
        assert!(outcome.timestamp >= before && outcome.timestamp <= after);
    }

    #[test]
    fn explicit_timestamp_override() {
        let outcome = Outcome::builder("req_ts2")
            .single_candidate("m", "p")
            .dialect("openai")
            .timestamp(1_700_000_000)
            .build();
        assert_eq!(outcome.timestamp, 1_700_000_000);
    }

    #[test]
    fn cancelled_builder_overrides_status() {
        let outcome = Outcome::builder("req_c")
            .single_candidate("m", "p")
            .dialect("openai")
            .cancelled()
            .build();
        assert!(!outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    }

    #[test]
    fn explicit_fallback_count_is_preserved() {
        let outcome = Outcome::builder("req_fc")
            .initial("m1", "p1")
            .final_candidate("m2", "p2")
            .dialect("openai")
            .attempt(make_attempt(
                "m1",
                "p1",
                false,
                100.0,
                Some(FailureClass::Timeout),
            ))
            .attempt(make_attempt("m2", "p2", true, 200.0, None))
            .fallback_count(5) // deliberately wrong, but explicit
            .build();
        assert_eq!(
            outcome.fallback_count, 5,
            "explicit fallback_count must be preserved"
        );
    }

    #[test]
    fn attempt_fields_round_trip() {
        let a = Attempt {
            attempt_id: "att_test".to_string(),
            candidate_model: "m".to_string(),
            candidate_provider: "p".to_string(),
            started_at: 100,
            completed_at: 200,
            latency_ms: 100.0,
            ttft_ms: Some(30.0),
            success: true,
            failure_class: None,
            failure_message: None,
            http_status: Some(200),
            rectified: true,
        };
        let json = serde_json::to_string(&a).unwrap();
        let restored: Attempt = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.attempt_id, "att_test");
        assert!(restored.rectified);
        assert_eq!(restored.http_status, Some(200));
    }

    #[test]
    fn outcome_ids_are_unique() {
        let o1 = Outcome::builder("r1")
            .single_candidate("m", "p")
            .dialect("openai")
            .build();
        let o2 = Outcome::builder("r2")
            .single_candidate("m", "p")
            .dialect("openai")
            .build();
        assert_ne!(o1.outcome_id, o2.outcome_id);
    }
}
