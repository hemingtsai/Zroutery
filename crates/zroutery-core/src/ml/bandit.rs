//! Offline bandit selection over reward policies, and the safety gate that can
//! refuse the winner.
//!
//! # What this module is
//!
//! This node makes two things real that did not exist, and keeps both of them
//! offline and unreachable:
//!
//! 1. **A learned reward weighting.** [`ml::reward`](super::reward) is a
//!    hand-set weight vector: `RewardPolicy::default()` fixes six numbers that
//!    nobody fitted and nobody re-derives. [`fit_outcome_proxy_weights`] replaces
//!    "hand-set" with "fitted from data" for those exact six numbers, and says
//!    plainly what it fitted toward.
//! 2. **A selection rule over declared arms with inspectable state.**
//!    [`run_bandit`] replays a dataset snapshot over a declared set of
//!    [`RewardArm`]s, accumulates per-arm observation counts and an uncertainty
//!    estimate, and selects one by an UCB1 rule whose exploration bonus, seed and
//!    assignment schedule are all reported.
//!
//! The third thing, and the one that carries the most weight, is
//! [`SafetyVerdict`]: a *value* that withholds acceptance. A candidate that
//! raises mean outcome-proxy reward while regressing the failure rate, the cost,
//! or a tail latency percentile is **not** an improvement, and this module
//! reports it unsafe. See [The safety gate](#the-safety-gate).
//!
//! # What "reward learning" means here, exactly
//!
//! The reward being learned is a **linear outcome proxy**: the accepted
//! [`RewardComputer`] score, with its six weights fitted rather than set by hand.
//!
//! It is **not** a learned utility function, and it is **not** a learned
//! preference. The distinction is enforced by construction rather than by
//! documentation. [`RewardBasis`] is a six-column basis vector whose columns
//! are the accepted score's own terms, read off
//! [`RewardComputer::compute_attempt`] and [`RewardComputer::compute_request`]
//! rather than invented beside them. The fit searches for weights `w` in that
//! same six-dimensional space, and the result is converted straight back into a
//! [`RewardPolicy`]. There is no second reward representation to drift away
//! from the accepted one, and `basis_reproduces_accepted_score` proves the
//! decomposition is exact rather than approximate.
//!
//! What the fit minimizes is stated by [`OUTCOME_PROXY_FIT_TARGET`] and is a
//! *pairwise ranking* objective, not a regression onto a score the module also
//! chose. A request A must be ranked above a request B when A is the better
//! outcome by the published order in [`OUTCOME_PROXY_ORDER`]. The fit finds the
//! weights that best reproduce that declared order on the fit partition. The
//! declared order is a total order over *measurements production already
//! records* — did it succeed, how slow, how expensive, how many fallbacks — and
//! it is the module's own published objective, not a discovery about anyone.
//!
//! Rejected alternatives, and why:
//!
//! - **Online or reinforcement reward.** Forbidden by this node's scope, and
//!   unsafe in a way that needs no further argument: a reward learned from live
//!   traffic explores against real users. Everything here reads a frozen
//!   snapshot.
//! - **A second learned reward function** (a neural utility head, say) sitting
//!   beside `RewardPolicy`. That would put two disagreeing notions of reward in
//!   the tree, and the one this node is not allowed to police — calibrated
//!   K-way scoring — is a different node's artifact. Fitting the accepted
//!   weights makes the accepted function *learned* instead of *duplicated*.
//! - **Fitting toward a hand-declared scalar score.** Legal-looking, and
//!   circular in the way that matters: the target would be another weight
//!   vector, so the fit would be measuring how well it reproduced a number
//!   someone typed. The declared *order* has no weights in it, so reproducing
//!   it is a real measurement.
//!
//! # (d) The outcome-proxy distinction, made structural
//!
//! **Every production sample carries `feedback: None`**, because no user-rating
//! source exists. So the reward that can honestly be learned here is an
//! outcome-derived proxy — success, latency, cost, fallback — and never a user
//! preference. Four things make that impossible to lose:
//!
//! 1. **The learner's target type has no preference field.** [`OutcomeProxy`]
//!    holds `success`, `latency_ms`, `cost` and `fallback_count`. There is no
//!    field a rating could be put in, so no future edit can quietly start
//!    reading one. `OutcomeProxy::from_targets` takes a [`Targets`] and nothing
//!    else.
//! 2. **The fit never sees `Feedback`.** `OutcomeTrainingSample::feedback` is
//!    carried past the projection untouched and read exactly once, to *count*
//!    it in [`RewardFitReport::feedback_signals_present`]. A count is not a
//!    signal. The count is published precisely so that a reader can see the
//!    learner had the opportunity and declined it.
//! 3. **The declared order and the fit target are published constants** whose
//!    names and values both say "outcome proxy", and
//!    [`REWARD_FIT_TARGET_DESCRIPTION`] is reproduced verbatim in the report.
//!    No report field can be read as "users prefer this": there is nothing in
//!    the input that could carry that reading.
//! 4. **The safety metrics never pass through the reward function.** Failure
//!    rate, mean cost, and the tail latency percentile are computed from raw
//!    outcome measurements. A reward weighting therefore cannot argue for its
//!    own approval — the gate is computed in a space the bandit does not
//!    control.
//!
//! # The bandit
//!
//! An **arm** is a named candidate reward policy: a complete [`RewardPolicy`]
//! weight vector plus a name. The bandit selects *over arms* — that is, over
//! candidate reward weightings — by replaying the snapshot and comparing the
//! outcome-proxy score each arm assigns to the same recorded outcomes. Three
//! things are always in the set:
//!
//! - the arm the fit produced ([`FITTED_ARM_NAME`], reserved and appended
//!   automatically);
//! - any number of caller-declared arms, typically the accepted hand-set policy
//!   (see [`RewardArm::accepted_prior`]) and ablations of it.
//!
//! Selection is UCB1 over per-arm statistics: `mean + c * sqrt(2 ln N / n)`. All
//! of `mean`, `n`, the bonus, and the resulting score are reported per arm in
//! [`ArmSelectionStatistics`], so a reader can recompute the choice by hand.
//!
//! ## Why the replay has a seeded assignment schedule
//!
//! An offline replay that scored every arm on every row would give every arm the
//! same pull count, and the UCB bonus — which depends only on `n` and the total
//! `N` — would be identical for all of them. The rule would collapse into
//! `argmax(mean)`, which is not a bandit. So the replay assigns each row to
//! exactly one arm through a **deterministic seeded hash** of the sample id
//! ([`SelectionConfig::seed`]). Arms then have genuinely different pull counts,
//! the bonus genuinely discriminates, and the per-arm `observations` in the
//! report mean something. The same seed and the same snapshot always assign the
//! same row to the same arm; a different seed replays a different schedule.
//!
//! This is an honest offline stand-in for allocation, and it is described as
//! one: **this module allocates no traffic and learns nothing online.** It reads
//! a frozen snapshot, replays it once, and reports.
//!
//! ## The replay is paired where it matters
//!
//! The seeded assignment exists to give the *bandit* differing pull counts. It
//! would be a mistake to let it confound the *safety* comparison, so the two
//! phases use different statistics over the same rows:
//!
//! - the bandit uses [`ArmSelectionStatistics`], from the arm's assigned rows;
//! - the safety gate uses [`ArmSafetyMetrics`], computed for **every** arm over
//!   **every** row of the evaluation partition, so the comparison is paired and
//!   an arm is never graded on rows another arm saw.
//!
//! # The safety gate
//!
//! [`SafetyVerdict`] is the gate, and it is a value with three states:
//! [`SafetyVerdict::Accept`], [`SafetyVerdict::Reject`], and
//! [`SafetyVerdict::InsufficientEvidence`]. Only `Accept` is an acceptance, and
//! it is reached only by clearing, in order:
//!
//! 1. **evidence** — the evaluation partition must hold at least
//!    [`SafetyConfig::min_evaluation_samples`] rows, and must hold at least one
//!    success, one failure, one cost observation, and enough latency
//!    observations to place a percentile. A comparison that cannot be made
//!    reports `InsufficientEvidence`, which is *not* an acceptance.
//! 2. **reward** — the candidate's mean outcome-proxy reward must beat the
//!    reference arm's by strictly more than
//!    [`SafetyConfig::min_reward_improvement`]. Equal is not better: an
//!    evaluation that cannot separate two arms is evidence of nothing. (This
//!    mirrors `WarmupVerdict`'s treatment of a tie.)
//! 2. **no regression** — every one of the following must hold, each with its
//!    own configured tolerance, each producing a named
//!    [`SafetyViolation`] when it does not:
//!    - failure rate (the required gate),
//!    - mean cost (the required gate),
//!    - tail latency at the configured percentile (the required gate),
//!    - fallback rate.
//!
//! A candidate that wins on mean reward and regresses on **any** of those is
//! [`SafetyVerdict::Reject`]. Mean-only evaluation is not available here: there
//! is no code path that reaches a verdict from the mean alone.
//!
//! ## Can it refuse, and how is refusal un-driftable?
//!
//! Yes, and in two distinct layers.
//!
//! **A refusal** is [`BanditError`] — a typed value with a reason, covering
//! insufficient data, a degenerate reward, a non-finite value, a schema
//! mismatch, and a configuration that would make the gate meaningless. Nothing
//! panics, and there is no silently defaulted policy: a refused run returns no
//! fitted weights, no statistics, and no verdict.
//!
//! **Acceptance itself cannot be granted by a stored field.** The report holds
//! the measured deltas and the tolerances, and *recomputes* the violations and
//! the verdict from them on every call:
//!
//! ```text
//! SafetyEvaluation::verdict()      -> recomputed from deltas + tolerances
//! BanditReport::accepted_arm()     -> calls the above, returns None unless Accept
//! ```
//!
//! There is no `is_safe: bool` anywhere in this module. A `SafetyVerdict` field
//! exists on the report for serializing what was decided, but
//! [`BanditReport::accepted_arm`] deliberately does not read it — it recomputes
//! — so corrupting that field cannot produce an acceptance.
//!
//! # The ceiling this fit cannot cross, stated up front
//!
//! The accepted score clamps its latency term at `latency_ms / 1000` and its cost
//! term at `min(cost, 1)`. The basis inherits both clamps, and that bounds what
//! any weight vector can do: **two requests whose latencies both exceed 1000 ms
//! carry an identical latency column, so no weights can order them by latency at
//! all.** The same holds for two costs above 1.0. The declared order does rank
//! them, so such pairs are simply unreachable, and they are counted rather than
//! hidden — they show up as holdout pairs the fit cannot get right, which is why
//! [`RewardFitReport::fitted_holdout_agreement`] is reported instead of a bare
//! "the fit worked".
//!
//! Widening the clamp would mean changing [`RewardComputer`], which is an accepted
//! surface this node consumes and does not edit. Reporting the ceiling is the
//! honest move available here.
//!
//! # Determinism
//!
//! The same snapshot, the same configuration, and the same seed produce the same
//! arm statistics, the same selection, and the same verdict.
//!
//! - Rows are put in a canonical total order — `(timestamp, sample_id)` — after
//!   duplicate sample ids are refused, so the run is a function of the
//!   snapshot's *contents* and not of the order a caller handed over.
//! - Cells are ordered by key, and the fit/holdout split takes trailing cells,
//!   so the partition is a function of the contents too.
//! - The fit is full-batch gradient descent with a fixed iteration count, a
//!   fixed learning rate, a fixed initialization at the prior, and a pair list
//!   in canonical order. **It contains no randomness and needs no seed at all**;
//!   `loss` and `softplus`/`sigmoid` are evaluated in the overflow-safe form so
//!   the iteration count cannot change the result through a numerical blowup.
//! - The seed is used for exactly one thing — the replay assignment schedule and
//!   its tie-breaking — and it is recorded in [`SelectionTrace::seed`].
//! - No report field carries a timestamp, a duration, or a run counter.
//!
//! # Refusal style
//!
//! Refusals follow the accepted 7E-2B style deliberately: a typed error carrying
//! the numbers that caused it, `thiserror` for the message, `Result` at every
//! boundary, and no `unwrap` on a value that came from outside. The dataset
//! projection is the same one warmup uses — canonical
//! [`OutcomeTrainingSample`] in, per-row schema and finiteness gates applied
//! before anything is read, and the legacy projection used only where an
//! accepted consumer demands it.
//!
//! # What this module deliberately does not do
//!
//! - It does not install, activate, schedule, or swap anything, and it registers
//!   no endpoint. It is a pure library entry point.
//! - It does not produce a [`DecisionDistribution`](super::decision_contract::DecisionDistribution).
//!   A calibrated K-way distribution is 7E-2D's artifact, and the bandit here
//!   deliberately reports a selected *name* and per-arm UCB *scores* rather than
//!   a probability vector, so it cannot be mistaken for one or pre-empt it.
//! - It does not keep a durable journal, snapshot, or activation record; those
//!   are 7E-2E and 7E-2F.
//! - It never returns `Action::Explore` and never calls
//!   [`ActionGuard`](super::reward::ActionGuard). `Action::Explore` stays as
//!   unreachable from the live router as it was before this module existed, and
//!   this module adds no path to it.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::dataset::{validate_outcome_sample, OutcomeTrainingSample};
use super::evaluation::RoutingMetrics;
use super::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use super::reward::{RewardComputer, RewardPolicy};

