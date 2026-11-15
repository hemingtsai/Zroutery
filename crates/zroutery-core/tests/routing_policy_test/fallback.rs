use zroutery_core::config::{ModelCapabilities, ModelEntry, ModelTier};
use zroutery_core::error::Error;
use zroutery_core::ir::Capability;
use zroutery_core::policy::{DecisionReason, PolicyFallback, PolicyPreference, PolicyRequirements};
use zroutery_core::registry::Resolution;
use zroutery_core::router::Router;

use super::support::*;

// ========================================================================
// 9. Escalation / De-escalation E2E tests
// ========================================================================

#[test]
fn escalation_finds_higher_tier() {
    // Standard has no eligible candidates (circuit open)
    // Escalate to Reasoning which has eligible candidates
    // Verify Reasoning candidate is selected
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
    ]);
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Trip the circuit breaker on the Standard model so it is ineligible.
    let timeout_err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-std-m", &timeout_err, &routing);
    }
    assert!(
        !router.allow_request("p1-std-m"),
        "circuit should be open after enough failures"
    );

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference::default();
    let fallback = PolicyFallback::Escalate {
        enabled: true,
        max_steps: 1,
    };

    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &preference,
            &fallback,
            None,
        )
        .unwrap();

    assert!(!candidates.is_empty(), "escalation should find a candidate");
    assert_eq!(
        candidates[0].exposed_id, "p1-reas-m",
        "escalation should land on the Reasoning model"
    );
}

#[test]
fn degradation_finds_lower_tier() {
    // Frontier has no eligible candidates (circuit open)
    // Degrade to Reasoning which has eligible candidates
    // Verify Reasoning candidate is selected
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "front-m", Some(ModelTier::Frontier)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
    ]);
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Trip the circuit breaker on the Frontier model so it is ineligible.
    let timeout_err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-front-m", &timeout_err, &routing);
    }
    assert!(
        !router.allow_request("p1-front-m"),
        "circuit should be open after enough failures"
    );

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference::default();
    let fallback = PolicyFallback::Degrade {
        enabled: true,
        max_steps: 1,
    };

    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Frontier),
            &[],
            &requirements,
            &preference,
            &fallback,
            None,
        )
        .unwrap();

    assert!(
        !candidates.is_empty(),
        "degradation should find a candidate"
    );
    assert_eq!(
        candidates[0].exposed_id, "p1-reas-m",
        "degradation should land on the Reasoning model"
    );
}

// ========================================================================
// 13. Fallback Contract Tests
// ========================================================================

/// Escalation goes to a higher tier.
#[test]
fn fallback_escalation_goes_higher() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
    ]);
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Make the Standard model ineligible via circuit breaker.
    let timeout_err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-std-m", &timeout_err, &routing);
    }

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference::default();
    let fallback = PolicyFallback::Escalate {
        enabled: true,
        max_steps: 1,
    };

    let (candidates, decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &preference,
            &fallback,
            None,
        )
        .unwrap();

    assert!(!candidates.is_empty(), "escalation should find a candidate");
    assert_eq!(candidates[0].exposed_id, "p1-reas-m");
    assert!(matches!(decision.reason, DecisionReason::Escalated { .. }));
}

/// Degradation goes to a lower tier.
#[test]
fn fallback_degradation_goes_lower() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "front-m", Some(ModelTier::Frontier)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
    ]);
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Make the Frontier model ineligible.
    let timeout_err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-front-m", &timeout_err, &routing);
    }

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference::default();
    let fallback = PolicyFallback::Degrade {
        enabled: true,
        max_steps: 1,
    };

    let (candidates, decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Frontier),
            &[],
            &requirements,
            &preference,
            &fallback,
            None,
        )
        .unwrap();

    assert!(
        !candidates.is_empty(),
        "degradation should find a candidate"
    );
    assert_eq!(candidates[0].exposed_id, "p1-reas-m");
    assert!(matches!(decision.reason, DecisionReason::Degraded { .. }));
}

