//! Training dataset for ML routing models.
//!
//! Combines [`RoutingFeatures`](super::features::RoutingFeatures) snapshots,
//! [`Outcome`](crate::outcome::Outcome) results, and
//! [`FeedbackSignal`](crate::feedback::FeedbackSignal) signals into
//! [`TrainingSample`] units that ML models consume.
//!
//! [`DatasetStore`] holds the canonical [`OutcomeTrainingSample`] under both a
//! count and an age bound, and ingests one request at a time through
//! [`DatasetStore::ingest`].
//!
//! # Where a sample's feature vector comes from
//!
//! The [`Outcome`](crate::outcome::Outcome) deliberately holds identity,
//! terminal state, timing and usage only — it has no feature vector, and
//! nothing here derives one. The exact per-candidate vectors captured at
//! decision time live in the retained shadow record's
//! [`ShadowInput::candidates`](super::shadow::ShadowInput), so ingestion takes
//! the retained decision-time input as an input and correlates it with the
//! request's one Outcome by request id.
//!
//! A request with no retained record — the `ml` feature off,
//! `config.shadow.enabled` false, evaluation refused, or a shadow fault — has no
//! features and therefore no sample. That case is reported as
//! [`Ingestion::NoDecisionTimeInput`] and counted separately from a refusal, so
//! "nothing was collected" is never indistinguishable from "something was
//! collected and rejected". See [`canonical_samples_from_decision_time`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::feedback::{DataOrigin, Feedback, FeedbackSignal};
use crate::ir::Usage;
use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use crate::ml::shadow::ShadowInput;
use crate::outcome::{Attempt, CandidateIdentity, FinalStatus, Outcome, OutcomeIdentity};

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
            ttft_ms: if success { attempt.ttft_ms } else { None },
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
    pub fn with_optional_feedback(mut self, feedback: Option<&Feedback>) -> Result<Self, String> {
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
    let canonical = outcome.canonicalized()?;
    request_sample(&canonical, features, origin, feedback)
}

