use serde::{Deserialize, Serialize};

use super::conversion::{deterministic_sample_id, request_identity, try_outcome_sample};

use crate::feedback::{DataOrigin, Feedback, FeedbackSignal};
use crate::ir::Usage;
use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
use crate::outcome::{Attempt, FinalStatus, Outcome, OutcomeIdentity};

// ---------------------------------------------------------------------------
// TrainingSample — the core training unit
// ---------------------------------------------------------------------------

/// A single training sample combining features, outcome, and target labels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingSample {
    pub sample_id: String,
    pub schema_version: u32,
    pub timestamp: i64,
    /// Feature snapshot at decision time.
    pub features: RoutingFeatures,
    /// Target labels derived from the outcome.
    pub targets: Targets,
    /// Provider+model identity for this sample.
    pub provider_id: String,
    pub model_id: String,
    /// Data provenance.
    pub origin: DataOrigin,
    /// Link back to the outcome.
    pub outcome_id: String,
    /// Feedback signals (if any).  An empty vector means no signal; it is
    /// never interpreted as a fabricated positive or negative rating.
    pub feedback: Vec<FeedbackSignal>,
}

/// Whether a sample describes the request as a whole or one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleScope {
    Request,
    Attempt { index: usize, attempt_id: String },
}

/// The canonical, lossless Outcome-to-ML sample boundary.
///
/// The older [`TrainingSample`] remains available for the accepted shadow and
/// replay consumers, but this type is the schema bridge for new Core-owned
/// Outcome records.  It keeps the full attempt evidence, usage/cost facts,
/// identity roles, terminal status, and optional Feedback together with the
/// fixed-size feature vector consumed by ML code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutcomeTrainingSample {
    pub sample_id: String,
    pub schema_version: u32,
    pub timestamp: i64,
    pub streaming: bool,
    pub dialect: String,
    pub features: RoutingFeatures,
    pub targets: Targets,
    pub provider_id: String,
    pub model_id: String,
    pub origin: DataOrigin,
    pub outcome_id: String,
    pub request_id: String,
    pub decision_id: Option<String>,
    pub response_id: Option<String>,
    pub final_status: FinalStatus,
    /// The sample-level success target.  For an attempt sample this is the
    /// attempt result; for a request sample it is the terminal result.
    pub success: bool,
    pub identity: OutcomeIdentity,
    pub scope: SampleScope,
    pub attempt_id: Option<String>,
    pub rectified: bool,
    /// Full ordered evidence is retained so a consumer can audit attribution.
    pub attempts: Vec<Attempt>,
    pub usage: Option<Usage>,
    pub estimated_cost: Option<f64>,
    pub actual_cost: Option<f64>,
    /// Captured terminal error facts, if the request did not succeed.
    pub terminal_error: Option<crate::outcome::FailureFacts>,
    /// `None` means no user/system signal was supplied.
    pub feedback: Option<Feedback>,
}

/// Descriptive aliases for callers that prefer the term "canonical sample".
pub type CanonicalTrainingSample = OutcomeTrainingSample;
pub type OutcomeDatasetSample = OutcomeTrainingSample;

impl TryFrom<&Outcome> for TrainingSample {
    type Error = String;

    fn try_from(outcome: &Outcome) -> Result<Self, Self::Error> {
        Self::from_outcome(outcome, RoutingFeatures::default(), DataOrigin::Native)
    }
}

impl From<OutcomeTrainingSample> for TrainingSample {
    fn from(sample: OutcomeTrainingSample) -> Self {
        sample.into_legacy()
    }
}

impl OutcomeTrainingSample {
    pub fn try_from_outcome(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> Result<Self, String> {
        try_outcome_sample(outcome, features, origin, None)
    }

    pub fn feedback_signals(&self) -> &[FeedbackSignal] {
        self.feedback
            .as_ref()
            .map(|feedback| feedback.signals.as_slice())
            .unwrap_or(&[])
    }

    pub fn has_feedback(&self) -> bool {
        self.feedback
            .as_ref()
            .is_some_and(|feedback| !feedback.is_empty())
    }
}

// ---------------------------------------------------------------------------
// Targets — what the models learn to predict
// ---------------------------------------------------------------------------

/// Training targets derived from an [`Outcome`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Targets {
    /// Binary: did the request succeed?
    pub success: bool,
    /// Regression: actual latency in ms (None if failed before completion).
    pub latency_ms: Option<f64>,
    /// Regression: actual TTFT in ms (None if not streaming or failed).
    pub ttft_ms: Option<f64>,
    /// Regression: actual cost (None if unknown).
    pub cost: Option<f64>,
    /// Classification: failure class (None if success).
    pub failure_class: Option<String>,
    /// Ordinal: fallback count.
    pub fallback_count: u32,
}

impl Targets {
    /// Extract targets from a validated Outcome.
    pub fn try_from_outcome(outcome: &Outcome) -> Result<Self, String> {
        outcome.validate()?;
        Ok(Self::from_validated_outcome(outcome))
    }

