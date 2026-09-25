//! Training dataset for ML routing models.
//!
//! Combines [`RoutingFeatures`](super::features::RoutingFeatures) snapshots,
//! [`Outcome`](crate::outcome::Outcome) results, and
//! [`FeedbackSignal`](crate::feedback::FeedbackSignal) signals into
//! [`TrainingSample`] units that ML models consume.
//!
//! [`DatasetStore`] provides bounded, retention-aware storage.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::feedback::{DataOrigin, Feedback, FeedbackSignal};
use crate::ir::Usage;
use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use crate::outcome::{
    Attempt, CandidateIdentity, FinalStatus, Outcome, OutcomeIdentity,
};

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

    fn from_attempt(attempt: &Attempt) -> Self {
        let success = attempt.is_terminal_success();
        Self {
            success,
            latency_ms: if success {
                Some(attempt.latency_ms)
            } else {
                None
            },
            ttft_ms: if success {
                attempt.ttft_ms
            } else {
                None
            },
            cost: None,
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
    pub fn with_optional_feedback(
        mut self,
        feedback: Option<&Feedback>,
    ) -> Result<Self, String> {
        if let Some(feedback) = feedback {
            feedback.validate()?;
            self.feedback = feedback.signals.clone();
        } else {
            self.feedback.clear();
        }
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// Canonical Outcome -> sample conversion
// ---------------------------------------------------------------------------

fn deterministic_sample_id(outcome: &Outcome, suffix: &str) -> String {
    format!("samp-{}-{suffix}", outcome.outcome_id)
}

fn request_identity(outcome: &Outcome) -> CandidateIdentity {
    outcome
        .served_identity()
        .or_else(|| outcome.last_attempted_identity())
        .or_else(|| outcome.planned_identity())
        .unwrap_or_else(|| CandidateIdentity::new("", ""))
}

fn validate_feedback(outcome: &Outcome, feedback: Option<&Feedback>) -> Result<(), String> {
    if let Some(feedback) = feedback {
        feedback.validate()?;
        if !feedback.matches_outcome(outcome) {
            return Err("feedback outcome_id does not match outcome".to_string());
        }
    }
    Ok(())
}

/// Pure checked request-level conversion into the canonical ML sample schema.
pub fn try_outcome_sample(
    outcome: &Outcome,
    features: RoutingFeatures,
    origin: DataOrigin,
    feedback: Option<&Feedback>,
) -> Result<OutcomeTrainingSample, String> {
    let outcome = outcome.canonicalized()?;
    validate_feedback(&outcome, feedback)?;
    let identity = outcome.identity();
    let model_provider = request_identity(&outcome);
    let targets = Targets::try_from_outcome(&outcome)?;
    let sample = OutcomeTrainingSample {
        sample_id: deterministic_sample_id(&outcome, "request"),
        schema_version: FEATURE_SCHEMA_VERSION,
        timestamp: outcome.timestamp,
        streaming: outcome.streaming,
        dialect: outcome.dialect.clone(),
        features,
        targets: targets.clone(),
        provider_id: model_provider.provider,
        model_id: model_provider.model,
        origin,
        outcome_id: outcome.outcome_id.clone(),
        request_id: outcome.request_id.clone(),
        decision_id: outcome.decision_id.clone(),
        response_id: outcome.response_id.clone(),
        final_status: outcome.final_status,
        success: targets.success,
        identity,
        scope: SampleScope::Request,
        attempt_id: None,
        rectified: false,
        attempts: outcome.attempts.clone(),
        usage: outcome.usage,
        estimated_cost: outcome.estimated_cost,
        actual_cost: outcome.actual_cost,
        terminal_error: outcome.terminal_error.clone(),
        feedback: feedback.cloned(),
    };
    validate_outcome_sample(&sample)?;
    Ok(sample)
}

/// Checked request-level conversion with a compatibility-shaped result.
pub fn sample_from_outcome(
    outcome: &Outcome,
    features: RoutingFeatures,
    origin: DataOrigin,
) -> Result<TrainingSample, String> {
    SampleBuilder::try_build(outcome, features, origin)
}

pub fn outcome_to_dataset_sample(
    outcome: &Outcome,
    features: RoutingFeatures,
    origin: DataOrigin,
) -> Result<OutcomeTrainingSample, String> {
    try_outcome_sample(outcome, features, origin, None)
}

/// Checked attempt/request conversion.  Samples are ordered by attempt and
/// followed by the request-level sample; all ids are deterministic.
pub fn try_samples_from_outcome(
    outcome: &Outcome,
    feature_snapshots: &[RoutingFeatures],
    origin: DataOrigin,
) -> Result<Vec<OutcomeTrainingSample>, String> {
    try_samples_from_outcome_with_feedback(outcome, feature_snapshots, origin, None)
}

pub fn try_samples_from_outcome_with_feedback(
    outcome: &Outcome,
    feature_snapshots: &[RoutingFeatures],
    origin: DataOrigin,
    feedback: Option<&Feedback>,
) -> Result<Vec<OutcomeTrainingSample>, String> {
    let outcome = outcome.canonicalized()?;
    validate_feedback(&outcome, feedback)?;
    let identity = outcome.identity();
    let mut samples = Vec::with_capacity(outcome.attempts.len() + 1);
    for (index, attempt) in outcome.attempts.iter().enumerate() {
        let features = feature_snapshots
            .get(index)
            .cloned()
            .unwrap_or_else(RoutingFeatures::default);
        let targets = Targets::from_attempt(attempt);
        let sample = OutcomeTrainingSample {
            sample_id: deterministic_sample_id(&outcome, &format!("attempt-{index}")),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: outcome.timestamp,
            streaming: outcome.streaming,
            dialect: outcome.dialect.clone(),
            features,
            targets: targets.clone(),
            provider_id: attempt.candidate_provider.clone(),
            model_id: attempt.candidate_model.clone(),
            origin,
            outcome_id: outcome.outcome_id.clone(),
            request_id: outcome.request_id.clone(),
            decision_id: outcome.decision_id.clone(),
            response_id: outcome.response_id.clone(),
            final_status: outcome.final_status,
            success: targets.success,
            identity: identity.clone(),
            scope: SampleScope::Attempt {
                index,
                attempt_id: attempt.attempt_id.clone(),
            },
            attempt_id: Some(attempt.attempt_id.clone()),
            rectified: attempt.rectified,
            attempts: outcome.attempts.clone(),
            usage: outcome.usage,
            estimated_cost: outcome.estimated_cost,
            actual_cost: outcome.actual_cost,
            terminal_error: outcome.terminal_error.clone(),
            feedback: feedback.cloned(),
        };
        validate_outcome_sample(&sample)?;
        samples.push(sample);
    }
    let request_features = feature_snapshots
        .last()
        .cloned()
        .unwrap_or_else(RoutingFeatures::default);
    samples.push(
        try_outcome_sample(&outcome, request_features, origin, feedback)?,
    );
    Ok(samples)
}

/// Descriptive alias for the canonical pure conversion.
pub fn canonical_samples_from_outcome(
    outcome: &Outcome,
    feature_snapshots: &[RoutingFeatures],
    origin: DataOrigin,
) -> Result<Vec<OutcomeTrainingSample>, String> {
    try_samples_from_outcome(outcome, feature_snapshots, origin)
}

impl OutcomeTrainingSample {
    pub fn into_legacy(self) -> TrainingSample {
        let feedback = self.feedback_signals().to_vec();
        TrainingSample {
            sample_id: self.sample_id,
            schema_version: self.schema_version,
            timestamp: self.timestamp,
            features: self.features,
            targets: self.targets,
            provider_id: self.provider_id,
            model_id: self.model_id,
            origin: self.origin,
            outcome_id: self.outcome_id,
            feedback,
        }
    }
}

// ---------------------------------------------------------------------------
// samples_from_outcome — compatibility attempt-level attribution
// ---------------------------------------------------------------------------

/// Generate legacy training samples from a complete Outcome, one per attempt.
///
/// The original API is retained for accepted ML consumers.  It now validates
/// the Outcome, uses deterministic ids, never assigns served identity to a
/// failed request, and returns an empty vector for malformed input.
pub fn samples_from_outcome(
    outcome: &Outcome,
    feature_snapshots: &[RoutingFeatures],
    origin: DataOrigin,
) -> Vec<TrainingSample> {
    let Ok(samples) = try_samples_from_outcome(outcome, feature_snapshots, origin) else {
        return Vec::new();
    };
    let mut legacy = Vec::new();
    for sample in samples {
        let is_request = sample.scope == SampleScope::Request;
        if is_request && feature_snapshots.is_empty() {
            // Preserve the historical behavior: without a feature snapshot
            // there is no request-level feature vector to materialize.
            continue;
        }
        // The compatibility shape keeps the original index suffix.
        let mut sample = sample.into_legacy();
        if let SampleScope::Attempt { index, .. } = sample_scope_from_id(&sample.sample_id) {
            sample.sample_id = format!("samp-{}-{index}", outcome.outcome_id);
        }
        legacy.push(sample);
    }
    legacy
}

// The canonical sample has already been validated; this helper only recovers
// the old numeric suffix without retaining mutable/global state.
fn sample_scope_from_id(sample_id: &str) -> SampleScope {
    let Some(index) = sample_id.rsplit('-').next().and_then(|value| value.parse::<usize>().ok())
    else {
        return SampleScope::Request;
    };
    SampleScope::Attempt {
        index,
        attempt_id: String::new(),
    }
}

// ---------------------------------------------------------------------------
// DatasetStore — bounded storage for training samples
// ---------------------------------------------------------------------------

/// Bounded store for training samples with retention policies.
pub struct DatasetStore {
    samples: Mutex<VecDeque<TrainingSample>>,
    max_samples: usize,
    max_age_secs: i64,
}

impl DatasetStore {
    pub fn new(max_samples: usize, max_age_secs: i64) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(max_samples.min(10_000))),
            max_samples,
            max_age_secs,
        }
    }

    /// Push a sample, evicting the oldest if at capacity.
    ///
    /// Validates the sample before inserting. Returns an error if the sample
    /// has invalid targets (NaN, Inf, or negative latency/ttft/cost).
    pub fn push(&self, sample: TrainingSample) -> Result<(), String> {
        validate_sample(&sample)?;
        if let Some(latency) = sample.targets.latency_ms {
            if !latency.is_finite() || latency < 0.0 {
                return Err(format!("invalid latency_ms: {}", latency));
            }
        }
        if let Some(ttft) = sample.targets.ttft_ms {
            if !ttft.is_finite() || ttft < 0.0 {
                return Err(format!("invalid ttft_ms: {}", ttft));
            }
        }
        if let Some(cost) = sample.targets.cost {
            if !cost.is_finite() || cost < 0.0 {
                return Err(format!("invalid cost: {}", cost));
            }
        }
        let mut samples = crate::sync::lock(&self.samples);
        if samples.len() >= self.max_samples {
            samples.pop_front();
        }
        samples.push_back(sample);
        Ok(())
    }

    /// Number of samples currently stored (regardless of age).
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.samples).len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        crate::sync::lock(&self.samples).is_empty()
    }

    /// Get samples that are within the retention window.
    pub fn training_slice(&self) -> Vec<TrainingSample> {
        let now = chrono::Utc::now().timestamp();
        crate::sync::lock(&self.samples)
            .iter()
            .filter(|s| now - s.timestamp < self.max_age_secs)
            .cloned()
            .collect()
    }

    /// Clear all samples.
    pub fn clear(&self) {
        crate::sync::lock(&self.samples).clear();
    }
}