/// The request-scope sample for an already canonical Outcome.
///
/// Separated from [`try_outcome_sample`] so the decision-time path can build the
/// request sample from the same code with a vector it looked up itself.
fn request_sample(
    outcome: &Outcome,
    features: RoutingFeatures,
    origin: DataOrigin,
    feedback: Option<&Feedback>,
) -> Result<OutcomeTrainingSample, String> {
    validate_feedback(outcome, feedback)?;
    let identity = outcome.identity();
    let model_provider = request_identity(outcome);
    let targets = Targets::try_from_outcome(outcome)?;
    let sample = OutcomeTrainingSample {
        sample_id: deterministic_sample_id(outcome, "request"),
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
    // The compatibility form tolerates a missing snapshot by substituting the
    // default vector; the decision-time path below refuses instead. `None` here
    // means "no snapshot was supplied for this attempt".
    let per_attempt: Vec<Option<RoutingFeatures>> = (0..outcome.attempts.len())
        .map(|index| feature_snapshots.get(index).cloned())
        .collect();
    let request_features = feature_snapshots.last().cloned();
    samples_from_features(
        outcome,
        &per_attempt,
        request_features,
        origin,
        feedback,
        false,
    )
}

/// Canonical samples for one request, built **only** from the feature vectors
/// retained at decision time.
///
/// `decision_time` is the request's own retained shadow input. Each attempt's
/// vector is looked up by the identity the attempt actually used, and the
/// request-scope vector by the identity the request ended on (served, else last
/// attempted, else planned — the same resolution
/// [`try_outcome_sample`] applies).
///
/// A retained vector that is absent, carries a foreign feature schema, or holds
/// a non-finite value is refused with a reason. Nothing is substituted,
/// reconstructed or re-derived: a request whose evidence is incomplete produces
/// no sample at all rather than one carrying invented features.
pub fn canonical_samples_from_decision_time(
    outcome: &Outcome,
    decision_time: &ShadowInput,
    origin: DataOrigin,
) -> Result<Vec<OutcomeTrainingSample>, String> {
    if decision_time.feature_schema != FEATURE_SCHEMA_VERSION {
        return Err(format!(
            "retained decision-time schema mismatch: {} != {}",
            decision_time.feature_schema, FEATURE_SCHEMA_VERSION
        ));
    }
    let canonical = outcome.canonicalized()?;
    let per_attempt: Vec<Option<RoutingFeatures>> = canonical
        .attempts
        .iter()
        .map(|attempt| {
            retained_features(
                decision_time,
                &attempt.candidate_model,
                &attempt.candidate_provider,
            )
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "no usable retained features for attempted candidate '{}/{}'",
                    attempt.candidate_provider, attempt.candidate_model
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let ended_on = request_identity(&canonical);
    let request_features = retained_features(decision_time, &ended_on.model, &ended_on.provider)
        .ok_or_else(|| {
            format!(
                "no usable retained features for the request's final candidate '{}/{}'",
                ended_on.provider, ended_on.model
            )
        })?;
    samples_from_features(
        &canonical,
        &per_attempt,
        Some(request_features),
        origin,
        None,
        true,
    )
}

/// The retained feature vector for one candidate identity, if it is usable.
///
/// A policy-rejected candidate is retained with the explicit UNKNOWN vector and
/// a rejection reason. That vector is a fact about the candidate (it was
/// considered and refused), not evidence about serving it, so an ineligible
/// candidate yields nothing rather than a sample full of UNKNOWNs.
fn retained_features(
    decision_time: &ShadowInput,
    model_id: &str,
    provider_id: &str,
) -> Option<RoutingFeatures> {
    decision_time
        .candidates
        .iter()
        .find(|candidate| {
            candidate.candidate_id == model_id && candidate.provider_id == provider_id
        })
        .filter(|candidate| {
            candidate.eligible
                && candidate.features.schema_version == FEATURE_SCHEMA_VERSION
                && candidate
                    .features
                    .values
                    .iter()
                    .all(|value| value.is_finite())
        })
        .map(|candidate| candidate.features.clone())
}

/// Build every canonical sample for one canonical Outcome.
///
/// `require_retained` is the difference that matters: when it is set, a missing
/// vector is an error instead of a substituted default. `per_attempt` is indexed
/// by attempt, and `request_features` is the request-scope vector.
fn samples_from_features(
    outcome: &Outcome,
    per_attempt: &[Option<RoutingFeatures>],
    request_features: Option<RoutingFeatures>,
    origin: DataOrigin,
    feedback: Option<&Feedback>,
    require_retained: bool,
) -> Result<Vec<OutcomeTrainingSample>, String> {
    validate_feedback(outcome, feedback)?;
    let identity = outcome.identity();
    let mut samples = Vec::with_capacity(outcome.attempts.len() + 1);
    for (index, attempt) in outcome.attempts.iter().enumerate() {
        let features = resolve_features(
            per_attempt.get(index).cloned().flatten(),
            require_retained,
            &format!(
                "attempt {index} ({}/{})",
                attempt.candidate_provider, attempt.candidate_model
            ),
        )?;

        let targets = Targets::from_attempt(attempt);
        let sample = OutcomeTrainingSample {
            sample_id: deterministic_sample_id(outcome, &format!("attempt-{index}")),
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
    let request_features =
        resolve_features(request_features, require_retained, "the request as a whole")?;
    samples.push(request_sample(outcome, request_features, origin, feedback)?);
    Ok(samples)
}

/// One feature vector, or an explicit refusal. A substituted default is only
/// ever allowed on the compatibility path.
fn resolve_features(
    features: Option<RoutingFeatures>,
    require_retained: bool,
    subject: &str,
) -> Result<RoutingFeatures, String> {
    match features {
        Some(features) => Ok(features),
        None if require_retained => Err(format!(
            "refusing to synthesize a feature vector for {subject}"
        )),
        None => Ok(RoutingFeatures::default()),
    }
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
    let Some(index) = sample_id
        .rsplit('-')
        .next()
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return SampleScope::Request;
    };
    SampleScope::Attempt {
        index,
        attempt_id: String::new(),
    }
}

// ---------------------------------------------------------------------------
// DatasetStore — bounded storage for canonical training samples
// ---------------------------------------------------------------------------

/// Production retention bounds.
///
/// A canonical sample carries its full attempt evidence, so the count bound is
/// deliberately far below the legacy 100k default: at roughly a kilobyte per
/// sample this caps the in-process dataset in the low tens of megabytes, which
/// is what a desktop proxy can hold without the dataset becoming the largest
/// thing in the process. Age is the second bound, because a sample's usefulness
/// is bounded by how stale the routing state it describes has become.
pub const PRODUCTION_MAX_SAMPLES: usize = 10_000;
/// Thirty days is the default age window; production uses a shorter one.
pub const PRODUCTION_MAX_AGE_SECS: i64 = 7 * 24 * 3600;

/// The outcome of one ingestion attempt, and the only way to tell the three
/// cases apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ingestion {
    /// The request produced these stored samples, in scope order.
    Ingested { sample_ids: Vec<String> },
    /// The request retained no decision-time input, so it has no features and
    /// no sample. Nothing was stored, and nothing was fabricated.
    NoDecisionTimeInput,
    /// The sample or its Outcome was refused. The reason is the boundary's own
    /// words; nothing is stored and nothing is repaired.
    Rejected { reason: String },
}

impl Ingestion {
    /// The ids stored, or an empty slice for the two non-storing cases.
    pub fn sample_ids(&self) -> &[String] {
        match self {
            Ingestion::Ingested { sample_ids } => sample_ids,
            _ => &[],
        }
    }

    /// Whether this ingestion stored at least one sample.
    pub fn is_ingested(&self) -> bool {
        matches!(self, Ingestion::Ingested { .. })
    }

    /// Whether nothing was stored because there were no decision-time features.
    pub fn is_without_decision_time_input(&self) -> bool {
        matches!(self, Ingestion::NoDecisionTimeInput)
    }
}

/// One snapshot of what the store collected, refused and evicted.
///
/// `no_decision_time_input` is the observable half of the dependency on
/// retained decision-time input: it counts requests that had no features to
/// collect from, which is a different fact from `rejected`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestionCounters {
    /// Requests that stored at least one sample.
    pub ingested: u64,
    /// Samples stored in total, across all requests.
    pub samples: u64,
    /// Requests with no retained decision-time input.
    pub no_decision_time_input: u64,
    /// Requests refused at the boundary, with a reason.
    pub rejected: u64,
    /// Samples dropped to stay inside the count bound.
    pub evicted_by_count: u64,
    /// Samples dropped for being older than the age bound.
    pub evicted_by_age: u64,
    /// Ingestion panics contained at the boundary.
    pub faults: u64,
}

/// Bounded store for canonical samples with a count and an age retention
/// policy.
///
/// The store holds [`OutcomeTrainingSample`]. The legacy [`TrainingSample`]
/// shape is reachable only through the explicit one-way
/// [`Self::legacy_training_slice`] adapter, never as the storage path.
pub struct DatasetStore {
    samples: Mutex<VecDeque<OutcomeTrainingSample>>,
    max_samples: usize,
    max_age_secs: i64,
    ingested: AtomicU64,
    samples_stored: AtomicU64,
    without_decision_time: AtomicU64,
    rejected: AtomicU64,
    evicted_by_count: AtomicU64,
    evicted_by_age: AtomicU64,
    faults: AtomicU64,
}

impl DatasetStore {
    pub fn new(max_samples: usize, max_age_secs: i64) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(max_samples.min(10_000))),
            max_samples,
            max_age_secs,
            ingested: AtomicU64::new(0),
            samples_stored: AtomicU64::new(0),
            without_decision_time: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            evicted_by_count: AtomicU64::new(0),
            evicted_by_age: AtomicU64::new(0),
            faults: AtomicU64::new(0),
        }
    }

    /// The production store: the bounds above, nothing configurable and no
    /// second feature source.
    pub fn production() -> Self {
        Self::new(PRODUCTION_MAX_SAMPLES, PRODUCTION_MAX_AGE_SECS)
    }

    /// The count bound.
    pub fn max_samples(&self) -> usize {
        self.max_samples
    }

    /// The age bound, in seconds.
    pub fn max_age_secs(&self) -> i64 {
        self.max_age_secs
    }

    /// Store one canonical sample, enforcing both retention bounds.
    ///
    /// The sample is validated first and refused with a reason if it is
    /// malformed or internally inconsistent; nothing is repaired and nothing
    /// invalid is stored. Expired samples are dropped before the insert, so a
    /// store whose traffic stops cannot keep expired samples resident.
    pub fn push(&self, sample: OutcomeTrainingSample) -> Result<(), String> {
        validate_outcome_sample(&sample)?;
        let mut samples = crate::sync::lock(&self.samples);
        self.evict_expired_locked(&mut samples, chrono::Utc::now().timestamp());
        while samples.len() >= self.max_samples {
            if samples.pop_front().is_some() {
                self.evicted_by_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        samples.push_back(sample);
        Ok(())
    }

    /// Ingest one request: the single validated Outcome plus the feature
    /// vectors retained at decision time.
    ///
    /// `request_id` is the correlation key and must be the request's own id: a
    /// caller that correlates the wrong record is refused rather than allowed
    /// to attribute one request's features to another request's outcome.
    ///
    /// `decision_time` is the request's retained shadow input, or `None` when it
    /// retained none. `None` yields [`Ingestion::NoDecisionTimeInput`] — never a
    /// sample built from re-derived or default features.
    ///
    /// All-or-nothing: the samples are built and validated together, and either
    /// every one of them is stored or none is. A request that is already
    /// represented is refused, so "one ingestion per request" is a property of
    /// the store rather than of the one caller that happens to exist today.
    pub fn ingest(
        &self,
        request_id: &str,
        outcome: &Outcome,
        decision_time: Option<&ShadowInput>,
    ) -> Ingestion {
        let Some(decision_time) = decision_time else {
            self.without_decision_time.fetch_add(1, Ordering::Relaxed);
            return Ingestion::NoDecisionTimeInput;
        };
        if outcome.request_id != request_id {
            return self.refuse(format!(
                "decision-time input correlated to request '{request_id}' does not match outcome request '{}'",
                outcome.request_id
            ));
        }
        let samples = match canonical_samples_from_decision_time(
            outcome,
            decision_time,
            DataOrigin::Native,
        ) {
            Ok(samples) => samples,
            Err(reason) => return self.refuse(reason),
        };
        let sample_ids = match self.store_all(request_id, &samples) {
            Ok(sample_ids) => sample_ids,
            Err(reason) => return self.refuse(reason),
        };
        self.ingested.fetch_add(1, Ordering::Relaxed);
        self.samples_stored
            .fetch_add(sample_ids.len() as u64, Ordering::Relaxed);
        Ingestion::Ingested { sample_ids }
    }

    /// Validate and store one whole ingestion, or none of it.
    fn store_all(
        &self,
        request_id: &str,
        samples: &[OutcomeTrainingSample],
    ) -> Result<Vec<String>, String> {
        for sample in samples {
            validate_outcome_sample(sample)?;
        }
        let mut stored = crate::sync::lock(&self.samples);
        if stored.iter().any(|sample| sample.request_id == request_id) {
            return Err(format!(
                "request '{request_id}' is already represented in the dataset"
            ));
        }
        self.evict_expired_locked(&mut stored, chrono::Utc::now().timestamp());
        // The count bound holds absolutely: a single request whose evidence
        // exceeds the whole store keeps its most recent samples and counts the
        // rest as evicted, rather than growing past the bound.
        let skipped = samples.len().saturating_sub(self.max_samples);
        let kept = &samples[skipped..];
        if skipped > 0 {
            self.evicted_by_count
                .fetch_add(skipped as u64, Ordering::Relaxed);
        }
        while stored.len() + kept.len() > self.max_samples {
            if stored.pop_front().is_some() {
                self.evicted_by_count.fetch_add(1, Ordering::Relaxed);
            } else {
                break;
            }
        }
        let sample_ids = kept.iter().map(|sample| sample.sample_id.clone()).collect();
        stored.extend(kept.iter().cloned());
        Ok(sample_ids)
    }

    /// Count one refusal and describe it.
    fn refuse(&self, reason: String) -> Ingestion {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        Ingestion::Rejected { reason }
    }

    /// Count one contained fault. Every panic this module can suffer is
    /// reported here rather than propagated.
    pub fn record_fault(&self) {
        self.faults.fetch_add(1, Ordering::Relaxed);
    }

    /// Everything the store collected, refused and evicted so far.
    pub fn counters(&self) -> IngestionCounters {
        IngestionCounters {
            ingested: self.ingested.load(Ordering::Relaxed),
            samples: self.samples_stored.load(Ordering::Relaxed),
            no_decision_time_input: self.without_decision_time.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            evicted_by_count: self.evicted_by_count.load(Ordering::Relaxed),
            evicted_by_age: self.evicted_by_age.load(Ordering::Relaxed),
            faults: self.faults.load(Ordering::Relaxed),
        }
    }

    /// Number of samples currently stored (regardless of age).
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.samples).len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        crate::sync::lock(&self.samples).is_empty()
    }

    /// Drop every sample older than the age bound and report how many left.
    pub fn evict_expired(&self) -> usize {
        let mut samples = crate::sync::lock(&self.samples);
        self.evict_expired_locked(&mut samples, chrono::Utc::now().timestamp())
    }

    fn evict_expired_locked(
        &self,
        samples: &mut VecDeque<OutcomeTrainingSample>,
        now: i64,
    ) -> usize {
        let before = samples.len();
        samples.retain(|sample| now - sample.timestamp < self.max_age_secs);
        let evicted = before - samples.len();
        if evicted > 0 {
            self.evicted_by_age
                .fetch_add(evicted as u64, Ordering::Relaxed);
        }
        evicted
    }

    /// The stored samples that are inside the retention window, oldest first.
    pub fn training_slice(&self) -> Vec<OutcomeTrainingSample> {
        let now = chrono::Utc::now().timestamp();
        crate::sync::lock(&self.samples)
            .iter()
            .filter(|sample| now - sample.timestamp < self.max_age_secs)
            .cloned()
            .collect()
    }

    /// The compatibility view of the retained samples.
    ///
    /// This is the only path from the store to the legacy [`TrainingSample`]
    /// shape, and it is one-way: nothing in this store is ever built from a
    /// legacy sample.
    pub fn legacy_training_slice(&self) -> Vec<TrainingSample> {
        self.training_slice()
            .into_iter()
            .map(OutcomeTrainingSample::into_legacy)
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

/// Run one dataset step with any panic contained and counted.
///
/// A dataset problem is a dataset problem: the boundary reports it and the
/// request continues. The closure form is what makes that testable, because the
/// store's own failure modes are all ordinary refusals.
pub fn contain_dataset_fault<T>(
    store: &DatasetStore,
    step: impl FnOnce() -> T,
) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)) {
        Ok(value) => Ok(value),
        Err(panic) => {
            store.record_fault();
            let reason = panic
                .downcast_ref::<&str>()
                .map(|reason| (*reason).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown payload".to_string());
            Err(format!("dataset fault: {reason}"))
        }
    }
}

