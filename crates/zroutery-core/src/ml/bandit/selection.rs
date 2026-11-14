use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{BanditConfig, BanditError, RewardFitConfig, SafetyConfig, SelectionConfig};
use super::reward::{
    agreement, build_pairs, cell_key, holdout_pairs_only_differ_on_success, objective,
    policy_from_weights, solve_weights, weights_vector, OutcomeProxy, RewardBasis, RewardFitReport,
    RewardFitVerdict, Row,
};
use super::safety::{
    arm_safety_metrics, compare_to_reference, evidence_of, ArmSafetyMetrics,
    ArmSelectionStatistics, RewardArm, SafetyEvaluation, SafetyEvidence, SafetyVerdict,
};
use super::{
    BASIS_WIDTH, B_LATENCY, FITTED_ARM_NAME, OUTCOME_PROXY_FIT_TARGET, OUTCOME_PROXY_ORDER,
    REWARD_FIT_TARGET_DESCRIPTION, UNIDENTIFIABLE_REASON, UNIDENTIFIABLE_WEIGHT,
};
use crate::ml::dataset::{validate_outcome_sample, OutcomeTrainingSample};
use crate::ml::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use crate::ml::reward::RewardPolicy;

// ---------------------------------------------------------------------------
// BanditReport / BanditOutcome
// ---------------------------------------------------------------------------

/// The replay that produced the selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionTrace {
    /// The recorded seed. The only source of randomness in the module.
    pub seed: u64,
    /// The UCB1 exploration coefficient used.
    pub exploration_c: f64,
    /// Rows in the evaluation partition that the replay ran over.
    pub replay_rows: usize,
    /// Pulls in total across every arm.
    pub total_observations: usize,
    /// The selected arm.
    pub selected: String,
    /// Arms that tied with the selected arm on `ucb_score`. Non-empty means the
    /// seed's tie-break decided the outcome, and a reader can see that.
    pub tied_arms: Vec<String>,
}

/// The honest report of one offline bandit run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanditReport {
    /// [`OUTCOME_PROXY_ORDER`], verbatim, at the top of the report.
    pub outcome_proxy_order: String,
    /// Rows in the snapshot.
    pub rows: usize,
    /// The reward fit's report.
    pub reward_fit: RewardFitReport,
    /// The per-arm bandit state, sorted by arm name.
    pub arms: Vec<ArmSelectionStatistics>,
    /// Every arm's measured outcome profile on the common evaluation set,
    /// sorted by arm name.
    pub safety_by_arm: Vec<ArmSafetyMetrics>,
    /// What the replay did.
    pub selection: SelectionTrace,
    /// What the evaluation partition contains.
    pub evidence: SafetyEvidence,
    /// The safety comparison of the selected arm against the reference.
    pub safety: SafetyEvaluation,
    /// The verdict as decided, for serializing. Acceptance does **not** read
    /// this field; see [`BanditReport::accepted_arm`].
    pub safety_verdict: SafetyVerdict,
}

impl BanditReport {
    /// The selected arm, recomputed from the report.
    ///
    /// [`SafetyEvaluation::verdict`] is called again here rather than reading
    /// [`BanditReport::safety_verdict`], so no stored field can hand out an
    /// acceptance. `None` unless the recomputed verdict is
    /// [`SafetyVerdict::Accept`].
    pub fn accepted_arm(&self) -> Option<&str> {
        if self.safety.verdict().is_acceptance() {
            Some(self.safety.candidate.as_str())
        } else {
            None
        }
    }

    /// Whether the run produced an acceptance, by the same recomputation.
    pub fn is_acceptable(&self) -> bool {
        self.safety.verdict().is_acceptance()
    }
}

/// Everything one offline run produced.
pub struct BanditOutcome {
    fitted: RewardPolicy,
    fitted_weights: [f64; BASIS_WIDTH],
    arms: Vec<ArmSelectionStatistics>,
    report: BanditReport,
}

impl std::fmt::Debug for BanditOutcome {
    /// Structural, without dumping the whole report.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BanditOutcome")
            .field("selected", &self.report.selection.selected)
            .field("seed", &self.report.selection.seed)
            .field("arms", &self.arms.len())
            .field("fit_verdict", &self.report.reward_fit.verdict)
            .field("safety_verdict", &self.report.safety.verdict())
            .field("accepted", &self.is_acceptable())
            .finish()
    }
}

