use serde::{Deserialize, Serialize};

use super::{ACCEPTED_PRIOR_ARM_NAME, DEFAULT_SEED};
use crate::ml::reward::RewardPolicy;

// ---------------------------------------------------------------------------
// BanditConfig
// ---------------------------------------------------------------------------

/// Configuration of the reward fit.
///
/// `RewardPolicy` is not `PartialEq`, so neither is this: a run's configuration is
/// compared through the report it produces, not by comparing the config values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardFitConfig {
    /// The hand-set weights the fit starts from and is regularized toward. The
    /// accepted `RewardPolicy::default()` by default: the fit is a correction of
    /// the accepted hand-set policy, not a replacement invented from nothing.
    pub prior: RewardPolicy,
    /// Trailing cells reserved as the out-of-sample holdout, which is also the
    /// common evaluation set the bandit replays and the safety gate compares on.
    /// Must be at least 1 and fewer than the number of cells present.
    pub holdout_cells: usize,
    /// Full-batch gradient-descent iterations. A fixed count is what makes the
    /// fit reproducible without a seed.
    pub iterations: usize,
    /// Gradient-descent step size. Must be finite and positive.
    pub learning_rate: f64,
    /// L2 pull toward `prior`. Must be finite and non-negative. At zero the
    /// six columns are unregularized and the fit is free to run away on a
    /// direction the data barely constrains.
    pub ridge_lambda: f64,
    /// Fewest comparable pairs the fit partition may yield. Below this the fit
    /// is refused rather than fitted on almost nothing.
    pub min_pair_count: usize,
    /// Ceiling on the pairs one cell may contribute, so a pathological cell
    /// cannot turn an offline run into an unbounded computation. Exceeding it
    /// is a refusal, not a truncation.
    pub max_pairs_per_cell: usize,
}

impl Default for RewardFitConfig {
    fn default() -> Self {
        Self {
            prior: RewardPolicy::default(),
            holdout_cells: 1,
            iterations: 400,
            learning_rate: 0.05,
            ridge_lambda: 0.01,
            min_pair_count: 16,
            max_pairs_per_cell: 4096,
        }
    }
}

/// Configuration of the bandit selection phase.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SelectionConfig {
    /// Replay assignment seed. Recorded in every report.
    pub seed: u64,
    /// The UCB1 exploration coefficient. Must be finite and non-negative. At
    /// zero the rule is `argmax(mean)`; a positive value is what makes it a
    /// bandit.
    pub exploration_c: f64,
    /// Fewest total pulls the replay may produce before selection is refused.
    pub min_total_observations: usize,
    /// Fewest pulls *each* arm must have received. Must be at least 1, because
    /// an arm with no observation has no estimate and no standard error, and
    /// scoring one anyway would be inventing its mean.
    pub min_arm_observations: usize,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            exploration_c: 0.5,
            min_total_observations: 32,
            min_arm_observations: 4,
        }
    }
}

/// Configuration of the safety gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// The arm the candidate is compared against. Must name a declared arm.
    pub reference_arm: String,
    /// Fewest rows the evaluation partition may hold. At least 2, because a
    /// single row cannot support a comparison.
    pub min_evaluation_samples: usize,
    /// How much mean outcome-proxy reward the candidate must add, strictly.
    /// Must be finite and positive: zero would make "no worse" an acceptance,
    /// and non-finite would make the gate vacuous.
    pub min_reward_improvement: f64,
    /// Largest tolerated increase in failure rate. Default 0.0: no regression.
    pub max_failure_rate_regression: f64,
    /// Largest tolerated increase in mean cost. Default 0.0: no regression.
    pub max_cost_regression: f64,
    /// Largest tolerated increase in the tail latency percentile, in
    /// milliseconds. Default 0.0: no regression.
    pub max_tail_latency_regression_ms: f64,
    /// Largest tolerated increase in fallback rate. Default 0.0: no regression.
    pub max_fallback_rate_regression: f64,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            reference_arm: ACCEPTED_PRIOR_ARM_NAME.to_string(),
            min_evaluation_samples: 8,
            min_reward_improvement: 1e-9,
            max_failure_rate_regression: 0.0,
            max_cost_regression: 0.0,
            max_tail_latency_regression_ms: 0.0,
            max_fallback_rate_regression: 0.0,
        }
    }
}