/// [`DatasetStore::ingest`] with any panic contained and turned into an
/// observable refusal, so ingestion can never fail a request.
pub fn contained_ingest(
    store: &DatasetStore,
    request_id: &str,
    outcome: &Outcome,
    decision_time: Option<&ShadowInput>,
) -> Ingestion {
    contain_dataset_fault(store, || store.ingest(request_id, outcome, decision_time))
        .unwrap_or_else(|reason| Ingestion::Rejected { reason })
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
    if sample
        .decision_id
        .as_ref()
        .is_some_and(|id| id.trim().is_empty())
        || sample
            .response_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
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
    if sample
        .features
        .values
        .iter()
        .any(|value| !value.is_finite())
    {
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
                return Err(
                    "request sample target success disagrees with sample success".to_string(),
                );
            }
            if sample.final_status == FinalStatus::Success {
                let Some(served) = &sample.identity.served else {
                    return Err("successful request sample requires served identity".to_string());
                };
                if sample.provider_id != served.provider || sample.model_id != served.model {
                    return Err(
                        "request sample provider/model does not match served identity".to_string(),
                    );
                }
            } else {
                if sample.identity.served.is_some() {
                    return Err("failed request sample cannot claim served identity".to_string());
                }
                if sample.targets.latency_ms.is_some() || sample.targets.ttft_ms.is_some() {
                    return Err(
                        "non-success request sample cannot carry success timing targets"
                            .to_string(),
                    );
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
                return Err(
                    "attempt sample target success disagrees with sample success".to_string(),
                );
            }
            // The same discipline the request scope already enforces, and the
            // one `Targets::from_attempt` already follows: a candidate that
            // failed has no service latency to learn from. Without this, a
            // hand-built or deserialized attempt sample could carry a failed
            // attempt's duration into the latency regression head and teach the
            // model that a failure was fast. `cost` is deliberately exempt: it
            // is money actually spent, not a service-quality measurement.
            if !sample.success
                && (sample.targets.latency_ms.is_some() || sample.targets.ttft_ms.is_some())
            {
                return Err(
                    "non-success attempt sample cannot carry success timing targets".to_string(),
                );
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
    use crate::feedback::{DataOrigin, FeedbackSignal};
    use crate::ml::features::{
        RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
    };
    use crate::outcome::{Attempt, Outcome};

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
            .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
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
        assert_eq!(targets.failure_class.as_deref(), Some("RateLimit"));
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

    /// The canonical request-scope sample for a validated Outcome.
    fn canonical_sample(outcome: &Outcome) -> OutcomeTrainingSample {
        try_outcome_sample(outcome, sample_features(), DataOrigin::Native, None)
            .expect("canonical sample from a valid outcome")
    }

    #[test]
    fn dataset_store_push_len() {
        let store = DatasetStore::new(100, 3600);
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        let outcome = success_outcome();
        store.push(canonical_sample(&outcome)).unwrap();

        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn dataset_store_refuses_an_inconsistent_canonical_sample() {
        let store = DatasetStore::new(100, 3600);
        let mut sample = canonical_sample(&success_outcome());
        // A success label the terminal state does not support.
        sample.final_status = FinalStatus::Failed;
        let err = store.push(sample).unwrap_err();
        assert!(err.contains("disagrees with final_status"), "got: {err}");
        assert!(store.is_empty(), "a refused sample is never stored");
    }

    // -- 6. DatasetStore eviction at capacity --

    #[test]
    fn dataset_store_eviction_at_capacity() {
        let store = DatasetStore::new(3, 3600); // capacity = 3
        let outcome = success_outcome();

        for _ in 0..5 {
            store.push(canonical_sample(&outcome)).unwrap();
        }

        assert_eq!(store.len(), 3, "should evict oldest to stay at capacity");
        assert_eq!(store.counters().evicted_by_count, 2);
    }

    // -- 7. DatasetStore age-based filtering and eviction --

    #[test]
    fn dataset_store_age_filtering() {
        // A sample that was inside the window when it was stored and has since
        // aged out of it stays resident but is hidden from every read: the age
        // bound is enforced on read as well as on insert.
        let store = DatasetStore::new(100, 1); // 1 second retention
        let mut aged = success_outcome();
        aged.timestamp = chrono::Utc::now().timestamp() - 60;
        store.push(canonical_sample(&aged)).unwrap();

        assert_eq!(store.len(), 1, "it was inside the window on insert");
        assert!(store.training_slice().is_empty(), "and outside it now");
        assert!(store.legacy_training_slice().is_empty());

        // A fresh sample is inside the window.
        store.push(canonical_sample(&success_outcome())).unwrap();
        assert_eq!(store.training_slice().len(), 1);
    }

    #[test]
    fn dataset_store_evicts_expired_samples_physically() {
        let store = DatasetStore::new(100, 60);
        let mut outcome = success_outcome();
        outcome.timestamp = chrono::Utc::now().timestamp() - 3600;
        store.push(canonical_sample(&outcome)).unwrap();
        assert_eq!(store.len(), 1);

        assert_eq!(store.evict_expired(), 1, "the stale sample left");
        assert!(
            store.is_empty(),
            "age eviction is physical, not read-time only"
        );
        assert_eq!(store.counters().evicted_by_age, 1);
        assert!(store.training_slice().is_empty());
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
        assert!(
            result.is_err(),
            "serde must reject wrong-dimension features"
        );
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
        let sample = SampleBuilder::build(&outcome, sample_features(), DataOrigin::Native)
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
            store.push(canonical_sample(&outcome)).unwrap();
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
            .attempt(make_attempt(
                "m",
                "p",
                false,
                100.0,
                Some(FailureClass::RateLimit),
            ))
            .attempt(make_attempt(
                "m2",
                "p2",
                false,
                100.0,
                Some(FailureClass::Timeout),
            ))
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
        let samples = samples_from_outcome(
            &outcome,
            std::slice::from_ref(&features),
            DataOrigin::Native,
        );

        // 1 attempt sample + 1 request-level sample
        assert_eq!(
            samples.len(),
            2,
            "single attempt: 1 attempt + 1 request sample"
        );

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
        assert_eq!(
            samples.len(),
            3,
            "two attempts: 2 attempt + 1 request sample"
        );

        // First attempt (failed)
        let first = &samples[0];
        assert_eq!(first.provider_id, "openai");
        assert_eq!(first.model_id, "gpt-4");
        assert!(!first.targets.success, "first attempt should be failure");
        assert!(
            first.targets.latency_ms.is_none(),
            "failed attempt should have no latency"
        );
        assert_eq!(
            first.targets.failure_class.as_deref(),
            Some("ProviderUnavailable")
        );
        assert_eq!(
            first.features.values[0], 1.0,
            "first attempt uses its own snapshot"
        );

        // Second attempt (succeeded)
        let second = &samples[1];
        assert_eq!(second.provider_id, "anthropic");
        assert_eq!(second.model_id, "claude-3");
        assert!(second.targets.success, "second attempt should be success");
        assert_eq!(second.targets.latency_ms, Some(400.0));
        assert!(second.targets.failure_class.is_none());
        assert_eq!(
            second.features.values[0], 0.5,
            "second attempt uses its own snapshot"
        );

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
            .attempt(make_attempt("gemini-pro", "google", true, 200.0, None))
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
        assert_eq!(
            samples.len(),
            4,
            "three attempts: 3 attempt + 1 request sample"
        );

        // First attempt (failed)
        assert_eq!(samples[0].provider_id, "openai");
        assert!(!samples[0].targets.success);
        assert_eq!(
            samples[0].targets.failure_class.as_deref(),
            Some("RateLimit")
        );
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
        assert_eq!(
            samples.len(),
            1,
            "no snapshots: only attempt sample, no request sample"
        );

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
