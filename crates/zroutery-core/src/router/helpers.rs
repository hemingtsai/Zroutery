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