impl BanditOutcome {
    /// The fitted reward policy, which is an accepted `RewardPolicy`.
    ///
    /// Produced for review. Installing it belongs to the node that owns
    /// installation, and nothing here installs it.
    pub fn fitted_policy(&self) -> &RewardPolicy {
        &self.fitted
    }

    /// The fitted weights in [`RewardBasis`] column order.
    pub fn fitted_weights(&self) -> &[f64; BASIS_WIDTH] {
        &self.fitted_weights
    }

    /// The per-arm bandit state.
    pub fn arms(&self) -> &[ArmSelectionStatistics] {
        &self.arms
    }

    /// The honest report.
    pub fn report(&self) -> &BanditReport {
        &self.report
    }

    /// Whether the gate accepted, by recomputation.
    pub fn is_acceptable(&self) -> bool {
        self.report.is_acceptable()
    }

    /// The accepted arm, or `None` when the gate withheld acceptance.
    pub fn accepted_arm(&self) -> Option<&str> {
        self.report.accepted_arm()
    }
}

// ---------------------------------------------------------------------------
// run_bandit
// ---------------------------------------------------------------------------

/// Fit a reward, replay the snapshot over the declared arms, select one, and
/// gate it.
///
/// The run is pure: the snapshot, the declared arms, and the configuration are
/// the only inputs, and the fitted weights, the per-arm state, the report and the
/// verdict are the only outputs. Nothing is installed, nothing is scheduled, and
/// nothing in the running product calls this.
///
/// `baseline_arms` are the caller-declared candidate policies. The arm the fit
/// produces is appended automatically under [`FITTED_ARM_NAME`].
pub fn run_bandit(
    snapshot: &[OutcomeTrainingSample],
    baseline_arms: &[RewardArm],
    config: &BanditConfig,
) -> Result<BanditOutcome, BanditError> {
    config.checked()?;
    if snapshot.is_empty() {
        return Err(BanditError::EmptySnapshot);
    }

    let rows = project_rows(snapshot)?;
    reject_degenerate_reward(&rows)?;
    let partition = split_cells(&rows, config.reward.holdout_cells)?;
    let holdout_rows = partition.holdout_rows.as_slice();

    let fit_report = fit_reward(&rows, &partition, &config.reward)?;
    let fitted_policy = fit_report.fitted.clone();
    let fitted_weights = weights_vector(&fitted_policy);

    let arms = assemble_arms(baseline_arms, fitted_policy.clone(), &config.safety)?;
    let reference = arms
        .iter()
        .find(|arm| arm.name == config.safety.reference_arm)
        .ok_or_else(|| BanditError::ReferenceArmUnknown {
            name: config.safety.reference_arm.clone(),
        })?;
    let reference_name = reference.name.clone();

    let (arm_statistics, trace_rows) = replay(holdout_rows, &arms, &config.selection)?;
    let evidence = evidence_of(holdout_rows);
    if evidence.total < config.safety.min_evaluation_samples {
        return Err(BanditError::EvaluationSetTooSmall {
            min: config.safety.min_evaluation_samples,
            found: evidence.total,
        });
    }

    let safety_by_arm = arms
        .iter()
        .map(|arm| arm_safety_metrics(arm, holdout_rows, &config.safety))
        .collect::<Result<Vec<_>, _>>()?;

    let selection = select_arm(&arm_statistics, &config.selection)?;
    let safety = compare_to_reference(
        &selection.selected,
        &reference_name,
        &safety_by_arm,
        &evidence,
        &config.safety,
    )?;
    let safety_verdict = safety.verdict();

    let report = BanditReport {
        outcome_proxy_order: OUTCOME_PROXY_ORDER.to_string(),
        rows: rows.len(),
        reward_fit: fit_report,
        arms: arm_statistics.clone(),
        safety_by_arm,
        selection: SelectionTrace {
            seed: config.selection.seed,
            exploration_c: config.selection.exploration_c,
            replay_rows: trace_rows,
            total_observations: arm_statistics.iter().map(|arm| arm.observations).sum(),
            selected: selection.selected.clone(),
            tied_arms: selection.tied_arms,
        },
        evidence,
        safety,
        safety_verdict,
    };

    Ok(BanditOutcome {
        fitted: fitted_policy,
        fitted_weights,
        arms: arm_statistics,
        report,
    })
}

/// What the selection phase produced before the report.
struct Selection {
    selected: String,
    tied_arms: Vec<String>,
}

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

