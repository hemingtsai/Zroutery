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
use crate::observation::ObservationStore;
use crate::election::Election;
use crate::error::{Error, Result};
use crate::ir::Capability;
use crate::policy::{
    CandidateDecision, DecisionReason, PolicyFallback, PolicyPreference, PolicyRequirements,
    PolicyRevision, RouteDecision, ScoringContext, TaskProfile, TaskProfileSummary,
    score_candidate, hash_to_u64,
};
use crate::registry::{Registry, Resolution};
use crate::stats_ext::StatsStore;

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
    /// Runtime observations (latency, health, cost) keyed by provider+model.
    /// Used by the policy scorer as the primary signal, falling back to legacy
    /// circuit-breaker state when no observation exists.
    observations: ObservationStore,
    /// Runtime statistics (percentiles, EWMA, failure breakdown) keyed by
    /// provider+model. Accumulated over the lifetime of the process.
    pub stats_store: StatsStore,
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
            observations: ObservationStore::new(),
            stats_store: StatsStore::new(),
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

    /// Build the ordered list of attempts, applying policy eligibility filtering
    /// and preference-based scoring.
    ///
    /// Like [`plan`] but candidates are first filtered through
    /// [`PolicyRequirements::check`], then scored and sorted by
    /// [`PolicyPreference`] weights. When no candidate passes eligibility,
    /// the [`PolicyFallback`] determines what happens next:
    ///
    /// - [`PolicyFallback::Reject`] — return [`Error::NoCandidate`].
    /// - [`PolicyFallback::Escalate`] — try higher tiers up to `max_steps`.
    /// - [`PolicyFallback::Degrade`] — try lower tiers up to `max_steps`.
    /// - [`PolicyFallback::IgnoreRequirements`] — use all members without filtering.
    pub fn plan_with_policy(
        &self,
        registry: &Registry,
        resolution: &Resolution,
        required_capabilities: &[Capability],
        requirements: &PolicyRequirements,
        preference: &PolicyPreference,
        fallback: &PolicyFallback,
        task: Option<&TaskProfile>,
    ) -> Result<(Vec<Candidate>, RouteDecision)> {
        // Collect the raw candidate pool from the registry.
        let members: Vec<&ModelEntry> = match resolution {
            Resolution::Direct(id) => {
                let entry = registry.entry(id)?;
                let provider = registry.provider_of(entry)?;
                if !provider.enabled {
                    return Err(Error::UnknownModel(format!("{id} (provider disabled)")));
                }
                vec![entry]
            }
            Resolution::Tier(tier) => registry.tier_members(*tier),
        };
        if members.is_empty() {
            let name = match resolution {
                Resolution::Direct(id) => id.clone(),
                Resolution::Tier(tier) => tier.virtual_id().to_string(),
            };
            return Err(Error::NoCandidate(name));
        }

        // Apply policy eligibility to each candidate.
        let filtered: Vec<&ModelEntry> = self.filter_eligible(&members, requirements);

        // Track the actual tier used (may differ from resolution after fallback).
        let mut effective_tier: Option<ModelTier> = match resolution {
            Resolution::Tier(tier) => Some(*tier),
            Resolution::Direct(_) => None,
        };

        // Collect eligibility results for every candidate (cheap: just strings and bools).
        let mut decisions: Vec<CandidateDecision> = Vec::with_capacity(members.len());
        for m in &members {
            let circuit_open = self.is_circuit_open(&m.exposed_id());
            let check = requirements.check(
                &m.exposed_id(),
                &m.provider_id,
                m.tier,
                &m.capabilities,
                circuit_open,
            );
            decisions.push(CandidateDecision {
                model_id: m.exposed_id(),
                provider_id: m.provider_id.clone(),
                tier: m.tier.map(|t| t.as_str().to_string()),
                eligible: check.eligible,
                rejection: if check.eligible {
                    None
                } else {
                    Some(check.reasons.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", "))
                },
                score: None,
                final_score: None,
            });
        }

        // Track fallback chain (tier IDs tried before the final selection).
        let mut fallback_chain: Vec<String> = Vec::new();

        let effective = if filtered.is_empty() {
            // No candidate passed eligibility — apply fallback.
            match fallback {
                PolicyFallback::Reject => {
                    let name = match resolution {
                        Resolution::Direct(id) => id.clone(),
                        Resolution::Tier(tier) => tier.virtual_id().to_string(),
                    };
                    tracing::warn!(
                        pool = %name,
                        "no candidate satisfies policy requirements; fallback=reject"
                    );
                    return Err(Error::NoCandidate(name));
                }
                PolicyFallback::Escalate { enabled, max_steps } if *enabled => {
                    let (candidates, tier) = self.fallback_tier(
                        registry, requirements, resolution,
                        *max_steps, true, // escalate = higher tiers
                    )?;
                    effective_tier = Some(tier);
                    candidates
                }
                PolicyFallback::Escalate { .. } => {
                    // Escalate disabled — reject.
                    let name = match resolution {
                        Resolution::Direct(id) => id.clone(),
                        Resolution::Tier(tier) => tier.virtual_id().to_string(),
                    };
                    return Err(Error::NoCandidate(name));
                }
                PolicyFallback::Degrade { enabled, max_steps } if *enabled => {
                    let (candidates, tier) = self.fallback_tier(
                        registry, requirements, resolution,
                        *max_steps, false, // degrade = lower tiers
                    )?;
                    effective_tier = Some(tier);
                    candidates
                }
                PolicyFallback::Degrade { .. } => {
                    let name = match resolution {
                        Resolution::Direct(id) => id.clone(),
                        Resolution::Tier(tier) => tier.virtual_id().to_string(),
                    };
                    return Err(Error::NoCandidate(name));
                }
                PolicyFallback::IgnoreRequirements => {
                    tracing::warn!(
                        "no candidate satisfies policy requirements; ignoring requirements"
                    );
                    members
                }
            }
        } else {
            filtered
        };

        // Record fallback chain entries.
        if let Some(tier) = effective_tier {
            if let Resolution::Tier(orig) = resolution {
                if tier != *orig {
                    fallback_chain.push(orig.virtual_id().to_string());
                }
            }
        }

        // Score and sort by policy preferences before delegating to
        // plan_candidates for health gating, failover and attempt capping.
        let (scored, score_breakdowns) = self.score_and_sort(effective, preference, task);
        let effective: Vec<&ModelEntry> = scored;

        // Annotate eligible decisions with their score breakdowns.
        for (model_id, breakdown, total_score) in &score_breakdowns {
            if let Some(d) = decisions.iter_mut().find(|d| &d.model_id == model_id) {
                d.score = Some(breakdown.clone());
                d.final_score = Some(*total_score);
            }
        }

        // Delegate to the standard routing machinery for health, ordering and
        // failover — but pass the already-filtered (or fallback) member list
        // through the same plan_candidates pipeline so everything else (circuit
        // breakers, strategy, attempt cap) works identically.
        let routing = &registry.config().routing;
        // Use the effective tier (which may have changed due to fallback)
        // for pool_name and election_tier.
        let (pool_name, election_tier) = match effective_tier {
            Some(tier) => (tier.virtual_id().to_string(), Some(tier)),
            None => match resolution {
                Resolution::Direct(id) => (id.clone(), None),
                Resolution::Tier(tier) => (tier.virtual_id().to_string(), Some(*tier)),
            },
        };
        let candidates = self.plan_candidates(
            registry,
            effective,
            pool_name.as_str(),
            election_tier,
            routing.strategy,
            routing.failover,
            routing.max_attempts,
            pool_name.clone(),
            required_capabilities,
            false, // capability filtering already applied above
            false,
        )?;

        // Mark the selected candidate.
        if let Some(first) = candidates.first() {
            if let Some(d) = decisions.iter_mut().find(|d| d.model_id == first.exposed_id) {
                d.eligible = true;
                d.rejection = None;
            }
        }

        // Determine the reason.
        let reason = if let Resolution::Direct(_) = resolution {
            DecisionReason::Direct
        } else if !fallback_chain.is_empty() {
            match fallback {
                PolicyFallback::Escalate { .. } => DecisionReason::Escalated {
                    from_tier: fallback_chain[0].clone(),
                },
                PolicyFallback::Degrade { .. } => DecisionReason::Degraded {
                    from_tier: fallback_chain[0].clone(),
                },
                _ => DecisionReason::PolicySelected,
            }
        } else {
            DecisionReason::PolicySelected
        };

        let policy_revision = PolicyRevision {
            policy_id: String::new(), // filled by caller
            policy_enabled: true,     // filled by caller
            requirements_hash: hash_to_u64(requirements),
            preference_hash: hash_to_u64(preference),
        };

        let decision = RouteDecision {
            decision_id: format!("dec-{}", uuid::Uuid::new_v4().simple()),
            timestamp: chrono::Utc::now().timestamp(),
            task: task
                .map(|t| TaskProfileSummary::from(t))
                .unwrap_or_else(|| TaskProfileSummary {
                    complexity: String::new(),
                    task_type: String::new(),
                    context_tokens: 0,
                    estimated_output_tokens: 0,
                    streaming: false,
                    has_tools: false,
                    has_vision: false,
                    required_capabilities: Vec::new(),
                }),
            policy_id: String::new(), // filled by caller
            client_id: None,          // filled by caller
            candidates: decisions,
            selected: candidates.first().map(|c| c.exposed_id.clone()),
            fallback_chain,
            reason,
            policy_revision,
        };

        Ok((candidates, decision))
    }

    /// Filter members through policy requirements eligibility check.
    ///
    /// Uses a read-only health check (`is_circuit_open`) to avoid consuming
    /// half-open permits, which would be a side effect during eligibility
    /// filtering. The actual `allow_request()` is called later at the point
    /// of send.
    fn filter_eligible<'a>(
        &self,
        members: &[&'a ModelEntry],
        requirements: &PolicyRequirements,
    ) -> Vec<&'a ModelEntry> {
        members
            .iter()
            .copied()
            .filter(|m| {
                let circuit_open = self.is_circuit_open(&m.exposed_id());
                let check = requirements.check(
                    &m.exposed_id(),
                    &m.provider_id,
                    m.tier,
                    &m.capabilities,
                    circuit_open,
                );
                check.eligible
            })
            .collect()
    }

    /// Try escalating or degrading through tiers until eligible candidates are found.
    ///
    /// `up` = true means escalate (higher tiers), false means degrade (lower tiers).
    ///
    /// Returns the filtered candidates AND the tier they were found in.
    fn fallback_tier<'a>(
        &'a self,
        registry: &'a Registry,
        requirements: &PolicyRequirements,
        resolution: &Resolution,
        max_steps: u32,
        up: bool,
    ) -> Result<(Vec<&'a ModelEntry>, ModelTier)> {
        // Determine the starting tier.
        let start_tier = match resolution {
            Resolution::Tier(tier) => *tier,
            Resolution::Direct(_) => {
                // Direct resolution: cannot escalate/degrade across tiers.
                let name = match resolution {
                    Resolution::Direct(id) => id.clone(),
                    _ => unreachable!(),
                };
                return Err(Error::NoCandidate(name));
            }
        };

        let mut current = start_tier;
        for _ in 0..max_steps {
            let next = if up { current.higher() } else { current.lower() };
            match next {
                Some(tier) => current = tier,
                None => break, // No more tiers in this direction.
            }
            let members = registry.tier_members(current);
            if members.is_empty() {
                continue;
            }
            let filtered = self.filter_eligible(&members, requirements);
            if !filtered.is_empty() {
                tracing::info!(
                    from = %start_tier.virtual_id(),
                    to = %current.virtual_id(),
                    candidates = filtered.len(),
                    "fallback tier found eligible candidates"
                );
                return Ok((filtered, current));
            }
        }

        let name = match resolution {
            Resolution::Direct(id) => id.clone(),
            Resolution::Tier(tier) => tier.virtual_id().to_string(),
        };
        tracing::warn!(
            pool = %name,
            direction = if up { "escalate" } else { "degrade" },
            max_steps,
            "fallback exhausted; no eligible candidates found"
        );
        Err(Error::NoCandidate(name))
    }

    /// Score candidates by policy preferences and return them sorted (best first),
    /// along with the per-candidate score breakdown for decision tracing.
    ///
    /// When a runtime observation exists for a candidate (from the
    /// [`ObservationStore`]), its health score and latency are used instead of
    /// the legacy circuit-breaker EWMA. Candidates that have never been observed
    /// fall back to the legacy path.
    fn score_and_sort<'a>(
        &self,
        members: Vec<&'a ModelEntry>,
        preference: &PolicyPreference,
        task: Option<&TaskProfile>,
    ) -> (Vec<&'a ModelEntry>, Vec<(String, crate::policy::ScoreBreakdown, f64)>) {
        let mut scored: Vec<(&ModelEntry, f64, crate::policy::ScoreBreakdown)> = members
            .iter()
            .map(|m| {
                // Try runtime observation first; fall back to legacy health.
                let obs = self.observations.get(&m.exposed_id(), &m.provider_id);
                let health = if obs.health.state != crate::observation::HealthState::Unknown {
                    obs.health.score()
                } else {
                    self.health_score(&m.exposed_id())
                };
                let streaming = task.map_or(false, |t| t.streaming);
                let avg_latency_ms = if streaming {
                    // For streaming, TTFT is the primary signal; fall back to
                    // total latency when no TTFT observation exists.
                    obs.latency.ttft_ms.value
                        .or(obs.latency.total_ms.value)
                        .unwrap_or_else(|| self.avg_latency(&m.exposed_id()))
                } else {
                    obs.latency.total_ms
                        .value
                        .unwrap_or_else(|| self.avg_latency(&m.exposed_id()))
                };

                let ctx = ScoringContext {
                    health,
                    avg_latency_ms,
                    input_per_mtok: m.pricing.as_ref().map(|p| p.input_per_mtok),
                    output_per_mtok: m.pricing.as_ref().map(|p| p.output_per_mtok),
                    priority: m.priority,
                    tier: m.tier,
                    task,
                };
                let s = score_candidate(preference, &ctx);
                (*m, s.total_score, s.breakdown)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let breakdowns = scored
            .iter()
            .map(|(m, score, bd)| (m.exposed_id(), bd.clone(), *score))
            .collect();
        let sorted = scored.into_iter().map(|(m, _, _)| m).collect();
        (sorted, breakdowns)
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
            routing.strict_capability_filter,
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
        strict: bool,
    ) -> Result<Vec<Candidate>> {
        // Capability filtering: exclude models whose declared capabilities
        // don't satisfy the request's requirements.  When `strict` is false,
        // fall back to the unfiltered list so the request is not rejected just
        // because no model declares every capability.  When `strict` is true,
        // return an error so that requests with unsatisfiable capability
        // requirements fail fast instead of being routed to a model that
        // cannot handle them.
        let members = if capability_filter && !required_capabilities.is_empty() {
            let filtered: Vec<&ModelEntry> = members
                .iter()
                .copied()
                .filter(|m| satisfies_capabilities(m, required_capabilities))
                .collect();
            if filtered.is_empty() {
                if strict {
                    tracing::warn!(
                        required = ?required_capabilities,
                        pool = %pool_name,
                        "no candidate satisfies required capabilities; strict mode rejects"
                    );
                    return Err(Error::NoCandidate(pool_name));
                }
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
                // Priority sorts by priority number, preserving input order
                // within same-priority groups. This ensures that upstream
                // ordering (e.g. from policy scoring) is respected.
                let mut sorted: Vec<&ModelEntry> = members.to_vec();
                sorted.sort_by_key(|m| m.priority);
                sorted
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
            let impact = class.impact();
            if impact.affects_observation {
                self.observations.record_failure(model_id, provider_id);
            }
            self.stats_store
                .record_classified_failure(model_id, provider_id, class);
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

    /// Read-only check: is the circuit breaker in Open state?
    ///
    /// Unlike [`allow_request`], this does not consume half-open probe permits
    /// and is safe to call during eligibility filtering where side effects are
    /// not desired.
    pub fn is_circuit_open(&self, model_id: &str) -> bool {
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

    // ---------------------------------------- capability filter (strict/soft)

    #[test]
    fn soft_fallback_keeps_unfiltered_candidates_when_none_match() {
        // Both models lack vision; the request requires vision.
        // With strict=false (default), the soft fallback kicks in and both
        // are kept.
        let cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "text-only", Some(ModelTier::Standard))
                .with_priority(0),
            ModelEntry::for_upstream("p2", "also-text", Some(ModelTier::Standard))
                .with_priority(1),
        ]);
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[Capability::Vision])
            .unwrap();
        assert_eq!(ids(&plan), vec!["p1-text-only", "p2-also-text"]);
    }

    #[test]
    fn strict_mode_rejects_when_no_candidate_satisfies_capability() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "text-only", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "also-text", Some(ModelTier::Standard)),
        ]);
        cfg.routing.strict_capability_filter = true;
        let r = reg(cfg);
        let router = Router::new();
        let err = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[Capability::Vision])
            .unwrap_err();
        assert!(matches!(err, Error::NoCandidate(_)));
    }

    #[test]
    fn capability_filter_prefers_matching_candidates() {
        // p1 has vision, p2 does not. A request requiring vision should
        // prefer p1, regardless of strict mode.
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "vision", Some(ModelTier::Standard)).with_priority(10),
            ModelEntry::for_upstream("p2", "no-vision", Some(ModelTier::Standard)).with_priority(0),
        ]);
        cfg.models[0].capabilities.vision = true;
        cfg.routing.strict_capability_filter = true;
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[Capability::Vision])
            .unwrap();
        assert_eq!(ids(&plan), vec!["p1-vision"]);
    }

    #[test]
    fn strict_mode_accepts_when_at_least_one_candidate_matches() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "vision", Some(ModelTier::Standard)).with_priority(0),
            ModelEntry::for_upstream("p2", "no-vision", Some(ModelTier::Standard)).with_priority(5),
        ]);
        cfg.models[0].capabilities.vision = true;
        cfg.routing.strict_capability_filter = true;
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[Capability::Vision])
            .unwrap();
        // Only the vision-capable model should be returned.
        assert_eq!(ids(&plan), vec!["p1-vision"]);
    }

    #[test]
    fn strict_mode_ignores_when_no_capabilities_required() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
        ]);
        cfg.routing.strict_capability_filter = true;
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[])
            .unwrap();
        assert_eq!(plan.len(), 2);
    }

    #[test]
    fn capability_filter_disabled_ignores_requirements() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
        ]);
        // capability_filter defaults to true, but let's explicitly disable it.
        cfg.routing.capability_filter = false;
        let r = reg(cfg);
        let router = Router::new();
        let plan = router
            .plan(&r, &Resolution::Tier(ModelTier::Standard), &[Capability::Vision])
            .unwrap();
        // Both candidates survive: the filter is off.
        assert_eq!(plan.len(), 2);
    }

    // ------------------------------------------------ observation-aware scoring

    #[test]
    fn observation_fast_latency_scores_higher() {
        let router = Router::new();
        // model-a: fast (100ms), model-b: slow (3000ms)
        router.record_outcome("p1-a", "p1", 100.0, Some(50.0));
        router.record_outcome("p2-b", "p2", 3000.0, Some(800.0));

        let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
        let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
        let members = vec![&m1, &m2];
        let pref = PolicyPreference {
            latency_weight: 1.0,
            health_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };

        let (sorted, _) = router.score_and_sort(members, &pref, None);
        assert_eq!(sorted[0].exposed_id(), "p1-a", "fast model should rank first");
    }

    #[test]
    fn observation_healthy_beats_degraded() {
        let router = Router::new();
        // model-a: healthy (1 success)
        router.record_outcome("p1-a", "p1", 200.0, None);
        // model-b: 2 consecutive failures = Degraded
        router.record_failure("p2-b", "p2");
        router.record_failure("p2-b", "p2");

        let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
        let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
        let members = vec![&m1, &m2];
        let pref = PolicyPreference {
            health_weight: 1.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };

        let (sorted, breakdowns) = router.score_and_sort(members, &pref, None);
        assert_eq!(sorted[0].exposed_id(), "p1-a", "healthy should rank above degraded");

        // Verify the health scores are actually different.
        let bd_a = breakdowns.iter().find(|(id, _, _)| id == "p1-a").unwrap();
        let bd_b = breakdowns.iter().find(|(id, _, _)| id == "p2-b").unwrap();
        assert!(bd_a.1.health > bd_b.1.health, "healthy ({}) > degraded ({})", bd_a.1.health, bd_b.1.health);
    }

    #[test]
    fn observation_unknown_falls_back_to_legacy() {
        let cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard)),
        ]);
        let routing = cfg.routing.clone();
        let router = Router::new();

        // Legacy health: model-a has some history, model-b has none.
        router.report_success("p1-a", 200, &routing);
        // No observations recorded — scoring should use legacy health_score.

        let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
        let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
        let members = vec![&m1, &m2];
        let pref = PolicyPreference {
            health_weight: 1.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };

        let (sorted, breakdowns) = router.score_and_sort(members, &pref, None);
        // Both have unknown observations: p1-a has legacy success (ratio=1.0),
        // p2-b has no legacy data (optimistic=1.0). Scores should be equal.
        assert_eq!(sorted.len(), 2);
        let bd_a = breakdowns.iter().find(|(id, _, _)| id == "p1-a").unwrap();
        let bd_b = breakdowns.iter().find(|(id, _, _)| id == "p2-b").unwrap();
        assert!(
            (bd_a.1.health - bd_b.1.health).abs() < 0.001,
            "both should use legacy health; got {} vs {}",
            bd_a.1.health, bd_b.1.health,
        );
    }

    #[test]
    fn observation_overrides_legacy_circuit_open() {
        let cfg = cfg_with(vec![ModelEntry::for_upstream(
            "p1",
            "a",
            Some(ModelTier::Standard),
        )]);
        let routing = cfg.routing.clone();
        let router = Router::new();

        // Trip the circuit breaker via legacy health.
        for _ in 0..routing.circuit_breaker.failure_threshold {
            router.report_failure("p1-a", &Error::Timeout(1), &routing);
        }
        assert_eq!(router.health_score("p1-a"), 0.0, "legacy: circuit open");

        // Record a fresh observation success.
        router.record_outcome("p1-a", "p1", 100.0, Some(50.0));

        let m = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
        let members = vec![&m];
        let pref = PolicyPreference {
            health_weight: 1.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };

        let (_, breakdowns) = router.score_and_sort(members, &pref, None);
        // Observation health is Healthy with success_rate=1.0, so score=1.0.
        // Legacy would give 0.0 (circuit open). Observation should win.
        assert!(
            breakdowns[0].1.health > 0.5,
            "observation health should override legacy; got {}",
            breakdowns[0].1.health,
        );
    }

    #[test]
    fn observation_recording_flows_through_to_plan_with_policy() {
        let mut cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "fast", Some(ModelTier::Standard)).with_priority(0),
            ModelEntry::for_upstream("p2", "slow", Some(ModelTier::Standard)).with_priority(0),
        ]);
        cfg.routing.strategy = crate::config::RoutingStrategy::Priority;
        let r = reg(cfg);
        let router = Router::new();

        // Record observations: p1-fast is fast, p2-slow is slow.
        router.record_outcome("p1-fast", "p1", 100.0, Some(50.0));
        router.record_outcome("p2-slow", "p2", 3000.0, Some(800.0));

        let pref = PolicyPreference {
            latency_weight: 1.0,
            health_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let requirements = PolicyRequirements::default();

        let (candidates, _decision) = router
            .plan_with_policy(
                &r,
                &Resolution::Tier(ModelTier::Standard),
                &[],
                &requirements,
                &pref,
                &PolicyFallback::default(),
                None,
            )
            .unwrap();

        assert_eq!(
            candidates[0].model_id(),
            "p1-fast",
            "plan_with_policy should prefer the faster model via observations"
        );
    }

    // --------------------------------------------------- 4B feedback loop tests

    #[test]
    fn observation_adapts_across_requests() {
        // Setup: two models, same tier
        let cfg = cfg_with(vec![
            ModelEntry::for_upstream("p1", "model-a", Some(ModelTier::Standard)),
            ModelEntry::for_upstream("p2", "model-b", Some(ModelTier::Standard)),
        ]);
        let _r = reg(cfg);
        let router = Router::new();

        // Request 1: model-a is fast (100ms), model-b is slow (2000ms)
        router.record_outcome("p1-model-a", "p1", 100.0, Some(50.0));
        router.record_outcome("p2-model-b", "p2", 2000.0, Some(800.0));

        let pref = PolicyPreference {
            latency_weight: 1.0,
            health_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let ma = ModelEntry::for_upstream("p1", "model-a", Some(ModelTier::Standard));
        let mb = ModelEntry::for_upstream("p2", "model-b", Some(ModelTier::Standard));
        let (sorted1, _) = router.score_and_sort(vec![&ma, &mb], &pref, None);
        assert_eq!(
            sorted1[0].exposed_id(),
            "p1-model-a",
            "first request: fast model wins"
        );

        // Now model-a fails twice, model-b succeeds
        router.record_failure("p1-model-a", "p1");
        router.record_failure("p1-model-a", "p1");
        router.record_outcome("p2-model-b", "p2", 200.0, Some(80.0));

        // Request 2: model-b should now rank higher (healthier)
        let pref_health = PolicyPreference {
            health_weight: 1.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let ma = ModelEntry::for_upstream("p1", "model-a", Some(ModelTier::Standard));
        let mb = ModelEntry::for_upstream("p2", "model-b", Some(ModelTier::Standard));
        let (sorted2, _) = router.score_and_sort(vec![&ma, &mb], &pref_health, None);
        assert_eq!(
            sorted2[0].exposed_id(),
            "p2-model-b",
            "after failures: healthy model wins"
        );
    }

    #[test]
    fn scoring_isolates_same_model_different_providers() {
        let router = Router::new();
        // Same model "shared-model" from two providers
        router.record_outcome("p1-shared-model", "p1", 100.0, Some(50.0));
        router.record_outcome("p2-shared-model", "p2", 3000.0, Some(800.0));

        let m1 = ModelEntry::for_upstream("p1", "shared-model", Some(ModelTier::Standard));
        let m2 = ModelEntry::for_upstream("p2", "shared-model", Some(ModelTier::Standard));
        let members = vec![&m1, &m2];
        let pref = PolicyPreference {
            latency_weight: 1.0,
            health_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let (sorted, _) = router.score_and_sort(members, &pref, None);
        assert_eq!(
            sorted[0].provider_id, "p1",
            "faster provider wins for same model"
        );
    }

    #[test]
    fn streaming_scoring_prefers_ttft() {
        let router = Router::new();
        // Model A: fast TTFT (100ms), slow total (5000ms)
        router.record_outcome("p1-a", "p1", 5000.0, Some(100.0));
        // Model B: slow TTFT (800ms), fast total (1000ms)
        router.record_outcome("p2-b", "p2", 1000.0, Some(800.0));

        // For streaming (TTFT matters), A should win
        let task_streaming = crate::policy::TaskProfile {
            streaming: true,
            ..Default::default()
        };
        let pref = PolicyPreference {
            latency_weight: 1.0,
            health_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
        let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
        let (sorted, _) =
            router.score_and_sort(vec![&m1, &m2], &pref, Some(&task_streaming));
        assert_eq!(sorted[0].exposed_id(), "p1-a", "streaming prefers fast TTFT");

        // For buffered (total matters), B should win
        let task_buffered = crate::policy::TaskProfile {
            streaming: false,
            ..Default::default()
        };
        let (sorted, _) =
            router.score_and_sort(vec![&m1, &m2], &pref, Some(&task_buffered));
        assert_eq!(
            sorted[0].exposed_id(),
            "p2-b",
            "buffered prefers fast total"
        );
    }

    #[test]
    fn legacy_scoring_unchanged_without_observations() {
        // When no observations exist, scoring should behave identically to pre-4B
        let router = Router::new();
        let m1 = ModelEntry::for_upstream("p1", "a", Some(ModelTier::Standard));
        let m2 = ModelEntry::for_upstream("p2", "b", Some(ModelTier::Standard));
        let members = vec![&m1, &m2];
        let pref = PolicyPreference::default();
        let (sorted, breakdowns) = router.score_and_sort(members, &pref, None);
        // Both have same config -> scores should be equal
        assert!(
            (breakdowns[0].2 - breakdowns[1].2).abs() < 0.01,
            "identical models without observations should have equal scores"
        );
        // Order preserved (stable sort)
        assert_eq!(sorted[0].exposed_id(), "p1-a");
    }
}
