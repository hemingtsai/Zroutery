//! Turning a resolved model into an ordered list of attempts.

use std::collections::HashMap;
use std::sync::Mutex;

use super::candidate::Candidate;
use super::helpers::resolution_name;
use super::Router;
use crate::config::{ModelEntry, ModelTier};
use crate::error::{Error, Result};
use crate::ir::Capability;
use crate::observation::ObservationStore;
use crate::policy::{
    canonical_capabilities, capability_label, hash_to_u64, DecisionReason, PolicyFallback,
    PolicyPreference, PolicyRequirements, PolicyRevision, RouteDecision, TaskProfile,
    TaskProfileSummary,
};
use crate::registry::{Registry, Resolution};
use crate::stats_ext::StatsStore;

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
    ///
    /// Request-derived capabilities are hard inputs on both direct and tier
    /// resolutions. The legacy capability-filter configuration cannot disable
    /// this gate.
    pub fn plan(
        &self,
        registry: &Registry,
        resolution: &Resolution,
        required_capabilities: &[Capability],
    ) -> Result<Vec<Candidate>> {
        let routing = &registry.config().routing;
        self.apply_routing_config(routing);
        match resolution {
            Resolution::Direct(id) => {
                let entry = registry.entry(id)?;
                let provider = registry.provider_of(entry)?;
                if !provider.enabled {
                    return Err(Error::UnknownModel(format!("{id} (provider disabled)")));
                }
                self.plan_candidates(
                    registry,
                    vec![entry],
                    id,
                    None,
                    routing.strategy,
                    false,
                    1,
                    id.clone(),
                    required_capabilities,
                    // Direct resolution has no policy fallback and must use the
                    // same hard capability gate as a tier.
                    true,
                    true,
                )
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

    /// Build a plan and retain the canonical eligibility/planned-identity
    /// evidence for either a direct model id or a virtual tier.
    pub fn plan_with_trace(
        &self,
        registry: &Registry,
        resolution: &Resolution,
        required_capabilities: &[Capability],
    ) -> Result<(Vec<Candidate>, RouteDecision)> {
        self.plan_with_policy(
            registry,
            resolution,
            required_capabilities,
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
    }

    /// Alias with an explicit eligibility-trace name for callers that want to
    /// make the lifecycle boundary visible at the call site.
    pub fn plan_with_eligibility_trace(
        &self,
        registry: &Registry,
        resolution: &Resolution,
        required_capabilities: &[Capability],
    ) -> Result<(Vec<Candidate>, RouteDecision)> {
        self.plan_with_trace(registry, resolution, required_capabilities)
    }

    /// Build the ordered list of attempts, applying policy eligibility filtering
    /// and preference-based scoring.
    ///
    /// Request-derived capabilities are checked in addition to policy
    /// requirements on every path. A policy fallback may change the tier or
    /// relax policy-only constraints, but it may not relax a request capability
    /// or reintroduce a candidate that the hard gate rejected.
    ///
    /// [`PolicyFallback::IgnoreRequirements`] is the narrowest such relaxation:
    /// it retries the pool with only the capability contract in force, dropping
    /// tier bounds and the provider/model allow-deny lists while keeping policy
    /// `required_capabilities`, the request-derived capability vector, and an
    /// open circuit as hard rejections. If nothing satisfies that contract the
    /// call still fails with `Error::NoCandidate` rather than planning an
    /// incapable candidate.
    #[allow(clippy::too_many_arguments)]
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
        // Eligibility reads breaker state (including the cooldown), so the
        // current routing config is applied before any of it is consulted.
        self.apply_routing_config(&registry.config().routing);
        let request_capabilities = canonical_capabilities(required_capabilities);
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
        // A tier with no usable members is not a special case: an empty pool is
        // exactly a pool that filtered down to nothing, so it must take the same
        // configured Escalate/Degrade route. Returning early here made "no model
        // configured for this tier" fail while "every member was filtered out"
        // fell back, which is inconsistent for the same client policy.

        let mut effective_tier: Option<ModelTier> = match resolution {
            Resolution::Tier(tier) => Some(*tier),
            Resolution::Direct(_) => None,
        };
        let mut decisions = Vec::with_capacity(members.len());
        for member in &members {
            self.push_candidate_decision(
                &mut decisions,
                member,
                requirements,
                &request_capabilities,
            );
        }

        let filtered =
            self.filter_eligible(registry, &members, requirements, &request_capabilities);
        let mut fallback_chain: Vec<String> = Vec::new();
        let effective = if filtered.is_empty() {
            match fallback {
                PolicyFallback::Reject => {
                    let name = resolution_name(resolution);
                    tracing::warn!(
                        pool = %name,
                        "no candidate satisfies hard eligibility; fallback=reject"
                    );
                    return Err(Error::NoCandidate(name));
                }
                PolicyFallback::Escalate { enabled, max_steps } if *enabled => {
                    let (candidates, tier, seen) = self.fallback_tier(
                        registry,
                        requirements,
                        resolution,
                        &request_capabilities,
                        *max_steps,
                        true,
                    )?;
                    effective_tier = Some(tier);
                    self.push_seen_decisions(
                        &mut decisions,
                        &seen,
                        requirements,
                        &request_capabilities,
                    );
                    candidates
                }
                PolicyFallback::Escalate { .. } => {
                    return Err(Error::NoCandidate(resolution_name(resolution)));
                }
                PolicyFallback::Degrade { enabled, max_steps } if *enabled => {
                    let (candidates, tier, seen) = self.fallback_tier(
                        registry,
                        requirements,
                        resolution,
                        &request_capabilities,
                        *max_steps,
                        false,
                    )?;
                    effective_tier = Some(tier);
                    self.push_seen_decisions(
                        &mut decisions,
                        &seen,
                        requirements,
                        &request_capabilities,
                    );
                    candidates
                }
                PolicyFallback::Degrade { .. } => {
                    return Err(Error::NoCandidate(resolution_name(resolution)));
                }
                PolicyFallback::IgnoreRequirements => {
                    // This explicit policy action may bypass non-capability
                    // policy constraints, but capability requirements and the
                    // request contract remain hard. It is not a capability
                    // soft pass.
                    let capability_only = PolicyRequirements {
                        required_capabilities: requirements.required_capabilities.clone(),
                        strict_capabilities: true,
                        ..Default::default()
                    };
                    let candidates = self.filter_eligible(
                        registry,
                        &members,
                        &capability_only,
                        &request_capabilities,
                    );
                    if candidates.is_empty() {
                        return Err(Error::NoCandidate(resolution_name(resolution)));
                    }
                    for member in &candidates {
                        let check = capability_only.check_with_request_capabilities(
                            &member.exposed_id(),
                            &member.provider_id,
                            member.tier,
                            &member.capabilities,
                            self.is_circuit_blocked(&member.exposed_id()),
                            &request_capabilities,
                        );
                        if check.eligible {
                            if let Some(decision) = decisions
                                .iter_mut()
                                .find(|decision| decision.model_id == member.exposed_id())
                            {
                                decision.eligible = true;
                                decision.rejection = None;
                            }
                        }
                    }
                    candidates
                }
            }
        } else {
            filtered
        };

        if let (Some(tier), Resolution::Tier(original)) = (effective_tier, resolution) {
            if tier != *original {
                fallback_chain.push(original.virtual_id().to_string());
            }
        }

        let (scored, score_breakdowns) = self.score_and_sort(effective, preference, task);
        for (model_id, breakdown, total_score) in &score_breakdowns {
            if let Some(decision) = decisions
                .iter_mut()
                .find(|decision| &decision.model_id == model_id)
            {
                // A documented remediation candidate remains visibly rejected
                // by the hard gate; do not give rejected evidence a score.
                if decision.eligible {
                    decision.score = Some(breakdown.clone());
                    decision.final_score = Some(*total_score);
                }
            }
        }

        let routing = &registry.config().routing;
        let (pool_name, election_tier) = match effective_tier {
            Some(tier) => (tier.virtual_id().to_string(), Some(tier)),
            None => match resolution {
                Resolution::Direct(id) => (id.clone(), None),
                Resolution::Tier(tier) => (tier.virtual_id().to_string(), Some(*tier)),
            },
        };
        let mut candidates = self.plan_candidates(
            registry,
            scored,
            pool_name.as_str(),
            election_tier,
            routing.strategy,
            routing.failover,
            routing.max_attempts,
            pool_name.clone(),
            &request_capabilities,
            false,
            false,
        )?;
        let mut remediation_capabilities = requirements.required_capabilities.clone();
        remediation_capabilities.extend_from_slice(&request_capabilities);
        let remediation_capabilities = canonical_capabilities(&remediation_capabilities);
        for candidate in &mut candidates {
            if self.capability_remediation_allowed(
                registry,
                &candidate.entry,
                &remediation_capabilities,
            ) {
                // A documented remediation result is not a capability pass;
                // retain it only with an observable degraded marker.
                candidate.degraded = true;
            }
        }

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

        // Preserve the legacy policy revision for requests with no
        // request-derived capabilities. The accepted shadow checksum consumes
        // this field, so changing the empty-vector representation would make
        // otherwise identical replays appear to be new decisions. Non-empty
        // requests include the canonical request vector in the identity.
        let requirements_hash = if request_capabilities.is_empty() {
            hash_to_u64(requirements)
        } else {
            hash_to_u64(&(requirements, request_capabilities.as_slice()))
        };
        let policy_revision = PolicyRevision {
            policy_id: String::new(),
            policy_enabled: true,
            requirements_hash,
            preference_hash: hash_to_u64(preference),
        };

        let mut task_summary = task.map(TaskProfileSummary::from).unwrap_or_default();
        for capability in &request_capabilities {
            let label = capability_label(*capability).to_string();
            if !task_summary.required_capabilities.contains(&label) {
                task_summary.required_capabilities.push(label);
            }
        }

        let decision = RouteDecision {
            decision_id: format!("dec-{}", uuid::Uuid::new_v4().simple()),
            timestamp: chrono::Utc::now().timestamp(),
            task: task_summary,
            policy_id: String::new(),
            client_id: None,
            candidates: decisions,
            selected: candidates
                .first()
                .map(|candidate| candidate.exposed_id.clone()),
            fallback_chain,
            reason,
            policy_revision,
            // No model has been consulted here. The ML serving path amends this
            // record when it re-orders the plan, so a decision that carries this
            // as `None` is one the router produced on its own.
            ml_ranking: None,
        };

        Ok((candidates, decision))
    }
}
