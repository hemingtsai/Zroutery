//! DecisionEngine — the sanitized, panic-free decision core for ML routing.
//!
//! The engine wraps the frozen Coordinator decision semantics in a
//! sanitization layer:
//!
//! 1. Every input candidate is classified into a [`CandidateOutcome`]:
//!    rejected as `"ineligible"`, `"non-finite prediction"` or
//!    `"non-finite utility"`, or admitted as valid.
//! 2. The decision is computed over the *valid* candidates only, reproducing
//!    `Coordinator::decide` exactly: session constraints first, then the
//!    switch rate limit, then the utility comparison with hysteresis
//!    (ties resolve to the last maximal candidate, as with `max_by`).
//! 3. Non-finite values never reach the decision logic and never escape in
//!    the output evidence, so no code path can panic on a NaN comparison
//!    (the Coordinator's `partial_cmp().unwrap()` is replaced by
//!    `f64::total_cmp`).
//!
//! The single deliberate divergence from the Coordinator: when no candidate
//! is valid, the terminal reason is `"no valid candidates"` (the
//! Coordinator's equivalent arm says `"no candidates available"`). Session
//! constraints and the switch rate limit still take precedence over that
//! terminal case, exactly as in the Coordinator.
//!
//! The engine is pure: no I/O, no locks, no clocks. Given the same config,
//! reward policy and input it always produces the same output, which makes
//! decisions replayable.

use serde::{Deserialize, Serialize};

use super::coordinator::{CoordinatorConfig, RoutingAction, RoutingDecision};
use super::model::Prediction;
use super::reward::{
    Action, ActionGuard, PredictionBundle, RewardPolicy, UtilityBreakdown, compute_utility,
};
use crate::session::SessionRoutingMode;

// ---------------------------------------------------------------------------
// EngineCandidate / EngineInput — engine input
// ---------------------------------------------------------------------------

/// A candidate handed to the engine: its identity key plus the prediction
/// bundle used to score it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineCandidate {
    /// Identity key: the model id (same convention as
    /// [`PredictionBundle::candidate_model`]). The engine keys all decisions
    /// and evidence on this field.
    pub candidate_id: String,
    /// Predictions for the candidate.
    pub bundle: PredictionBundle,
    /// Hard eligibility flag; ineligible candidates never join the decision
    /// set, regardless of their predictions.
    pub eligible: bool,
}

/// Borrowed input for [`DecisionEngine::decide`].
#[derive(Debug, Clone, Copy)]
pub struct EngineInput<'a> {
    /// Identity key of the candidate the request currently sits on.
    pub current_candidate: &'a str,
    /// Candidates to consider, in priority order. The first *valid* candidate
    /// plays the role the Coordinator gives to `candidates.first()`.
    pub candidates: &'a [EngineCandidate],
    /// Session routing mode (session constraints outrank utility).
    pub session_mode: SessionRoutingMode,
    /// Switches already performed in this session (rate limit + utility).
    pub session_switch_count: u32,
    /// Whether the current attempt is a fallback (fallback penalty).
    pub is_fallback: bool,
}

// ---------------------------------------------------------------------------
// CandidateOutcome / EngineOutput — engine output
// ---------------------------------------------------------------------------

/// Per-candidate evidence produced by the sanitization pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateOutcome {
    /// Identity key, mirroring [`EngineCandidate::candidate_id`].
    pub candidate_id: String,
    /// The eligibility flag from the input.
    pub eligible: bool,
    /// `false` when the candidate was rejected before joining the decision set.
    pub valid: bool,
    /// Why the candidate was rejected: `"ineligible"`, `"non-finite
    /// prediction"` or `"non-finite utility"`; `None` for valid candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
    /// The candidate's utility. Always finite: rejected candidates carry the
    /// default (all-zero) breakdown so no non-finite value ever escapes.
    pub utility: UtilityBreakdown,
}

/// Result of [`DecisionEngine::decide`]: the routing decision plus
/// per-candidate evidence in input order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineOutput {
    /// The decision, identical to what the Coordinator would produce over the
    /// valid candidates.
    pub decision: RoutingDecision,
    /// One entry per input candidate, in input order.
    pub candidates: Vec<CandidateOutcome>,
}

// ---------------------------------------------------------------------------
// DecisionEngine
// ---------------------------------------------------------------------------

/// Panic-free decision core reproducing the Coordinator's frozen semantics
/// over a sanitized candidate set.
#[derive(Debug, Clone)]
pub struct DecisionEngine {
    /// Switch hysteresis and rate-limit configuration.
    pub config: CoordinatorConfig,
    /// The policy used for utility computation. This field is authoritative:
    /// it takes precedence over the policy embedded in `config`.
    pub reward_policy: RewardPolicy,
}

impl DecisionEngine {
    pub fn new(config: CoordinatorConfig, reward_policy: RewardPolicy) -> Self {
        Self {
            config,
            reward_policy,
        }
    }