impl BanditConfig {
    /// Refuse a configuration that would make the run, or the gate, meaningless.
    pub(super) fn checked(&self) -> Result<(), BanditError> {
        let reward = &self.reward;
        if reward.holdout_cells == 0 {
            return Err(BanditError::EmptyHoldoutCells);
        }
        if reward.iterations == 0 {
            return Err(BanditError::EmptyFitIterations);
        }
        if !reward.learning_rate.is_finite() || reward.learning_rate <= 0.0 {
            return Err(BanditError::MeaninglessLearningRate(reward.learning_rate));
        }
        if !reward.ridge_lambda.is_finite() || reward.ridge_lambda < 0.0 {
            return Err(BanditError::MeaninglessRidgeLambda(reward.ridge_lambda));
        }
        if reward.min_pair_count == 0 {
            return Err(BanditError::EmptyPairFloor);
        }
        if reward.max_pairs_per_cell == 0 {
            return Err(BanditError::EmptyPairCeiling);
        }

        let selection = &self.selection;
        if !selection.exploration_c.is_finite() || selection.exploration_c < 0.0 {
            return Err(BanditError::MeaninglessExploration(selection.exploration_c));
        }
        if selection.min_arm_observations == 0 {
            return Err(BanditError::EmptyArmObservationFloor);
        }

        let safety = &self.safety;
        if !safety.min_reward_improvement.is_finite() || safety.min_reward_improvement <= 0.0 {
            return Err(BanditError::MeaninglessSafetyFloor(
                safety.min_reward_improvement,
            ));
        }
        if safety.min_evaluation_samples < 2 {
            return Err(BanditError::MeaninglessEvaluationFloor(
                safety.min_evaluation_samples,
            ));
        }
        for (name, value) in [
            (
                "max_failure_rate_regression",
                safety.max_failure_rate_regression,
            ),
            ("max_cost_regression", safety.max_cost_regression),
            (
                "max_tail_latency_regression_ms",
                safety.max_tail_latency_regression_ms,
            ),
            (
                "max_fallback_rate_regression",
                safety.max_fallback_rate_regression,
            ),
        ] {
            if !value.is_finite() {
                return Err(BanditError::MeaninglessTolerance { name, value });
            }
            if value < 0.0 {
                return Err(BanditError::NegativeTolerance { name, value });
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Apply every per-row gate, then put the rows in canonical order.
///
/// The gates are the same ones warmup applies, and for the same reason: the
/// evidence needed to *police* a row is in the canonical sample, so a row is
/// validated before anything is read out of it.
fn project_rows(snapshot: &[OutcomeTrainingSample]) -> Result<Vec<Row>, BanditError> {
    let mut projected = Vec::with_capacity(snapshot.len());
    for (index, sample) in snapshot.iter().enumerate() {
        if sample.schema_version != FEATURE_SCHEMA_VERSION {
            return Err(BanditError::SchemaMismatch {
                index,
                component: "sample",
                found: sample.schema_version,
                expected: FEATURE_SCHEMA_VERSION,
            });
        }
        if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
            return Err(BanditError::SchemaMismatch {
                index,
                component: "feature",
                found: sample.features.schema_version,
                expected: FEATURE_SCHEMA_VERSION,
            });
        }
        if sample.features.values.len() != FEATURE_DIMENSION {
            return Err(BanditError::InvalidSample {
                index,
                reason: format!(
                    "feature vector of width {} is not the {FEATURE_DIMENSION} this build expects",
                    sample.features.values.len()
                ),
            });
        }
        if let Some((position, value)) = sample
            .features
            .values
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(BanditError::InvalidSample {
                index,
                reason: format!("feature {position} is not finite: {value}"),
            });
        }
        for (field, value) in [
            ("latency_ms", sample.targets.latency_ms),
            ("cost", sample.targets.cost),
        ] {
            if let Some(value) = value {
                if !value.is_finite() {
                    return Err(BanditError::NonFiniteOutcome {
                        index,
                        field,
                        value,
                    });
                }
            }
        }
        validate_outcome_sample(sample)
            .map_err(|reason| BanditError::InvalidSample { index, reason })?;

        let proxy = OutcomeProxy::from_targets(&sample.targets);
        // The confidence is 1.0 by declaration, not by measurement: a dataset
        // row carries no decision-time confidence, and inventing one would let an
        // arm be rewarded for a number unrelated to the recorded outcome. See
        // `RewardBasis::from_targets`.
        let basis = RewardBasis::from_targets(&sample.targets, proxy.switch_count(), 1.0);
        if !basis.is_finite() {
            return Err(BanditError::NonFiniteOutcome {
                index,
                field: "reward basis",
                value: basis.values[B_LATENCY],
            });
        }
        projected.push(Row {
            sample_id: sample.sample_id.clone(),
            cell: cell_key(sample),
            proxy,
            basis,
            legacy: sample.clone().into_legacy(),
        });
    }

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, row) in projected.iter().enumerate() {
        if let Some(first) = seen.insert(row.sample_id.as_str(), index) {
            return Err(BanditError::DuplicateSampleId {
                sample_id: row.sample_id.clone(),
                first,
                second: index,
            });
        }
    }

    // `(timestamp, sample_id)` is a total order, and the duplicate check has made
    // the id component unique, so the run is a function of the snapshot's
    // contents rather than of the order a caller handed over.
    let order: BTreeMap<&str, i64> = snapshot
        .iter()
        .map(|sample| (sample.sample_id.as_str(), sample.timestamp))
        .collect();
    projected.sort_by(|left, right| {
        order[left.sample_id.as_str()]
            .cmp(&order[right.sample_id.as_str()])
            .then_with(|| left.sample_id.cmp(&right.sample_id))
    });
    Ok(projected)
}

/// Refuse a snapshot whose outcomes are all identical.
pub(super) fn reject_degenerate_reward(rows: &[Row]) -> Result<(), BanditError> {
    let mut shapes: BTreeMap<Vec<u64>, usize> = BTreeMap::new();
    for row in rows {
        // The bit patterns of the six columns identify an outcome shape exactly,
        // and ordering them keeps the count independent of row order.
        let key = row
            .basis
            .values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<u64>>();
        *shapes.entry(key).or_default() += 1;
    }
    if shapes.len() < 2 {
        return Err(BanditError::DegenerateReward { rows: rows.len() });
    }
    Ok(())
}

/// The number of distinct outcome shapes, for the report.
fn distinct_shapes(rows: &[Row]) -> usize {
    let mut shapes: BTreeMap<Vec<u64>, ()> = BTreeMap::new();
    for row in rows {
        shapes.insert(
            row.basis
                .values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<u64>>(),
            (),
        );
    }
    shapes.len()
}

// ---------------------------------------------------------------------------
// Partition
// ---------------------------------------------------------------------------

/// The cells of a snapshot, in canonical key order, and the two partitions the
/// run works from.
struct Partition<'a> {
    /// Every cell key, sorted.
    cells: Vec<&'a str>,
    /// Rows in the leading cells, which the reward fit is computed on.
    fit_rows: Vec<&'a Row>,
    /// Rows in the trailing cells, which are the holdout and double as the
    /// common evaluation set for the bandit replay and the safety gate.
    holdout_rows: Vec<&'a Row>,
}

