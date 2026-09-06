//! Reward / utility signals for ML routing.
//!
//! Computes scalar reward signals for individual attempts and complete
//! requests.  The [`RewardComputer`] applies a configurable [`RewardPolicy`]
//! so that success, latency, cost, fallback, and switching penalties can be
//! tuned independently.
//!
//! [`ActionGuard`] translates session constraints and model confidence into
//! a routing [`Action`] (Keep / Switch / Explore).

use serde::{Deserialize, Serialize};

use crate::session::SessionRoutingMode;

// ---------------------------------------------------------------------------
// AttemptReward / RequestReward — reward signals
// ---------------------------------------------------------------------------

/// Reward signal for a single attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptReward {
    pub attempt_id: String,
    /// 1.0 for success, -1.0 for failure (scaled by policy weight).
    pub success_reward: f64,
    /// Negative penalty for slow responses.
    pub latency_reward: f64,
    /// Negative penalty for expensive responses.
    pub cost_reward: f64,
    /// Penalty for being a fallback attempt.
    pub fallback_penalty: f64,
    /// Sum of all components.
    pub total: f64,
}

/// Aggregated reward for a complete request (all attempts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestReward {
    pub outcome_id: String,
    pub attempt_rewards: Vec<AttemptReward>,
    /// Penalty for switching candidates.
    pub switch_cost: f64,
    /// Penalty for low-confidence predictions.
    pub uncertainty_penalty: f64,
    /// Sum of all components.
    pub total: f64,
}

// ---------------------------------------------------------------------------
// RewardPolicy — configurable weights
// ---------------------------------------------------------------------------

/// How rewards are computed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardPolicy {
    pub success_weight: f64,
    pub latency_weight: f64,
    pub cost_weight: f64,
    pub fallback_penalty: f64,
    pub switch_cost: f64,
    pub uncertainty_weight: f64,
}

impl Default for RewardPolicy {
    fn default() -> Self {
        RewardPolicy {
            success_weight: 1.0,
            latency_weight: 0.3,
            cost_weight: 0.1,
            fallback_penalty: -0.5,
            switch_cost: -0.2,
            uncertainty_weight: 0.1,
        }
    }
}

// ---------------------------------------------------------------------------
// RewardComputer — computes reward signals
// ---------------------------------------------------------------------------

/// Computes reward signals from outcomes using a [`RewardPolicy`].
pub struct RewardComputer {
    policy: RewardPolicy,
}

impl RewardComputer {
    pub fn new(policy: RewardPolicy) -> Self {
        Self { policy }
    }

    /// Compute the reward for a single attempt.
    ///
    /// * `success` — whether the attempt succeeded.
    /// * `latency_ms` — measured latency in milliseconds.
    /// * `cost` — measured cost (0.0–1.0 normalized).
    /// * `is_fallback` — whether this attempt was a fallback.
    pub fn compute_attempt(
        &self,
        success: bool,
        latency_ms: f64,
        cost: f64,
        is_fallback: bool,
    ) -> AttemptReward {
        let success_reward = if success {
            self.policy.success_weight
        } else {
            -self.policy.success_weight
        };
        let latency_reward =
            -self.policy.latency_weight * (latency_ms / 1000.0).min(1.0);
        let cost_reward = -self.policy.cost_weight * (cost / 1.0).min(1.0);
        let fallback_penalty = if is_fallback {
            self.policy.fallback_penalty
        } else {
            0.0
        };
        let total = success_reward + latency_reward + cost_reward + fallback_penalty;
        AttemptReward {
            attempt_id: String::new(),
            success_reward,
            latency_reward,
            cost_reward,
            fallback_penalty,
            total,
        }
    }

    /// Compute the aggregated reward for a complete request.
    ///
    /// * `attempts` — individual attempt rewards.
    /// * `switch_count` — number of candidate switches that occurred.
    /// * `confidence` — model confidence in [0, 1].
    pub fn compute_request(
        &self,
        attempts: &[AttemptReward],
        switch_count: u32,
        confidence: f64,
    ) -> RequestReward {
        let attempt_total: f64 = attempts.iter().map(|a| a.total).sum();
        let switch_cost = self.policy.switch_cost * switch_count as f64;
        let uncertainty_penalty =
            -self.policy.uncertainty_weight * (1.0 - confidence);
        let total = attempt_total + switch_cost + uncertainty_penalty;
        RequestReward {
            outcome_id: String::new(),
            attempt_rewards: attempts.to_vec(),
            switch_cost,
            uncertainty_penalty,
            total,
        }
    }
}

