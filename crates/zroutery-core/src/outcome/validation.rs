//! Checking an [`Outcome`] against the attempt evidence it claims.

use crate::failure::FailureClass;

use super::builder::{
    legacy_identity, validate_identity_pair, validate_non_negative_finite,
    validate_optional_identity_pair, validate_optional_non_negative_finite,
};
use super::{Attempt, FinalStatus, Outcome};

impl Outcome {
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

    pub(super) fn fill_identity_fields(&mut self) {
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
}
