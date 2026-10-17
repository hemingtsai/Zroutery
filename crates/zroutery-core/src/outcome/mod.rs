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
//!
//! # Why this is eight files
//!
//! The file was 1822 lines, of which 960 were the two `impl` blocks, and those
//! blocks mixed unrelated jobs: reading identity back out, deciding whether a
//! record can be trusted, and handing it to the stores downstream. Nobody could
//! change one without reading the other two.
//!
//! `Outcome` and `OutcomeBuilder` are split files, so their inherent methods
//! are too. rustc merges the blocks and nothing about either type changes; what
//! changes is that the methods no longer sit next to each other. This table is
//! the index:
//!
//! | file | holds |
//! |---|---|
//! | `identity` | `CandidateIdentity`, `OutcomeIdentity`, `FailureFacts` |
//! | `status` | `FinalStatus` |
//! | `attempt` | `Attempt` |
//! | `accessors` | the read-only views: identity roles, model, provider, failure |
//! | `validation` | `validate`, `is_valid`, `canonicalized` |
//! | `feedback` | `to_feedback`, `try_to_feedback`, `record_to_observation` |
//! | `builder` | every `OutcomeBuilder` method |
//!
//! `builder` and `classify_final` stay here because they are the two ways into
//! the type; everything else in those files answers to one of them.

mod accessors;
mod attempt;
mod builder;
mod feedback;
mod identity;
mod status;
mod validation;

#[cfg(test)]
mod tests;

use crate::failure::FailureClass;
use crate::ir::Usage;

use serde::{Deserialize, Serialize};

pub use attempt::Attempt;
pub use identity::{
    failure_class_wire_name, CandidateIdentity, ErrorFacts, FailureFacts, OutcomeIdentity,
};
pub use status::FinalStatus;

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
}
