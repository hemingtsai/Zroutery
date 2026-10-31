//! Narrowing a pool to the candidates that may be tried, and scoring them.

use super::helpers::resolution_name;
use super::Router;
use crate::config::{ModelEntry, ModelTier};
use crate::error::{Error, Result};
use crate::ir::Capability;
use crate::policy::{
    canonical_capabilities, score_candidate, CandidateDecision, PolicyPreference,
    PolicyRequirements, ScoringContext, TaskProfile,
};
use crate::registry::{Registry, Resolution};

impl Router {
    /// Whether this member satisfies the required capabilities through at least
    /// one documented remediation rather than natively.
    ///
    /// Native support is not remediation: a model that supports every
    /// requirement outright must not carry a degraded marker, which is why this
    /// predicate no longer returns true for an all-`Supported` set. It returns
    /// true only when at least one requirement is `Unknown` and a configured
    /// remediation covers it, while every other requirement is natively
    /// `Supported`. An `Unsupported` requirement can never be remediated.
    ///
    /// Two remediations exist: a vision model can describe an image the target
    /// cannot see, and a reasoning request can be served without its reasoning
    /// controls by a target whose thinking support is unknown. Both are
    /// explicit configuration, both mark the candidate degraded, and neither
    /// is a capability pass.
    pub(super) fn capability_remediation_allowed(
        &self,
        registry: &Registry,
        member: &ModelEntry,
        required_capabilities: &[Capability],
    ) -> bool {
        if required_capabilities.is_empty() {
            return false;
        }
        let config = registry.config();
        let rectifier = &config.routing.rectifier;
        let vision_remediation =
            config.vision.enabled || (rectifier.enabled && rectifier.media_fallback);
        let thinking_remediation = rectifier.enabled && rectifier.thinking_fallback;
        let mut remediated = false;
        for capability in required_capabilities {
            match member.capabilities.capability_state(*capability) {
                crate::ir::CapabilityState::Supported => {}
                crate::ir::CapabilityState::Unknown
                    if *capability == Capability::Vision && vision_remediation =>
                {
                    remediated = true;
                }
                crate::ir::CapabilityState::Unknown
                    if *capability == Capability::Thinking && thinking_remediation =>
                {
                    remediated = true;
                }
                crate::ir::CapabilityState::Unknown | crate::ir::CapabilityState::Unsupported => {
                    return false
                }
            }
        }
        remediated
    }

    pub(super) fn request_capability_remediation_allowed(
        &self,
        registry: &Registry,
        member: &ModelEntry,
        request_capabilities: &[Capability],
    ) -> bool {
        self.capability_remediation_allowed(registry, member, request_capabilities)
    }

    /// Filter members through the combined policy/request eligibility check.
    ///
    /// Uses a read-only health check (`is_circuit_blocked`) to avoid consuming
    /// half-open permits, which would be a side effect during eligibility
    /// filtering. Only a breaker that is still serving its cooldown counts as
    /// unavailable; an Open breaker whose timeout has elapsed stays eligible and
    /// claims its probe permit later, at the point of send.
    pub(super) fn filter_eligible<'a>(
        &self,
        registry: &Registry,
        members: &[&'a ModelEntry],
        requirements: &PolicyRequirements,
        request_capabilities: &[Capability],
    ) -> Vec<&'a ModelEntry> {
        let mut hard = Vec::new();
        let mut remediated = Vec::new();
        let mut remediation_capabilities = requirements.required_capabilities.clone();
        remediation_capabilities.extend_from_slice(request_capabilities);
        let remediation_capabilities = canonical_capabilities(&remediation_capabilities);

        for member in members {
            let circuit_open = self.is_circuit_blocked(&member.exposed_id())
                || self.is_provider_gated(&member.provider_id)
                || registry.provider_of(member).is_ok_and(|provider| {
                    self.default_model_rate_limit(member, provider).is_some()
                });
            let check = requirements.check_with_request_capabilities(
                &member.exposed_id(),
                &member.provider_id,
                member.tier,
                &member.capabilities,
                circuit_open,
                request_capabilities,
            );
            if check.eligible {
                hard.push(*member);
            } else if check.reasons.iter().all(|reason| {
                matches!(reason, crate::policy::RejectionReason::UnknownCapability(_))
            }) && self.capability_remediation_allowed(
                registry,
                member,
                &remediation_capabilities,
            ) {
                remediated.push(*member);
            }
        }