use super::dataset::Targets;
use super::dataset::TrainingSample as DatasetTrainingSample;

// ---------------------------------------------------------------------------
// Published constants — what is being fit, and toward what
// ---------------------------------------------------------------------------

/// Number of columns in a [`RewardBasis`] row: one per `RewardPolicy` field.
pub const BASIS_WIDTH: usize = 6;

/// Column of the success sign (`RewardPolicy::success_weight`).
pub const B_SUCCESS: usize = 0;
/// Column of the clamped latency penalty (`RewardPolicy::latency_weight`).
pub const B_LATENCY: usize = 1;
/// Column of the clamped cost penalty (`RewardPolicy::cost_weight`).
pub const B_COST: usize = 2;
/// Column of the fallback indicator (`RewardPolicy::fallback_penalty`).
pub const B_FALLBACK: usize = 3;
/// Column of the switch count (`RewardPolicy::switch_cost`).
pub const B_SWITCH: usize = 4;
/// Column of the uncertainty penalty (`RewardPolicy::uncertainty_weight`).
pub const B_UNCERTAINTY: usize = 5;

/// Name of each `RewardPolicy` field, indexed by its [`RewardBasis`] column.
pub const WEIGHT_NAMES: [&str; BASIS_WIDTH] = [
    "success_weight",
    "latency_weight",
    "cost_weight",
    "fallback_penalty",
    "switch_cost",
    "uncertainty_weight",
];

