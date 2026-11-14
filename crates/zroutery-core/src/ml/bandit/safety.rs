use serde::{Deserialize, Serialize};

use super::config::{BanditError, SafetyConfig, SafetyTolerances};
use super::reward::Row;
use super::{ACCEPTED_PRIOR_ARM_NAME, DEFAULT_TAIL_PERCENTILE};
use crate::ml::dataset::TrainingSample as DatasetTrainingSample;
use crate::ml::evaluation::RoutingMetrics;
use crate::ml::reward::{RewardComputer, RewardPolicy};

// ---------------------------------------------------------------------------
// RewardArm
// ---------------------------------------------------------------------------

/// A named candidate reward policy. One arm is one point the bandit selects
/// over.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardArm {
    /// Unique, non-empty name.
    pub name: String,
    /// The candidate weight vector.
    pub policy: RewardPolicy,
}

impl RewardArm {
    /// Declare an arm.
    pub fn new(name: impl Into<String>, policy: RewardPolicy) -> Self {
        Self {
            name: name.into(),
            policy,
        }
    }

    /// The accepted hand-set policy, named as
    /// [`ACCEPTED_PRIOR_ARM_NAME`]. The natural reference arm, and the natural
    /// control in a test: it is what the weights looked like before anything was
    /// fitted.
    pub fn accepted_prior() -> Self {
        Self::new(ACCEPTED_PRIOR_ARM_NAME, RewardPolicy::default())
    }
}

// ---------------------------------------------------------------------------
// Bandit statistics
// ---------------------------------------------------------------------------

/// What the bandit knows about one arm, in full.
///
/// Every number the selection rule used is here, so the choice can be recomputed
/// by hand from the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmSelectionStatistics {
    /// The arm's name.
    pub arm: String,
    /// The arm's weights.
    pub policy: RewardPolicy,
    /// How many rows the seeded replay assigned to this arm.
    pub observations: usize,
    /// The arm's mean outcome-proxy reward over those rows.
    pub mean_outcome_proxy_reward: f64,
    /// Sample variance of that reward, over `observations - 1` degrees of
    /// freedom.
    pub reward_variance: f64,
    /// `sqrt(variance / observations)`; the uncertainty estimate.
    pub reward_standard_error: f64,
    /// Total pulls across every arm, which is what the bonus scales against.
    pub total_observations: usize,
    /// `exploration_c * sqrt(2 ln(total) / observations)`.
    pub exploration_bonus: f64,
    /// `mean + bonus`. The number the selection maximizes.
    pub ucb_score: f64,
}

/// The measured outcome profile of one arm over the whole evaluation partition.
///
/// The four raw outcome rates are computed straight from the records, and the
/// reward dimension is scored on one fixed evaluation scale shared by every arm
/// rather than on the arm's own policy. A reward weighting therefore cannot
/// argue for its own approval: when every arm is measured on the same rows the
/// reward delta is identically zero and the gate has no improvement to accept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmSafetyMetrics {
    /// The arm's name.
    pub arm: String,
    /// Mean outcome-proxy reward over every evaluation row, on the fixed
    /// evaluation scale [`safety_evaluation_policy`] selects - never on this
    /// arm's own weights.
    pub mean_outcome_proxy_reward: f64,
    /// `1 - success_rate`.
    pub failure_rate: f64,
    /// Mean measured cost over the rows that carry one.
    pub mean_cost: f64,
    /// The configured tail latency percentile, in milliseconds, over the rows
    /// that carry a latency measurement.
    pub tail_latency_ms: f64,
    /// Which percentile `tail_latency_ms` is. Published so the number is not
    /// read as a mean.
    pub tail_percentile: f64,
    /// Fraction of rows that took at least one fallback.
    pub fallback_rate: f64,
    /// The accepted evaluator's own metrics for the same rows, as a
    /// cross-check. `ml::evaluation` is consumed here, not restated.
    pub accepted_metrics: RoutingMetrics,
}

