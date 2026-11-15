use zroutery_core::config::{ModelCapabilities, ModelTier};
use zroutery_core::ir::Capability;
use zroutery_core::policy::{PolicyRequirements, RejectionReason};

// ========================================================================
// 4. Eligibility: tier bounds, forbidden provider, capability, circuit open
// ========================================================================

#[test]
fn eligibility_all_pass() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        min_tier: Some(ModelTier::Fast),
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities {
        tools: true,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
    assert!(check.reasons.is_empty());
}

#[test]
fn eligibility_below_min_tier_rejected() {
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Fast), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::BelowMinTier)));
}

#[test]
fn eligibility_above_max_tier_rejected() {
    let reqs = PolicyRequirements {
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Frontier), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::AboveMaxTier)));
}

#[test]
fn eligibility_at_min_tier_accepted() {
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_at_max_tier_accepted() {
    let reqs = PolicyRequirements {
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Reasoning), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_forbidden_provider_rejected() {
    let reqs = PolicyRequirements {
        forbidden_providers: vec!["bad-provider".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "bad-provider", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::ProviderForbidden)));
}

#[test]
fn eligibility_forbidden_provider_other_accepted() {
    let reqs = PolicyRequirements {
        forbidden_providers: vec!["bad-provider".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check(
        "m",
        "good-provider",
        Some(ModelTier::Standard),
        &caps,
        false,
    );
    assert!(check.eligible);
}

#[test]
fn eligibility_missing_capability_rejected() {
    // With strict_capabilities = true, unknown capabilities are rejected.
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: true,
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::MissingCapability(Capability::Vision))));
}

#[test]
fn eligibility_unknown_capability_is_not_a_soft_pass() {
    // An undeclared provider capability is not support. A documented
    // remediation is a separate router decision and must be observable.
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: false,
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(check.reasons.iter().any(|reason| matches!(
        reason,
        RejectionReason::UnknownCapability(Capability::Vision)
    )));
}

#[test]
fn eligibility_capability_present_accepted() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: true,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_circuit_open_rejected() {
    let reqs = PolicyRequirements::default();
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, true);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::CircuitOpen)));
}

#[test]
fn eligibility_multiple_failures_reported() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: true, // so unknown vision counts as a failure
        forbidden_providers: vec!["bad".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "bad", Some(ModelTier::Standard), &caps, true);
    assert!(!check.eligible);
    assert_eq!(check.reasons.len(), 3);
}

#[test]
fn eligibility_allowed_providers_restricts() {
    let reqs = PolicyRequirements {
        allowed_providers: vec!["openai".into(), "anthropic".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    assert!(
        reqs.check("m", "openai", Some(ModelTier::Standard), &caps, false)
            .eligible
    );
    assert!(
        !reqs
            .check("m", "deepseek", Some(ModelTier::Standard), &caps, false)
            .eligible
    );
}

#[test]
fn eligibility_forbidden_models_rejected() {
    let reqs = PolicyRequirements {
        forbidden_models: vec!["bad-model".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    assert!(
        !reqs
            .check("bad-model", "p", Some(ModelTier::Standard), &caps, false)
            .eligible
    );
    assert!(
        reqs.check("good-model", "p", Some(ModelTier::Standard), &caps, false)
            .eligible
    );
}

#[test]
fn eligibility_allowed_models_restricts() {
    let reqs = PolicyRequirements {
        allowed_models: vec!["gpt-4".into(), "gpt-5".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    assert!(
        reqs.check("gpt-4", "p", Some(ModelTier::Standard), &caps, false)
            .eligible
    );
    assert!(
        !reqs
            .check("claude-3", "p", Some(ModelTier::Standard), &caps, false)
            .eligible
    );
}
