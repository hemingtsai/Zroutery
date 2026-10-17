//! Ordering the pool once it is known: strategy, election, and the round-robin cursor.

use super::candidate::Candidate;
use super::helpers::by_priority;
use super::helpers::satisfies_capabilities;
use super::Router;
use crate::circuit_breaker::CircuitState;
use crate::config::{ModelEntry, ModelTier, RoutingStrategy};
use crate::election::Election;
use crate::error::{Error, Result};
use crate::ir::Capability;
#[cfg(feature = "ml")]
use crate::observation::ObservationStore;
use crate::registry::Registry;
use rand::Rng;

impl Router {
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
    pub(super) fn plan_candidates(
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
        _capability_filter: bool,
        _strict: bool,
    ) -> Result<Vec<Candidate>> {
        // Request-derived capabilities are a hard gate. The old configuration
        // switches are retained in this private signature for compatibility,
        // but they cannot turn an unknown or unsupported capability into a
        // pass and cannot reinsert the unfiltered pool.
        let members = if !required_capabilities.is_empty() {
            let supported: Vec<&ModelEntry> = members
                .iter()
                .copied()
                .filter(|member| satisfies_capabilities(member, required_capabilities))
                .collect();
            let filtered = if supported.is_empty() {
                let remediated: Vec<&ModelEntry> = members
                    .iter()
                    .copied()
                    .filter(|member| {
                        self.request_capability_remediation_allowed(
                            registry,
                            member,
                            required_capabilities,
                        )
                    })
                    .collect();
                if remediated.is_empty() {
                    tracing::warn!(
                        required = ?required_capabilities,
                        pool = %pool_name,
                        "no candidate satisfies hard request capabilities"
                    );
                    return Err(Error::NoCandidate(pool_name));
                }
                remediated
            } else {
                supported
            };
            filtered
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
            let degraded = degraded_ids.contains(&entry.exposed_id())
                || self.request_capability_remediation_allowed(
                    registry,
                    entry,
                    required_capabilities,
                );
            out.push(Candidate::new(entry, provider, degraded));
        }
        if out.is_empty() {
            return Err(Error::NoCandidate(pool_name));
        }
        Ok(out)
    }

    pub(super) fn order<'a>(
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
    pub(super) fn elected_order<'a>(
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

    /// Runtime observations, keyed by provider+model. Read-only access for the
    /// shadow evaluation snapshot, which needs the same signals the policy
    /// scorer saw without consulting the router's ranking paths.
    #[cfg(feature = "ml")]
    pub fn observations(&self) -> &ObservationStore {
        &self.observations
    }

    /// Order a group randomly, with probability proportional to weight.
    pub(super) fn weighted_shuffle<'a>(&self, members: &[&'a ModelEntry]) -> Vec<&'a ModelEntry> {
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
}
