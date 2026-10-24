//! Writing health back: observations, circuit state, and the counters everything else reads.

use std::time::{Duration, Instant};

use super::candidate::ModelHealth;
use super::helpers::classification_trace_label;
use super::tiers::HealthState;
use super::Router;
use crate::circuit_breaker::CircuitState;
use crate::config::RoutingConfig;
use crate::error::Error;
use crate::failure::{ClassifiedFailure, FailureClass};

impl Router {
    /// Apply a canonical classified failure to the legacy health/circuit view.
    ///
    /// Observation and circuit accounting intentionally use separate impact
    /// bits. For example, a rate limit degrades observations but must not open
    /// the circuit; missing keys and local configuration failures do neither.
    pub fn report_classified_failure(
        &self,
        model_id: &str,
        failure: &ClassifiedFailure,
        routing: &RoutingConfig,
    ) {
        self.update_classified_health(model_id, failure, routing, None);
    }

    pub(super) fn update_classified_health(
        &self,
        model_id: &str,
        failure: &ClassifiedFailure,
        routing: &RoutingConfig,
        safe_message: Option<String>,
    ) {
        if !failure.affects_observation() && !failure.affects_circuit() {
            // A local failure (for example a missing secret) carries no evidence
            // about the model, so it must not create a health row. It is still a
            // terminal outcome for the attempt, so any half-open probe permit it
            // claimed is returned here. Otherwise the breaker stays HalfOpen with
            // no permit and cannot send again.
            self.release_half_open_permit(model_id);
            return;
        }
        let mut health = crate::sync::lock(&self.health);
        let h = health
            .entry(model_id.to_string())
            .or_insert_with(|| HealthState::new(routing.circuit_breaker.clone()));
        // A configuration hot swap reaches a model that already has a health
        // row through the routing config this report carries.
        h.breaker.set_config(routing.circuit_breaker.clone());
        if failure.affects_circuit() {
            h.breaker.record_failure();
        } else {
            // A neutral failure (rate limit, authentication, bad request) does
            // not change circuit health, but it does end the probe. Return the
            // permit so a later request can probe again instead of the model
            // being stuck at zero permits with no second cooldown. Turning the
            // rate limit into a circuit failure would be the wrong fix.
            h.breaker.release_half_open_permit();
        }
        if failure.affects_observation() || failure.affects_circuit() {
            h.total_failure += 1;
        }
        // Store only a fixed classification token for a caller-supplied
        // ClassifiedFailure. The legacy Error wrapper may provide its already
        // redacted `safe_message` explicitly below.
        h.last_error = Some(safe_message.unwrap_or_else(|| classification_trace_label(failure)));
    }

    /// Apply one canonical classified attempt to both router health views.
    /// This is the preferred adapter for a caller that has a provider id and
    /// owns the complete attempt result; it avoids separate classifiers and
    /// keeps circuit, observation, and stats decisions on one impact table.
    pub fn record_classified_attempt(
        &self,
        model_id: &str,
        provider_id: &str,
        failure: &ClassifiedFailure,
        routing: &RoutingConfig,
    ) {
        self.report_classified_failure(model_id, failure, routing);
        self.record_classified_failure(model_id, provider_id, failure);
        self.note_provider_rate_limit(provider_id, failure, routing);
    }

    /// Refresh the set of providers whose rate limit covers every model.
    ///
    /// Called when the configuration is built or swapped. Gates for providers
    /// that no longer exist are dropped with the rest: a provider id is only
    /// meaningful while something serves it.
    pub fn set_group_limited_providers(&self, ids: impl IntoIterator<Item = String>) {
        let ids: std::collections::HashSet<String> = ids.into_iter().collect();
        crate::sync::lock(&self.provider_gates).retain(|id, _| ids.contains(id));
        *crate::sync::lock(&self.group_limited) = ids;
    }

    /// Is this provider's shared cooldown still running?
    ///
    /// Consumes nothing — unlike [`Self::allow_request`] it never claims a
    /// half-open probe — so eligibility filtering can ask it freely. An
    /// elapsed window is dropped as it is read.
    pub fn is_provider_gated(&self, provider_id: &str) -> bool {
        let now = Instant::now();
        let mut gates = crate::sync::lock(&self.provider_gates);
        match gates.get(provider_id) {
            Some(until) if now < *until => true,
            Some(_) => {
                // The window elapsed; drop the row so the map does not retain
                // every provider that was ever rate limited.
                gates.remove(provider_id);
                false
            }
            None => false,
        }
    }

    /// Open the provider-wide gate when a shared-limit provider answers 429.
    ///
    /// The per-model impact table deliberately keeps a rate limit off the
    /// circuit: a quota is not a sick model. That stays true. What changes for
    /// a group-limited provider is that the *pool* is unusable, so the gate
    /// blocks the provider's other models for the same cooldown the circuit
    /// breaker would have used. When the window elapses the next request
    /// probes normally; a still-exhausted quota answers 429 again and reopens
    /// it.
    fn note_provider_rate_limit(
        &self,
        provider_id: &str,
        failure: &ClassifiedFailure,
        routing: &RoutingConfig,
    ) {
        if failure.class != FailureClass::RateLimit {
            return;
        }
        if !crate::sync::lock(&self.group_limited).contains(provider_id) {
            return;
        }
        let cooldown = routing.circuit_breaker.timeout_secs.max(1);
        let until = Instant::now() + Duration::from_secs(cooldown);
        crate::sync::lock(&self.provider_gates).insert(provider_id.to_string(), until);
    }