    /// Make a routing decision over the sanitized candidate set.
    ///
    /// Never panics: non-finite predictions and utilities are rejected before
    /// any comparison, and the best-candidate selection uses `total_cmp`.
    pub fn decide(&self, input: &EngineInput) -> EngineOutput {
        // -- Phase 1: sanitize every input candidate ------------------------
        //
        // `valid` preserves input order; the first valid candidate feeds the
        // session guard exactly like the Coordinator's `candidates.first()`.
        let mut outcomes: Vec<CandidateOutcome> = Vec::with_capacity(input.candidates.len());
        let mut valid: Vec<(&EngineCandidate, UtilityBreakdown)> = Vec::new();

        for candidate in input.candidates {
            let outcome = self.classify_candidate(candidate, input);
            if outcome.valid {
                valid.push((candidate, outcome.utility.clone()));
            }
            outcomes.push(outcome);
        }

        // -- Phase 2: decide over the valid set -----------------------------
        // Mirrors Coordinator::decide (session guard -> rate limit -> utility).

        // 1. Session constraints first
        let session_action = ActionGuard::decide(
            input.current_candidate,
            valid
                .first()
                .map(|(candidate, _)| candidate.candidate_id.as_str())
                .unwrap_or(""),
            input.session_mode,
            valid
                .first()
                .map(|(candidate, _)| candidate.bundle.success.confidence)
                .unwrap_or(0.0),
        );
        if session_action == Action::Keep {
            return EngineOutput {
                decision: RoutingDecision {
                    action: RoutingAction::Keep,
                    selected_candidate: input.current_candidate.to_string(),
                    utility: UtilityBreakdown::default(),
                    reason: "session constraint: pinned/sticky".into(),
                },
                candidates: outcomes,
            };
        }

        // 2. Switch rate limit
        if input.session_switch_count >= self.config.max_switches_per_session {
            return EngineOutput {
                decision: RoutingDecision {
                    action: RoutingAction::Keep,
                    selected_candidate: input.current_candidate.to_string(),
                    utility: UtilityBreakdown::default(),
                    reason: "switch rate limit reached".into(),
                },
                candidates: outcomes,
            };
        }

        // 3. Utilities were computed during sanitization.

        // 4. Find best candidate. `total_cmp` is a total order — sanitization
        //    already guarantees finite totals, so this is defensive — and
        //    `max_by` resolves ties to the LAST maximal element, matching the
        //    Coordinator's tie behavior.
        let best = valid
            .iter()
            .max_by(|a, b| a.1.total.total_cmp(&b.1.total));
        let current_utility = valid
            .iter()
            .find(|(candidate, _)| candidate.candidate_id == input.current_candidate);

        let decision = match (best, current_utility) {
            (Some((best_candidate, best_util)), Some((_, current_util))) => {
                let delta = best_util.total - current_util.total;
                if best_candidate.candidate_id != input.current_candidate
                    && delta > self.config.switch_threshold
                {
                    RoutingDecision {
                        action: RoutingAction::Switch,
                        selected_candidate: best_candidate.candidate_id.clone(),
                        utility: best_util.clone(),
                        reason: format!(
                            "utility delta {:.3} > threshold {:.3}",
                            delta, self.config.switch_threshold
                        ),
                    }
                } else {
                    RoutingDecision {
                        action: RoutingAction::Keep,
                        selected_candidate: input.current_candidate.to_string(),
                        utility: current_util.clone(),
                        reason: "utility delta below threshold".into(),
                    }
                }
            }
            (Some((best_candidate, best_util)), None) => RoutingDecision {
                action: RoutingAction::Switch,
                selected_candidate: best_candidate.candidate_id.clone(),
                utility: best_util.clone(),
                reason: "no current candidate, selecting best".into(),
            },
            // No candidate survived sanitization (or none were supplied).
            _ => RoutingDecision {
                action: RoutingAction::Keep,
                selected_candidate: input.current_candidate.to_string(),
                utility: UtilityBreakdown::default(),
                reason: "no valid candidates".into(),
            },
        };

        EngineOutput {
            decision,
            candidates: outcomes,
        }
    }

    /// Classify one input candidate: rejected (ineligible / non-finite
    /// prediction / non-finite utility) or valid with its computed utility.
    fn classify_candidate(&self, candidate: &EngineCandidate, input: &EngineInput) -> CandidateOutcome {
        let rejected = |eligible: bool, reason: &str| CandidateOutcome {
            candidate_id: candidate.candidate_id.clone(),
            eligible,
            valid: false,
            rejection_reason: Some(reason.to_string()),
            utility: UtilityBreakdown::default(),
        };

        if !candidate.eligible {
            return rejected(false, "ineligible");
        }
        if !bundle_finite(&candidate.bundle) {
            return rejected(true, "non-finite prediction");
        }
        let utility = compute_utility(
            &candidate.bundle,
            &self.reward_policy,
            input.is_fallback,
            input.session_switch_count,
        );
        if !utility_finite(&utility) {
            return rejected(true, "non-finite utility");
        }
        CandidateOutcome {
            candidate_id: candidate.candidate_id.clone(),
            eligible: true,
            valid: true,
            rejection_reason: None,
            utility,
        }
    }
}

// ---------------------------------------------------------------------------
// Sanitization helpers
// ---------------------------------------------------------------------------

/// True when the prediction's value and confidence are both finite.
fn prediction_finite(prediction: &Prediction) -> bool {
    prediction.value.is_finite() && prediction.confidence.is_finite()
}

/// True when every value and confidence in the bundle is finite.
fn bundle_finite(bundle: &PredictionBundle) -> bool {
    prediction_finite(&bundle.success)
        && prediction_finite(&bundle.latency)
        && prediction_finite(&bundle.ttft)
        && prediction_finite(&bundle.cost)
}