/// The arm the reward fit produces. Reserved: a caller may not declare it.
pub const FITTED_ARM_NAME: &str = "fitted-outcome-proxy";

/// The accepted hand-set [`RewardPolicy::default`], named for use as a
/// reference arm and as a comparison point in tests.
pub const ACCEPTED_PRIOR_ARM_NAME: &str = "accepted-hand-set-prior";

/// The default replay seed. Deliberate and recorded, not a bare `0`.
pub const DEFAULT_SEED: u64 = 0x7E2C_BA4D;

/// The outcome-derived order the reward fit is trying to reproduce.
///
/// This is the whole of "what is being fit toward", stated in one place. It is a
/// **total order over outcome measurements production already records**, and it
/// contains no weights, so reproducing it is a measurement rather than an echo.
///
/// 1. a request that succeeded outranks one that did not;
/// 2. then lower measured latency wins;
/// 3. then lower measured cost wins;
/// 4. then fewer fallbacks win.
///
/// A request with no latency or no cost measurement ranks *after* every
/// measured value, because "never measured" is at least as bad as the worst
/// thing that was measured. Nothing here is a user judgement, a rating, or a
/// preference of any kind: every term is a number in
/// [`OutcomeTrainingSample::targets`].
pub const OUTCOME_PROXY_ORDER: &str =
    "safety-first total order over recorded outcomes: success desc, then measured latency asc, \
     then measured cost asc, then fallback count asc; an unmeasured latency or cost ranks after \
     every measured value";

