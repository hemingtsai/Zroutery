//! One attempt at routing a request to a candidate model.

use crate::failure::{ClassifiedFailure, FailureClass};

use serde::{Deserialize, Serialize};

use super::FailureFacts;
/// A single attempt at routing a request to a candidate model.
///
/// When failover occurs, each candidate in the chain produces one `Attempt`.
/// When the rectifier repairs a request and retries the same candidate, the
/// retry is a separate attempt with `rectified = true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// Unique id for this attempt (e.g. `att_<uuid>`).
    pub attempt_id: String,
    /// The model that was tried.
    pub candidate_model: String,
    /// The provider that was tried.
    pub candidate_provider: String,
    /// Unix timestamp (seconds) when the attempt started.
    pub started_at: i64,
    /// Unix timestamp (seconds) when the attempt completed.
    pub completed_at: i64,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: f64,
    /// Time to first token (streaming only).
    pub ttft_ms: Option<f64>,
    /// Whether this attempt succeeded.
    pub success: bool,
    /// Classification of the failure (None if success).
    pub failure_class: Option<FailureClass>,
    /// Human-readable failure message (None if success).
    pub failure_message: Option<String>,
    /// HTTP status code returned by the provider, if any.
    pub http_status: Option<u16>,
    /// Whether this attempt was a rectifier retry (same candidate, repaired request).
    pub rectified: bool,
    /// What this attempt cost, in the pricing currency of its model.
    ///
    /// Per attempt rather than only per request, because a failover chain spends
    /// more than the attempt that answered: most providers bill the failed call,
    /// and a router that cannot attribute it cannot tell a cheap single-attempt
    /// plan from an expensive one that needed three.
    ///
    /// `None` means the price or the usage was not available for this attempt,
    /// which is a different statement from zero — a zero here would claim a call
    /// was free.
    ///
    /// Defaults to `None`, so an outcome persisted before this field existed
    /// still reads, with its cost unattributed rather than fabricated.
    #[serde(default)]
    pub cost: Option<f64>,
}

impl Attempt {
    /// Whether this attempt can be treated as a successful terminal response.
    pub fn is_terminal_success(&self) -> bool {
        self.success
            && self.failure_class.is_none()
            && self.failure_message.is_none()
            && self
                .http_status
                .is_none_or(|status| (200..=299).contains(&status))
    }

    /// Return the captured failure facts without deriving a new class.
    pub fn failure_facts(&self) -> Option<FailureFacts> {
        self.failure_class
            .map(|class| FailureFacts::new(class, self.failure_message.clone(), self.http_status))
    }

    /// Return the accepted classified failure context for this attempt.
    pub fn classified_failure(&self) -> Option<ClassifiedFailure> {
        self.failure_facts().map(|facts| facts.classified())
    }
}
