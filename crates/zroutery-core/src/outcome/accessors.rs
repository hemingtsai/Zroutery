//! Read-only views over an [`Outcome`]: identity, timing and failure evidence.

use crate::failure::{ClassifiedFailure, FailureClass};

use super::builder::{identity_from_parts, legacy_identity};
use super::{Attempt, CandidateIdentity, FailureFacts, FinalStatus, Outcome, OutcomeIdentity};
impl Outcome {
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
}