/// Split the rows by cell, taking the trailing cells as the holdout.
fn split_cells<'a>(rows: &'a [Row], holdout_cells: usize) -> Result<Partition<'a>, BanditError> {
    let mut by_cell: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        by_cell.entry(row.cell.as_str()).or_default().push(row);
    }
    // Sorted by cell key, so the split below is a function of the snapshot's
    // contents. Taking first-seen order here instead would make the partition
    // depend on the order rows happened to arrive in.
    let ordered: Vec<&str> = by_cell.keys().copied().collect();
    if holdout_cells >= ordered.len() {
        return Err(BanditError::HoldoutTakesEveryCell {
            total: ordered.len(),
            holdout_cells,
        });
    }
    let split = ordered.len() - holdout_cells;
    let fit_rows: Vec<&Row> = ordered[..split]
        .iter()
        .flat_map(|cell| by_cell[*cell].iter().copied())
        .collect();
    let holdout_rows: Vec<&Row> = ordered[split..]
        .iter()
        .flat_map(|cell| by_cell[*cell].iter().copied())
        .collect();
    Ok(Partition {
        cells: ordered,
        fit_rows,
        holdout_rows,
    })
}

// ---------------------------------------------------------------------------
// The fit
// ---------------------------------------------------------------------------

/// Fit the six weights on the fit partition and check them on the holdout.
fn fit_reward(
    rows: &[Row],
    partition: &Partition<'_>,
    config: &RewardFitConfig,
) -> Result<RewardFitReport, BanditError> {
    let (fit_rows, holdout_rows) = (&partition.fit_rows, &partition.holdout_rows);
    let fit_pairs = build_pairs(fit_rows, config.max_pairs_per_cell)?;
    if fit_pairs.len() < config.min_pair_count {
        return Err(BanditError::InsufficientPairs {
            pairs: fit_pairs.len(),
            min_pairs: config.min_pair_count,
        });
    }
    let holdout_pairs = build_pairs(holdout_rows, config.max_pairs_per_cell)?;
    if holdout_pairs.is_empty() {
        return Err(BanditError::HoldoutNotComparable);
    }

    let prior = weights_vector(&config.prior);
    let fitted = solve_weights(
        &fit_pairs,
        prior,
        config.iterations,
        config.learning_rate,
        config.ridge_lambda,
    )?;

    let prior_agreement = agreement(&holdout_pairs, &prior);
    let fitted_agreement = agreement(&holdout_pairs, &fitted);
    let (Some(prior_agreement), Some(fitted_agreement)) = (prior_agreement, fitted_agreement)
    else {
        return Err(BanditError::HoldoutNotComparable);
    };
    let agreement_delta = fitted_agreement - prior_agreement;
    let verdict = if agreement_delta > 0.0 {
        RewardFitVerdict::Better
    } else if agreement_delta < 0.0 {
        RewardFitVerdict::Worse
    } else {
        RewardFitVerdict::NotBetter
    };
    // A holdout in which *every* pair differs on the success column cannot
    // demonstrate that the other five weights learned anything: a prior that
    // weights success heavily enough also scores every one of those pairs
    // correctly, so a better agreement would be a fact about the base rate rather
    // than about the fit. That is the same downgrade warmup applies to a
    // single-class holdout, for the same reason.
    let verdict = if holdout_pairs_only_differ_on_success(&holdout_pairs) {
        verdict.downgraded_to_not_better()
    } else {
        verdict
    };

    let feedback_signals_present = rows
        .iter()
        .filter(|row| !row.legacy.feedback.is_empty())
        .count();

    Ok(RewardFitReport {
        target_description: OUTCOME_PROXY_FIT_TARGET.to_string(),
        order_description: OUTCOME_PROXY_ORDER.to_string(),
        outcome_proxy_not_preference: REWARD_FIT_TARGET_DESCRIPTION.to_string(),
        cells_total: partition.cells.len(),
        fit_cells: partition.cells.len() - config.holdout_cells,
        holdout_cells: config.holdout_cells,
        fit_rows: fit_rows.len(),
        holdout_rows: holdout_rows.len(),
        pairs_fit: fit_pairs.len(),
        pairs_holdout: holdout_pairs.len(),
        fit_objective_at_prior: objective(&fit_pairs, &prior, &prior, config.ridge_lambda),
        fit_objective_fitted: objective(&fit_pairs, &fitted, &prior, config.ridge_lambda),
        prior_holdout_agreement: prior_agreement,
        fitted_holdout_agreement: fitted_agreement,
        agreement_delta,
        verdict,
        prior: config.prior.clone(),
        prior_weights: prior.to_vec(),
        fitted: policy_from_weights(fitted),
        fitted_weights: fitted.to_vec(),
        unidentifiable_weights: vec![UNIDENTIFIABLE_WEIGHT.to_string()],
        unidentifiable_reason: UNIDENTIFIABLE_REASON.to_string(),
        feedback_signals_present,
        distinct_outcome_shapes: distinct_shapes(rows),
    })
}

