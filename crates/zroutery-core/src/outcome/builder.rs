//! The consuming builder behind [`Outcome::builder`].

use crate::failure::{ClassifiedFailure, FailureClass};

use crate::ir::Usage;

use super::{Attempt, CandidateIdentity, FailureFacts, FinalStatus, Outcome, OutcomeBuilder};
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

pub(super) fn legacy_identity(model: &str, provider: &str) -> Option<CandidateIdentity> {
    if model.trim().is_empty() && provider.trim().is_empty() {
        None
    } else {
        Some(CandidateIdentity::new(model, provider))
    }
}

pub(super) fn identity_from_parts(
    model: Option<&str>,
    provider: Option<&str>,
) -> Option<CandidateIdentity> {
    match (model, provider) {
        (Some(model), Some(provider))
            if !model.trim().is_empty() && !provider.trim().is_empty() =>
        {
            Some(CandidateIdentity::new(model, provider))
        }
        _ => None,
    }
}

pub(super) fn validate_identity_pair(
    label: &str,
    model: &str,
    provider: &str,
) -> Result<(), String> {
    if model.trim().is_empty() != provider.trim().is_empty() {
        return Err(format!(
            "{label} model/provider must be both present or both absent"
        ));
    }
    Ok(())
}

pub(super) fn validate_optional_identity_pair(
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

pub(super) fn validate_non_negative_finite(label: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{label} must be finite and non-negative: {value}"));
    }
    Ok(())
}

pub(super) fn validate_optional_non_negative_finite(
    label: &str,
    value: Option<f64>,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_non_negative_finite(label, value)?;
    }
    Ok(())
}
