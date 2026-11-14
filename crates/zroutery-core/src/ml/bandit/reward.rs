use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::BanditError;
use super::{
    BASIS_WIDTH, B_COST, B_FALLBACK, B_LATENCY, B_SUCCESS, B_SWITCH, B_UNCERTAINTY, WEIGHT_NAMES,
};
use crate::ml::dataset::{OutcomeTrainingSample, Targets, TrainingSample as DatasetTrainingSample};
use crate::ml::reward::{RewardComputer, RewardPolicy};

// ---------------------------------------------------------------------------
// OutcomeProxy — the measured facts, and the only thing this node can learn from
// ---------------------------------------------------------------------------

/// The raw outcome measurements for one request.
///
/// This is the *entire* input vocabulary of the learned reward. There is no
/// rating, no `Feedback`, and no preference field, and there is no field one
/// could be added to without changing this type's shape in review.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutcomeProxy {
    /// Did the request end in a terminal success?
    pub success: bool,
    /// Measured latency in milliseconds; `None` when nothing completed.
    pub latency_ms: Option<f64>,
    /// Measured cost; `None` when no billing fact was captured.
    pub cost: Option<f64>,
    /// How many fallbacks the request took.
    pub fallback_count: u32,
}

impl OutcomeProxy {
    /// Project the outcome targets. Takes a [`Targets`] and nothing else.
    pub fn from_targets(targets: &Targets) -> Self {
        Self {
            success: targets.success,
            latency_ms: targets.latency_ms,
            cost: targets.cost,
            fallback_count: targets.fallback_count,
        }
    }

    /// Whether this request took at least one fallback.
    pub fn is_fallback(&self) -> bool {
        self.fallback_count > 0
    }

    /// The declared switch proxy: each recorded fallback is one candidate switch
    /// that actually happened.
    ///
    /// The accepted `RewardComputer::compute_request` takes a `switch_count`, and
    /// a dataset row records no separate switch counter. `fallback_count` is the
    /// switch count that was genuinely observed, so it is used rather than
    /// inventing one.
    pub fn switch_count(&self) -> u32 {
        self.fallback_count
    }

    /// Latency as `compute_attempt` prices it, or `None` when unmeasured.
    pub fn measured_latency_ms(&self) -> Option<f64> {
        self.latency_ms
    }
}

/// The published outcome order, as a comparison. See [`OUTCOME_PROXY_ORDER`].
///
/// Every term is a measurement. Two requests that agree on all four are
/// genuinely indistinguishable from recorded outcomes alone, and this returns
/// `Equal` rather than inventing a preference between them.
pub fn compare_outcome_proxy(left: &OutcomeProxy, right: &OutcomeProxy) -> Ordering {
    match (left.success, right.success) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let left_latency = left.latency_ms.unwrap_or(f64::INFINITY);
    let right_latency = right.latency_ms.unwrap_or(f64::INFINITY);
    let ordering = left_latency.total_cmp(&right_latency);
    if ordering != Ordering::Equal {
        return ordering;
    }
    let left_cost = left.cost.unwrap_or(f64::INFINITY);
    let right_cost = right.cost.unwrap_or(f64::INFINITY);
    let ordering = left_cost.total_cmp(&right_cost);
    if ordering != Ordering::Equal {
        return ordering;
    }
    left.fallback_count.cmp(&right.fallback_count)
}

// ---------------------------------------------------------------------------
// RewardBasis — the accepted score, decomposed exactly
// ---------------------------------------------------------------------------