    /// Alias for [`Self::record_classified_attempt`] used by report-oriented
    /// adapters.
    pub fn report_classified_attempt(
        &self,
        model_id: &str,
        provider_id: &str,
        failure: &ClassifiedFailure,
        routing: &RoutingConfig,
    ) {
        self.record_classified_attempt(model_id, provider_id, failure, routing);
    }

    /// Record a canonical failure in observation/statistics stores without
    /// touching the legacy circuit breaker. The caller can pair this with
    /// [`Self::report_classified_failure`] when it owns both health views.
    ///
    /// Circuit *health* is untouched, but a failure that does not affect the
    /// circuit still terminates an in-flight attempt, so any half-open probe
    /// permit that attempt claimed is returned. This is idempotent and is what
    /// keeps a neutral failure from parking the model in `HalfOpen` with no
    /// permit.
    pub fn record_classified_failure(
        &self,
        model_id: &str,
        provider_id: &str,
        failure: &ClassifiedFailure,
    ) {
        if !failure.affects_circuit() {
            self.release_half_open_permit(model_id);
        }
        if failure.affects_observation() {
            self.observations.record_failure(model_id, provider_id);
        }
        self.stats_store
            .record_classified_failure(model_id, provider_id, failure.class);
    }

    pub fn report_success(&self, model_id: &str, latency_ms: u64, routing: &RoutingConfig) {
        let mut health = crate::sync::lock(&self.health);
        let h = health
            .entry(model_id.to_string())
            .or_insert_with(|| HealthState::new(routing.circuit_breaker.clone()));
        h.breaker.set_config(routing.circuit_breaker.clone());
        h.breaker.record_success();
        h.total_success += 1;
        h.last_error = None;
        h.avg_latency_ms = if h.avg_latency_ms == 0.0 {
            latency_ms as f64
        } else {
            h.avg_latency_ms * 0.7 + latency_ms as f64 * 0.3
        };
    }

    pub fn report_failure(&self, model_id: &str, error: &Error, routing: &RoutingConfig) {
        // Keep the legacy Error-shaped API, but make the canonical classified
        // result the sole input to every health decision.
        let failure = error.classified();
        self.update_classified_health(model_id, &failure, routing, Some(error.safe_message()));
    }

    /// Whether a request may actually be sent to this model.
    ///
    /// This is the router-level wrapper around [`CircuitBreaker::allow_request`].
    /// It is called by the pipeline right before an upstream send, so half-open
    /// probes are limited to one in flight per model.
    pub fn allow_request(&self, model_id: &str) -> bool {
        let health = crate::sync::lock(&self.health);
        match health.get(model_id) {
            Some(h) => h.breaker.allow_request(),
            // Models without health data are new and therefore closed.
            None => true,
        }
    }

    /// [`Self::allow_request`] plus the provider's shared rate-limit gate.
    ///
    /// A provider whose quota is exhausted refuses every model it serves, so
    /// the send-time check has to see the gate the per-model breaker knows
    /// nothing about.
    pub fn allow_request_for(&self, model_id: &str, provider_id: &str) -> bool {
        !self.is_provider_gated(provider_id) && self.allow_request(model_id)
    }

    /// Release a half-open probe permit after a rectifier retry.
    pub fn release_half_open_permit(&self, model_id: &str) {
        let health = crate::sync::lock(&self.health);
        if let Some(h) = health.get(model_id) {
            h.breaker.release_half_open_permit();
        }
    }

    /// Record a successful request outcome in the observation store.
    ///
    /// Updates both latency and health signals for the given model/provider pair.
    /// The pipeline calls this alongside [`report_success`] so that the policy
    /// scorer can use runtime observations instead of legacy EWMA values.
    pub fn record_outcome(
        &self,
        model_id: &str,
        provider_id: &str,
        latency_ms: f64,
        ttft_ms: Option<f64>,
    ) {
        self.observations
            .record_success(model_id, provider_id, latency_ms, ttft_ms);
    }

    /// Record a failed request outcome in the observation store.
    ///
    /// Updates the health signal for the given model/provider pair. The pipeline
    /// calls this alongside [`report_failure`] so that the policy scorer can use
    /// runtime observations instead of legacy circuit-breaker state.
    pub fn record_failure(&self, model_id: &str, provider_id: &str) {
        self.observations.record_failure(model_id, provider_id);
    }

