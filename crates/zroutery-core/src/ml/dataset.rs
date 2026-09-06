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

use crate::feedback::{DataOrigin, FeedbackSignal};
use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use crate::outcome::Outcome;

// ---------------------------------------------------------------------------
// TrainingSample — the core training unit
// ---------------------------------------------------------------------------

/// A single training sample combining features, outcome, and target labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Feedback signals (if any).
    pub feedback: Vec<FeedbackSignal>,
}

// ---------------------------------------------------------------------------
// Targets — what the models learn to predict
// ---------------------------------------------------------------------------

/// Training targets derived from an [`Outcome`].
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Extract targets from an [`Outcome`].
    pub fn from_outcome(outcome: &Outcome) -> Self {
        Targets {
            success: outcome.success,
            latency_ms: if outcome.success {
                Some(outcome.total_latency_ms)
            } else {
                None
            },
            ttft_ms: outcome.ttft_ms,
            cost: outcome.actual_cost,
            failure_class: outcome
                .attempts
                .last()
                .and_then(|a| a.failure_class)
                .map(|fc| format!("{:?}", fc)),
            fallback_count: outcome.fallback_count,
        }
    }
}

// ---------------------------------------------------------------------------
// SampleBuilder — constructs TrainingSample from runtime data
// ---------------------------------------------------------------------------

/// Builds a [`TrainingSample`] from an [`Outcome`] and feature snapshot.
pub struct SampleBuilder;

impl SampleBuilder {
    /// Build a `TrainingSample` from an `Outcome` and the feature context at
    /// decision time.
    pub fn build(
        outcome: &Outcome,
        features: RoutingFeatures,
        origin: DataOrigin,
    ) -> TrainingSample {
        TrainingSample {
            sample_id: format!("samp-{}", uuid::Uuid::new_v4().simple()),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: outcome.timestamp,
            features,
            targets: Targets::from_outcome(outcome),
            provider_id: outcome.final_provider.clone(),
            model_id: outcome.final_model.clone(),
            origin,
            outcome_id: outcome.outcome_id.clone(),
            feedback: Vec::new(),
        }
    }
}

impl TrainingSample {
    /// Attach feedback signals to this sample.
    pub fn with_feedback(mut self, feedback: Vec<FeedbackSignal>) -> Self {
        self.feedback = feedback;
        self
    }
}

// ---------------------------------------------------------------------------
// samples_from_outcome — attempt-level attribution
// ---------------------------------------------------------------------------

/// Generate training samples from a complete [`Outcome`], one per attempt.
///
/// Failed attempts get their own feature snapshot + outcome.
/// The final successful attempt gets the request-level utility.
///
/// This solves the attribution problem: when a request fails on candidate A
/// and succeeds on candidate B (fallback), each attempt gets its own sample
/// with the correct provider/model identity and success flag.
pub fn samples_from_outcome(
    outcome: &Outcome,
    feature_snapshots: &[RoutingFeatures],
    origin: DataOrigin,
) -> Vec<TrainingSample> {
    let mut samples = Vec::new();
    for (i, attempt) in outcome.attempts.iter().enumerate() {
        let features = feature_snapshots
            .get(i)
            .cloned()
            .unwrap_or_else(RoutingFeatures::default);
        let targets = Targets {
            success: attempt.success,
            latency_ms: if attempt.success {
                Some(attempt.latency_ms)
            } else {
                None
            },
            ttft_ms: attempt.ttft_ms,
            cost: None, // per-attempt cost not available
            failure_class: attempt.failure_class.map(|fc| format!("{:?}", fc)),
            fallback_count: 0, // per-attempt, not per-request
        };
        samples.push(TrainingSample {
            sample_id: format!("samp-{}-{}", outcome.outcome_id, i),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: outcome.timestamp,
            features,
            targets,
            provider_id: attempt.candidate_provider.clone(),
            model_id: attempt.candidate_model.clone(),
            origin: origin.clone(),
            outcome_id: outcome.outcome_id.clone(),
            feedback: Vec::new(),
        });
    }
    // Also create a request-level sample for the final outcome.
    if let Some(last_features) = feature_snapshots.last() {
        samples.push(SampleBuilder::build(outcome, last_features.clone(), origin));
    }
    samples
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        let mut features = RoutingFeatures::default();
        features.schema_version = 99;
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
        let samples = samples_from_outcome(&outcome, &[features.clone()], DataOrigin::Native);

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