// ---------------------------------------------------------------------------
// Arms
// ---------------------------------------------------------------------------

/// Sort and validate the arm set, appending the fitted arm.
pub(super) fn assemble_arms(
    baseline_arms: &[RewardArm],
    fitted: RewardPolicy,
    safety: &SafetyConfig,
) -> Result<Vec<RewardArm>, BanditError> {
    if baseline_arms.is_empty() {
        return Err(BanditError::NoArms);
    }
    let mut arms = baseline_arms.to_vec();
    for (index, arm) in arms.iter().enumerate() {
        if arm.name.trim().is_empty() {
            return Err(BanditError::EmptyArmName { index });
        }
        if arm.name == FITTED_ARM_NAME {
            return Err(BanditError::ReservedArmName {
                name: arm.name.clone(),
            });
        }
    }
    arms.push(RewardArm::new(FITTED_ARM_NAME, fitted));
    // Canonical arm order, so the replay assignment below is a function of the
    // arm *set* rather than of the order the caller declared them in.
    arms.sort_by(|left, right| left.name.cmp(&right.name));
    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    for arm in &arms {
        if seen.insert(arm.name.as_str(), ()).is_some() {
            return Err(BanditError::DuplicateArmName {
                name: arm.name.clone(),
            });
        }
    }
    if !arms.iter().any(|arm| arm.name == safety.reference_arm) {
        return Err(BanditError::ReferenceArmUnknown {
            name: safety.reference_arm.clone(),
        });
    }
    Ok(arms)
}

