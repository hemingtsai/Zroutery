use zroutery_core::config::{ModelCapabilities, ModelEntry, ModelTier};
use zroutery_core::error::Error;
use zroutery_core::ir::Capability;
use zroutery_core::policy::{PolicyFallback, PolicyPreference, PolicyRequirements};
use zroutery_core::registry::Resolution;
use zroutery_core::router::Router;

use super::support::*;

// ========================================================================
// 8. E2E decision: policy -> eligibility -> scoring -> candidate selection
// ========================================================================

/// Test 1: Policy selects best candidate based on requirements + scoring.
///
/// Three models all in the Reasoning tier, all with tools capability.
/// Policy requires Tools and prefers the Reasoning tier.  Scoring with
/// priority weight selects the model with the best (lowest) priority.
#[test]
fn e2e_selects_best_candidate_by_scoring() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Reasoning)).with_priority(10),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
        ModelEntry::for_upstream("p1", "front-m", Some(ModelTier::Reasoning)).with_priority(20),
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        ..Default::default()
    };
    // Tier weight and priority weight are both active so scoring actually
    // differentiates: all three are at distance-0 from preferred (Reasoning),
    // but priority 0 scores highest.
    let preference = PolicyPreference {
        preferred_tier: Some(ModelTier::Reasoning),
        tier_weight: 0.5,
        priority_weight: 0.5,
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
    };
    let fallback = PolicyFallback::Reject;

    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Reasoning),
            &[],
            &requirements,
            &preference,
            &fallback,
            None,
        )
        .unwrap();

    assert!(!candidates.is_empty(), "at least one candidate expected");
    assert_eq!(candidates[0].exposed_id, "p1-reas-m");
}

/// Test 2: Policy rejects when required capability is missing.
///
/// Two models in the Standard tier: one has tools only, the other has no
/// capabilities at all.  The policy requires both Tools and Vision with
/// strict mode.  Neither model qualifies, and the Reject fallback produces
/// a `NoCandidate` error.
#[test]
fn e2e_rejects_when_capability_missing() {
    let mut no_caps = ModelEntry::for_upstream("p1", "no-caps", Some(ModelTier::Standard));
    no_caps.capabilities = ModelCapabilities::default(); // all false

    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "tools-only", Some(ModelTier::Standard)),
        no_caps,
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Tools, Capability::Vision],
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
        "expected NoCandidate, got: {err:?}"
    );
}

/// Test 3: Policy escalates tier on fallback.
///
/// The Standard-tier model has an open circuit breaker so it is filtered
/// out by eligibility.  With `Escalate { max_steps: 1 }` the router moves
/// up to the Reasoning tier and selects the healthy model there.
#[test]
fn e2e_escalates_tier_on_fallback() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)).with_priority(0),
    ]);
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Trip the circuit breaker on the Standard model.
    let timeout_err = Error::Timeout(5);
    for _ in 0..routing.circuit_breaker.failure_threshold {
        router.report_failure("p1-std-m", &timeout_err, &routing);
    }
    assert!(
        !router.allow_request("p1-std-m"),
        "circuit should be open after enough failures"
    );

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference {
        preferred_tier: Some(ModelTier::Standard),
        ..Default::default()
    };
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