impl Default for DatasetStore {
    fn default() -> Self {
        Self::new(100_000, 30 * 24 * 3600) // 100k samples, 30 days
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a training sample.
///
/// Checks:
/// - Feature dimension matches [`FEATURE_DIMENSION`].
/// - Schema version matches [`FEATURE_SCHEMA_VERSION`].
/// - All feature values are finite (no NaN / Inf).
pub fn validate_sample(sample: &TrainingSample) -> Result<(), String> {
    if sample.features.values.len() != FEATURE_DIMENSION {
        return Err(format!(
            "feature dimension mismatch: {} vs {}",
            sample.features.values.len(),
            FEATURE_DIMENSION,
        ));
    }
    if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(format!(
            "schema version mismatch: {} vs {}",
            sample.features.schema_version, FEATURE_SCHEMA_VERSION,
        ));
    }
    for (i, v) in sample.features.values.iter().enumerate() {
        if v.is_nan() || v.is_infinite() {
            return Err(format!("feature[{}] = {} is not finite", i, v));
        }
    }
    Ok(())
}

/// Validate the canonical Outcome-to-ML sample without mutating any store.
pub fn validate_outcome_sample(sample: &OutcomeTrainingSample) -> Result<(), String> {
    if sample.sample_id.trim().is_empty() {
        return Err("sample_id must not be empty".to_string());
    }
    if sample.outcome_id.trim().is_empty() || sample.request_id.trim().is_empty() {
        return Err("canonical sample request/outcome identity is incomplete".to_string());
    }
    if sample.decision_id.as_ref().is_some_and(|id| id.trim().is_empty())
        || sample.response_id.as_ref().is_some_and(|id| id.trim().is_empty())
    {
        return Err("canonical sample correlation ids must not be empty".to_string());
    }
    if sample.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(format!(
            "schema version mismatch: {} vs {}",
            sample.schema_version, FEATURE_SCHEMA_VERSION
        ));
    }
    if sample.features.values.len() != FEATURE_DIMENSION {
        return Err("canonical sample feature dimension mismatch".to_string());
    }
    if sample.features.values.iter().any(|value| !value.is_finite()) {
        return Err("canonical sample features must be finite".to_string());
    }
    for (label, value) in [
        ("latency_ms", sample.targets.latency_ms),
        ("ttft_ms", sample.targets.ttft_ms),
        ("cost", sample.targets.cost),
    ] {
        if let Some(value) = value {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("canonical sample {label} is invalid: {value}"));
            }
        }
    }
    match &sample.scope {
        SampleScope::Request => {
            if sample.attempt_id.is_some() {
                return Err("request sample cannot carry attempt_id".to_string());
            }
            if sample.success != (sample.final_status == FinalStatus::Success) {
                return Err("request sample success disagrees with final_status".to_string());
            }
            if sample.targets.success != sample.success {
                return Err("request sample target success disagrees with sample success".to_string());
            }
            if sample.final_status == FinalStatus::Success {
                let Some(served) = &sample.identity.served else {
                    return Err("successful request sample requires served identity".to_string());
                };
                if sample.provider_id != served.provider || sample.model_id != served.model {
                    return Err("request sample provider/model does not match served identity".to_string());
                }
            } else {
                if sample.identity.served.is_some() {
                    return Err("failed request sample cannot claim served identity".to_string());
                }
                if sample.targets.latency_ms.is_some() || sample.targets.ttft_ms.is_some() {
                    return Err("non-success request sample cannot carry success timing targets".to_string());
                }
            }
        }
        SampleScope::Attempt { index, attempt_id } => {
            if *index >= sample.attempts.len() {
                return Err("attempt sample index is out of range".to_string());
            }
            if sample.attempt_id.as_deref() != Some(attempt_id.as_str()) {
                return Err("attempt sample id does not match scope".to_string());
            }
            let attempt = &sample.attempts[*index];
            if attempt.attempt_id != *attempt_id {
                return Err("attempt sample evidence id mismatch".to_string());
            }
            if sample.provider_id != attempt.candidate_provider
                || sample.model_id != attempt.candidate_model
            {
                return Err("attempt sample identity does not match attempt evidence".to_string());
            }
            if sample.success != attempt.is_terminal_success() {
                return Err("attempt sample success disagrees with attempt evidence".to_string());
            }
            if sample.targets.success != sample.success {
                return Err("attempt sample target success disagrees with sample success".to_string());
            }
        }
    }
    if sample.final_status == FinalStatus::Success && sample.terminal_error.is_some() {
        return Err("successful sample cannot carry terminal error facts".to_string());
    }
    if let Some(facts) = &sample.terminal_error {
        if let Some(last) = sample.attempts.last() {
            if last.failure_class.is_some() && last.failure_class != Some(facts.class) {
                return Err("sample terminal error disagrees with attempt evidence".to_string());
            }
        }
    }
    if let Some(feedback) = &sample.feedback {
        feedback.validate()?;
        if feedback.outcome_id != sample.outcome_id {
            return Err("canonical sample feedback outcome_id mismatch".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::FailureClass;
    use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN};
    use crate::outcome::{Attempt, Outcome};
    use crate::feedback::{DataOrigin, FeedbackSignal};

    // -- helpers --

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
            ttft_ms: if success { Some(latency_ms * 0.3) } else { None },
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

    fn success_outcome() -> Outcome {
        Outcome::builder("req_success")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(true)
            .attempt(make_attempt("gpt-4", "openai", true, 350.0, None))
            .total_latency_ms(350.0)
            .ttft_ms(105.0)
            .cost(Some(0.01), Some(0.009))
            .build()
    }

    fn failure_outcome() -> Outcome {
        Outcome::builder("req_fail")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                120.0,
                Some(FailureClass::RateLimit),
            ))
            .total_latency_ms(120.0)
            .build()
    }

    fn fallback_outcome() -> Outcome {
        Outcome::builder("req_fallback")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("openai")
            .streaming(true)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                200.0,
                Some(FailureClass::ProviderUnavailable),
            ))
            .attempt(make_attempt(
                "claude-3",
                "anthropic",
                true,
                400.0,
                None,
            ))
            .total_latency_ms(600.0)
            .ttft_ms(120.0)
            .cost(Some(0.02), Some(0.018))
            .build()
    }

    fn sample_features() -> RoutingFeatures {
        let mut f = RoutingFeatures::default();
        f.values[0] = 1.0; // streaming
        f.values[1] = 0.5; // context
        f
    }

    // -- 1. TrainingSample construction from Outcome --

    #[test]
    fn training_sample_construction_from_outcome() {
        let outcome = success_outcome();
        let features = sample_features();
        let sample = SampleBuilder::build(&outcome, features.clone(), DataOrigin::Native);

        assert!(sample.sample_id.starts_with("samp-"));
        assert_eq!(sample.schema_version, FEATURE_SCHEMA_VERSION);
        assert_eq!(sample.timestamp, outcome.timestamp);
        assert_eq!(sample.provider_id, "openai");
        assert_eq!(sample.model_id, "gpt-4");
        assert_eq!(sample.origin, DataOrigin::Native);
        assert_eq!(sample.outcome_id, outcome.outcome_id);
        assert!(sample.feedback.is_empty());
        // Features are preserved
        assert_eq!(sample.features.values[0], 1.0);
    }

    // -- 2. Targets::from_outcome for success case --

    #[test]
    fn targets_from_outcome_success() {
        let outcome = success_outcome();
        let targets = Targets::from_outcome(&outcome);

        assert!(targets.success);
        assert_eq!(targets.latency_ms, Some(350.0));
        assert_eq!(targets.ttft_ms, Some(105.0));
        assert_eq!(targets.cost, Some(0.009));
        assert!(targets.failure_class.is_none());
        assert_eq!(targets.fallback_count, 0);
    }

    // -- 3. Targets::from_outcome for failure case --

    #[test]
    fn targets_from_outcome_failure() {
        let outcome = failure_outcome();
        let targets = Targets::from_outcome(&outcome);

        assert!(!targets.success);
        assert!(targets.latency_ms.is_none());
        assert!(targets.ttft_ms.is_none());
        assert!(targets.cost.is_none());
        assert_eq!(
            targets.failure_class.as_deref(),
            Some("RateLimit")
        );
        assert_eq!(targets.fallback_count, 0);
    }

    // -- 4. Targets::from_outcome for partial failure (fallback) --

    #[test]
    fn targets_from_outcome_fallback_success() {
        let outcome = fallback_outcome();
        let targets = Targets::from_outcome(&outcome);

        assert!(targets.success, "final attempt succeeded");
        assert_eq!(targets.latency_ms, Some(600.0));
        assert_eq!(targets.ttft_ms, Some(120.0));
        assert_eq!(targets.cost, Some(0.018));
        // failure_class comes from the *last* attempt — which succeeded
        assert!(targets.failure_class.is_none());
        assert_eq!(targets.fallback_count, 1);
    }

    #[test]
    fn targets_from_outcome_fallback_all_fail() {
        let outcome = Outcome::builder("req_all_fail")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("openai")
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

        let targets = Targets::from_outcome(&outcome);
        assert!(!targets.success);
        assert!(targets.latency_ms.is_none());
        assert_eq!(targets.failure_class.as_deref(), Some("Timeout"));
        assert_eq!(targets.fallback_count, 1);
    }

    // -- 5. DatasetStore push/get/len --

    #[test]
    fn dataset_store_push_len() {
        let store = DatasetStore::new(100, 3600);
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
        store.push(sample).unwrap();

        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    // -- 6. DatasetStore eviction at capacity --

    #[test]
    fn dataset_store_eviction_at_capacity() {
        let store = DatasetStore::new(3, 3600); // capacity = 3
        let outcome = success_outcome();

        for _ in 0..5 {
            let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
            store.push(sample).unwrap();
        }

        assert_eq!(store.len(), 3, "should evict oldest to stay at capacity");
    }

    // -- 7. DatasetStore age-based filtering --

    #[test]
    fn dataset_store_age_filtering() {
        // Use a very short retention window so "old" samples are filtered.
        let store = DatasetStore::new(100, 1); // 1 second retention

        let mut outcome = success_outcome();
        // Push with a timestamp far in the past
        outcome.timestamp = chrono::Utc::now().timestamp() - 60;
        let old_sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
        store.push(old_sample).unwrap();

        // Push with current timestamp
        let fresh_outcome = success_outcome();
        let fresh_sample =
            SampleBuilder::build(&fresh_outcome, sample_features(), DataOrigin::Native);
        store.push(fresh_sample).unwrap();

        assert_eq!(store.len(), 2, "both stored");
        let slice = store.training_slice();
        assert_eq!(slice.len(), 1, "only fresh sample within retention");
    }

    // -- 8. validate_sample passes valid sample --

    #[test]
    fn validate_sample_passes_valid() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
        assert!(validate_sample(&sample).is_ok());
    }

    // -- 9. validate_sample rejects wrong dimension --
    // NOTE: Rust's type system ([f32; 32]) makes it impossible to construct a
    // RoutingFeatures with the wrong number of values at compile time, and serde
    // will reject mismatched array lengths during deserialization. This test
    // verifies that serde enforces the constraint at the serialization boundary.

    #[test]
    fn validate_sample_dimension_enforced_by_serde() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
        let mut json = serde_json::to_value(&sample).unwrap();
        // Shrink the features values array to 31 elements
        let vals = json["features"]["values"].as_array_mut().unwrap();
        vals.pop();
        let result: Result<TrainingSample, _> = serde_json::from_value(json);
        assert!(result.is_err(), "serde must reject wrong-dimension features");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("expected an array of length 32"),
            "unexpected error: {}",
            err_msg
        );
    }

    // -- 10. validate_sample rejects NaN features --

    #[test]
    fn validate_sample_rejects_nan_features() {
        let outcome = success_outcome();
        let mut features = sample_features();
        features.values[5] = f32::NAN;
        let sample = SampleBuilder::build(&outcome, features, DataOrigin::Native);
        let err = validate_sample(&sample).unwrap_err();
        assert!(err.contains("not finite"), "got: {}", err);
        assert!(err.contains("feature[5]"), "got: {}", err);
    }

    #[test]
    fn validate_sample_rejects_inf_features() {
        let outcome = success_outcome();
        let mut features = sample_features();
        features.values[10] = f32::INFINITY;
        let sample = SampleBuilder::build(&outcome, features, DataOrigin::Native);
        let err = validate_sample(&sample).unwrap_err();
        assert!(err.contains("not finite"), "got: {}", err);
    }

    // -- 11. validate_sample rejects wrong schema version --

    #[test]
    fn validate_sample_rejects_wrong_schema_version() {
        let outcome = success_outcome();
        let features = RoutingFeatures {
            schema_version: 99,
            ..Default::default()
        };
        let sample = SampleBuilder::build(&outcome, features, DataOrigin::Native);
        let err = validate_sample(&sample).unwrap_err();
        assert!(err.contains("schema version mismatch"), "got: {}", err);
    }

    // -- 12. SampleBuilder with different DataOrigins --

    #[test]
    fn sample_builder_native_origin() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
        assert_eq!(sample.origin, DataOrigin::Native);
    }

    #[test]
    fn sample_builder_imported_origin() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Imported);
        assert_eq!(sample.origin, DataOrigin::Imported);
    }

    #[test]
    fn sample_builder_synthetic_origin() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Synthetic);
        assert_eq!(sample.origin, DataOrigin::Synthetic);
    }

    // -- 13. Native vs Imported vs Synthetic provenance --

    #[test]
    fn provenance_distinguishes_origins() {
        let outcome = success_outcome();
        let native = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native);
        let imported = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Imported);
        let synthetic = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Synthetic);

        assert_ne!(native.origin, imported.origin);
        assert_ne!(native.origin, synthetic.origin);
        assert_ne!(imported.origin, synthetic.origin);
    }

    // -- 14. Serialization round-trip --

    #[test]
    fn training_sample_serde_round_trip() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(
            &outcome,
            sample_features(),
            DataOrigin::Native,
        )
        .with_feedback(vec![
            FeedbackSignal::ExplicitRating { score: 4.5 },
            FeedbackSignal::ConversationContinued,
        ]);

        let json = serde_json::to_string(&sample).unwrap();
        let restored: TrainingSample = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.sample_id, sample.sample_id);
        assert_eq!(restored.schema_version, sample.schema_version);
        assert_eq!(restored.timestamp, sample.timestamp);
        assert_eq!(restored.provider_id, sample.provider_id);
        assert_eq!(restored.model_id, sample.model_id);
        assert_eq!(restored.origin, sample.origin);
        assert_eq!(restored.outcome_id, sample.outcome_id);
        assert_eq!(restored.feedback.len(), 2);
        assert_eq!(restored.features.values.len(), FEATURE_DIMENSION);
    }

    #[test]
    fn targets_serde_round_trip() {
        let outcome = success_outcome();
        let targets = Targets::from_outcome(&outcome);
        let json = serde_json::to_string(&targets).unwrap();
        let restored: Targets = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.success, targets.success);
        assert_eq!(restored.latency_ms, targets.latency_ms);
        assert_eq!(restored.ttft_ms, targets.ttft_ms);
        assert_eq!(restored.cost, targets.cost);
        assert_eq!(restored.failure_class, targets.failure_class);
        assert_eq!(restored.fallback_count, targets.fallback_count);
    }

    // -- 15. Feature snapshot preserved in sample --

    #[test]
    fn feature_snapshot_preserved() {
        let outcome = success_outcome();
        let mut features = RoutingFeatures::default();
        features.values[0] = 1.0;
        features.values[3] = 0.75;
        features.values[8] = 0.33;
        features.values[17] = 0.95;

        let sample = SampleBuilder::build(&outcome, features.clone(), DataOrigin::Native);

        assert_eq!(sample.features.values[0], 1.0);
        assert_eq!(sample.features.values[3], 0.75);
        assert_eq!(sample.features.values[8], 0.33);
        assert_eq!(sample.features.values[17], 0.95);
        // Unknown features remain at sentinel
        assert_eq!(sample.features.values[1], UNKNOWN);
    }

    // -- with_feedback --

    #[test]
    fn with_feedback_attaches_signals() {
        let outcome = success_outcome();
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native)
            .with_feedback(vec![
                FeedbackSignal::RetryRequested,
                FeedbackSignal::ExplicitRating { score: 2.0 },
            ]);

        assert_eq!(sample.feedback.len(), 2);
        assert!(matches!(sample.feedback[0], FeedbackSignal::RetryRequested));
    }

    // -- DatasetStore clear --

    #[test]
    fn dataset_store_clear() {
        let store = DatasetStore::new(100, 3600);
        let outcome = success_outcome();
        for _ in 0..10 {
            store
                .push(SampleBuilder::build(
                    &outcome,
                    sample_features(),
                    DataOrigin::Native,
                ))
                .unwrap();
        }
        assert_eq!(store.len(), 10);
        store.clear();
        assert!(store.is_empty());
    }

    // -- DatasetStore default --

    #[test]
    fn dataset_store_default() {
        let store = DatasetStore::default();
        assert_eq!(store.len(), 0);
        // Just verify it constructs without panic
        assert!(store.is_empty());
    }

    // -- failure_class formatting --

    #[test]
    fn failure_class_debug_formatting() {
        let outcome = Outcome::builder("req_fc")
            .single_candidate("m", "p")
            .dialect("openai")
            .attempt(make_attempt("m", "p", false, 100.0, Some(FailureClass::RateLimit)))
            .attempt(make_attempt("m2", "p2", false, 100.0, Some(FailureClass::Timeout)))
            .total_latency_ms(200.0)
            .build();

        let targets = Targets::from_outcome(&outcome);
        // failure_class from last attempt
        assert_eq!(targets.failure_class.as_deref(), Some("Timeout"));
    }

    #[test]
    fn failure_class_none_for_success() {
        let outcome = success_outcome();
        let targets = Targets::from_outcome(&outcome);
        assert!(targets.failure_class.is_none());
    }

    // -- samples_from_outcome: attempt-level attribution --

    #[test]
    fn samples_from_outcome_single_attempt() {
        let outcome = success_outcome();
        let features = sample_features();
        let samples = samples_from_outcome(&outcome, std::slice::from_ref(&features), DataOrigin::Native);

        // 1 attempt sample + 1 request-level sample
        assert_eq!(samples.len(), 2, "single attempt: 1 attempt + 1 request sample");

        // Attempt sample
        let attempt_sample = &samples[0];
        assert!(attempt_sample.sample_id.starts_with("samp-"));
        assert!(attempt_sample.sample_id.contains(&outcome.outcome_id));
        assert!(attempt_sample.sample_id.ends_with("-0"));
        assert_eq!(attempt_sample.provider_id, "openai");
        assert_eq!(attempt_sample.model_id, "gpt-4");
        assert!(attempt_sample.targets.success);
        assert_eq!(attempt_sample.targets.latency_ms, Some(350.0));
        assert_eq!(attempt_sample.targets.ttft_ms, Some(105.0));
        assert_eq!(attempt_sample.targets.fallback_count, 0);
        assert!(attempt_sample.targets.failure_class.is_none());

        // Request-level sample (from SampleBuilder::build)
        let request_sample = &samples[1];
        assert_eq!(request_sample.provider_id, "openai");
        assert_eq!(request_sample.model_id, "gpt-4");
        assert!(request_sample.targets.success);
    }

    #[test]
    fn samples_from_outcome_two_attempts_fallback() {
        let outcome = fallback_outcome();
        let features_a = {
            let mut f = RoutingFeatures::default();
            f.values[0] = 1.0; // streaming
            f
        };
        let features_b = {
            let mut f = RoutingFeatures::default();
            f.values[0] = 0.5;
            f
        };
        let samples = samples_from_outcome(
            &outcome,
            &[features_a.clone(), features_b.clone()],
            DataOrigin::Native,
        );

        // 2 attempt samples + 1 request-level sample
        assert_eq!(samples.len(), 3, "two attempts: 2 attempt + 1 request sample");

        // First attempt (failed)
        let first = &samples[0];
        assert_eq!(first.provider_id, "openai");
        assert_eq!(first.model_id, "gpt-4");
        assert!(!first.targets.success, "first attempt should be failure");
        assert!(first.targets.latency_ms.is_none(), "failed attempt should have no latency");
        assert_eq!(
            first.targets.failure_class.as_deref(),
            Some("ProviderUnavailable")
        );
        assert_eq!(first.features.values[0], 1.0, "first attempt uses its own snapshot");

        // Second attempt (succeeded)
        let second = &samples[1];
        assert_eq!(second.provider_id, "anthropic");
        assert_eq!(second.model_id, "claude-3");
        assert!(second.targets.success, "second attempt should be success");
        assert_eq!(second.targets.latency_ms, Some(400.0));
        assert!(second.targets.failure_class.is_none());
        assert_eq!(second.features.values[0], 0.5, "second attempt uses its own snapshot");

        // Request-level sample
        let request = &samples[2];
        assert_eq!(request.provider_id, "anthropic");
        assert_eq!(request.model_id, "claude-3");
        assert!(request.targets.success);
        assert_eq!(request.targets.fallback_count, 1);
    }

    #[test]
    fn samples_from_outcome_three_attempts() {
        let outcome = Outcome::builder("req_three")
            .initial("gpt-4", "openai")
            .final_candidate("gemini-pro", "google")
            .dialect("openai")
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                100.0,
                Some(FailureClass::RateLimit),
            ))
            .attempt(make_attempt(
                "claude-3",
                "anthropic",
                false,
                150.0,
                Some(FailureClass::Timeout),
            ))
            .attempt(make_attempt(
                "gemini-pro",
                "google",
                true,
                200.0,
                None,
            ))
            .total_latency_ms(450.0)
            .ttft_ms(60.0)
            .build();

        let f1 = {
            let mut f = RoutingFeatures::default();
            f.values[0] = 1.0;
            f
        };
        let f2 = {
            let mut f = RoutingFeatures::default();
            f.values[0] = 0.8;
            f
        };
        let f3 = {
            let mut f = RoutingFeatures::default();
            f.values[0] = 0.6;
            f
        };
        let samples = samples_from_outcome(
            &outcome,
            &[f1.clone(), f2.clone(), f3.clone()],
            DataOrigin::Native,
        );

        // 3 attempt samples + 1 request-level sample
        assert_eq!(samples.len(), 4, "three attempts: 3 attempt + 1 request sample");

        // First attempt (failed)
        assert_eq!(samples[0].provider_id, "openai");
        assert!(!samples[0].targets.success);
        assert_eq!(samples[0].targets.failure_class.as_deref(), Some("RateLimit"));
        assert_eq!(samples[0].features.values[0], 1.0);

        // Second attempt (failed)
        assert_eq!(samples[1].provider_id, "anthropic");
        assert!(!samples[1].targets.success);
        assert_eq!(samples[1].targets.failure_class.as_deref(), Some("Timeout"));
        assert_eq!(samples[1].features.values[0], 0.8);

        // Third attempt (succeeded)
        assert_eq!(samples[2].provider_id, "google");
        assert!(samples[2].targets.success);
        assert!(samples[2].targets.failure_class.is_none());
        assert_eq!(samples[2].features.values[0], 0.6);

        // Request-level sample
        assert_eq!(samples[3].provider_id, "google");
        assert!(samples[3].targets.success);
    }

    #[test]
    fn samples_from_outcome_uses_default_features_when_missing() {
        let outcome = success_outcome();
        // Provide empty feature_snapshots — should fall back to default
        let samples = samples_from_outcome(&outcome, &[], DataOrigin::Native);

        // 1 attempt sample (with default features) + 1 request-level sample
        // But wait — no feature_snapshots means the request-level sample is
        // also skipped (feature_snapshots.last() is None).
        assert_eq!(samples.len(), 1, "no snapshots: only attempt sample, no request sample");

        let attempt = &samples[0];
        assert_eq!(attempt.provider_id, "openai");
        // Default features: all UNKNOWN
        assert_eq!(attempt.features.values[0], UNKNOWN);
    }

    #[test]
    fn samples_from_outcome_attempt_provider_model_identity() {
        // Each attempt should use its own candidate, not the final candidate
        let outcome = fallback_outcome();
        let features = vec![RoutingFeatures::default(); 2];
        let samples = samples_from_outcome(&outcome, &features, DataOrigin::Native);

        // First attempt: openai/gpt-4 (not the final anthropic/claude-3)
        assert_eq!(samples[0].provider_id, "openai");
        assert_eq!(samples[0].model_id, "gpt-4");

        // Second attempt: anthropic/claude-3
        assert_eq!(samples[1].provider_id, "anthropic");
        assert_eq!(samples[1].model_id, "claude-3");
    }

    #[test]
    fn samples_from_outcome_preserves_origin() {
        let outcome = success_outcome();
        let features = vec![sample_features()];
        let samples = samples_from_outcome(&outcome, &features, DataOrigin::Imported);

        for sample in &samples {
            assert_eq!(sample.origin, DataOrigin::Imported);
        }
    }

    #[test]
    fn samples_from_outcome_preserves_outcome_id() {
        let outcome = fallback_outcome();
        let features = vec![RoutingFeatures::default(); 2];
        let samples = samples_from_outcome(&outcome, &features, DataOrigin::Native);

        for sample in &samples {
            assert_eq!(sample.outcome_id, outcome.outcome_id);
        }
    }
}