/// Everything one offline run is configured with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BanditConfig {
    /// The reward fit.
    pub reward: RewardFitConfig,
    /// The bandit selection phase.
    pub selection: SelectionConfig,
    /// The safety gate.
    pub safety: SafetyConfig,
}

/// The tolerances, as stored on a [`SafetyEvaluation`] so the verdict is a pure
/// function of the report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SafetyTolerances {
    pub min_reward_improvement: f64,
    pub max_failure_rate_regression: f64,
    pub max_cost_regression: f64,
    pub max_tail_latency_regression_ms: f64,
    pub max_fallback_rate_regression: f64,
}

impl From<&SafetyConfig> for SafetyTolerances {
    fn from(config: &SafetyConfig) -> Self {
        Self {
            min_reward_improvement: config.min_reward_improvement,
            max_failure_rate_regression: config.max_failure_rate_regression,
            max_cost_regression: config.max_cost_regression,
            max_tail_latency_regression_ms: config.max_tail_latency_regression_ms,
            max_fallback_rate_regression: config.max_fallback_rate_regression,
        }
    }
}

// ---------------------------------------------------------------------------
// BanditError
// ---------------------------------------------------------------------------

/// Every way an offline run refuses.
///
/// The run fails closed. A refusal is a typed value carrying the numbers that
/// caused it; there is no panic, no silently defaulted policy, and no partially
/// fitted result handed back as a success.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BanditError {
    /// The snapshot held no rows at all.
    #[error("bandit snapshot is empty")]
    EmptySnapshot,

    /// The snapshot held one sample id twice, so the canonical row order would
    /// not be a function of the snapshot's contents.
    #[error(
        "bandit snapshot holds sample id '{sample_id}' at indexes {first} and {second}; \
         the canonical row order would not be a function of the snapshot's contents"
    )]
    DuplicateSampleId {
        sample_id: String,
        first: usize,
        second: usize,
    },

    /// A row was encoded against a different schema than this build accepts.
    #[error(
        "bandit sample {index} carries {component} schema version {found}, expected {expected}"
    )]
    SchemaMismatch {
        index: usize,
        component: &'static str,
        found: u32,
        expected: u32,
    },

    /// A row's outcome measurement was not a finite number.
    #[error("bandit sample {index} has a non-finite outcome {field}: {value}")]
    NonFiniteOutcome {
        index: usize,
        field: &'static str,
        value: f64,
    },

    /// The canonical sample failed its own validator.
    #[error("bandit sample {index} is invalid: {reason}")]
    InvalidSample { index: usize, reason: String },

    /// Every row in the snapshot carries one identical outcome, so there is no
    /// outcome proxy to learn from and no order to reproduce.
    #[error(
        "bandit snapshot is degenerate: all {rows} rows carry one identical outcome, so there is \
         no outcome proxy to fit and no order for the bandit to select on"
    )]
    DegenerateReward { rows: usize },

    /// No arms were declared.
    #[error("bandit run declares no arms")]
    NoArms,

    /// An arm name was empty.
    #[error("bandit arm at index {index} has an empty name")]
    EmptyArmName { index: usize },

    /// Two declared arms share a name, so a report could not say which was which.
    #[error("bandit declares arm '{name}' more than once")]
    DuplicateArmName { name: String },

    /// A caller-declared arm used the name the fit reserves for itself.
    #[error("bandit arm '{name}' uses the name the fitted arm reserves; rename the declared arm")]
    ReservedArmName { name: String },

    /// The reference arm named by the configuration is not in the arm set.
    #[error("safety reference arm '{name}' is not one of the declared arms")]
    ReferenceArmUnknown { name: String },

    /// The holdout reservation would consume every cell, leaving nothing to fit.
    #[error("bandit holds out {holdout_cells} cells, but the snapshot only has {total}")]
    HoldoutTakesEveryCell { total: usize, holdout_cells: usize },

    /// The holdout reservation is zero, so nothing would be held out.
    #[error("reward fit must hold out at least one cell")]
    EmptyHoldoutCells,

    /// The fit was configured to take no gradient steps.
    #[error("reward fit must run at least one iteration")]
    EmptyFitIterations,

    /// The learning rate is not a usable positive finite number.
    #[error("reward fit learning rate {0} must be finite and positive")]
    MeaninglessLearningRate(f64),

    /// The ridge coefficient is not a usable non-negative finite number.
    #[error("reward fit ridge lambda {0} must be finite and non-negative")]
    MeaninglessRidgeLambda(f64),

    /// The pair floor is zero, so the fit would accept no data at all.
    #[error("reward fit must require at least one comparable pair")]
    EmptyPairFloor,

    /// The per-cell pair ceiling is zero, so no pair could ever be built.
    #[error("reward fit max_pairs_per_cell must be at least 1")]
    EmptyPairCeiling,

    /// One cell holds more rows than the configured pair ceiling allows. The run
    /// is refused rather than sampling the cell down, so the fit is never a
    /// silent subsample.
    #[error(
        "cell '{cell}' holds {rows} rows, which would build more than the {max_pairs} comparable \
         pairs this run is configured to build"
    )]
    CellTooLargeToPair {
        cell: String,
        rows: usize,
        max_pairs: usize,
    },

    /// The fit partition cannot support a fit.
    #[error(
        "the fit partition yields {pairs} comparable pairs, fewer than the configured floor of \
         {min_pairs}"
    )]
    InsufficientPairs { pairs: usize, min_pairs: usize },

    /// The holdout partition holds no comparable pair, so the fit cannot be
    /// checked out of sample and the run refuses instead of reporting an
    /// in-sample improvement as if it were a real one.
    #[error("the holdout partition yields no comparable pair, so the fit cannot be checked out of sample")]
    HoldoutNotComparable,

    /// The reward improvement floor is not a usable positive finite number. A
    /// floor of zero or less would let "no worse" pass as an improvement, and a
    /// non-finite floor would make the reward gate meaningless.
    #[error("safety min_reward_improvement {0} must be finite and positive")]
    MeaninglessSafetyFloor(f64),

    /// The safety evaluation floor is too small to compare anything with.
    #[error("safety min_evaluation_samples must be at least 2, got {0}")]
    MeaninglessEvaluationFloor(usize),

    /// A regression tolerance was not finite, which would make that gate
    /// vacuous — a gate that cannot fail is not a gate.
    #[error(
        "safety tolerance {name} = {value} must be finite; a gate that cannot fail is not a gate"
    )]
    MeaninglessTolerance { name: &'static str, value: f64 },

    /// A regression tolerance was negative, which is not a tolerance.
    #[error("safety tolerance {name} = {value} must be non-negative")]
    NegativeTolerance { name: &'static str, value: f64 },

    /// The exploration coefficient was not a usable non-negative finite number.
    #[error("selection exploration_c {0} must be finite and non-negative")]
    MeaninglessExploration(f64),

    /// The per-arm observation floor is zero, so an unobserved arm would be
    /// scored on an invented mean.
    #[error("selection min_arm_observations must be at least 1")]
    EmptyArmObservationFloor,

    /// The replay produced fewer total pulls than the configuration demands.
    #[error(
        "the replay produced {total} pulls in total, fewer than the configured floor of {min}; \
         there is not enough data to select over the arms"
    )]
    TooFewTotalObservations { total: usize, min: usize },

    /// One arm was pulled too few times to have a usable estimate.
    #[error(
        "arm '{name}' was pulled {observations} times, fewer than the configured floor of {min}; \
         its mean and standard error would be invented"
    )]
    ArmUnderObserved {
        name: String,
        observations: usize,
        min: usize,
    },

    /// The evaluation partition is smaller than the safety floor.
    #[error(
        "the evaluation partition holds {found} rows, fewer than the safety floor of {min}; \
         the comparison cannot be made"
    )]
    EvaluationSetTooSmall { min: usize, found: usize },

    /// The fit produced a weight that is not a finite number.
    #[error("reward fit produced a non-finite {component}: {value}")]
    NonFittedWeight { component: &'static str, value: f64 },

    /// An arm's mean outcome-proxy reward is not a finite number.
    #[error("arm '{arm}' scored a non-finite mean outcome-proxy reward: {value}")]
    NonFiniteScore { arm: String, value: f64 },

    /// The evaluation partition holds no latency observation, so a tail latency
    /// percentile cannot be placed and the gate refuses rather than reporting 0.0
    /// milliseconds of tail.
    #[error("the evaluation partition holds no latency observation, so no tail latency percentile can be placed")]
    NoLatencyObserved,
}
