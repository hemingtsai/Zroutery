//! The promotion gate: what has to be true before a candidate model serves.
//!
//! # Why this is a gate and not a boolean
//!
//! "Training succeeded" is not a reason to serve. A model can train cleanly,
//! verify, and still be worse than the baseline it would replace; a model can
//! beat the baseline on utility and still select ineligible candidates; a model
//! can look fine on the traces it was fitted on and have been fitted on all of
//! them. Each of those is a different failure with a different fix, and a gate
//! that collapses them into one boolean cannot tell an operator which one
//! happened.
//!
//! So [`PromotionGate::evaluate`] returns one criterion per check, each with its
//! own measurement and its own reason, and a verdict that is the conjunction of
//! them. A rejected candidate names the criterion that rejected it.
//!
//! # The three verdicts are not interchangeable
//!
//! * [`PromotionVerdict::Promoted`] — every criterion held.
//! * [`PromotionVerdict::Rejected`] — the evidence was sufficient and the
//!   candidate failed a criterion. More data will not fix this.
//! * [`PromotionVerdict::Blocked`] — the evidence was *insufficient* to judge.
//!   More data might. A gate that reported this as a rejection would train
//!   people to ignore rejections, and a gate that reported it as a promotion
//!   would be the thing this whole module exists to prevent.
//!
//! # Reproducibility
//!
//! The decision is recorded with the model identity, the dataset identity, the
//! evaluation report, the gate configuration and a timestamp. Re-running the
//! gate on the same inputs and the same configuration reproduces the same
//! verdict, so a decision can be re-derived and audited rather than trusted.

use serde::{Deserialize, Serialize};

use super::comparison::{BaselinePairing, RoutingComparison, RoutingVerdict};
use super::learning::TrainingReport;
use super::traces::DatasetFingerprint;

// ---------------------------------------------------------------------------
// PromotionCriterion
// ---------------------------------------------------------------------------

/// One named check, with the number it was decided on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionCriterion {
    /// Stable identifier, so a decision can be reasoned about by name.
    pub name: String,
    /// Whether this criterion held.
    pub held: bool,
    /// The measured value, where there is one.
    pub measured: Option<f64>,
    /// The threshold it was compared against, where there is one.
    pub threshold: Option<f64>,
    /// Why, in words a reader can act on.
    pub reason: String,
}

impl PromotionCriterion {
    fn held(
        name: &str,
        measured: Option<f64>,
        threshold: Option<f64>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            held: true,
            measured,
            threshold,
            reason: reason.into(),
        }
    }

    fn failed(
        name: &str,
        measured: Option<f64>,
        threshold: Option<f64>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            held: false,
            measured,
            threshold,
            reason: reason.into(),
        }
    }

    /// A criterion that could not be decided because the evidence was missing.
    fn unknown(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            held: false,
            measured: None,
            threshold: None,
            reason: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// PromotionVerdict
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionVerdict {
    Promoted,
    Rejected,
    Blocked,
}

impl std::fmt::Display for PromotionVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PromotionVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            PromotionVerdict::Promoted => "PROMOTED",
            PromotionVerdict::Rejected => "REJECTED",
            PromotionVerdict::Blocked => "BLOCKED",
        }
    }
}

// ---------------------------------------------------------------------------
// PromotionConfig
// ---------------------------------------------------------------------------

