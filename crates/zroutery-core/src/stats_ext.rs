//! Runtime statistics for provider+model pairs.
//!
//! Unlike [`crate::stats`] (the bounded request log for the GUI), this module
//! maintains *streaming* statistics that accumulate over the lifetime of the
//! process: exponential weighted moving averages for latency, sorted-ring-buffer
//! percentile estimators, and failure counts broken down by [`FailureClass`].
//!
//! All mutable state lives behind a single `Mutex` per store, matching the
//! convention established by the rest of the crate.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::failure::FailureClass;

// -----------------------------------------------------------------------
// PercentileEstimator
// -----------------------------------------------------------------------

/// Streaming percentile estimator using a sorted ring buffer.
///
/// Good enough for routing decisions -- not a statistical library.
#[derive(Debug, Clone)]
pub struct PercentileEstimator {
    /// Sorted samples (ring buffer, max capacity).
    samples: Vec<f64>,
    capacity: usize,
    /// Total number of observations ever recorded (including evicted ones).
    total_count: u64,
}

impl PercentileEstimator {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            capacity,
            total_count: 0,
        }
    }

    /// Record a new sample.
    pub fn record(&mut self, value: f64) {
        self.total_count += 1;
        if self.samples.len() < self.capacity {
            // Insert in sorted order
            let pos = self.samples.partition_point(|&x| x < value);
            self.samples.insert(pos, value);
        } else {
            // Replace oldest (simple: replace a rotating position).
            // For routing purposes, this is good enough.
            let pos = (self.total_count as usize) % self.capacity;
            self.samples[pos] = value;
            self.samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        }
    }

    pub fn sample_count(&self) -> u64 {
        self.total_count
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Get the p-th percentile (0.0 to 1.0).
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let idx = ((self.samples.len() - 1) as f64 * p).round() as usize;
        self.samples.get(idx).copied()
    }

    pub fn min(&self) -> Option<f64> {
        self.samples.first().copied()
    }

    pub fn max(&self) -> Option<f64> {
        self.samples.last().copied()
    }
}

// -----------------------------------------------------------------------
// Ewma
// -----------------------------------------------------------------------

/// Exponential Weighted Moving Average.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ewma {
    pub value: Option<f64>,
    pub alpha: f64,
    pub sample_count: u64,
}

impl Ewma {
    pub fn new(alpha: f64) -> Self {
        Self {
            value: None,
            alpha,
            sample_count: 0,
        }
    }

    /// Update with a new observation.
    pub fn update(&mut self, sample: f64) {
        self.sample_count += 1;
        self.value = Some(match self.value {
            None => sample,
            Some(prev) => self.alpha * sample + (1.0 - self.alpha) * prev,
        });
    }

    pub fn is_known(&self) -> bool {
        self.value.is_some()
    }
}

impl Default for Ewma {
    fn default() -> Self {
        // alpha=0.3 gives recent samples ~30% weight, good for routing
        Self::new(0.3)
    }
}

// -----------------------------------------------------------------------
// LatencyStats
// -----------------------------------------------------------------------

/// Historical latency statistics for a single metric (TTFT or total).
#[derive(Debug, Clone)]
pub struct LatencyStats {
    pub ewma: Ewma,
    estimator: PercentileEstimator,
}

impl LatencyStats {
    pub fn new() -> Self {
        Self {
            ewma: Ewma::default(),
            estimator: PercentileEstimator::new(500), // keep last 500 samples
        }
    }

    pub fn record(&mut self, value: f64) {
        self.ewma.update(value);
        self.estimator.record(value);
    }

    pub fn p50(&self) -> Option<f64> {
        self.estimator.percentile(0.5)
    }

    pub fn p95(&self) -> Option<f64> {
        self.estimator.percentile(0.95)
    }

    pub fn min(&self) -> Option<f64> {
        self.estimator.min()
    }

    pub fn max(&self) -> Option<f64> {
        self.estimator.max()
    }

    pub fn sample_count(&self) -> u64 {
        self.estimator.sample_count()
    }

    pub fn is_empty(&self) -> bool {
        self.estimator.is_empty()
    }
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self::new()
    }
}

