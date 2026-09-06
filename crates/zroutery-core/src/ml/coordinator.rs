//! ML Coordinator — the decision layer that translates predictions into routing actions.
//!
//! The Coordinator is the "brake system" of ML routing. It ensures:
//! - Eligibility > Session constraints > Utility > Exploration
//! - ML never bypasses hard constraints
//! - Session pinning is respected
//! - Switching has hysteresis to prevent thrashing

use serde::{Deserialize, Serialize};

use super::reward::{
    Action, ActionGuard, PredictionBundle, RewardPolicy, UtilityBreakdown, compute_utility,
};
use crate::session::SessionRoutingMode;

/// Decision from the Coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub action: RoutingAction,
    pub selected_candidate: String,
    pub utility: UtilityBreakdown,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingAction {
    Keep,
    Switch,
    Explore,
}

/// Configuration for the Coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorConfig {
    pub reward_policy: RewardPolicy,
    /// Minimum utility delta to trigger a switch (hysteresis).
    pub switch_threshold: f64,
    /// Maximum switches per session to prevent thrashing.
    pub max_switches_per_session: u32,
    /// Whether to allow exploration (trying new candidates for learning).
    pub exploration_enabled: bool,
    /// Probability of exploration when utility is close.
    pub exploration_probability: f64,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        CoordinatorConfig {
            reward_policy: RewardPolicy::default(),
            switch_threshold: 0.1,
            max_switches_per_session: 5,
            exploration_enabled: false,
            exploration_probability: 0.05,
        }
    }
}

pub struct Coordinator {
    config: CoordinatorConfig,
}

impl Coordinator {
    pub fn new(config: CoordinatorConfig) -> Self {
        Self { config }
    }