// ---------------------------------------------------------------------------
// Action — routing decision
// ---------------------------------------------------------------------------

/// Routing action decided by the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Stay on the current candidate.
    Keep,
    /// Switch to a different candidate.
    Switch,
    /// Try a new candidate for exploration / learning.
    Explore,
}

// ---------------------------------------------------------------------------
// ActionGuard — prevents unsafe actions
// ---------------------------------------------------------------------------

/// Decides which [`Action`] to take given session constraints and confidence.
pub struct ActionGuard;

impl ActionGuard {
    /// Determine what action to take given current state.
    ///
    /// Session constraints override ML preference:
    /// - `Pinned` always forces `Keep`.
    /// - `Sticky` with low confidence (< 0.8) forces `Keep`.
    /// - Otherwise the predicted best candidate and confidence decide.
    pub fn decide(
        current_candidate: &str,
        predicted_best: &str,
        session_mode: SessionRoutingMode,
        confidence: f64,
    ) -> Action {
        // Session constraints override ML preference
        match session_mode {
            SessionRoutingMode::Pinned => return Action::Keep,
            SessionRoutingMode::Sticky if confidence < 0.8 => return Action::Keep,
            _ => {}
        }
        if current_candidate == predicted_best {
            Action::Keep
        } else if confidence > 0.7 {
            Action::Switch
        } else {
            Action::Explore
        }
    }
}

// ---------------------------------------------------------------------------
// PredictionBundle / UtilityBreakdown — utility scoring
// ---------------------------------------------------------------------------

/// Predictions from all 4 models for a single candidate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PredictionBundle {
    pub candidate_model: String,
    pub candidate_provider: String,
    pub success: crate::ml::model::Prediction,
    pub latency: crate::ml::model::Prediction,
    pub ttft: crate::ml::model::Prediction,
    pub cost: crate::ml::model::Prediction,
}

/// Utility breakdown from a prediction bundle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UtilityBreakdown {
    pub success: f64,
    pub latency: f64,
    pub ttft: f64,
    pub cost: f64,
    pub fallback: f64,
    pub uncertainty: f64,
    pub switch_cost: f64,
    pub total: f64,
}

impl Default for UtilityBreakdown {
    fn default() -> Self {
        UtilityBreakdown {
            success: 0.0,
            latency: 0.0,
            ttft: 0.0,
            cost: 0.0,
            fallback: 0.0,
            uncertainty: 0.0,
            switch_cost: 0.0,
            total: 0.0,
        }
    }
}