// -----------------------------------------------------------------------
// FailureStats
// -----------------------------------------------------------------------

/// Failure statistics broken down by FailureClass.
#[derive(Debug, Clone, Default)]
pub struct FailureStats {
    /// Total failures by class.
    pub by_class: HashMap<FailureClass, u64>,
    /// Total classified failures.
    pub total: u64,
}

impl FailureStats {
    pub fn record(&mut self, class: FailureClass) {
        *self.by_class.entry(class).or_insert(0) += 1;
        self.total += 1;
    }

    pub fn count(&self, class: FailureClass) -> u64 {
        self.by_class.get(&class).copied().unwrap_or(0)
    }
}

// -----------------------------------------------------------------------
// ProviderModelStats
// -----------------------------------------------------------------------

/// Complete runtime statistics for a provider+model pair.
#[derive(Debug, Clone)]
pub struct ProviderModelStats {
    pub model_id: String,
    pub provider_id: String,

    // Request counts
    pub total_requests: u64,
    pub total_successes: u64,
    pub total_failures: u64,

    // Latency statistics
    pub ttft: LatencyStats,
    pub total_latency: LatencyStats,

    // Failure breakdown
    pub failures: FailureStats,
}

impl ProviderModelStats {
    pub fn new(model_id: String, provider_id: String) -> Self {
        Self {
            model_id,
            provider_id,
            total_requests: 0,
            total_successes: 0,
            total_failures: 0,
            ttft: LatencyStats::new(),
            total_latency: LatencyStats::new(),
            failures: FailureStats::default(),
        }
    }

    pub fn record_success(&mut self, latency_ms: f64, ttft_ms: Option<f64>) {
        self.total_requests += 1;
        self.total_successes += 1;
        self.total_latency.record(latency_ms);
        if let Some(ttft) = ttft_ms {
            self.ttft.record(ttft);
        }
    }

    pub fn record_failure(&mut self, class: FailureClass) {
        self.total_requests += 1;
        self.total_failures += 1;
        self.failures.record(class);
    }

    pub fn success_rate(&self) -> f64 {
        if self.total_requests == 0 {
            0.0
        } else {
            self.total_successes as f64 / self.total_requests as f64
        }
    }
}

// -----------------------------------------------------------------------
// StatsStore
// -----------------------------------------------------------------------

/// Composite key matching ObservationStore.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StatsKey {
    provider_id: String,
    model_id: String,
}

/// Stores runtime statistics for all provider+model pairs.
#[derive(Debug)]
pub struct StatsStore {
    stats: Mutex<HashMap<StatsKey, ProviderModelStats>>,
}

impl StatsStore {
    pub fn new() -> Self {
        Self {
            stats: Mutex::new(HashMap::new()),
        }
    }

    fn key(model_id: &str, provider_id: &str) -> StatsKey {
        StatsKey {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }

    pub fn get(&self, model_id: &str, provider_id: &str) -> ProviderModelStats {
        crate::sync::lock(&self.stats)
            .get(&Self::key(model_id, provider_id))
            .cloned()
            .unwrap_or_else(|| ProviderModelStats::new(model_id.into(), provider_id.into()))
    }

    pub fn record_success(
        &self,
        model_id: &str,
        provider_id: &str,
        latency_ms: f64,
        ttft_ms: Option<f64>,
    ) {
        let mut map = crate::sync::lock(&self.stats);
        let stats = map
            .entry(Self::key(model_id, provider_id))
            .or_insert_with(|| ProviderModelStats::new(model_id.into(), provider_id.into()));
        stats.record_success(latency_ms, ttft_ms);
    }

    pub fn record_classified_failure(
        &self,
        model_id: &str,
        provider_id: &str,
        class: FailureClass,
    ) {
        let mut map = crate::sync::lock(&self.stats);
        let stats = map
            .entry(Self::key(model_id, provider_id))
            .or_insert_with(|| ProviderModelStats::new(model_id.into(), provider_id.into()));
        stats.record_failure(class);
    }
}

impl Default for StatsStore {
    fn default() -> Self {
        Self::new()
    }
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Ewma ----

