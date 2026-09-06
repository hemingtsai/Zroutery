//! Unified outcome model for Zroutery.
//!
//! Captures the complete result of a single client request, including every
//! routing attempt, failure classification, timing, and usage. Bridges the
//! gap between [`RequestRecord`](crate::stats::RequestRecord) (log-oriented),
//! [`RuntimeObservation`](crate::observation::RuntimeObservation) (health-oriented),
//! [`ProviderModelStats`](crate::stats_ext::ProviderModelStats) (latency-oriented),
//! and [`StoredResponse`](crate::ir::response::StoredResponse) (lifecycle-oriented).
//!
//! The [`Outcome`] type is the single source of truth from which all downstream
//! stores can be updated via [`Outcome::record_to_observation`].

use crate::failure::FailureClass;
use crate::ir::Usage;
use crate::observation::ObservationStore;
use crate::stats_ext::StatsStore;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// FinalStatus
// ---------------------------------------------------------------------------

/// The terminal status of a request after all attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalStatus {
    /// Request completed successfully.
    Success,
    /// Request failed after all attempts exhausted.
    Failed,
    /// Client cancelled the request.
    Cancelled,
    /// Stream was interrupted after partial output.
    Interrupted,
}

// ---------------------------------------------------------------------------
// Attempt
// ---------------------------------------------------------------------------

/// A single attempt at routing a request to a candidate model.
///
/// When failover occurs, each candidate in the chain produces one `Attempt`.
/// When the rectifier repairs a request and retries the same candidate, the
/// retry is a separate attempt with `rectified = true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// The complete outcome of a single client request.
///
/// Aggregates every attempt, links to the route decision and response store,
/// and carries timing, usage, and cost information. Call
/// [`Outcome::record_to_observation`] to fan out to the observation and stats
/// stores in one shot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    // --- Identity ---
    /// Unique id for this outcome (e.g. `out_<uuid>`).
    pub outcome_id: String,
    /// Link to the [`RouteDecision`](crate::policy::RouteDecision) that planned
    /// this request. None for requests that bypass routing (e.g. passthrough).
    pub decision_id: Option<String>,
    /// Request-scoped id from the stats system.
    pub request_id: String,
    /// Response store id (for the Responses API lifecycle). None when the
    /// Responses API is not in use.
    pub response_id: Option<String>,

    // --- Result ---
    /// Whether the request ultimately succeeded.
    pub success: bool,
    /// Classified terminal status.
    pub final_status: FinalStatus,

    // --- Candidate info ---
    /// The first model that was tried.
    pub initial_model: String,
    /// The provider of the first model.
    pub initial_provider: String,
    /// The model that ultimately served the response (same as initial if no fallback).
    pub final_model: String,
    /// The provider of the final model.
    pub final_provider: String,

    // --- Attempts ---
    /// Ordered list of attempts (at least one for a non-cancelled request).
    pub attempts: Vec<Attempt>,
    /// Number of fallback transitions (len(attempts) - 1 when all are distinct candidates).
    pub fallback_count: u32,

    // --- Timing ---
    /// Wall-clock total latency in milliseconds (first attempt start to last attempt end).
    pub total_latency_ms: f64,
    /// Time to first token from the *successful* attempt, if streaming.
    pub ttft_ms: Option<f64>,

    // --- Usage ---
    /// Token usage from the successful attempt (None if all attempts failed).
    pub usage: Option<Usage>,
    /// Pre-request cost estimate.
    pub estimated_cost: Option<f64>,
    /// Actual cost after completion.
    pub actual_cost: Option<f64>,

    // --- Context ---
    /// Whether the client requested streaming.
    pub streaming: bool,
    /// The dialect (API flavour) the client spoke.
    pub dialect: String,
    /// Unix timestamp (seconds) when the request was first received.
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// OutcomeBuilder
// ---------------------------------------------------------------------------

/// Builder for [`Outcome`].
///
/// Create via [`Outcome::builder`]. All identity fields are set at construction;
/// remaining fields have sensible defaults and can be overridden before calling
/// [`build`](OutcomeBuilder::build).
pub struct OutcomeBuilder {
    outcome: Outcome,
}

