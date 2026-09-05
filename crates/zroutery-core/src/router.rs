//! Candidate selection, health tracking and failover ordering.
//!
//! The router is long lived: it keeps health state across configuration
//! reloads, keyed by exposed model id.

use std::collections::HashMap;
use std::sync::Mutex;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use crate::config::{ClassifierConfig, ModelTier, ModelEntry, ProviderConfig, RoutingConfig, RoutingStrategy};
use crate::election::Election;
use crate::error::{Error, Result};
use crate::ir::Capability;
use crate::registry::{Registry, Resolution};

/// Round robin cursor key for the classifier pool, which is not a tier.
const CLASSIFIER_POOL: &str = "classifier";

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
}

impl Candidate {
    fn new(entry: &ModelEntry, provider: &ProviderConfig, degraded: bool) -> Self {
        Candidate {
            exposed_id: entry.exposed_id(),
            entry: entry.clone(),
            provider: provider.clone(),
            degraded,
        }
    }

    pub fn model_id(&self) -> &str {
        &self.exposed_id
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
    /// Seconds left before an open breaker may probe, 0 when not open.
    pub cooldown_remaining_secs: u64,
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct HealthState {
    breaker: CircuitBreaker,
    total_success: u64,
    total_failure: u64,
    avg_latency_ms: f64,
    last_error: Option<String>,
}

impl HealthState {
    fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breaker: CircuitBreaker::new(config),
            total_success: 0,
            total_failure: 0,
            avg_latency_ms: 0.0,
            last_error: None,
        }
    }
}