    #[test]
    fn ewma_first_value_is_exact() {
        let mut e = Ewma::new(0.3);
        e.update(100.0);
        assert_eq!(e.value, Some(100.0));
        assert_eq!(e.sample_count, 1);
        assert!(e.is_known());
    }

    #[test]
    fn ewma_converges_to_constant() {
        let mut e = Ewma::new(0.3);
        for _ in 0..200 {
            e.update(42.0);
        }
        let v = e.value.unwrap();
        assert!(
            (v - 42.0).abs() < 1e-9,
            "ewma should converge to constant, got {v}"
        );
    }

    #[test]
    fn ewma_alpha_one_means_latest_only() {
        let mut e = Ewma::new(1.0);
        e.update(10.0);
        e.update(20.0);
        e.update(30.0);
        assert_eq!(e.value, Some(30.0));
    }

    #[test]
    fn ewma_alpha_zero_means_never_changes() {
        let mut e = Ewma::new(0.0);
        e.update(10.0);
        e.update(999.0);
        e.update(-50.0);
        assert_eq!(e.value, Some(10.0));
    }

    #[test]
    fn ewma_default_is_not_known() {
        let e = Ewma::default();
        assert!(!e.is_known());
        assert_eq!(e.value, None);
    }

    // ---- PercentileEstimator ----

    #[test]
    fn percentile_empty_returns_none() {
        let est = PercentileEstimator::new(10);
        assert!(est.percentile(0.5).is_none());
        assert!(est.min().is_none());
        assert!(est.max().is_none());
        assert!(est.is_empty());
    }

    #[test]
    fn percentile_single_value() {
        let mut est = PercentileEstimator::new(10);
        est.record(42.0);
        assert_eq!(est.percentile(0.0), Some(42.0));
        assert_eq!(est.percentile(0.5), Some(42.0));
        assert_eq!(est.percentile(1.0), Some(42.0));
        assert_eq!(est.min(), Some(42.0));
        assert_eq!(est.max(), Some(42.0));
        assert_eq!(est.len(), 1);
        assert_eq!(est.sample_count(), 1);
    }

    #[test]
    fn percentile_sorted_insertion() {
        let mut est = PercentileEstimator::new(10);
        est.record(30.0);
        est.record(10.0);
        est.record(20.0);
        assert_eq!(est.min(), Some(10.0));
        assert_eq!(est.max(), Some(30.0));
        assert_eq!(est.len(), 3);
    }

    #[test]
    fn percentile_p50_p95_accuracy() {
        let mut est = PercentileEstimator::new(100);
        // Insert 1..=100
        for i in 1..=100 {
            est.record(i as f64);
        }
        let p50 = est.percentile(0.5).unwrap();
        let p95 = est.percentile(0.95).unwrap();
        // p50 should be near 50
        assert!(
            (p50 - 50.0).abs() < 2.0,
            "p50 should be ~50, got {p50}"
        );
        // p95 should be near 95
        assert!(
            (p95 - 95.0).abs() < 2.0,
            "p95 should be ~95, got {p95}"
        );
    }

    #[test]
    fn percentile_ring_eviction() {
        let mut est = PercentileEstimator::new(5);
        for i in 1..=10 {
            est.record(i as f64);
        }
        // Should still hold capacity worth of samples
        assert_eq!(est.len(), 5);
        // total_count reflects all observations
        assert_eq!(est.sample_count(), 10);
    }

    // ---- LatencyStats ----

    #[test]
    fn latency_stats_record_and_query() {
        let mut ls = LatencyStats::new();
        assert!(ls.is_empty());

        ls.record(100.0);
        ls.record(200.0);
        ls.record(300.0);

        assert_eq!(ls.sample_count(), 3);
        assert!(!ls.is_empty());

        let p50 = ls.p50().unwrap();
        let p95 = ls.p95().unwrap();
        assert!(
            (p50 - 200.0).abs() < 1.0,
            "p50 should be ~200, got {p50}"
        );
        assert!(
            (p95 - 300.0).abs() < 1.0,
            "p95 should be ~300, got {p95}"
        );

        assert_eq!(ls.min(), Some(100.0));
        assert_eq!(ls.max(), Some(300.0));
        assert!(ls.ewma.is_known());
    }