    /// Compatibility form that fails closed for malformed outcomes.  It keeps
    /// the historical Debug spelling of failure classes for existing consumers.
    pub fn from_outcome(outcome: &Outcome) -> Self {
        Self::try_from_outcome(outcome).unwrap_or(Self {
            success: false,
            latency_ms: None,
            ttft_ms: None,
            cost: None,
            failure_class: None,
            fallback_count: outcome.fallback_count,
        })
    }

    fn from_validated_outcome(outcome: &Outcome) -> Self {
        let success = outcome.is_terminal_success();
        Self {
            success,
            latency_ms: success.then_some(outcome.total_latency_ms),
            ttft_ms: if success { outcome.ttft_ms } else { None },
            // Cost is a captured fact even for a non-success terminal result;
            // it must not be used to infer success.
            cost: outcome.actual_cost,
            failure_class: if success {
                None
            } else {
                outcome
                    .terminal_failure_facts()
                    .map(|facts| format!("{:?}", facts.class))
            },
            fallback_count: outcome.fallback_count,
        }
    }

    pub(super) fn from_attempt(attempt: &Attempt) -> Self {
        let success = attempt.is_terminal_success();
        Self {
            success,
            latency_ms: if success {
                Some(attempt.latency_ms)
            } else {
                None
            },
            ttft_ms: if success { attempt.ttft_ms } else { None },
            // The attempt's own recorded cost, not a hard-coded `None`.
            //
            // It used to be `None` unconditionally, which made every
            // attempt-scoped sample cost-free. The routing comparison reads *only*
            // attempt samples, so `mean_cost` was a structural constant for every
            // arm, `RewardPolicy::cost_weight` contributed nothing to the observed
            // utility the promotion gate reads, and the gate's cost budget was
            // measured against zero — while the request-scoped sample beside it
            // carried a perfectly good figure that nothing consumed.
            //
            // `None` still means unattributed, which is not the same as zero: a
            // provider that reports no usage on a failed call has told us nothing
            // about whether that call was billed.
            cost: attempt.cost,
            failure_class: attempt.failure_class.map(|class| format!("{:?}", class)),
            fallback_count: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// SampleBuilder — constructs TrainingSample from runtime data
// ---------------------------------------------------------------------------

/// Builds a [`TrainingSample`] from an [`Outcome`] and feature snapshot.
pub struct SampleBuilder;

impl SampleBuilder {
    /// Build a legacy `TrainingSample` deterministically from an Outcome.
    ///
    /// The checked form is preferred at new boundaries.  The infallible form
    /// remains for compatibility and fails closed for malformed input.
    pub fn build(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> TrainingSample {
        Self::try_build(outcome, features.clone(), origin)
            .unwrap_or_else(|_| Self::failed_closed(outcome, features, origin))
    }

    pub fn try_build(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> Result<TrainingSample, String> {
        let sample = try_outcome_sample(outcome, features, origin, None)?;
        Ok(sample.into_legacy())
    }

    pub fn try_build_with_feedback(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
        feedback: Option<&Feedback>,
    ) -> Result<TrainingSample, String> {
        let sample = try_outcome_sample(outcome, features, origin, feedback)?;
        Ok(sample.into_legacy())
    }

    pub fn build_with_feedback(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
        feedback: Option<&Feedback>,
    ) -> TrainingSample {
        Self::try_build_with_feedback(outcome, features.clone(), origin, feedback)
            .unwrap_or_else(|_| Self::failed_closed(outcome, features, origin))
    }

    fn failed_closed(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> TrainingSample {
        let identity = request_identity(outcome);
        TrainingSample {
            sample_id: deterministic_sample_id(outcome, "request"),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: outcome.timestamp,
            features,
            targets: Targets {
                success: false,
                latency_ms: None,
                ttft_ms: None,
                cost: None,
                failure_class: None,
                fallback_count: outcome.fallback_count,
            },
            provider_id: identity.provider,
            model_id: identity.model,
            origin,
            outcome_id: outcome.outcome_id.clone(),
            feedback: Vec::new(),
        }
    }
}

impl TrainingSample {
    /// Checked compatibility construction from a canonical Outcome.
    pub fn try_from_outcome(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> Result<Self, String> {
        SampleBuilder::try_build(outcome, features, origin)
    }

    pub fn from_outcome(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> Result<Self, String> {
        Self::try_from_outcome(outcome, features, origin)
    }

    /// Attach feedback signals to this sample.
    pub fn with_feedback(mut self, feedback: Vec<FeedbackSignal>) -> Self {
        self.feedback = feedback;
        self
    }

    /// Attach only a validated, matching Feedback record.  `None` is an
    /// explicit absence and leaves the sample without fabricated signals.
    pub fn with_optional_feedback(mut self, feedback: Option<&Feedback>) -> Result<Self, String> {
        if let Some(feedback) = feedback {
            feedback.validate()?;
            // The canonical path refuses a Feedback that belongs to another
            // Outcome. Accepting one here would attach a foreign record and
            // silently drop its correlation id.
            if feedback.outcome_id != self.outcome_id {
                return Err("feedback outcome_id does not match outcome".to_string());
            }
            self.feedback = feedback.signals.clone();
        } else {
            self.feedback.clear();
        }
        Ok(self)
    }
}
