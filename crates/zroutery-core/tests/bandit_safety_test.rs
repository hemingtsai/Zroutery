#![cfg(feature = "ml")]

//! Node 7E-2C gate tests: offline bandit selection and the safety gate.
//!
//! The claims under test are the ones this node owns —
//!
//! 1. **learned reward**: the six `RewardPolicy` weights are fitted from data,
//!    the fitting is deterministic, the fit is out-of-sample checked, and the
//!    report says in words that it is an outcome proxy and not a preference;
//! 2. **bandit selection**: a real UCB1 rule over declared arms, with per-arm
//!    observation counts and an uncertainty estimate, a recorded seed, and a
//!    refusal when there is not enough data to select at all;
//! 3. **safety evaluation that can refuse**: mean reward up while the failure
//!    rate, the cost, or the tail latency regresses is *unsafe*; and acceptance
//!    cannot be granted by writing a field into the report;
//! 4. **replayable and deterministic**: same data, config, and seed ⇒ same arm
//!    statistics, same selection, same verdict — including across a reordered
//!    input snapshot;
//! 5. **production inaccessibility**: nothing here is reachable from the running
//!    product, and `Action::Explore` stays unreachable;
//! 6. **fail-closed refusals**: every malformed input is a typed refusal with a
//!    reason, never a panic and never a silently defaulted policy.

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::bandit::{
    accepted_outcome_proxy_score, compare_outcome_proxy, run_bandit, ArmSafetyMetrics,
    ArmSelectionStatistics, BanditConfig, BanditError, BanditOutcome, BanditReport, OutcomeProxy,
    RewardArm, RewardBasis, RewardFitConfig, RewardFitReport, RewardFitVerdict, SafetyConfig,
    SafetyEvaluation, SafetyEvidence, SafetyTolerances, SafetyVerdict, SafetyViolation,
    SelectionConfig, SelectionTrace, ACCEPTED_PRIOR_ARM_NAME, FITTED_ARM_NAME, OUTCOME_PROXY_ORDER,
    REWARD_FIT_TARGET_DESCRIPTION, UNIDENTIFIABLE_WEIGHT,
};
use zroutery_core::ml::dataset::{
    outcome_to_dataset_sample, OutcomeTrainingSample, TrainingSample as DatasetTrainingSample,
};
use zroutery_core::ml::evaluation::RoutingMetrics;
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION, F_CONTEXT_TOKENS};
use zroutery_core::ml::reward::{Action, ActionGuard, RewardPolicy};
use zroutery_core::outcome::{Attempt, Outcome};
use zroutery_core::session::SessionRoutingMode;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The terminal state a fixture outcome is built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Success,
    Failed,
}

/// One routing cell: a `(model, provider)` pair. Cells are the unit the fit/holdout
/// split takes and the only scope pairs are formed in.
const CELLS: [(&str, &str); 4] = [
    ("model-a", "provider-a"),
    ("model-b", "provider-b"),
    ("model-c", "provider-a"),
    ("model-d", "provider-b"),
];

/// A real Outcome built through the accepted builder, so every fixture row travels
/// the same collection path production does.
fn fixture_outcome(
    model: &str,
    provider: &str,
    terminal: Terminal,
    latency_ms: f64,
    cost: f64,
    fallbacks: u32,
    timestamp: i64,
) -> Outcome {
    let success = terminal == Terminal::Success;
    let failure_class = (!success).then_some(FailureClass::RateLimit);
    let attempt = Attempt {
        attempt_id: format!("att_{timestamp}"),
        candidate_model: model.to_string(),
        candidate_provider: provider.to_string(),
        started_at: timestamp,
        completed_at: timestamp + 1,
        latency_ms,
        ttft_ms: success.then_some(50.0),
        success,
        failure_class,
        failure_message: failure_class.map(|class| format!("fixture {class:?}")),
        http_status: match terminal {
            Terminal::Success => Some(200),
            Terminal::Failed => Some(429),
        },
        rectified: false,
    };
    let builder = Outcome::builder(format!("req_{timestamp}"))
        .single_candidate(model, provider)
        .dialect("openai")
        .streaming(false)
        .attempt(attempt)
        .total_latency_ms(latency_ms)
        .cost(Some(cost), Some(cost))
        .timestamp(timestamp);
    // A fallback is expressed the way production records it.
    let mut builder = builder.fallback_count(fallbacks);
    if success {
        builder = builder.ttft_ms(50.0);
    }
    builder.build()
}

fn sample(
    model: &str,
    provider: &str,
    terminal: Terminal,
    latency_ms: f64,
    cost: f64,
    fallbacks: u32,
    timestamp: i64,
) -> OutcomeTrainingSample {
    let outcome = fixture_outcome(
        model, provider, terminal, latency_ms, cost, fallbacks, timestamp,
    );
    let mut features = RoutingFeatures::default();
    features.values[F_CONTEXT_TOKENS] = 0.5;
    outcome_to_dataset_sample(&outcome, features, DataOrigin::Native)
        .expect("a fixture outcome must canonicalize")
}

/// Build the canonical snapshot.
///
/// The accepted dataset layer derives `sample_id` from the outcome's `outcome_id`,
/// and `OutcomeBuilder` mints a fresh UUID for that on every `build`. A snapshot
/// is therefore a *value*: rebuilding it yields a different dataset with different
/// sample ids, which is exactly why the determinism claim in this node is stated
/// over a given snapshot. This function is called once per process and the result
/// shared, so every test compares runs over the same dataset.
fn build_snapshot() -> Vec<OutcomeTrainingSample> {
    let mut rows = Vec::new();
    let mut timestamp = 1_700_000_000i64;
    for (model, provider) in CELLS {
        for row in cell_rows() {
            timestamp += 1;
            rows.push(sample(
                model, provider, row.0, row.1, row.2, row.3, timestamp,
            ));
        }
    }
    rows
}

/// The shared snapshot. See [`build_snapshot`] for why it is built once.
fn snapshot() -> &'static [OutcomeTrainingSample] {
    static SNAPSHOT: std::sync::OnceLock<Vec<OutcomeTrainingSample>> = std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(build_snapshot)
}

/// A clone of the shared snapshot, for tests that reorder or filter it.
fn misordered_snapshot() -> Vec<OutcomeTrainingSample> {
    snapshot().to_vec()
}

/// The rows of one cell, as `(terminal, latency_ms, cost, fallbacks)`.
///
/// The generating relationship: among successful requests that took no fallback,
/// **lower latency goes with higher cost**, tuned so the accepted prior's two terms
/// cancel. `compute_attempt` prices latency as `latency_ms / 1000` and cost as
/// `min(cost, 1)`, so the prior's score for such a row is
///
/// ```text
/// 1.0 - 0.3 * latency / 1000 - 0.1 * cost
/// ```
///
/// and setting `cost = 1.3 - 0.003 * latency` makes that `0.87` for *every* one of
/// them, to within floating-point noise. The prior therefore cannot rank these
/// rows: it gives them all the same score, and a tie does not reproduce a strict
/// order, so a tie counts as incorrect. The declared order ranks them by latency,
/// and a fit that keeps the cost term near zero while raising the latency term
/// separates all of them.
///
/// This is a deliberately adversarial fixture, built so that "the fit learned
/// nothing" cannot pass for "the fit worked". It is worth being precise about what
/// it demonstrates, though: it shows the fit can *separate* rows the prior leaves
/// tied. It does not show the prior is grossly wrong, and with `latency_weight =
/// 0.3` against `cost_weight = 0.1` over an unclamped cost range the prior is in
/// fact close to monotone in latency. The test therefore asserts the *discipline* —
/// the claim is the sign of the measured out-of-sample delta — rather than
/// inventing a fixture that manufactures a dramatic margin the accepted weights do
/// not permit.
fn cell_rows() -> Vec<(Terminal, f64, f64, u32)> {
    let mut rows: Vec<(Terminal, f64, f64, u32)> = Vec::new();
    // 100 ms to 430 ms, which is where the tie relationship keeps cost positive.
    let mut latency = 100.0;
    while latency <= 430.0 {
        let cost = 1.3 - 0.003 * latency;
        rows.push((Terminal::Success, latency, cost, 0));
        latency += 10.0;
    }
    // A slow success that took a fallback: both the prior and the declared order
    // rank it last among successes, so it is a pair everyone gets right.
    rows.push((Terminal::Success, 700.0, 0.05, 1));
    rows.push((Terminal::Success, 850.0, 0.02, 2));
    // Failures, which the declared order ranks last outright.
    rows.push((Terminal::Failed, 0.0, 0.30, 0));
    rows.push((Terminal::Failed, 0.0, 0.60, 1));
    rows
}