    /// Record a classified outcome in both observation and stats stores.
    ///
    /// On success, both stores receive latency and TTFT data. On failure, the
    /// classification determines whether the observation store is updated (client
    /// errors are skipped), while the stats store always records the failure class.
    pub fn record_classified_outcome(
        &self,
        model_id: &str,
        provider_id: &str,
        latency_ms: f64,
        ttft_ms: Option<f64>,
        success: bool,
        failure_class: Option<crate::failure::FailureClass>,
    ) {
        if success {
            self.observations
                .record_success(model_id, provider_id, latency_ms, ttft_ms);
            self.stats_store
                .record_success(model_id, provider_id, latency_ms, ttft_ms);
        } else if let Some(class) = failure_class {
            // Compatibility shape for existing pipeline callers. The class is
            // already canonical; constructing the envelope here avoids a
            // second status/body classifier while routing the actual impact
            // through the same adapter as `ClassifiedFailure` callers.
            let failure = ClassifiedFailure {
                class,
                status: None,
                message: String::new(),
                impact: class.impact(),
            };
            self.record_classified_failure(model_id, provider_id, &failure);
        }
    }

    /// Canonical-failure variant for callers that already performed structural
    /// classification. It intentionally shares the same observation/statistics
    /// path as the legacy class-shaped compatibility method above.
    pub fn record_classified_failure_outcome(
        &self,
        model_id: &str,
        provider_id: &str,
        failure: &ClassifiedFailure,
    ) {
        self.record_classified_failure(model_id, provider_id, failure);
    }

    /// Clear circuit breaker state for one model (GUI "retry now").
    pub fn reset(&self, model_id: &str) {
        let mut health = crate::sync::lock(&self.health);
        if let Some(h) = health.get_mut(model_id) {
            h.breaker.reset();
            h.last_error = None;
        }
    }

    /// Push a routing config's circuit-breaker tunables onto every existing
    /// health row.
    ///
    /// Health rows are keyed by model id and outlive a configuration reload, and
    /// the breaker used to copy its parameters only when the row was first
    /// created. A saved threshold, success threshold or cooldown therefore had no
    /// effect on a model that had ever been seen. This makes the new parameters
    /// take effect for those rows too. The recorded evidence is preserved:
    /// counts, error-rate totals and the `Open`/`HalfOpen` state are kept, so a
    /// config change never resets a tripped breaker behind the operator's back.
    pub fn apply_routing_config(&self, routing: &RoutingConfig) {
        let mut health = crate::sync::lock(&self.health);
        for h in health.values_mut() {
            h.breaker.set_config(routing.circuit_breaker.clone());
        }
    }

    /// Drop health entries for models that no longer exist.
    ///
    /// Called when the configuration changes: without this the map only ever
    /// grew, and entries for deleted models kept showing up in the GUI snapshot
    /// for as long as the process lived. History that still matters survives —
    /// a re-added model id simply starts fresh, like a new install would.
    pub fn retain_models(&self, known: impl Fn(&str) -> bool) {
        let mut health = crate::sync::lock(&self.health);
        health.retain(|id, _| known(id));
    }

    pub fn is_cooling(&self, model_id: &str) -> bool {
        crate::sync::lock(&self.health)
            .get(model_id)
            .map(|h| h.breaker.state() == CircuitState::Open)
            .unwrap_or(false)
    }

    /// Read-only check: is the circuit breaker in Open state?
    ///
    /// Unlike [`allow_request`], this does not consume half-open probe permits
    /// and is safe to call during eligibility filtering where side effects are
    /// not desired. This is the raw state, including an Open breaker whose
    /// cooldown has already elapsed; eligibility filtering wants
    /// [`Self::is_circuit_blocked`] instead.
    pub fn is_circuit_open(&self, model_id: &str) -> bool {
        crate::sync::lock(&self.health)
            .get(model_id)
            .map(|h| h.breaker.state() == CircuitState::Open)
            .unwrap_or(false)
    }

    /// Read-only check: would the breaker refuse a request to this model right
    /// now?
    ///
    /// This is [`Self::is_circuit_open`] narrowed to the part of the Open state
    /// that is still serving its cooldown. Once the configured timeout has
    /// elapsed the breaker is ready to admit a half-open probe, so eligibility
    /// filtering must treat the model as available — rejecting it here would
    /// keep the model out forever because the Open → HalfOpen transition only
    /// happens inside [`Self::allow_request`]. The single probe permit is still
    /// claimed atomically at the point of send, so this predicate is a
    /// classification, not a reservation.
    pub fn is_circuit_blocked(&self, model_id: &str) -> bool {
        crate::sync::lock(&self.health)
            .get(model_id)
            .map(|h| h.breaker.state() == CircuitState::Open && !h.breaker.can_probe())
            .unwrap_or(false)
    }

    pub fn health_snapshot(&self) -> Vec<ModelHealth> {
        let health = crate::sync::lock(&self.health);
        let mut out: Vec<ModelHealth> = health
            .iter()
            .map(|(id, h)| {
                let state = h.breaker.state();
                ModelHealth {
                    model_id: id.clone(),
                    state,
                    consecutive_failures: h.breaker.consecutive_failures(),
                    total_success: h.total_success,
                    total_failure: h.total_failure,
                    avg_latency_ms: h.avg_latency_ms,
                    cooldown_remaining_secs: h.breaker.open_remaining_secs(),
                    last_error: h.last_error.clone(),
                }
            })
            .collect();
        out.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        out
    }
}