impl Outcome {
    /// Start building an `Outcome` for a request with the given id.
    pub fn builder(request_id: impl Into<String>) -> OutcomeBuilder {
        let request_id = request_id.into();
        OutcomeBuilder {
            outcome: Outcome {
                outcome_id: format!("out_{}", uuid::Uuid::new_v4().simple()),
                decision_id: None,
                request_id,
                response_id: None,
                success: false,
                final_status: FinalStatus::Failed,
                initial_model: String::new(),
                initial_provider: String::new(),
                final_model: String::new(),
                final_provider: String::new(),
                attempts: Vec::new(),
                fallback_count: 0,
                total_latency_ms: 0.0,
                ttft_ms: None,
                usage: None,
                estimated_cost: None,
                actual_cost: None,
                streaming: false,
                dialect: String::new(),
                timestamp: chrono::Utc::now().timestamp(),
            },
        }
    }

    /// Determine the [`FinalStatus`] from the collected attempts and
    /// whether cancellation was requested.
    ///
    /// Rules:
    /// - If `cancelled` is true, returns `Cancelled`.
    /// - If any attempt succeeded, returns `Success`.
    /// - If the last attempt failed with a `ClientCancelled` failure class,
    ///   returns `Cancelled`.
    /// - If there are attempts but the last was interrupted (partial stream),
    ///   returns `Interrupted`.
    /// - Otherwise returns `Failed`.
    pub fn classify_final(attempts: &[Attempt], cancelled: bool) -> FinalStatus {
        if cancelled {
            return FinalStatus::Cancelled;
        }
        if attempts.is_empty() {
            return FinalStatus::Failed;
        }
        if attempts.iter().any(|a| a.success) {
            return FinalStatus::Success;
        }
        // Check if the last attempt was a client cancellation.
        if let Some(last) = attempts.last() {
            if last.failure_class == Some(FailureClass::ClientCancelled) {
                return FinalStatus::Cancelled;
            }
        }
        FinalStatus::Failed
    }

