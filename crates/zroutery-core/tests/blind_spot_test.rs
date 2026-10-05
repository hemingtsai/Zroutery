//! Is a provider added to the configuration actually reachable?
//!
//! # The defect this pins
//!
//! `ml_routing.exploration_probability` ships at `0.0`. `explore` returns
//! `Exploit` at zero *before it draws*, so with the shipped default exploration
//! can never route anywhere the deterministic plan did not already choose. A
//! candidate the plan never picks therefore receives no traffic from any source:
//! not from the model, which has no observation of it, and not from exploration.
//! It accumulates nothing, can never be discovered to be better, and the
//! operator who added it sees a configuration entry and a working router and no
//! indication that the two are unrelated.
//!
//! Measured cost of that default on the three-provider fixture: adding a fourth
//! provider at a non-winning priority gave it **0 of 60 requests**.
//!
//! Changing the default is a product decision this repository has deliberately
//! not taken on its own — a router that explores by default spends real money on
//! deliberate mistakes. So the fix here is not the number. It is that the
//! consequence becomes *visible*, which is what these tests check: the blind
//! candidates get named, and whether anything can ever close the gap is stated
//! rather than left for the operator to derive from a probability.
//!
//! # Why the linkage is measured, not asserted
//!
//! `MlStatus::blind_spots_are_permanent` claims that at probability zero nothing
//! can reach an unobserved candidate. That claim is only as good as `explore`'s
//! behaviour, and `explore` is a hash draw over `(seed, request_id)` — the kind
//! of function whose behaviour is easy to state and easy to get wrong. So the
//! fourth test below does not assert the linkage; it *derives* it, by running
//! `explore` over a body of request ids at each probability and comparing what
//! happened against what the status document says. Change `explore` to draw at
//! zero and the status document becomes a lie that this test catches.

use zroutery_core::ml::dataset::IngestionCounters;
use zroutery_core::ml::serving::{explore, ExplorationConfig, ExplorationOutcome};
use zroutery_core::ml::{BlindCandidate, MlRouterCounts, MlStatus, ShadowStatus};
use zroutery_core::observation::ObservationStore;

/// The candidates a two-provider configuration names, as `(model, provider)`.
fn two_providers() -> Vec<(String, String)> {
    vec![
        ("alpha-cheap".to_string(), "p1".to_string()),
        ("alpha-strong".to_string(), "p2".to_string()),
    ]
}

/// A status document carrying just the fields this question is about.
fn status(exploration_probability: f64, blind_candidates: Vec<BlindCandidate>) -> MlStatus {
    MlStatus {
        routing_enabled: false,
        durable_state: false,
        traces_open: false,
        model_store_open: false,
        active: None,
        active_decision: None,
        history: Vec::new(),
        routing: MlRouterCounts {
            rankings: 0,
            fallbacks: 0,
            explorations: 0,
            blind_explorations: 0,
            attached: false,
        },
        exploration_probability,
        exploration_seed: 0,
        blind_candidates,
        dataset: IngestionCounters {
            ingested: 0,
            samples: 0,
            no_decision_time_input: 0,
            rejected: 0,
            evicted_by_count: 0,
            evicted_by_age: 0,
            faults: 0,
        },
        traces: None,
        shadow: ShadowStatus {
            enabled: false,
            decisions_recorded: 0,
            faults: 0,
        },
        read_at: 0,
    }
}

/// **The candidate that has never served is named.** The list is derived from the
/// configuration against the observation store, so it reports what the operator
/// just did — added a provider — rather than what the router happened to try.
#[test]
fn a_configured_candidate_with_no_observations_is_named() {
    let observations = ObservationStore::new();
    // The plan's own pick has served; the other provider has not been touched.
    observations.record_success("alpha-strong", "p2", 40.0, Some(20.0));

    let blind = BlindCandidate::unobserved(
        two_providers()
            .iter()
            .map(|(model, provider)| (model.as_str(), provider.as_str())),
        &observations,
    );

    assert_eq!(
        blind,
        vec![BlindCandidate {
            model_id: "alpha-cheap".to_string(),
            provider_id: "p1".to_string(),
        }],
        "exactly the provider that has never served should be listed"
    );
}

/// **A candidate that was tried and failed is not blind.** This is the false-alarm
/// guard, and it is the one that matters: a report which cried "unreachable" about
/// a provider that is failing on every request would train its reader to ignore
/// it, which is worse than not reporting at all.
#[test]
fn a_candidate_that_only_failed_is_not_blind() {
    let observations = ObservationStore::new();
    observations.record_failure("alpha-cheap", "p1");
    observations.record_success("alpha-strong", "p2", 40.0, Some(20.0));

    let blind = BlindCandidate::unobserved(
        two_providers()
            .iter()
            .map(|(model, provider)| (model.as_str(), provider.as_str())),
        &observations,
    );

    assert!(
        blind.is_empty(),
        "a provider tried {n} times and failed every time has been observed; \
         listing it as unreachable would be wrong. Listed: {blind:?}",
        n = observations.get("alpha-cheap", "p1").health.total_requests,
        blind = blind
    );
}