/// The objective the reward fit minimizes, named.
///
/// A ridge-regularized pairwise logistic (ranking) objective over pairs drawn
/// from the same routing cell, evaluated against [`OUTCOME_PROXY_ORDER`]. It is
/// *not* a regression onto a score this module also chose.
pub const OUTCOME_PROXY_FIT_TARGET: &str =
    "ridge-regularized pairwise ranking loss: weights are fit so the linear outcome-proxy score \
     reproduces the published safety-first outcome order on pairs drawn within a routing cell";

/// The honest one-sentence description of what the learned weights mean, and
/// what they do not. Reproduced verbatim in every
/// [`RewardFitReport::target_description`] so no reader can lose it.
pub const REWARD_FIT_TARGET_DESCRIPTION: &str =
    "the fitted weights are the accepted reward policy's six weights, fit offline to reproduce a \
     published safety-first ordering of recorded outcomes (success, latency, cost, fallback); \
     they are an outcome-derived proxy for which recorded outcomes went well and are NOT a \
     user preference, rating, or satisfaction signal, because no such signal exists in the data";

/// The weights that cannot be identified from outcome data, and why.
///
/// `uncertainty_weight` multiplies a *decision-time* confidence. A dataset row
/// records what happened, not what the model was confident about beforehand, so
/// the column is identically zero for every row and the gradient with respect
/// to it is identically zero. The fit therefore cannot move it, the ridge term
/// holds it at the prior, and the report says so rather than presenting it as
/// learned. This is a real limit, published instead of papered over.
pub const UNIDENTIFIABLE_WEIGHT: &str = "uncertainty_weight";

/// Why `uncertainty_weight` is unidentifiable, for the report.
pub const UNIDENTIFIABLE_REASON: &str =
    "multiplies a decision-time confidence that a dataset row does not carry; the column is zero \
     for every row, so the objective has no gradient with respect to it and the ridge term holds \
     it at the prior";

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
struct Row {
    sample_id: String,
    cell: String,
    proxy: OutcomeProxy,
    basis: RewardBasis,
    legacy: DatasetTrainingSample,
}

/// The routing cell a row belongs to.
///
/// A cell is a `(provider, model, dialect, streaming)` tuple: requests
/// production already treated as alike. Pairs are only ever formed *within* a
/// cell, so a pair is a comparison production effectively made anyway, and no
/// comparison can be an artifact of two unrelated routes.
fn cell_key(sample: &OutcomeTrainingSample) -> String {
    format!(
        "{}|{}|{}|{}",
        sample.provider_id, sample.model_id, sample.dialect, sample.streaming
    )
}

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
// Reward fitting
// ---------------------------------------------------------------------------

/// One comparable pair, as a difference row and a label.
#[derive(Debug, Clone, Copy)]
struct Pair {
    difference: [f64; BASIS_WIDTH],
    label: f64,
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
fn agreement(pairs: &[Pair], weights: &[f64; BASIS_WIDTH]) -> Option<f64> {
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
fn softplus(z: f64) -> f64 {
    if z > 0.0 {
        z + (-z).exp().ln_1p()
    } else {
        z.exp().ln_1p()
    }
}

/// Numerically safe logistic function.
fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let exponential = z.exp();
        exponential / (1.0 + exponential)
    }
}