/// What the evaluation partition actually contains.
///
/// Published so an `InsufficientEvidence` verdict is legible: this is *why* the
/// comparison could not be made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyEvidence {
    /// Rows in the evaluation partition.
    pub total: usize,
    /// Rows labelled a success.
    pub successes: usize,
    /// Rows labelled a non-success.
    pub failures: usize,
    /// Rows carrying a cost measurement.
    pub cost_observations: usize,
    /// Rows carrying a latency measurement.
    pub latency_observations: usize,
    /// Rows that took at least one fallback.
    pub fallback_observations: usize,
    /// Rows whose sample carried a `Feedback` signal, counted and never read.
    pub feedback_signals_present: usize,
}

// ---------------------------------------------------------------------------
// The safety gate
// ---------------------------------------------------------------------------

/// One named way a candidate failed the safety gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyViolation {
    /// The candidate added no mean outcome-proxy reward over the reference. An
    /// evaluation that cannot separate two arms is evidence of nothing, so this
    /// is a violation and not a pass.
    NoRewardImprovement { delta: f64, required: f64 },
    /// The candidate failed more often.
    FailureRateRegressed { delta: f64, allowed: f64 },
    /// The candidate cost more.
    CostRegressed { delta: f64, allowed: f64 },
    /// The candidate's tail latency got worse.
    TailLatencyRegressed { delta_ms: f64, allowed_ms: f64 },
    /// The candidate fell back more often.
    FallbackRateRegressed { delta: f64, allowed: f64 },
}

impl SafetyViolation {
    /// Which measured quantity regressed, for a message.
    pub fn dimension(&self) -> &'static str {
        match self {
            Self::NoRewardImprovement { .. } => "mean_outcome_proxy_reward",
            Self::FailureRateRegressed { .. } => "failure_rate",
            Self::CostRegressed { .. } => "mean_cost",
            Self::TailLatencyRegressed { .. } => "tail_latency_ms",
            Self::FallbackRateRegressed { .. } => "fallback_rate",
        }
    }

    /// The measured regression, signed as an increase in the dimension named by
    /// [`SafetyViolation::dimension`]. For
    /// [`SafetyViolation::NoRewardImprovement`] it is the reward shortfall,
    /// which is the negation of the improvement.
    pub fn magnitude(&self) -> f64 {
        match self {
            Self::NoRewardImprovement { delta, .. } => -*delta,
            Self::FailureRateRegressed { delta, .. }
            | Self::CostRegressed { delta, .. }
            | Self::FallbackRateRegressed { delta, .. } => *delta,
            Self::TailLatencyRegressed { delta_ms, .. } => *delta_ms,
        }
    }
}

/// The safety verdict.
///
/// Three states, and only the first is an acceptance. This is a *value*, and
/// it is recomputed from the measured deltas and the tolerances on every call —
/// see [`SafetyEvaluation::verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyVerdict {
    /// The candidate beat the reference on mean outcome-proxy reward and
    /// regressed nothing.
    Accept,
    /// The candidate regressed at least one gated quantity, or did not actually
    /// improve the mean. Not an improvement, whichever it was.
    Reject,
    /// The comparison could not be made. Withholds acceptance: an evaluation
    /// that cannot decide is not a pass.
    InsufficientEvidence,
}

impl SafetyVerdict {
    /// Whether this verdict is an acceptance.
    pub fn is_acceptance(self) -> bool {
        matches!(self, Self::Accept)
    }
}

