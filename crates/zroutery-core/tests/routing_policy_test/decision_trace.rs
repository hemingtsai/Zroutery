use zroutery_core::config::{ModelCapabilities, ModelEntry, ModelTier};
use zroutery_core::error::Error;
use zroutery_core::ir::Capability;
use zroutery_core::policy::{DecisionReason, PolicyFallback, PolicyPreference, PolicyRequirements};
use zroutery_core::registry::Resolution;
use zroutery_core::router::Router;

use super::support::*;

// ========================================================================
// 11. Eligibility vs Scoring Separation
// ========================================================================

/// Ineligible candidates appear in the decision trace with no score, not
/// with a negative score. Eligability filtering removes them from the
/// candidate list rather than giving them bad scores.
#[test]
fn ineligible_candidate_has_no_score() {
    let mut no_caps = ModelEntry::for_upstream("p1", "no-caps", Some(ModelTier::Standard));
    no_caps.capabilities = ModelCapabilities::default();
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "good", Some(ModelTier::Standard)),
        no_caps,
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
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

    // Only the model with tools should be in the candidate list.
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].exposed_id, "p1-good");

    // The ineligible model should appear in the decision trace with no score.
    let rejected_decision = decision
        .candidates
        .iter()
        .find(|c| c.model_id == "p1-no-caps");
    assert!(
        rejected_decision.is_some(),
        "ineligible candidate must appear in decision trace"
    );
    let rejected = rejected_decision.unwrap();
    assert!(!rejected.eligible);
    assert!(
        rejected.score.is_none(),
        "ineligible candidate must have no score"
    );
    assert!(
        rejected.final_score.is_none(),
        "ineligible candidate must have no final_score"
    );
    assert!(
        rejected.rejection.is_some(),
        "ineligible candidate must have a rejection reason"
    );
}

/// When all candidates are eligible, all get scores in the decision trace.
#[test]
fn all_eligible_candidates_get_scores() {
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "m1", Some(ModelTier::Standard)).with_priority(0),
        ModelEntry::for_upstream("p1", "m2", Some(ModelTier::Standard)).with_priority(5),
        ModelEntry::for_upstream("p1", "m3", Some(ModelTier::Standard)).with_priority(10),
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements::default(); // no constraints
    let fallback = PolicyFallback::Reject;

    let (_candidates, decision) = router
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

    // All three candidates should have scores.
    assert_eq!(decision.candidates.len(), 3);
    for cd in &decision.candidates {
        assert!(cd.eligible, "all candidates should be eligible");
        assert!(
            cd.score.is_some(),
            "eligible candidate must have a score breakdown"
        );
        assert!(
            cd.final_score.is_some(),
            "eligible candidate must have a final_score"
        );
        assert!(
            cd.rejection.is_none(),
            "eligible candidate must have no rejection"
        );
    }
}

// ========================================================================
// 12. Decision Trace Tests
// ========================================================================

/// Decision trace records rejection reasons for ineligible candidates.
#[test]
fn decision_trace_records_rejections() {
    let mut no_caps = ModelEntry::for_upstream("p1", "reject-me", Some(ModelTier::Standard));
    no_caps.capabilities = ModelCapabilities::default(); // all false
    let cfg = e2e_cfg_with(vec![
        ModelEntry::for_upstream("p1", "good", Some(ModelTier::Standard)),
        no_caps,
    ]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    // Require Tools (good model has it from for_upstream; reject-me does not).
    let requirements = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        strict_capabilities: true,
        ..Default::default()
    };
    let fallback = PolicyFallback::Reject;

    let (_candidates, decision) = router
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

    let rejected = decision
        .candidates
        .iter()
        .find(|c| c.model_id == "p1-reject-me");
    assert!(rejected.is_some());
    let rejected = rejected.unwrap();
    assert!(!rejected.eligible);
    // The rejection string should contain the missing capability reason.
    let rejection = rejected.rejection.as_ref().unwrap();
    assert!(
        rejection.contains("missing_capability"),
        "rejection should mention missing_capability, got: {rejection}"
    );
}

/// Decision trace records score breakdown for eligible candidates.
#[test]
fn decision_trace_records_score_breakdown() {
    let cfg = e2e_cfg_with(vec![ModelEntry::for_upstream(
        "p1",
        "scored",
        Some(ModelTier::Standard),
    )]);
    let reg = e2e_reg(cfg);
    let router = Router::new();

    let requirements = PolicyRequirements::default();
    let fallback = PolicyFallback::Reject;

    let (_candidates, decision) = router
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

    let scored = decision.candidates.first().unwrap();
    assert!(scored.eligible);
    assert!(
        scored.score.is_some(),
        "eligible candidate must have score breakdown"
    );
    let breakdown = scored.score.as_ref().unwrap();
    // All dimension scores should be in [0.0, 1.0].
    assert!(breakdown.health >= 0.0 && breakdown.health <= 1.0);
    assert!(breakdown.latency >= 0.0 && breakdown.latency <= 1.0);
    assert!(breakdown.cost >= 0.0 && breakdown.cost <= 1.0);
    assert!(breakdown.priority >= 0.0 && breakdown.priority <= 1.0);
    assert!(breakdown.tier >= 0.0 && breakdown.tier <= 1.0);
    // Final score should be in [0.0, 1.0].
    let final_score = scored.final_score.unwrap();
    assert!(
        (0.0..=1.0).contains(&final_score),
        "final_score out of range: {final_score}"
    );
}

/// Decision trace records the fallback chain when escalation occurs.
#[test]
fn decision_trace_records_fallback_chain() {
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
    assert!(!router.allow_request("p1-std-m"));

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

    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].exposed_id, "p1-reas-m");
    // Fallback chain should record the original tier.
    assert!(
        !decision.fallback_chain.is_empty(),
        "fallback chain must record the escalation"
    );
    assert!(
        decision
            .fallback_chain
            .contains(&"standard-class".to_string()),
        "fallback chain should mention the original tier"
    );
    // Reason should indicate escalation.
    assert!(
        matches!(decision.reason, DecisionReason::Escalated { .. }),
        "reason should be Escalated, got: {:?}",
        decision.reason
    );
}