/// The accepted reward score for one request, as a linear function of weights.
///
/// The columns are the accepted score's own terms, read off
/// [`RewardComputer::compute_attempt`] and [`RewardComputer::compute_request`]:
///
/// | column | accepted term | weight field |
/// |---|---|---|
/// | [`B_SUCCESS`] | `+/- success_weight` | `success_weight` |
/// | [`B_LATENCY`] | `-latency_weight * min(latency_ms/1000, 1)` | `latency_weight` |
/// | [`B_COST`] | `-cost_weight * min(cost, 1)` | `cost_weight` |
/// | [`B_FALLBACK`] | `fallback_penalty` when a fallback was taken | `fallback_penalty` |
/// | [`B_SWITCH`] | `switch_cost * switch_count` | `switch_cost` |
/// | [`B_UNCERTAINTY`] | `-uncertainty_weight * (1 - confidence)` | `uncertainty_weight` |
///
/// Because the decomposition is exact, a fitted weight vector *is* a
/// [`RewardPolicy`]: [`RewardBasis::score`] and
/// [`accepted_outcome_proxy_score`] agree to the last bit, and
/// `basis_reproduces_accepted_score` proves it rather than asserting it.
///
/// A request with no latency measurement contributes `0.0` to the latency
/// column, and one with no cost measurement contributes `0.0` to the cost
/// column. That is not a imputation invented here: it is what the accepted
/// `compute_attempt` does when handed the absent measurement, and the accepted
/// function prices only what was measured. A failed request is not credited with
/// a latency win, and is not charged for a latency nobody observed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RewardBasis {
    /// One column per `RewardPolicy` field; see the table above.
    pub values: [f64; BASIS_WIDTH],
}

impl RewardBasis {
    /// Build the basis row for one request's outcome.
    ///
    /// * `switch_count` — the observed switch proxy; see
    ///   [`OutcomeProxy::switch_count`].
    /// * `confidence` — the confidence the accepted scorer would have reported.
    ///   The bandit always passes `1.0`, and
    ///   [`RewardFitReport::unidentifiable_weights`] explains why: a dataset row
    ///   carries no decision-time confidence, so any other choice would be
    ///   invented, and inventing it would let an arm be rewarded for a number
    ///   that has nothing to do with the recorded outcome.
    pub fn from_targets(targets: &Targets, switch_count: u32, confidence: f64) -> Self {
        let mut values = [0.0; BASIS_WIDTH];
        values[B_SUCCESS] = if targets.success { 1.0 } else { -1.0 };
        // `min(x, 1000.0) / 1000.0` is `min(x / 1000.0, 1.0)` for a latency
        // that is non-negative by the row validator, written this way so an
        // absurd latency cannot overflow the division.
        values[B_LATENCY] = -targets.latency_ms.unwrap_or(0.0).clamp(0.0, 1000.0) / 1000.0;
        values[B_COST] = -targets.cost.unwrap_or(0.0).clamp(0.0, 1.0);
        values[B_FALLBACK] = if targets.fallback_count > 0 { 1.0 } else { 0.0 };
        values[B_SWITCH] = f64::from(switch_count);
        values[B_UNCERTAINTY] = -(1.0 - confidence);
        Self { values }
    }

    /// The score this basis assigns under `policy`, through the accepted
    /// computer.
    pub fn score(&self, policy: &RewardPolicy) -> f64 {
        dot(&self.values, &weights_vector(policy))
    }

    /// Whether every column is finite.
    pub fn is_finite(&self) -> bool {
        self.values.iter().all(|value| value.is_finite())
    }
}

/// The six `RewardPolicy` fields as a basis-column-ordered vector.
pub fn weights_vector(policy: &RewardPolicy) -> [f64; BASIS_WIDTH] {
    [
        policy.success_weight,
        policy.latency_weight,
        policy.cost_weight,
        policy.fallback_penalty,
        policy.switch_cost,
        policy.uncertainty_weight,
    ]
}

/// The inverse of [`weights_vector`]: a fitted vector becomes a policy.
///
/// Fitted weights *are* `RewardPolicy` weights. There is no second reward
/// representation to keep in sync.
pub fn policy_from_weights(weights: [f64; BASIS_WIDTH]) -> RewardPolicy {
    RewardPolicy {
        success_weight: weights[B_SUCCESS],
        latency_weight: weights[B_LATENCY],
        cost_weight: weights[B_COST],
        fallback_penalty: weights[B_FALLBACK],
        switch_cost: weights[B_SWITCH],
        uncertainty_weight: weights[B_UNCERTAINTY],
    }
}