/// The safety comparison of one candidate against the reference arm.
///
/// Holds the deltas and the tolerances, and *derives* the violations and the
/// verdict. Nothing here is a stored boolean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyEvaluation {
    /// The reference arm's name.
    pub reference: String,
    /// The candidate arm's name.
    pub candidate: String,
    /// Rows the comparison was made over. The same rows for both arms, so the
    /// comparison is paired.
    pub evaluation_samples: usize,
    /// The reference arm's measured profile.
    pub reference_metrics: ArmSafetyMetrics,
    /// The candidate arm's measured profile.
    pub candidate_metrics: ArmSafetyMetrics,
    /// `candidate.mean_outcome_proxy_reward - reference.mean_outcome_proxy_reward`;
    /// positive is an improvement.
    pub mean_reward_delta: f64,
    /// `candidate.failure_rate - reference.failure_rate`; positive is a
    /// regression.
    pub failure_rate_delta: f64,
    /// `candidate.mean_cost - reference.mean_cost`; positive is a regression.
    pub mean_cost_delta: f64,
    /// `candidate.tail_latency_ms - reference.tail_latency_ms`; positive is a
    /// regression.
    pub tail_latency_delta_ms: f64,
    /// `candidate.fallback_rate - reference.fallback_rate`; positive is a
    /// regression.
    pub fallback_rate_delta: f64,
    /// The tolerances this comparison was judged against, stored so the verdict
    /// is a pure function of the report.
    pub tolerances: SafetyTolerances,
    /// Why the evidence was insufficient, when it was. `None` when the
    /// comparison could be made.
    pub insufficient_reason: Option<String>,
}

impl SafetyEvaluation {
    /// Recompute the named violations from the stored deltas and tolerances.
    ///
    /// Recomputed rather than stored: a violation that was written into the
    /// report at evaluation time could later disagree with the numbers next to
    /// it, and the gate's whole job is to be the thing that cannot drift.
    pub fn violations(&self) -> Vec<SafetyViolation> {
        let mut violations = Vec::new();
        if self.mean_reward_delta <= self.tolerances.min_reward_improvement {
            violations.push(SafetyViolation::NoRewardImprovement {
                delta: self.mean_reward_delta,
                required: self.tolerances.min_reward_improvement,
            });
        }
        if self.failure_rate_delta > self.tolerances.max_failure_rate_regression {
            violations.push(SafetyViolation::FailureRateRegressed {
                delta: self.failure_rate_delta,
                allowed: self.tolerances.max_failure_rate_regression,
            });
        }
        if self.mean_cost_delta > self.tolerances.max_cost_regression {
            violations.push(SafetyViolation::CostRegressed {
                delta: self.mean_cost_delta,
                allowed: self.tolerances.max_cost_regression,
            });
        }
        if self.tail_latency_delta_ms > self.tolerances.max_tail_latency_regression_ms {
            violations.push(SafetyViolation::TailLatencyRegressed {
                delta_ms: self.tail_latency_delta_ms,
                allowed_ms: self.tolerances.max_tail_latency_regression_ms,
            });
        }
        if self.fallback_rate_delta > self.tolerances.max_fallback_rate_regression {
            violations.push(SafetyViolation::FallbackRateRegressed {
                delta: self.fallback_rate_delta,
                allowed: self.tolerances.max_fallback_rate_regression,
            });
        }
        violations
    }

    /// Recompute the verdict. `InsufficientEvidence` outranks everything, so an
    /// undecidable comparison can never be reported as a pass.
    pub fn verdict(&self) -> SafetyVerdict {
        if self.insufficient_reason.is_some() {
            return SafetyVerdict::InsufficientEvidence;
        }
        if self.violations().is_empty() {
            SafetyVerdict::Accept
        } else {
            SafetyVerdict::Reject
        }
    }
}

// ---------------------------------------------------------------------------
// Safety evaluation
// ---------------------------------------------------------------------------

/// A percentile over a sorted slice, using the accepted evaluator's definition.
///
/// This is the same rule as `ml::evaluation`'s private `percentile`: linear
/// interpolation at `p * (n - 1)`. It is restated here only because that
/// function is not public, and `percentile_matches_accepted_evaluator` pins the
/// restatement to the accepted one at p95.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let index = p * (sorted.len() - 1) as f64;
    let low = index.floor() as usize;
    let high = index.ceil() as usize;
    if low == high {
        sorted[low]
    } else {
        let fraction = index - low as f64;
        sorted[low] * (1.0 - fraction) + sorted[high] * fraction
    }
}