/// A configuration sized for the four-cell fixture: the last cell is held out, so
/// the fit sees three and the bandit and the gate see one.
fn fixture_config() -> BanditConfig {
    BanditConfig {
        reward: RewardFitConfig {
            holdout_cells: 1,
            min_pair_count: 8,
            ..RewardFitConfig::default()
        },
        selection: SelectionConfig {
            seed: 4_242,
            min_total_observations: 8,
            min_arm_observations: 1,
            ..SelectionConfig::default()
        },
        safety: SafetyConfig {
            reference_arm: ACCEPTED_PRIOR_ARM_NAME.to_string(),
            min_evaluation_samples: 4,
            ..SafetyConfig::default()
        },
    }
}

fn declared_arms() -> Vec<RewardArm> {
    vec![
        RewardArm::accepted_prior(),
        // A success-only ablation: ignores latency, cost, and fallbacks entirely.
        RewardArm::new(
            "success-only",
            RewardPolicy {
                success_weight: 1.0,
                latency_weight: 0.0,
                cost_weight: 0.0,
                fallback_penalty: 0.0,
                switch_cost: 0.0,
                uncertainty_weight: 0.0,
            },
        ),
    ]
}

fn report_json(snapshot: &[OutcomeTrainingSample], config: &BanditConfig) -> String {
    let outcome =
        run_bandit(snapshot, &declared_arms(), config).expect("the fixture snapshot must run");
    serde_json::to_string(outcome.report()).expect("a report must serialize")
}

// ---------------------------------------------------------------------------
// The schedule witness
// ---------------------------------------------------------------------------

/// One arm's seed-dependent state, at the resolution of the replay itself.
///
/// The five `f64` fields are compared as raw bit patterns rather than as
/// numbers. That is deliberate and it cuts both ways: two runs that replayed the
/// same schedule accumulate the same sums in the same canonical order and so
/// produce equal bits, while any difference whatsoever in the accumulated sums
/// shows up. So the comparison needs no tolerance, and it cannot hide a
/// difference behind one.
#[derive(Debug, Clone, PartialEq)]
struct ArmWitness {
    arm: String,
    observations: usize,
    mean_bits: u64,
    variance_bits: u64,
    standard_error_bits: u64,
    bonus_bits: u64,
    ucb_bits: u64,
}

/// Everything one seeded replay decided, and nothing else.
///
/// This is the witness for "a different seed explores a different assignment
/// schedule". It is deliberately **not** the per-arm observation count. The
/// counts are three integers that sum to the replay size, so the space they range
/// over is tiny, and two seeds land on the same one often enough to break a
/// matrix: with 38 rows over 3 arms the probability that two independent seeds
/// agree on the whole count vector is 0.0108, about one run in ninety-three.
/// The statistics below are functions of *which* rows each arm received, not of
/// how many, so the witness ranges over the schedule itself rather than over its
/// size.
///
/// The recorded seed is **not** part of the witness, and that is the whole
/// difficulty in choosing one. `SelectionTrace::seed` echoes back the seed the
/// caller asked for, so two runs made with different seeds always differ there;
/// a witness that included it would pass for any implementation at all, including
/// one whose schedule ignored the seed entirely. What is left here is only what
/// the replay *did*, and a replay that stopped depending on the seed would make
/// every field of it equal.
#[derive(Debug, Clone, PartialEq)]
struct ScheduleWitness {
    arms: Vec<ArmWitness>,
    selected: String,
    tied_arms: Vec<String>,
}

