//! Runtime observation foundation.
//!
//! Formalizes runtime signals (latency, health, cost) that the router uses for
//! scoring. Unifies signals previously scattered across circuit_breaker, stats,
//! and billing into a single observation layer.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Signal<T> — a value with provenance
// ---------------------------------------------------------------------------

/// A runtime signal with freshness tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal<T> {
    /// The observed value (None if never observed).
    pub value: Option<T>,
    /// Number of observations that produced this value.
    pub sample_count: u32,
    /// When the last observation was made (as unix timestamp for serde).
    pub observed_at: Option<i64>,
}

impl<T: Default + Copy> Default for Signal<T> {
    fn default() -> Self {
        Signal { value: None, sample_count: 0, observed_at: None }
    }
}

impl<T> Signal<T> {
    pub fn new(value: T) -> Self {
        Signal {
            value: Some(value),
            sample_count: 1,
            observed_at: Some(chrono::Utc::now().timestamp()),
        }
    }

    pub fn is_known(&self) -> bool {
        self.value.is_some()
    }

    pub fn is_stale(&self, max_age_secs: i64) -> bool {
        match self.observed_at {
            Some(ts) => chrono::Utc::now().timestamp() - ts > max_age_secs,
            None => true,
        }
    }
}

// ---------------------------------------------------------------------------
// ObservationFreshness
// ---------------------------------------------------------------------------

/// How fresh a runtime observation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationFreshness {
    /// Observed within the last 30 seconds.
    Fresh,
    /// Observed within the last 5 minutes.
    Recent,
    /// Observed within the last 30 minutes.
    Stale,
    /// Older than 30 minutes or never observed.
    Unknown,
}

impl Default for ObservationFreshness {
    fn default() -> Self {
        Self::Unknown
    }
}

impl ObservationFreshness {
    pub fn from_age_secs(secs: i64) -> Self {
        if secs < 30 {
            Self::Fresh
        } else if secs < 300 {
            Self::Recent
        } else if secs < 1800 {
            Self::Stale
        } else {
            Self::Unknown
        }
    }

    /// Score weight multiplier based on freshness.
    /// Fresh observations get full weight, stale ones get reduced weight.
    pub fn weight(&self) -> f64 {
        match self {
            Self::Fresh => 1.0,
            Self::Recent => 0.8,
            Self::Stale => 0.4,
            Self::Unknown => 0.1,
        }
    }
}

// ---------------------------------------------------------------------------
// LatencyObservation
// ---------------------------------------------------------------------------

/// Detailed latency breakdown for a model/provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyObservation {
    /// Time to first token (most important for streaming UX).
    pub ttft_ms: Signal<f64>,
    /// Total request duration.
    pub total_ms: Signal<f64>,
    /// Output tokens per second (generation speed).
    pub tokens_per_sec: Signal<f64>,
}

