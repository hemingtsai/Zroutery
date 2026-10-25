//! Candidate and outcome identity, and the failure evidence both carry.

use crate::failure::{ClassifiedFailure, FailureClass};

use serde::{Deserialize, Serialize};
/// The provider/model pair involved in one routing position.
///
/// This is deliberately a small value type rather than a route plan.  An
/// identity can be planned before an attempt, observed on the last attempt, or
/// present only when a successful terminal response was actually served.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateIdentity {
    pub model: String,
    pub provider: String,
}

impl CandidateIdentity {
    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn from_model_provider(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self::new(model, provider)
    }

    pub fn from_provider_model(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(model, provider)
    }
}

impl From<(&str, &str)> for CandidateIdentity {
    fn from((model, provider): (&str, &str)) -> Self {
        Self::new(model, provider)
    }
}

/// The three routing identities that must never be collapsed into one field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutcomeIdentity {
    /// Selected before attempts were made.  `None` means no candidate was
    /// available (for example, a budget denial or no-candidate rejection).
    pub planned: Option<CandidateIdentity>,
    /// Identity of the final attempt made, if any.
    pub last_attempted: Option<CandidateIdentity>,
    /// Identity that produced a successful terminal response, if any.
    pub served: Option<CandidateIdentity>,
}

/// The accepted failure vocabulary plus the error facts captured for one
/// terminal failure.
///
/// `FailureClass` remains the sole classification authority.  This type does
/// not inspect messages or HTTP status codes to reclassify anything; it only
/// carries the facts already attached to an attempt/outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureFacts {
    #[serde(alias = "failure_class")]
    pub class: FailureClass,
    #[serde(alias = "failure_message")]
    pub message: Option<String>,
    #[serde(alias = "status")]
    pub http_status: Option<u16>,
}

impl FailureFacts {
    pub fn new(class: FailureClass, message: Option<String>, http_status: Option<u16>) -> Self {
        Self {
            class,
            message,
            http_status,
        }
    }

    pub fn class(&self) -> FailureClass {
        self.class
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn status(&self) -> Option<u16> {
        self.http_status
    }

    /// Rehydrate the accepted classification context without classifying again.
    pub fn classified(&self) -> ClassifiedFailure {
        ClassifiedFailure {
            class: self.class,
            status: self.http_status,
            message: self.message.clone().unwrap_or_default(),
            impact: self.class.impact(),
        }
    }
}

/// Backwards/forwards-friendly spelling for callers that refer to error facts.
pub type ErrorFacts = FailureFacts;

/// Stable, lossless wire spelling for the accepted failure vocabulary.
pub const fn failure_class_wire_name(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Transport => "transport",
        FailureClass::Timeout => "timeout",
        FailureClass::RateLimit => "rate_limit",
        FailureClass::Authentication => "authentication",
        FailureClass::ProviderUnavailable => "provider_unavailable",
        FailureClass::ProviderRejected => "provider_rejected",
        FailureClass::Protocol => "protocol",
        FailureClass::Capability => "capability",
        FailureClass::InvalidRequest => "invalid_request",
        FailureClass::ClientCancelled => "client_cancelled",
        FailureClass::MissingApiKey => "missing_api_key",
        FailureClass::OverBudget => "over_budget",
        FailureClass::NoCandidate => "no_candidate",
        FailureClass::Configuration => "configuration",
        FailureClass::Interrupted => "interrupted",
        FailureClass::QuotaExhausted => "quota_exhausted",
        FailureClass::BalanceExhausted => "balance_exhausted",
        FailureClass::Unknown => "unknown",
    }
}