/// **With exploration off, the gap is permanent and says so.** This is the whole
/// point: `exploration_probability: 0` on its own is a number, and an operator
/// has to know it means *nothing random ever happens*. The status document says
/// it in a sentence.
#[test]
fn with_exploration_off_a_blind_candidate_is_permanent() {
    let blind = vec![BlindCandidate {
        model_id: "alpha-cheap".to_string(),
        provider_id: "p1".to_string(),
    }];
    let doc = status(0.0, blind);

    assert!(
        doc.blind_spots_are_permanent(),
        "one unobserved candidate with exploration off is the permanent case"
    );
    let warning = doc
        .blind_spot_warning()
        .expect("a permanent blind spot must produce a warning");
    for expected in [
        "alpha-cheap",
        "exploration_probability",
        "never tried",
        "no traffic will ever reach it",
    ] {
        assert!(
            warning.contains(expected),
            "the warning should name {expected:?} so the reader knows what to change; \
             got: {warning}"
        );
    }

    // The warning must not recommend the option that does not work.
    //
    // Raising exploration is the obvious advice and it is a trap: exploration
    // starves the paired evidence the promotion gate needs, so the operator would
    // trade an unreachable provider for a permanently unpromotable model and end
    // up on the deterministic plan anyway. That finding is measured in
    // `exploration_starves_the_evidence_a_promotion_needs`. A warning that says
    // "raise exploration_probability" without saying this is worse than no
    // warning, because it is acted on.
    assert!(
        warning.contains("blocks promotion"),
        "the warning has to say what raising exploration actually costs, or the \
         obvious reading is that it is a free fix; got: {warning}"
    );
    assert!(
        warning.contains("top priority"),
        "the warning should still offer the option that works; got: {warning}"
    );
}

/// **With exploration on, an unobserved candidate is a cold start, not a fault.**
///
/// The same status document with one number changed must go quiet. Reporting a
/// cold start as a problem would be noise, and noise is how a real report stops
/// being read.
#[test]
fn with_exploration_on_an_unobserved_candidate_is_only_a_cold_start() {
    let blind = vec![BlindCandidate {
        model_id: "alpha-cheap".to_string(),
        provider_id: "p1".to_string(),
    }];

    assert!(
        !status(0.05, blind.clone()).blind_spots_are_permanent(),
        "exploration on means traffic can resolve the gap"
    );
    assert_eq!(
        status(0.05, blind).blind_spot_warning(),
        None,
        "a cold start that exploration will resolve is not a warning"
    );
    // And with nothing unobserved there is nothing to say at any probability,
    // including zero — which is the common case on a single-provider install
    // and must not produce a spurious warning.
    assert_eq!(
        status(0.0, Vec::new()).blind_spot_warning(),
        None,
        "a one-provider installation has no blind spot and should not be warned about"
    );
}

/// **The status document agrees with what `explore` actually does.**
///
/// The claim under test is not "`exploration_probability` is 0" — it is "nothing
/// can reach an unobserved candidate". So rather than asserting the linkage,
/// this runs `explore` over a body of request ids at each probability, counts how
/// often it left `Exploit`, and requires the document's verdict to match.
///
/// If someone changes `explore` to draw at probability zero, the count at 0
/// becomes non-zero while the document still says permanent, and this fails. That
/// is the mutation it exists to catch, and it is a mutation no reading of the
/// status struct would reveal.
#[test]
fn blind_spot_warning_matches_what_exploration_actually_does() {
    let eligible = vec!["alpha-strong".to_string(), "alpha-cheap".to_string()];
    let pick = "alpha-strong";

    // A body of request ids wide enough that a 5% draw cannot plausibly miss
    // every one of them.
    let requests: Vec<String> = (0..2_000).map(|i| format!("req-{i}")).collect();

    let left_exploit = |probability: f64| {
        let config = ExplorationConfig {
            probability,
            seed: 42,
        };
        requests
            .iter()
            .filter(|request_id| {
                !matches!(
                    explore(&config, request_id, pick, &eligible),
                    ExplorationOutcome::Exploit
                )
            })
            .count()
    };

    let at_zero = left_exploit(0.0);
    let at_five_percent = left_exploit(0.05);

    assert_eq!(
        at_zero,
        0,
        "explore left Exploit {at_zero} times at probability 0 over {} requests. If \
         this is no longer zero the early return has changed, and \
         `blind_spots_are_permanent` is now claiming a permanence that does not \
         exist.",
        requests.len()
    );
    assert!(
        at_five_percent > requests.len() / 100,
        "at probability 0.05 exploration left Exploit on only {at_five_percent} of {} \
         requests; the second arm of this comparison is not doing its job if \
         exploration never fires",
        requests.len()
    );

    // Now the document must say what the function did.
    let blind = vec![BlindCandidate {
        model_id: "alpha-cheap".to_string(),
        provider_id: "p1".to_string(),
    }];
    assert_eq!(
        status(0.0, blind.clone()).blind_spots_are_permanent(),
        at_zero == 0,
        "the document's verdict must follow explore's measured behaviour"
    );
    assert_eq!(
        status(0.05, blind).blind_spots_are_permanent(),
        at_five_percent == 0,
        "the document's verdict must follow explore's measured behaviour"
    );
}

/// The list is sorted, so two reads of an unchanged configuration diff cleanly.
/// An operator watching this over time should not see it reshuffle because a
/// `HashMap` iteration order moved.
#[test]
fn the_blind_list_is_ordered() {
    let observations = ObservationStore::new();
    let candidates = [
        ("zeta/last".to_string(), "p9".to_string()),
        ("alpha/first".to_string(), "p1".to_string()),
        ("middle/one".to_string(), "p5".to_string()),
    ];

    let blind = BlindCandidate::unobserved(
        candidates
            .iter()
            .map(|(model, provider)| (model.as_str(), provider.as_str())),
        &observations,
    );
    let ids: Vec<&str> = blind.iter().map(|b| b.model_id.as_str()).collect();

    assert_eq!(
        ids,
        vec!["alpha/first", "middle/one", "zeta/last"],
        "an unobserved set should be reported in a stable order"
    );
}
