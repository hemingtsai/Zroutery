use super::sample::{OutcomeTrainingSample, SampleBuilder, SampleScope, Targets, TrainingSample};
use super::validation::validate_outcome_sample;

use crate::feedback::{DataOrigin, Feedback};
use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
use crate::ml::shadow::ShadowInput;
use crate::outcome::{CandidateIdentity, Outcome};

// ---------------------------------------------------------------------------
// Canonical Outcome -> sample conversion
// ---------------------------------------------------------------------------

pub(super) fn deterministic_sample_id(outcome: &Outcome, suffix: &str) -> String {
    format!("samp-{}-{suffix}", outcome.outcome_id)
}

pub(super) fn request_identity(outcome: &Outcome) -> CandidateIdentity {
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