        // Never prefer a degraded remediation candidate while a hard-eligible
        // candidate exists in the same pool.
        if hard.is_empty() {
            remediated
        } else {
            hard
        }
    }

    pub(super) fn push_candidate_decision(
        &self,
        registry: &Registry,
        decisions: &mut Vec<CandidateDecision>,
        member: &ModelEntry,
        requirements: &PolicyRequirements,
        request_capabilities: &[Capability],
    ) {
        let rate_limited = registry
            .provider_of(member)
            .is_ok_and(|provider| self.default_model_rate_limit(member, provider).is_some());
        let circuit_open = self.is_circuit_blocked(&member.exposed_id())
            || self.is_provider_gated(&member.provider_id)
            || rate_limited;
        let check = requirements.check_with_request_capabilities(
            &member.exposed_id(),
            &member.provider_id,
            member.tier,
            &member.capabilities,
            circuit_open,
            request_capabilities,
        );
        let rejection = (!check.eligible).then(|| {
            crate::policy::bounded_trace_text(&if rate_limited {
                "rate_limit_cooldown".to_string()
            } else {
                check.reason_strings().join(", ")
            })
        });
        decisions.push(CandidateDecision {
            model_id: member.exposed_id(),
            provider_id: member.provider_id.clone(),
            tier: member.tier.map(|tier| tier.as_str().to_string()),
            eligible: check.eligible,
            rejection,
            score: None,
            final_score: None,
        });
    }

    pub(super) fn push_seen_decisions(
        &self,
        registry: &Registry,
        decisions: &mut Vec<CandidateDecision>,
        seen: &[&ModelEntry],
        requirements: &PolicyRequirements,
        request_capabilities: &[Capability],
    ) {
        for member in seen {
            if decisions
                .iter()
                .any(|decision| decision.model_id == member.exposed_id())
            {
                continue;
            }
            self.push_candidate_decision(
                registry,
                decisions,
                member,
                requirements,
                request_capabilities,
            );
        }
    }

    /// Try escalating or degrading through tiers until eligible candidates are found.
    ///
    /// `up` = true means escalate (higher tiers), false means degrade (lower tiers).
    /// The returned `seen` pool is retained so the caller can publish rejected
    /// evidence for every tier it inspected, not only the successful tier.
    pub(super) fn fallback_tier<'a>(
        &'a self,
        registry: &'a Registry,
        requirements: &PolicyRequirements,
        resolution: &Resolution,
        request_capabilities: &[Capability],
        max_steps: u32,
        up: bool,
    ) -> Result<(Vec<&'a ModelEntry>, ModelTier, Vec<&'a ModelEntry>)> {
        let start_tier = match resolution {
            Resolution::Tier(tier) => *tier,
            Resolution::Direct(id) => return Err(Error::NoCandidate(id.clone())),
        };

        let mut current = start_tier;
        let mut seen: Vec<&ModelEntry> = Vec::new();
        for _ in 0..max_steps {
            let Some(next) = (if up {
                current.higher()
            } else {
                current.lower()
            }) else {
                break;
            };
            current = next;
            let members = registry.tier_members(current);
            for member in &members {
                if !seen
                    .iter()
                    .any(|entry| entry.exposed_id() == member.exposed_id())
                {
                    seen.push(*member);
                }
            }
            if members.is_empty() {
                continue;
            }
            let filtered =
                self.filter_eligible(registry, &members, requirements, request_capabilities);
            if !filtered.is_empty() {
                tracing::info!(
                    from = %start_tier.virtual_id(),
                    to = %current.virtual_id(),
                    candidates = filtered.len(),
                    "fallback tier found eligible candidates"
                );
                return Ok((filtered, current, seen));
            }
        }

        let name = resolution_name(resolution);
        tracing::warn!(
            pool = %name,
            direction = if up { "escalate" } else { "degrade" },
            max_steps,
            "fallback exhausted; no eligible candidates found"
        );
        let mut inspected = registry.tier_members(start_tier);
        inspected.extend(seen);
        Err(self.rate_limited_pool(registry, &inspected, name))
    }

    /// Score candidates by policy preferences and return them sorted (best first),
    /// along with the per-candidate score breakdown for decision tracing.
    ///
    /// When a runtime observation exists for a candidate (from the
    /// [`ObservationStore`]), its health score and latency are used instead of
    /// the legacy circuit-breaker EWMA. Candidates that have never been observed
    /// fall back to the legacy path.
    pub(super) fn score_and_sort<'a>(
        &self,
        members: Vec<&'a ModelEntry>,
        preference: &PolicyPreference,
        task: Option<&TaskProfile>,
    ) -> (
        Vec<&'a ModelEntry>,
        Vec<(String, crate::policy::ScoreBreakdown, f64)>,
    ) {
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
                let streaming = task.is_some_and(|t| t.streaming);
                let avg_latency_ms = if streaming {
                    // For streaming, TTFT is the primary signal; fall back to
                    // total latency when no TTFT observation exists.
                    obs.latency
                        .ttft_ms
                        .value
                        .or(obs.latency.total_ms.value)
                        .unwrap_or_else(|| self.avg_latency(&m.exposed_id()))
                } else {
                    obs.latency
                        .total_ms
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
}