/// The accepted score for one request, computed through the accepted
/// [`RewardComputer`] itself.
///
/// Published so the exactness of [`RewardBasis`] can be checked against the
/// accepted surface rather than against a restatement of it.
pub fn accepted_outcome_proxy_score(
    proxy: &OutcomeProxy,
    switch_count: u32,
    confidence: f64,
    policy: &RewardPolicy,
) -> f64 {
    let computer = RewardComputer::new(policy.clone());
    let attempt = computer.compute_attempt(
        proxy.success,
        proxy.latency_ms.unwrap_or(0.0),
        proxy.cost.unwrap_or(0.0),
        proxy.is_fallback(),
    );
    computer
        .compute_request(std::slice::from_ref(&attempt), switch_count, confidence)
        .total
}

/// Inner product over the basis width.
fn dot(left: &[f64; BASIS_WIDTH], right: &[f64; BASIS_WIDTH]) -> f64 {
    let mut total = 0.0;
    for index in 0..BASIS_WIDTH {
        total += left[index] * right[index];
    }
    total
}

// ---------------------------------------------------------------------------
// Row — one projected dataset row
// ---------------------------------------------------------------------------

/// One canonical sample, projected into the two things this node needs from it.
pub(super) struct Row {
    pub(super) sample_id: String,
    pub(super) cell: String,
    pub(super) proxy: OutcomeProxy,
    pub(super) basis: RewardBasis,
    pub(super) legacy: DatasetTrainingSample,
}

/// The routing cell a row belongs to.
///
/// A cell is a `(provider, model, dialect, streaming)` tuple: requests
/// production already treated as alike. Pairs are only ever formed *within* a
/// cell, so a pair is a comparison production effectively made anyway, and no
/// comparison can be an artifact of two unrelated routes.
pub(super) fn cell_key(sample: &OutcomeTrainingSample) -> String {
    format!(
        "{}|{}|{}|{}",
        sample.provider_id, sample.model_id, sample.dialect, sample.streaming
    )
}

// ---------------------------------------------------------------------------
// Reward fitting
// ---------------------------------------------------------------------------

/// One comparable pair, as a difference row and a label.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pair {
    pub(super) difference: [f64; BASIS_WIDTH],
    pub(super) label: f64,
}

/// Whether a fitted reward reproduces the declared order better than its prior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardFitVerdict {
    /// Strictly better out-of-sample agreement with the declared order.
    Better,
    /// The holdout did not separate the fit from the prior.
    NotBetter,
    /// Out-of-sample agreement got worse.
    Worse,
}

impl RewardFitVerdict {
    /// Whether this verdict claims a better learned reward at all.
    pub fn is_improvement(self) -> bool {
        matches!(self, Self::Better)
    }

    /// Withhold an improvement claim a degenerate holdout cannot support.
    ///
    /// A holdout whose pairs are *all* decided by the success column cannot
    /// demonstrate that the other five weights learned anything: a prior with a
    /// large enough `success_weight` also orders every such pair correctly, so a
    /// better score is a fact about the holdout's base rate rather than about the
    /// fit. That is a fact about the data, not about the weights, so it is not
    /// reported as an improvement. A `Worse` verdict survives: a fit that is
    /// confidently wrong on such a holdout really is worse.
    ///
    /// See [`holdout_pairs_only_differ_on_success`], which is the only caller, so
    /// the downgrading rule and its trigger are in one place.
    pub fn downgraded_to_not_better(self) -> Self {
        match self {
            Self::Better => Self::NotBetter,
            other => other,
        }
    }
}