/// The one reward scale every arm is measured on for the safety comparison.
///
/// The evaluation partition is the same recorded rows for every arm, so a
/// per-arm scale would let a pure rescale of the arm's weights read as an
/// improvement: the raw outcome metrics are identical and the only number that
/// moved would be the arm's own units. Scoring every arm on one fixed,
/// arm-independent policy keeps the reward dimension comparable. When the arms
/// really did see the same rows the dimension is then identically zero, which
/// is the honest report - there is no arm-dependent outcome evidence to claim
/// an improvement from - and `NoRewardImprovement` withholds acceptance rather
/// than manufacturing one.
fn safety_evaluation_policy() -> RewardPolicy {
    RewardPolicy::default()
}

/// Measure one arm over the whole evaluation partition.
///
/// Every metric here is a raw outcome measurement, and the reward dimension is
/// scored on [`safety_evaluation_policy`] rather than on `arm.policy`, so no
/// metric can be moved by an arm's own weighting.
pub(super) fn arm_safety_metrics(
    arm: &RewardArm,
    rows: &[&Row],
    config: &SafetyConfig,
) -> Result<ArmSafetyMetrics, BanditError> {
    if rows.is_empty() {
        return Err(BanditError::EvaluationSetTooSmall {
            min: config.min_evaluation_samples,
            found: 0,
        });
    }
    let total = rows.len();
    let reward = RewardComputer::new(safety_evaluation_policy());
    let mut reward_total = 0.0;
    let mut failures = 0usize;
    let mut cost_total = 0.0;
    let mut cost_observed = 0usize;
    let mut latencies: Vec<f64> = Vec::new();
    let mut fallbacks = 0usize;

    for row in rows {
        let attempt = reward.compute_attempt(
            row.proxy.success,
            row.proxy.latency_ms.unwrap_or(0.0),
            row.proxy.cost.unwrap_or(0.0),
            row.proxy.is_fallback(),
        );
        let request = reward.compute_request(
            std::slice::from_ref(&attempt),
            row.proxy.switch_count(),
            1.0,
        );
        if !request.total.is_finite() {
            return Err(BanditError::NonFiniteScore {
                arm: arm.name.clone(),
                value: request.total,
            });
        }
        reward_total += request.total;
        if !row.proxy.success {
            failures += 1;
        }
        if let Some(cost) = row.proxy.cost {
            cost_total += cost;
            cost_observed += 1;
        }
        if let Some(latency) = row.proxy.latency_ms {
            latencies.push(latency);
        }
        if row.proxy.is_fallback() {
            fallbacks += 1;
        }
    }

    if latencies.is_empty() {
        return Err(BanditError::NoLatencyObserved);
    }
    latencies.sort_by(f64::total_cmp);

    let legacy: Vec<DatasetTrainingSample> = rows.iter().map(|row| row.legacy.clone()).collect();
    Ok(ArmSafetyMetrics {
        arm: arm.name.clone(),
        mean_outcome_proxy_reward: reward_total / total as f64,
        failure_rate: failures as f64 / total as f64,
        mean_cost: if cost_observed == 0 {
            0.0
        } else {
            cost_total / cost_observed as f64
        },
        tail_latency_ms: percentile(&latencies, DEFAULT_TAIL_PERCENTILE),
        tail_percentile: DEFAULT_TAIL_PERCENTILE,
        fallback_rate: fallbacks as f64 / total as f64,
        accepted_metrics: RoutingMetrics::from_samples(&legacy),
    })
}

