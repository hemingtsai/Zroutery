//! Tier escalation: which tiers are fully cooling, and what that means for the announcement.

use super::candidate::ModelHealth;
use std::collections::HashSet;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::config::{ModelEntry, ModelTier};

/// Tiers whose every enabled member is currently serving a breaker cooldown,
/// in tier order.
///
/// This is the health-driven half of the condition that makes
/// [`PolicyFallback::Escalate`] fire, read from the same breaker state the
/// eligibility filter reads. Deriving it is the point: a separately maintained
/// "am I escalated" flag would be a second opinion about a decision the breaker
/// already knows, and the two could disagree.
///
/// **A narrower claim than "this tier was escalated from."** A tier is also
/// empty when the user configured no model for it, or when every member failed a
/// capability requirement. Neither is an outage and neither is reported. A tier
/// with no enabled member is not reported either: "every member is cooling" is
/// vacuously true of an empty set, and an unconfigured tier is not an outage.
///
/// A member with no health record yet counts as not cooling, which is what the
/// router assumes about it — an unobserved candidate is closed.
pub fn tiers_fully_cooling(models: &[ModelEntry], health: &[ModelHealth]) -> Vec<ModelTier> {
    let cooling: HashSet<&str> = health
        .iter()
        .filter(|entry| entry.cooldown_remaining_secs > 0)
        .map(|entry| entry.model_id.as_str())
        .collect();
    ModelTier::ALL
        .iter()
        .copied()
        .filter(|tier| {
            let mut members = models
                .iter()
                .filter(|model| model.enabled && model.tier.as_ref() == Some(tier));
            match members.next() {
                None => false,
                Some(first) => {
                    cooling.contains(first.exposed_id().as_str())
                        && members.all(|m| cooling.contains(m.exposed_id().as_str()))
                }
            }
        })
        .collect()
}

/// What moved between two observations of the escalated-tier set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierEscalationChange {
    /// Tiers that have no usable member now and did before.
    pub opened: Vec<ModelTier>,
    /// Tiers that had none and do now.
    pub closed: Vec<ModelTier>,
}

/// The single thing worth telling a user about a [`TierEscalationChange`].
///
/// At most one per observation, on purpose. An outage and a recovery inside the
/// same observation window is one event with two ends, and two notifications for
/// one event is how a notification becomes something people turn off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierAnnouncement {
    Escalated(Vec<ModelTier>),
    Recovered(Vec<ModelTier>),
}

/// Compare two escalated-tier sets.
///
/// This is the whole of the de-duplication. It takes the previous set rather
/// than remembering anything, so a watcher that calls it on a timer announces a
/// tier **once per outage episode** and not once per observation: the second tick
/// of a steady outage compares a set against itself and reports no change, no
/// matter how many requests were escalated in between.
///
/// Both sides are compared by membership rather than by order, so a reordering
/// cannot manufacture an announcement, and the output is in tier order so the
/// text is stable.
pub fn tier_escalation_change(
    previous: &[ModelTier],
    current: &[ModelTier],
) -> TierEscalationChange {
    let was = |tier: &ModelTier| previous.contains(tier);
    let now = |tier: &ModelTier| current.contains(tier);
    TierEscalationChange {
        opened: ModelTier::ALL
            .iter()
            .copied()
            .filter(|tier| now(tier) && !was(tier))
            .collect(),
        closed: ModelTier::ALL
            .iter()
            .copied()
            .filter(|tier| was(tier) && !now(tier))
            .collect(),
    }
}

impl TierEscalationChange {
    /// Nothing happened, so there is nothing to say.
    pub fn is_empty(&self) -> bool {
        self.opened.is_empty() && self.closed.is_empty()
    }

    /// At most one announcement, and an escalation outranks a recovery.
    pub fn announcement(&self) -> Option<TierAnnouncement> {
        if !self.opened.is_empty() {
            Some(TierAnnouncement::Escalated(self.opened.clone()))
        } else if !self.closed.is_empty() {
            Some(TierAnnouncement::Recovered(self.closed.clone()))
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub(super) struct HealthState {
    pub(super) breaker: CircuitBreaker,
    pub(super) total_success: u64,
    pub(super) total_failure: u64,
    pub(super) avg_latency_ms: f64,
    pub(super) last_error: Option<String>,
}

impl HealthState {
    pub(super) fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breaker: CircuitBreaker::new(config),
            total_success: 0,
            total_failure: 0,
            avg_latency_ms: 0.0,
            last_error: None,
        }
    }
}
