use zroutery_core::config::{ModelCapabilities, ModelEntry, ModelTier};
use zroutery_core::error::Error;
use zroutery_core::ir::Capability;
use zroutery_core::policy::{PolicyFallback, PolicyPreference, PolicyRequirements};
use zroutery_core::registry::Resolution;
use zroutery_core::router::Router;

use super::support::*;

// ========================================================================
// 10. Policy Regression Matrix
// ========================================================================

/// Coding task with tools requirement selects a reasoning-capable model.
///
/// The policy requires Tools capability with min_tier=Reasoning. A Standard
/// model with tools is below min_tier and must be rejected; only the Reasoning
/// model is eligible.
#[test]
fn matrix_coding_task_selects_reasoning_with_tools() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "std-tools", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "reas-tools", Some(ModelTier::Reasoning)).with_priority(10),
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        min_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let preference = PolicyPreference::default();
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

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].exposed_id, "p1-reas-tools");
}

/// Vision task rejects non-vision models.
///
/// Policy requires Vision capability with strict mode. Two Standard-tier
/// models: one with vision, one without. Only the vision model survives.
#[test]
fn matrix_vision_task_rejects_non_vision() {
    let mut no_vision = ModelEntry::for_upstream("p1", "text-only", Some(ModelTier::Standard));
    no_vision.capabilities = ModelCapabilities::default(); // all false
    let mut has_vision = ModelEntry::for_upstream("p1", "has-vision", Some(ModelTier::Standard));
    has_vision.capabilities.vision = true;
    let cfg = e2e_cfg_with(vec![has_vision, no_vision]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: true,
        ..Default::default()
    };
    let fallback = PolicyFallback::Reject;

    let (candidates, decision) = router
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
    assert_eq!(candidates[0].exposed_id, "p1-has-vision");
    // The text-only model should appear as rejected in the decision trace.
    let rejected = decision
        .candidates
        .iter()
        .find(|c| c.model_id == "p1-text-only");
    assert!(rejected.is_some());
    assert!(!rejected.unwrap().eligible);
    assert!(rejected.unwrap().rejection.is_some());
}

/// Long context task rejects low-context (Fast-tier) models.
///
/// Policy sets min_tier=Reasoning to reject Fast-tier models. A Fast-tier
/// model produces NoCandidate when it is the only option.
#[test]
fn matrix_long_context_rejects_small_window() {
    let cfg = e2e_cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "fast-m",
        Some(ModelTier::Fast),
    )]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        min_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let fallback = PolicyFallback::Reject;

    let err = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Fast),
            &[],
            &requirements,
            &PolicyPreference::default(),
            &fallback,
            None,
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::NoCandidate(_)),
        "Fast model below min_tier=Reasoning should produce NoCandidate, got: {err:?}"
    );
}

/// Simple chat can use the Fast tier.
///
/// A Fast-tier model with no special capabilities is eligible when there are
/// no capability or tier requirements.
#[test]
fn matrix_simple_chat_uses_fast_tier() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "fast-chat", Some(ModelTier::Fast)).with_priority(0),
        ModelEntry::for_upstream("p1", "std-chat", Some(ModelTier::Standard)).with_priority(0),
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements::default();
    let preference = PolicyPreference {
        preferred_tier: Some(ModelTier::Fast),
        tier_weight: 1.0,
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
    };
    let fallback = PolicyFallback::Reject;

    let (candidates, _decision) = router
        .plan_with_policy(
            &reg,
            &Resolution::Tier(ModelTier::Fast),
            &[],
            &requirements,
            &preference,
            &fallback,
            None,
        )
        .unwrap();

    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].exposed_id, "p1-fast-chat");
}

/// Tool use always requires the Tools capability.
///
/// Policy requires Tools capability with strict mode. A model without tools
/// is rejected even if it is in the preferred tier.
#[test]
fn matrix_tool_use_requires_tools_capability() {
    let mut no_tools = ModelEntry::for_upstream("p1", "no-tools", Some(ModelTier::Standard));
    no_tools.capabilities = ModelCapabilities::default(); // all false
    let cfg = e2e_cfg_with(vec![no_tools]);
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
        "model without tools should be rejected, got: {err:?}"
    );
}