#[derive(Debug)]
pub struct Router {
    health: Mutex<HashMap<String, HealthState>>,
    /// Round robin cursors, keyed by pool name (a tier's virtual id, or the
    /// classifier pool) rather than by `ModelTier`: routing pools exist that
    /// are not a tier.
    rr: Mutex<HashMap<String, usize>>,
    /// The last election, when one has been held. `Balanced` follows the order it
    /// decided instead of re-deciding per request, which is the whole point: a
    /// route that changes under load cannot be reasoned about.
    election: Mutex<Option<Election>>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Router {
            health: Mutex::new(HashMap::new()),
            rr: Mutex::new(HashMap::new()),
            election: Mutex::new(None),
        }
    }

    /// Build the ordered list of attempts for a resolved model id.
    pub fn plan(
        &self,
        registry: &Registry,
        resolution: &Resolution,
        required_capabilities: &[Capability],
    ) -> Result<Vec<Candidate>> {
        let routing = &registry.config().routing;
        match resolution {
            Resolution::Direct(id) => {
                let entry = registry.entry(id)?;
                let provider = registry.provider_of(entry)?;
                if !provider.enabled {
                    return Err(Error::UnknownModel(format!("{id} (provider disabled)")));
                }
                Ok(vec![Candidate::new(entry, provider, false)])
            }
            Resolution::Tier(tier) => self.plan_tier(
                registry,
                *tier,
                routing,
                required_capabilities,
                routing.capability_filter,
            ),
        }
    }

    /// Build the ordered attempts for an Auto Mode classifier request.
    ///
    /// The pool is a selection of existing models, so candidates reuse the
    /// registry's identity: provider, secret, protocol, pricing and — through
    /// [`plan_candidates`] — the *same* health state a main request sees. A
    /// provider that is open is open for both; there is deliberately no
    /// separate "classifier health", because the model does not get sicker
    /// depending on who asked.
    ///
    /// Candidates name models by exposed id or alias. Unknown or disabled
    /// references are skipped here rather than failing the plan: validation
    /// already reported them at save time, and one stale entry must not take
    /// the whole classifier pool down.
    pub fn plan_classifier(
        &self,
        registry: &Registry,
        config: &ClassifierConfig,
    ) -> Result<Vec<Candidate>> {
        let mut members: Vec<ModelEntry> = Vec::new();
        for candidate in &config.candidates {
            if !candidate.enabled {
                continue;
            }
            let Ok(entry) = registry.entry(&candidate.model) else {
                tracing::debug!(
                    model = %candidate.model,
                    "classifier candidate is not a configured model; skipping"
                );
                continue;
            };
            if !entry.enabled {
                continue;
            }
            if !registry
                .provider_of(entry)
                .map(|p| p.enabled)
                .unwrap_or(false)
            {
                continue;
            }
            // The pool's own priority orders the candidate here, not the
            // model's tier priority: a model can be last in its tier and
            // first in the classifier pool.
            let mut entry = entry.clone();
            entry.priority = candidate.priority;
            members.push(entry);
        }
        if members.is_empty() {
            return Err(Error::NoCandidate("classifier".to_string()));
        }
        let refs: Vec<&ModelEntry> = members.iter().collect();
        self.plan_candidates(
            registry,
            refs,
            CLASSIFIER_POOL,
            // Elections are held per tier; the classifier pool has no tier,
            // so Balanced degrades to priority order inside `order`.
            None,
            config.strategy,
            config.failover,
            config.max_attempts,
            "classifier".to_string(),
            // Classifier requests don't carry capability requirements.
            &[],
            false,
        )
    }

    fn plan_tier(
        &self,
        registry: &Registry,
        tier: ModelTier,
        routing: &RoutingConfig,
        required_capabilities: &[Capability],
        capability_filter: bool,
    ) -> Result<Vec<Candidate>> {
        let members = registry.tier_members(tier);
        if members.is_empty() {
            return Err(Error::NoCandidate(tier.virtual_id().to_string()));
        }
        self.plan_candidates(
            registry,
            members,
            tier.virtual_id(),
            Some(tier),
            routing.strategy,
            routing.failover,
            routing.max_attempts,
            tier.virtual_id().to_string(),
            required_capabilities,
            capability_filter,
        )
    }

    /// Turn a pool of models into an ordered list of attempts, applying health,
    /// circuit breakers, the strategy's ordering, failover and the attempt cap.
    ///
    /// Every routing pool — a tier today, the Auto Mode classifier pool next —
    /// shares this one implementation, so a candidate means the same thing
    /// wherever it came from: health filtering, degraded marking and failover
    /// cannot drift between pools.
    ///
    /// `counter_key` names the pool for the round robin cursor. `election_tier`
    /// is `Some` only for tier pools: `Balanced` follows the last election's
    /// order, and elections only exist per tier, so a pool without one orders
    /// by priority instead.
    #[allow(clippy::too_many_arguments)]
    fn plan_candidates(
        &self,
        registry: &Registry,
        members: Vec<&ModelEntry>,
        counter_key: &str,
        election_tier: Option<ModelTier>,
        strategy: RoutingStrategy,
        failover: bool,
        max_attempts: u32,
        pool_name: String,
        required_capabilities: &[Capability],
        capability_filter: bool,
    ) -> Result<Vec<Candidate>> {
        // Capability filtering: exclude models whose declared capabilities
        // don't satisfy the request's requirements. Soft fallback — if no
        // candidate survives, keep the original list so the request is not
        // rejected for a missing capability declaration alone.
        let members = if capability_filter && !required_capabilities.is_empty() {
            let filtered: Vec<&ModelEntry> = members
                .iter()
                .copied()
                .filter(|m| satisfies_capabilities(m, required_capabilities))
                .collect();
            if filtered.is_empty() {
                tracing::warn!(
                    required = ?required_capabilities,
                    pool = %pool_name,
                    "no candidate satisfies required capabilities; falling back to unfiltered list"
                );
                members
            } else {
                filtered
            }
        } else {
            members
        };

        let (closed, half_open): (Vec<&ModelEntry>, Vec<&ModelEntry>) = {
            let health = crate::sync::lock(&self.health);
            let mut closed = Vec::new();
            let mut half_open = Vec::new();
            for m in members {
                match health.get(&m.exposed_id()) {
                    Some(h) => match h.breaker.state() {
                        CircuitState::Closed => closed.push(m),
                        CircuitState::HalfOpen => half_open.push(m),
                        // An open circuit that has waited out its timeout is
                        // eligible for a half-open probe; it is still degraded.
                        CircuitState::Open if h.breaker.can_probe() => half_open.push(m),
                        CircuitState::Open => {}
                    },
                    None => closed.push(m),
                }
            }
            (closed, half_open)
        };

        let mut ordered = self.order(&closed, counter_key, election_tier, strategy);
        let mut degraded_ids: Vec<String> = Vec::new();
        if ordered.is_empty() {
            // No closed candidate: fall back to half-open probes, marked degraded.
            ordered = half_open;
            degraded_ids = ordered.iter().map(|m| m.exposed_id()).collect();
        } else if failover {
            // Half-open candidates are allowed but treated as degraded.
            for m in half_open {
                degraded_ids.push(m.exposed_id());
                ordered.push(m);
            }
        }

        let limit = if failover {
            max_attempts.max(1) as usize
        } else {
            1
        };

        let mut out = Vec::new();
        for entry in ordered.into_iter().take(limit) {
            let provider = registry.provider_of(entry)?;
            let degraded = degraded_ids.contains(&entry.exposed_id());
            out.push(Candidate::new(entry, provider, degraded));
        }
        if out.is_empty() {
            return Err(Error::NoCandidate(pool_name));
        }
        Ok(out)
    }

    fn order<'a>(
        &self,
        members: &[&'a ModelEntry],
        counter_key: &str,
        election_tier: Option<ModelTier>,
        strategy: RoutingStrategy,
    ) -> Vec<&'a ModelEntry> {
        if members.len() <= 1 {
            return members.to_vec();
        }
        match strategy {
            // Elections are held per tier; a pool that has none (the classifier
            // pool) falls back to priority, which is what the user configured.
            RoutingStrategy::Balanced => match election_tier {
                Some(tier) => self.elected_order(members, tier),
                None => by_priority(members),
            },
            RoutingStrategy::Priority => {
                let mut groups: Vec<(i32, Vec<&ModelEntry>)> = Vec::new();
                for m in members {
                    match groups.last_mut() {
                        Some((p, g)) if *p == m.priority => g.push(m),
                        _ => groups.push((m.priority, vec![m])),
                    }
                }
                groups.sort_by_key(|(p, _)| *p);
                groups
                    .into_iter()
                    .flat_map(|(_, g)| self.weighted_shuffle(&g))
                    .collect()
            }
            RoutingStrategy::WeightedRandom => self.weighted_shuffle(members),
            RoutingStrategy::RoundRobin => {
                let mut rr = crate::sync::lock(&self.rr);
                let counter = rr.entry(counter_key.to_string()).or_insert(0);
                let start = *counter % members.len();
                *counter = counter.wrapping_add(1);
                let mut v = Vec::with_capacity(members.len());
                v.extend_from_slice(&members[start..]);
                v.extend_from_slice(&members[..start]);
                v
            }
            RoutingStrategy::LowestLatency => {
                let health = crate::sync::lock(&self.health);
                let mut v = members.to_vec();
                v.sort_by(|a, b| {
                    // Unmeasured models sort first so they get probed.
                    let la = health
                        .get(&a.exposed_id())
                        .map(|h| h.avg_latency_ms)
                        .unwrap_or(0.0);
                    let lb = health
                        .get(&b.exposed_id())
                        .map(|h| h.avg_latency_ms)
                        .unwrap_or(0.0);
                    la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
                });
                v
            }
        }
    }

    /// The order the last election decided, for the members that still exist.
    ///
    /// Anything the election did not see — added since, or unavailable when it ran
    /// — goes after what it did, in priority order, so a new model is used but does
    /// not silently take the primary slot it was never measured for. With no
    /// election yet this is plain priority order, which is what the user configured
    /// by hand.
    fn elected_order<'a>(
        &self,
        members: &[&'a ModelEntry],
        tier: ModelTier,
    ) -> Vec<&'a ModelEntry> {
        let guard = crate::sync::lock(&self.election);
        let Some(order) = guard.as_ref().and_then(|e| e.order_for(tier)) else {
            return by_priority(members);
        };

        let mut elected: Vec<&ModelEntry> = Vec::with_capacity(members.len());
        for id in &order {
            if let Some(entry) = members.iter().find(|m| m.exposed_id() == *id) {
                elected.push(entry);
            }
        }
        let unseen: Vec<&ModelEntry> = members
            .iter()
            .filter(|m| !order.iter().any(|id| *id == m.exposed_id()))
            .copied()
            .collect();
        elected.extend(by_priority(&unseen));
        elected
    }

    /// Record an election. The order it decided is used until the next one.
    pub fn set_election(&self, election: Election) {
        *crate::sync::lock(&self.election) = Some(election);
    }

    pub fn election(&self) -> Option<Election> {
        crate::sync::lock(&self.election).clone()
    }

    /// Order a group randomly, with probability proportional to weight.
    fn weighted_shuffle<'a>(&self, members: &[&'a ModelEntry]) -> Vec<&'a ModelEntry> {
        let mut pool: Vec<&ModelEntry> = members.to_vec();
        let mut out = Vec::with_capacity(pool.len());
        let mut rng = rand::thread_rng();
        while !pool.is_empty() {
            let total: u64 = pool.iter().map(|m| m.weight.max(1) as u64).sum();
            let mut pick = rng.gen_range(0..total);
            let mut chosen = 0usize;
            for (i, m) in pool.iter().enumerate() {
                let w = m.weight.max(1) as u64;
                if pick < w {
                    chosen = i;
                    break;
                }
                pick -= w;
            }
            out.push(pool.remove(chosen));
        }
        out
    }

    pub fn report_success(&self, model_id: &str, latency_ms: u64, routing: &RoutingConfig) {
        let mut health = crate::sync::lock(&self.health);
        let h = health
            .entry(model_id.to_string())
            .or_insert_with(|| HealthState::new(routing.circuit_breaker.clone()));
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
        if !error.counts_against_health() {
            return;
        }
        let mut health = crate::sync::lock(&self.health);
        let h = health
            .entry(model_id.to_string())
            .or_insert_with(|| HealthState::new(routing.circuit_breaker.clone()));
        h.breaker.record_failure();
        h.total_failure += 1;
        // The GUI shows this string in the health table; the unredacted body
        // stays in the log where it belongs.
        h.last_error = Some(error.safe_message());
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

    /// Release a half-open probe permit after a rectifier retry.
    pub fn release_half_open_permit(&self, model_id: &str) {
        let health = crate::sync::lock(&self.health);
        if let Some(h) = health.get(model_id) {
            h.breaker.release_half_open_permit();
        }
    }

    /// Clear circuit breaker state for one model (GUI "retry now").
    pub fn reset(&self, model_id: &str) {
        let mut health = crate::sync::lock(&self.health);
        if let Some(h) = health.get_mut(model_id) {
            h.breaker.reset();
            h.last_error = None;
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

/// Priority order with a stable tiebreak, shared by the `Priority` strategy and
/// by `Balanced` when no election has been held (or the pool has no tier).
fn by_priority<'a>(members: &[&'a ModelEntry]) -> Vec<&'a ModelEntry> {
    let mut sorted = members.to_vec();
    sorted.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.exposed_id().cmp(&b.exposed_id()))
    });
    sorted
}

