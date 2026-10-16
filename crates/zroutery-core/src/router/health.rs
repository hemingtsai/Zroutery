//! Reading health, and deciding what a failure means for retry and fallback.

use super::candidate::FailureDisposition;
use super::Router;
use crate::circuit_breaker::CircuitState;
use crate::failure::ClassifiedFailure;

impl Router {
    /// Health score for a model, 0.0 (circuit open / unknown) to 1.0 (healthy).
    ///
    /// Factors in circuit breaker state and consecutive failure count.
    pub fn health_score(&self, model_id: &str) -> f64 {
        let health = crate::sync::lock(&self.health);
        match health.get(model_id) {
            Some(h) => match h.breaker.state() {
                CircuitState::Closed => {
                    if h.total_success + h.total_failure == 0 {
                        1.0 // no data yet — optimistic
                    } else {
                        let ratio =
                            h.total_success as f64 / (h.total_success + h.total_failure) as f64;
                        ratio.clamp(0.0, 1.0)
                    }
                }
                CircuitState::HalfOpen => 0.3,
                CircuitState::Open => 0.0,
            },
            None => 1.0, // no health data = new model, treat as healthy
        }
    }

    /// Exponentially weighted moving average latency in milliseconds.
    /// Returns 0.0 when no successful request has been recorded.
    pub fn avg_latency(&self, model_id: &str) -> f64 {
        crate::sync::lock(&self.health)
            .get(model_id)
            .map(|h| h.avg_latency_ms)
            .unwrap_or(0.0)
    }

    /// Whether the canonical failure allows another attempt on the same
    /// candidate.
    pub fn should_retry_failure(&self, failure: &ClassifiedFailure) -> bool {
        failure.retryable()
    }

    /// Whether the canonical failure allows trying a different candidate.
    pub fn should_fallback_failure(&self, failure: &ClassifiedFailure) -> bool {
        failure.fallbackable()
    }

    /// Convert the canonical impact table into a priority disposition.
    ///
    /// This is an adapter, not a second classifier. It is not an exhaustive
    /// capability query: for failures that are both retryable and fallbackable,
    /// `Retry` wins the summary, while callers can independently query
    /// [`Self::should_retry_failure`] and [`Self::should_fallback_failure`].
    pub fn failure_disposition(&self, failure: &ClassifiedFailure) -> FailureDisposition {
        if failure.retryable() {
            FailureDisposition::Retry
        } else if failure.fallbackable() {
            FailureDisposition::Fallback
        } else {
            FailureDisposition::Stop
        }
    }
}