    /// Make a routing decision given candidate predictions.
    pub fn decide(
        &self,
        current_candidate: &str,
        candidates: &[PredictionBundle],
        session_mode: SessionRoutingMode,
        session_switch_count: u32,
        is_fallback: bool,
    ) -> RoutingDecision {
        // 1. Session constraints first
        let session_action = ActionGuard::decide(
            current_candidate,
            candidates
                .first()
                .map(|c| c.candidate_model.as_str())
                .unwrap_or(""),
            session_mode,
            candidates
                .first()
                .map(|c| c.success.confidence)
                .unwrap_or(0.0),
        );
        if session_action == Action::Keep {
            return RoutingDecision {
                action: RoutingAction::Keep,
                selected_candidate: current_candidate.to_string(),
                utility: UtilityBreakdown::default(),
                reason: "session constraint: pinned/sticky".into(),
            };
        }

        // 2. Switch rate limit
        if session_switch_count >= self.config.max_switches_per_session {
            return RoutingDecision {
                action: RoutingAction::Keep,
                selected_candidate: current_candidate.to_string(),
                utility: UtilityBreakdown::default(),
                reason: "switch rate limit reached".into(),
            };
        }

        // 3. Compute utility for each candidate
        let utilities: Vec<(String, UtilityBreakdown)> = candidates
            .iter()
            .map(|bundle| {
                let util = compute_utility(
                    bundle,
                    &self.config.reward_policy,
                    is_fallback,
                    session_switch_count,
                );
                (bundle.candidate_model.clone(), util)
            })
            .collect();

        // 4. Find best candidate
        let best = utilities
            .iter()
            .max_by(|a, b| a.1.total.partial_cmp(&b.1.total).unwrap());
        let current_utility = utilities.iter().find(|(id, _)| id == current_candidate);

        match (best, current_utility) {
            (Some((best_id, best_util)), Some((_, current_util))) => {
                let delta = best_util.total - current_util.total;
                if best_id != current_candidate && delta > self.config.switch_threshold {
                    RoutingDecision {
                        action: RoutingAction::Switch,
                        selected_candidate: best_id.clone(),
                        utility: best_util.clone(),
                        reason: format!(
                            "utility delta {:.3} > threshold {:.3}",
                            delta, self.config.switch_threshold
                        ),
                    }
                } else {
                    RoutingDecision {
                        action: RoutingAction::Keep,
                        selected_candidate: current_candidate.to_string(),
                        utility: current_util.clone(),
                        reason: "utility delta below threshold".into(),
                    }
                }
            }
            (Some((best_id, best_util)), None) => RoutingDecision {
                action: RoutingAction::Switch,
                selected_candidate: best_id.clone(),
                utility: best_util.clone(),
                reason: "no current candidate, selecting best".into(),
            },
            _ => RoutingDecision {
                action: RoutingAction::Keep,
                selected_candidate: current_candidate.to_string(),
                utility: UtilityBreakdown::default(),
                reason: "no candidates available".into(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::model::Prediction;

    fn make_prediction(value: f64, confidence: f64) -> Prediction {
        Prediction {
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
    fn keep_when_session_pinned() {
        let coordinator = Coordinator::new(CoordinatorConfig::default());
        let candidates = vec![
            make_bundle("model-a", 0.9, 500.0, 200.0, 0.01),
            make_bundle("model-b", 0.99, 100.0, 50.0, 0.005),
        ];

        let decision = coordinator.decide("model-a", &candidates, SessionRoutingMode::Pinned, 0, false);
        assert_eq!(decision.action, RoutingAction::Keep);
        assert_eq!(decision.selected_candidate, "model-a");
        assert!(decision.reason.contains("pinned"));
    }

    #[test]
    fn switch_when_utility_delta_above_threshold() {
        let coordinator = Coordinator::new(CoordinatorConfig::default());
        // model-b (the better one) must be first so ActionGuard sees it as predicted_best
        let candidates = vec![
            make_bundle("model-b", 0.95, 200.0, 100.0, 0.01),
            make_bundle("model-a", 0.3, 4000.0, 1500.0, 0.9),
        ];

        let decision = coordinator.decide("model-a", &candidates, SessionRoutingMode::Free, 0, false);
        assert_eq!(decision.action, RoutingAction::Switch);
        assert_eq!(decision.selected_candidate, "model-b");
        assert!(decision.reason.contains("utility delta"));
    }

    #[test]
    fn keep_when_utility_delta_below_threshold() {
        let coordinator = Coordinator::new(CoordinatorConfig::default());
        // model-b is only slightly better — below the 0.1 threshold
        // model-b must be first so ActionGuard sees it as predicted_best
        let candidates = vec![
            make_bundle("model-b", 0.9, 480.0, 190.0, 0.02),
            make_bundle("model-a", 0.9, 500.0, 200.0, 0.02),
        ];

        let decision = coordinator.decide("model-a", &candidates, SessionRoutingMode::Free, 0, false);
        assert_eq!(decision.action, RoutingAction::Keep);
        assert_eq!(decision.selected_candidate, "model-a");
        assert!(decision.reason.contains("below threshold"));
    }

    #[test]
    fn switch_rate_limit_reached() {
        let config = CoordinatorConfig {
            max_switches_per_session: 3,
            ..CoordinatorConfig::default()
        };
        let coordinator = Coordinator::new(config);
        // model-b must be first so ActionGuard doesn't short-circuit with Keep
        let candidates = vec![
            make_bundle("model-b", 0.99, 100.0, 50.0, 0.001),
            make_bundle("model-a", 0.3, 4000.0, 1500.0, 0.9),
        ];

        // Already switched 3 times (at limit)
        let decision = coordinator.decide("model-a", &candidates, SessionRoutingMode::Free, 3, false);
        assert_eq!(decision.action, RoutingAction::Keep);
        assert_eq!(decision.selected_candidate, "model-a");
        assert!(decision.reason.contains("rate limit"));
    }

    #[test]
    fn no_candidates_keep() {
        let coordinator = Coordinator::new(CoordinatorConfig::default());
        let candidates: Vec<PredictionBundle> = vec![];

        let decision = coordinator.decide("model-a", &candidates, SessionRoutingMode::Free, 0, false);
        assert_eq!(decision.action, RoutingAction::Keep);
        assert_eq!(decision.selected_candidate, "model-a");
        assert!(decision.reason.contains("no candidates"));
    }

    #[test]
    fn coordinator_default_config() {
        let config = CoordinatorConfig::default();
        assert!((config.switch_threshold - 0.1).abs() < 1e-10);
        assert_eq!(config.max_switches_per_session, 5);
        assert!(!config.exploration_enabled);
        assert!((config.exploration_probability - 0.05).abs() < 1e-10);
    }

    #[test]
    fn sticky_low_confidence_forces_keep() {
        let coordinator = Coordinator::new(CoordinatorConfig::default());
        // High confidence predictions, but Sticky + low confidence on first candidate
        let candidates = vec![
            PredictionBundle {
                candidate_model: "model-a".to_string(),
                candidate_provider: "test".to_string(),
                success: make_prediction(0.99, 0.3), // low confidence
                latency: make_prediction(100.0, 0.3),
                ttft: make_prediction(50.0, 0.3),
                cost: make_prediction(0.01, 0.3),
            },
        ];

        let decision = coordinator.decide("model-a", &candidates, SessionRoutingMode::Sticky, 0, false);
        assert_eq!(decision.action, RoutingAction::Keep);
        assert!(decision.reason.contains("session constraint"));
    }
}