/// The thresholds a promotion is decided against.
///
/// Every field is serialised into the decision record, so the configuration a
/// decision was made under is recoverable rather than assumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionConfig {
    /// The baseline the candidate must beat. Named so "beat the baseline" is a
    /// statement about a specific baseline and not about whichever one was
    /// convenient.
    pub required_baseline: String,
    /// Minimum paired requests before a routing comparison counts. Inherited
    /// from the comparison so the two can never disagree about the floor.
    pub min_paired_requests: usize,
    /// Minimum paired utility improvement, in observed-utility units.
    pub min_utility_delta: f64,
    /// Maximum tolerated number of success regressions against the baseline.
    pub max_success_regressions: usize,
    /// Maximum tolerated fallback-rate increase.
    pub max_fallback_rate_delta: f64,
    /// Maximum tolerated mean-cost increase.
    pub max_cost_delta: f64,
    /// Maximum tolerated mean-latency increase, in milliseconds.
    pub max_latency_delta_ms: f64,
    /// Whether the candidate may select a candidate the recorded decision
    /// marked ineligible. Always false in any configuration worth shipping.
    pub allow_ineligible_selections: bool,
    /// Minimum out-of-sample success head loss improvement, in nats, relative
    /// to the log loss of an uninformed predictor.
    ///
    /// Deliberately not zero. A model that ties `ln(2)` has learned nothing, and
    /// a gate whose only model-quality criterion is "not worse than a coin
    /// flip" lets the untrained model through while reporting that it had been
    /// checked. The default asks for a small but real margin.
    pub min_holdout_loss_improvement: f64,
}

impl Default for PromotionConfig {
    fn default() -> Self {
        Self {
            required_baseline: "baseline.lowest_latency".to_string(),
            min_paired_requests: super::comparison::MIN_PAIRED_REQUESTS,
            min_utility_delta: super::comparison::UTILITY_DELTA_THRESHOLD,
            max_success_regressions: 0,
            max_fallback_rate_delta: 0.02,
            max_cost_delta: 0.05,
            max_latency_delta_ms: 250.0,
            allow_ineligible_selections: false,
            min_holdout_loss_improvement: 0.05,
        }
    }
}

impl PromotionConfig {
    /// A content-addressed identity for this configuration.
    pub fn identity(&self) -> String {
        let mut hash = 0xcbf29ce484222325u64;
        let mut mix = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
        };
        mix(b"zroutery-promotion-config-v1\0");
        mix(self.required_baseline.as_bytes());
        mix(&self.min_paired_requests.to_le_bytes());
        mix(&self.min_utility_delta.to_le_bytes());
        mix(&(self.max_success_regressions as u64).to_le_bytes());
        mix(&self.max_fallback_rate_delta.to_le_bytes());
        mix(&self.max_cost_delta.to_le_bytes());
        mix(&self.max_latency_delta_ms.to_le_bytes());
        mix(&[u8::from(self.allow_ineligible_selections)]);
        mix(&self.min_holdout_loss_improvement.to_le_bytes());
        format!("{hash:016x}")
    }
}

// ---------------------------------------------------------------------------
// PromotionDecision
// ---------------------------------------------------------------------------

/// A promotion decision, with everything needed to re-derive it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionDecision {
    pub verdict: PromotionVerdict,
    pub candidate_commit: String,
    /// The model the decision names, and the number of learning events it
    /// records.
    ///
    /// Carried because both are inputs to the commit id. A decision that did not
    /// carry them could not be checked against a checkpoint by the store that
    /// installs it, which would leave the store trusting the id it was handed
    /// rather than re-deriving it.
    pub model_id: String,
    pub learning_event_count: u64,
    /// The body the evidence was computed over, i.e. the whole body the model
    /// was trained *from*. This is the identity a reader needs to reproduce the
    /// decision.
    pub dataset_fingerprint: DatasetFingerprint,
    /// The fitted partition, which is a strictly smaller set. Recorded as well
    /// because "promoted on 120 requests" and "fitted on 252 samples" are
    /// different facts and a decision that only names one of them is ambiguous.
    pub fitted_partition_fingerprint: DatasetFingerprint,
    /// The holdout out-of-sample log loss the candidate achieved.
    pub holdout_loss: f64,
    /// Identity of the gate configuration.
    pub gate_config_identity: String,
    /// The configuration itself, so the thresholds are readable.
    pub gate_config: PromotionConfig,
    /// Every criterion, in evaluation order.
    pub criteria: Vec<PromotionCriterion>,
    /// The baseline pairing the decision rests on.
    pub baseline: String,
    pub paired_requests: usize,
    /// Unix seconds when the decision was made.
    pub decided_at: i64,
    /// The revision the decision was taken against, when the caller has one.
    pub revision: Option<String>,
}