// ---------------------------------------------------------------------------
// Replay and selection
// ---------------------------------------------------------------------------

/// A deterministic seeded hash, used only for the replay assignment schedule.
fn splitmix64(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A deterministic seeded hash of a sample id.
pub(super) fn seeded_hash(seed: u64, sample_id: &str) -> u64 {
    // FNV-1a over the id, then mixed with the seed. Both steps are pure integer
    // arithmetic, so the assignment is reproducible across platforms and does
    // not depend on any hash map's iteration order.
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in sample_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    splitmix64(seed ^ hash)
}

/// Replay the evaluation partition, accumulating per-arm state.
fn replay(
    rows: &[&Row],
    arms: &[RewardArm],
    config: &SelectionConfig,
) -> Result<(Vec<ArmSelectionStatistics>, usize), BanditError> {
    let mut rewards: Vec<Vec<f64>> = vec![Vec::new(); arms.len()];
    for row in rows {
        let index = (seeded_hash(config.seed, &row.sample_id) % arms.len() as u64) as usize;
        let score = row.basis.score(&arms[index].policy);
        if !score.is_finite() {
            return Err(BanditError::NonFiniteScore {
                arm: arms[index].name.clone(),
                value: score,
            });
        }
        rewards[index].push(score);
    }

    let total_observations: usize = rewards.iter().map(Vec::len).sum();
    if total_observations < config.min_total_observations {
        return Err(BanditError::TooFewTotalObservations {
            total: total_observations,
            min: config.min_total_observations,
        });
    }

    let mut statistics = Vec::with_capacity(arms.len());
    for (index, arm) in arms.iter().enumerate() {
        let observed = &rewards[index];
        if observed.len() < config.min_arm_observations {
            return Err(BanditError::ArmUnderObserved {
                name: arm.name.clone(),
                observations: observed.len(),
                min: config.min_arm_observations,
            });
        }
        let mean = observed.iter().sum::<f64>() / observed.len() as f64;
        if !mean.is_finite() {
            return Err(BanditError::NonFiniteScore {
                arm: arm.name.clone(),
                value: mean,
            });
        }
        let variance = if observed.len() > 1 {
            observed
                .iter()
                .map(|value| (value - mean) * (value - mean))
                .sum::<f64>()
                / (observed.len() - 1) as f64
        } else {
            0.0
        };
        let standard_error = (variance / observed.len() as f64).sqrt();
        let exploration_bonus = config.exploration_c
            * (2.0 * (total_observations as f64).ln() / observed.len() as f64).sqrt();
        let ucb_score = mean + exploration_bonus;
        if !ucb_score.is_finite() {
            return Err(BanditError::NonFiniteScore {
                arm: arm.name.clone(),
                value: ucb_score,
            });
        }
        statistics.push(ArmSelectionStatistics {
            arm: arm.name.clone(),
            policy: arm.policy.clone(),
            observations: observed.len(),
            mean_outcome_proxy_reward: mean,
            reward_variance: variance,
            reward_standard_error: standard_error,
            total_observations,
            exploration_bonus,
            ucb_score,
        });
    }
    Ok((statistics, rows.len()))
}

/// Select the arm with the highest UCB score, breaking ties deterministically
/// with the recorded seed.
fn select_arm(
    statistics: &[ArmSelectionStatistics],
    config: &SelectionConfig,
) -> Result<Selection, BanditError> {
    if statistics.is_empty() {
        return Err(BanditError::NoArms);
    }
    let best = statistics
        .iter()
        .map(|arm| arm.ucb_score)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut tied: Vec<&ArmSelectionStatistics> = statistics
        .iter()
        .filter(|arm| arm.ucb_score == best)
        .collect();
    if tied.is_empty() {
        return Err(BanditError::NoArms);
    }
    tied.sort_by(|left, right| {
        seeded_hash(config.seed, left.arm.as_str())
            .cmp(&seeded_hash(config.seed, right.arm.as_str()))
            .then_with(|| left.arm.cmp(&right.arm))
    });
    let selected = tied[0].arm.clone();
    let tied_arms = tied.iter().map(|arm| arm.arm.clone()).collect();
    Ok(Selection {
        selected,
        tied_arms,
    })
}
