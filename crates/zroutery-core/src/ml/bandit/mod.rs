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

mod config;
mod reward;
mod safety;
mod selection;

pub use config::*;
pub use reward::*;
pub use safety::*;
pub use selection::*;

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

/// The tail latency percentile the safety gate reads by default.
pub const DEFAULT_TAIL_PERCENTILE: f64 = 0.95;

#[cfg(test)]
mod tests;
