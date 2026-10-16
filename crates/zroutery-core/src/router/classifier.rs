//! The Auto Mode classifier pool, which shares the tier implementation so health cannot drift between pools.

use super::candidate::Candidate;
use super::Router;
use super::CLASSIFIER_POOL;
use crate::config::{ClassifierConfig, ModelEntry, ModelTier, RoutingConfig};
use crate::error::{Error, Result};
use crate::ir::Capability;
use crate::policy::canonical_capabilities;
use crate::registry::Registry;

impl Router {
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
        self.apply_routing_config(&registry.config().routing);
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
            // OpenCode's free tier cannot answer the classifier: its gate forces
            // an agent-shaped stream and the classifier is fail-closed, so a free
            // model would over-block.
            if registry
                .provider_of(entry)
                .is_ok_and(|provider| provider.kind.is_opencode())
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

    pub(super) fn plan_tier(
        &self,
        registry: &Registry,
        tier: ModelTier,
        routing: &RoutingConfig,
        required_capabilities: &[Capability],
        _capability_filter: bool,
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
            &canonical_capabilities(required_capabilities),
            // Request-derived capabilities are always filtered below. These
            // legacy flags remain parameters for API/internal compatibility.
            true,
            true,
        )
    }
}
