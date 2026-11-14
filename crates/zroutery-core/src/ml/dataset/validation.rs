use super::sample::{OutcomeTrainingSample, SampleScope, TrainingSample};

use crate::ml::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use crate::outcome::FinalStatus;

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
    // The outer envelope version is not the vector's own schema: a nested
    // version this build does not understand must not pass on a matching outer
    // one.
    if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(format!(
            "feature schema version mismatch: {} vs {}",
            sample.features.schema_version, FEATURE_SCHEMA_VERSION
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