/// Reject fallback returns an error when no candidates are eligible.
#[test]
fn fallback_reject_returns_error() {
    let mut no_caps = ModelEntry::for_upstream("p1", "no-caps", Some(ModelTier::Standard));
    no_caps.capabilities = ModelCapabilities::default();
    let cfg = e2e_cfg_with(vec![no_caps]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        strict_capabilities: true,
        ..Default::default()
    };
    let fallback = PolicyFallback::Reject;

    let err = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &PolicyPreference::default(),
            &fallback,
            None,
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::NoCandidate(_)),
        "Reject fallback should produce NoCandidate, got: {err:?}"
    );
}

/// An empty tier takes the same configured fallback as a tier that filtered
/// down to nothing.
///
/// "No model is configured for this tier" and "every member was rejected" are
/// the same routing state for the client policy, so Escalate/Degrade must run
/// for both. Reject must still reject.
#[test]
fn empty_tier_uses_the_configured_fallback() {
    let cfg = e2e_cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "std-m",
        Some(ModelTier::Standard),
    )]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let escalate = PolicyFallback::Escalate {
        enabled: true,
        max_steps: 1,
    };
    let (candidates, decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Fast),
            &[],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &escalate,
            None,
        )
        .unwrap();
    assert_eq!(candidates[0].exposed_id, "p1-std-m");
    assert!(matches!(decision.reason, DecisionReason::Escalated { .. }));
    assert!(decision.fallback_chain.contains(&"fast-class".to_string()));

    // Degrading from an empty higher tier reaches the lower one.
    let degrade = PolicyFallback::Degrade {
        enabled: true,
        max_steps: 2,
    };
    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Frontier),
            &[],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &degrade,
            None,
        )
        .unwrap();
    assert_eq!(candidates[0].exposed_id, "p1-std-m");

    // Reject is unchanged: an empty pool is a hard error.
    let err = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Fast),
            &[],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::NoCandidate(_)),
        "Reject must still reject an empty tier, got: {err:?}"
    );

    // A tier whose only member is disabled is empty for the same purpose.
    let mut disabled = ModelEntry::for_upstream("p1", "fast-m", Some(ModelTier::Fast));
    disabled.enabled = false;
    let reg = e2e_reg(e2e_cfg_with(vec![
        disabled,
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Standard)),
    ]));
    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Fast),
            &[],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &escalate,
            None,
        )
        .unwrap();
    assert_eq!(candidates[0].exposed_id, "p1-std-m");
}

/// Max escalation steps is respected: escalation stops after max_steps even
/// if higher tiers have eligible candidates.
#[test]
fn fallback_respects_max_steps() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
        ModelEntry::for_upstream("p1", "front-m", Some(ModelTier::Frontier)).with_priority(0),
    ]);
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Make Standard and Reasoning ineligible.
    let timeout_err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-std-m", &timeout_err, &routing);
        router.report_failure("p1-reas-m", &timeout_err, &routing);
    }

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference::default();

    // max_steps=1: can only escalate one step (Standard -> Reasoning).
    // Reasoning is also open, so this should fail.
    let fallback_1 = PolicyFallback::Escalate {
        enabled: true,
        max_steps: 1,
    };
    let err = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &preference,
            &fallback_1,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::NoCandidate(_)),
        "max_steps=1 should not reach Frontier, got: {err:?}"
    );

    // max_steps=2: can escalate two steps (Standard -> Reasoning -> Frontier).
    // Frontier is healthy, so this should succeed.
    let fallback_2 = PolicyFallback::Escalate {
        enabled: true,
        max_steps: 2,
    };
    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &preference,
            &fallback_2,
            None,
        )
        .unwrap();
    assert!(!candidates.is_empty(), "max_steps=2 should reach Frontier");
    assert_eq!(candidates[0].exposed_id, "p1-front-m");
}
