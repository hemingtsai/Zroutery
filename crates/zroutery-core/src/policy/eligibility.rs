//! Whether a candidate may be used at all, and the single reason it may not.

use super::capabilities::{canonical_capabilities, capability_label};
use super::policy::PolicyRequirements;

use crate::config::{ModelCapabilities, ModelTier};
use crate::ir::{Capability, CapabilityState};

// -------------------------------------------------------- Eligibility

/// Result of checking a candidate against policy requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibilityCheck {
    pub eligible: bool,
    pub reasons: Vec<RejectionReason>,
}

/// Why a candidate was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RejectionReason {
    /// The provider explicitly declared the capability unsupported.
    MissingCapability(Capability),
    /// The provider has not declared the capability. This is never a pass for
    /// a request-derived requirement.
    UnknownCapability(Capability),
    BelowMinTier,
    AboveMaxTier,
    /// Candidate has no tier assigned but policy requires tier bounds.
    UnknownTier,
    ProviderForbidden,
    ProviderNotAllowed,
    ModelForbidden,
    ModelNotAllowed,
    CircuitOpen,
}

impl EligibilityCheck {
    /// Return fixed, bounded reason tokens for serialization into a trace.
    pub fn reason_strings(&self) -> Vec<String> {
        self.reasons.iter().map(ToString::to_string).collect()
    }

    /// Whether the check contains an unknown request capability.
    pub fn has_unknown_capability(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| matches!(reason, RejectionReason::UnknownCapability(_)))
    }
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectionReason::MissingCapability(cap) => {
                write!(f, "missing_capability:{}", capability_label(*cap))
            }
            RejectionReason::UnknownCapability(cap) => {
                write!(f, "unknown_capability:{}", capability_label(*cap))
            }
            RejectionReason::BelowMinTier => write!(f, "below_min_tier"),
            RejectionReason::AboveMaxTier => write!(f, "above_max_tier"),
            RejectionReason::UnknownTier => write!(f, "unknown_tier"),
            RejectionReason::ProviderForbidden => write!(f, "provider_forbidden"),
            RejectionReason::ProviderNotAllowed => write!(f, "provider_not_allowed"),
            RejectionReason::ModelForbidden => write!(f, "model_forbidden"),
            RejectionReason::ModelNotAllowed => write!(f, "model_not_allowed"),
            RejectionReason::CircuitOpen => write!(f, "circuit_open"),
        }
    }
}

impl PolicyRequirements {
    /// Check whether a candidate satisfies the configured policy constraints.
    ///
    /// This compatibility entry point preserves the historical reason shape
    /// for policy-only checks. Request-derived requirements should use
    /// [`Self::check_with_request_capabilities`], which also gives unknown
    /// capabilities their explicit trace reason.
    pub fn check(
        &self,
        model_id: &str,
        provider_id: &str,
        tier: Option<ModelTier>,
        capabilities: &ModelCapabilities,
        circuit_open: bool,
    ) -> EligibilityCheck {
        self.check_internal(model_id, provider_id, tier, capabilities, circuit_open, &[])
    }

    /// Check policy constraints plus hard request-derived capabilities.
    ///
    /// `request_capabilities` is never softened by `strict_capabilities` or by
    /// a policy fallback. Both policy and request vectors are normalized to the
    /// canonical capability order before reasons are emitted.
    pub fn check_with_request_capabilities(
        &self,
        model_id: &str,
        provider_id: &str,
        tier: Option<ModelTier>,
        capabilities: &ModelCapabilities,
        circuit_open: bool,
        request_capabilities: &[Capability],
    ) -> EligibilityCheck {
        self.check_internal(
            model_id,
            provider_id,
            tier,
            capabilities,
            circuit_open,
            request_capabilities,
        )
    }

    /// Check only the hard request capability contract.
    pub fn check_request_capabilities(
        model_id: &str,
        provider_id: &str,
        tier: Option<ModelTier>,
        capabilities: &ModelCapabilities,
        circuit_open: bool,
        request_capabilities: &[Capability],
    ) -> EligibilityCheck {
        PolicyRequirements::default().check_with_request_capabilities(
            model_id,
            provider_id,
            tier,
            capabilities,
            circuit_open,
            request_capabilities,
        )
    }

    fn check_internal(
        &self,
        model_id: &str,
        provider_id: &str,
        tier: Option<ModelTier>,
        capabilities: &ModelCapabilities,
        circuit_open: bool,
        request_capabilities: &[Capability],
    ) -> EligibilityCheck {
        let policy_capabilities = canonical_capabilities(&self.required_capabilities);
        let request_capabilities = canonical_capabilities(request_capabilities);
        let mut reasons = Vec::new();

        // Capability checks use the accepted tri-state provider contract. A
        // request-derived requirement is always strict; policy-only unknown
        // capabilities are also fail-closed, with the legacy switch retained
        // only to preserve the historical reason shape.
        for capability in Capability::ALL {
            let required_by_request = request_capabilities.contains(&capability);
            let required_by_policy = policy_capabilities.contains(&capability);
            if !required_by_request && !required_by_policy {
                continue;
            }

            match capabilities.capability_state(capability) {
                CapabilityState::Supported => {}
                CapabilityState::Unsupported => {
                    reasons.push(RejectionReason::MissingCapability(capability));
                }
                CapabilityState::Unknown if required_by_request => {
                    reasons.push(RejectionReason::UnknownCapability(capability));
                }
                CapabilityState::Unknown if self.strict_capabilities => {
                    // Preserve the original strict-policy reason shape for
                    // compatibility with callers of `check`.
                    reasons.push(RejectionReason::MissingCapability(capability));
                }
                CapabilityState::Unknown => {
                    // A policy-only soft setting cannot turn an unknown
                    // provider capability into a pass. A caller that has a
                    // documented remediation must represent it at the router
                    // boundary and mark the resulting candidate degraded.
                    reasons.push(RejectionReason::UnknownCapability(capability));
                }
            }
        }

        // Tier bounds: if the policy sets min/max tier and the candidate has
        // no tier, the candidate cannot satisfy tier constraints.
        if self.min_tier.is_some() || self.max_tier.is_some() {
            match tier {
                Some(t) => {
                    if let Some(min) = self.min_tier {
                        if t < min {
                            reasons.push(RejectionReason::BelowMinTier);
                        }
                    }
                    if let Some(max) = self.max_tier {
                        if t > max {
                            reasons.push(RejectionReason::AboveMaxTier);
                        }
                    }
                }
                None => {
                    reasons.push(RejectionReason::UnknownTier);
                }
            }
        }

        // Provider constraints.
        if !self.allowed_providers.is_empty()
            && !self.allowed_providers.iter().any(|p| p == provider_id)
        {
            reasons.push(RejectionReason::ProviderNotAllowed);
        }
        if self.forbidden_providers.iter().any(|p| p == provider_id) {
            reasons.push(RejectionReason::ProviderForbidden);
        }

        // Model constraints.
        if !self.allowed_models.is_empty() && !self.allowed_models.iter().any(|m| m == model_id) {
            reasons.push(RejectionReason::ModelNotAllowed);
        }
        if self.forbidden_models.iter().any(|m| m == model_id) {
            reasons.push(RejectionReason::ModelForbidden);
        }

        // Circuit state is a separate, deterministic rejection reason. It is
        // not a capability fallback and must not be hidden by policy fallback.
        if circuit_open {
            reasons.push(RejectionReason::CircuitOpen);
        }

        EligibilityCheck {
            eligible: reasons.is_empty(),
            reasons,
        }
    }
}
