//! Does the cost head use the cost axis?
//!
//! # The question
//!
//! `ModelEnsemble` has four heads. Three of them — success, latency and TTFT — had
//! targets for as long as anyone could remember. The fourth, cost, had none:
//! `Attempt` had no cost field, so every attempt-scoped sample carried
//! `targets.cost: None`, so `ModelEnsemble::update_all` skipped the cost head on
//! every sample and it stayed cold.
//!
//! That made the cost axis *inert* in a way that was easy to mistake for *working
//! but unhelpful*. Once targets arrive, two separate questions open, and passing
//! one says nothing about the other:
//!
//! 1. Does the head **learn** the targets it is given?
//! 2. Does what it learned **reach a routing decision**?
//!
//! Both are answered here, and both are written so that a head which ignores cost
//! — or which learns it and has it discarded on the way to the decision — cannot
//! pass.
//!
//! # The confound, and why these tests are shaped the way they are
//!
//! The obvious end-to-end check is worthless. In the routing fixtures, a
//! candidate's cost is a deterministic function of *which model it is*: one model
//! is cheap, another is nine times dearer. A head that learned nothing about cost
//! but instead correlated cost with whatever features happen to separate those two
//! models — priority, tier, observation statistics — would score just as well on
//! any fixture we have.
//!
//! So these tests never go through model identity. They hold the features fixed and
//! vary only the cost target, which is the only arrangement in which "predicted
//! cost tracks trained cost" means what it says.

use zroutery_core::ml::dataset::Targets;
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
};
use zroutery_core::ml::model_identity::ModelEnsemble;
use zroutery_core::ml::reward::{compute_utility, PredictionBundle, RewardPolicy};
use zroutery_core::ml::{DatasetTrainingSample, Prediction, RoutingModel};

/// The features every sample in these tests carries.
///
/// Deliberately unremarkable: two non-zero coordinates and the rest at `UNKNOWN`,
/// so nothing about the vector implies a price. The two coordinates are what give
/// the linear head something to put weight on.
fn features(marker: f32) -> RoutingFeatures {
    let mut values = [UNKNOWN; FEATURE_DIMENSION];
    values[0] = marker;
    values[1] = -marker;
    RoutingFeatures {
        schema_version: FEATURE_SCHEMA_VERSION,
        values,
    }
}

/// One attempt-scoped sample whose only cost-relevant content is `cost`.
fn sample(index: usize, marker: f32, cost: Option<f64>) -> DatasetTrainingSample {
    DatasetTrainingSample {
        sample_id: format!("cost-{index}"),
        schema_version: 1,
        timestamp: 1_700_000_000 + index as i64,
        features: features(marker),
        targets: Targets {
            success: true,
            latency_ms: Some(100.0),
            ttft_ms: None,
            cost,
            failure_class: None,
            fallback_count: 0,
        },
        provider_id: "p".to_string(),
        model_id: "m".to_string(),
        origin: zroutery_core::feedback::DataOrigin::Native,
        outcome_id: format!("out-{index}"),
        feedback: Vec::new(),
    }
}

/// Train an ensemble on `repeats` passes of a body whose cost is constant at
/// `cost_per_sample`.
fn trained_on_cost(cost_per_sample: f64, repeats: usize) -> ModelEnsemble {
    let mut ensemble = ModelEnsemble::new();
    for pass in 0..repeats {
        for index in 0..40 {
            // The marker alternates so the head has a feature to weight, and it is
            // uncorrelated with cost: every sample costs the same.
            let marker = if index % 2 == 0 { 0.5 } else { -0.5 };
            ensemble.update_all(&sample(pass * 100 + index, marker, Some(cost_per_sample)));
        }
    }
    ensemble
}

/// The cost head's prediction for a probe vector.
fn predicted_cost(ensemble: &ModelEnsemble) -> f64 {
    let bundle = zroutery_core::ml::predict_bundle(ensemble, "m", "p", &features(0.5));
    bundle.cost.value
}