impl ScheduleWitness {
    /// One legible line, for an assertion message.
    fn summary(&self) -> String {
        let arms = self
            .arms
            .iter()
            .map(|arm| {
                format!(
                    "{}: n={} mean={} ucb={}",
                    arm.arm,
                    arm.observations,
                    f64::from_bits(arm.mean_bits),
                    f64::from_bits(arm.ucb_bits)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "selected {} tied [{}] arms {{{}}}",
            self.selected,
            self.tied_arms.join(", "),
            arms
        )
    }
}

/// The witness for one run's replay.
///
/// Taken from `run_bandit`'s own outputs, so it observes the production schedule
/// rather than restating it. The caller keeps the seed, because the seed is the
/// thing being varied and not a thing the witness is made of.
fn schedule_witness(arms: &[ArmSelectionStatistics], trace: &SelectionTrace) -> ScheduleWitness {
    ScheduleWitness {
        arms: arms
            .iter()
            .map(|arm| ArmWitness {
                arm: arm.arm.clone(),
                observations: arm.observations,
                mean_bits: arm.mean_outcome_proxy_reward.to_bits(),
                variance_bits: arm.reward_variance.to_bits(),
                standard_error_bits: arm.reward_standard_error.to_bits(),
                bonus_bits: arm.exploration_bonus.to_bits(),
                ucb_bits: arm.ucb_score.to_bits(),
            })
            .collect(),
        selected: trace.selected.clone(),
        tied_arms: trace.tied_arms.clone(),
    }
}

/// The witness for one seed's replay of the given snapshot.
fn witness_for_seed(rows: &[OutcomeTrainingSample], seed: u64) -> ScheduleWitness {
    let mut config = fixture_config();
    config.selection.seed = seed;
    let outcome =
        run_bandit(rows, &declared_arms(), &config).expect("the fixture snapshot must run");
    schedule_witness(outcome.arms(), &outcome.report().selection)
}

/// The seeds the schedule property is checked over.
///
/// Both ends of the seed range and the fixture's own default are in the set, so
/// the claim is not an artefact of two convenient interior values, and the count
/// vector assertion is not the only thing standing between two seeds being
/// compared at all.
const SCHEDULE_SEEDS: [u64; 5] = [0, 1, 4_242, 99_991, u64::MAX];

/// The smallest gap the witness argument requires between two replayed rows'
/// scores under one arm.
///
/// `replay` sums at most 38 scores, each of magnitude at most a few units, so a
/// sum accumulates at most 38 rounding steps of at most 2^-53 each: two sums
/// differing by more than about 1.6e-13 cannot produce the same `f64`.
/// Requiring 1e-9 leaves four orders of magnitude of headroom over that bound, so
/// the premise below is not a statement about exact arithmetic.
const MIN_REPLAY_SCORE_GAP: f64 = 1e-9;

/// The fitted arm's policy, as the report published it.
///
/// Read back off the run rather than refitted, so the scores below are scored
/// under exactly the policy the replay used.
fn fitted_policy_of(outcome: &BanditOutcome) -> &RewardPolicy {
    outcome
        .arms()
        .iter()
        .find(|arm| arm.arm == FITTED_ARM_NAME)
        .map(|arm| &arm.policy)
        .expect("the fit's own arm is always reported")
}

/// The closest pair of scores in a set, or `f64::INFINITY` for fewer than two.
fn smallest_gap(scores: &[f64]) -> f64 {
    let mut sorted = scores.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .fold(f64::INFINITY, f64::min)
}

/// The scores the replay assigns to the rows of the evaluation partition, under
/// one arm's policy.
///
/// This is the production computation reached through the public surface rather
/// than restated: `run_bandit` builds
/// `RewardBasis::from_targets(&targets, proxy.switch_count(), 1.0)` for every row
/// and scores it with `basis.score(policy)`. The evaluation partition is the
/// trailing cell of the split, and every cell in this fixture is one pass over
/// [`cell_rows`], so one pass over `cell_rows` *is* the partition's content — the
/// sample ids differ per cell but the schedule hashes those, while these scores
/// do not read an id at all.
fn replay_row_scores(policy: &RewardPolicy) -> Vec<f64> {
    cell_rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let timestamp = 1_700_000_000i64 + index as i64 + 1;
            let built = sample(
                "model-d",
                "provider-b",
                row.0,
                row.1,
                row.2,
                row.3,
                timestamp,
            );
            let proxy = OutcomeProxy::from_targets(&built.targets);
            RewardBasis::from_targets(&built.targets, proxy.switch_count(), 1.0).score(policy)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Gate 1 — the learned reward
// ---------------------------------------------------------------------------

#[test]
fn reward_weights_are_fitted_not_hand_set() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let report = outcome.report();

    let prior = report.reward_fit.prior_weights.clone();
    let fitted = report.reward_fit.fitted_weights.clone();

    assert_ne!(
        prior, fitted,
        "the fitted weights must differ from the hand-set prior, otherwise nothing was fit"
    );
    assert_eq!(fitted.len(), 6, "a fitted weight per RewardPolicy field");
    assert_eq!(prior.len(), 6);

    // The fitted weights are an accepted RewardPolicy, not a parallel notion.
    assert!((outcome.fitted_policy().success_weight - fitted[0]).abs() < f64::EPSILON);
    assert!((outcome.fitted_policy().cost_weight - fitted[2]).abs() < f64::EPSILON);

    // The behavioural claim, which is the one that matters: the fixture is built
    // so the accepted prior cannot separate its success rows at all (every one
    // scores exactly 0.87) while the declared order ranks them by latency. The fit
    // must reproduce that order and the prior must not.
    let slower = OutcomeProxy {
        success: true,
        latency_ms: Some(400.0),
        cost: Some(0.10),
        fallback_count: 0,
    };
    let faster_but_costlier = OutcomeProxy {
        success: true,
        latency_ms: Some(100.0),
        cost: Some(1.00),
        fallback_count: 0,
    };
    assert_eq!(
        compare_outcome_proxy(&faster_but_costlier, &slower),
        std::cmp::Ordering::Less,
        "the declared order ranks the faster request first"
    );

    let prior = &report.reward_fit.prior;
    let prior_delta = score_delta(&faster_but_costlier, &slower, prior);
    assert!(
        prior_delta <= 0.0,
        "the accepted prior cannot separate this pair, so it scores {prior_delta}"
    );

    let fitted_delta = score_delta(&faster_but_costlier, &slower, &report.reward_fit.fitted);
    assert!(
        fitted_delta > 0.0,
        "the fit must rank the faster request above the slower one, scored {fitted_delta}"
    );
}

/// The accepted reward's score difference between two outcomes under a policy.
///
/// Positive means `left` scores above `right`, which is what the declared order
/// requires when it says `left` should be ranked first.
fn score_delta(left: &OutcomeProxy, right: &OutcomeProxy, policy: &RewardPolicy) -> f64 {
    let score = |proxy: &OutcomeProxy| {
        accepted_outcome_proxy_score(proxy, proxy.switch_count(), 1.0, policy)
    };
    score(left) - score(right)
}

#[test]
fn fitted_reward_is_checked_out_of_sample_and_reported_honestly() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let fit = &outcome.report().reward_fit;

    assert!(fit.pairs_fit > 0, "the fit partition must yield pairs");
    assert!(
        fit.pairs_holdout > 0,
        "the out-of-sample check needs holdout pairs"
    );
    assert_eq!(
        fit.fit_cells + fit.holdout_cells,
        fit.cells_total,
        "the split must account for every cell"
    );
    assert_eq!(fit.feedback_signals_present, 0);

    // The in-sample objective always improves; that is not evidence of anything,
    // and the report publishes it separately from the out-of-sample number.
    assert!(
        fit.fit_objective_fitted < fit.fit_objective_at_prior,
        "a fitted objective should improve in sample"
    );
    assert!(
        fit.fitted_holdout_agreement >= fit.prior_holdout_agreement,
        "the fit must not lose out-of-sample agreement"
    );
    assert_eq!(
        fit.verdict,
        RewardFitVerdict::Better,
        "the fixture is built so the fit beats the prior out of sample: prior agreement {} vs \
         fitted agreement {}, prior weights {:?} vs fitted weights {:?}",
        fit.prior_holdout_agreement,
        fit.fitted_holdout_agreement,
        fit.prior_weights,
        fit.fitted_weights
    );
    assert!(fit.is_improvement());
    assert!(fit.agreement_delta > 0.0);
}

#[test]
fn the_improvement_claim_tracks_the_measured_delta_and_nothing_else() {
    // The discipline the out-of-sample check exists to enforce: the improvement
    // claim is exactly the sign of the measured agreement delta, and the verdict is
    // derived from it. Sweep a range of step sizes so the claim is exercised on
    // both sides of the decision, including configurations where the fit barely
    // moves and where it is pushed well past the prior.
    for (iterations, learning_rate, ridge_lambda) in [
        (1usize, 1e-12, 0.01f64),
        (400, 0.05, 0.01),
        (400, 0.5, 0.0),
        (40, 40.0, 0.0),
        (2_000, 0.01, 0.5),
    ] {
        let mut config = fixture_config();
        config.reward.iterations = iterations;
        config.reward.learning_rate = learning_rate;
        config.reward.ridge_lambda = ridge_lambda;

        let outcome = run_bandit(snapshot(), &declared_arms(), &config)
            .expect("every step size runs and reports honestly");
        let fit = &outcome.report().reward_fit;

        assert_eq!(
            fit.agreement_delta,
            fit.fitted_holdout_agreement - fit.prior_holdout_agreement,
            "the delta must be the difference it claims to be \
             (iterations={iterations} lr={learning_rate})"
        );
        assert_eq!(
            fit.is_improvement(),
            fit.agreement_delta > 0.0,
            "an improvement claim is exactly a positive out-of-sample delta \
             (iterations={iterations} lr={learning_rate} got {})",
            fit.agreement_delta
        );
        assert_eq!(
            fit.verdict,
            if fit.agreement_delta > 0.0 {
                RewardFitVerdict::Better
            } else if fit.agreement_delta < 0.0 {
                RewardFitVerdict::Worse
            } else {
                RewardFitVerdict::NotBetter
            },
            "the verdict must follow the measured delta, not a separate opinion \
             (iterations={iterations} lr={learning_rate})"
        );
        // And the bandit still selected an arm, so the fit's verdict is a
        // statement about the reward and not about whether the run happened.
        assert!(!outcome.report().selection.selected.is_empty());
    }
}

#[test]
fn a_fit_is_scored_on_the_declared_order_and_never_on_its_own_objective() {
    // The in-sample objective is not evidence. A configuration that drives it hard
    // is reported with its out-of-sample agreement alongside, and the improvement
    // claim follows that and nothing else.
    let mut config = fixture_config();
    config.reward.iterations = 2_000;
    config.reward.learning_rate = 0.01;
    config.reward.ridge_lambda = 0.5;

    let outcome = run_bandit(snapshot(), &declared_arms(), &config).expect("run");
    let fit = &outcome.report().reward_fit;
    assert!(
        fit.fit_objective_fitted < fit.fit_objective_at_prior,
        "the in-sample objective improved"
    );
    // Whatever the in-sample number says, the claim is the out-of-sample one, and
    // the two are reported as separate fields so they cannot be confused.
    assert_eq!(
        fit.is_improvement(),
        fit.agreement_delta > 0.0,
        "the claim is the out-of-sample delta, not the objective"
    );
    assert!(
        fit.pairs_fit > 0 && fit.pairs_holdout > 0,
        "both the fit partition and its out-of-sample check must have real pairs"
    );
    assert_ne!(
        fit.pairs_fit, fit.pairs_holdout,
        "the in-sample and out-of-sample pair counts are reported separately"
    );
}

// ---------------------------------------------------------------------------
// Gate 1 / (d) — the outcome proxy, never a preference
// ---------------------------------------------------------------------------

#[test]
fn the_reward_target_is_declared_as_an_outcome_proxy_not_a_preference() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let report = outcome.report();

    assert_eq!(
        report.reward_fit.outcome_proxy_not_preference,
        REWARD_FIT_TARGET_DESCRIPTION
    );
    assert!(
        REWARD_FIT_TARGET_DESCRIPTION.contains("NOT a user preference"),
        "the published description must deny a preference reading in its own words"
    );
    assert_eq!(report.outcome_proxy_order, OUTCOME_PROXY_ORDER);

    // No field of the learner's input could carry a rating: the proxy type is
    // success, latency, cost, and fallbacks only, and it is built from the
    // outcome targets with nothing else in scope.
    let row = &misordered_snapshot()[0];
    let proxy = zroutery_core::ml::bandit::OutcomeProxy::from_targets(&row.targets);
    assert!(
        proxy.success || !proxy.success,
        "success is a plain outcome label"
    );
    assert_eq!(proxy.switch_count(), proxy.fallback_count);
    assert_eq!(proxy.is_fallback(), proxy.fallback_count > 0);

    // The proxy is exactly the published vocabulary, and nothing else.
    let serialized = serde_json::to_value(proxy).expect("a proxy serializes");
    let mut keys: Vec<String> = serialized
        .as_object()
        .expect("a proxy is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["cost", "fallback_count", "latency_ms", "success"],
        "the learner's input vocabulary is four outcome measurements and no rating field"
    );
}

#[test]
fn the_unidentifiable_weight_is_reported_rather_than_claimed() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let fit = &outcome.report().reward_fit;

    assert!(
        fit.unidentifiable_weights
            .contains(&UNIDENTIFIABLE_WEIGHT.to_string()),
        "uncertainty_weight multiplies a decision-time confidence a dataset row does not \
         carry, and the report must say so"
    );
    assert!(!fit.unidentifiable_reason.is_empty());

    // And it really is unmoved: the gradient with respect to it is identically
    // zero, so the ridge term holds it exactly at the prior.
    assert_eq!(
        fit.fitted.uncertainty_weight, fit.prior.uncertainty_weight,
        "an unidentifiable weight must sit at the prior, not drift"
    );
    assert_eq!(fit.fitted_weights[5], fit.prior_weights[5]);
}

