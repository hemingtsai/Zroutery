//! What the router hands back: a candidate, the health it has, and how a failure is disposed of.

use crate::circuit_breaker::CircuitState;
use serde::{Deserialize, Serialize};

use crate::config::{ModelEntry, ProviderConfig};
use crate::policy::PlannedIdentity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    /// Do not attempt another candidate.
    Stop,
    /// Prefer retrying the same candidate before considering fallback.
    Retry,
    /// Prefer moving to a different candidate.
    Fallback,
}

/// One attempt: which model, on which provider.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Derived once here so health keys, log records and response headers all
    /// agree on the name.
    pub exposed_id: String,
    pub entry: ModelEntry,
    pub provider: ProviderConfig,
    /// True when this candidate is only being tried because everything else is
    /// unavailable (open circuit) or because it is in a half-open probe.
    pub degraded: bool,
    /// Explicit account/key selected after model ranking. No secret is held here.
    pub credential: Option<InferenceCredential>,
    /// Quote chosen before cost ranking; execution uses this exact credential.
    pub rate_quote: Option<crate::billing::RateQuote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceCredential {
    pub account_id: String,
    pub key_id: String,
}

impl Candidate {
    pub(super) fn new(entry: &ModelEntry, provider: &ProviderConfig, degraded: bool) -> Self {
        Candidate {
            exposed_id: entry.exposed_id(),
            entry: entry.clone(),
            provider: provider.clone(),
            degraded,
            credential: None,
            rate_quote: None,
        }
    }

    pub fn model_id(&self) -> &str {
        &self.exposed_id
    }

    /// The identity selected for this candidate before an attempt begins.
    pub fn planned_identity(&self) -> PlannedIdentity {
        PlannedIdentity::new(
            self.exposed_id.clone(),
            self.entry.provider_id.clone(),
            self.entry.tier.map(|tier| tier.as_str().to_string()),
        )
    }
}

/// Per model health, exposed to the GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHealth {
    pub model_id: String,
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub total_success: u64,
    pub total_failure: u64,
    /// Exponentially weighted average latency of successful calls.
    pub avg_latency_ms: f64,
    /// Seconds left for a circuit or default-credential rate-limit window.
    pub cooldown_remaining_secs: u64,
    /// Rate-limit availability is independent of the circuit's health state.
    #[serde(default)]
    pub rate_limit_remaining_secs: u64,
    pub last_error: Option<String>,
}