/// The honest report of one reward fit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardFitReport {
    /// [`OUTCOME_PROXY_FIT_TARGET`], verbatim.
    pub target_description: String,
    /// [`OUTCOME_PROXY_ORDER`], verbatim.
    pub order_description: String,
    /// [`REWARD_FIT_TARGET_DESCRIPTION`], verbatim: what was fit, and what it
    /// is not.
    pub outcome_proxy_not_preference: String,
    /// Cells in the snapshot.
    pub cells_total: usize,
    /// Cells the fit was computed on.
    pub fit_cells: usize,
    /// Cells held out, which are also the bandit replay set and the safety
    /// evaluation set.
    pub holdout_cells: usize,
    /// Rows in the fit partition.
    pub fit_rows: usize,
    /// Rows in the holdout partition.
    pub holdout_rows: usize,
    /// Comparable pairs in the fit partition.
    pub pairs_fit: usize,
    /// Comparable pairs in the holdout partition.
    pub pairs_holdout: usize,
    /// In-sample objective at the prior.
    pub fit_objective_at_prior: f64,
    /// In-sample objective at the fitted weights. Lower is better; this is an
    /// in-sample number and is not evidence of anything out of sample.
    pub fit_objective_fitted: f64,
    /// Out-of-sample agreement with the declared order, at the prior.
    pub prior_holdout_agreement: f64,
    /// Out-of-sample agreement with the declared order, at the fitted weights.
    pub fitted_holdout_agreement: f64,
    /// `fitted_holdout_agreement - prior_holdout_agreement`; positive is better.
    pub agreement_delta: f64,
    /// The out-of-sample verdict. This is the one that matters; the objectives
    /// above are in-sample.
    pub verdict: RewardFitVerdict,
    /// The hand-set prior the fit started from.
    pub prior: RewardPolicy,
    /// The prior weight vector, in [`RewardBasis`] column order.
    pub prior_weights: Vec<f64>,
    /// The fitted weights, which are a `RewardPolicy`.
    pub fitted: RewardPolicy,
    /// The fitted weight vector, in [`RewardBasis`] column order.
    pub fitted_weights: Vec<f64>,
    /// Weights the outcome data cannot identify, with the reason. See
    /// [`UNIDENTIFIABLE_WEIGHT`].
    pub unidentifiable_weights: Vec<String>,
    /// Why those weights are unidentifiable, in words.
    pub unidentifiable_reason: String,
    /// How many rows in the snapshot carried a `Feedback` signal. Counted, never
    /// read: it is published so a reader can see the learner had the
    /// opportunity and declined it. It is not a reward input and cannot become
    /// one.
    pub feedback_signals_present: usize,
    /// Distinct outcome shapes in the snapshot, i.e. how much outcome variety
    /// there was to fit at all.
    pub distinct_outcome_shapes: usize,
}

impl RewardFitReport {
    /// Whether this report claims a better learned reward.
    ///
    /// There is no separate stored flag that could disagree with the verdict.
    pub fn is_improvement(&self) -> bool {
        self.verdict.is_improvement()
    }
}

/// How well a weight vector reproduces a pair list's declared order.
///
/// A pair scored exactly `0.0` counts as *incorrect*: a reward that cannot
/// separate two requests the declared order separates is not doing the job, and
/// crediting it would make the number flattering.
pub(super) fn agreement(pairs: &[Pair], weights: &[f64; BASIS_WIDTH]) -> Option<f64> {
    if pairs.is_empty() {
        return None;
    }
    let correct = pairs
        .iter()
        .filter(|pair| pair.label * dot(&pair.difference, weights) > 0.0)
        .count();
    Some(correct as f64 / pairs.len() as f64)
}

/// Numerically safe `ln(1 + e^z)`, in the form that cannot overflow for large
/// `|z|`.
pub(super) fn softplus(z: f64) -> f64 {
    if z > 0.0 {
        z + (-z).exp().ln_1p()
    } else {
        z.exp().ln_1p()
    }
}

/// Numerically safe logistic function.
pub(super) fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let exponential = z.exp();
        exponential / (1.0 + exponential)
    }
}

