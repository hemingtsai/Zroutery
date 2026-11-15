use zroutery_core::config::ModelTier;
use zroutery_core::policy::{score_candidate, PolicyPreference};

use super::support::*;

// ========================================================================
// 5. Scoring: prefers healthy, low latency, preferred tier
// ========================================================================

#[test]
fn scoring_perfect_candidate_scores_high() {
    let pref = PolicyPreference::default();
    let ctx = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert!(
        s.total_score >= 0.85,
        "expected >=0.85, got {}",
        s.total_score
    );
    assert_eq!(s.breakdown.health, 1.0);
    assert!((s.breakdown.latency - 1.0).abs() < 0.001);
    assert!((s.breakdown.cost - 1.0).abs() < 0.001);
    assert_eq!(s.breakdown.priority, 1.0);
}

#[test]
fn scoring_prefers_healthy_over_unhealthy() {
    let pref = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let healthy = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let unhealthy = make_scoring_ctx(0.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s_h = score_candidate(&pref, &healthy);
    let s_u = score_candidate(&pref, &unhealthy);
    assert!(
        s_h.total_score > s_u.total_score,
        "healthy {} should beat unhealthy {}",
        s_h.total_score,
        s_u.total_score
    );
}

#[test]
fn scoring_prefers_low_latency() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 1.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let fast = make_scoring_ctx(1.0, 200.0, Some(1.0), 0, Some(ModelTier::Standard));
    let slow = make_scoring_ctx(1.0, 2000.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s_f = score_candidate(&pref, &fast);
    let s_s = score_candidate(&pref, &slow);
    assert!(
        s_f.total_score > s_s.total_score,
        "fast {} should beat slow {}",
        s_f.total_score,
        s_s.total_score
    );
}

#[test]
fn scoring_prefers_cheaper_cost() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let cheap = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let expensive = make_scoring_ctx(1.0, 100.0, Some(50.0), 0, Some(ModelTier::Standard));
    let s_c = score_candidate(&pref, &cheap);
    let s_e = score_candidate(&pref, &expensive);
    assert!(s_c.total_score > s_e.total_score);
}

#[test]
fn scoring_prefers_preferred_tier() {
    let pref = PolicyPreference {
        preferred_tier: Some(ModelTier::Standard),
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 1.0,
    };
    let exact = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let close = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Reasoning));
    let far = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Frontier));
    let s_exact = score_candidate(&pref, &exact);
    let s_close = score_candidate(&pref, &close);
    let s_far = score_candidate(&pref, &far);
    assert!(s_exact.total_score > s_close.total_score);
    assert!(s_close.total_score > s_far.total_score);
}

#[test]
fn scoring_prefers_lower_priority_number() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 1.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let high_prio = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let low_prio = make_scoring_ctx(1.0, 100.0, Some(1.0), 50, Some(ModelTier::Standard));
    let s_h = score_candidate(&pref, &high_prio);
    let s_l = score_candidate(&pref, &low_prio);
    assert!(s_h.total_score > s_l.total_score);
}

#[test]
fn scoring_unmeasured_latency_gets_benefit_of_doubt() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 1.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let unmeasured = make_scoring_ctx(1.0, 0.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &unmeasured);
    assert!(
        (s.breakdown.latency - 1.0).abs() < 0.001,
        "unmeasured should get full latency score, got {}",
        s.breakdown.latency
    );
}

#[test]
fn scoring_unknown_cost_is_neutral() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let unknown = make_scoring_ctx(1.0, 100.0, None, 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &unknown);
    assert!((s.breakdown.cost - 0.5).abs() < 0.001);
}

#[test]
fn scoring_weights_are_normalized() {
    let pref = PolicyPreference {
        preferred_tier: None,
        health_weight: 5.0,
        latency_weight: 5.0,
        cost_weight: 5.0,
        priority_weight: 5.0,
        tier_weight: 5.0,
    };
    let ctx = make_scoring_ctx(0.5, 500.0, Some(5.0), 25, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert!(
        s.total_score >= 0.0 && s.total_score <= 1.0,
        "score out of range: {}",
        s.total_score
    );
}

#[test]
fn scoring_zero_weights_produce_zero() {
    let pref = PolicyPreference {
        preferred_tier: None,
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
    };
    let ctx = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert_eq!(s.total_score, 0.0);
}