impl LatencyObservation {
    /// Compute a single latency score (0.0 = slow, 1.0 = fast).
    /// Prioritizes TTFT for streaming, total latency for buffered.
    pub fn score(&self, streaming: bool) -> f64 {
        if streaming {
            // For streaming, TTFT is most important.
            match self.ttft_ms.value {
                Some(ttft) => (1.0 - (ttft / 2000.0).min(1.0)).max(0.0),
                None => 0.5, // Unknown = neutral
            }
        } else {
            // For buffered, total latency matters.
            match self.total_ms.value {
                Some(total) => (1.0 - (total / 5000.0).min(1.0)).max(0.0),
                None => 0.5,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HealthObservation
// ---------------------------------------------------------------------------

/// Health state of a model/provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthState {
    /// Recent successes, no issues.
    Healthy,
    /// Some failures but still accepting requests.
    Degraded,
    /// Circuit breaker open, not accepting requests.
    Unavailable,
    /// No observations yet.
    Unknown,
}

impl Default for HealthState {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Health metrics for a model/provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthObservation {
    pub state: HealthState,
    pub success_rate: Signal<f64>,
    pub consecutive_failures: u32,
    pub total_requests: u64,
    pub total_failures: u64,
}

impl HealthObservation {
    pub fn score(&self) -> f64 {
        match self.state {
            HealthState::Healthy => self.success_rate.value.unwrap_or(0.9),
            HealthState::Degraded => self.success_rate.value.unwrap_or(0.5) * 0.5,
            HealthState::Unavailable => 0.0,
            HealthState::Unknown => 0.5,
        }
    }
}

// ---------------------------------------------------------------------------
// CostObservation
// ---------------------------------------------------------------------------

/// Cost tracking for a model/provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostObservation {
    /// Estimated cost for the current request (computed before sending).
    pub estimated: Signal<f64>,
    /// Actual cost after the request completed.
    pub actual: Signal<f64>,
    /// Estimation error (actual - estimated).
    pub estimation_error: Signal<f64>,
}

// ---------------------------------------------------------------------------
// RuntimeObservation — unified observation
// ---------------------------------------------------------------------------

/// Complete runtime observation for a model/provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeObservation {
    pub model_id: String,
    pub provider_id: String,
    pub health: HealthObservation,
    pub latency: LatencyObservation,
    pub cost: CostObservation,
    pub freshness: ObservationFreshness,
}

impl RuntimeObservation {
    /// Update health after a successful request.
    pub fn record_success(&mut self, latency_ms: f64, ttft_ms: Option<f64>) {
        self.health.consecutive_failures = 0;
        self.health.total_requests += 1;
        self.health.state = HealthState::Healthy;
        self.health.success_rate = Signal::new(
            (self.health.total_requests - self.health.total_failures) as f64
                / self.health.total_requests as f64,
        );
        self.latency.total_ms = Signal::new(latency_ms);
        if let Some(ttft) = ttft_ms {
            self.latency.ttft_ms = Signal::new(ttft);
        }
        self.freshness = ObservationFreshness::Fresh;
    }

    /// Update health after a failed request.
    pub fn record_failure(&mut self) {
        self.health.consecutive_failures += 1;
        self.health.total_requests += 1;
        self.health.total_failures += 1;
        self.health.state = if self.health.consecutive_failures >= 5 {
            HealthState::Unavailable
        } else if self.health.consecutive_failures >= 2 {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        };
        self.health.success_rate = Signal::new(
            (self.health.total_requests - self.health.total_failures) as f64
                / self.health.total_requests as f64,
        );
        self.freshness = ObservationFreshness::Fresh;
    }
}

// ---------------------------------------------------------------------------
// ObservationStore — per-model observations
// ---------------------------------------------------------------------------

/// Stores runtime observations for all models.
#[derive(Debug)]
pub struct ObservationStore {
    observations: Mutex<HashMap<String, RuntimeObservation>>,
}

impl ObservationStore {
    pub fn new() -> Self {
        Self { observations: Mutex::new(HashMap::new()) }
    }

    /// Get or create observation for a model.
    pub fn get(&self, model_id: &str) -> RuntimeObservation {
        crate::sync::lock(&self.observations)
            .get(model_id)
            .cloned()
            .unwrap_or_else(|| RuntimeObservation {
                model_id: model_id.to_string(),
                ..Default::default()
            })
    }

    /// Record a successful request.
    pub fn record_success(
        &self,
        model_id: &str,
        provider_id: &str,
        latency_ms: f64,
        ttft_ms: Option<f64>,
    ) {
        let mut map = crate::sync::lock(&self.observations);
        let obs = map.entry(model_id.to_string()).or_insert_with(|| {
            RuntimeObservation {
                model_id: model_id.to_string(),
                provider_id: provider_id.to_string(),
                ..Default::default()
            }
        });
        obs.record_success(latency_ms, ttft_ms);
    }

    /// Record a failed request.
    pub fn record_failure(&self, model_id: &str, provider_id: &str) {
        let mut map = crate::sync::lock(&self.observations);
        let obs = map.entry(model_id.to_string()).or_insert_with(|| {
            RuntimeObservation {
                model_id: model_id.to_string(),
                provider_id: provider_id.to_string(),
                ..Default::default()
            }
        });
        obs.record_failure();
    }
}

impl Default for ObservationStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Signal ---------------------------------------------------------------

    #[test]
    fn signal_new_is_known() {
        let s = Signal::new(42u32);
        assert!(s.is_known());
        assert_eq!(s.value, Some(42));
        assert_eq!(s.sample_count, 1);
        assert!(s.observed_at.is_some());
    }

    #[test]
    fn signal_default_is_unknown() {
        let s: Signal<u32> = Signal::default();
        assert!(!s.is_known());
        assert_eq!(s.sample_count, 0);
        assert!(s.observed_at.is_none());
    }

    #[test]
    fn signal_is_stale_with_no_timestamp() {
        let s: Signal<u32> = Signal::default();
        assert!(s.is_stale(60));
    }

    #[test]
    fn signal_is_stale_within_threshold() {
        let s = Signal::new(1u32);
        assert!(!s.is_stale(60), "freshly created signal should not be stale");
    }

    // -- ObservationFreshness -------------------------------------------------

    #[test]
    fn freshness_from_age_secs() {
        assert_eq!(ObservationFreshness::from_age_secs(0), ObservationFreshness::Fresh);
        assert_eq!(ObservationFreshness::from_age_secs(29), ObservationFreshness::Fresh);
        assert_eq!(ObservationFreshness::from_age_secs(30), ObservationFreshness::Recent);
        assert_eq!(ObservationFreshness::from_age_secs(299), ObservationFreshness::Recent);
        assert_eq!(ObservationFreshness::from_age_secs(300), ObservationFreshness::Stale);
        assert_eq!(ObservationFreshness::from_age_secs(1799), ObservationFreshness::Stale);
        assert_eq!(ObservationFreshness::from_age_secs(1800), ObservationFreshness::Unknown);
    }

    #[test]
    fn freshness_weight() {
        assert!((ObservationFreshness::Fresh.weight() - 1.0).abs() < f64::EPSILON);
        assert!((ObservationFreshness::Recent.weight() - 0.8).abs() < f64::EPSILON);
        assert!((ObservationFreshness::Stale.weight() - 0.4).abs() < f64::EPSILON);
        assert!((ObservationFreshness::Unknown.weight() - 0.1).abs() < f64::EPSILON);
    }

    // -- LatencyObservation ---------------------------------------------------

    #[test]
    fn latency_score_streaming_with_ttft() {
        let obs = LatencyObservation {
            ttft_ms: Signal::new(200.0),
            total_ms: Signal::new(1000.0),
            tokens_per_sec: Signal::new(50.0),
        };
        let score = obs.score(true);
        // 1.0 - (200/2000) = 0.9
        assert!((score - 0.9).abs() < 0.01, "expected ~0.9, got {score}");
    }

    #[test]
    fn latency_score_streaming_no_ttft() {
        let obs = LatencyObservation::default();
        assert!((obs.score(true) - 0.5).abs() < f64::EPSILON, "unknown = neutral 0.5");
    }

    #[test]
    fn latency_score_buffered_with_total() {
        let obs = LatencyObservation {
            ttft_ms: Signal::new(100.0),
            total_ms: Signal::new(1000.0),
            tokens_per_sec: Signal::default(),
        };
        let score = obs.score(false);
        // 1.0 - (1000/5000) = 0.8
        assert!((score - 0.8).abs() < 0.01, "expected ~0.8, got {score}");
    }

    #[test]
    fn latency_score_buffered_no_total() {
        let obs = LatencyObservation::default();
        assert!((obs.score(false) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn latency_score_clamps_large_values() {
        let obs = LatencyObservation {
            ttft_ms: Signal::new(10_000.0),
            total_ms: Signal::default(),
            tokens_per_sec: Signal::default(),
        };
        assert!((obs.score(true) - 0.0).abs() < f64::EPSILON, "huge TTFT clamps to 0.0");
    }

    // -- HealthObservation ----------------------------------------------------

    #[test]
    fn health_score_healthy() {
        let h = HealthObservation {
            state: HealthState::Healthy,
            success_rate: Signal::new(0.95),
            ..Default::default()
        };
        assert!((h.score() - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn health_score_healthy_no_rate() {
        let h = HealthObservation {
            state: HealthState::Healthy,
            ..Default::default()
        };
        assert!((h.score() - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn health_score_degraded() {
        let h = HealthObservation {
            state: HealthState::Degraded,
            success_rate: Signal::new(0.8),
            ..Default::default()
        };
        assert!((h.score() - 0.4).abs() < 0.01, "0.8 * 0.5 = 0.4");
    }

    #[test]
    fn health_score_unavailable() {
        let h = HealthObservation {
            state: HealthState::Unavailable,
            ..Default::default()
        };
        assert!((h.score() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn health_score_unknown() {
        let h = HealthObservation::default();
        assert!((h.score() - 0.5).abs() < f64::EPSILON);
    }

    // -- RuntimeObservation ---------------------------------------------------

    #[test]
    fn record_success_updates_health_and_latency() {
        let mut obs = RuntimeObservation::default();
        obs.record_success(500.0, Some(100.0));

        assert_eq!(obs.health.state, HealthState::Healthy);
        assert_eq!(obs.health.consecutive_failures, 0);
        assert_eq!(obs.health.total_requests, 1);
        assert_eq!(obs.health.total_failures, 0);
        assert!((obs.health.success_rate.value.unwrap() - 1.0).abs() < f64::EPSILON);
        assert!((obs.latency.total_ms.value.unwrap() - 500.0).abs() < f64::EPSILON);
        assert!((obs.latency.ttft_ms.value.unwrap() - 100.0).abs() < f64::EPSILON);
        assert_eq!(obs.freshness, ObservationFreshness::Fresh);
    }

    #[test]
    fn record_success_without_ttft() {
        let mut obs = RuntimeObservation::default();
        obs.record_success(300.0, None);
        assert!(obs.latency.ttft_ms.value.is_none());
        assert!(obs.latency.total_ms.value.is_some());
    }

    #[test]
    fn record_failure_increments_and_changes_state() {
        let mut obs = RuntimeObservation::default();

        // First failure — still Healthy
        obs.record_failure();
        assert_eq!(obs.health.consecutive_failures, 1);
        assert_eq!(obs.health.state, HealthState::Healthy);
        assert_eq!(obs.health.total_requests, 1);
        assert_eq!(obs.health.total_failures, 1);

        // Second failure — Degraded
        obs.record_failure();
        assert_eq!(obs.health.consecutive_failures, 2);
        assert_eq!(obs.health.state, HealthState::Degraded);

        // Third through fourth — still Degraded
        obs.record_failure();
        obs.record_failure();
        assert_eq!(obs.health.consecutive_failures, 4);
        assert_eq!(obs.health.state, HealthState::Degraded);

        // Fifth failure — Unavailable
        obs.record_failure();
        assert_eq!(obs.health.consecutive_failures, 5);
        assert_eq!(obs.health.state, HealthState::Unavailable);
    }

    #[test]
    fn record_success_resets_consecutive_failures() {
        let mut obs = RuntimeObservation::default();
        obs.record_failure();
        obs.record_failure();
        assert_eq!(obs.health.state, HealthState::Degraded);

        obs.record_success(200.0, Some(50.0));
        assert_eq!(obs.health.consecutive_failures, 0);
        assert_eq!(obs.health.state, HealthState::Healthy);
    }

    // -- ObservationStore -----------------------------------------------------

    #[test]
    fn store_get_returns_default_for_unknown() {
        let store = ObservationStore::new();
        let obs = store.get("unknown-model");
        assert_eq!(obs.model_id, "unknown-model");
        assert_eq!(obs.health.state, HealthState::Unknown);
    }

    #[test]
    fn store_record_success() {
        let store = ObservationStore::new();
        store.record_success("m1", "p1", 400.0, Some(80.0));
        let obs = store.get("m1");
        assert_eq!(obs.health.state, HealthState::Healthy);
        assert!(obs.latency.total_ms.is_known());
    }

    #[test]
    fn store_record_failure() {
        let store = ObservationStore::new();
        store.record_failure("m1", "p1");
        let obs = store.get("m1");
        assert_eq!(obs.health.consecutive_failures, 1);
        assert_eq!(obs.health.total_failures, 1);
    }

    #[test]
    fn store_accumulates_across_calls() {
        let store = ObservationStore::new();
        store.record_success("m1", "p1", 300.0, None);
        store.record_success("m1", "p1", 200.0, None);
        let obs = store.get("m1");
        assert_eq!(obs.health.total_requests, 2);
        assert!((obs.health.success_rate.value.unwrap() - 1.0).abs() < f64::EPSILON);
    }
}
