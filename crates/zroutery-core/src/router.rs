//! Candidate selection, health tracking and failover ordering.
//!
//! The router is long lived: it keeps health state across configuration
//! reloads, keyed by exposed model id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::config::{ModelClass, ModelEntry, ProviderConfig, RoutingConfig, RoutingStrategy};
use crate::election::Election;
use crate::error::{Error, Result};
use crate::registry::{Registry, Resolution};

/// One attempt: which model, on which provider.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Derived once here so health keys, log records and response headers all
    /// agree on the name.
    pub exposed_id: String,
    pub entry: ModelEntry,
    pub provider: ProviderConfig,
    /// True when this candidate is only being tried because everything else is
    /// in cooldown.
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelHealth {
    pub model_id: String,
    pub consecutive_failures: u32,
    pub total_success: u64,
    pub total_failure: u64,
    /// Exponentially weighted average latency of successful calls.
    pub avg_latency_ms: f64,
    /// Seconds left in cooldown, 0 when healthy.
    pub cooldown_remaining_secs: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
struct HealthState {
    consecutive_failures: u32,
    total_success: u64,
    total_failure: u64,
    avg_latency_ms: f64,
    cooldown_until_ms: u64,
    last_error: Option<String>,
}

/// Monotonic millisecond clock that tests can fast forward.
#[derive(Debug)]
struct Clock {
    base: Instant,
    offset_ms: AtomicU64,
}

impl Clock {
    fn new() -> Self {
        Clock {
            base: Instant::now(),
            offset_ms: AtomicU64::new(0),
        }
    }
    fn now_ms(&self) -> u64 {
        self.base.elapsed().as_millis() as u64 + self.offset_ms.load(Ordering::Relaxed)
    }
    fn advance(&self, ms: u64) {
        self.offset_ms.fetch_add(ms, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct Router {
    health: Mutex<HashMap<String, HealthState>>,
    rr: Mutex<HashMap<ModelClass, usize>>,
    /// The last election, when one has been held. `Balanced` follows the order it
    /// decided instead of re-deciding per request, which is the whole point: a
    /// route that changes under load cannot be reasoned about.
    election: Mutex<Option<Election>>,
    clock: Clock,
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
            clock: Clock::new(),
        }
    }

    /// Build the ordered list of attempts for a resolved model id.
    pub fn plan(&self, registry: &Registry, resolution: &Resolution) -> Result<Vec<Candidate>> {
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
            Resolution::Class(class) => self.plan_class(registry, *class, routing),
        }
    }

    fn plan_class(
        &self,
        registry: &Registry,
        class: ModelClass,
        routing: &RoutingConfig,
    ) -> Result<Vec<Candidate>> {
        let members = registry.class_members(class);
        if members.is_empty() {
            return Err(Error::NoCandidate(class.virtual_id().to_string()));
        }

        let now = self.clock.now_ms();
        let (healthy, cooling): (Vec<&ModelEntry>, Vec<&ModelEntry>) = {
            let health = crate::sync::lock(&self.health);
            members.into_iter().partition(|m| {
                health
                    .get(&m.exposed_id())
                    .map(|h| h.cooldown_until_ms <= now)
                    .unwrap_or(true)
            })
        };

        let mut ordered = self.order(&healthy, class, routing.strategy);
        // Everything is cooling down: still try, newest cooldown last.
        let mut degraded_ids: Vec<String> = Vec::new();
        if ordered.is_empty() {
            ordered = cooling.clone();
            degraded_ids = ordered.iter().map(|m| m.exposed_id()).collect();
        } else if routing.failover {
            for m in &cooling {
                degraded_ids.push(m.exposed_id());
                ordered.push(m);
            }
        }

        let limit = if routing.failover {
            routing.max_attempts.max(1) as usize
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
            return Err(Error::NoCandidate(class.virtual_id().to_string()));
        }
        Ok(out)
    }

    fn order<'a>(
        &self,
        members: &[&'a ModelEntry],
        class: ModelClass,
        strategy: RoutingStrategy,
    ) -> Vec<&'a ModelEntry> {
        if members.len() <= 1 {
            return members.to_vec();
        }
        match strategy {
            RoutingStrategy::Balanced => self.elected_order(members, class),
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
                let counter = rr.entry(class).or_insert(0);
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
        class: ModelClass,
    ) -> Vec<&'a ModelEntry> {
        let by_priority = |list: &mut Vec<&'a ModelEntry>| {
            list.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| a.exposed_id().cmp(&b.exposed_id()))
            })
        };

        let guard = crate::sync::lock(&self.election);
        let Some(order) = guard.as_ref().and_then(|e| e.order_for(class)) else {
            let mut fallback = members.to_vec();
            by_priority(&mut fallback);
            return fallback;
        };

        let mut elected: Vec<&ModelEntry> = Vec::with_capacity(members.len());
        for id in &order {
            if let Some(entry) = members.iter().find(|m| m.exposed_id() == *id) {
                elected.push(entry);
            }
        }
        let mut unseen: Vec<&ModelEntry> = members
            .iter()
            .filter(|m| !order.iter().any(|id| *id == m.exposed_id()))
            .copied()
            .collect();
        by_priority(&mut unseen);
        elected.extend(unseen);
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

    pub fn report_success(&self, model_id: &str, latency_ms: u64) {
        let mut health = crate::sync::lock(&self.health);
        let h = health.entry(model_id.to_string()).or_default();
        h.consecutive_failures = 0;
        h.cooldown_until_ms = 0;
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
        let now = self.clock.now_ms();
        let h = health.entry(model_id.to_string()).or_default();
        // A cooldown that has already expired was the model's second chance.
        // Judging a fresh failure against the stale streak would re-trip the
        // breaker on the first blip after recovery, so the streak restarts
        // from zero and the spent cooldown is cleared so this only happens once.
        if h.cooldown_until_ms != 0 && h.cooldown_until_ms <= now {
            h.consecutive_failures = 0;
            h.cooldown_until_ms = 0;
        }
        h.consecutive_failures += 1;
        h.total_failure += 1;
        h.last_error = Some(error.to_string());
        if h.consecutive_failures >= routing.break_after_failures.max(1) {
            h.cooldown_until_ms = now + routing.cooldown_secs * 1000;
        }
    }

    /// Clear cooldown and failure streak for one model (GUI "retry now").
    pub fn reset(&self, model_id: &str) {
        let mut health = crate::sync::lock(&self.health);
        if let Some(h) = health.get_mut(model_id) {
            h.consecutive_failures = 0;
            h.cooldown_until_ms = 0;
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
        let now = self.clock.now_ms();
        self.health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(model_id)
            .map(|h| h.cooldown_until_ms > now)
            .unwrap_or(false)
    }

    pub fn health_snapshot(&self) -> Vec<ModelHealth> {
        let now = self.clock.now_ms();
        let health = crate::sync::lock(&self.health);
        let mut out: Vec<ModelHealth> = health
            .iter()
            .map(|(id, h)| ModelHealth {
                model_id: id.clone(),
                consecutive_failures: h.consecutive_failures,
                total_success: h.total_success,
                total_failure: h.total_failure,
                avg_latency_ms: h.avg_latency_ms,
                cooldown_remaining_secs: h.cooldown_until_ms.saturating_sub(now) / 1000,
                last_error: h.last_error.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        out
    }

    /// Test/maintenance hook: fast forward the internal clock.
    #[doc(hidden)]
    pub fn advance_clock_ms(&self, ms: u64) {
        self.clock.advance(ms);
    }
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
            ModelEntry::for_upstream("p1", "a", Some(ModelClass::Sonnet)),
            ModelEntry::for_upstream("p2", "b", Some(ModelClass::Sonnet)),
        ]));
        let router = Router::new();
        let plan = router.plan(&r, &Resolution::Direct("p1-a".into())).unwrap();
        assert_eq!(ids(&plan), vec!["p1-a"]);
    }

    #[test]
    fn class_plan_follows_priority_and_respects_max_attempts() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "first", Some(ModelClass::Opus)).with_priority(0),
            ModelEntry::for_upstream("p2", "second", Some(ModelClass::Opus)).with_priority(10),
            ModelEntry::for_upstream("p1", "third", Some(ModelClass::Opus)).with_priority(20),
        ]);
        cfg.routing.max_attempts = 2;
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Class(ModelClass::Opus))
            .unwrap();
        assert_eq!(ids(&plan), vec!["p1-first", "p2-second"]);
    }

    #[test]
    fn failover_disabled_yields_single_attempt() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelClass::Haiku)),
            ModelEntry::for_upstream("p2", "b", Some(ModelClass::Haiku)),
        ]);
        cfg.routing.failover = false;
        let r = reg(cfg);
        let plan = Router::new()
            .plan(&r, &Resolution::Class(ModelClass::Haiku))
            .unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn empty_class_is_an_error() {
        let r = reg(cfg_with(vec![ModelEntry::for_upstream("p1", "a", None)]));
        let err = Router::new()
            .plan(&r, &Resolution::Class(ModelClass::Sonnet))
            .unwrap_err();
        assert!(matches!(err, Error::NoCandidate(_)));
    }

    #[test]
    fn circuit_breaker_demotes_then_recovers() {
        let cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "bad", Some(ModelClass::Sonnet)).with_priority(0),
            ModelEntry::for_upstream("p2", "good", Some(ModelClass::Sonnet)).with_priority(5),
        ]);
        let routing = cfg.routing.clone();
        let r = reg(cfg);
        let router = Router::new();

        assert_eq!(
            ids(&router
                .plan(&r, &Resolution::Class(ModelClass::Sonnet))
                .unwrap())[0],
            "p1-bad"
        );

        let err = Error::Timeout(5);
        for _ in 0..routing.break_after_failures {
            router.report_failure("p1-bad", &err, &routing);
        }
        assert!(router.is_cooling("p1-bad"));
        let plan = router
            .plan(&r, &Resolution::Class(ModelClass::Sonnet))
            .unwrap();
        assert_eq!(ids(&plan), vec!["p2-good", "p1-bad"]);
        assert!(!plan[0].degraded && plan[1].degraded);

        router.advance_clock_ms(routing.cooldown_secs * 1000 + 1);
        assert!(!router.is_cooling("p1-bad"));
        assert_eq!(
            ids(&router
                .plan(&r, &Resolution::Class(ModelClass::Sonnet))
                .unwrap())[0],
            "p1-bad"
        );
    }

    #[test]
    fn a_recovered_breaker_needs_fresh_evidence_to_trip_again() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "flaky",
            Some(ModelClass::Opus),
        )]);
        let routing = cfg.routing.clone();
        let router = Router::new();
        let err = Error::Timeout(1);

        for _ in 0..routing.break_after_failures {
            router.report_failure("p1-flaky", &err, &routing);
        }
        assert!(router.is_cooling("p1-flaky"));

        // Cooldown over, then one transient blip: that is not a pattern yet.
        router.advance_clock_ms(routing.cooldown_secs * 1000 + 1);
        router.report_failure("p1-flaky", &err, &routing);
        assert!(
            !router.is_cooling("p1-flaky"),
            "one failure after recovery should not re-open the breaker"
        );

        // Reaching the threshold again with new failures does.
        for _ in 1..routing.break_after_failures {
            router.report_failure("p1-flaky", &err, &routing);
        }
        assert!(router.is_cooling("p1-flaky"));
    }

    #[test]
    fn retain_models_drops_entries_that_no_longer_exist() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "kept",
            Some(ModelClass::Opus),
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
    fn all_cooling_still_produces_a_plan() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "only",
            Some(ModelClass::Opus),
        )]);
        let routing = cfg.routing.clone();
        let r = reg(cfg);
        let router = Router::new();
        for _ in 0..10 {
            router.report_failure("p1-only", &Error::Timeout(1), &routing);
        }
        let plan = router
            .plan(&r, &Resolution::Class(ModelClass::Opus))
            .unwrap();
        assert_eq!(ids(&plan), vec!["p1-only"]);
        assert!(plan[0].degraded);
    }

    #[test]
    fn client_errors_do_not_open_the_breaker() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "a",
            Some(ModelClass::Opus),
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
            Some(ModelClass::Opus),
        )]);
        let routing = cfg.routing.clone();
        let router = Router::new();
        router.report_failure("p1-a", &Error::Timeout(1), &routing);
        router.report_success("p1-a", 200);
        let snap = router.health_snapshot();
        assert_eq!(snap[0].consecutive_failures, 0);
        assert_eq!(snap[0].total_success, 1);
        assert_eq!(snap[0].total_failure, 1);
        assert_eq!(snap[0].avg_latency_ms, 200.0);
        router.report_success("p1-a", 400);
        assert!((router.health_snapshot()[0].avg_latency_ms - 260.0).abs() < 0.001);
    }

    #[test]
    fn round_robin_rotates() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelClass::Haiku)),
            ModelEntry::for_upstream("p2", "b", Some(ModelClass::Haiku)),
        ]);
        cfg.routing.strategy = RoutingStrategy::RoundRobin;
        let r = reg(cfg);
        let router = Router::new();
        let first = ids(&router
            .plan(&r, &Resolution::Class(ModelClass::Haiku))
            .unwrap())[0]
            .to_string();
        let second = ids(&router
            .plan(&r, &Resolution::Class(ModelClass::Haiku))
            .unwrap())[0]
            .to_string();
        assert_ne!(first, second);
    }

    #[test]
    fn lowest_latency_prefers_the_fast_model() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "slow", Some(ModelClass::Sonnet)),
            ModelEntry::for_upstream("p2", "fast", Some(ModelClass::Sonnet)),
        ]);
        cfg.routing.strategy = RoutingStrategy::LowestLatency;
        let r = reg(cfg);
        let router = Router::new();
        router.report_success("p1-slow", 3000);
        router.report_success("p2-fast", 300);
        assert_eq!(
            ids(&router
                .plan(&r, &Resolution::Class(ModelClass::Sonnet))
                .unwrap())[0],
            "p2-fast"
        );
    }

    #[test]
    fn weighted_random_covers_all_and_favours_weight() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "heavy", Some(ModelClass::Opus)),
            ModelEntry::for_upstream("p2", "light", Some(ModelClass::Opus)),
        ]);
        cfg.models[0].weight = 9;
        cfg.models[1].weight = 1;
        cfg.routing.strategy = RoutingStrategy::WeightedRandom;
        let r = reg(cfg);
        let router = Router::new();
        let mut heavy_first = 0;
        for _ in 0..400 {
            let plan = router
                .plan(&r, &Resolution::Class(ModelClass::Opus))
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
}