/// A model whose breaker cooldown has elapsed stays eligible on the policy
/// path.
///
/// The Open → HalfOpen transition happens inside `allow_request`, so eligibility
/// filtering must not treat a probeable Open breaker as unavailable: doing so
/// keeps the model out of every tier for the rest of the process lifetime, long
/// after the upstream recovered. The single probe permit is still claimed
/// atomically at the point of send.
#[test]
fn policy_path_admits_a_model_whose_cooldown_elapsed() {
    let mut cfg = e2e_cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "std-m",
        Some(ModelTier::Standard),
    )]);
    cfg.routing.circuit_breaker.failure_threshold = 1;
    cfg.routing.circuit_breaker.timeout_secs = 0;
    let routing = cfg.routing.clone();
    let reg = e2e_reg(cfg);
    let router = Router::new();

    router.report_failure("p1-std-m", &Error::Timeout(5), &routing);
    assert!(router.is_cooling("p1-std-m"), "one failure tripped it");
    assert!(router.is_circuit_open("p1-std-m"), "breaker is Open");
    assert!(
        !router.is_circuit_blocked("p1-std-m"),
        "the cooldown already elapsed, so it is probeable"
    );

    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap();
    assert_eq!(candidates[0].exposed_id, "p1-std-m");

    // Availability is not a reservation: exactly one probe permit is claimed
    // at the point of send.
    assert!(router.allow_request("p1-std-m"));
    assert!(!router.allow_request("p1-std-m"));

    // Control: while the cooldown is still running the policy path still
    // rejects the model rather than planning an impossible send.
    let mut cooling_cfg = e2e_cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "std-m",
        Some(ModelTier::Standard),
    )]);
    cooling_cfg.routing.circuit_breaker.failure_threshold = 1;
    cooling_cfg.routing.circuit_breaker.timeout_secs = 60;
    let cooling_routing = cooling_cfg.routing.clone();
    let cooling_reg = e2e_reg(cooling_cfg);
    let cooling_router = Router::new();
    cooling_router.report_failure("p1-std-m", &Error::Timeout(5), &cooling_routing);
    assert!(cooling_router.is_circuit_blocked("p1-std-m"));
    let err = cooling_router
        .plan_with_policy(
            &cooling_reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &PolicyRequirements::default(),
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::NoCandidate(_)),
        "a breaker still serving its cooldown is not eligible, got: {err:?}"
    );
}

/// Test 4: Forbidden provider is never selected.
///
/// Two models in the Standard tier: one from the forbidden provider "p1"
/// (with better priority) and one from the allowed provider "p2".  The
/// policy must select the "p2" model and never include the "p1" model.
#[test]
fn e2e_forbidden_provider_never_selected() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "bad-m", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p2", "good-m", Some(ModelTier::Standard)).with_priority(10),
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        forbidden_providers: vec!["p1".into()],
        ..Default::default()
    };
    let fallback = PolicyFallback::Reject;

    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &PolicyPreference::default(),
            &fallback,
            None,
        )
        .unwrap();

    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].exposed_id, "p2-good-m");
    assert!(
        candidates.iter().all(|c| c.exposed_id != "p1-bad-m"),
        "forbidden provider model must not appear in candidates"
    );
}

/// Test 5: Tier bounds are enforced.
///
/// Policy: `min_tier = Standard`, `max_tier = Reasoning`.
/// Standard and Reasoning models are within bounds and eligible.
/// A Fast-tier model is below `min_tier` and produces `NoCandidate`.
#[test]
fn e2e_tier_bounds_enforced() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-m", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p1", "reas-m", Some(ModelTier::Reasoning)),
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let fallback = PolicyFallback::Reject;

    // Standard model is within [Standard, Reasoning] → eligible.
    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &requirements,
            &PolicyPreference::default(),
            &fallback,
            None,
        )
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].exposed_id, "p1-std-m");

    // Reasoning model is within [Standard, Reasoning] → eligible.
    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Reasoning),
            &[],
            &requirements,
            &PolicyPreference::default(),
            &fallback,
            None,
        )
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].exposed_id, "p1-reas-m");

    // Fast model is below min_tier=Standard → rejected.
    let cfg_fast = e2e_cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "fast-m",
        Some(ModelTier::Fast),
    )]);
    let reg_fast = e2e_reg(cfg_fast);
    let err = router
        .plan_with_policy(
            &reg_fast,
            &Resolution::Tier(ModelTier::Fast),
            &[],
            &requirements,
            &PolicyPreference::default(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::NoCandidate(_)),
        "Fast model below min_tier should produce NoCandidate, got: {err:?}"
    );
}