/// **The head learns its targets.** Features held fixed, cost varied by 10x.
///
/// This is the test that rules out "the cost head is decorative". `update_all`
/// skips the cost head when `targets.cost` is `None`, so a head that ignored its
/// targets would return the cold default for both ensembles and the ratio below
/// would be 1.0, not 10.
#[test]
fn the_cost_head_learns_the_targets_it_is_given() {
    let cheap = trained_on_cost(0.001, 8);
    let dear = trained_on_cost(0.010, 8);

    let cheap_prediction = predicted_cost(&cheap);
    let dear_prediction = predicted_cost(&dear);

    assert!(
        cheap_prediction > 0.0,
        "a head trained on a cost of 0.001 predicts {cheap_prediction}; a cold or \
         collapsed head would predict the 0.0 floor it clamps to"
    );
    let ratio = dear_prediction / cheap_prediction;
    assert!(
        (ratio - 10.0).abs() < 0.5,
        "training on costs 10x apart produced predictions {cheap_prediction} and \
         {dear_prediction}, a ratio of {ratio:.3}. The head is linear with a bias, \
         so the ratio should be 10 to within the online fit's error."
    );

    // And the head says it was trained. A head that never saw a target reports zero
    // samples, which is what made the original defect invisible in every report.
    let samples = cheap.cost.sample_count();
    assert!(
        samples >= 300,
        "the cost head reports {samples} samples after 8 passes over 40; \
         update_all is skipping it"
    );
}

/// **Targets absent means untouched.** The other half of the original defect.
///
/// This is why the axis was inert rather than merely wrong: a sample with no cost
/// leaves the head exactly where it was, so a body of cost-free samples produces
/// a head that has learned nothing while looking perfectly healthy.
#[test]
fn samples_without_a_cost_leave_the_cost_head_untouched() {
    let mut ensemble = ModelEnsemble::new();
    for index in 0..200 {
        ensemble.update_all(&sample(index, 0.5, None));
    }
    assert_eq!(
        ensemble.cost.sample_count(),
        0,
        "the cost head took {} samples from cost-free samples; it should take none",
        ensemble.cost.sample_count()
    );
    // The cold head predicts `0.01`, not zero. Worth stating rather than assuming:
    // an untrained cost head looks like a *cheap* candidate rather than an absent
    // one, so a body with no cost targets does not read as "cost unknown" — it
    // reads as "cost is about a cent". Nothing downstream can tell those apart,
    // because a prediction carries no statement about whether it was trained.
    // `Prediction::cold` carries that, and the cost term ignores it.
    assert_eq!(
        predicted_cost(&ensemble),
        0.01,
        "the cold cost head's seeded bias changed; this test also documents that a \
         never-trained cost head reports a plausible non-zero figure"
    );
    // What actually tells the decision that this head knows nothing is the
    // confidence, not `Prediction::cold`: no head ever constructs a cold
    // prediction, so that flag is `false` on every ensemble prediction including
    // a wholly untrained one. `compute_utility` folds confidence into its
    // uncertainty term, so this is the signal that reaches the ranking — and the
    // only one.
    let prediction = ensemble.cost.predict(&features(0.5));
    assert!(
        !prediction.cold,
        "the ensemble heads report `cold: false` even at zero samples; if a head \
         starts constructing cold predictions, this test should say so"
    );
    assert!(
        prediction.confidence <= 0.2,
        "an untrained cost head reports confidence {}; below 20 samples it should \
         report near the floor, because confidence is the only thing that tells the \
         decision the head knows nothing",
        prediction.confidence
    );
    // The other heads are unaffected, which is the point: this is a per-head gate,
    // not a broken sample.
    assert!(
        ensemble.success.sample_count() >= 200,
        "the success head should have trained regardless of cost"
    );
}