#[test]
fn feedback_is_counted_and_never_read() {
    let rows = misordered_snapshot();
    // Every production sample carries `feedback: None`, so the count is zero and
    // the fit is driven entirely by outcome measurements.
    assert!(rows.iter().all(|row| row.feedback.is_none()));
    let outcome = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    assert_eq!(outcome.report().reward_fit.feedback_signals_present, 0);
    assert_eq!(outcome.report().evidence.feedback_signals_present, 0);
    assert!(
        outcome.report().reward_fit.distinct_outcome_shapes > 1,
        "the outcome variety must come from measured outcomes"
    );
}

// ---------------------------------------------------------------------------
// Gate 2 — bandit selection
// ---------------------------------------------------------------------------

#[test]
fn the_bandit_reports_per_arm_counts_and_uncertainty() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let arms = outcome.arms();

    assert_eq!(arms.len(), 3, "two declared arms plus the fitted one");
    let names: Vec<&str> = arms.iter().map(|arm| arm.arm.as_str()).collect();
    assert!(names.contains(&FITTED_ARM_NAME));
    assert!(names.contains(&ACCEPTED_PRIOR_ARM_NAME));
    assert!(names.contains(&"success-only"));

    for arm in arms {
        assert!(arm.observations > 0, "{} was never pulled", arm.arm);
        assert!(
            arm.reward_variance >= 0.0 && arm.reward_variance.is_finite(),
            "{} variance must be a real number",
            arm.arm
        );
        assert!(
            arm.reward_standard_error.is_finite(),
            "{} must publish an uncertainty estimate",
            arm.arm
        );
        assert!(
            (arm.ucb_score - (arm.mean_outcome_proxy_reward + arm.exploration_bonus)).abs() < 1e-12,
            "{} UCB must be mean + bonus, recomputable by hand",
            arm.arm
        );
        assert!(
            arm.exploration_bonus > 0.0,
            "{} must carry a positive exploration term at c = 0.5",
            arm.arm
        );
    }

    // The pull counts must genuinely differ, or the UCB bonus is a constant and
    // the "bandit" would be a plain argmax in disguise.
    let counts: Vec<usize> = arms.iter().map(|arm| arm.observations).collect();
    assert_eq!(counts.len(), 3);
    assert!(
        counts.iter().any(|count| *count != counts[0]),
        "a seeded assignment must give arms different pull counts, got {counts:?}"
    );
}

#[test]
fn selection_picks_the_highest_ucb_score() {
    let config = fixture_config();
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &config)
        .expect("the fixture snapshot must run");
    let report = outcome.report();
    let selected = report
        .arms
        .iter()
        .find(|arm| arm.arm == report.selection.selected)
        .expect("the selected arm must be one of the reported arms");

    let best = report
        .arms
        .iter()
        .map(|arm| arm.ucb_score)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (selected.ucb_score - best).abs() < f64::EPSILON,
        "selection must be argmax over the reported UCB scores"
    );
    assert_eq!(
        report.selection.seed, config.selection.seed,
        "the seed is recorded"
    );
    assert!(report
        .selection
        .tied_arms
        .contains(&report.selection.selected));
}

#[test]
fn a_different_seed_replays_a_different_schedule() {
    let rows = misordered_snapshot();
    let mut first = fixture_config();
    first.selection.seed = 1;
    let mut second = fixture_config();
    second.selection.seed = 99_991;

    let a = run_bandit(&rows, &declared_arms(), &first).expect("run");
    let b = run_bandit(&rows, &declared_arms(), &second).expect("run");

    // The witness is the whole replay outcome, not the per-arm observation count.
    // The counts are three integers summing to the replay size, so two seeds
    // collide on them about one run in ninety-three; the statistics are functions
    // of *which* rows each arm received, so they collide only if the two replays
    // assigned the same rows to the same arms. See [`ScheduleWitness`].
    let left = schedule_witness(a.arms(), &a.report().selection);
    let right = schedule_witness(b.arms(), &b.report().selection);
    assert_ne!(
        left,
        right,
        "a different seed must explore a different assignment schedule\n  seed {}: {}\n  \
         seed {}: {}",
        first.selection.seed,
        left.summary(),
        second.selection.seed,
        right.summary()
    );

    // Two premises make that witness worth having, and both are checked rather
    // than asserted in prose. They live in the two tests named here rather than
    // being repeated here: that the arm the argument leans on scores the replayed
    // rows at distinguishable values, so two different row sets cannot share a sum
    // (`the_replayed_rows_are_distinguishable_under_the_fitted_arm`), and that a
    // single row moving between two arms leaves every count untouched while both
    // means move (`the_schedule_witness_sees_a_row_move_the_counts_cannot`).

    // But the fit is seed-free, so the learned reward must not move.
    assert_eq!(
        a.fitted_policy().cost_weight,
        b.fitted_policy().cost_weight,
        "the fit is full-batch and deterministic, so it must not depend on the seed"
    );
    assert_eq!(
        a.fitted_policy().latency_weight,
        b.fitted_policy().latency_weight
    );
}

#[test]
fn distinct_seeds_replay_pairwise_distinct_schedules() {
    let rows = misordered_snapshot();
    let witnesses: Vec<ScheduleWitness> = SCHEDULE_SEEDS
        .iter()
        .map(|seed| witness_for_seed(&rows, *seed))
        .collect();

    // Every pair, not one convenient pair. If the schedule were seed-independent
    // this fails on the first comparison, whatever the seeds are; if two seeds
    // genuinely replayed the same schedule it names the two that did.
    for (index, left) in witnesses.iter().enumerate() {
        for (offset, right) in witnesses.iter().skip(index + 1).enumerate() {
            assert_ne!(
                left,
                right,
                "two distinct seeds replayed the same assignment schedule\n  seed {}: \
                 {}\n  seed {}: {}",
                SCHEDULE_SEEDS[index],
                left.summary(),
                SCHEDULE_SEEDS[index + 1 + offset],
                right.summary()
            );
        }
    }
}

#[test]
fn the_replayed_rows_are_distinguishable_under_the_fitted_arm() {
    let rows = misordered_snapshot();
    let outcome = run_bandit(&rows, &declared_arms(), &fixture_config()).expect("run");
    let scores = replay_row_scores(fitted_policy_of(&outcome));

    // `replay_row_scores` stands in for the evaluation partition, so the fixture
    // claim it rests on is checked here: four cells, each one pass over
    // `cell_rows`, and the replay holding out exactly one of them.
    assert_eq!(
        outcome.report().rows,
        CELLS.len() * cell_rows().len(),
        "every cell must be one pass over cell_rows for the scores below to be the \
         replayed rows' scores"
    );
    assert_eq!(
        scores.len(),
        outcome.report().selection.replay_rows,
        "one score per replayed row"
    );
    assert!(
        scores.iter().all(|score| score.is_finite()),
        "every replayed row must score finitely, got {scores:?}"
    );
    let gap = smallest_gap(&scores);
    assert!(
        gap > MIN_REPLAY_SCORE_GAP,
        "the fitted arm must score the replayed rows at values far enough apart that two \
         different row sets cannot share a sum; the closest pair is {gap:e} apart and \
         {MIN_REPLAY_SCORE_GAP:e} is required, got {scores:?}"
    );
}

#[test]
fn the_schedule_witness_sees_a_row_move_the_counts_cannot() {
    // Why the witness is not the count vector, as arithmetic rather than as
    // assertion. Move exactly one replayed row from one arm to another: every
    // per-arm observation count is unchanged, so the old witness is blind to it,
    // while the donor's and the receiver's means both move.
    let rows = misordered_snapshot();
    let outcome = run_bandit(&rows, &declared_arms(), &fixture_config()).expect("run");
    let scores = replay_row_scores(fitted_policy_of(&outcome));
    let moved = scores.len() / 2;
    assert!(
        moved > 0 && moved < scores.len(),
        "the fixture splits in two"
    );

    let before = scores[..moved].to_vec();
    let mut after = before.clone();
    after[moved - 1] = scores[moved];

    // The old witness: two integers per arm, and they are equal.
    let counts_before: Vec<usize> = vec![before.len(), scores.len() - before.len()];
    let counts_after: Vec<usize> = vec![after.len(), scores.len() - after.len()];
    assert_eq!(
        counts_before, counts_after,
        "moving one row between arms leaves the count vector identical"
    );

    // The new witness: the same two arms report different means.
    let mean_before = before.iter().sum::<f64>() / before.len() as f64;
    let mean_after = after.iter().sum::<f64>() / after.len() as f64;
    let gap = smallest_gap(&scores) / moved as f64;
    assert!(
        (mean_before - mean_after).abs() >= gap,
        "one row moved between two arms with equal counts must move their means, by at \
         least the smallest score gap divided by the pull count; got {mean_before} and \
         {mean_after}"
    );
}