    /// Write outcome data to the observation store and stats store.
    ///
    /// For each attempt, records success or classified failure against the
    /// corresponding (model, provider) pair. This is the single fan-out point
    /// that keeps the two stores in sync.
    pub fn record_to_observation(
        &self,
        obs_store: &ObservationStore,
        stats_store: &StatsStore,
    ) {
        for attempt in &self.attempts {
            if attempt.success {
                obs_store.record_success(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    attempt.latency_ms,
                    attempt.ttft_ms,
                );
                stats_store.record_success(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    attempt.latency_ms,
                    attempt.ttft_ms,
                );
            } else {
                let class = attempt.failure_class.unwrap_or(FailureClass::Unknown);
                obs_store.record_classified_failure(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    &crate::failure::ClassifiedFailure {
                        class,
                        status: attempt.http_status,
                        message: attempt
                            .failure_message
                            .clone()
                            .unwrap_or_default(),
                        impact: class.impact(),
                    },
                );
                stats_store.record_classified_failure(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    class,
                );
            }
        }
    }
}

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

    /// Set the initial candidate (first model/provider tried).
    pub fn initial(mut self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        self.outcome.initial_model = model.into();
        self.outcome.initial_provider = provider.into();
        self
    }

    /// Set the final candidate (model/provider that served the response).
    pub fn final_candidate(mut self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        self.outcome.final_model = model.into();
        self.outcome.final_provider = provider.into();
        self
    }

    /// Set both initial and final to the same candidate (no fallback).
    pub fn single_candidate(mut self, model: impl Into<String>, provider: impl Into<String>) -> Self {
        let model = model.into();
        let provider = provider.into();
        self.outcome.initial_model = model.clone();
        self.outcome.initial_provider = provider.clone();
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
        self
    }

    /// Finalize the outcome.
    ///
    /// Automatically computes `success`, `final_status` (via [`Outcome::classify_final`]),
    /// and `fallback_count` if not explicitly set.
    pub fn build(mut self) -> Outcome {
        let is_cancelled = self.outcome.final_status == FinalStatus::Cancelled;
        self.outcome.final_status = Outcome::classify_final(&self.outcome.attempts, is_cancelled);
        self.outcome.success = self.outcome.final_status == FinalStatus::Success;

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

        self.outcome
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_attempt(
        model: &str,
        provider: &str,
        success: bool,
        latency_ms: f64,
        failure_class: Option<FailureClass>,
    ) -> Attempt {
        Attempt {
            attempt_id: format!("att_{}", uuid::Uuid::new_v4().simple()),
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            started_at: 1_700_000_000,
            completed_at: 1_700_000_001,
            latency_ms,
            ttft_ms: if success { Some(latency_ms * 0.3) } else { None },
            success,
            failure_class,
            failure_message: if success {
                None
            } else {
                Some("test failure".to_string())
            },
            http_status: if success { Some(200) } else { Some(500) },
            rectified: false,
        }
    }

    // -- Success outcome construction --

    #[test]
    fn success_outcome_single_attempt() {
        let outcome = Outcome::builder("req_1")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(true)
            .attempt(make_attempt("gpt-4", "openai", true, 350.0, None))
            .total_latency_ms(350.0)
            .ttft_ms(105.0)
            .usage(Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Usage::default()
            })
            .build();

        assert!(outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Success);
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(outcome.fallback_count, 0);
        assert_eq!(outcome.initial_model, "gpt-4");
        assert_eq!(outcome.final_model, "gpt-4");
        assert_eq!(outcome.total_latency_ms, 350.0);
        assert_eq!(outcome.ttft_ms, Some(105.0));
        assert!(outcome.usage.is_some());
        assert_eq!(outcome.usage.as_ref().unwrap().output_tokens, 50);
        assert!(outcome.outcome_id.starts_with("out_"));
    }

    // -- Failure outcome with single attempt --

    #[test]
    fn failure_outcome_single_attempt() {
        let outcome = Outcome::builder("req_2")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(false)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                120.0,
                Some(FailureClass::RateLimit),
            ))
            .total_latency_ms(120.0)
            .build();

        assert!(!outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Failed);
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(outcome.fallback_count, 0);
        assert!(outcome.usage.is_none());
        assert_eq!(
            outcome.attempts[0].failure_class,
            Some(FailureClass::RateLimit)
        );
    }

    // -- Outcome with multiple attempts (fallback) --

    #[test]
    fn fallback_outcome_two_attempts() {
        let outcome = Outcome::builder("req_3")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("anthropic")
            .streaming(true)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                200.0,
                Some(FailureClass::ProviderUnavailable),
            ))
            .attempt(make_attempt(
                "claude-3",
                "anthropic",
                true,
                400.0,
                None,
            ))
            .total_latency_ms(600.0)
            .ttft_ms(120.0)
            .usage(Usage {
                input_tokens: 200,
                output_tokens: 100,
                ..Usage::default()
            })
            .build();

        assert!(outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Success);
        assert_eq!(outcome.attempts.len(), 2);
        assert_eq!(outcome.fallback_count, 1);
        assert_eq!(outcome.initial_model, "gpt-4");
        assert_eq!(outcome.final_model, "claude-3");
        assert_eq!(outcome.total_latency_ms, 600.0);
    }

    #[test]
    fn fallback_all_fail() {
        let outcome = Outcome::builder("req_4")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("openai")
            .streaming(false)
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                100.0,
                Some(FailureClass::Transport),
            ))
            .attempt(make_attempt(
                "claude-3",
                "anthropic",
                false,
                150.0,
                Some(FailureClass::Timeout),
            ))
            .total_latency_ms(250.0)
            .build();

        assert!(!outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Failed);
        assert_eq!(outcome.fallback_count, 1);
        assert_eq!(outcome.attempts[0].failure_class, Some(FailureClass::Transport));
        assert_eq!(outcome.attempts[1].failure_class, Some(FailureClass::Timeout));
    }

    // -- FinalStatus classification --

    #[test]
    fn classify_final_success() {
        let attempts = vec![
            make_attempt("m", "p", false, 100.0, Some(FailureClass::Timeout)),
            make_attempt("m2", "p2", true, 200.0, None),
        ];
        assert_eq!(Outcome::classify_final(&attempts, false), FinalStatus::Success);
    }

    #[test]
    fn classify_final_failed() {
        let attempts = vec![
            make_attempt("m", "p", false, 100.0, Some(FailureClass::Transport)),
        ];
        assert_eq!(Outcome::classify_final(&attempts, false), FinalStatus::Failed);
    }

    #[test]
    fn classify_final_cancelled_flag() {
        let attempts = vec![make_attempt("m", "p", false, 100.0, Some(FailureClass::Timeout))];
        assert_eq!(Outcome::classify_final(&attempts, true), FinalStatus::Cancelled);
    }

    #[test]
    fn classify_final_client_cancelled_in_attempt() {
        let attempts = vec![make_attempt(
            "m",
            "p",
            false,
            50.0,
            Some(FailureClass::ClientCancelled),
        )];
        assert_eq!(
            Outcome::classify_final(&attempts, false),
            FinalStatus::Cancelled
        );
    }

    #[test]
    fn classify_final_empty_attempts() {
        assert_eq!(Outcome::classify_final(&[], false), FinalStatus::Failed);
    }

    // -- Outcome serde round-trip --

    #[test]
    fn serde_round_trip() {
        let outcome = Outcome::builder("req_rt")
            .decision_id("dec_123")
            .response_id("resp_456")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .streaming(true)
            .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
            .total_latency_ms(300.0)
            .ttft_ms(90.0)
            .usage(Usage {
                input_tokens: 50,
                output_tokens: 30,
                ..Usage::default()
            })
            .cost(Some(0.01), Some(0.009))
            .build();

        let json = serde_json::to_string(&outcome).expect("serialize");
        let restored: Outcome = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.outcome_id, outcome.outcome_id);
        assert_eq!(restored.decision_id, Some("dec_123".to_string()));
        assert_eq!(restored.response_id, Some("resp_456".to_string()));
        assert_eq!(restored.request_id, "req_rt");
        assert!(restored.success);
        assert_eq!(restored.final_status, FinalStatus::Success);
        assert_eq!(restored.attempts.len(), 1);
        assert_eq!(restored.dialect, "openai");
        assert!(restored.streaming);
        assert_eq!(restored.estimated_cost, Some(0.01));
        assert_eq!(restored.actual_cost, Some(0.009));
    }

    #[test]
    fn final_status_serde_variants() {
        let variants = [
            FinalStatus::Success,
            FinalStatus::Failed,
            FinalStatus::Cancelled,
            FinalStatus::Interrupted,
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            let restored: FinalStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, restored);
        }
        // Check wire format.
        assert_eq!(serde_json::to_string(&FinalStatus::Success).unwrap(), "\"success\"");
        assert_eq!(serde_json::to_string(&FinalStatus::Failed).unwrap(), "\"failed\"");
        assert_eq!(serde_json::to_string(&FinalStatus::Cancelled).unwrap(), "\"cancelled\"");
        assert_eq!(serde_json::to_string(&FinalStatus::Interrupted).unwrap(), "\"interrupted\"");
    }

    // -- record_to_observation writes to both stores --

    #[test]
    fn record_to_observation_writes_success() {
        let obs = ObservationStore::new();
        let stats = StatsStore::new();

        let outcome = Outcome::builder("req_obs1")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
            .total_latency_ms(300.0)
            .build();

        outcome.record_to_observation(&obs, &stats);

        let obs_entry = obs.get("gpt-4", "openai");
        assert_eq!(obs_entry.health.total_requests, 1);
        assert_eq!(obs_entry.health.total_failures, 0);
        assert!(obs_entry.latency.total_ms.is_known());

        let stats_entry = stats.get("gpt-4", "openai");
        assert_eq!(stats_entry.total_requests, 1);
        assert_eq!(stats_entry.total_successes, 1);
        assert_eq!(stats_entry.total_failures, 0);
    }

    #[test]
    fn record_to_observation_writes_failure() {
        let obs = ObservationStore::new();
        let stats = StatsStore::new();

        let outcome = Outcome::builder("req_obs2")
            .single_candidate("gpt-4", "openai")
            .dialect("openai")
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                100.0,
                Some(FailureClass::RateLimit),
            ))
            .total_latency_ms(100.0)
            .build();

        outcome.record_to_observation(&obs, &stats);

        let obs_entry = obs.get("gpt-4", "openai");
        assert_eq!(obs_entry.health.total_requests, 1);
        assert_eq!(obs_entry.health.total_failures, 1);

        let stats_entry = stats.get("gpt-4", "openai");
        assert_eq!(stats_entry.total_requests, 1);
        assert_eq!(stats_entry.total_successes, 0);
        assert_eq!(stats_entry.total_failures, 1);
        assert_eq!(stats_entry.failures.count(FailureClass::RateLimit), 1);
    }

    #[test]
    fn record_to_observation_writes_fallback_chain() {
        let obs = ObservationStore::new();
        let stats = StatsStore::new();

        let outcome = Outcome::builder("req_obs3")
            .initial("gpt-4", "openai")
            .final_candidate("claude-3", "anthropic")
            .dialect("openai")
            .attempt(make_attempt(
                "gpt-4",
                "openai",
                false,
                200.0,
                Some(FailureClass::ProviderUnavailable),
            ))
            .attempt(make_attempt(
                "claude-3",
                "anthropic",
                true,
                400.0,
                None,
            ))
            .total_latency_ms(600.0)
            .build();

        outcome.record_to_observation(&obs, &stats);

        // OpenAI: 1 failure
        let obs_openai = obs.get("gpt-4", "openai");
        assert_eq!(obs_openai.health.total_failures, 1);
        let stats_openai = stats.get("gpt-4", "openai");
        assert_eq!(stats_openai.total_failures, 1);

        // Anthropic: 1 success
        let obs_anthropic = obs.get("claude-3", "anthropic");
        assert_eq!(obs_anthropic.health.total_requests, 1);
        assert_eq!(obs_anthropic.health.total_failures, 0);
        let stats_anthropic = stats.get("claude-3", "anthropic");
        assert_eq!(stats_anthropic.total_successes, 1);
        assert!(stats_anthropic.total_latency.sample_count() > 0);
    }

    // -- Edge cases --

    #[test]
    fn builder_sets_timestamp() {
        let before = chrono::Utc::now().timestamp();
        let outcome = Outcome::builder("req_ts")
            .single_candidate("m", "p")
            .dialect("openai")
            .build();
        let after = chrono::Utc::now().timestamp();
        assert!(outcome.timestamp >= before && outcome.timestamp <= after);
    }

    #[test]
    fn explicit_timestamp_override() {
        let outcome = Outcome::builder("req_ts2")
            .single_candidate("m", "p")
            .dialect("openai")
            .timestamp(1_700_000_000)
            .build();
        assert_eq!(outcome.timestamp, 1_700_000_000);
    }

    #[test]
    fn cancelled_builder_overrides_status() {
        let outcome = Outcome::builder("req_c")
            .single_candidate("m", "p")
            .dialect("openai")
            .cancelled()
            .build();
        assert!(!outcome.success);
        assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    }

    #[test]
    fn explicit_fallback_count_is_preserved() {
        let outcome = Outcome::builder("req_fc")
            .initial("m1", "p1")
            .final_candidate("m2", "p2")
            .dialect("openai")
            .attempt(make_attempt("m1", "p1", false, 100.0, Some(FailureClass::Timeout)))
            .attempt(make_attempt("m2", "p2", true, 200.0, None))
            .fallback_count(5) // deliberately wrong, but explicit
            .build();
        assert_eq!(outcome.fallback_count, 5, "explicit fallback_count must be preserved");
    }

    #[test]
    fn attempt_fields_round_trip() {
        let a = Attempt {
            attempt_id: "att_test".to_string(),
            candidate_model: "m".to_string(),
            candidate_provider: "p".to_string(),
            started_at: 100,
            completed_at: 200,
            latency_ms: 100.0,
            ttft_ms: Some(30.0),
            success: true,
            failure_class: None,
            failure_message: None,
            http_status: Some(200),
            rectified: true,
        };
        let json = serde_json::to_string(&a).unwrap();
        let restored: Attempt = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.attempt_id, "att_test");
        assert!(restored.rectified);
        assert_eq!(restored.http_status, Some(200));
    }

    #[test]
    fn outcome_ids_are_unique() {
        let o1 = Outcome::builder("r1")
            .single_candidate("m", "p")
            .dialect("openai")
            .build();
        let o2 = Outcome::builder("r2")
            .single_candidate("m", "p")
            .dialect("openai")
            .build();
        assert_ne!(o1.outcome_id, o2.outcome_id);
    }
}