/// What the evaluation partition contains.
pub(super) fn evidence_of(rows: &[&Row]) -> SafetyEvidence {
    let mut evidence = SafetyEvidence {
        total: rows.len(),
        ..SafetyEvidence::default()
    };
    for row in rows {
        if row.proxy.success {
            evidence.successes += 1;
        } else {
            evidence.failures += 1;
        }
        if row.proxy.cost.is_some() {
            evidence.cost_observations += 1;
        }
        if row.proxy.latency_ms.is_some() {
            evidence.latency_observations += 1;
        }
        if row.proxy.is_fallback() {
            evidence.fallback_observations += 1;
        }
        if !row.legacy.feedback.is_empty() {
            evidence.feedback_signals_present += 1;
        }
    }
    evidence
}

/// Compare the selected arm against the reference and derive the verdict.
pub(super) fn compare_to_reference(
    candidate: &str,
    reference: &str,
    metrics: &[ArmSafetyMetrics],
    evidence: &SafetyEvidence,
    config: &SafetyConfig,
) -> Result<SafetyEvaluation, BanditError> {
    let find = |name: &str| {
        metrics
            .iter()
            .find(|entry| entry.arm == name)
            .ok_or_else(|| BanditError::ReferenceArmUnknown {
                name: name.to_string(),
            })
    };
    let candidate_metrics = find(candidate)?;
    let reference_metrics = find(reference)?;

    // Evidence first. An undecidable comparison is reported as undecidable, and
    // `SafetyVerdict::InsufficientEvidence` is not an acceptance, so a partition
    // that cannot support the gate withholds it rather than passing it.
    let insufficient_reason = insufficient_reason(evidence, config);
    if let Some(reason) = &insufficient_reason {
        return Ok(SafetyEvaluation {
            reference: reference.to_string(),
            candidate: candidate.to_string(),
            evaluation_samples: evidence.total,
            reference_metrics: reference_metrics.clone(),
            candidate_metrics: candidate_metrics.clone(),
            mean_reward_delta: 0.0,
            failure_rate_delta: 0.0,
            mean_cost_delta: 0.0,
            tail_latency_delta_ms: 0.0,
            fallback_rate_delta: 0.0,
            tolerances: SafetyTolerances::from(config),
            insufficient_reason: Some(reason.clone()),
        });
    }

    Ok(SafetyEvaluation {
        reference: reference.to_string(),
        candidate: candidate.to_string(),
        evaluation_samples: evidence.total,
        reference_metrics: reference_metrics.clone(),
        candidate_metrics: candidate_metrics.clone(),
        mean_reward_delta: candidate_metrics.mean_outcome_proxy_reward
            - reference_metrics.mean_outcome_proxy_reward,
        failure_rate_delta: candidate_metrics.failure_rate - reference_metrics.failure_rate,
        mean_cost_delta: candidate_metrics.mean_cost - reference_metrics.mean_cost,
        tail_latency_delta_ms: candidate_metrics.tail_latency_ms
            - reference_metrics.tail_latency_ms,
        fallback_rate_delta: candidate_metrics.fallback_rate - reference_metrics.fallback_rate,
        tolerances: SafetyTolerances::from(config),
        insufficient_reason: None,
    })
}

/// Why the evidence cannot decide, or `None` when it can.
pub(super) fn insufficient_reason(
    evidence: &SafetyEvidence,
    config: &SafetyConfig,
) -> Option<String> {
    if evidence.total < config.min_evaluation_samples {
        return Some(format!(
            "the evaluation partition holds {} rows, fewer than the safety floor of {}",
            evidence.total, config.min_evaluation_samples
        ));
    }
    if evidence.successes == 0 {
        return Some(
            "the evaluation partition holds no success, so a failure rate cannot be compared"
                .to_string(),
        );
    }
    if evidence.failures == 0 {
        return Some(
            "the evaluation partition holds no failure, so a failure rate cannot be compared"
                .to_string(),
        );
    }
    if evidence.cost_observations == 0 {
        return Some(
            "the evaluation partition holds no cost measurement, so a cost comparison cannot be made"
                .to_string(),
        );
    }
    if evidence.latency_observations < 2 {
        return Some(
            "the evaluation partition holds fewer than two latency measurements, so a tail latency \
             percentile cannot be placed"
                .to_string(),
        );
    }
    None
}