/// True when every utility component (and the total) is finite.
fn utility_finite(utility: &UtilityBreakdown) -> bool {
    utility.success.is_finite()
        && utility.latency.is_finite()
        && utility.ttft.is_finite()
        && utility.cost.is_finite()
        && utility.fallback.is_finite()
        && utility.uncertainty.is_finite()
        && utility.switch_cost.is_finite()
        && utility.total.is_finite()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::coordinator::Coordinator;
    use crate::ml::model::Prediction;

    // -- fixture helpers -----------------------------------------------------

    fn pred(value: f64, confidence: f64) -> Prediction {
        Prediction::trained(value, confidence, 100)
    }

    fn bundle(model: &str, success: f64, latency: f64, ttft: f64, cost: f64) -> PredictionBundle {
        PredictionBundle {
            candidate_model: model.to_string(),
            candidate_provider: "test-provider".to_string(),
            success: pred(success, 0.9),
            latency: pred(latency, 0.8),
            ttft: pred(ttft, 0.7),
            cost: pred(cost, 0.6),
        }
    }

    fn with_success_conf(mut bundle: PredictionBundle, confidence: f64) -> PredictionBundle {
        bundle.success.confidence = confidence;
        bundle
    }

    fn engine_candidate(candidate_id: &str, bundle: PredictionBundle) -> EngineCandidate {
        EngineCandidate {
            candidate_id: candidate_id.to_string(),
            bundle,
            eligible: true,
        }
    }

    fn default_engine() -> DecisionEngine {
        DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default())
    }

    fn run_engine(
        engine: &DecisionEngine,
        current: &str,
        candidates: &[EngineCandidate],
        mode: SessionRoutingMode,
        switch_count: u32,
        is_fallback: bool,
    ) -> EngineOutput {
        let input = EngineInput {
            current_candidate: current,
            candidates,
            session_mode: mode,
            session_switch_count: switch_count,
            is_fallback,
        };
        engine.decide(&input)
    }

    /// Run the frozen Coordinator over `bundles` with `policy` (the config's
    /// embedded policy is overridden so both sides score identically).
    fn run_coordinator(
        config: CoordinatorConfig,
        policy: RewardPolicy,
        current: &str,
        bundles: &[PredictionBundle],
        mode: SessionRoutingMode,
        switch_count: u32,
        is_fallback: bool,
    ) -> RoutingDecision {
        let coordinator = Coordinator::new(CoordinatorConfig {
            reward_policy: policy,
            ..config
        });
        coordinator.decide(current, bundles, mode, switch_count, is_fallback)
    }

    fn assert_utility_identical(actual: &UtilityBreakdown, expected: &UtilityBreakdown, ctx: &str) {
        let fields = [
            ("success", actual.success, expected.success),
            ("latency", actual.latency, expected.latency),
            ("ttft", actual.ttft, expected.ttft),
            ("cost", actual.cost, expected.cost),
            ("fallback", actual.fallback, expected.fallback),
            ("uncertainty", actual.uncertainty, expected.uncertainty),
            ("switch_cost", actual.switch_cost, expected.switch_cost),
            ("total", actual.total, expected.total),
        ];
        for (name, actual_value, expected_value) in fields {
            assert!(
                (actual_value - expected_value).abs() < 1e-10,
                "{ctx}: utility {name}: engine={actual_value} coordinator={expected_value}"
            );
        }
    }

    /// Field-by-field comparison of an engine decision against the
    /// Coordinator's. The only tolerated difference is the empty-list
    /// terminal reason ("no candidates available" -> "no valid candidates").
    fn assert_decisions_match(engine: &RoutingDecision, coordinator: &RoutingDecision, ctx: &str) {
        assert_eq!(
            engine.action, coordinator.action,
            "{ctx}: action mismatch (engine reason: {:?}, coordinator reason: {:?})",
            engine.reason, coordinator.reason
        );
        assert_eq!(
            engine.selected_candidate, coordinator.selected_candidate,
            "{ctx}: selected_candidate mismatch"
        );
        assert_utility_identical(&engine.utility, &coordinator.utility, ctx);
        if coordinator.reason == "no candidates available" {
            // The engine's terminal arm for an empty valid set is deliberately
            // more precise about *why* the set is empty.
            assert_eq!(engine.reason, "no valid candidates", "{ctx}: terminal reason");
        } else {
            assert_eq!(engine.reason, coordinator.reason, "{ctx}: reason mismatch");
        }
    }

    /// Run engine and Coordinator over the same valid fixtures and assert the
    /// decisions match field by field.
    fn cross_check(
        current: &str,
        bundles: &[(&str, PredictionBundle)],
        mode: SessionRoutingMode,
        switch_count: u32,
        is_fallback: bool,
    ) -> (EngineOutput, RoutingDecision) {
        cross_check_with(
            CoordinatorConfig::default(),
            RewardPolicy::default(),
            current,
            bundles,
            mode,
            switch_count,
            is_fallback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn cross_check_with(
        config: CoordinatorConfig,
        policy: RewardPolicy,
        current: &str,
        bundles: &[(&str, PredictionBundle)],
        mode: SessionRoutingMode,
        switch_count: u32,
        is_fallback: bool,
    ) -> (EngineOutput, RoutingDecision) {
        let engine = DecisionEngine::new(config.clone(), policy.clone());
        let candidates: Vec<EngineCandidate> = bundles
            .iter()
            .map(|(id, bundle)| engine_candidate(id, bundle.clone()))
            .collect();
        let engine_output =
            run_engine(&engine, current, &candidates, mode, switch_count, is_fallback);

        let coordinator_bundles: Vec<PredictionBundle> =
            bundles.iter().map(|(_, bundle)| bundle.clone()).collect();
        let coordinator_decision = run_coordinator(
            config,
            policy,
            current,
            &coordinator_bundles,
            mode,
            switch_count,
            is_fallback,
        );

        // All fixtures are eligible and finite: every outcome must be valid,
        // in input order, keyed by candidate_id.
        assert_eq!(
            engine_output.candidates.len(),
            bundles.len(),
            "outcome count must match input count"
        );
        for (outcome, (id, _)) in engine_output.candidates.iter().zip(bundles.iter()) {
            assert_eq!(outcome.candidate_id, *id, "outcome order must follow input order");
            assert!(outcome.eligible, "fixture candidate must be eligible");
            assert!(outcome.valid, "fixture candidate must be valid");
            assert!(
                outcome.rejection_reason.is_none(),
                "valid candidate must carry no rejection reason"
            );
        }

        assert_decisions_match(&engine_output.decision, &coordinator_decision, "cross_check");
        (engine_output, coordinator_decision)
    }

    // -- exact cross-checks vs Coordinator::decide ---------------------------

    #[test]
    fn cross_check_switch_above_threshold() {
        // model-b first so the ActionGuard sees it as predicted_best.
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[
                ("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-b");
        assert!(output.decision.reason.contains("utility delta"));
    }

    #[test]
    fn cross_check_keep_below_threshold() {
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[
                ("model-b", bundle("model-b", 0.9, 480.0, 190.0, 0.02)),
                ("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert_eq!(output.decision.reason, "utility delta below threshold");
    }

    #[test]
    fn cross_check_pinned_forces_keep() {
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[
                ("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Pinned,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert_eq!(output.decision.reason, "session constraint: pinned/sticky");
    }

    #[test]
    fn cross_check_pinned_with_empty_candidates() {
        // Even with an empty valid set the session guard fires first, exactly
        // as in the Coordinator; the "no valid candidates" terminal only
        // applies once guard and rate limit allow a decision.
        let (output, coordinator_decision) =
            cross_check("model-a", &[], SessionRoutingMode::Pinned, 0, false);
        assert_eq!(
            coordinator_decision.reason,
            "session constraint: pinned/sticky"
        );
        assert_eq!(output.decision.reason, "session constraint: pinned/sticky");
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
    }

    #[test]
    fn cross_check_sticky_low_confidence_forces_keep() {
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[(
                "model-a",
                with_success_conf(bundle("model-a", 0.99, 100.0, 50.0, 0.01), 0.3),
            )],
            SessionRoutingMode::Sticky,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.reason, "session constraint: pinned/sticky");
    }

    #[test]
    fn cross_check_sticky_high_confidence_proceeds_to_utility() {
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[
                (
                    "model-b",
                    with_success_conf(bundle("model-b", 0.95, 200.0, 100.0, 0.01), 0.9),
                ),
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Sticky,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-b");
    }

    #[test]
    fn cross_check_switch_rate_limit_reached() {
        let config = CoordinatorConfig {
            max_switches_per_session: 3,
            ..CoordinatorConfig::default()
        };
        let (output, coordinator_decision) = cross_check_with(
            config,
            RewardPolicy::default(),
            "model-a",
            &[
                ("model-b", bundle("model-b", 0.99, 100.0, 50.0, 0.001)),
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Free,
            3,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert!(output.decision.reason.contains("rate limit"));
    }

    #[test]
    fn cross_check_below_rate_limit_allows_switch() {
        let config = CoordinatorConfig {
            max_switches_per_session: 3,
            ..CoordinatorConfig::default()
        };
        let (output, _) = cross_check_with(
            config,
            RewardPolicy::default(),
            "model-a",
            &[
                ("model-b", bundle("model-b", 0.99, 100.0, 50.0, 0.001)),
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Free,
            2,
            false,
        );
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-b");
    }

    #[test]
    fn cross_check_no_current_candidate_in_list() {
        let (output, coordinator_decision) = cross_check(
            "model-z",
            &[
                ("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
                ("model-c", bundle("model-c", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-b");
        assert_eq!(
            output.decision.reason,
            "no current candidate, selecting best"
        );
    }

    #[test]
    fn cross_check_empty_candidates_free_mode() {
        let (output, coordinator_decision) =
            cross_check("model-a", &[], SessionRoutingMode::Free, 0, false);
        assert_eq!(coordinator_decision.action, RoutingAction::Keep);
        assert_eq!(coordinator_decision.reason, "no candidates available");
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert_eq!(output.decision.reason, "no valid candidates");
        assert_utility_identical(
            &output.decision.utility,
            &UtilityBreakdown::default(),
            "empty valid set",
        );
        assert!(output.candidates.is_empty());
    }

    #[test]
    fn cross_check_tie_resolves_to_last() {
        // Two candidates with identical utilities: max_by keeps the LAST
        // maximal element, in both the engine and the Coordinator.
        let (output, coordinator_decision) = cross_check(
            "model-z",
            &[
                ("model-b", bundle("model-b", 0.9, 500.0, 200.0, 0.02)),
                ("model-c", bundle("model-c", 0.9, 500.0, 200.0, 0.02)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Switch);
        assert_eq!(coordinator_decision.selected_candidate, "model-c");
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-c");

        // Current inside the tied set: best is the last (model-c), the delta
        // is 0.0, so hysteresis keeps the current candidate.
        let (kept, coordinator_kept) = cross_check(
            "model-b",
            &[
                ("model-b", bundle("model-b", 0.9, 500.0, 200.0, 0.02)),
                ("model-c", bundle("model-c", 0.9, 500.0, 200.0, 0.02)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(coordinator_kept.action, RoutingAction::Keep);
        assert_eq!(kept.decision.action, RoutingAction::Keep);
        assert_eq!(kept.decision.selected_candidate, "model-b");
    }

    #[test]
    fn cross_check_current_first_in_list_short_circuits() {
        // When the current candidate is first in the list, the ActionGuard's
        // same-candidate rule forces Keep before any utility comparison —
        // even though model-b scores far better.
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
                ("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(coordinator_decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert_eq!(output.decision.reason, "session constraint: pinned/sticky");
    }

    #[test]
    fn cross_check_custom_threshold_config() {
        // Delta ~0.28: above the default threshold (0.1), below 0.5.
        let bundles = [
            ("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
            ("model-a", bundle("model-a", 0.75, 1000.0, 400.0, 0.1)),
        ];
        let (default_output, default_coordinator) =
            cross_check("model-a", &bundles, SessionRoutingMode::Free, 0, false);
        assert_eq!(
            default_coordinator.action,
            RoutingAction::Switch,
            "fixture delta must exceed the default threshold"
        );
        assert_eq!(default_output.decision.action, RoutingAction::Switch);

        let custom = CoordinatorConfig {
            switch_threshold: 0.5,
            ..CoordinatorConfig::default()
        };
        let (custom_output, custom_coordinator) = cross_check_with(
            custom,
            RewardPolicy::default(),
            "model-a",
            &bundles,
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_eq!(custom_coordinator.action, RoutingAction::Keep);
        assert_eq!(custom_output.decision.action, RoutingAction::Keep);
        assert_eq!(custom_output.decision.reason, "utility delta below threshold");
    }

    #[test]
    fn engine_reward_policy_takes_precedence_over_config_policy() {
        // The engine's `reward_policy` field is authoritative for utility,
        // even when it differs from the policy embedded in `config`.
        let config = CoordinatorConfig::default(); // embedded success_weight = 1.0
        let engine_policy = RewardPolicy {
            success_weight: 2.0,
            ..RewardPolicy::default()
        };
        let engine = DecisionEngine::new(config.clone(), engine_policy.clone());
        let candidates = vec![engine_candidate(
            "model-a",
            bundle("model-a", 0.9, 500.0, 200.0, 0.02),
        )];
        let output = run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);

        // success component = 0.9 * 2.0 (engine policy), not 0.9 * 1.0.
        assert!(
            (output.candidates[0].utility.success - 1.8).abs() < 1e-10,
            "engine must score with its own reward_policy, got {}",
            output.candidates[0].utility.success
        );

        let expected = run_coordinator(
            config,
            engine_policy,
            "model-z",
            &[candidates[0].bundle.clone()],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_decisions_match(&output.decision, &expected, "policy precedence");
    }

    #[derive(Clone, Copy)]
    enum CurrentPosition {
        First,
        Second,
        Absent,
    }

    /// Fixture pair for the full matrix: model-b is always first (the guard's
    /// predicted_best); the current candidate sits first, second, or outside
    /// the list. `delta_above` toggles between a decisive and a marginal gap.
    fn matrix_fixture(
        confidence: f64,
        delta_above: bool,
        position: CurrentPosition,
    ) -> (Vec<(&'static str, PredictionBundle)>, &'static str) {
        let best = if delta_above {
            with_success_conf(bundle("model-b", 0.95, 200.0, 100.0, 0.01), confidence)
        } else {
            with_success_conf(bundle("model-b", 0.9, 480.0, 190.0, 0.02), confidence)
        };
        let runner_up = |model: &'static str| {
            if delta_above {
                bundle(model, 0.3, 4000.0, 1500.0, 0.9)
            } else {
                bundle(model, 0.9, 500.0, 200.0, 0.02)
            }
        };
        match position {
            CurrentPosition::First => (
                vec![("model-a", runner_up("model-a")), ("model-b", best)],
                "model-a",
            ),
            CurrentPosition::Second => (
                vec![("model-b", best), ("model-a", runner_up("model-a"))],
                "model-a",
            ),
            CurrentPosition::Absent => (
                vec![("model-b", best), ("model-c", runner_up("model-c"))],
                "model-a",
            ),
        }
    }

    #[test]
    fn cross_check_full_matrix() {
        let modes: [(SessionRoutingMode, f64); 5] = [
            (SessionRoutingMode::Free, 0.9),   // guard: Switch
            (SessionRoutingMode::Free, 0.5),   // guard: Explore (must not short-circuit)
            (SessionRoutingMode::Sticky, 0.9), // sticky + confident: proceeds
            (SessionRoutingMode::Sticky, 0.3), // sticky + low confidence: forced Keep
            (SessionRoutingMode::Pinned, 0.9), // pinned: forced Keep
        ];

        // Empty valid set across every mode, rate-limit state and fallback.
        for (mode, _) in modes {
            for &rate_limited in &[false, true] {
                for &is_fallback in &[false, true] {
                    let switch_count = if rate_limited { 5 } else { 1 };
                    cross_check("model-a", &[], mode, switch_count, is_fallback);
                }
            }
        }

        // Non-empty matrix: mode x rate limit x delta x current position x fallback.
        for (mode, confidence) in modes {
            for &rate_limited in &[false, true] {
                for &delta_above in &[true, false] {
                    for position in [
                        CurrentPosition::First,
                        CurrentPosition::Second,
                        CurrentPosition::Absent,
                    ] {
                        for &is_fallback in &[false, true] {
                            let switch_count = if rate_limited { 5 } else { 1 };
                            let (bundles, current) =
                                matrix_fixture(confidence, delta_above, position);
                            cross_check(current, &bundles, mode, switch_count, is_fallback);
                        }
                    }
                }
            }
        }
    }

    // -- sanitization: non-finite predictions ---------------------------------

    #[test]
    fn nan_success_value_rejected_others_unaffected() {
        let engine = default_engine();
        let mut poisoned = bundle("model-b", 0.99, 100.0, 50.0, 0.005);
        poisoned.success.value = f64::NAN;
        let candidates = vec![
            engine_candidate("model-b", poisoned),
            engine_candidate("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
            engine_candidate("model-c", bundle("model-c", 0.3, 4000.0, 1500.0, 0.9)),
        ];
        let output =
            run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);

        assert_eq!(output.candidates.len(), 3);
        let rejected = &output.candidates[0];
        assert!(rejected.eligible);
        assert!(!rejected.valid);
        assert_eq!(
            rejected.rejection_reason.as_deref(),
            Some("non-finite prediction")
        );
        assert_utility_identical(
            &rejected.utility,
            &UtilityBreakdown::default(),
            "rejected utility",
        );
        assert!(output.candidates[1].valid);
        assert!(output.candidates[2].valid);

        // Decision runs over the survivors only and matches the Coordinator
        // over the same survivors.
        let valid_bundles = vec![
            candidates[1].bundle.clone(),
            candidates[2].bundle.clone(),
        ];
        let expected = run_coordinator(
            CoordinatorConfig::default(),
            RewardPolicy::default(),
            "model-z",
            &valid_bundles,
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_decisions_match(&output.decision, &expected, "nan filtered");
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-a");
    }

    #[test]
    fn all_candidates_nan_keep_no_valid() {
        let engine = default_engine();
        let mut nan_value = bundle("model-a", 0.9, 500.0, 200.0, 0.02);
        nan_value.success.value = f64::NAN;
        let mut nan_confidence = bundle("model-b", 0.99, 100.0, 50.0, 0.005);
        nan_confidence.cost.confidence = f64::NAN;
        let candidates = vec![
            engine_candidate("model-a", nan_value),
            engine_candidate("model-b", nan_confidence),
        ];
        let output =
            run_engine(&engine, "model-a", &candidates, SessionRoutingMode::Free, 0, false);

        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert_eq!(output.decision.reason, "no valid candidates");
        assert_utility_identical(
            &output.decision.utility,
            &UtilityBreakdown::default(),
            "empty valid set",
        );
        for outcome in &output.candidates {
            assert!(!outcome.valid);
            assert_eq!(
                outcome.rejection_reason.as_deref(),
                Some("non-finite prediction")
            );
        }
    }

    #[test]
    fn non_finite_prediction_in_any_slot_rejected() {
        type Slot = fn(&mut PredictionBundle);
        let slots: [(&str, Slot); 8] = [
            ("success.value", |b| b.success.value = f64::NAN),
            ("success.confidence", |b| b.success.confidence = f64::NAN),
            ("latency.value", |b| b.latency.value = f64::INFINITY),
            (
                "latency.confidence",
                |b| b.latency.confidence = f64::NEG_INFINITY,
            ),
            ("ttft.value", |b| b.ttft.value = f64::NAN),
            ("ttft.confidence", |b| b.ttft.confidence = f64::NAN),
            ("cost.value", |b| b.cost.value = f64::INFINITY),
            ("cost.confidence", |b| b.cost.confidence = f64::NAN),
        ];
        for (slot, poison) in slots {
            let engine = default_engine();
            let mut poisoned = bundle("model-b", 0.95, 200.0, 100.0, 0.01);
            poison(&mut poisoned);
            let candidates = vec![
                engine_candidate("model-b", poisoned),
                engine_candidate("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
            ];
            let output =
                run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);
            assert!(!output.candidates[0].valid, "slot {slot} should be rejected");
            assert_eq!(
                output.candidates[0].rejection_reason.as_deref(),
                Some("non-finite prediction"),
                "slot {slot}"
            );
            assert!(
                output.candidates[1].valid,
                "slot {slot} must not affect other candidates"
            );
            assert_eq!(
                output.decision.selected_candidate, "model-a",
                "slot {slot}"
            );
        }
    }

    #[test]
    fn inf_latency_value_rejected() {
        let engine = default_engine();
        let mut poisoned = bundle("model-a", 0.9, 500.0, 200.0, 0.02);
        poisoned.latency.value = f64::INFINITY;
        let candidates = vec![engine_candidate("model-a", poisoned)];
        let output =
            run_engine(&engine, "model-a", &candidates, SessionRoutingMode::Free, 0, false);

        assert!(!output.candidates[0].valid);
        assert_eq!(
            output.candidates[0].rejection_reason.as_deref(),
            Some("non-finite prediction")
        );
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.reason, "no valid candidates");
    }

    #[test]
    fn nan_confidence_rejected() {
        let engine = default_engine();
        let mut poisoned = bundle("model-a", 0.9, 500.0, 200.0, 0.02);
        poisoned.success.confidence = f64::NAN;
        let candidates = vec![
            engine_candidate("model-a", poisoned),
            engine_candidate("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
        ];
        let output =
            run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);

        assert!(!output.candidates[0].valid);
        assert_eq!(
            output.candidates[0].rejection_reason.as_deref(),
            Some("non-finite prediction")
        );
        assert!(output.candidates[1].valid);
        assert_eq!(output.decision.selected_candidate, "model-b");
    }

    // -- sanitization: eligibility --------------------------------------------

    #[test]
    fn ineligible_candidate_excluded() {
        let engine = default_engine();
        let mut best = engine_candidate("model-b", bundle("model-b", 0.99, 100.0, 50.0, 0.005));
        best.eligible = false;
        let candidates = vec![
            best,
            engine_candidate("model-c", bundle("model-c", 0.95, 200.0, 100.0, 0.01)),
            engine_candidate("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
        ];
        let output =
            run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);

        let rejected = &output.candidates[0];
        assert!(!rejected.eligible);
        assert!(!rejected.valid);
        assert_eq!(rejected.rejection_reason.as_deref(), Some("ineligible"));
        assert_utility_identical(
            &rejected.utility,
            &UtilityBreakdown::default(),
            "ineligible utility",
        );

        // The ineligible candidate must not win even though it scores best.
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-c");

        let valid_bundles = vec![
            candidates[1].bundle.clone(),
            candidates[2].bundle.clone(),
        ];
        let expected = run_coordinator(
            CoordinatorConfig::default(),
            RewardPolicy::default(),
            "model-z",
            &valid_bundles,
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_decisions_match(&output.decision, &expected, "ineligible filtered");
    }

    #[test]
    fn ineligible_takes_precedence_over_non_finite() {
        let engine = default_engine();
        let mut broken = bundle("model-b", f64::NAN, 100.0, 50.0, 0.005);
        broken.success.value = f64::NAN;
        let candidates = vec![
            EngineCandidate {
                candidate_id: "model-b".to_string(),
                bundle: broken,
                eligible: false,
            },
            engine_candidate("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
        ];
        let output =
            run_engine(&engine, "model-a", &candidates, SessionRoutingMode::Free, 0, false);

        assert_eq!(
            output.candidates[0].rejection_reason.as_deref(),
            Some("ineligible")
        );
        assert!(output.candidates[1].valid);
    }

    // -- sanitization: non-finite utilities -----------------------------------

    #[test]
    fn non_finite_utility_rejected() {
        // Finite inputs and a finite weight, but the success term overflows
        // to +inf.
        let policy = RewardPolicy {
            success_weight: 1e300,
            ..RewardPolicy::default()
        };
        let engine = DecisionEngine::new(CoordinatorConfig::default(), policy.clone());
        let candidates = vec![
            engine_candidate("model-h", bundle("model-h", 1e300, 200.0, 100.0, 0.01)),
            engine_candidate("model-n", bundle("model-n", 0.9, 500.0, 200.0, 0.02)),
        ];
        let output =
            run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);

        let rejected = &output.candidates[0];
        assert!(rejected.eligible);
        assert!(!rejected.valid);
        assert_eq!(
            rejected.rejection_reason.as_deref(),
            Some("non-finite utility")
        );
        assert_utility_identical(
            &rejected.utility,
            &UtilityBreakdown::default(),
            "non-finite utility",
        );
        assert!(output.candidates[1].valid);

        let expected = run_coordinator(
            CoordinatorConfig::default(),
            policy,
            "model-z",
            &[candidates[1].bundle.clone()],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_decisions_match(&output.decision, &expected, "non-finite utility filtered");
        assert_eq!(output.decision.selected_candidate, "model-n");
    }

    #[test]
    fn non_finite_policy_weight_rejects_all_candidates() {
        // A NaN weight poisons every utility; the Coordinator would panic on
        // the NaN comparison, the engine must reject and fall back to Keep.
        let policy = RewardPolicy {
            latency_weight: f64::NAN,
            ..RewardPolicy::default()
        };
        let engine = DecisionEngine::new(CoordinatorConfig::default(), policy.clone());
        let candidates = vec![
            engine_candidate("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
            engine_candidate("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
        ];
        let output =
            run_engine(&engine, "model-a", &candidates, SessionRoutingMode::Free, 0, false);

        for outcome in &output.candidates {
            assert!(!outcome.valid);
            assert_eq!(
                outcome.rejection_reason.as_deref(),
                Some("non-finite utility")
            );
        }
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-a");
        assert_eq!(output.decision.reason, "no valid candidates");

        // Equivalent to the Coordinator over an empty candidate list.
        let expected = run_coordinator(
            CoordinatorConfig::default(),
            policy,
            "model-a",
            &[],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert_decisions_match(&output.decision, &expected, "nan policy");
    }

    #[test]
    fn huge_but_finite_utility_still_valid() {
        let (output, coordinator_decision) = cross_check(
            "model-a",
            &[
                ("model-h", bundle("model-h", 1e300, 200.0, 100.0, 0.01)),
                ("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
            ],
            SessionRoutingMode::Free,
            0,
            false,
        );
        assert!(
            output.candidates[0].valid,
            "finite utility is the only validity criterion"
        );
        assert!(output.candidates[0].utility.total > 1e299);
        assert_eq!(coordinator_decision.selected_candidate, "model-h");
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_eq!(output.decision.selected_candidate, "model-h");
    }

    // -- sanitization ordering -------------------------------------------------

    #[test]
    fn guard_uses_first_valid_candidate() {
        let engine = default_engine();
        // The first candidate has a NaN confidence: if the guard saw it,
        // Sticky + NaN (`NaN < 0.8` is false) would NOT force a Keep and the
        // decision would reach the utility path. The engine must key the
        // guard on the first VALID candidate (confidence 0.5 -> forced Keep).
        let mut nan_conf = bundle("model-a", 0.95, 200.0, 100.0, 0.01);
        nan_conf.success.confidence = f64::NAN;
        let candidates = vec![
            engine_candidate("model-a", nan_conf),
            engine_candidate(
                "model-b",
                with_success_conf(bundle("model-b", 0.9, 500.0, 200.0, 0.02), 0.5),
            ),
        ];
        let output =
            run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Sticky, 0, false);

        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_eq!(output.decision.selected_candidate, "model-z");
        assert_eq!(output.decision.reason, "session constraint: pinned/sticky");
        assert!(!output.candidates[0].valid);
        assert!(output.candidates[1].valid);

        let expected = run_coordinator(
            CoordinatorConfig::default(),
            RewardPolicy::default(),
            "model-z",
            &[candidates[1].bundle.clone()],
            SessionRoutingMode::Sticky,
            0,
            false,
        );
        assert_decisions_match(&output.decision, &expected, "guard first valid");
    }

    #[test]
    fn outcomes_preserve_input_order_and_flags() {
        let engine = default_engine();
        let mut nan = bundle("model-c", 0.9, 500.0, 200.0, 0.02);
        nan.ttft.value = f64::NAN;
        let mut ineligible =
            engine_candidate("model-b", bundle("model-b", 0.99, 100.0, 50.0, 0.005));
        ineligible.eligible = false;
        let candidates = vec![
            engine_candidate("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
            ineligible,
            engine_candidate("model-c", nan),
            engine_candidate("model-d", bundle("model-d", 0.3, 4000.0, 1500.0, 0.9)),
        ];
        let output =
            run_engine(&engine, "model-z", &candidates, SessionRoutingMode::Free, 0, false);

        let ids: Vec<&str> = output
            .candidates
            .iter()
            .map(|outcome| outcome.candidate_id.as_str())
            .collect();
        assert_eq!(ids, vec!["model-a", "model-b", "model-c", "model-d"]);

        assert!(output.candidates[0].valid);
        assert_eq!(
            output.candidates[1].rejection_reason.as_deref(),
            Some("ineligible")
        );
        assert_eq!(
            output.candidates[2].rejection_reason.as_deref(),
            Some("non-finite prediction")
        );
        assert!(output.candidates[3].valid);
    }

    #[test]
    fn decision_utility_matches_selected_outcome() {
        let engine = default_engine();

        // Switch case: the decision carries the winner's breakdown.
        let candidates = vec![
            engine_candidate("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
            engine_candidate("model-a", bundle("model-a", 0.3, 4000.0, 1500.0, 0.9)),
        ];
        let output =
            run_engine(&engine, "model-a", &candidates, SessionRoutingMode::Free, 0, false);
        assert_eq!(output.decision.action, RoutingAction::Switch);
        assert_utility_identical(
            &output.decision.utility,
            &output.candidates[0].utility,
            "switch winner",
        );

        // Keep case: the decision carries the current candidate's breakdown.
        let close = vec![
            engine_candidate("model-b", bundle("model-b", 0.9, 480.0, 190.0, 0.02)),
            engine_candidate("model-a", bundle("model-a", 0.9, 500.0, 200.0, 0.02)),
        ];
        let output = run_engine(&engine, "model-a", &close, SessionRoutingMode::Free, 0, false);
        assert_eq!(output.decision.action, RoutingAction::Keep);
        assert_utility_identical(
            &output.decision.utility,
            &output.candidates[1].utility,
            "kept current",
        );
    }

    // -- serde -----------------------------------------------------------------

    #[test]
    fn engine_output_serde_round_trip() {
        let engine = default_engine();
        let mut nan = bundle("model-c", 0.9, 500.0, 200.0, 0.02);
        nan.cost.confidence = f64::NAN;
        let candidates = vec![
            engine_candidate("model-b", bundle("model-b", 0.95, 200.0, 100.0, 0.01)),
            engine_candidate("model-c", nan),
        ];
        let output =
            run_engine(&engine, "model-a", &candidates, SessionRoutingMode::Free, 0, false);

        let json = serde_json::to_string(&output).unwrap();
        let restored: EngineOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.decision.action, output.decision.action);
        assert_eq!(restored.decision.selected_candidate, output.decision.selected_candidate);
        assert_eq!(restored.decision.reason, output.decision.reason);
        assert_eq!(restored.candidates.len(), 2);
        assert!(restored.candidates[0].valid);
        assert_eq!(
            restored.candidates[1].rejection_reason.as_deref(),
            Some("non-finite prediction")
        );
        assert!(
            (restored.candidates[0].utility.total - output.candidates[0].utility.total).abs()
                < 1e-10
        );

        // rejection_reason is skipped when None (valid candidates).
        let valid_json = serde_json::to_string(&output.candidates[0]).unwrap();
        assert!(
            !valid_json.contains("rejection_reason"),
            "rejection_reason should be skipped for valid candidates: {valid_json}"
        );

        let candidate_json = serde_json::to_string(&candidates[0]).unwrap();
        let restored_candidate: EngineCandidate = serde_json::from_str(&candidate_json).unwrap();
        assert_eq!(restored_candidate.candidate_id, "model-b");
        assert!(restored_candidate.eligible);
        assert!(
            (restored_candidate.bundle.success.value - candidates[0].bundle.success.value).abs()
                < 1e-10
        );
    }
}