/// The ridge-regularized pairwise ranking objective.
pub(super) fn objective(
    pairs: &[Pair],
    weights: &[f64; BASIS_WIDTH],
    prior: &[f64; BASIS_WIDTH],
    lambda: f64,
) -> f64 {
    let mut ranking = 0.0;
    for pair in pairs {
        ranking += softplus(-pair.label * dot(&pair.difference, weights));
    }
    ranking /= pairs.len() as f64;
    let mut penalty = 0.0;
    for index in 0..BASIS_WIDTH {
        let delta = weights[index] - prior[index];
        penalty += delta * delta;
    }
    ranking + lambda * penalty
}

/// Full-batch gradient descent from the prior.
///
/// Deterministic by construction: fixed iteration count, fixed step size, fixed
/// initialization, pairs in canonical order, and no sampling anywhere. That is
/// why the fit needs no seed — the seed in [`SelectionConfig`] is for the replay
/// schedule, not for this.
pub(super) fn solve_weights(
    pairs: &[Pair],
    prior: [f64; BASIS_WIDTH],
    iterations: usize,
    learning_rate: f64,
    ridge_lambda: f64,
) -> Result<[f64; BASIS_WIDTH], BanditError> {
    let mut weights = prior;
    let count = pairs.len() as f64;
    for _ in 0..iterations {
        let mut gradient = [0.0f64; BASIS_WIDTH];
        for pair in pairs {
            let coefficient = -pair.label * sigmoid(-pair.label * dot(&pair.difference, &weights));
            for (accumulated, basis) in gradient.iter_mut().zip(pair.difference) {
                *accumulated += coefficient * basis;
            }
        }
        for index in 0..BASIS_WIDTH {
            let slope =
                gradient[index] / count + 2.0 * ridge_lambda * (weights[index] - prior[index]);
            if !slope.is_finite() {
                return Err(BanditError::NonFittedWeight {
                    component: WEIGHT_NAMES[index],
                    value: slope,
                });
            }
            weights[index] -= learning_rate * slope;
            if !weights[index].is_finite() {
                return Err(BanditError::NonFittedWeight {
                    component: WEIGHT_NAMES[index],
                    value: weights[index],
                });
            }
        }
    }
    Ok(weights)
}

/// Whether every pair in a list is decided by the success column alone.
///
/// A holdout in which no pair shares a success label cannot show that the
/// non-success weights were learned. `success_weight` alone explains all of its
/// pairs, so a better score there is a fact about the base rate.
pub(super) fn holdout_pairs_only_differ_on_success(pairs: &[Pair]) -> bool {
    !pairs.is_empty() && pairs.iter().all(|pair| pair.difference[B_SUCCESS] != 0.0)
}

/// Build every comparable pair within each cell, in canonical order.
pub(super) fn build_pairs(
    rows: &[&Row],
    max_pairs_per_cell: usize,
) -> Result<Vec<Pair>, BanditError> {
    let mut by_cell: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        by_cell.entry(row.cell.as_str()).or_default().push(row);
    }
    let mut pairs = Vec::new();
    for (cell, members) in by_cell {
        if members.len().saturating_sub(1) * members.len() / 2 > max_pairs_per_cell {
            return Err(BanditError::CellTooLargeToPair {
                cell: cell.to_string(),
                rows: members.len(),
                max_pairs: max_pairs_per_cell,
            });
        }
        for left in 0..members.len() {
            for right in (left + 1)..members.len() {
                match compare_outcome_proxy(&members[left].proxy, &members[right].proxy) {
                    Ordering::Less => pairs.push(Pair {
                        difference: difference(&members[left].basis, &members[right].basis),
                        label: 1.0,
                    }),
                    Ordering::Greater => pairs.push(Pair {
                        difference: difference(&members[right].basis, &members[left].basis),
                        label: 1.0,
                    }),
                    Ordering::Equal => {}
                }
            }
        }
    }
    Ok(pairs)
}

fn difference(left: &RewardBasis, right: &RewardBasis) -> [f64; BASIS_WIDTH] {
    let mut result = [0.0; BASIS_WIDTH];
    for (index, slot) in result.iter_mut().enumerate() {
        *slot = left.values[index] - right.values[index];
    }
    result
}
