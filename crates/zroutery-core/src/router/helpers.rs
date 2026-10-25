//! The small pure functions the planning path shares.

use crate::config::ModelEntry;
use crate::failure::ClassifiedFailure;
use crate::ir::Capability;
use crate::registry::Resolution;

pub(super) fn resolution_name(resolution: &Resolution) -> String {
    match resolution {
        Resolution::Direct(id) => id.clone(),
        Resolution::Tier(tier) => tier.virtual_id().to_string(),
    }
}

pub(super) fn classification_trace_label(failure: &ClassifiedFailure) -> String {
    let class = format!("{:?}", failure.class).to_lowercase();
    match failure.status {
        Some(status) => format!("{class}:{status}"),
        None => class,
    }
}

/// Priority order with a stable tiebreak, shared by the `Priority` strategy and
/// by `Balanced` when no election has been held (or the pool has no tier).
///
/// # Why the tiebreak is not optional
///
/// A tier holds every enabled model of that class, and a real configuration
/// holds dozens of them — most of which share one priority, because the user
/// sets a priority to express an intent ("this one first") rather than to
/// number every model. So the tiebreak decides the order of almost the whole
/// pool.
///
/// Without it the order of equal-priority models is whatever order the
/// candidates happened to arrive in, and that order is not a function of the
/// configuration: it comes from the policy's own scoring, from a hash
/// iteration, and from whatever else contributed a candidate. Two requests
/// with the same question could then be served by two different upstreams,
/// which is the opposite of what a `priority` strategy promises and the reason
/// a deterministic configuration behaved as though it were random.
///
/// The tiebreak is the exposed id, which is a property of the configuration
/// alone: the same configuration always yields the same order, and the order
/// is the same on every run and every machine.
pub(super) fn by_priority<'a>(members: &[&'a ModelEntry]) -> Vec<&'a ModelEntry> {
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
/// A boolean capability declaration is tri-state at the policy boundary: true
/// is `Supported`, while false is `Unknown`, never an implicit pass.
pub(super) fn satisfies_capabilities(model: &ModelEntry, required: &[Capability]) -> bool {
    required.iter().all(|capability| {
        model.capabilities.capability_state(*capability) == crate::ir::CapabilityState::Supported
    })
}