/// Check if a model's capabilities satisfy the request's requirements.
///
/// Every listed capability must be present on the model. With a typed enum,
/// there are no unknown capabilities — every variant is known.
fn satisfies_capabilities(model: &ModelEntry, required: &[Capability]) -> bool {
    required.iter().all(|cap| model.capabilities.supports(*cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ProviderKind};
    use std::sync::Arc;
    fn cfg_with(models: Vec<ModelEntry>) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.providers.push(ProviderConfig::new(
            "p1",
            "P1",
            ProviderKind::OpenAICompatible,
        ));
        cfg.providers
            .push(ProviderConfig::new("p2", "P2", ProviderKind::Anthropic));
        cfg.models = models;
        cfg
    }

    fn reg(cfg: AppConfig) -> Registry {
        Registry::new(Arc::new(cfg))
    }

    fn ids(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|c| c.exposed_id.as_str()).collect()
    }

    #[test]
    fn direct_request_does_not_failover() {
        let r = reg(cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
        ]));
        let router = Router::new();
        let plan = router.plan(&r, &Resolution::Direct("p1-a".into()), &[]).unwrap();
        assert_eq!(ids(&plan), vec!["p1-a"]);
    }

    #[test]
    fn tier_plan_follows_priority_and_respects_max_attempts() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "first", Some(ModelTier::Reasoning)).with_priority(0),
            ModelEntry::for_upstream("p2", "second", Some(ModelTier::Reasoning)).with_priority(10),
            ModelEntry::for_upstream("p1", "third", Some(ModelTier::Reasoning)).with_priority(20),
        ]);
        cfg.routing.max_attempts = 2;
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Reasoning), &[])
            .unwrap();
        assert_eq!(ids(&plan), vec!["p1-first", "p2-second"]);
    }

    #[test]
    fn failover_disabled_yields_single_attempt() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Fast)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Fast)),
        ]);
        cfg.routing.failover = false;
        let r = reg(cfg);
        let plan = Router::new()
            .plan(&r, &Resolution::Tier(ModelTier::Fast), &[])
            .unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn empty_tier_is_an_error() {
        let r = reg(cfg_with(vec![ModelEntry::for_upstream("p1", "a", None)]));
        let err = Router::new()
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap_err();
        assert!(matches!(err, Error::NoCandidate(_)));
    }

    #[test]
    fn circuit_breaker_demotes_then_recovers() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "bad", Some(ModelTier::Standard)).with_priority(0),
            ModelEntry::for_upstream("p2", "good", Some(ModelTier::Standard)).with_priority(5),
        ]);
        cfg.routing.circuit_breaker.timeout_secs = 0;
        let routing = cfg.routing.clone();
        let r = reg(cfg);
        let router = Router::new();

        assert_eq!(
            ids(&router
                .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
                .unwrap())[0],
            "p1-bad"
        );

        let err = Error::Timeout(5);
        for _ in 0..routing.circuit_breaker.failure_threshold {
            router.report_failure("p1-bad", &err, &routing);
        }
        assert!(router.is_cooling("p1-bad"));
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap();
        assert_eq!(ids(&plan), vec!["p2-good", "p1-bad"]);
        assert!(!plan[0].degraded && plan[1].degraded);

        // The timeout has already elapsed (0s), so the router offers the model
        // as a half-open probe. One probe success is not enough; the configured
        // success threshold (2) must be reached before it is healthy again.
        assert!(router.allow_request("p1-bad"));
        router.report_success("p1-bad", 10, &routing);
        assert!(
            router.health_snapshot()[0].state == CircuitState::HalfOpen,
            "a single probe success should not close the breaker yet"
        );
        router.report_success("p1-bad", 10, &routing);
        assert_eq!(router.health_snapshot()[0].state, CircuitState::Closed);
        assert_eq!(
            ids(&router
                .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
                .unwrap())[0],
            "p1-bad"
        );
    }

    #[test]
    fn a_recovered_breaker_needs_fresh_evidence_to_trip_again() {
        let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "flaky",
            Some(ModelTier::Reasoning),
        )]);
        cfg.routing.circuit_breaker.timeout_secs = 0;
        let routing = cfg.routing.clone();
        let router = Router::new();
        let err = Error::Timeout(1);

        for _ in 0..routing.circuit_breaker.failure_threshold {
            router.report_failure("p1-flaky", &err, &routing);
        }
        assert!(router.is_cooling("p1-flaky"));

        // Move through half-open to a fully closed breaker.
        assert!(router.allow_request("p1-flaky"));
        router.report_success("p1-flaky", 10, &routing);
        router.report_success("p1-flaky", 10, &routing);
        assert!(!router.is_cooling("p1-flaky"));

        // A single transient blip after recovery is not a pattern yet.
        router.report_failure("p1-flaky", &err, &routing);
        assert!(
            !router.is_cooling("p1-flaky"),
            "one failure after recovery should not re-open the breaker"
        );

        // Reaching the threshold again with new failures does.
        for _ in 1..routing.circuit_breaker.failure_threshold {
            router.report_failure("p1-flaky", &err, &routing);
        }
        assert!(router.is_cooling("p1-flaky"));
    }

    #[test]
    fn retain_models_drops_entries_that_no_longer_exist() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "kept",
            Some(ModelTier::Reasoning),
        )]);
        let routing = cfg.routing.clone();
        let router = Router::new();
        router.report_failure("p1-kept", &Error::Timeout(1), &routing);
        router.report_failure("p1-gone", &Error::Timeout(1), &routing);

        let known: std::collections::HashSet<String> =
            ["p1-kept".to_string()].into_iter().collect();
        router.retain_models(|id| known.contains(id));

        let snapshot = router.health_snapshot();
        let ids: Vec<&str> = snapshot.iter().map(|h| h.model_id.as_str()).collect();
        assert_eq!(ids, vec!["p1-kept"]);
    }

    #[test]
    fn all_open_with_timeout_elapsed_still_produces_a_probe_plan() {
        let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "only",
            Some(ModelTier::Reasoning),
        )]);
        cfg.routing.circuit_breaker.timeout_secs = 0;
        let routing = cfg.routing.clone();
        let r = reg(cfg);
        let router = Router::new();
        for _ in 0..10 {
            router.report_failure("p1-only", &Error::Timeout(1), &routing);
        }
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Reasoning), &[])
            .unwrap();
        assert_eq!(ids(&plan), vec!["p1-only"]);
        assert!(plan[0].degraded);
    }

    #[test]
    fn client_errors_do_not_open_the_breaker() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "a",
            Some(ModelTier::Reasoning),
        )]);
        let routing = cfg.routing.clone();
        let router = Router::new();
        for _ in 0..10 {
            router.report_failure("p1-a", &Error::invalid("bad json"), &routing);
        }
        assert!(!router.is_cooling("p1-a"));
        assert!(router.health_snapshot().is_empty());
    }

    #[test]
    fn success_resets_streak_and_tracks_latency() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "a",
            Some(ModelTier::Reasoning),
        )]);
        let routing = cfg.routing.clone();
        let router = Router::new();
        router.report_failure("p1-a", &Error::Timeout(1), &routing);
        router.report_success("p1-a", 200, &routing);
        let snap = router.health_snapshot();
        assert_eq!(snap[0].consecutive_failures, 0);
        assert_eq!(snap[0].total_success, 1);
        assert_eq!(snap[0].total_failure, 1);
        assert_eq!(snap[0].avg_latency_ms, 200.0);
        router.report_success("p1-a", 400, &routing);
        assert!((router.health_snapshot()[0].avg_latency_ms - 260.0).abs() < 0.001);
    }

    #[test]
    fn success_creates_health_with_routing_breaker_config() {
        let mut cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "a",
            Some(ModelTier::Reasoning),
        )]);
        cfg.routing.circuit_breaker.failure_threshold = 1;
        let routing = cfg.routing.clone();
        let router = Router::new();

        // First observation is a success, so the health entry must be created
        // with the routing config rather than the default breaker config.
        router.report_success("p1-a", 10, &routing);
        router.report_failure("p1-a", &Error::Timeout(1), &routing);
        assert!(
            router.is_cooling("p1-a"),
            "routing failure_threshold=1 should open after one failure"
        );
    }

    #[test]
    fn round_robin_rotates() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Fast)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Fast)),
        ]);
        cfg.routing.strategy = RoutingStrategy::RoundRobin;
        let r = reg(cfg);
        let router = Router::new();
        let first = ids(&router
            .plan(&r, &Resolution::Tier(ModelTier::Fast), &[])
            .unwrap())[0]
            .to_string();
        let second = ids(&router
            .plan(&r, &Resolution::Tier(ModelTier::Fast), &[])
            .unwrap())[0]
            .to_string();
        assert_ne!(first, second);
    }

    #[test]
    fn lowest_latency_prefers_the_fast_model() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "slow", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "fast", Some(ModelTier::Standard)),
        ]);
        cfg.routing.strategy = RoutingStrategy::LowestLatency;
        let routing = cfg.routing.clone();
        let r = reg(cfg);
        let router = Router::new();
        router.report_success("p1-slow", 3000, &routing);
        router.report_success("p2-fast", 300, &routing);
        assert_eq!(
            ids(&router
                .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
                .unwrap())[0],
            "p2-fast"
        );
    }

    #[test]
    fn weighted_random_covers_all_and_favours_weight() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "heavy", Some(ModelTier::Reasoning)),
            ModelEntry::for_upstream("p2", "light", Some(ModelTier::Reasoning)),
        ]);
        cfg.models[0].weight = 9;
        cfg.models[1].weight = 1;
        cfg.routing.strategy = RoutingStrategy::WeightedRandom;
        let r = reg(cfg);
        let router = Router::new();
        let mut heavy_first = 0;
        for _ in 0..400 {
            let plan = router
                .plan(&r, &Resolution::Tier(ModelTier::Reasoning), &[])
                .unwrap();
            assert_eq!(
                plan.len(),
                2,
                "every member must remain in the failover chain"
            );
            if plan[0].exposed_id == "p1-heavy" {
                heavy_first += 1;
            }
        }
        assert!(heavy_first > 280, "heavy won only {heavy_first}/400");
    }

    // ------------------------------------------------------- classifier pool

    fn classifier_cfg_with(models: Vec<ModelEntry>, candidates: &[(&str, i32)]) -> AppConfig {
        let mut cfg = cfg_with(models);
        cfg.classifier = ClassifierConfig {
            enabled: true,
            candidates: candidates
                .iter()
                .map(|(model, priority)| crate::config::ClassifierCandidate {
                    model: (*model).to_string(),
                    priority: *priority,
                    enabled: true,
                })
                .collect(),
            ..ClassifierConfig::default()
        };
        cfg
    }

    #[test]
    fn classifier_pool_follows_candidate_priority_not_tier_priority() {
        // glm is the *last* member of its tier but the *first* classifier
        // candidate; the two orderings are independent.
        let r = reg(classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "glm", Some(ModelTier::Fast)).with_priority(50),
                ModelEntry::for_upstream("p2", "deepseek", Some(ModelTier::Standard))
                    .with_priority(0),
            ],
            &[("p2-deepseek", 20), ("p1-glm", 10)],
        ));
        let plan = Router::new().plan_classifier(&r, &r.config().classifier).unwrap();
        assert_eq!(ids(&plan), vec!["p1-glm", "p2-deepseek"]);
    }

    #[test]
    fn classifier_candidates_may_point_at_unclassified_models() {
        // A model with no tier is still a perfectly good classifier.
        let r = reg(classifier_cfg_with(
            vec![ModelEntry::for_upstream("p1", "glm", None)],
            &[("p1-glm", 10)],
        ));
        let plan = Router::new().plan_classifier(&r, &r.config().classifier).unwrap();
        assert_eq!(ids(&plan), vec!["p1-glm"]);
    }

    #[test]
    fn classifier_pool_respects_its_own_attempt_budget() {
        let mut cfg = classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "a", None),
                ModelEntry::for_upstream("p2", "b", None),
                ModelEntry::for_upstream("p1", "c", None),
            ],
            &[("p1-a", 10), ("p2-b", 20), ("p1-c", 30)],
        );
        cfg.classifier.max_attempts = 2;
        let r = reg(cfg);
        let plan = Router::new().plan_classifier(&r, &r.config().classifier).unwrap();
        assert_eq!(ids(&plan), vec!["p1-a", "p2-b"]);
    }

    #[test]
    fn classifier_failover_off_yields_one_attempt() {
        let mut cfg = classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "a", None),
                ModelEntry::for_upstream("p2", "b", None),
            ],
            &[("p1-a", 10), ("p2-b", 20)],
        );
        cfg.classifier.failover = false;
        let r = reg(cfg);
        let plan = Router::new().plan_classifier(&r, &r.config().classifier).unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn unknown_or_disabled_candidates_are_skipped_not_fatal() {
        // Two live candidates and one dangling reference: the pool still works.
        let mut cfg = classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "a", None),
                ModelEntry::for_upstream("p2", "b", None),
            ],
            &[("p1-a", 10), ("p3-missing", 5), ("p2-b", 20)],
        );
        // A disabled candidate entry is skipped too.
        cfg.classifier.candidates[2].enabled = false;
        let r = reg(cfg);
        let plan = Router::new().plan_classifier(&r, &r.config().classifier).unwrap();
        assert_eq!(ids(&plan), vec!["p1-a"]);
    }

    #[test]
    fn an_empty_classifier_pool_is_an_error() {
        // No candidates at all, or only ones that resolve to nothing: the
        // request must fail rather than silently fall through to the main pool.
        let cfg = classifier_cfg_with(vec![ModelEntry::for_upstream("p1", "a", None)], &[]);
        let r = reg(cfg);
        let err = Router::new()
            .plan_classifier(&r, &r.config().classifier)
            .unwrap_err();
        assert!(matches!(err, Error::NoCandidate(_)));

        let cfg = classifier_cfg_with(vec![], &[("p1-gone", 10)]);
        let r = reg(cfg);
        assert!(Router::new()
            .plan_classifier(&r, &r.config().classifier)
            .is_err());
    }

    #[test]
    fn classifier_health_is_shared_with_the_main_pool() {
        // A model that fails as a classifier must be demoted for main requests
        // too: the circuit breaker is keyed by model, not by pool.
        let cfg = classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "glm", Some(ModelTier::Standard)).with_priority(0),
                ModelEntry::for_upstream("p2", "main", Some(ModelTier::Standard)).with_priority(5),
            ],
            &[("p1-glm", 10)],
        );
        let routing = cfg.routing.clone();
        let r = reg(cfg);
        let router = Router::new();

        for _ in 0..routing.circuit_breaker.failure_threshold {
            router.report_failure("p1-glm", &Error::Timeout(1), &routing);
        }
        // The classifier pool has nothing left: glm is cooling and it was the
        // only candidate.
        assert!(router
            .plan_classifier(&r, &r.config().classifier)
            .is_err());

        // ...and the main tier plan demotes glm for main requests as well.
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap();
        assert_eq!(ids(&plan)[0], "p2-main");
    }

    #[test]
    fn classifier_balanced_strategy_falls_back_to_priority() {
        // Elections are per tier; the classifier pool has none, so Balanced
        // must not silently produce an unsorted plan.
        let mut cfg = classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "a", None).with_priority(30),
                ModelEntry::for_upstream("p2", "b", None).with_priority(10),
            ],
            &[("p1-a", 30), ("p2-b", 10)],
        );
        cfg.classifier.strategy = RoutingStrategy::Balanced;
        let r = reg(cfg);
        let plan = Router::new().plan_classifier(&r, &r.config().classifier).unwrap();
        assert_eq!(ids(&plan), vec!["p2-b", "p1-a"]);
    }

    #[test]
    fn classifier_round_robin_rotates_independently_of_tiers() {
        let mut cfg = classifier_cfg_with(
            vec![
                ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
                ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
            ],
            &[("p1-a", 10), ("p2-b", 10)],
        );
        cfg.classifier.strategy = RoutingStrategy::RoundRobin;
        let r = reg(cfg);
        let router = Router::new();
        let first = ids(&router.plan_classifier(&r, &r.config().classifier).unwrap())[0]
            .to_string();
        let second = ids(&router.plan_classifier(&r, &r.config().classifier).unwrap())[0]
            .to_string();
        assert_ne!(first, second);
    }
}