#[test]
fn the_bandit_refuses_when_there_is_not_enough_data_to_select() {
    let rows = misordered_snapshot();
    let mut config = fixture_config();
    // The holdout cell holds 8 rows and there are 3 arms, so a floor above that
    // is arithmetically unmeetable.
    config.selection.min_total_observations = 10_000;
    assert!(matches!(
        run_bandit(&rows, &declared_arms(), &config),
        Err(BanditError::TooFewTotalObservations { min: 10_000, .. })
    ));

    let mut config = fixture_config();
    config.selection.min_arm_observations = 100;
    match run_bandit(&rows, &declared_arms(), &config) {
        Err(BanditError::TooFewTotalObservations { .. }) => {}
        Err(BanditError::ArmUnderObserved { name, min, .. }) => {
            assert_eq!(min, 100);
            assert!(!name.is_empty());
        }
        other => panic!("expected an observation-floor refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Gate 3 — the safety gate refuses
// ---------------------------------------------------------------------------

#[test]
fn mean_reward_up_with_a_regressed_failure_rate_is_unsafe() {
    // The reference arm is the accepted hand-set policy; the candidate is an arm
    // that scores the *mean* higher while failing more often.
    let reference = ArmProfile::new("reference", 0.50, 0.10, 0.02, 400.0, 0.05);
    let candidate = ArmProfile::new("candidate", 0.55, 0.14, 0.01, 350.0, 0.05);
    let evaluation = evaluate(&reference, &candidate, default_tolerances());

    assert!(
        evaluation.mean_reward_delta > 0.0,
        "the candidate did improve the mean outcome proxy"
    );
    assert!(
        evaluation.failure_rate_delta > 0.0,
        "and it regressed the failure rate"
    );
    assert_eq!(
        evaluation.verdict(),
        SafetyVerdict::Reject,
        "improving the mean while failing more is not an improvement"
    );
    assert!(!evaluation.verdict().is_acceptance());
    assert!(evaluation
        .violations()
        .iter()
        .any(|violation| matches!(violation, SafetyViolation::FailureRateRegressed { .. })));
}

#[test]
fn mean_reward_up_with_a_regressed_cost_is_unsafe() {
    let reference = ArmProfile::new("reference", 0.50, 0.10, 0.02, 400.0, 0.05);
    let candidate = ArmProfile::new("candidate", 0.55, 0.10, 0.05, 400.0, 0.05);
    let evaluation = evaluate(&reference, &candidate, default_tolerances());

    assert!(evaluation.mean_reward_delta > 0.0);
    assert!(evaluation.mean_cost_delta > 0.0);
    assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);
    assert!(evaluation
        .violations()
        .iter()
        .any(|violation| matches!(violation, SafetyViolation::CostRegressed { .. })));
}

#[test]
fn mean_reward_up_with_a_regressed_tail_latency_is_unsafe() {
    let reference = ArmProfile::new("reference", 0.50, 0.10, 0.02, 400.0, 0.05);
    let candidate = ArmProfile::new("candidate", 0.55, 0.10, 0.02, 900.0, 0.05);
    let evaluation = evaluate(&reference, &candidate, default_tolerances());

    assert!(evaluation.mean_reward_delta > 0.0);
    assert!(evaluation.tail_latency_delta_ms > 0.0);
    assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);
    assert!(evaluation
        .violations()
        .iter()
        .any(|violation| matches!(violation, SafetyViolation::TailLatencyRegressed { .. })));
}

#[test]
fn a_clean_improvement_is_accepted() {
    let reference = ArmProfile::new("reference", 0.50, 0.12, 0.03, 800.0, 0.10);
    let candidate = ArmProfile::new("candidate", 0.62, 0.08, 0.02, 500.0, 0.05);
    let evaluation = evaluate(&reference, &candidate, default_tolerances());

    assert!(evaluation.violations().is_empty(), "nothing regressed");
    assert_eq!(evaluation.verdict(), SafetyVerdict::Accept);
    assert!(evaluation.verdict().is_acceptance());
}

#[test]
fn a_configured_tolerance_moves_the_verdict_in_the_documented_direction() {
    let reference = ArmProfile::new("reference", 0.50, 0.10, 0.02, 400.0, 0.05);
    let candidate = ArmProfile::new("candidate", 0.55, 0.12, 0.02, 400.0, 0.05);

    let strict = evaluate(&reference, &candidate, default_tolerances());
    assert_eq!(strict.verdict(), SafetyVerdict::Reject);

    // A configured tolerance is the only thing that can turn a regression into an
    // acceptance, and it is in the report, so the reader can see it was widened.
    let mut tolerant = default_tolerances();
    tolerant.max_failure_rate_regression = 0.05;
    let relaxed = evaluate(&reference, &candidate, tolerant);
    assert!(relaxed.violations().is_empty());
    assert_eq!(relaxed.verdict(), SafetyVerdict::Accept);
    assert_eq!(relaxed.tolerances.max_failure_rate_regression, 0.05);
}

#[test]
fn the_safety_metrics_never_pass_through_the_reward_function() {
    // The gate reads raw outcome measurements. This pins that a policy which
    // scores a much better *mean outcome proxy reward* still cannot hide a
    // failure-rate regression, because the two numbers are computed separately.
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let report = outcome.report();
    let evidence = report.evidence;

    let successes = evidence.successes as f64;
    let failures = evidence.failures as f64;
    let measured_failure_rate = failures / (successes + failures);
    for arm in &report.safety_by_arm {
        assert!(
            (arm.failure_rate - measured_failure_rate).abs() < 1e-12,
            "every arm sees the same raw failure rate {}: a reward weighting cannot \
             move a measured outcome",
            measured_failure_rate
        );
        assert!(
            arm.tail_percentile == 0.95,
            "the tail percentile is published so the number is not read as a mean"
        );
        // And the accepted evaluator agrees about the same rows.
        assert!(
            (arm.accepted_metrics.success_rate - successes / (successes + failures)).abs() < 1e-12
        );
    }
}

#[test]
fn rescaling_every_weight_cannot_manufacture_a_safety_acceptance() {
    // Every arm is measured over the same recorded rows, so a comparison that
    // scored each arm with that arm's own weights would see four zero deltas
    // and one number - the arm's own scoring unit - that a pure rescale could
    // move. Multiplying every weight by 100 preserves the ranking of any
    // outcome and multiplies the arm's score by 100, which is exactly the
    // change that used to read as a mean-reward improvement and win an
    // acceptance.
    let rows = misordered_snapshot();
    let prior = RewardPolicy::default();
    let scaled = RewardPolicy {
        success_weight: prior.success_weight * 100.0,
        latency_weight: prior.latency_weight * 100.0,
        cost_weight: prior.cost_weight * 100.0,
        fallback_penalty: prior.fallback_penalty * 100.0,
        switch_cost: prior.switch_cost * 100.0,
        uncertainty_weight: prior.uncertainty_weight * 100.0,
    };

    // Control: with no arm rescaled, the run is already rejected, because two
    // arms scored over the same recorded rows have no identifiable improvement.
    // The rescaled run below must not turn that rejection into an acceptance.
    let control = run_bandit(
        &rows,
        &[
            RewardArm::accepted_prior(),
            RewardArm::new("identical-ranking", prior.clone()),
        ],
        &fixture_config(),
    )
    .expect("the fixture snapshot must run");
    assert_eq!(
        control.report().safety.mean_reward_delta,
        0.0,
        "arms over the same rows have no reward delta"
    );
    assert_eq!(
        control.report().safety.verdict(),
        SafetyVerdict::Reject,
        "a comparison with no identifiable improvement cannot be accepted"
    );
    assert_eq!(control.accepted_arm(), None);

    // The re-scaled arm still wins the UCB replay, because the replay scores
    // each arm in that arm's own units. Safety must not follow it.
    let outcome = run_bandit(
        &rows,
        &[
            RewardArm::accepted_prior(),
            RewardArm::new("scaled-identical-ranking", scaled),
        ],
        &fixture_config(),
    )
    .expect("the fixture snapshot must run");
    let report = outcome.report();
    assert_eq!(
        report.selection.selected, "scaled-identical-ranking",
        "the replay is expected to select the re-scaled arm on its own scale"
    );

    // The four raw metrics are the same rows for every arm, and the reward
    // dimension is now scored on the one fixed evaluation scale, so it carries
    // no information either.
    let rewards: Vec<f64> = report
        .safety_by_arm
        .iter()
        .map(|arm| arm.mean_outcome_proxy_reward)
        .collect();
    assert!(
        rewards.windows(2).all(|pair| pair[0] == pair[1]),
        "every arm must be scored on one shared scale, got {rewards:?}"
    );
    assert_eq!(
        report.safety.mean_reward_delta, 0.0,
        "a rescale of the candidate's weights must not move the compared reward"
    );
    assert_eq!(
        report.safety.verdict(),
        SafetyVerdict::Reject,
        "multiplying every weight by 100 must not turn a rejection into an acceptance"
    );
    assert_eq!(
        outcome.accepted_arm(),
        None,
        "a pure weight rescale grants no acceptance"
    );
}

#[test]
fn acceptance_cannot_be_granted_by_writing_a_field_into_the_report() {
    let reference = ArmProfile::new("reference", 0.50, 0.10, 0.02, 400.0, 0.05);
    let candidate = ArmProfile::new("candidate", 0.55, 0.30, 0.02, 400.0, 0.05);
    let mut report = report_for(
        evaluate(&reference, &candidate, default_tolerances()),
        "candidate",
    );

    assert_eq!(report.safety.verdict(), SafetyVerdict::Reject);
    assert_eq!(report.accepted_arm(), None);

    // Forge the stored verdict. Acceptance reads a recomputation, so this changes
    // the serialized field and nothing else.
    report.safety_verdict = SafetyVerdict::Accept;
    assert_eq!(report.safety.verdict(), SafetyVerdict::Reject);
    assert_eq!(
        report.accepted_arm(),
        None,
        "a forged verdict grants nothing"
    );
    assert!(!report.is_acceptable());

    // Forge the deltas instead, and the gate does accept — which is exactly why
    // the deltas are the evidence and the verdict is not the control. There is
    // no field whose value alone decides acceptance.
    let mut forged = report;
    forged.safety.failure_rate_delta = -0.5;
    assert_eq!(forged.safety.verdict(), SafetyVerdict::Accept);
    assert_eq!(forged.accepted_arm(), Some("candidate"));
}

#[test]
fn a_safety_config_that_cannot_fail_is_refused() {
    for (mutate, name) in [
        (
            Box::new(|config: &mut SafetyConfig| {
                config.max_failure_rate_regression = f64::INFINITY;
            }) as Box<dyn Fn(&mut SafetyConfig)>,
            "max_failure_rate_regression",
        ),
        (
            Box::new(|config: &mut SafetyConfig| {
                config.max_cost_regression = f64::INFINITY;
            }),
            "max_cost_regression",
        ),
        (
            Box::new(|config: &mut SafetyConfig| {
                config.max_tail_latency_regression_ms = f64::INFINITY;
            }),
            "max_tail_latency_regression_ms",
        ),
        (
            Box::new(|config: &mut SafetyConfig| {
                config.max_fallback_rate_regression = f64::NEG_INFINITY;
            }),
            "max_fallback_rate_regression",
        ),
    ] {
        let mut config = fixture_config();
        mutate(&mut config.safety);
        let error = run_bandit(&misordered_snapshot(), &declared_arms(), &config)
            .expect_err("a gate that cannot fail must be refused, not run");
        assert!(
            matches!(error, BanditError::MeaninglessTolerance { .. }),
            "{name} = non-finite must be a typed refusal, got {error}"
        );
    }

    // A zero improvement floor would make "no worse" an acceptance.
    let mut config = fixture_config();
    config.safety.min_reward_improvement = 0.0;
    assert!(matches!(
        run_bandit(&misordered_snapshot(), &declared_arms(), &config),
        Err(BanditError::MeaninglessSafetyFloor(_))
    ));

    // A one-row evaluation set cannot support a comparison at all.
    let mut config = fixture_config();
    config.safety.min_evaluation_samples = 1;
    assert!(matches!(
        run_bandit(&misordered_snapshot(), &declared_arms(), &config),
        Err(BanditError::MeaninglessEvaluationFloor(1))
    ));
}

#[test]
fn an_undecidable_comparison_is_reported_as_insufficient_not_accepted() {
    // The fit cells hold both classes, and the holdout cell — the one the gate
    // compares on — holds successes only, so a failure rate cannot be compared.
    // The gate must say it cannot decide, and "cannot decide" is not an acceptance.
    let mut rows = Vec::new();
    let mut timestamp = 1_800_000_000i64;
    for (cell_index, (model, provider)) in CELLS.iter().enumerate() {
        let holdout_cell = cell_index == CELLS.len() - 1;
        for row in cell_rows() {
            timestamp += 1;
            let terminal = if holdout_cell && row.0 == Terminal::Failed {
                Terminal::Success
            } else {
                row.0
            };
            rows.push(sample(
                model, provider, terminal, row.1, row.2, row.3, timestamp,
            ));
        }
    }

    let outcome = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect("a single-class evaluation partition still runs");
    let safety = &outcome.report().safety;
    // The gate's own view of the holdout is what matters, and it has no failures.
    assert_eq!(safety.candidate_metrics.failure_rate, 0.0);
    assert_eq!(safety.reference_metrics.failure_rate, 0.0);
    // The fit still saw both classes, so this is a property of the *evaluation*
    // partition and not of the whole snapshot.
    assert!(outcome.report().reward_fit.pairs_fit > 0);

    let verdict = safety.verdict();
    assert_eq!(verdict, SafetyVerdict::InsufficientEvidence);
    assert!(!verdict.is_acceptance());
    assert_eq!(outcome.accepted_arm(), None);
    assert!(!outcome.is_acceptable());
    let reason = safety
        .insufficient_reason
        .as_deref()
        .expect("an undecidable verdict must say why");
    assert!(reason.contains("no failure"), "got {reason}");
}

// ---------------------------------------------------------------------------
// Gate 4 — replayable and deterministic
// ---------------------------------------------------------------------------

#[test]
fn the_same_data_config_and_seed_reproduce_byte_identical_reports() {
    let config = fixture_config();
    let first = report_json(&misordered_snapshot(), &config);
    for round in 0..3 {
        assert_eq!(
            report_json(&misordered_snapshot(), &config),
            first,
            "round {round} must reproduce the same arm statistics, selection, and verdict"
        );
    }
}

#[test]
fn a_reordered_snapshot_reproduces_the_same_report() {
    let config = fixture_config();
    let ordered = misordered_snapshot();
    let first = report_json(&ordered, &config);

    // Reverse, then rotate. The canonical `(timestamp, sample_id)` order must make
    // the run a function of the contents, not of the caller's ordering.
    let mut reversed = ordered.clone();
    reversed.reverse();
    assert_eq!(
        report_json(&reversed, &config),
        first,
        "reversed input changed the run"
    );

    let mut rotated = ordered.clone();
    rotated.rotate_left(5);
    assert_eq!(
        report_json(&rotated, &config),
        first,
        "rotated input changed the run"
    );
}

#[test]
fn the_report_carries_no_wall_clock_or_run_counter() {
    let config = fixture_config();
    let first = report_json(&misordered_snapshot(), &config);
    let second = report_json(&misordered_snapshot(), &config);
    let value: serde_json::Value = serde_json::from_str(&first).expect("valid json");

    for forbidden in [
        "created_at",
        "updated_at",
        "duration",
        "elapsed",
        "run_id",
        "timestamp",
        "started_at",
        "finished_at",
    ] {
        assert!(
            value.get(forbidden).is_none(),
            "the report must not carry a {forbidden} field"
        );
    }
    assert_eq!(
        first, second,
        "no field may vary between two identical runs"
    );
}

#[test]
fn the_fit_needs_no_seed_at_all() {
    // The fit is full-batch with a fixed iteration count and a fixed prior. Two
    // runs whose *selection* seeds differ must produce identical fitted weights.
    let mut a = fixture_config();
    a.selection.seed = 1;
    let mut b = fixture_config();
    b.selection.seed = 7;

    let first = run_bandit(&misordered_snapshot(), &declared_arms(), &a).expect("run");
    let second = run_bandit(&misordered_snapshot(), &declared_arms(), &b).expect("run");
    assert_eq!(first.fitted_weights(), second.fitted_weights());
    assert_eq!(
        first.report().reward_fit.fitted_holdout_agreement,
        second.report().reward_fit.fitted_holdout_agreement
    );
}

// ---------------------------------------------------------------------------
// Gate 5 — production inaccessibility
// ---------------------------------------------------------------------------

/// The source of the module, with every comment line removed, so a tripwire
/// matches code and cannot be satisfied or defeated by prose.
fn code_only(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_module_reaches_no_installation_or_scheduling_surface() {
    // The run is a pure library call: same inputs, same outputs, no clock, no
    // filesystem, no network, no global state.
    let config = fixture_config();
    let rows = misordered_snapshot();
    let first = run_bandit(&rows, &declared_arms(), &config).expect("run");
    let second = run_bandit(&rows, &declared_arms(), &config).expect("run");
    assert_eq!(
        serde_json::to_string(first.report()).expect("serializable"),
        serde_json::to_string(second.report()).expect("serializable")
    );

    // The module's code names no live predictor, no route surface, no scheduler,
    // and no network. The tripwire reads code only, so a doc comment that merely
    // *mentions* one of these cannot mask a real use or fake a clean bill.
    let code = code_only(include_str!("../src/ml/bandit.rs"));
    for forbidden in [
        "ShadowEngine",
        "ShadowStore",
        "ShadowDecision",
        "ModelEnsemblePredictor",
        "ModelStore",
        "try_train",
        "run_warmup",
        "std::fs",
        "std::net",
        "std::thread",
        "tokio",
        "axum",
        "reqwest",
        "SystemTime",
        "Instant",
        "rand::",
    ] {
        assert!(
            !code.contains(forbidden),
            "the bandit code must not reference {forbidden}"
        );
    }
}

#[test]
fn action_explore_stays_unreachable_from_the_live_router() {
    // `Action::Explore` is accepted behaviour in `ml::reward`, and it is the one
    // action that could route a real request somewhere new. This node must not
    // add a path to it, and it must not return one.
    let code = code_only(include_str!("../src/ml/bandit.rs"));
    assert!(
        !code.contains("Action::Explore"),
        "the bandit must not produce or name an exploration action"
    );
    assert!(
        !code.contains("ActionGuard"),
        "the bandit must not call the live action guard"
    );
    assert!(
        !code.contains("use super::reward::{Action"),
        "the bandit must not even import the action type"
    );

    // The guard itself is unchanged: this node added nothing to it, and the arm
    // set is a set of reward weightings, not a set of routing actions.
    assert_eq!(
        ActionGuard::decide("a", "b", SessionRoutingMode::Free, 0.5),
        Action::Explore,
        "the accepted guard behaviour is untouched"
    );
    assert_eq!(
        ActionGuard::decide("a", "b", SessionRoutingMode::Pinned, 0.99),
        Action::Keep,
        "a pinned session still keeps"
    );

    // No arm is a routing action: the type carries a reward policy and a name,
    // and its serialized form has no action variant in it.
    let arm = &declared_arms()[0];
    let keys: Vec<String> = serde_json::to_value(arm)
        .expect("an arm serializes")
        .as_object()
        .expect("an arm is an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["name", "policy"]);
}

#[test]
fn the_fitted_policy_is_produced_but_not_installed() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    // The artifact is reachable for review, which is this node's whole job. There
    // is no method that installs it, swaps it, or writes it anywhere: the outcome
    // exposes the policy and the report, and that is all.
    let _policy: RewardPolicy = outcome.fitted_policy().clone();
    let _report = outcome.report();
    // The fitted arm is an arm, exactly like the declared ones, and it is
    // selected over like any other.
    assert!(outcome.arms().iter().any(|arm| arm.arm == FITTED_ARM_NAME));
}

// ---------------------------------------------------------------------------
// Gate 6 — fail-closed refusals
// ---------------------------------------------------------------------------

#[test]
fn an_empty_snapshot_is_refused() {
    assert!(matches!(
        run_bandit(&[], &declared_arms(), &fixture_config()),
        Err(BanditError::EmptySnapshot)
    ));
}

#[test]
fn a_degenerate_reward_is_refused() {
    // Every row identical: there is no outcome proxy to fit and no order to
    // reproduce. This must be a typed refusal, not a run that reports a fit.
    let rows: Vec<OutcomeTrainingSample> = (0..32)
        .map(|index| {
            sample(
                "model-a",
                "provider-a",
                Terminal::Success,
                250.0,
                0.02,
                0,
                1_700_000_000 + index,
            )
        })
        .collect();
    assert!(matches!(
        run_bandit(&rows, &declared_arms(), &fixture_config()),
        Err(BanditError::DegenerateReward { rows: 32 })
    ));
}

#[test]
fn a_schema_mismatch_is_refused() {
    let mut rows = misordered_snapshot();
    rows[3].schema_version = FEATURE_SCHEMA_VERSION + 1;
    let error = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect_err("a mismatched schema must refuse");
    assert!(
        matches!(
            error,
            BanditError::SchemaMismatch {
                index: 3,
                component: "sample",
                ..
            }
        ),
        "got {error}"
    );

    let mut rows = misordered_snapshot();
    rows[5].features.schema_version = FEATURE_SCHEMA_VERSION + 7;
    let error = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect_err("a mismatched feature schema must refuse");
    assert!(
        matches!(
            error,
            BanditError::SchemaMismatch {
                index: 5,
                component: "feature",
                ..
            }
        ),
        "got {error}"
    );
}

#[test]
fn a_non_finite_outcome_is_refused() {
    let mut rows = misordered_snapshot();
    rows[2].targets.cost = Some(f64::NAN);
    let error = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect_err("a non-finite cost must refuse");
    assert!(
        matches!(
            error,
            BanditError::NonFiniteOutcome {
                index: 2,
                field: "cost",
                ..
            }
        ),
        "got {error}"
    );

    let mut rows = misordered_snapshot();
    rows[1].targets.latency_ms = Some(f64::INFINITY);
    let error = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect_err("a non-finite latency must refuse");
    assert!(
        matches!(
            error,
            BanditError::NonFiniteOutcome {
                index: 1,
                field: "latency_ms",
                ..
            }
        ),
        "got {error}"
    );

    let mut rows = misordered_snapshot();
    rows[0].features.values[0] = f32::NAN;
    let error = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect_err("a non-finite feature must refuse");
    assert!(
        matches!(error, BanditError::InvalidSample { index: 0, .. }),
        "got {error}"
    );
}

#[test]
fn a_duplicate_sample_id_is_refused() {
    let mut rows = misordered_snapshot();
    let twin = rows[4].clone();
    rows.push(twin);
    let error = run_bandit(&rows, &declared_arms(), &fixture_config())
        .expect_err("a duplicate sample id must refuse");
    assert!(
        matches!(error, BanditError::DuplicateSampleId { .. }),
        "got {error}"
    );
}

#[test]
fn an_inadequate_fit_partition_is_refused() {
    let mut config = fixture_config();
    config.reward.min_pair_count = 100_000;
    let error = run_bandit(&misordered_snapshot(), &declared_arms(), &config)
        .expect_err("an unfittable partition must refuse");
    assert!(
        matches!(error, BanditError::InsufficientPairs { .. }),
        "got {error}"
    );

    // Holding out every cell leaves nothing to fit on.
    let mut config = fixture_config();
    config.reward.holdout_cells = 4;
    assert!(matches!(
        run_bandit(&misordered_snapshot(), &declared_arms(), &config),
        Err(BanditError::HoldoutTakesEveryCell {
            total: 4,
            holdout_cells: 4
        })
    ));
}

#[test]
fn an_unusable_arm_set_is_refused() {
    let config = fixture_config();
    let fitted = RewardPolicy::default();

    assert!(matches!(
        run_bandit(&misordered_snapshot(), &[], &config),
        Err(BanditError::NoArms)
    ));
    assert!(matches!(
        run_bandit(
            &misordered_snapshot(),
            &[RewardArm::new("", fitted.clone())],
            &config
        ),
        Err(BanditError::EmptyArmName { index: 0 })
    ));
    assert!(matches!(
        run_bandit(
            &misordered_snapshot(),
            &[RewardArm::new(FITTED_ARM_NAME, fitted.clone())],
            &config
        ),
        Err(BanditError::ReservedArmName { .. })
    ));
    assert!(matches!(
        run_bandit(
            &misordered_snapshot(),
            &[
                RewardArm::new("dup", fitted.clone()),
                RewardArm::new("dup", fitted.clone())
            ],
            &config
        ),
        Err(BanditError::DuplicateArmName { .. })
    ));
    assert!(matches!(
        run_bandit(
            &misordered_snapshot(),
            &[RewardArm::new("not-the-reference", fitted)],
            &config
        ),
        Err(BanditError::ReferenceArmUnknown { .. })
    ));
}

#[test]
fn refusals_carry_reasons_and_never_panic() {
    // Every refusal path is an `Err` with a rendered reason, so a caller that
    // logs the error learns something specific rather than "it failed".
    let cases: Vec<BanditError> = vec![
        BanditError::EmptySnapshot,
        BanditError::DegenerateReward { rows: 4 },
        BanditError::SchemaMismatch {
            index: 1,
            component: "sample",
            found: 9,
            expected: 1,
        },
        BanditError::NonFiniteOutcome {
            index: 2,
            field: "cost",
            value: f64::NAN,
        },
        BanditError::NonFittedWeight {
            component: "cost_weight",
            value: f64::INFINITY,
        },
        BanditError::NonFiniteScore {
            arm: "a".to_string(),
            value: f64::NAN,
        },
        BanditError::MeaninglessSafetyFloor(0.0),
        BanditError::MeaninglessTolerance {
            name: "max_cost_regression",
            value: f64::INFINITY,
        },
        BanditError::NoLatencyObserved,
        BanditError::ArmUnderObserved {
            name: "a".to_string(),
            observations: 0,
            min: 4,
        },
    ];
    for error in cases {
        let rendered = error.to_string();
        assert!(
            rendered.len() > 10 && !rendered.is_empty(),
            "a refusal must render a reason, got {rendered:?}"
        );
    }
}

#[test]
fn a_refused_run_yields_no_policy_and_no_verdict() {
    // The failure mode this rules out: a refusal that still hands back a
    // default-fitted policy, so a caller cannot tell it was refused.
    let mut config = fixture_config();
    config.safety.max_cost_regression = f64::INFINITY;
    let result = run_bandit(&misordered_snapshot(), &declared_arms(), &config);
    assert!(result.is_err());
    // `BanditOutcome` exposes the fitted policy only on success, so there is no
    // value to read here at all: `result` is an `Err`, and that is the whole
    // answer.
    assert!(matches!(
        result,
        Err(BanditError::MeaninglessTolerance { .. })
    ));
}

// ---------------------------------------------------------------------------
// Cross-checks against the accepted surfaces
// ---------------------------------------------------------------------------

#[test]
fn the_safety_metrics_agree_with_the_accepted_evaluator() {
    let outcome = run_bandit(&misordered_snapshot(), &declared_arms(), &fixture_config())
        .expect("the fixture snapshot must run");
    let report = outcome.report();

    for arm in &report.safety_by_arm {
        let accepted = &arm.accepted_metrics;
        assert!(
            (arm.failure_rate - (1.0 - accepted.success_rate)).abs() < 1e-12,
            "the gate's failure rate must agree with the accepted evaluator"
        );
        assert!(
            (arm.fallback_rate - accepted.fallback_rate).abs() < 1e-12,
            "the gate's fallback rate must agree with the accepted evaluator"
        );
        assert!(
            (arm.tail_latency_ms - accepted.p95_latency_ms).abs() < 1e-9,
            "the gate's tail latency must be the accepted evaluator's p95, got {} vs {}",
            arm.tail_latency_ms,
            accepted.p95_latency_ms
        );
        assert!(
            (arm.mean_cost - accepted.mean_cost).abs() < 1e-12,
            "the gate's mean cost must agree with the accepted evaluator"
        );
    }
}

#[test]
fn the_dataset_projection_is_the_one_warmup_uses() {
    // Both nodes read the canonical sample and project to the legacy shape only
    // where an accepted consumer needs it. This node's legacy rows must be what
    // `into_legacy` produces, and the accepted evaluator must see them.
    let outcome = run_bandit(snapshot(), &declared_arms(), &fixture_config()).expect("run");
    let evidence = outcome.report().evidence;

    // The accepted evaluator saw the projected rows; a projection that dropped the
    // targets would show up as an empty or degenerate metric set.
    for arm in &outcome.report().safety_by_arm {
        let accepted = &arm.accepted_metrics;
        assert_eq!(accepted.total_requests, evidence.total);
        assert!(accepted.success_rate > 0.0 && accepted.success_rate < 1.0);
        assert!(accepted.mean_cost > 0.0);
        assert!(accepted.p95_latency_ms > 0.0);
        assert!(accepted.fallback_rate > 0.0, "the fixture takes fallbacks");
    }
}

#[test]
fn a_legacy_projection_round_trip_is_preserved() {
    // The projection is the accepted one, applied to a real sample.
    let row = misordered_snapshot().remove(0);
    let legacy: DatasetTrainingSample = row.clone().into_legacy();
    assert_eq!(legacy.sample_id, row.sample_id);
    assert_eq!(legacy.outcome_id, row.outcome_id);
    assert_eq!(legacy.provider_id, row.provider_id);
    assert_eq!(legacy.model_id, row.model_id);
    assert_eq!(legacy.schema_version, row.schema_version);
    assert_eq!(legacy.targets, row.targets);
    assert!(
        legacy.feedback.is_empty(),
        "an absent Feedback must project to an empty signal list, never a fabricated rating"
    );
    let metrics = RoutingMetrics::from_samples(std::slice::from_ref(&legacy));
    assert_eq!(metrics.total_requests, 1);
}

// ---------------------------------------------------------------------------
// Helpers for the synthetic safety comparisons above
// ---------------------------------------------------------------------------

/// A stand-in arm profile, so a safety comparison can be constructed directly
/// without a dataset. The real path is `run_bandit`; these tests exist to pin the
/// verdict rule itself against each gated dimension in isolation.
struct ArmProfile {
    name: &'static str,
    mean_outcome_proxy_reward: f64,
    failure_rate: f64,
    mean_cost: f64,
    tail_latency_ms: f64,
    fallback_rate: f64,
}

impl ArmProfile {
    fn new(
        name: &'static str,
        mean_outcome_proxy_reward: f64,
        failure_rate: f64,
        mean_cost: f64,
        tail_latency_ms: f64,
        fallback_rate: f64,
    ) -> Self {
        Self {
            name,
            mean_outcome_proxy_reward,
            failure_rate,
            mean_cost,
            tail_latency_ms,
            fallback_rate,
        }
    }
}

fn default_tolerances() -> SafetyTolerances {
    SafetyTolerances {
        min_reward_improvement: 1e-9,
        max_failure_rate_regression: 0.0,
        max_cost_regression: 0.0,
        max_tail_latency_regression_ms: 0.0,
        max_fallback_rate_regression: 0.0,
    }
}

fn stub_metrics(stub: &ArmProfile) -> ArmSafetyMetrics {
    ArmSafetyMetrics {
        arm: stub.name.to_string(),
        mean_outcome_proxy_reward: stub.mean_outcome_proxy_reward,
        failure_rate: stub.failure_rate,
        mean_cost: stub.mean_cost,
        tail_latency_ms: stub.tail_latency_ms,
        tail_percentile: 0.95,
        fallback_rate: stub.fallback_rate,
        accepted_metrics: RoutingMetrics::default(),
    }
}

/// Build the safety comparison for a synthetic pair of profiles.
fn evaluate(
    reference: &ArmProfile,
    candidate: &ArmProfile,
    tolerances: SafetyTolerances,
) -> SafetyEvaluation {
    SafetyEvaluation {
        reference: reference.name.to_string(),
        candidate: candidate.name.to_string(),
        evaluation_samples: 100,
        reference_metrics: stub_metrics(reference),
        candidate_metrics: stub_metrics(candidate),
        mean_reward_delta: candidate.mean_outcome_proxy_reward
            - reference.mean_outcome_proxy_reward,
        failure_rate_delta: candidate.failure_rate - reference.failure_rate,
        mean_cost_delta: candidate.mean_cost - reference.mean_cost,
        tail_latency_delta_ms: candidate.tail_latency_ms - reference.tail_latency_ms,
        fallback_rate_delta: candidate.fallback_rate - reference.fallback_rate,
        tolerances,
        insufficient_reason: None,
    }
}

/// Wrap a safety comparison in a report, so `accepted_arm` can be exercised
/// through its real path.
fn report_for(evaluation: SafetyEvaluation, selected: &str) -> BanditReport {
    let verdict = evaluation.verdict();
    let evidence = SafetyEvidence {
        total: 100,
        successes: 90,
        failures: 10,
        cost_observations: 100,
        latency_observations: 90,
        fallback_observations: 10,
        feedback_signals_present: 0,
    };
    let candidate_metrics = evaluation.candidate_metrics.clone();
    let reference_metrics = evaluation.reference_metrics.clone();
    BanditReport {
        outcome_proxy_order: OUTCOME_PROXY_ORDER.to_string(),
        rows: 100,
        reward_fit: empty_fit_report(),
        arms: Vec::new(),
        safety_by_arm: vec![reference_metrics, candidate_metrics],
        selection: SelectionTrace {
            seed: 0,
            exploration_c: 0.0,
            replay_rows: 0,
            total_observations: 0,
            selected: selected.to_string(),
            tied_arms: Vec::new(),
        },
        evidence,
        safety: evaluation,
        safety_verdict: verdict,
    }
}

/// A fit report with nothing in it, for a synthetic report. Nothing in the
/// acceptance path reads any of it — `accepted_arm` recomputes from `safety`,
/// which is exactly what the forgery test proves.
fn empty_fit_report() -> RewardFitReport {
    RewardFitReport {
        target_description: String::new(),
        order_description: String::new(),
        outcome_proxy_not_preference: String::new(),
        cells_total: 0,
        fit_cells: 0,
        holdout_cells: 0,
        fit_rows: 0,
        holdout_rows: 0,
        pairs_fit: 0,
        pairs_holdout: 0,
        fit_objective_at_prior: 0.0,
        fit_objective_fitted: 0.0,
        prior_holdout_agreement: 0.0,
        fitted_holdout_agreement: 0.0,
        agreement_delta: 0.0,
        verdict: RewardFitVerdict::NotBetter,
        prior: RewardPolicy::default(),
        prior_weights: vec![1.0, 0.3, 0.1, -0.5, -0.2, 0.1],
        fitted: RewardPolicy::default(),
        fitted_weights: vec![1.0, 0.3, 0.1, -0.5, -0.2, 0.1],
        unidentifiable_weights: vec![UNIDENTIFIABLE_WEIGHT.to_string()],
        unidentifiable_reason: String::new(),
        feedback_signals_present: 0,
        distinct_outcome_shapes: 0,
    }
}