    // ---- FailureStats ----

    #[test]
    fn failure_stats_by_class_counting() {
        let mut fs = FailureStats::default();
        assert_eq!(fs.total, 0);

        fs.record(FailureClass::Timeout);
        fs.record(FailureClass::Timeout);
        fs.record(FailureClass::RateLimit);

        assert_eq!(fs.total, 3);
        assert_eq!(fs.count(FailureClass::Timeout), 2);
        assert_eq!(fs.count(FailureClass::RateLimit), 1);
        assert_eq!(fs.count(FailureClass::Transport), 0);
    }

    // ---- ProviderModelStats ----

    #[test]
    fn provider_model_stats_success_rate() {
        let mut ps = ProviderModelStats::new("gpt-4".into(), "openai".into());
        assert_eq!(ps.success_rate(), 0.0);

        ps.record_success(100.0, Some(50.0));
        ps.record_success(200.0, Some(80.0));
        assert_eq!(ps.success_rate(), 1.0);

        ps.record_failure(FailureClass::Timeout);
        assert_eq!(ps.total_requests, 3);
        assert_eq!(ps.total_successes, 2);
        assert_eq!(ps.total_failures, 1);
        let rate = ps.success_rate();
        assert!(
            (rate - 2.0 / 3.0).abs() < 1e-9,
            "success_rate should be ~0.667, got {rate}"
        );

        // Latency stats should have 2 samples
        assert_eq!(ps.total_latency.sample_count(), 2);
        assert_eq!(ps.ttft.sample_count(), 2);
    }

    #[test]
    fn provider_model_stats_failure_breakdown() {
        let mut ps = ProviderModelStats::new("m".into(), "p".into());
        ps.record_failure(FailureClass::Transport);
        ps.record_failure(FailureClass::RateLimit);
        ps.record_failure(FailureClass::Transport);

        assert_eq!(ps.failures.count(FailureClass::Transport), 2);
        assert_eq!(ps.failures.count(FailureClass::RateLimit), 1);
    }

    // ---- StatsStore ----

    #[test]
    fn stats_store_isolation_between_providers() {
        let store = StatsStore::new();
        store.record_success("m", "p1", 100.0, None);
        store.record_success("m", "p2", 200.0, None);
        store.record_success("m", "p2", 300.0, None);

        let p1 = store.get("m", "p1");
        let p2 = store.get("m", "p2");

        assert_eq!(p1.total_requests, 1);
        assert_eq!(p2.total_requests, 2);
    }

    #[test]
    fn stats_store_accumulates_across_calls() {
        let store = StatsStore::new();
        store.record_success("model", "provider", 100.0, Some(40.0));
        store.record_success("model", "provider", 200.0, Some(60.0));
        store.record_classified_failure("model", "provider", FailureClass::Timeout);

        let stats = store.get("model", "provider");
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.total_successes, 2);
        assert_eq!(stats.total_failures, 1);
        assert_eq!(stats.total_latency.sample_count(), 2);
        assert_eq!(stats.ttft.sample_count(), 2);
        assert_eq!(stats.failures.count(FailureClass::Timeout), 1);
    }

    #[test]
    fn stats_store_get_unknown_returns_empty() {
        let store = StatsStore::new();
        let stats = store.get("unknown", "unknown");
        assert_eq!(stats.total_requests, 0);
        assert!(stats.total_latency.is_empty());
    }

    // ---- Critical invariant ----

    #[test]
    fn invariant_ewma_value_always_between_min_and_max() {
        let mut ls = LatencyStats::new();
        let values = [50.0, 200.0, 10.0, 500.0, 75.0, 300.0, 1.0, 1000.0];
        for &v in &values {
            ls.record(v);
            let ewma = ls.ewma.value.unwrap();
            let min = ls.min().unwrap();
            let max = ls.max().unwrap();
            assert!(
                ewma >= min && ewma <= max,
                "ewma {ewma} must be in [{min}, {max}] after recording {v}"
            );
        }
    }
}