/// The ridge-regularized pairwise ranking objective.
fn objective(
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
fn solve_weights(
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
fn holdout_pairs_only_differ_on_success(pairs: &[Pair]) -> bool {
    !pairs.is_empty() && pairs.iter().all(|pair| pair.difference[B_SUCCESS] != 0.0)
}

/// Build every comparable pair within each cell, in canonical order.
fn build_pairs(rows: &[&Row], max_pairs_per_cell: usize) -> Result<Vec<Pair>, BanditError> {
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
/// None of these pass through the reward function. They are the raw outcome
/// measurements, which is exactly why a reward weighting cannot argue for its
/// own approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmSafetyMetrics {
    /// The arm's name.
    pub arm: String,
    /// Mean outcome-proxy reward over every evaluation row.
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
    fn checked(&self) -> Result<(), BanditError> {
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
fn reject_degenerate_reward(rows: &[Row]) -> Result<(), BanditError> {
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
fn assemble_arms(
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
fn seeded_hash(seed: u64, sample_id: &str) -> u64 {
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

/// Measure one arm over the whole evaluation partition.
///
/// Every metric here is a raw outcome measurement. None of them passes through
/// the reward function, which is what stops a reward weighting from grading its
/// own homework.
fn arm_safety_metrics(
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
    let reward = RewardComputer::new(arm.policy.clone());
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

/// The tail latency percentile the safety gate reads by default.
pub const DEFAULT_TAIL_PERCENTILE: f64 = 0.95;

/// What the evaluation partition contains.
fn evidence_of(rows: &[&Row]) -> SafetyEvidence {
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
fn compare_to_reference(
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
fn insufficient_reason(evidence: &SafetyEvidence, config: &SafetyConfig) -> Option<String> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(success: f64, latency: f64, cost: f64, fallback: f64) -> RewardPolicy {
        RewardPolicy {
            success_weight: success,
            latency_weight: latency,
            cost_weight: cost,
            fallback_penalty: fallback,
            switch_cost: -0.2,
            uncertainty_weight: 0.1,
        }
    }

    fn targets(
        success: bool,
        latency_ms: Option<f64>,
        cost: Option<f64>,
        fallback_count: u32,
    ) -> Targets {
        Targets {
            success,
            latency_ms,
            ttft_ms: success.then_some(10.0),
            cost,
            failure_class: (!success).then(|| "Test".to_string()),
            fallback_count,
        }
    }

    // -- The basis is the accepted score, exactly --

    #[test]
    fn basis_reproduces_accepted_score() {
        let cases = [
            (true, Some(120.0), Some(0.02), 0u32),
            (true, Some(999.0), Some(1.0), 2),
            (true, None, None, 0),
            (false, None, Some(0.5), 1),
            (false, None, None, 3),
            (true, Some(0.0), Some(0.0), 0),
        ];
        let policies = [
            RewardPolicy::default(),
            policy(2.0, 0.7, 0.4, -1.5),
            policy(0.0, 0.0, 0.0, 0.0),
        ];
        for (success, latency, cost, fallback) in cases {
            let label = targets(success, latency, cost, fallback);
            let proxy = OutcomeProxy::from_targets(&label);
            for switch_count in [0u32, 1, 3] {
                for confidence in [0.0f64, 0.5, 1.0] {
                    let basis = RewardBasis::from_targets(&label, switch_count, confidence);
                    for candidate in &policies {
                        assert_eq!(
                            basis.score(candidate),
                            accepted_outcome_proxy_score(
                                &proxy,
                                switch_count,
                                confidence,
                                candidate
                            ),
                            "basis must equal the accepted score for \
                             success={success} latency={latency:?} cost={cost:?} \
                             fallback={fallback} switches={switch_count} \
                             confidence={confidence} policy={candidate:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn weights_vector_round_trips_through_a_policy() {
        let original = policy(1.5, 0.25, 0.75, -0.5);
        // `RewardPolicy` is not `PartialEq`, so the round trip is checked field by
        // field rather than by adding a derive to an accepted type.
        let restored = policy_from_weights(weights_vector(&original));
        assert!((restored.success_weight - original.success_weight).abs() < f64::EPSILON);
        assert!((restored.latency_weight - original.latency_weight).abs() < f64::EPSILON);
        assert!((restored.cost_weight - original.cost_weight).abs() < f64::EPSILON);
        assert!((restored.fallback_penalty - original.fallback_penalty).abs() < f64::EPSILON);
        assert!((restored.switch_cost - original.switch_cost).abs() < f64::EPSILON);
        assert!((restored.uncertainty_weight - original.uncertainty_weight).abs() < f64::EPSILON);
    }

    // -- The declared order --

    #[test]
    fn outcome_order_puts_success_first() {
        let success = OutcomeProxy {
            success: true,
            latency_ms: Some(900.0),
            cost: Some(1.0),
            fallback_count: 3,
        };
        let failure = OutcomeProxy {
            success: false,
            latency_ms: None,
            cost: Some(0.0),
            fallback_count: 0,
        };
        assert_eq!(compare_outcome_proxy(&success, &failure), Ordering::Less);
        assert_eq!(compare_outcome_proxy(&failure, &success), Ordering::Greater);
    }

    #[test]
    fn outcome_order_ranks_unmeasured_last_and_ties_cleanly() {
        let measured = OutcomeProxy {
            success: true,
            latency_ms: Some(900.0),
            cost: Some(0.5),
            fallback_count: 1,
        };
        let unmeasured = OutcomeProxy {
            success: true,
            latency_ms: None,
            cost: None,
            fallback_count: 1,
        };
        assert_eq!(
            compare_outcome_proxy(&measured, &unmeasured),
            Ordering::Less,
            "an unmeasured latency must rank after every measured value"
        );
        assert_eq!(
            compare_outcome_proxy(&measured, &measured),
            Ordering::Equal,
            "identical outcomes must tie, not be invented apart"
        );
    }

    // -- Numeric stability --

    #[test]
    fn softplus_and_sigmoid_survive_extreme_inputs() {
        for z in [-1e12, -700.0, -1.0, 0.0, 1.0, 700.0, 1e12] {
            let value = softplus(z);
            assert!(
                value.is_finite(),
                "softplus({z}) must be finite, got {value}"
            );
            let probability = sigmoid(z);
            assert!(
                probability.is_finite() && (0.0..=1.0).contains(&probability),
                "sigmoid({z}) must be finite and in [0,1], got {probability}"
            );
        }
        // `softplus(0) = ln 2`, not 0: the function is the logistic loss, and the
        // only property under test is that it stays finite at both extremes.
        assert!((softplus(0.0) - std::f64::consts::LN_2).abs() < 1e-12);
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-12);
    }

    // -- Percentile matches the accepted evaluator --

    #[test]
    fn percentile_matches_accepted_evaluator() {
        let latencies: Vec<f64> = (0..40).map(|index| 10.0 + f64::from(index) * 7.5).collect();
        let mut sorted = latencies.clone();
        sorted.sort_by(f64::total_cmp);
        let computed = percentile(&sorted, DEFAULT_TAIL_PERCENTILE);

        let samples: Vec<DatasetTrainingSample> = latencies
            .iter()
            .enumerate()
            .map(|(index, latency)| DatasetTrainingSample {
                sample_id: format!("s{index}"),
                schema_version: FEATURE_SCHEMA_VERSION,
                timestamp: index as i64,
                features: super::super::features::RoutingFeatures::default(),
                targets: targets(true, Some(*latency), Some(0.01), 0),
                provider_id: "p".to_string(),
                model_id: "m".to_string(),
                origin: crate::feedback::DataOrigin::Native,
                outcome_id: format!("o{index}"),
                feedback: Vec::new(),
            })
            .collect();
        let accepted = RoutingMetrics::from_samples(&samples);
        assert!(
            (computed - accepted.p95_latency_ms).abs() < 1e-9,
            "restated percentile ({computed}) must match the accepted evaluator ({})",
            accepted.p95_latency_ms
        );
    }

    #[test]
    fn percentile_of_empty_and_singleton() {
        assert!(percentile(&[], 0.95).abs() < f64::EPSILON);
        assert!((percentile(&[42.0], 0.95) - 42.0).abs() < f64::EPSILON);
    }

    // -- Degeneracy and configuration --

    #[test]
    fn degenerate_reward_is_refused() {
        let rows: Vec<Row> = (0..4)
            .map(|index| {
                let label = targets(true, Some(100.0), Some(0.01), 0);
                Row {
                    sample_id: format!("s{index}"),
                    cell: "c".to_string(),
                    proxy: OutcomeProxy::from_targets(&label),
                    basis: RewardBasis::from_targets(&label, 0, 1.0),
                    legacy: DatasetTrainingSample {
                        sample_id: format!("s{index}"),
                        schema_version: FEATURE_SCHEMA_VERSION,
                        timestamp: 0,
                        features: super::super::features::RoutingFeatures::default(),
                        targets: label,
                        provider_id: "p".to_string(),
                        model_id: "m".to_string(),
                        origin: crate::feedback::DataOrigin::Native,
                        outcome_id: format!("o{index}"),
                        feedback: Vec::new(),
                    },
                }
            })
            .collect();
        assert!(matches!(
            reject_degenerate_reward(&rows),
            Err(BanditError::DegenerateReward { rows: 4 })
        ));
    }

    #[test]
    fn meaningless_configurations_are_refused() {
        let base = BanditConfig::default();
        assert!(base.checked().is_ok());

        let mut config = base.clone();
        config.reward.holdout_cells = 0;
        assert!(matches!(
            config.checked(),
            Err(BanditError::EmptyHoldoutCells)
        ));

        let mut config = base.clone();
        config.reward.iterations = 0;
        assert!(matches!(
            config.checked(),
            Err(BanditError::EmptyFitIterations)
        ));

        let mut config = base.clone();
        config.reward.learning_rate = f64::INFINITY;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessLearningRate(_))
        ));

        let mut config = base.clone();
        config.reward.ridge_lambda = -1.0;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessRidgeLambda(_))
        ));

        let mut config = base.clone();
        config.safety.min_reward_improvement = 0.0;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessSafetyFloor(_))
        ));

        let mut config = base.clone();
        config.safety.min_reward_improvement = f64::NAN;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessSafetyFloor(_))
        ));

        let mut config = base.clone();
        config.safety.min_evaluation_samples = 1;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessEvaluationFloor(1))
        ));

        // An infinite tolerance would make one of the three required gates
        // incapable of failing, which is the exact failure mode this node exists
        // to prevent.
        let mut config = base.clone();
        config.safety.max_failure_rate_regression = f64::INFINITY;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessTolerance {
                name: "max_failure_rate_regression",
                ..
            })
        ));

        let mut config = base.clone();
        config.safety.max_cost_regression = f64::NAN;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessTolerance { .. })
        ));

        let mut config = base.clone();
        config.safety.max_tail_latency_regression_ms = -1.0;
        assert!(matches!(
            config.checked(),
            Err(BanditError::NegativeTolerance { .. })
        ));

        let mut config = base.clone();
        config.selection.exploration_c = f64::NEG_INFINITY;
        assert!(matches!(
            config.checked(),
            Err(BanditError::MeaninglessExploration(_))
        ));

        let mut config = base.clone();
        config.selection.min_arm_observations = 0;
        assert!(matches!(
            config.checked(),
            Err(BanditError::EmptyArmObservationFloor)
        ));
    }

    // -- The seeded assignment is reproducible and seed-dependent --

    #[test]
    fn seeded_assignment_is_deterministic_and_seed_sensitive() {
        let ids = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let first: Vec<usize> = ids
            .iter()
            .map(|id| (seeded_hash(7, id) % 3) as usize)
            .collect();
        let second: Vec<usize> = ids
            .iter()
            .map(|id| (seeded_hash(7, id) % 3) as usize)
            .collect();
        assert_eq!(first, second, "the same seed must assign identically");
        let other: Vec<usize> = ids
            .iter()
            .map(|id| (seeded_hash(8, id) % 3) as usize)
            .collect();
        assert_ne!(
            first, other,
            "a different seed must explore a different schedule"
        );
    }

    #[test]
    fn a_success_only_holdout_cannot_carry_an_improvement_claim() {
        // Pairs that all differ on the success column: `success_weight` alone
        // explains every one of them, so a better score is a fact about the base
        // rate and not about the learned weights.
        let pairs = vec![
            Pair {
                difference: [2.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                label: 1.0,
            },
            Pair {
                difference: [2.0, 0.1, 0.0, 0.0, 0.0, 0.0],
                label: 1.0,
            },
        ];
        assert!(holdout_pairs_only_differ_on_success(&pairs));
        assert_eq!(
            RewardFitVerdict::Better.downgraded_to_not_better(),
            RewardFitVerdict::NotBetter
        );
        assert_eq!(
            RewardFitVerdict::Worse.downgraded_to_not_better(),
            RewardFitVerdict::Worse,
            "a fit that is confidently wrong is still worse"
        );

        // One pair that shares a success label is enough to make the holdout able
        // to discriminate, and the claim is no longer suppressed.
        let mixed = vec![
            pairs[0],
            Pair {
                difference: [0.0, 0.2, 0.0, 0.0, 0.0, 0.0],
                label: 1.0,
            },
        ];
        assert!(!holdout_pairs_only_differ_on_success(&mixed));
        assert!(!holdout_pairs_only_differ_on_success(&[]));
    }

    #[test]
    fn the_fit_can_report_a_better_reward() {
        // The guard above suppresses a claim on a degenerate holdout only. A fit
        // that genuinely reproduces the declared order better must be able to say
        // so, or the verdict would be decoration.
        assert!(RewardFitVerdict::Better.is_improvement());
        assert!(!RewardFitVerdict::NotBetter.is_improvement());
        assert!(!RewardFitVerdict::Worse.is_improvement());
    }

    // -- The safety verdict is recomputed, never a stored flag --

    #[test]
    fn violations_are_recomputed_from_deltas_and_tolerances() {
        let evaluation = SafetyEvaluation {
            reference: "r".to_string(),
            candidate: "c".to_string(),
            evaluation_samples: 100,
            reference_metrics: metrics_stub("r"),
            candidate_metrics: metrics_stub("c"),
            mean_reward_delta: 0.5,
            failure_rate_delta: 0.02,
            mean_cost_delta: -0.001,
            tail_latency_delta_ms: -5.0,
            fallback_rate_delta: 0.0,
            tolerances: SafetyTolerances {
                min_reward_improvement: 1e-9,
                max_failure_rate_regression: 0.0,
                max_cost_regression: 0.0,
                max_tail_latency_regression_ms: 0.0,
                max_fallback_rate_regression: 0.0,
            },
            insufficient_reason: None,
        };

        // Mean reward improved and cost and tail improved, but the failure rate
        // regressed: the whole point of the gate.
        let violations = evaluation.violations();
        assert_eq!(violations.len(), 1, "only the failure rate regressed");
        assert!(matches!(
            violations[0],
            SafetyViolation::FailureRateRegressed { .. }
        ));
        assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);
        assert!(!evaluation.verdict().is_acceptance());
    }

    #[test]
    fn equal_reward_is_not_an_improvement() {
        let mut evaluation = SafetyEvaluation {
            reference: "r".to_string(),
            candidate: "c".to_string(),
            evaluation_samples: 100,
            reference_metrics: metrics_stub("r"),
            candidate_metrics: metrics_stub("c"),
            mean_reward_delta: 0.0,
            failure_rate_delta: 0.0,
            mean_cost_delta: 0.0,
            tail_latency_delta_ms: 0.0,
            fallback_rate_delta: 0.0,
            tolerances: SafetyTolerances {
                min_reward_improvement: 1e-9,
                max_failure_rate_regression: 0.0,
                max_cost_regression: 0.0,
                max_tail_latency_regression_ms: 0.0,
                max_fallback_rate_regression: 0.0,
            },
            insufficient_reason: None,
        };
        assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);
        assert!(matches!(
            evaluation.violations()[0],
            SafetyViolation::NoRewardImprovement { .. }
        ));

        evaluation.tolerances.min_reward_improvement = 0.1;
        assert!(
            !evaluation.violations().is_empty(),
            "raising the floor is stricter, not looser: a zero improvement still fails it"
        );
        assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);

        evaluation.mean_reward_delta = 0.5;
        assert!(
            evaluation.violations().is_empty(),
            "an improvement past the floor clears it"
        );
        assert_eq!(evaluation.verdict(), SafetyVerdict::Accept);
    }

    #[test]
    fn insufficient_evidence_outranks_a_clean_comparison() {
        let evaluation = SafetyEvaluation {
            reference: "r".to_string(),
            candidate: "c".to_string(),
            evaluation_samples: 100,
            reference_metrics: metrics_stub("r"),
            candidate_metrics: metrics_stub("c"),
            mean_reward_delta: 10.0,
            failure_rate_delta: -0.5,
            mean_cost_delta: -0.5,
            tail_latency_delta_ms: -500.0,
            fallback_rate_delta: -0.5,
            tolerances: SafetyTolerances {
                min_reward_improvement: 1e-9,
                max_failure_rate_regression: 0.0,
                max_cost_regression: 0.0,
                max_tail_latency_regression_ms: 0.0,
                max_fallback_rate_regression: 0.0,
            },
            insufficient_reason: Some("the evaluation partition holds no failure".to_string()),
        };
        assert_eq!(evaluation.verdict(), SafetyVerdict::InsufficientEvidence);
        assert!(!evaluation.verdict().is_acceptance());
        assert!(
            evaluation.violations().is_empty(),
            "no violation was measured, and the verdict is still not an acceptance"
        );
    }

    #[test]
    fn insufficient_reason_names_every_gap() {
        let config = SafetyConfig::default();
        let mut evidence = SafetyEvidence {
            total: 100,
            successes: 50,
            failures: 50,
            cost_observations: 50,
            latency_observations: 50,
            fallback_observations: 0,
            feedback_signals_present: 0,
        };
        assert_eq!(insufficient_reason(&evidence, &config), None);

        evidence.failures = 0;
        assert!(insufficient_reason(&evidence, &config)
            .unwrap()
            .contains("no failure"));

        evidence.failures = 50;
        evidence.successes = 0;
        assert!(insufficient_reason(&evidence, &config)
            .unwrap()
            .contains("no success"));

        evidence.successes = 50;
        evidence.cost_observations = 0;
        assert!(insufficient_reason(&evidence, &config)
            .unwrap()
            .contains("no cost measurement"));

        evidence.cost_observations = 50;
        evidence.latency_observations = 1;
        assert!(insufficient_reason(&evidence, &config)
            .unwrap()
            .contains("tail latency"));

        evidence.latency_observations = 50;
        evidence.total = 1;
        assert!(insufficient_reason(&evidence, &config)
            .unwrap()
            .contains("safety floor"));
    }

    #[test]
    fn arm_names_are_validated() {
        let fitted = RewardPolicy::default();
        let safety = SafetyConfig::default();
        assert!(matches!(
            assemble_arms(&[], fitted.clone(), &safety),
            Err(BanditError::NoArms)
        ));
        assert!(matches!(
            assemble_arms(
                &[RewardArm::new("  ", fitted.clone())],
                fitted.clone(),
                &safety
            ),
            Err(BanditError::EmptyArmName { index: 0 })
        ));
        assert!(matches!(
            assemble_arms(
                &[RewardArm::new(FITTED_ARM_NAME, fitted.clone())],
                fitted.clone(),
                &safety
            ),
            Err(BanditError::ReservedArmName { .. })
        ));
        assert!(matches!(
            assemble_arms(
                &[
                    RewardArm::new("dup", fitted.clone()),
                    RewardArm::new("dup", fitted.clone())
                ],
                fitted.clone(),
                &safety
            ),
            Err(BanditError::DuplicateArmName { .. })
        ));
        assert!(matches!(
            assemble_arms(
                &[RewardArm::new("only", fitted.clone())],
                fitted.clone(),
                &safety
            ),
            Err(BanditError::ReferenceArmUnknown { .. })
        ));

        let arms = assemble_arms(&[RewardArm::accepted_prior()], fitted, &safety)
            .expect("a prior arm plus the fitted arm is a valid set");
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].name, ACCEPTED_PRIOR_ARM_NAME);
        assert_eq!(arms[1].name, FITTED_ARM_NAME);
    }

    fn metrics_stub(arm: &str) -> ArmSafetyMetrics {
        ArmSafetyMetrics {
            arm: arm.to_string(),
            mean_outcome_proxy_reward: 0.0,
            failure_rate: 0.0,
            mean_cost: 0.0,
            tail_latency_ms: 0.0,
            tail_percentile: DEFAULT_TAIL_PERCENTILE,
            fallback_rate: 0.0,
            accepted_metrics: RoutingMetrics::default(),
        }
    }
}