/// **What the head learned reaches the decision.** The bundle's cost is not dropped
/// between the ensemble and the utility, and the arithmetic is the documented one.
///
/// Without this, a head could learn perfectly and the routing path could ignore it,
/// and every test above would still pass.
#[test]
fn a_learned_cost_reaches_the_ranking_utility() {
    let cheap = trained_on_cost(0.001, 8);
    let dear = trained_on_cost(0.010, 8);
    let policy = RewardPolicy::default();

    let cheap_bundle = zroutery_core::ml::predict_bundle(&cheap, "m", "p", &features(0.5));
    let dear_bundle = zroutery_core::ml::predict_bundle(&dear, "m", "p", &features(0.5));

    // **This assertion is the guard, and it has to come first.**
    //
    // Without it, everything below degenerates. If the cost head were disabled,
    // both ensembles would predict the seeded bias, `dear - cheap` would be zero,
    // and the algebraic identity being checked would hold as `0 == 0` — the test
    // would pass with the cost axis disconnected. Verified: disabling
    // `ModelEnsemble::update_all`'s cost branch leaves this test green and fails
    // only `the_cost_head_learns_the_targets_it_is_given`.
    assert!(
        (dear_bundle.cost.value - cheap_bundle.cost.value).abs() > 1e-9,
        "the two ensembles predict the same cost ({} and {}), so nothing downstream \
         can distinguish them and the identity below would hold trivially",
        cheap_bundle.cost.value,
        dear_bundle.cost.value
    );

    let cheap_utility = compute_utility(&cheap_bundle, &policy, false, 0);
    let dear_utility = compute_utility(&dear_bundle, &policy, false, 0);

    // The cost term is exactly the documented weight applied to the clamped value,
    // and it is the *only* term that differs: both bundles were built from the
    // same features and differ solely in their cost prediction.
    assert!(
        cheap_utility.cost < 0.0,
        "a predicted cost of {} produced a cost term of {}; a non-positive cost \
         term means the head's output never reached the utility",
        cheap_bundle.cost.value,
        cheap_utility.cost
    );
    let expected_gap =
        policy.cost_weight * (dear_bundle.cost.value - cheap_bundle.cost.value).min(1.0);
    let actual_gap = cheap_utility.total - dear_utility.total;
    assert!(
        (actual_gap - expected_gap).abs() < 1e-9,
        "the utility gap between a cheap and a dear candidate is {actual_gap}, but \
         the cost head's predictions account for {expected_gap}. Something between \
         the bundle and the total is absorbing the difference."
    );
    // And the other three terms are identical, so the gap really is the cost axis
    // and not two predictions drifting together.
    assert_eq!(cheap_utility.success, dear_utility.success);
    assert_eq!(cheap_utility.latency, dear_utility.latency);
    assert_eq!(cheap_utility.ttft, dear_utility.ttft);
}

/// **The scale, measured.** Why the cost head has never changed a routing decision
/// on these fixtures, stated as a number rather than as an impression.
///
/// `compute_utility` scores cost as `-cost_weight * min(cost_dollars, 1.0)`. A
/// real per-request LLM bill is cents, so the clamp never bites and the term is
/// `0.1 x dollars` — while a candidate that succeeds where another fails is worth
/// a full `1.0`. On the three-provider fixture the dearest provider costs 9x the
/// cheapest and the entire utility consequence is three orders of magnitude below
/// the success term.
///
/// **This is arithmetic, not wiring.** The bundles are constructed by hand, so this
/// test is unaffected by whether the cost head is connected, and it would pass
/// unchanged with the head disabled. That is the point: it pins the *weights*, and
/// the wiring is pinned by the two tests above.
///
/// It is pinned as a test so that changing the normalisation cannot happen
/// silently. If someone fixes the scale, this fails and the change has to be
/// argued for rather than slipped in — and arguing for it is the right outcome,
/// because a cost term that can outvote reliability is a policy decision nobody
/// has made.
#[test]
fn cost_cannot_outvote_success_at_the_shipped_weights() {
    let policy = RewardPolicy::default();

    let bundle = |success: f64, cost: f64| PredictionBundle {
        candidate_model: "m".into(),
        candidate_provider: "p".into(),
        success: Prediction::trained(success, 0.9, 500),
        latency: Prediction::trained(40.0, 0.9, 500),
        ttft: Prediction::trained(40.0, 0.9, 500),
        cost: Prediction::trained(cost, 0.9, 500),
    };

    // The two prices on the three-provider fixture, nine times apart.
    let cheap_and_reliable = bundle(0.75, 0.00088);
    let dear_and_reliable = bundle(0.75, 0.00792);
    let cheap_and_failing = bundle(0.0, 0.00088);

    let cheap_total = compute_utility(&cheap_and_reliable, &policy, false, 0).total;
    let dear_total = compute_utility(&dear_and_reliable, &policy, false, 0).total;
    let failing_total = compute_utility(&cheap_and_failing, &policy, false, 0).total;

    // Nine times the price, and the utility difference is real but tiny.
    let cost_gap = cheap_total - dear_total;
    // And a success difference of the same candidates is enormous by comparison.
    let success_gap = cheap_total - failing_total;

    assert!(
        cost_gap > 0.0,
        "paying nine times more should score worse on cost; cheap {cheap_total} vs \
         dear {dear_total}"
    );
    assert!(
        success_gap > 0.0,
        "succeeding where another candidate fails should score better"
    );
    assert!(
        cost_gap < success_gap / 100.0,
        "at the shipped weights, nine times the price moves utility by {cost_gap:.6} \
         while a success difference moves it by {success_gap:.6} — a ratio of \
         {:.0}:1. The cost head can inform the ranking and still never outvote a \
         reliability difference.",
        success_gap / cost_gap
    );
}