/// Compute utility from a prediction bundle.
pub fn compute_utility(
    bundle: &PredictionBundle,
    policy: &RewardPolicy,
    is_fallback: bool,
    switch_count: u32,
) -> UtilityBreakdown {
    let success = bundle.success.value * policy.success_weight;
    let latency = -policy.latency_weight * (bundle.latency.value / 5000.0).min(1.0);
    let ttft = -policy.latency_weight * 0.5 * (bundle.ttft.value / 2000.0).min(1.0);
    let cost = -policy.cost_weight * (bundle.cost.value / 1.0).min(1.0);
    let fallback = if is_fallback {
        policy.fallback_penalty
    } else {
        0.0
    };
    let avg_confidence = (bundle.success.confidence
        + bundle.latency.confidence
        + bundle.ttft.confidence
        + bundle.cost.confidence)
        / 4.0;
    let uncertainty = -policy.uncertainty_weight * (1.0 - avg_confidence);
    let switch = policy.switch_cost * switch_count as f64;
    let total = success + latency + ttft + cost + fallback + uncertainty + switch;
    UtilityBreakdown {
        success,
        latency,
        ttft,
        cost,
        fallback,
        uncertainty,
        switch_cost: switch,
        total,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- RewardComputer tests --

    #[test]
    fn success_reward_greater_than_failure() {
        let computer = RewardComputer::new(RewardPolicy::default());
        let success = computer.compute_attempt(true, 100.0, 0.01, false);
        let failure = computer.compute_attempt(false, 100.0, 0.01, false);
        assert!(
            success.total > failure.total,
            "success total ({}) should be > failure total ({})",
            success.total,
            failure.total,
        );
    }

    #[test]
    fn fallback_penalty_applied() {
        let computer = RewardComputer::new(RewardPolicy::default());
        let normal = computer.compute_attempt(true, 100.0, 0.01, false);
        let fallback = computer.compute_attempt(true, 100.0, 0.01, true);
        assert!(
            fallback.total < normal.total,
            "fallback total ({}) should be < normal total ({})",
            fallback.total,
            normal.total,
        );
        assert!(
            (fallback.fallback_penalty - (-0.5)).abs() < 1e-10,
            "fallback_penalty should be -0.5, got {}",
            fallback.fallback_penalty,
        );
        assert!(
            normal.fallback_penalty.abs() < 1e-10,
            "normal fallback_penalty should be 0.0, got {}",
            normal.fallback_penalty,
        );
    }

    #[test]
    fn switch_cost_accumulates() {
        let computer = RewardComputer::new(RewardPolicy::default());
        let attempt = computer.compute_attempt(true, 100.0, 0.01, false);

        let no_switch = computer.compute_request(&[attempt.clone()], 0, 1.0);
        let one_switch = computer.compute_request(&[attempt.clone()], 1, 1.0);
        let two_switches = computer.compute_request(&[attempt.clone()], 2, 1.0);

        assert!(
            no_switch.total > one_switch.total,
            "no_switch ({}) should be > one_switch ({})",
            no_switch.total,
            one_switch.total,
        );
        assert!(
            one_switch.total > two_switches.total,
            "one_switch ({}) should be > two_switches ({})",
            one_switch.total,
            two_switches.total,
        );
        // Each switch should add switch_cost = -0.2
        let delta = one_switch.total - no_switch.total;
        assert!(
            (delta - (-0.2)).abs() < 1e-10,
            "switch delta should be -0.2, got {}",
            delta,
        );
    }

    #[test]
    fn uncertainty_penalty_for_low_confidence() {
        let computer = RewardComputer::new(RewardPolicy::default());
        let attempt = computer.compute_attempt(true, 100.0, 0.01, false);

        let high_conf = computer.compute_request(&[attempt.clone()], 0, 1.0);
        let low_conf = computer.compute_request(&[attempt.clone()], 0, 0.0);

        assert!(
            high_conf.total > low_conf.total,
            "high_conf ({}) should be > low_conf ({})",
            high_conf.total,
            low_conf.total,
        );
        // confidence=1.0 -> penalty = -0.1 * (1-1) = 0
        assert!(
            high_conf.uncertainty_penalty.abs() < 1e-10,
            "high_conf uncertainty should be ~0, got {}",
            high_conf.uncertainty_penalty,
        );
        // confidence=0.0 -> penalty = -0.1 * (1-0) = -0.1
        assert!(
            (low_conf.uncertainty_penalty - (-0.1)).abs() < 1e-10,
            "low_conf uncertainty should be -0.1, got {}",
            low_conf.uncertainty_penalty,
        );
    }

    // -- ActionGuard tests --

    #[test]
    fn pinned_session_always_keep() {
        let action = ActionGuard::decide("model-a", "model-b", SessionRoutingMode::Pinned, 0.99);
        assert_eq!(action, Action::Keep, "Pinned should always Keep");
    }

    #[test]
    fn sticky_low_confidence_keep() {
        let action = ActionGuard::decide("model-a", "model-b", SessionRoutingMode::Sticky, 0.5);
        assert_eq!(
            action,
            Action::Keep,
            "Sticky + low confidence should Keep"
        );
    }

    #[test]
    fn high_confidence_different_candidate_switch() {
        let action = ActionGuard::decide("model-a", "model-b", SessionRoutingMode::Free, 0.9);
        assert_eq!(
            action,
            Action::Switch,
            "high confidence + different candidate should Switch"
        );
    }

    #[test]
    fn same_candidate_keep() {
        let action = ActionGuard::decide("model-a", "model-a", SessionRoutingMode::Free, 0.9);
        assert_eq!(action, Action::Keep, "same candidate should Keep");
    }

    #[test]
    fn low_confidence_different_candidate_explore() {
        let action = ActionGuard::decide("model-a", "model-b", SessionRoutingMode::Free, 0.5);
        assert_eq!(
            action,
            Action::Explore,
            "low confidence + different candidate should Explore"
        );
    }

    #[test]
    fn sticky_high_confidence_different_candidate_switch() {
        // Sticky with confidence >= 0.8 does not force Keep
        let action = ActionGuard::decide("model-a", "model-b", SessionRoutingMode::Sticky, 0.9);
        assert_eq!(
            action,
            Action::Switch,
            "Sticky + high confidence + different candidate should Switch"
        );
    }

    // -- RewardPolicy default test --

    #[test]
    fn reward_policy_default_values() {
        let policy = RewardPolicy::default();
        assert!((policy.success_weight - 1.0).abs() < 1e-10);
        assert!((policy.latency_weight - 0.3).abs() < 1e-10);
        assert!((policy.cost_weight - 0.1).abs() < 1e-10);
        assert!((policy.fallback_penalty - (-0.5)).abs() < 1e-10);
        assert!((policy.switch_cost - (-0.2)).abs() < 1e-10);
        assert!((policy.uncertainty_weight - 0.1).abs() < 1e-10);
    }

    // -- Serde round-trip tests --

    #[test]
    fn attempt_reward_serde_round_trip() {
        let computer = RewardComputer::new(RewardPolicy::default());
        let reward = computer.compute_attempt(true, 200.0, 0.05, true);
        let json = serde_json::to_string(&reward).unwrap();
        let restored: AttemptReward = serde_json::from_str(&json).unwrap();
        assert!((restored.total - reward.total).abs() < 1e-10);
        assert!((restored.success_reward - reward.success_reward).abs() < 1e-10);
        assert!((restored.fallback_penalty - reward.fallback_penalty).abs() < 1e-10);
    }

    #[test]
    fn request_reward_serde_round_trip() {
        let computer = RewardComputer::new(RewardPolicy::default());
        let attempt = computer.compute_attempt(true, 100.0, 0.01, false);
        let reward = computer.compute_request(&[attempt], 1, 0.8);
        let json = serde_json::to_string(&reward).unwrap();
        let restored: RequestReward = serde_json::from_str(&json).unwrap();
        assert!((restored.total - reward.total).abs() < 1e-10);
        assert_eq!(restored.attempt_rewards.len(), 1);
    }

    // -- PredictionBundle / compute_utility tests --

    fn make_prediction(value: f64, confidence: f64) -> crate::ml::model::Prediction {
        crate::ml::model::Prediction {
            value,
            confidence,
            sample_count: 100,
            cold: false,
        }
    }

    fn make_bundle(model: &str, success: f64, latency: f64, ttft: f64, cost: f64) -> PredictionBundle {
        PredictionBundle {
            candidate_model: model.to_string(),
            candidate_provider: "test-provider".to_string(),
            success: make_prediction(success, 0.9),
            latency: make_prediction(latency, 0.8),
            ttft: make_prediction(ttft, 0.7),
            cost: make_prediction(cost, 0.6),
        }
    }

    #[test]
    fn prediction_bundle_construction() {
        let bundle = make_bundle("gpt-4", 0.95, 300.0, 100.0, 0.02);
        assert_eq!(bundle.candidate_model, "gpt-4");
        assert_eq!(bundle.candidate_provider, "test-provider");
        assert!((bundle.success.value - 0.95).abs() < 1e-10);
        assert!((bundle.latency.value - 300.0).abs() < 1e-10);
        assert!((bundle.ttft.value - 100.0).abs() < 1e-10);
        assert!((bundle.cost.value - 0.02).abs() < 1e-10);
    }

    #[test]
    fn prediction_bundle_serde_round_trip() {
        let bundle = make_bundle("claude-3", 0.85, 200.0, 80.0, 0.01);
        let json = serde_json::to_string(&bundle).unwrap();
        let restored: PredictionBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.candidate_model, "claude-3");
        assert!((restored.success.value - 0.85).abs() < 1e-10);
    }

    #[test]
    fn compute_utility_known_values() {
        let policy = RewardPolicy::default();
        let bundle = make_bundle("model-a", 0.9, 2500.0, 1000.0, 0.5);

        let utility = compute_utility(&bundle, &policy, false, 0);

        // success = 0.9 * 1.0 = 0.9
        assert!((utility.success - 0.9).abs() < 1e-10);
        // latency = -0.3 * (2500/5000).min(1.0) = -0.3 * 0.5 = -0.15
        assert!((utility.latency - (-0.15)).abs() < 1e-10);
        // ttft = -0.3 * 0.5 * (1000/2000).min(1.0) = -0.15 * 0.5 = -0.075
        assert!((utility.ttft - (-0.075)).abs() < 1e-10);
        // cost = -0.1 * (0.5/1.0).min(1.0) = -0.05
        assert!((utility.cost - (-0.05)).abs() < 1e-10);
        // fallback = 0 (not fallback)
        assert!((utility.fallback).abs() < 1e-10);
        // avg_confidence = (0.9 + 0.8 + 0.7 + 0.6) / 4 = 0.75
        // uncertainty = -0.1 * (1 - 0.75) = -0.025
        assert!((utility.uncertainty - (-0.025)).abs() < 1e-10);
        // switch_cost = 0 (switch_count=0)
        assert!((utility.switch_cost).abs() < 1e-10);
        // total = 0.9 + (-0.15) + (-0.075) + (-0.05) + 0 + (-0.025) + 0 = 0.6
        assert!((utility.total - 0.6).abs() < 1e-10);
    }

    #[test]
    fn compute_utility_with_fallback() {
        let policy = RewardPolicy::default();
        let bundle = make_bundle("model-b", 0.8, 1000.0, 500.0, 0.1);

        let utility = compute_utility(&bundle, &policy, true, 0);
        // fallback_penalty = -0.5
        assert!((utility.fallback - (-0.5)).abs() < 1e-10);
    }

    #[test]
    fn compute_utility_with_switches() {
        let policy = RewardPolicy::default();
        let bundle = make_bundle("model-c", 0.8, 1000.0, 500.0, 0.1);

        let utility = compute_utility(&bundle, &policy, false, 3);
        // switch_cost = -0.2 * 3 = -0.6
        assert!((utility.switch_cost - (-0.6)).abs() < 1e-10);
    }

    #[test]
    fn utility_breakdown_total_equals_sum_of_components() {
        let policy = RewardPolicy::default();
        let bundle = make_bundle("model-d", 0.7, 2000.0, 800.0, 0.3);

        let utility = compute_utility(&bundle, &policy, true, 2);
        let sum = utility.success
            + utility.latency
            + utility.ttft
            + utility.cost
            + utility.fallback
            + utility.uncertainty
            + utility.switch_cost;
        assert!(
            (utility.total - sum).abs() < 1e-10,
            "total ({}) should equal sum of components ({})",
            utility.total,
            sum,
        );
    }

    #[test]
    fn utility_breakdown_serde_round_trip() {
        let policy = RewardPolicy::default();
        let bundle = make_bundle("model-e", 0.85, 1500.0, 600.0, 0.05);
        let utility = compute_utility(&bundle, &policy, false, 1);
        let json = serde_json::to_string(&utility).unwrap();
        let restored: UtilityBreakdown = serde_json::from_str(&json).unwrap();
        assert!((restored.total - utility.total).abs() < 1e-10);
    }

    #[test]
    fn utility_breakdown_default_is_zero() {
        let utility = UtilityBreakdown::default();
        assert!((utility.total).abs() < 1e-10);
        assert!((utility.success).abs() < 1e-10);
        assert!((utility.latency).abs() < 1e-10);
        assert!((utility.ttft).abs() < 1e-10);
        assert!((utility.cost).abs() < 1e-10);
        assert!((utility.fallback).abs() < 1e-10);
        assert!((utility.uncertainty).abs() < 1e-10);
        assert!((utility.switch_cost).abs() < 1e-10);
    }
}