impl PromotionDecision {
    /// The criteria that did not hold, for the reason a decision went the way
    /// it did.
    pub fn blockers(&self) -> Vec<&PromotionCriterion> {
        self.criteria.iter().filter(|c| !c.held).collect()
    }

    pub fn is_promoted(&self) -> bool {
        self.verdict == PromotionVerdict::Promoted
    }
}

// ---------------------------------------------------------------------------
// PromotionGate
// ---------------------------------------------------------------------------

/// Decides whether a candidate model may serve.
#[derive(Debug, Clone)]
pub struct PromotionGate {
    config: PromotionConfig,
}

impl PromotionGate {
    pub fn new(config: PromotionConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &PromotionConfig {
        &self.config
    }

    /// Evaluate a candidate against its training report and its routing
    /// comparison.
    ///
    /// `comparison` must have been produced over the same body the model was
    /// fitted on, or at least over a body the caller has reason to trust; the
    /// dataset fingerprints of the two are compared here so a mismatch is
    /// caught here rather than discovered later.
    pub fn evaluate(
        &self,
        training: &TrainingReport,
        comparison: &RoutingComparison,
        revision: Option<String>,
    ) -> PromotionDecision {
        let mut criteria: Vec<PromotionCriterion> = Vec::new();

        // -- 1. The pairing the decision rests on must exist. -----------------
        let pairing: Option<&BaselinePairing> = comparison
            .paired
            .iter()
            .find(|pair| pair.baseline == self.config.required_baseline);
        let Some(pairing) = pairing else {
            criteria.push(PromotionCriterion::unknown(
                "baseline_present",
                format!(
                    "the comparison contains no pairing against the required baseline '{}'",
                    self.config.required_baseline
                ),
            ));
            return self.decide(training, criteria, String::new(), 0, revision);
        };
        criteria.push(PromotionCriterion::held(
            "baseline_present",
            None,
            None,
            format!("paired against '{}'", pairing.baseline),
        ));

        // -- 2. Enough paired evidence to judge at all. ------------------------
        criteria.push(
            if pairing.deltas.paired_requests < self.config.min_paired_requests {
                PromotionCriterion::unknown(
                    "paired_evidence",
                    format!(
                        "{} paired requests against a floor of {}",
                        pairing.deltas.paired_requests, self.config.min_paired_requests
                    ),
                )
            } else {
                PromotionCriterion::held(
                    "paired_evidence",
                    Some(pairing.deltas.paired_requests as f64),
                    Some(self.config.min_paired_requests as f64),
                    format!(
                        "{} paired requests meet the floor of {}",
                        pairing.deltas.paired_requests, self.config.min_paired_requests
                    ),
                )
            },
        );

        // -- 3. Out-of-sample model quality. ----------------------------------
        // The log loss of an uninformed model is ln(2); the candidate must beat
        // it by the configured margin. Reported even when the paired evidence is
        // thin, because it is the one criterion that can be met with little data.
        let improvement = std::f64::consts::LN_2 - training.holdout_loss;
        criteria.push(if !training.holdout_loss.is_finite() {
            PromotionCriterion::unknown(
                "holdout_quality",
                "the frozen holdout produced no finite loss, so the model was never judged out of sample",
            )
        } else if improvement < self.config.min_holdout_loss_improvement {
            PromotionCriterion::failed(
                "holdout_quality",
                Some(training.holdout_loss),
                Some(std::f64::consts::LN_2 - self.config.min_holdout_loss_improvement),
                format!(
                    "holdout log loss {:.4} did not beat an uninformed model by the required {:.4}",
                    training.holdout_loss, self.config.min_holdout_loss_improvement
                ),
            )
        } else {
            PromotionCriterion::held(
                "holdout_quality",
                Some(training.holdout_loss),
                Some(self.config.min_holdout_loss_improvement),
                format!(
                    "holdout log loss {:.4} beats ln(2) by {:.4}",
                    training.holdout_loss, improvement
                ),
            )
        });

        // -- 4. Routing utility. ---------------------------------------------
        criteria.push(PromotionCriterion {
            name: "routing_utility".to_string(),
            held: pairing.deltas.mean_observed_utility_delta >= self.config.min_utility_delta,
            measured: Some(pairing.deltas.mean_observed_utility_delta),
            threshold: Some(self.config.min_utility_delta),
            reason: format!(
                "mean observed utility moved {:+.4} against a required {:+.4} over {} paired requests",
                pairing.deltas.mean_observed_utility_delta,
                self.config.min_utility_delta,
                pairing.deltas.paired_requests
            ),
        });

        // -- 5. Safety: never route to something the decision rejected. -------
        let ineligible = comparison
            .arm("ml.candidate")
            .map(|arm| arm.ineligible_selections)
            .unwrap_or(0);
        criteria.push(
            if ineligible == 0 || self.config.allow_ineligible_selections {
                PromotionCriterion::held(
                    "selection_safety",
                    Some(ineligible as f64),
                    Some(0.0),
                    format!(
                        "{ineligible} selections of a candidate the decision had marked ineligible"
                    ),
                )
            } else {
                PromotionCriterion::failed(
                    "selection_safety",
                    Some(ineligible as f64),
                    Some(0.0),
                    format!(
                        "{ineligible} selections of a candidate the decision had marked ineligible"
                    ),
                )
            },
        );

        // -- 6. Safety regression budget. ------------------------------------
        criteria.push(if pairing.deltas.candidate_regressions > self.config.max_success_regressions
        {
            PromotionCriterion::failed(
                "success_regression",
                Some(pairing.deltas.candidate_regressions as f64),
                Some(self.config.max_success_regressions as f64),
                format!(
                    "{} requests succeeded under the baseline and failed under the candidate, against a budget of {}",
                    pairing.deltas.candidate_regressions, self.config.max_success_regressions
                ),
            )
        } else {
            PromotionCriterion::held(
                "success_regression",
                Some(pairing.deltas.candidate_regressions as f64),
                Some(self.config.max_success_regressions as f64),
                format!(
                    "{} regressions against a budget of {}",
                    pairing.deltas.candidate_regressions, self.config.max_success_regressions
                ),
            )
        });

        // -- 7. Cost regression budget. --------------------------------------
        criteria.push(PromotionCriterion {
            name: "cost_regression".to_string(),
            held: pairing.deltas.mean_cost_delta <= self.config.max_cost_delta,
            measured: Some(pairing.deltas.mean_cost_delta),
            threshold: Some(self.config.max_cost_delta),
            reason: format!(
                "mean cost moved {:+.6} against a tolerated {:+.6}",
                pairing.deltas.mean_cost_delta, self.config.max_cost_delta
            ),
        });

        // -- 8. Latency regression budget. -----------------------------------
        criteria.push(PromotionCriterion {
            name: "latency_regression".to_string(),
            held: pairing.deltas.mean_latency_delta_ms <= self.config.max_latency_delta_ms,
            measured: Some(pairing.deltas.mean_latency_delta_ms),
            threshold: Some(self.config.max_latency_delta_ms),
            reason: format!(
                "mean latency moved {:+.1} ms against a tolerated {:+.1} ms",
                pairing.deltas.mean_latency_delta_ms, self.config.max_latency_delta_ms
            ),
        });

        // -- 9. Dataset identity: the model and the evidence must be about the
        //       same body of data. ------------------------------------------
        //
        // The comparison runs over the whole body the model was trained *from*,
        // so this compares `source_fingerprint` and not `dataset_fingerprint`:
        // the latter names the fitted partition, which is a strictly smaller set
        // by construction and could never match. Comparing the wrong one made this
        // criterion unsatisfiable, i.e. every promotion rejected for a reason that
        // had nothing to do with the model.
        let same_body =
            comparison.dataset_fingerprint.as_str() == training.source_fingerprint.as_str();
        criteria.push(if same_body {
            PromotionCriterion::held(
                "dataset_identity",
                None,
                None,
                format!(
                    "both the model and the evidence name source body {} (fitted partition {})",
                    training.source_fingerprint, training.dataset_fingerprint
                ),
            )
        } else {
            PromotionCriterion::failed(
                "dataset_identity",
                None,
                None,
                format!(
                    "the model was trained from body {} but the comparison was computed over {}",
                    training.source_fingerprint, comparison.dataset_fingerprint
                ),
            )
        });

        self.decide(
            training,
            criteria,
            pairing.baseline.clone(),
            pairing.deltas.paired_requests,
            revision,
        )
    }

    /// Turn the criteria into a verdict.
    ///
    /// The rule, stated once so it can be checked: a candidate whose paired
    /// evidence was below the floor is `Blocked` whatever else held, because
    /// nothing else was measured with enough power to mean anything. Otherwise
    /// every criterion must hold.
    fn decide(
        &self,
        training: &TrainingReport,
        criteria: Vec<PromotionCriterion>,
        baseline: String,
        paired_requests: usize,
        revision: Option<String>,
    ) -> PromotionDecision {
        let evidence_short = criteria
            .iter()
            .any(|criterion| criterion.name == "paired_evidence" && criterion.measured.is_none())
            || criteria
                .iter()
                .any(|c| c.name == "baseline_present" && !c.held);

        let verdict = if evidence_short {
            PromotionVerdict::Blocked
        } else if criteria.iter().all(|criterion| criterion.held) {
            PromotionVerdict::Promoted
        } else {
            PromotionVerdict::Rejected
        };

        PromotionDecision {
            verdict,
            candidate_commit: training.final_commit.clone(),
            model_id: training.model_id.clone(),
            learning_event_count: training.learning_event_count,
            dataset_fingerprint: training.source_fingerprint.clone(),
            fitted_partition_fingerprint: training.dataset_fingerprint.clone(),
            holdout_loss: training.holdout_loss,
            gate_config_identity: self.config.identity(),
            gate_config: self.config.clone(),
            criteria,
            baseline,
            paired_requests,
            decided_at: now_seconds(),
            revision,
        }
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Whether a verdict names a routing improvement, for callers that want the
/// comparison's own verdict rather than the gate's.
pub fn comparison_was_improved(verdict: RoutingVerdict) -> bool {
    matches!(verdict, RoutingVerdict::Improved)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::comparison::{ArmMetrics, BaselinePairing, PairedDeltas, RoutingComparison};
    use crate::ml::learning::{FeatureCoverage, PassReport};
    use crate::ml::reward::RewardPolicy;

    /// A distinguishable dataset identity for a given seed, without building a
    /// whole body of samples to hash.
    fn fingerprint(seed: u64) -> DatasetFingerprint {
        DatasetFingerprint::parse(&format!("{seed:016x}"))
            .expect("a u64 always formats as sixteen hex digits")
    }

    fn report(dataset: DatasetFingerprint, holdout_loss: f64) -> TrainingReport {
        TrainingReport {
            model_id: "shadow".to_string(),
            sample_count: 120,
            request_count: 40,
            train_size: 84,
            validation_size: 18,
            holdout_size: 18,
            train_groups: 28,
            validation_groups: 6,
            holdout_groups: 6,
            passes: vec![PassReport {
                pass: 1,
                train_loss_before: 0.7,
                train_loss_after: 0.5,
                validation_loss: 0.55,
                validation: Default::default(),
                commit_id: "0000000000000001".to_string(),
            }],
            holdout_loss,
            holdout: Default::default(),
            final_commit: "cafebabecafebabe".to_string(),
            base_commit: "0000000000000000".to_string(),
            learning_event_count: 84,
            dataset_fingerprint: dataset.clone(),
            source_fingerprint: dataset.clone(),
            config_identity: "00112233445566aa".to_string(),
            feature_schema_version: super::super::features::FEATURE_SCHEMA_VERSION,
            reward_policy: RewardPolicy::default(),
            coverage: FeatureCoverage::measure(&[]),
            produced_at: 1_700_000_000,
        }
    }

    fn deltas(utility: f64, regressions: usize, cost: f64, latency: f64) -> PairedDeltas {
        PairedDeltas {
            paired_requests: 100,
            success_rate_delta: 0.0,
            fallback_rate_delta: 0.0,
            mean_latency_delta_ms: latency,
            mean_ttft_delta_ms: 0.0,
            mean_cost_delta: cost,
            mean_observed_utility_delta: utility,
            both_succeeded: 100 - regressions,
            candidate_regressions: regressions,
            candidate_improvements: 100,
            both_failed: 0,
            utility_regressions: regressions,
        }
    }

    fn comparison(
        fingerprint: DatasetFingerprint,
        paired: Vec<BaselinePairing>,
        ineligible: usize,
    ) -> RoutingComparison {
        RoutingComparison {
            dataset_fingerprint: fingerprint,
            traces: 100,
            candidate_commit: "cafebabecafebabe".to_string(),
            arms: vec![ArmMetrics {
                policy: "ml.candidate".to_string(),
                ineligible_selections: ineligible,
                ..Default::default()
            }],
            paired,
            reward_policy: RewardPolicy::default(),
            min_paired_requests: PromotionConfig::default().min_paired_requests,
            produced_at: 1_700_000_000,
        }
    }

    fn pairing(deltas: PairedDeltas) -> BaselinePairing {
        BaselinePairing {
            baseline: "baseline.lowest_latency".to_string(),
            deltas,
            verdict: RoutingVerdict::Improved,
        }
    }

    fn gate() -> PromotionGate {
        PromotionGate::new(PromotionConfig::default())
    }

    #[test]
    fn a_candidate_that_beats_the_baseline_on_every_axis_is_promoted() {
        let report = report(fingerprint(1), 0.30);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.5, 0, 0.0, 0.0))],
            0,
        );
        let decision = gate().evaluate(&report, &comparison, Some("rev-1".into()));
        assert_eq!(
            decision.verdict,
            PromotionVerdict::Promoted,
            "{:?}",
            decision.criteria
        );
        assert!(decision.is_promoted());
        assert!(decision.blockers().is_empty());
        assert_eq!(decision.revision.as_deref(), Some("rev-1"));
        assert!(!decision.gate_config_identity.is_empty());
    }

    #[test]
    fn too_little_paired_evidence_blocks_rather_than_rejects() {
        let report = report(fingerprint(2), 0.30);
        let mut thin = deltas(0.5, 0, 0.0, 0.0);
        thin.paired_requests = 5;
        let comparison = comparison(report.dataset_fingerprint.clone(), vec![pairing(thin)], 0);
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Blocked);
        let blockers: Vec<&str> = decision
            .blockers()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(blockers.contains(&"paired_evidence"));
    }

    #[test]
    fn a_missing_baseline_pairing_blocks() {
        let report = report(fingerprint(3), 0.30);
        let comparison = comparison(report.dataset_fingerprint.clone(), Vec::new(), 0);
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Blocked);
        assert!(decision
            .blockers()
            .iter()
            .any(|c| c.name == "baseline_present"));
    }

    #[test]
    fn a_utility_regression_rejects_with_the_number_attached() {
        let report = report(fingerprint(4), 0.30);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(-0.4, 0, 0.0, 0.0))],
            0,
        );
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Rejected);
        let criterion = decision
            .blockers()
            .into_iter()
            .find(|c| c.name == "routing_utility")
            .expect("the utility criterion must be the blocker");
        assert_eq!(criterion.measured, Some(-0.4));
        assert!(criterion.reason.contains("-0.4000"));
    }

    #[test]
    fn success_regressions_reject_even_when_utility_improves() {
        let report = report(fingerprint(5), 0.30);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.9, 3, 0.0, 0.0))],
            0,
        );
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Rejected);
        assert!(decision
            .blockers()
            .iter()
            .any(|c| c.name == "success_regression"));
    }

    #[test]
    fn selecting_an_ineligible_candidate_rejects() {
        let report = report(fingerprint(6), 0.30);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.9, 0, 0.0, 0.0))],
            2,
        );
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Rejected);
        assert!(decision
            .blockers()
            .iter()
            .any(|c| c.name == "selection_safety"));
    }

    #[test]
    fn a_cost_or_latency_regression_rejects() {
        let report = report(fingerprint(7), 0.30);
        let dear = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.9, 0, 0.9, 0.0))],
            0,
        );
        assert_eq!(
            gate().evaluate(&report, &dear, None).verdict,
            PromotionVerdict::Rejected
        );
        let slow = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.9, 0, 0.0, 900.0))],
            0,
        );
        assert_eq!(
            gate().evaluate(&report, &slow, None).verdict,
            PromotionVerdict::Rejected
        );
    }

    #[test]
    fn a_model_scored_on_a_different_dataset_is_refused() {
        let body_a = fingerprint(11);
        let body_b = fingerprint(12);
        let report = report(body_a.clone(), 0.30);
        let comparison = comparison(body_b, vec![pairing(deltas(0.9, 0, 0.0, 0.0))], 0);
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Rejected);
        assert!(decision
            .blockers()
            .iter()
            .any(|c| c.name == "dataset_identity"));
    }

    #[test]
    fn an_untrained_model_is_refused_on_holdout_quality() {
        // A holdout loss of exactly ln(2) is what an uninformed model scores.
        let report = report(fingerprint(8), std::f64::consts::LN_2);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.9, 0, 0.0, 0.0))],
            0,
        );
        let decision = gate().evaluate(&report, &comparison, None);
        assert_eq!(decision.verdict, PromotionVerdict::Rejected);
        assert!(decision
            .blockers()
            .iter()
            .any(|c| c.name == "holdout_quality"));
    }

    #[test]
    fn the_same_inputs_reproduce_the_same_decision() {
        let report = report(fingerprint(9), 0.30);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.5, 0, 0.0, 0.0))],
            0,
        );
        let first = gate().evaluate(&report, &comparison, None);
        let second = gate().evaluate(&report, &comparison, None);
        assert_eq!(first.verdict, second.verdict);
        assert_eq!(first.criteria, second.criteria);
        assert_eq!(first.gate_config_identity, second.gate_config_identity);
    }

    #[test]
    fn changing_the_gate_configuration_changes_its_identity() {
        let base = PromotionConfig::default();
        assert_eq!(base.identity(), base.clone().identity());
        assert_ne!(
            base.identity(),
            PromotionConfig {
                max_cost_delta: 0.5,
                ..base.clone()
            }
            .identity()
        );
    }

    #[test]
    fn the_gate_reads_the_comparison_verdict_separately_from_its_own() {
        // The comparison's verdict and the gate's verdict answer different
        // questions, and collapsing them would let a promotion rest on the
        // comparison's opinion rather than on the gate's criteria.
        assert!(comparison_was_improved(RoutingVerdict::Improved));
        assert!(!comparison_was_improved(RoutingVerdict::NoDifference));
        assert!(!comparison_was_improved(
            RoutingVerdict::InsufficientEvidence
        ));
    }

    #[test]
    fn a_promotion_records_everything_needed_to_re_derive_it() {
        let report = report(fingerprint(10), 0.30);
        let comparison = comparison(
            report.dataset_fingerprint.clone(),
            vec![pairing(deltas(0.5, 0, 0.0, 0.0))],
            0,
        );
        let decision = gate().evaluate(&report, &comparison, Some("rev-9".into()));

        // The record has to survive a round trip through storage with every
        // identity intact, or "auditable" means nothing.
        let encoded = serde_json::to_string(&decision).expect("encode");
        let decoded: PromotionDecision = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, decision);
        assert_eq!(
            DatasetFingerprint::parse(decoded.dataset_fingerprint.as_str())
                .expect("the recorded fingerprint restores"),
            report.dataset_fingerprint
        );

        // And every criterion must be individually checkable against its own
        // measurement, not merely present.
        for criterion in &decision.criteria {
            assert!(
                !criterion.reason.is_empty(),
                "criterion {} carries no reason",
                criterion.name
            );
        }
        assert!(decision
            .criteria
            .iter()
            .any(|c| c.name == "routing_utility" && c.measured.is_some() && c.threshold.is_some()));
    }
}
