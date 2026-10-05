#![cfg(feature = "ml")]

//! Node 7E-2D gate tests: offline calibration and the K-way distribution.
//!
//! The claims under test are the ones this node owns —
//!
//! 1. purity: the module reaches no installation, scheduling, or routing
//!    surface;
//! 2. K-way normalization: the emitted vector sums to one within the 7E-2A
//!    tolerance, has arity equal to the axis, is accepted by the 7E-2A type,
//!    and an unranked candidate takes exactly `0.0` without moving the rest;
//! 3. calibration metrics: a binned reliability curve with per-bin counts and a
//!    summary error, measured **over the final emitted vector** through an API
//!    that cannot accept pre-normalization numbers, and sharp enough to catch a
//!    confidently-wrong model;
//! 4. determinism: the same snapshot and configuration give byte-identical
//!    calibrator and report, *and* two logically identical snapshots carrying
//!    deliberately different UUIDs give the same partition, calibrator,
//!    distributions, measurements and verdict;
//! 5. holdout and drift: the partitions are disjoint, a shifted holdout is
//!    refused, and a degenerate partition is refused rather than producing a
//!    calibrated-looking vector;
//! 6. fail-closed: every malformed input is a typed refusal with a reason, and
//!    the module holds no panic path.

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::calibration::{
    measure_emitted, measure_marginal, project_cohorts, run_calibration, AcceptanceTolerances,
    CalibrationConfig, CalibrationError, CalibrationMeasure, CalibrationVerdict,
    CandidateCalibration, CandidateInput, CohortContext, DecisionCohort, DegeneracyReason,
    DriftConfig, DriftVerdict, EmittedDecision, FitConfig, HoldoutConfig, KWayCalibrator,
    MarginalView, PartitionKind, ReliabilityBin, ReliabilityConfig, UnrankedReason,
    DEFAULT_PROBABILITY_FLOOR, DISTRIBUTION_ROLE,
};
use zroutery_core::ml::dataset::{
    try_samples_from_outcome, OutcomeTrainingSample, SampleScope, Targets,
};
use zroutery_core::ml::decision_contract::{
    DecisionContractError, DecisionDistribution, DISTRIBUTION_NORMALIZATION_TOLERANCE,
};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model::{ModelState, Prediction, RoutingModel};
use zroutery_core::outcome::{
    Attempt, CandidateIdentity, FailureFacts, FinalStatus, Outcome, OutcomeIdentity,
};

// ---------------------------------------------------------------------------
// Fixture: scripted success heads
// ---------------------------------------------------------------------------

/// The feature index a scripted head reads its answer from.
const SCRIPT_INDEX: usize = 0;

/// A success head whose output is scripted per candidate, in per-mille.
///
/// The accepted `SuccessModel` cannot produce the specific miscalibration this
/// node has to measure — a head that is confidently wrong about one candidate
/// while being right about the others — so the fixture supplies one. It reads
/// the same fixed-size feature vector every other head reads and returns a
/// value in `[0, 1]`, so it is a `RoutingModel` like any other and the
/// calibration code cannot tell it apart from a trained one.
struct ScriptedModel {
    /// Raw success probability in per-mille, indexed by the candidate tag the
    /// feature vector carries.
    table: Vec<u16>,
}

impl ScriptedModel {
    fn new(table: &[u16]) -> Self {
        let mut values = table.to_vec();
        while values.len() < 4 {
            values.push(500);
        }
        Self { table: values }
    }
}

impl RoutingModel for ScriptedModel {
    fn name(&self) -> &str {
        "scripted-success"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        // The feature vector carries a candidate tag in its whole part and the
        // scripted probability in its per-mille part, so one vector fully
        // determines this head's answer.
        let encoded = features.values[SCRIPT_INDEX];
        let tag = if encoded < 0.0 {
            0_usize
        } else {
            (f64::from(encoded) / 1000.0).clamp(0.0, 64.0) as usize
        };
        let milli = self
            .table
            .get(tag)
            .copied()
            .unwrap_or_else(|| self.table.first().copied().unwrap_or(500));
        Prediction::trained(f64::from(milli) / 1000.0, 0.5, 1_000)
    }

    fn update(&mut self, _features: &RoutingFeatures, _target: f64) {}

    fn sample_count(&self) -> u64 {
        1_000
    }

    fn save(&self) -> ModelState {
        let mut parameters: Vec<f64> = self.table.iter().map(|milli| f64::from(*milli)).collect();
        parameters.push(0.0);
        ModelState::new("scripted-success", parameters)
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        if state.parameters.len() < 2 {
            return Err("scripted head state is truncated".to_string());
        }
        Ok(Self {
            table: state.parameters[..state.parameters.len() - 1]
                .iter()
                .map(|value| *value as u16)
                .collect(),
        })
    }

    fn reset(&mut self) {
        self.table.fill(500);
    }
}

/// A head that echoes the per-mille recorded in the feature vector.
///
/// The drift fixture needs a head whose output genuinely differs between the two
/// partitions. A table-indexed head cannot do that, because it returns the same
/// number for the same candidate tag in both partitions and the shift cancels.
/// This one reads what the snapshot recorded, which is what a real head does.
struct FeatureEchoModel;

impl RoutingModel for FeatureEchoModel {
    fn name(&self) -> &str {
        "feature-echo-success"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        let encoded = f64::from(features.values[SCRIPT_INDEX]);
        Prediction::trained(
            (encoded - 1000.0 * (encoded / 1000.0).trunc()).max(0.0) / 1000.0,
            0.5,
            1_000,
        )
    }

    fn update(&mut self, _features: &RoutingFeatures, _target: f64) {}

    fn sample_count(&self) -> u64 {
        1_000
    }

    fn save(&self) -> ModelState {
        ModelState::new(self.name(), vec![0.0, 0.0])
    }

    fn load(_state: &ModelState) -> Result<Self, String> {
        Ok(Self)
    }

    fn reset(&mut self) {}
}

/// A head that emits a fixed probability for every candidate.
struct ConstantModel(f64);
impl RoutingModel for ConstantModel {
    fn name(&self) -> &str {
        "constant-success"
    }

    fn predict(&self, _features: &RoutingFeatures) -> Prediction {
        Prediction::trained(self.0, 0.5, 1_000)
    }

    fn update(&mut self, _features: &RoutingFeatures, _target: f64) {}

    fn sample_count(&self) -> u64 {
        1_000
    }

    fn save(&self) -> ModelState {
        ModelState::new(self.name(), vec![self.0, 0.0])
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        Ok(Self(state.parameters.first().copied().unwrap_or(0.5)))
    }

    fn reset(&mut self) {}
}

/// A head that emits a number no probability may be.
struct BrokenModel(f64);

impl RoutingModel for BrokenModel {
    fn name(&self) -> &str {
        "broken-success"
    }

    fn predict(&self, _features: &RoutingFeatures) -> Prediction {
        Prediction::trained(self.0, 0.5, 1_000)
    }

    fn update(&mut self, _features: &RoutingFeatures, _target: f64) {}

    fn sample_count(&self) -> u64 {
        1_000
    }

    fn save(&self) -> ModelState {
        ModelState::new(self.name(), vec![self.0, 0.0])
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        Ok(Self(state.parameters.first().copied().unwrap_or(f64::NAN)))
    }

    fn reset(&mut self) {}
}

// ---------------------------------------------------------------------------
// Fixture: the snapshot
// ---------------------------------------------------------------------------

const ALPHA: (&str, &str) = ("p-alpha", "model-alpha");
const BRAVO: (&str, &str) = ("p-bravo", "model-bravo");
const CHARLIE: (&str, &str) = ("p-charlie", "model-charlie");

/// Per-mille the honest head reports for each candidate, by tag order.
const HONEST_MILLI: [u16; 3] = [900, 550, 400];
/// A head certain that `alpha` serves, and wrong about it most of the time.
const CONFIDENT_WRONG_MILLI: [u16; 3] = [950, 30, 30];
/// A head whose ranking is the exact reverse of the truth, and confident.
const REVERSED_MILLI: [u16; 3] = [900, 100, 400];
/// A head that is correctly ranked and merely over-confident.
const MILD_MILLI: [u16; 3] = [350, 820, 680];
/// The other regime, for the drift fixture: the same candidates, reported at
/// the opposite end of the unit interval.
const SHIFTED_MILLI: [u16; 3] = [120, 880, 610];

/// One decision in the fixture.
#[derive(Clone)]
struct Spec {
    /// The candidates the decision considered, in attempt order, each with the
    /// per-mille the scripted head reports for it.
    candidates: Vec<(&'static str, &'static str, u16)>,
    /// Who served, or `None` when the request failed outright.
    served: Option<(&'static str, &'static str)>,
}

fn identity(pair: (&str, &str)) -> CandidateIdentity {
    CandidateIdentity::new(pair.1, pair.0)
}

const BASE_TIMESTAMP: i64 = 1_700_000_000;

/// The design of the main fixture.
///
/// Three candidates, one decision per cohort, every candidate attempted every
/// time. Every fifth request fails outright, so the "nobody served" path is
/// exercised rather than assumed.
///
/// The winner is a repeating four-cycle, `bravo, bravo, charlie, alpha`, so the
/// empirical winner distribution is **exactly** `(alpha 0.25, bravo 0.50,
/// charlie 0.25)` in any run of whole cycles. With 200 decisions and a 40-decision
/// holdout, both partitions are whole numbers of cycles, so the fit partition
/// and the holdout have *identical* empirical frequencies. That matters: it
/// removes sampling noise from the comparison, so every gap the report shows
/// is a calibration gap and not a small-sample artefact.
///
/// The head's own ranking is `(alpha, bravo, charlie)` and its three numbers sum
/// to 1.85 — not to one. Both halves matter. The first is what a normalization
/// step cannot repair, and the second is what a normalization step silently
/// assumes away. The head's ranking is also *non-monotone* with respect to the
/// truth: `alpha` scores highest and ties for lowest, so no shared one-dimensional
/// monotone map can reproduce `(0.25, 0.50, 0.25)` at all. That is the structural
/// reason the independent route cannot win here, and it is a property of the
/// fixture rather than of the optimizer.
fn main_fixture(count: usize) -> Vec<Spec> {
    const CYCLE: [(&str, &str); 4] = [BRAVO, BRAVO, CHARLIE, ALPHA];
    let mut specs = Vec::with_capacity(count);
    let mut cursor = 0_usize;
    for index in 0..count {
        // Every fifth request fails outright, so the "nobody served" path is
        // exercised rather than assumed.
        let served = if index % 5 == 4 {
            None
        } else {
            let winner = CYCLE[cursor % CYCLE.len()];
            cursor += 1;
            Some(winner)
        };
        specs.push(Spec {
            candidates: honest_axis(),
            served,
        });
    }
    specs
}

/// The fixture's axis: the three candidates, each with the per-mille the honest
/// head reports for it.
fn honest_axis() -> Vec<(&'static str, &'static str, u16)> {
    vec![
        (ALPHA.0, ALPHA.1, HONEST_MILLI[0]),
        (BRAVO.0, BRAVO.1, HONEST_MILLI[1]),
        (CHARLIE.0, CHARLIE.1, HONEST_MILLI[2]),
    ]
}

/// The same decisions, but every decision from `from` onwards is recorded in a
/// different regime: the head's output moves to the other end of the unit
/// interval. This is what a real regime change looks like to a probability
/// head, and it is what the drift gate has to notice.
fn shifted_fixture(specs: &[Spec], from: usize) -> Vec<Spec> {
    specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            if index < from {
                return spec.clone();
            }
            Spec {
                candidates: spec
                    .candidates
                    .iter()
                    .map(|(provider, model, _)| {
                        let milli = if *provider == ALPHA.0 {
                            SHIFTED_MILLI[0]
                        } else if *provider == BRAVO.0 {
                            SHIFTED_MILLI[1]
                        } else {
                            SHIFTED_MILLI[2]
                        };
                        (*provider, *model, milli)
                    })
                    .collect(),
                served: spec.served,
            }
        })
        .collect()
}

/// A fixture whose head is *correctly ranked* and merely over-confident.
///
/// The empirical winner distribution is exactly
/// `(alpha 1/6, bravo 1/2, charlie 1/3)`, which is **monotone in the head's
/// own ranking** `(alpha, bravo, charlie) = (0.35, 0.82, 0.68)`. A shared
/// one-dimensional monotone map can represent that, so this scenario is the
/// control for the reversed-ranking one: whatever the joint route does here, it
/// is not winning because the independent route was handed an impossible task.
fn mild_fixture(count: usize) -> Vec<Spec> {
    const CYCLE: [(&str, &str); 6] = [BRAVO, BRAVO, BRAVO, CHARLIE, CHARLIE, ALPHA];
    let axis = vec![
        (ALPHA.0, ALPHA.1, MILD_MILLI[0]),
        (BRAVO.0, BRAVO.1, MILD_MILLI[1]),
        (CHARLIE.0, CHARLIE.1, MILD_MILLI[2]),
    ];
    let mut specs = Vec::with_capacity(count);
    let mut cursor = 0_usize;
    for index in 0..count {
        let served = if index % 5 == 4 {
            None
        } else {
            let winner = CYCLE[cursor % CYCLE.len()];
            cursor += 1;
            Some(winner)
        };
        specs.push(Spec {
            candidates: axis.clone(),
            served,
        });
    }
    specs
}

/// Render a fixture into a canonical snapshot.
///
/// `salt` replaces every identifier, so the same logical data can be rendered
/// twice under deliberately different UUID-derived ids.
fn render(specs: &[Spec], salt: u64) -> Vec<OutcomeTrainingSample> {
    let mut rows = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        let outcome_id = format!("req_{salt:016x}{index:08x}");
        let decision_id = format!("dec-{salt:016x}{index:08x}");
        let served = spec.served.map(identity);
        let status = if served.is_some() {
            FinalStatus::Success
        } else {
            FinalStatus::Failed
        };
        let planned = spec
            .candidates
            .first()
            .map(|(provider, model, _)| identity((*provider, *model)));

        let attempts: Vec<Attempt> = spec
            .candidates
            .iter()
            .enumerate()
            .map(|(slot, (provider, model, _))| {
                let won = served.as_ref().is_some_and(|winner| {
                    winner.provider() == *provider && winner.model() == *model
                });
                Attempt {
                    attempt_id: format!("att_{salt:016x}{index:08x}{slot:04x}"),
                    candidate_model: (*model).to_string(),
                    candidate_provider: (*provider).to_string(),
                    started_at: BASE_TIMESTAMP + index as i64,
                    completed_at: BASE_TIMESTAMP + index as i64 + 1,
                    latency_ms: if won { 120.0 } else { 30.0 },
                    ttft_ms: if won { Some(40.0) } else { None },
                    success: won,
                    failure_class: if won {
                        None
                    } else {
                        Some(FailureClass::ProviderUnavailable)
                    },
                    failure_message: if won {
                        None
                    } else {
                        Some("scripted failure".to_string())
                    },
                    http_status: if won { Some(200) } else { Some(503) },
                    rectified: false,
                    cost: None,
                }
            })
            .collect();

        for slot in 0..spec.candidates.len() {
            let (provider, model, milli) = spec.candidates[slot];
            let won = served
                .as_ref()
                .is_some_and(|winner| winner.provider() == provider && winner.model() == model);
            rows.push(OutcomeTrainingSample {
                sample_id: format!("samp-{outcome_id}-attempt-{slot}"),
                schema_version: FEATURE_SCHEMA_VERSION,
                timestamp: BASE_TIMESTAMP + index as i64,
                streaming: false,
                dialect: "anthropic".to_string(),
                features: scripted_features(slot as u16, milli),
                targets: Targets {
                    success: won,
                    latency_ms: if won { Some(120.0) } else { None },
                    ttft_ms: if won { Some(40.0) } else { None },
                    cost: Some(0.01),
                    failure_class: if won {
                        None
                    } else {
                        Some("provider_unavailable".to_string())
                    },
                    fallback_count: 0,
                },
                provider_id: provider.to_string(),
                model_id: model.to_string(),
                origin: DataOrigin::Native,
                outcome_id: outcome_id.clone(),
                request_id: format!("r-{salt:016x}{index:08x}"),
                decision_id: Some(decision_id.clone()),
                response_id: Some(format!("resp-{salt:016x}{index:08x}")),
                final_status: status,
                success: won,
                identity: OutcomeIdentity {
                    planned: planned.clone(),
                    last_attempted: attempts.last().map(|attempt| {
                        CandidateIdentity::new(
                            attempt.candidate_model.clone(),
                            attempt.candidate_provider.clone(),
                        )
                    }),
                    served: served.clone(),
                },
                scope: SampleScope::Attempt {
                    index: slot,
                    attempt_id: attempts[slot].attempt_id.clone(),
                },
                attempt_id: Some(attempts[slot].attempt_id.clone()),
                rectified: false,
                attempts: attempts.clone(),
                usage: None,
                estimated_cost: None,
                actual_cost: None,
                terminal_error: terminal_facts(served.is_some()),
                feedback: None,
            });
        }

        // The request-scope row. `project_cohorts` must ignore it: a request is
        // not a candidate, and treating it as one would put a fourth entry in
        // an axis that only ever held three.
        let request_identity = served
            .clone()
            .or_else(|| planned.clone())
            .unwrap_or_else(|| CandidateIdentity::new("unknown", "unknown"));
        rows.push(OutcomeTrainingSample {
            sample_id: format!("samp-{outcome_id}-request"),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: BASE_TIMESTAMP + index as i64,
            streaming: false,
            dialect: "anthropic".to_string(),
            features: scripted_features(0, 500),
            targets: Targets {
                success: served.is_some(),
                latency_ms: if served.is_some() { Some(120.0) } else { None },
                ttft_ms: if served.is_some() { Some(40.0) } else { None },
                cost: Some(0.01),
                failure_class: if served.is_some() {
                    None
                } else {
                    Some("provider_unavailable".to_string())
                },
                fallback_count: 0,
            },
            provider_id: request_identity.provider().to_string(),
            model_id: request_identity.model().to_string(),
            origin: DataOrigin::Native,
            outcome_id: outcome_id.clone(),
            request_id: format!("r-{salt:016x}{index:08x}"),
            decision_id: Some(decision_id.clone()),
            response_id: None,
            final_status: status,
            success: served.is_some(),
            identity: OutcomeIdentity {
                planned: planned.clone(),
                last_attempted: attempts.last().map(|attempt| {
                    CandidateIdentity::new(
                        attempt.candidate_model.clone(),
                        attempt.candidate_provider.clone(),
                    )
                }),
                served: served.clone(),
            },
            scope: SampleScope::Request,
            attempt_id: None,
            rectified: false,
            attempts: attempts.clone(),
            usage: None,
            estimated_cost: None,
            actual_cost: None,
            terminal_error: terminal_facts(served.is_some()),
            feedback: None,
        });
    }
    rows
}

fn terminal_facts(success: bool) -> Option<FailureFacts> {
    if success {
        None
    } else {
        Some(FailureFacts {
            class: FailureClass::ProviderUnavailable,
            message: Some("scripted failure".to_string()),
            http_status: Some(503),
        })
    }
}

fn scripted_features(tag: u16, milli: u16) -> RoutingFeatures {
    let mut values = [0.0_f32; FEATURE_DIMENSION];
    for (position, value) in values.iter_mut().enumerate() {
        *value = if position == SCRIPT_INDEX {
            f32::from(tag) * 1000.0 + f32::from(milli)
        } else {
            // A plain, finite, present filler. The scripted head reads one
            // index; the calibrator reads no feature at all.
            0.5
        };
    }
    RoutingFeatures {
        values,
        schema_version: FEATURE_SCHEMA_VERSION,
    }
}

fn honest_model() -> ScriptedModel {
    ScriptedModel::new(&HONEST_MILLI)
}

fn mild_model() -> ScriptedModel {
    ScriptedModel::new(&MILD_MILLI)
}

/// The head used for the drift fixture: it reports whatever the snapshot
/// recorded, so the fit partition and the holdout really are two regimes.
///
/// A table-indexed head cannot do this job: it returns the same number for the
/// same candidate tag in both partitions, the shift cancels, and the drift gate
/// would correctly report no shift — for the wrong reason.
fn echo_model() -> FeatureEchoModel {
    FeatureEchoModel
}

fn mild_config() -> CalibrationConfig {
    CalibrationConfig {
        holdout: HoldoutConfig {
            holdout_cohorts: 60,
            min_fit_cohorts: 20,
            min_attributed_outcomes: 4,
        },
        ..CalibrationConfig::default()
    }
}

fn config() -> CalibrationConfig {
    CalibrationConfig {
        holdout: HoldoutConfig {
            holdout_cohorts: 40,
            min_fit_cohorts: 20,
            min_attributed_outcomes: 4,
        },
        ..CalibrationConfig::default()
    }
}

/// Emit every decision's vector at the calibrator's identity parameterization
/// and measure it.
///
/// The point of going through the identity rather than through a fitted
/// calibrator is that what is under test is the *measurement*. A fitted
/// calibrator would repair a broken head, and the test would then be about the
/// fit rather than about whether the gate can see a confidently-wrong model.
/// The projection is in arrival order, because that is the only order in which
/// a prefix and a suffix mean "earlier" and "later".
///
/// This was the third defect in one family. `project_cohorts` used to sort by
/// `CohortOrderKey`, which is a *content* key, and `run_calibration` then took
/// `cohorts.split_at(len - holdout)` as a frozen holdout. The reasoning behind the
/// sort was sound — cohorts that tie on the key are indistinguishable to every sum
/// the module computes — and it was applied in the wrong place, because a
/// partition is not a sum. Which cohorts land in the holdout is decided by
/// position, and position in a content-sorted vector is a function of tie-breaking
/// rather than of history.
///
/// `sort_by` is stable, so tied cohorts kept the order `group_attempt_rows`
/// produced, and the degenerate case therefore appeared only when the ties fell
/// differently. Measured on a window whose recorded decisions alternated two
/// served identities perfectly, the holdout held one identity in five runs out of
/// six and six in the other, and the gate correctly refused the degenerate minority.
///
/// Asserted on the fixture that has three candidates cycling, so a content-sorted
/// projection and an arrival-ordered one are trivially distinguishable.
#[test]
fn projection_is_in_arrival_order_not_content_order() {
    let specs = main_fixture(24);
    let snapshot = render(&specs, 7);
    let cohorts =
        project_cohorts(&snapshot, &honest_model(), DEFAULT_PROBABILITY_FLOOR).expect("projection");

    let served: Vec<String> = cohorts
        .iter()
        .map(|cohort| {
            cohort
                .served()
                .map(|identity| format!("{}/{}", identity.provider(), identity.model()))
                .unwrap_or_else(|| "<none>".to_string())
        })
        .collect();

    // Every fifth decision served nobody, so arrival order has a recognisable
    // rhythm: three served, one not, repeating.
    assert_eq!(served.len(), specs.len(), "every decision becomes a cohort");
    for (index, identity) in served.iter().enumerate() {
        let expected_served = index % 5 != 4;
        assert_eq!(
            identity != "<none>",
            expected_served,
            "decision {index} served {identity}, but the fixture says {}",
            if expected_served { "served" } else { "nobody" }
        );
    }

    // And the served identities cycle, which a content sort would flatten into
    // runs of one candidate.
    let distinct = served
        .iter()
        .filter(|identity| *identity != "<none>")
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        distinct.len() >= 3,
        "the fixture cycles three candidates; an arrival-ordered projection keeps \
         all three interleaved, a content-ordered one would not. Saw {distinct:?}"
    );
}

/// The frozen holdout is a later slice of history, so it carries the later
/// decisions' identities rather than whichever content cluster sorted last.
///
/// The complement of `projection_is_in_arrival_order_not_content_order`: that one
/// pins the projection, this one pins the partition taken from it. The split is
/// performed here rather than through `run_calibration` so the assertion is about
/// the rule — "the trailing decisions", by the same arithmetic the gate uses — and
/// not about whatever the outcome happens to expose.
#[test]
fn the_holdout_is_the_later_slice_rather_than_a_content_cluster() {
    let specs = main_fixture(400);
    let snapshot = render(&specs, 11);
    let holdout_size = config().holdout.holdout_cohorts;
    let cohorts =
        project_cohorts(&snapshot, &honest_model(), DEFAULT_PROBABILITY_FLOOR).expect("projection");
    assert!(
        cohorts.len() > holdout_size,
        "the fixture produced {} cohorts, which is not longer than the {holdout_size}-cohort \
         holdout, so the split proves nothing",
        cohorts.len()
    );

    let distinct_in_tail = |slice: &[zroutery_core::ml::DecisionCohort]| {
        slice
            .iter()
            .filter_map(|cohort| cohort.served())
            .map(|identity| {
                (
                    identity.provider().to_string(),
                    identity.model().to_string(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };

    // The trailing decisions cycle through three candidates, so the tail must too.
    // Under a content-ordered projection the tail was one candidate and the gate
    // refused with `SingleServedCandidate`.
    let tail = distinct_in_tail(&cohorts[cohorts.len() - holdout_size..]);
    assert!(
        tail.len() >= 2,
        "the trailing {holdout_size} decisions hold {} distinct served \
         identities; a later slice of a cycling history holds several, and one \
         means the tail was a content cluster rather than a slice of time. Saw {tail:?}",
        tail.len()
    );

    // And the head, for symmetry: a prefix of a cycling history is not one
    // candidate either. Both halves are checked because a projection that put the
    // whole body in one cluster would satisfy neither.
    let head = distinct_in_tail(&cohorts[..holdout_size]);
    assert!(
        head.len() >= 2,
        "the leading {holdout_size} decisions hold {} distinct served identities. \
         Saw {head:?}",
        head.len()
    );
}

fn uncalibrated_measure(snapshot: &[OutcomeTrainingSample], table: &[u16]) -> CalibrationMeasure {
    uncalibrated_measure_with(snapshot, &ScriptedModel::new(table))
}

fn uncalibrated_measure_with(
    snapshot: &[OutcomeTrainingSample],
    model: &dyn RoutingModel,
) -> CalibrationMeasure {
    let cohorts =
        project_cohorts(snapshot, model, DEFAULT_PROBABILITY_FLOOR).expect("projection succeeds");
    let identity = KWayCalibrator::uncalibrated();
    let emitted: Vec<EmittedDecision> = cohorts
        .iter()
        .map(|cohort| identity.distribution(cohort, DEFAULT_PROBABILITY_FLOOR))
        .collect::<Result<Vec<_>, _>>()
        .expect("emission succeeds");
    measure_emitted(&emitted, &ReliabilityConfig::default()).expect("measurement succeeds")
}

fn candidate_row<'a>(measure: &'a CalibrationMeasure, provider: &str) -> &'a CandidateCalibration {
    measure
        .per_candidate
        .iter()
        .find(|row| row.candidate.provider() == provider)
        .unwrap_or_else(|| panic!("no per-candidate row for {provider}"))
}

fn probe_context(timestamp: i64) -> CohortContext {
    CohortContext {
        timestamp,
        dialect: "anthropic".to_string(),
        streaming: false,
        final_status_rank: 0,
        failure_class_rank: None,
    }
}

// ---------------------------------------------------------------------------
// GATE 1 — K-way normalization
// ---------------------------------------------------------------------------

/// GATE 1: every emitted vector sums to one inside the 7E-2A tolerance, has
/// arity equal to its axis, and is accepted by the 7E-2A type.
///
/// The tolerance is asserted against the accepted constant rather than a copy of
/// it, so a change to 7E-2A cannot silently leave this test asserting the wrong
/// thing.
#[test]
fn every_emitted_vector_is_normalized_and_accepted_by_the_contract_type() {
    let snapshot = render(&main_fixture(200), 7);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");

    assert_eq!(outcome.holdout_distributions().len(), 40);
    for decision in outcome.holdout_distributions() {
        let distribution = decision.distribution();
        let mass = distribution.total_mass();
        assert!(
            (mass - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE,
            "emitted mass {mass} is outside the accepted tolerance {DISTRIBUTION_NORMALIZATION_TOLERANCE}"
        );
        assert_eq!(distribution.k(), distribution.outcomes().len());
        assert_eq!(distribution.k(), distribution.probabilities().len());
        assert_eq!(distribution.k(), 3, "the fixture's axis is three wide");
        for probability in distribution.probabilities() {
            assert!(probability.is_finite(), "a probability was not finite");
            assert!(*probability >= 0.0, "a probability was negative");
            assert!(*probability <= 1.0, "a probability exceeded one");
        }
        // The subject is one of the outcomes, which the type already enforces;
        // restated so the test says what it relies on.
        assert!(distribution.outcomes().contains(distribution.subject()));
        for outcome_axis in distribution.outcomes() {
            assert!(distribution.probability_of(outcome_axis).is_some());
        }
    }
}

/// GATE 1, second half: the 7E-2A type is the gate, and it really does refuse.
///
/// Every way this node could emit an invalid vector is fed to the accepted
/// constructor and must come back as the matching typed refusal. If the type
/// stopped refusing any of these, this node's "the type validates my output"
/// claim would be vacuous.
#[test]
fn the_contract_type_refuses_every_vector_this_node_could_have_emitted() {
    let alpha = identity(ALPHA);
    let bravo = identity(BRAVO);
    let outcomes = vec![alpha.clone(), bravo.clone()];

    let unnormalized =
        DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![0.4, 0.4]);
    assert!(matches!(
        unnormalized,
        Err(DecisionContractError::DistributionNotNormalized { .. })
    ));

    let arity = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![0.5, 0.4, 0.1]);
    assert!(matches!(
        arity,
        Err(DecisionContractError::DistributionArity { .. })
    ));

    let negative = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![1.2, -0.2]);
    assert!(matches!(
        negative,
        Err(DecisionContractError::NegativeProbability { .. })
    ));

    let infinite =
        DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![f64::INFINITY, 0.0]);
    assert!(matches!(
        infinite,
        Err(DecisionContractError::NonFiniteProbability { .. })
    ));

    let duplicate = DecisionDistribution::try_new(
        alpha.clone(),
        vec![alpha.clone(), alpha.clone()],
        vec![0.5, 0.5],
    );
    assert!(matches!(
        duplicate,
        Err(DecisionContractError::DuplicateDistributionOutcome(_))
    ));

    let blank = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![0.5, 0.5]);
    assert!(blank.is_ok(), "a well-formed vector is accepted");

    let foreign_subject =
        DecisionDistribution::try_new(identity(CHARLIE), outcomes.clone(), vec![0.5, 0.5]);
    assert!(matches!(
        foreign_subject,
        Err(DecisionContractError::DistributionSubjectNotAnOutcome(_))
    ));

    let empty = DecisionDistribution::try_new(alpha.clone(), Vec::new(), Vec::new());
    assert!(matches!(
        empty,
        Err(DecisionContractError::EmptyDistribution)
    ));

    // Zero mass is legal, which is what makes an unranked candidate
    // representable at all rather than a special case.
    let zero_mass = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![1.0, 0.0]);
    assert!(
        zero_mass.is_ok(),
        "an exact zero must be a legal probability, not a refusal"
    );
}

/// GATE 1, third part: an unranked candidate takes exactly `0.0` and does not
/// move the ranked ones.
///
/// The assertion that matters is the second group. "The unranked candidate has
/// no mass" is easy; "the ranked candidates kept precisely the vector they would
/// have had without it" is the property that makes an unranked candidate
/// representable *without distorting the rest*, so the same decision is emitted
/// twice, with and without the unranked entry.
#[test]
fn an_unranked_candidate_takes_exactly_zero_without_moving_the_ranked_ones() {
    let ranked_only = DecisionCohort::try_new(
        probe_context(10),
        Some(&identity(ALPHA)),
        vec![
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.9,
            },
            CandidateInput::Ranked {
                candidate: identity(BRAVO),
                raw_success_probability: 0.55,
            },
        ],
        Some(identity(BRAVO)),
    )
    .expect("a two-candidate cohort is valid");

    let with_unranked = DecisionCohort::try_new(
        probe_context(10),
        Some(&identity(ALPHA)),
        vec![
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.9,
            },
            CandidateInput::Unranked {
                candidate: identity(CHARLIE),
                reason: UnrankedReason::NotEligible,
            },
            CandidateInput::Ranked {
                candidate: identity(BRAVO),
                raw_success_probability: 0.55,
            },
        ],
        Some(identity(BRAVO)),
    )
    .expect("an axis may carry an unranked candidate");

    assert_eq!(ranked_only.arity(), 2);
    assert_eq!(with_unranked.arity(), 3);
    assert_eq!(with_unranked.ranked_count(), 2);
    assert_eq!(
        with_unranked
            .candidates()
            .iter()
            .filter(|input| !input.is_ranked())
            .count(),
        1
    );

    let identity_calibrator = KWayCalibrator::uncalibrated();
    let bare = identity_calibrator
        .distribution(&ranked_only, DEFAULT_PROBABILITY_FLOOR)
        .expect("emission succeeds");
    let padded = identity_calibrator
        .distribution(&with_unranked, DEFAULT_PROBABILITY_FLOOR)
        .expect("emission succeeds");

    let bare_vector = bare.distribution().probabilities();
    let padded_vector = padded.distribution().probabilities();
    assert_eq!(padded_vector.len(), 3);
    assert_eq!(
        padded_vector[1], 0.0,
        "the unranked candidate must hold exactly 0.0, not a small mass"
    );
    assert!(
        padded.unranked().contains(&identity(CHARLIE)),
        "the unranked candidate must be reported as unranked, not as a low-probability candidate"
    );
    assert_eq!(
        padded_vector[0], bare_vector[0],
        "the first ranked candidate must keep its mass exactly"
    );
    assert_eq!(
        padded_vector[2], bare_vector[1],
        "the second ranked candidate must keep its mass exactly"
    );
    assert!(
        (padded.distribution().total_mass() - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE
    );

    // Every unranked reason is representable and typed.
    for reason in [
        UnrankedReason::NotEligible,
        UnrankedReason::InsufficientEvidence,
        UnrankedReason::ModelCold,
    ] {
        assert_eq!(reason.label(), reason.to_string());
        let cohort = DecisionCohort::try_new(
            probe_context(11),
            Some(&identity(ALPHA)),
            vec![
                CandidateInput::Ranked {
                    candidate: identity(ALPHA),
                    raw_success_probability: 0.8,
                },
                CandidateInput::Unranked {
                    candidate: identity(BRAVO),
                    reason,
                },
            ],
            Some(identity(ALPHA)),
        )
        .expect("an axis may carry an unranked candidate");
        let emitted = identity_calibrator
            .distribution(&cohort, DEFAULT_PROBABILITY_FLOOR)
            .expect("emission succeeds");
        assert_eq!(emitted.distribution().probabilities()[1], 0.0);
    }
}

// ---------------------------------------------------------------------------
// GATE 2 — calibration metrics
// ---------------------------------------------------------------------------

/// GATE 2: the headline measurement is over the FINAL emitted vector, and it is
/// close to a hand-computed truth.
///
/// The fixture is built so the truth can be written down. The winner cycle makes
/// the holdout's 40 decisions carry exactly 8 alpha wins, 16 bravo wins and 8
/// charlie wins, so the empirical per-candidate truth is `0.20 / 0.40 / 0.20`.
/// The emitted vector is asserted against the fitted numbers, and the verdict
/// against the ceilings, with no threshold chosen after seeing the result.
#[test]
fn the_final_vector_is_measured_and_is_close_to_the_hand_computed_truth() {
    let snapshot = render(&main_fixture(200), 11);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    assert_eq!(report.cohorts_total, 200);
    assert_eq!(report.fit_cohorts, 160);
    assert_eq!(report.holdout_cohorts, 40);
    assert_eq!(report.holdout_attributed_cohorts, 32);
    assert_eq!(report.ranked_observations, 120);

    let rows = &report.final_vector.per_candidate;
    assert_eq!(rows.len(), 3, "one row per candidate, bin-free");
    for (provider, expected_served) in [(ALPHA.0, 8_usize), (BRAVO.0, 16), (CHARLIE.0, 8)] {
        let row = candidate_row(&report.final_vector, provider);
        assert_eq!(row.observations, 40, "{provider} is in every decision");
        assert_eq!(row.served, expected_served, "{provider} served count");
    }
    assert!((candidate_row(&report.final_vector, BRAVO.0).observed_frequency - 0.40).abs() < 1e-12);
    assert!((candidate_row(&report.final_vector, ALPHA.0).observed_frequency - 0.20).abs() < 1e-12);
    assert!(
        (candidate_row(&report.final_vector, CHARLIE.0).observed_frequency - 0.20).abs() < 1e-12
    );

    // The emitted vector itself, axis for axis.
    let emitted = outcome.holdout_distributions()[0]
        .distribution()
        .probabilities()
        .to_vec();
    assert_eq!(emitted.len(), 3);
    assert!(
        (emitted[0] - 0.2487).abs() < 0.01,
        "alpha mass drifted from the fitted 0.2487: {emitted:?}"
    );
    assert!(
        (emitted[1] - 0.5032).abs() < 0.01,
        "bravo mass drifted from the fitted 0.5032: {emitted:?}"
    );
    assert!(
        (emitted[2] - 0.2481).abs() < 0.01,
        "charlie mass drifted from the fitted 0.2481: {emitted:?}"
    );

    // The summary errors over the final vector.
    let ece = report.final_vector.expected_calibration_error();
    assert!(
        ece < 0.07,
        "final vector ece {ece} is worse than the fit achieves"
    );
    assert!(report.final_vector.maximum_calibration_error().is_some());
    assert!(report.final_vector.brier_score().is_some());
    assert!(report.final_vector.log_loss().is_some());
    assert!(report.final_vector.multiclass_log_loss.is_some());
    assert!((report.final_vector.observed_positive_rate() - 32.0 / 120.0).abs() < 1e-12);
    assert_eq!(report.verdict, CalibrationVerdict::Calibrated);
    assert!(report.is_calibrated());

    // The binned curve really is binned, and its counts add up.
    let curve = &report.final_vector.curve;
    assert_eq!(curve.bin_count, 10);
    assert_eq!(curve.bins.len(), 10);
    assert_eq!(
        curve.bins.iter().map(|bin| bin.count).sum::<usize>(),
        curve.observations
    );
    assert!(
        curve.populated_bins() >= 2,
        "a curve with one populated row is not a curve"
    );
    assert!((curve.bins[0].lower - 0.0).abs() < 1e-12);
    assert!((curve.bins[9].upper - 1.0).abs() < 1e-12);
    for bin in curve.bins.iter().filter(|bin| bin.count > 0) {
        assert!(bin.gap.is_finite());
        assert!((bin.gap - (bin.mean_predicted - bin.observed_frequency)).abs() < 1e-12);
        assert!(bin.observed_frequency >= 0.0 && bin.observed_frequency <= 1.0);
    }

    // Subordination, structurally: the marginal numbers live in a different
    // type, and the headline is no worse than the normalized independent route
    // on the binned measure.
    //
    // It is worth being precise about what separates them, because it is not
    // the binned number. On this fixture the two routes *tie* on expected
    // calibration error (0.0667 each): the normalized route errs by about the
    // same amount on every candidate, and averaging that over three candidates
    // in three bins gives the same figure the joint achieves. The bin-free
    // table is what separates them — the joint's worst named candidate is
    // 0.1032 out while the normalized route's is 0.1673 out. That is the whole
    // argument for reporting a per-candidate table beside the curve, and it is
    // asserted as such rather than assumed.
    assert!(
        ece <= report.marginal_normalized.expected_calibration_error() + 1e-9,
        "the fitted joint must be no worse on the binned measure: {ece} vs {}",
        report.marginal_normalized.expected_calibration_error()
    );
    assert!(
        report
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < report
                .marginal_normalized
                .maximum_candidate_calibration_error()
                .unwrap_or(0.0),
        "and decisively better on the worst named candidate: {} vs {}",
        report
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(f64::NAN),
        report
            .marginal_normalized
            .maximum_candidate_calibration_error()
            .unwrap_or(f64::NAN)
    );
    assert_eq!(
        report.final_vector.observations, report.marginal_normalized.observations,
        "both routes must be measured over the same observations"
    );
}

/// GATE 2, second part: the measurement has teeth. Two deliberately
/// confidently-wrong heads, both rejected, through the same validated-type path
/// as the headline.
#[test]
fn a_confidently_wrong_head_is_detected_by_the_measurement() {
    let snapshot = render(&main_fixture(200), 13);
    let ceilings = AcceptanceTolerances::from(&ReliabilityConfig::default());

    // A head certain that alpha serves, when alpha serves a fifth of the time.
    let confident = uncalibrated_measure(&snapshot, &CONFIDENT_WRONG_MILLI);
    assert!(
        !confident.passes(&ceilings),
        "a confident head must be refused"
    );
    assert_eq!(
        CalibrationVerdict::from_measurements(&confident, &ceilings),
        CalibrationVerdict::Miscalibrated
    );
    assert!(
        confident.maximum_calibration_error().unwrap_or(0.0) > 0.5,
        "a head 74 points wrong on its confident candidate must be caught: {:?}",
        confident.per_candidate
    );
    let alpha = candidate_row(&confident, ALPHA.0);
    assert!(alpha.mean_predicted > 0.9, "the head really was confident");
    assert!(
        (alpha.observed_frequency - 0.20).abs() < 1e-12,
        "and really was wrong about it"
    );

    // A head whose ranking is the exact reverse of the truth, and confident.
    let reversed = uncalibrated_measure(&snapshot, &REVERSED_MILLI);
    assert!(
        !reversed.passes(&ceilings),
        "a reversed ranking must be refused"
    );
    assert!(
        reversed.maximum_calibration_error().unwrap_or(0.0) > 0.3,
        "a reversed ranking must be caught: {:?}",
        reversed.per_candidate
    );
    // The two broken heads fail in different ways, and both are caught. The
    // confident head is wrong about one candidate and right about the other
    // two; the reversed head is wrong about all three at once. Which of the two
    // costs more depends on the axis, so no ordering between them is claimed
    // here — only that both are rejected and that both cost more than the
    // merely-miscalibrated honest head.
    //
    // The honest head is itself **miscalibrated**, and that is the premise of
    // the whole node rather than a flaw in the fixture: at the identity
    // parameterization it is out by 0.29 on its most over-claimed candidate,
    // which is exactly the error the fitted joint exists to remove. So the gate
    // is not "reject everything" — the same rule accepts the fitted joint over
    // this very snapshot.
    let honest = uncalibrated_measure(&snapshot, &HONEST_MILLI);
    assert_eq!(honest.per_candidate.len(), 3);
    assert!(
        !honest.passes(&ceilings),
        "the unfitted honest head is not calibrated either: {:?}",
        honest.per_candidate
    );
    for (label, measure) in [("confident", &confident), ("reversed", &reversed)] {
        assert!(
            measure.multiclass_log_loss.unwrap_or(0.0)
                > honest.multiclass_log_loss.unwrap_or(f64::INFINITY),
            "the {label} head must cost more multiclass log loss than the merely-miscalibrated honest one"
        );
        assert!(
            measure.brier_score().unwrap_or(1.0) > honest.brier_score().unwrap_or(0.0),
            "the {label} head must have a worse Brier score than the honest one"
        );
    }
    let fitted = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    assert!(fitted.report().final_vector.passes(&ceilings));
    assert!(
        fitted
            .report()
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < honest.maximum_candidate_calibration_error().unwrap_or(0.0),
        "and the fit is what closed the honest head's per-candidate gap"
    );

    // A head that is maximally *uninformative* is the known blind spot of a
    // binned gap measure, and it is pinned here rather than left unexamined: a
    // constant head produces a uniform vector, and against a non-uniform truth
    // its per-candidate errors partly cancel. This is a real limitation of
    // expected calibration error, it is why the per-candidate table exists, and
    // it is why this node does not rest the claim on the binned number alone.
    let constant = uncalibrated_measure_with(&snapshot, &ConstantModel(0.98));
    let masses: Vec<f64> = constant
        .per_candidate
        .iter()
        .map(|row| row.mean_predicted)
        .collect();
    assert!(
        masses.iter().all(|mass| (mass - 1.0 / 3.0).abs() < 1e-9),
        "a constant head must produce a uniform vector on a symmetric axis: {masses:?}"
    );
    assert!(
        constant
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            > 0.05,
        "the per-candidate table still sees the uniform head's error: {:?}",
        constant.per_candidate
    );
    assert!(
        constant.expected_calibration_error() < 0.07,
        "and the binned measure largely does not, which is the documented blind spot"
    );
}

/// GATE 2, third part: a binned curve can hide a per-candidate error, and this
/// node does not rest the claim on the bins alone.
///
/// On the main fixture the independent route lands all three of its calibrated
/// masses inside the `[0.2, 0.3)` bin, where their errors very nearly cancel.
/// Its binned expected calibration error is therefore ~0, while its worst
/// *named candidate* is more than twelve points out. The bin-free table is what
/// makes that visible, and the acceptance rule checks it, so the cancellation
/// cannot buy a calibration claim.
#[test]
fn the_bin_free_candidate_table_catches_what_the_bins_average_away() {
    let snapshot = render(&main_fixture(200), 17);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let marginal = &outcome.report().marginal_calibrated;

    let populated: Vec<&ReliabilityBin> = marginal
        .curve
        .bins
        .iter()
        .filter(|bin| bin.count > 0)
        .collect();
    assert_eq!(
        populated.len(),
        1,
        "the fixture is chosen so the independent route's three masses collide in one bin"
    );
    assert_eq!(populated[0].count, 120);
    assert!(
        marginal.expected_calibration_error() < 1e-9,
        "the binned error cancels: {}",
        marginal.expected_calibration_error()
    );

    // The bin-free table does not cancel.
    let worst = marginal
        .maximum_candidate_calibration_error()
        .expect("the table is populated");
    assert!(
        worst > 0.12,
        "the worst named candidate is more than twelve points out and the bins hid it: {worst}"
    );
    let bravo = marginal
        .per_candidate
        .iter()
        .find(|row| row.candidate.provider() == BRAVO.0)
        .expect("bravo is on the axis");
    assert!(
        bravo.gap < -0.10,
        "bravo is under-claimed by the shared map"
    );

    // And a ceiling on the bins alone would have accepted it. The
    // per-candidate ceiling is what refuses, under one shared rule.
    let bins_only = AcceptanceTolerances {
        max_expected_calibration_error: 0.10,
        max_calibration_error: 0.25,
        max_candidate_calibration_error: 1.0,
    };
    let strict = AcceptanceTolerances {
        max_candidate_calibration_error: 0.05,
        ..bins_only
    };
    assert!(
        marginal.passes(&bins_only),
        "with no per-candidate ceiling the cancellation would buy a claim"
    );
    assert!(
        !marginal.passes(&strict),
        "the per-candidate ceiling is what refuses it"
    );

    // The normalized route is refused under the same strict ceiling, so the
    // per-candidate ceiling is not merely punishing a route that has no
    // candidate structure at all.
    let normalized = &outcome.report().marginal_normalized;
    assert!(!normalized.passes(&strict));

    // The fitted joint accepts the *default* ceilings, which is the actual
    // claim. It does not clear the 0.05 ceiling used above, and this test says
    // so rather than quietly picking a ceiling that flatters it.
    let defaults = AcceptanceTolerances::from(&ReliabilityConfig::default());
    assert!(
        outcome.report().final_vector.passes(&defaults),
        "the fitted joint clears the default ceilings"
    );
    assert!(
        !outcome.report().final_vector.passes(&strict),
        "the fitted joint is not inside five points on every candidate, and that is reported"
    );
    assert!(
        outcome
            .report()
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < normalized
                .maximum_candidate_calibration_error()
                .unwrap_or(0.0),
        "but it is still the closest of the three"
    );
}

/// GATE 2, fourth part: the verdict is recomputed, never read.
///
/// The stored verdict and the recomputed verdict must agree, and tightening a
/// ceiling below the measured value flips the recomputed one — which is only
/// possible if the rule really reads the stored measurement.
#[test]
fn the_verdict_is_recomputed_from_the_stored_measurement() {
    let snapshot = render(&main_fixture(200), 19);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    assert_eq!(
        report.verdict,
        report.recomputed_verdict(),
        "the recorded verdict and the recomputed verdict must agree"
    );
    assert_eq!(
        report.is_calibrated(),
        report.recomputed_verdict().is_calibrated()
    );
    assert_eq!(
        CalibrationVerdict::from_measurements(&report.final_vector, &report.tolerances),
        report.verdict,
        "the verdict is a pure function of the final measurement and the ceilings"
    );

    let strict = AcceptanceTolerances {
        max_candidate_calibration_error: 0.001,
        ..report.tolerances
    };
    assert_eq!(
        CalibrationVerdict::from_measurements(&report.final_vector, &strict),
        CalibrationVerdict::Miscalibrated,
        "tightening a ceiling below the measured value must flip the verdict"
    );
    assert_eq!(report.verdict, CalibrationVerdict::Calibrated);
    assert!(report.headline().contains("final vector"));
}

/// GATE 2, fifth part: the honest answer to the question this node exists to
/// answer, measured.
///
/// The joint route and the independent route are both fitted on the same
/// partition and both measured on the same holdout with the same rule. The
/// damage normalization does to the independent route is a number, and so is the
/// joint route's distance from the same starting point.
#[test]
fn the_joint_route_beats_the_normalized_independent_route_and_the_damage_is_a_number() {
    let snapshot = render(&main_fixture(200), 67);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    // The head's three raw numbers sum to 1.85, so it is not a distribution and
    // has to be either calibrated or normalized before it can be compared to one.
    assert!(
        (HONEST_MILLI.iter().map(|m| f64::from(*m)).sum::<f64>() / 1000.0 - 1.85).abs() < 1e-12
    );

    // Normalizing the independently-calibrated vector degraded it. This is the
    // measured answer to "does normalizing make it worse", and the sign is the
    // claim.
    assert!(
        report.normalization_damage.degraded_calibration(),
        "normalization must be reported as having degraded calibration: {:?}",
        report.normalization_damage
    );
    assert!(
        report.normalization_damage.delta > 0.05,
        "{:?}",
        report.normalization_damage
    );
    assert!(
        report
            .normalization_damage
            .expected_calibration_error_before
            < 0.01,
        "the independent route is well calibrated as three separate probabilities"
    );
    // And the bin-free view of the same question agrees.
    assert!(
        report.normalization_damage.candidate_delta().unwrap_or(0.0) > 0.0,
        "the worst named candidate got worse too: {:?}",
        report.normalization_damage
    );
    assert_eq!(
        report.normalization_damage.delta,
        report.normalization_damage.expected_calibration_error_after
            - report
                .normalization_damage
                .expected_calibration_error_before
    );

    // The joint route, fitted on the same partition and measured on the same
    // holdout under the same rule, beats the normalized independent route on
    // the bin-free view and ties it on the binned one.
    assert!(
        report.final_vector.expected_calibration_error()
            <= report.marginal_normalized.expected_calibration_error() + 1e-9,
        "the fitted joint must be no worse on the binned measure"
    );
    let joint_worst = report
        .final_vector
        .maximum_candidate_calibration_error()
        .expect("the final vector has candidates");
    let normalized_worst = report
        .marginal_normalized
        .maximum_candidate_calibration_error()
        .expect("the normalized route has candidates");
    let calibrated_worst = report
        .marginal_calibrated
        .maximum_candidate_calibration_error()
        .expect("the calibrated route has candidates");
    assert!(
        joint_worst < normalized_worst,
        "the joint must be closer on the worst named candidate: {joint_worst} vs {normalized_worst}"
    );
    assert!(
        joint_worst < calibrated_worst,
        "and closer than the un-normalized independent route: {joint_worst} vs {calibrated_worst}"
    );
    // Every named candidate the joint emits is within a tenth of the truth,
    // while the normalized route is out by more than a sixth on one of them.
    assert!(joint_worst < 0.11, "joint worst {joint_worst}");
    assert!(
        normalized_worst > 0.16,
        "normalized worst {normalized_worst}"
    );

    // And it beats the same parameterization left unfitted, so the fit earned
    // its place.
    let (uncalibrated_ece, fitted_ece) = report
        .improvement_over_uncalibrated()
        .expect("the report carries the comparison");
    assert!(
        fitted_ece < uncalibrated_ece,
        "the fit must improve on the identity parameterization: {uncalibrated_ece} -> {fitted_ece}"
    );
    let (uncalibrated_worst, fitted_worst) = (
        report
            .uncalibrated_joint
            .maximum_candidate_calibration_error()
            .expect("measured"),
        joint_worst,
    );
    assert!(
        fitted_worst < uncalibrated_worst,
        "on the worst named candidate too: {uncalibrated_worst} -> {fitted_worst}"
    );

    // The mild fixture is the control: a head whose ranking already agrees with
    // the outcome, so the independent route is not handed an impossible task.
    // The joint route must still be calibrated there, which is what shows the
    // result above is about the *joint* parameterization and not about a
    // fixture built to be unwinnable.
    let mild = run_calibration(
        &render(&mild_fixture(240), 71),
        &mild_model(),
        &mild_config(),
    )
    .expect("the mild fixture calibrates");
    let mild_report = mild.report();
    assert_eq!(mild_report.verdict, CalibrationVerdict::Calibrated);
    assert!(
        mild_report.final_vector.expected_calibration_error() < 0.07,
        "mild final ece {}",
        mild_report.final_vector.expected_calibration_error()
    );
    assert!(
        mild_report
            .marginal_calibrated
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < 0.05,
        "with a correctly ranked head the independent route really is close, which is the point"
    );
    // The independent route's intercepts moved the *right* way here, unlike on
    // the reversed fixture: the slope is positive, so the map preserved the
    // head's own ranking.
    assert!(
        mild.marginal_calibrator().slope() > 0.0,
        "a correctly ranked head needs a positively sloped map"
    );
    assert!(
        outcome.marginal_calibrator().slope() < 0.0,
        "a reversed head needs a negatively sloped map, which is a sign the map is fighting its input"
    );
}

// ---------------------------------------------------------------------------
// GATE 3 — determinism, including UUID invariance
// ---------------------------------------------------------------------------

/// GATE 3, first half: the same snapshot and configuration give byte-identical
/// calibrator and report.
///
/// Serialized bytes rather than `PartialEq`, because `PredictionMetrics` has
/// none and the accepted warmup node set the same precedent.
#[test]
fn one_snapshot_and_configuration_give_byte_identical_output() {
    let snapshot = render(&main_fixture(200), 23);
    let first = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let second = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");

    assert_eq!(
        serde_json::to_string(first.calibrator()).unwrap(),
        serde_json::to_string(second.calibrator()).unwrap(),
        "the fitted calibrator must be byte-identical across runs"
    );
    assert_eq!(
        serde_json::to_string(first.marginal_calibrator()).unwrap(),
        serde_json::to_string(second.marginal_calibrator()).unwrap()
    );
    assert_eq!(
        serde_json::to_string(first.report()).unwrap(),
        serde_json::to_string(second.report()).unwrap(),
        "the report must be byte-identical across runs: it holds no timestamp"
    );
    assert_eq!(
        serde_json::to_string(&first.holdout_records()).unwrap(),
        serde_json::to_string(&second.holdout_records()).unwrap()
    );
}

/// GATE 3, second half — **the test that actually bites**.
///
/// `outcome_id` is `req_{uuid}` (`stats.rs`), `decision_id` is `dec-{uuid}`
/// (`router.rs`), and `deterministic_sample_id` is
/// `format!("samp-{}-{suffix}", outcome.outcome_id)` (`ml/dataset.rs`). So all
/// three identifiers are UUID-derived, and `Decision.timestamp` is
/// second-resolution, so a timestamp comparison ties constantly and the UUID
/// becomes the tiebreaker.
///
/// This test renders the *same logical data* twice under deliberately different
/// UUIDs, reverses the second rendering's row order, and asserts that the
/// partition, the fitted calibrator, the emitted distributions, every
/// measurement, the damage, the drift and the verdict are identical. An
/// in-process byte comparison cannot catch a UUID leak, because the UUIDs are
/// fixed within a process. This can.
#[test]
fn uuid_invariance_the_partition_and_every_number_survive_a_uuid_change() {
    let specs = main_fixture(200);
    let first = render(&specs, 0x1111_2222_3333_4444);
    let second = render(&specs, 0xAAAA_BBBB_CCCC_DDDD);

    // No row of the first rendering may share an identifier with the second, and
    // every identifier of the second must really carry its own salt.
    let first_ids: Vec<&str> = first.iter().map(|row| row.sample_id.as_str()).collect();
    for row in &second {
        assert!(
            !first_ids.contains(&row.sample_id.as_str()),
            "the two renderings must not share any sample id"
        );
        assert!(row.sample_id.starts_with("samp-req_aaaa"));
        assert!(row.outcome_id.starts_with("req_aaaa"));
        assert!(row
            .decision_id
            .as_deref()
            .is_some_and(|id| id.starts_with("dec-aaaa")));
    }

    // The two renderings carry the same history in the same order, differing only
    // in their identifiers. A row reversal used to be applied here as well, on the
    // reasoning that "row arrival order is not the content order either".
    //
    // It was removed because it conflated two independent claims, and the second
    // one is false. `render` stamps `BASE_TIMESTAMP + index`, so reversing the
    // rows reverses the *history*: it is not the same dataset in a different
    // order, it is the same measurements running backwards in time. A frozen
    // holdout is by definition a later slice, so a time-reversed dataset must
    // produce a different one.
    //
    // What is asserted now is the property this test is named for. Row-order
    // independence is asserted where it is true — within a partition — by
    // `projection_is_in_arrival_order_not_content_order`.

    let one = run_calibration(&first, &honest_model(), &config()).expect("calibration runs");
    let two = run_calibration(&second, &honest_model(), &config()).expect("calibration runs");

    // The partition.
    assert_eq!(one.report().fit_cohorts, two.report().fit_cohorts);
    assert_eq!(one.report().holdout_cohorts, two.report().holdout_cohorts);
    assert_eq!(
        one.report().fit_attributed_cohorts,
        two.report().fit_attributed_cohorts
    );
    assert_eq!(
        one.report().holdout_attributed_cohorts,
        two.report().holdout_attributed_cohorts
    );
    assert_eq!(
        one.calibrator().fit_cohorts(),
        two.calibrator().fit_cohorts()
    );

    // The calibrator, byte for byte.
    assert_eq!(
        serde_json::to_string(one.calibrator()).unwrap(),
        serde_json::to_string(two.calibrator()).unwrap(),
        "a UUID leaked into the fit"
    );
    assert_eq!(
        serde_json::to_string(one.marginal_calibrator()).unwrap(),
        serde_json::to_string(two.marginal_calibrator()).unwrap()
    );

    // The emitted distributions, byte for byte, fingerprints included — the
    // fingerprints are computed from content, so a UUID in one would show here.
    assert_eq!(
        serde_json::to_string(&one.holdout_records()).unwrap(),
        serde_json::to_string(&two.holdout_records()).unwrap(),
        "a UUID leaked into the emitted vectors or their fingerprints"
    );

    // Every number in the report.
    assert_eq!(
        serde_json::to_string(one.report()).unwrap(),
        serde_json::to_string(two.report()).unwrap(),
        "a UUID leaked into the report"
    );
    assert_eq!(one.report().verdict, two.report().verdict);
    assert_eq!(one.is_calibrated(), two.is_calibrated());
}

// ---------------------------------------------------------------------------
// GATE 4 -- holdout and drift
// ---------------------------------------------------------------------------

/// GATE 4, first part: the calibrator is fitted on one partition and measured
/// on a disjoint one, and it is actually fitted.
#[test]
fn the_holdout_is_disjoint_from_the_partition_the_calibrator_was_fitted_on() {
    let snapshot = render(&main_fixture(200), 31);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    assert_eq!(
        report.fit_cohorts + report.holdout_cohorts,
        report.cohorts_total
    );
    assert_eq!(
        outcome.calibrator().fit_cohorts(),
        report.fit_cohorts,
        "the calibrator and the report must agree on the fit size"
    );
    assert_eq!(
        outcome.calibrator().fit_attributed_cohorts(),
        report.fit_attributed_cohorts
    );
    assert!(report.fit_cohorts > report.holdout_cohorts);
    assert!(outcome.calibrator().is_fitted());
    assert!(
        outcome.calibrator().final_log_loss() < outcome.calibrator().initial_log_loss(),
        "the fit must improve on the identity parameterization in-sample too"
    );
    assert!(outcome.calibrator().final_log_loss().is_finite());
    assert!(
        outcome.marginal_calibrator().final_log_loss()
            < outcome.marginal_calibrator().initial_log_loss()
    );

    // The intercepts are sum-to-zero, which is what makes the temperature and
    // the intercepts separately identifiable.
    let intercept_sum: f64 = outcome
        .calibrator()
        .intercepts()
        .iter()
        .map(|record| record.intercept)
        .sum();
    assert!(
        intercept_sum.abs() < 1e-9,
        "the intercepts must be projected to sum to zero, got {intercept_sum}"
    );
    for record in outcome.calibrator().intercepts() {
        assert_eq!(
            record.fit_cohorts, 160,
            "every candidate is in every decision"
        );
        assert!(
            record.fit_serving_cohorts > 0,
            "every candidate serves sometimes"
        );
    }
    assert!(outcome.calibrator().temperature() > 0.0);

    // The holdout's fingerprints are content-derived and appear once per
    // decision.
    let fingerprints: Vec<&str> = outcome
        .holdout_distributions()
        .iter()
        .map(EmittedDecision::cohort_fingerprint)
        .collect();
    assert_eq!(fingerprints.len(), report.holdout_cohorts);
    assert!(fingerprints
        .iter()
        .all(|fingerprint| fingerprint.len() == 16));
}

/// Two candidates the head reports identically, decided 60/40.
///
/// The only signal is the empirical winner frequency, so a fit that solves the
/// reported mean log loss must reproduce it and a fit whose step size scales
/// with the row count will not.
fn contested_cohorts(count: usize) -> Vec<DecisionCohort> {
    (0..count)
        .map(|index| {
            let alpha_serves = index % 10 < 6;
            DecisionCohort::try_new(
                probe_context(BASE_TIMESTAMP + index as i64),
                None,
                vec![
                    CandidateInput::Ranked {
                        candidate: identity(ALPHA),
                        raw_success_probability: 0.5,
                    },
                    CandidateInput::Ranked {
                        candidate: identity(BRAVO),
                        raw_success_probability: 0.5,
                    },
                ],
                Some(identity(if alpha_serves { ALPHA } else { BRAVO })),
            )
            .expect("a two-candidate contested cohort")
        })
        .collect()
}

/// GATE 4, third part: the joint fit solves the loss the report publishes.
///
/// The reported objective is the *mean* log loss over the attributed cohorts.
/// Accumulating an unnormalised gradient made the fitted map a function of the
/// snapshot's row count: the same 60/40 distribution fitted on ten decisions
/// learned roughly the truth, while the same distribution replicated to ten
/// thousand decisions learned its reverse and reported a worse mean loss. A
/// fit of a mean loss must be invariant under replication, because replicating
/// a distribution does not add information.
#[test]
fn replicating_the_same_distribution_does_not_move_the_joint_fit() {
    let base = contested_cohorts(10);
    let mut replicated = Vec::with_capacity(base.len() * 1_000);
    for _ in 0..1_000 {
        replicated.extend(base.iter().cloned());
    }

    let config = CalibrationConfig::default();
    let small = KWayCalibrator::fit(&base, &config, DEFAULT_PROBABILITY_FLOOR).expect("fit");
    let large = KWayCalibrator::fit(&replicated, &config, DEFAULT_PROBABILITY_FLOOR)
        .expect("the replicated fit");

    assert_eq!(small.intercepts().len(), large.intercepts().len());
    for (small_record, large_record) in small.intercepts().iter().zip(large.intercepts()) {
        assert_eq!(small_record.candidate, large_record.candidate);
        assert!(
            (small_record.intercept - large_record.intercept).abs() < 1e-9,
            "{} moved from {} to {} when the same distribution was replicated",
            small_record.candidate.provider(),
            small_record.intercept,
            large_record.intercept
        );
    }
    assert!(
        (small.temperature() - large.temperature()).abs() < 1e-9,
        "temperature moved from {} to {}",
        small.temperature(),
        large.temperature()
    );

    // And the fit still improves on the identity parameterization at both
    // sizes: solving the reported loss must not trade one degeneracy for
    // another.
    for calibrator in [&small, &large] {
        assert!(
            calibrator.final_log_loss() < calibrator.initial_log_loss(),
            "the fit must beat identity: {} vs {}",
            calibrator.final_log_loss(),
            calibrator.initial_log_loss()
        );
        assert!(calibrator.final_log_loss().is_finite());
    }

    // The fitted mass tracks the 60/40 the data actually shows rather than
    // reversing it. `alpha` is the always-present first candidate.
    let alpha = small
        .intercepts()
        .iter()
        .find(|record| record.candidate == identity(ALPHA))
        .expect("alpha is in the vocabulary");
    let bravo = small
        .intercepts()
        .iter()
        .find(|record| record.candidate == identity(BRAVO))
        .expect("bravo is in the vocabulary");
    assert!(
        alpha.intercept > bravo.intercept,
        "the more frequent winner must not be pushed below the less frequent one: \
         {} vs {}",
        alpha.intercept,
        bravo.intercept
    );
}

/// A legitimate same-candidate retry stays one candidate on the axis.
///
/// The product retries the same provider/model after a rectifier repairs a
/// request and records each attempt separately. Projection used to push one
/// candidate per attempt and then reject the resulting axis as a duplicate, so
/// a single repaired request failed the whole batch. The candidate axis is now
/// the set of unique identities and a retried candidate's evidence is its
/// terminal attempt.
#[test]
fn a_same_candidate_rectifier_retry_is_one_candidate() {
    let model = "model-alpha";
    let provider = "p-alpha";

    let outcome = Outcome::builder("req-rectified")
        .decision_id("dec-rectified")
        .dialect("anthropic")
        .timestamp(BASE_TIMESTAMP)
        .single_candidate(model, provider)
        .attempt(Attempt {
            attempt_id: "att-rectified-0".to_string(),
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            started_at: BASE_TIMESTAMP,
            completed_at: BASE_TIMESTAMP + 1,
            latency_ms: 30.0,
            ttft_ms: None,
            success: false,
            failure_class: Some(FailureClass::Transport),
            failure_message: Some("first attempt failed".to_string()),
            http_status: Some(503),
            rectified: false,
            cost: None,
        })
        .attempt(Attempt {
            attempt_id: "att-rectified-1".to_string(),
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            started_at: BASE_TIMESTAMP + 1,
            completed_at: BASE_TIMESTAMP + 2,
            latency_ms: 140.0,
            ttft_ms: Some(50.0),
            success: true,
            failure_class: None,
            failure_message: None,
            http_status: Some(200),
            rectified: true,
            cost: None,
        })
        .total_latency_ms(170.0)
        .cost(Some(0.01), Some(0.01))
        .build();

    outcome
        .validate()
        .expect("a failed-then-rectified outcome is valid");
    assert!(outcome.success, "the terminal attempt succeeded");
    assert_eq!(
        outcome.served_identity(),
        Some(identity((provider, model))),
        "the retried candidate is the served identity"
    );

    // Two attempts on one candidate, plus the request-scope row.
    let samples = try_samples_from_outcome(
        &outcome,
        &[scripted_features(0, 300), scripted_features(0, 700)],
        DataOrigin::Native,
    )
    .expect("the canonical projection accepts a retried candidate");
    assert_eq!(samples.len(), 3, "two attempts and the request row");
    assert_eq!(
        samples[0].attempts.len(),
        2,
        "both attempts stay in the retained evidence"
    );
    assert!(!samples[0].rectified, "the first attempt is not the retry");
    assert!(
        samples[1].rectified,
        "the second attempt is the rectifier retry"
    );

    let cohorts = project_cohorts(&samples, &echo_model(), DEFAULT_PROBABILITY_FLOOR)
        .expect("a repaired request must not fail the whole batch");
    assert_eq!(cohorts.len(), 1, "both attempts belong to one decision");
    let cohort = &cohorts[0];
    assert_eq!(cohort.arity(), 1, "the identity appears on the axis once");
    assert_eq!(
        cohort.candidates()[0].candidate(),
        &identity((provider, model))
    );
    assert_eq!(cohort.served(), Some(&identity((provider, model))));
    let raw = cohort.candidates()[0]
        .raw_success_probability()
        .expect("the retried candidate is ranked");
    assert!(
        (raw - 0.7).abs() < 1e-12,
        "the terminal attempt is the candidate's evidence, got {raw}"
    );
    assert!(
        cohort.is_attributed(),
        "the retried candidate is attributed"
    );
}

/// GATE 4, second part: a regime shift between the two partitions is REFUSED,
/// and the refusal carries the measurement.
///
/// The holdout's head output is moved to the other end of the unit interval,
/// which is what a real regime change looks like to a probability head.
#[test]
fn a_shifted_holdout_is_refused_with_the_drift_measurement_attached() {
    let specs = shifted_fixture(&main_fixture(200), 160);
    let snapshot = render(&specs, 37);

    let error = run_calibration(&snapshot, &echo_model(), &config())
        .expect_err("a shifted holdout must be refused, not reported on");
    let CalibrationError::DistributionShift { measurement } = &error else {
        panic!("expected a drift refusal, got {error:?}");
    };
    assert!(
        measurement.population_stability_index
            > measurement.tolerances.max_population_stability_index,
        "the refusal must be caused by the measured shift: {measurement}"
    );
    assert!(
        measurement.population_stability_index > 0.5,
        "the two regimes share no probability bin at all, so the shift must be large: {measurement}"
    );
    assert_eq!(measurement.verdict(), DriftVerdict::Shifted);
    assert!(!measurement.is_acceptable());
    assert_eq!(measurement.fit_cohorts, 160);
    assert_eq!(measurement.holdout_cohorts, 40);
    assert!(measurement.summary().contains("psi="));
    // The refusal text carries the numbers, not just a verdict.
    assert!(error.to_string().contains("psi="));
    // The two partitions really do occupy disjoint bins, which is why the index
    // is what it is.
    let fit_support: Vec<usize> = measurement
        .fit_bin_proportions
        .iter()
        .enumerate()
        .filter(|(_, share)| **share > 0.0)
        .map(|(slot, _)| slot)
        .collect();
    let holdout_support: Vec<usize> = measurement
        .holdout_bin_proportions
        .iter()
        .enumerate()
        .filter(|(_, share)| **share > 0.0)
        .map(|(slot, _)| slot)
        .collect();
    assert_eq!(fit_support, vec![4, 5, 9], "the honest regime's bins");
    assert_eq!(holdout_support, vec![1, 6, 8], "the shifted regime's bins");
}

/// GATE 4, third part: the unshifted fixture is *not* refused, so the drift gate
/// is a gate and not a blanket rejection.
#[test]
fn an_unshifted_holdout_passes_the_drift_gate() {
    let snapshot = render(&main_fixture(200), 41);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let drift = &outcome.report().drift;
    assert!(drift.is_acceptable());
    assert_eq!(drift.verdict(), DriftVerdict::WithinTolerance);
    assert_eq!(drift.bin_count, 10);
    assert_eq!(drift.fit_bin_proportions.len(), 10);
    assert_eq!(drift.holdout_bin_proportions.len(), 10);
    assert!(
        (drift.fit_bin_proportions.iter().sum::<f64>() - 1.0).abs() < 1e-9,
        "the drift histogram must be a distribution"
    );
    assert!((drift.holdout_bin_proportions.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    // The head reports the same three numbers in both partitions, so the shift
    // is exactly zero. This asserts the measurement is sensitive to the input
    // rather than to the partition boundary.
    assert!(drift.population_stability_index < 1e-9, "{drift}");
    assert!(drift.base_rate_delta < 1e-9);
    assert!(drift.arity_delta < 1e-9);
    assert_eq!(drift.tolerances.bin_count, 10);
    assert_eq!(drift.tolerances.max_population_stability_index, 0.25);
    assert_eq!(drift.tolerances.max_base_rate_delta, 0.20);
}

/// GATE 4, fourth part: a degenerate partition is refused rather than producing
/// a calibrated-looking vector.
///
/// Each case is built to look as plausible as possible: requests that all
/// failed, and requests where one candidate always won.
#[test]
fn a_degenerate_partition_is_refused_with_a_typed_reason() {
    // Every request fails, so no cohort is attributed and every observed
    // frequency would be zero — any non-zero mass would be confidently wrong.
    let mut all_failed = main_fixture(200);
    for spec in &mut all_failed {
        spec.served = None;
    }
    assert_degenerate(
        &render(&all_failed, 43),
        DegeneracyReason::NoAttributedOutcome,
        "every request failed",
    );

    // One candidate always wins, so the axis cannot be told apart and a uniform
    // 1/K answer would score the base rate. This is the K-way analogue of the
    // single-class holdout 7E-2B refused on.
    let mut one_winner = main_fixture(200);
    for spec in &mut one_winner {
        if spec.served.is_some() {
            spec.served = Some(BRAVO);
        }
    }
    assert_degenerate(
        &render(&one_winner, 47),
        DegeneracyReason::SingleServedCandidate,
        "only one candidate ever served",
    );
}

/// GATE 4, fifth part: too few attributed decisions for the configured floor is
/// a refusal naming the holdout partition.
#[test]
fn a_sparsely_attributed_holdout_is_refused_by_name() {
    let mut sparse = main_fixture(200);
    // The holdout is the trailing forty decisions. Leave exactly two of them
    // served, by *different* candidates, so the attributed floor is what fires
    // rather than the single-winner check.
    for (index, spec) in sparse.iter_mut().enumerate() {
        if index < 160 {
            continue;
        }
        spec.served = match index {
            160 => Some(BRAVO),
            161 => Some(CHARLIE),
            _ => None,
        };
    }
    let error = run_calibration(&render(&sparse, 53), &honest_model(), &config())
        .expect_err("a holdout with two served decisions must be refused");
    match &error {
        CalibrationError::DegeneratePartition {
            partition,
            reason,
            detail,
        } => {
            assert_eq!(*partition, PartitionKind::Holdout);
            assert_eq!(
                *reason,
                DegeneracyReason::TooFewAttributedOutcomes,
                "the floor is what must fire here"
            );
            assert!(
                detail.contains('2'),
                "the refusal must carry its count: {detail}"
            );
        }
        other => panic!("expected a holdout degeneracy, got {other:?}"),
    }
    assert!(error.to_string().contains("holdout"));
}

/// GATE 4, sixth part: a snapshot too small to split at all is refused before
/// anything is fitted, and a configuration that could never split is refused
/// even earlier.
#[test]
fn a_snapshot_too_small_to_hold_out_is_refused() {
    let error = run_calibration(&render(&main_fixture(12), 59), &honest_model(), &config())
        .expect_err("twelve decisions cannot fill a forty-decision holdout");
    assert!(
        matches!(error, CalibrationError::SnapshotTooSmall { .. }),
        "expected a size refusal, got {error:?}"
    );

    let impossible = CalibrationConfig {
        holdout: HoldoutConfig {
            holdout_cohorts: 40,
            min_fit_cohorts: usize::MAX,
            min_attributed_outcomes: 4,
        },
        ..CalibrationConfig::default()
    };
    let error = run_calibration(
        &render(&main_fixture(200), 61),
        &honest_model(),
        &impossible,
    )
    .expect_err("a holdout that consumes the whole snapshot is meaningless");
    assert!(
        matches!(error, CalibrationError::MeaninglessHoldoutSplit { .. }),
        "expected a configuration refusal, got {error:?}"
    );
}

fn assert_degenerate(snapshot: &[OutcomeTrainingSample], expected: DegeneracyReason, why: &str) {
    match run_calibration(snapshot, &honest_model(), &config()) {
        Err(error) => {
            match &error {
                CalibrationError::DegeneratePartition {
                    partition,
                    reason,
                    detail,
                } => {
                    assert_eq!(*reason, expected, "{why}: got {error}");
                    assert!(
                        matches!(partition, PartitionKind::Fit | PartitionKind::Holdout),
                        "the refusal must name its partition"
                    );
                    assert!(!detail.is_empty(), "a refusal must carry its numbers");
                }
                other => panic!("{why}: expected a degeneracy, got {other:?}"),
            }
            assert!(
                !error.to_string().is_empty(),
                "a refusal must carry a reason"
            );
        }
        Ok(outcome) => panic!(
            "{why}: expected a {expected:?} refusal, got a verdict of {:?}",
            outcome.report().verdict
        ),
    }
}

// ---------------------------------------------------------------------------
// GATE 5 — fail-closed, typed refusals
// ---------------------------------------------------------------------------

/// GATE 5, first part: the cohort-scope refusals.
///
/// The mixed-`decision_id` case is the one worth stating: a request whose
/// attempt rows disagree about carrying a decision id is **refused**, not
/// coalesced. Coalescing would merge two real planning events into one invented
/// candidate set and would count a candidate twice if the same identity appeared
/// in both, so losing that fact is treated as the fail-closed condition it is.
#[test]
fn an_incoherent_decision_scope_is_refused_not_coalesced() {
    let mut snapshot = render(&main_fixture(200), 73);
    // Strip the decision id from one attempt row of one request, so that request
    // now disagrees with itself.
    let victim = snapshot
        .iter()
        .position(|row| matches!(row.scope, SampleScope::Attempt { index: 1, .. }))
        .expect("the fixture has a second attempt");
    let request = snapshot[victim].outcome_id.clone();
    snapshot[victim].decision_id = None;
    assert!(snapshot
        .iter()
        .any(|row| row.outcome_id == request && row.decision_id.is_some()));
    assert!(snapshot
        .iter()
        .any(|row| row.outcome_id == request && row.decision_id.is_none()));

    let error = run_calibration(&snapshot, &honest_model(), &config())
        .expect_err("a request that disagrees with itself must be refused");
    assert!(
        matches!(error, CalibrationError::MixedDecisionScope { .. }),
        "expected a mixed decision scope refusal, got {error:?}"
    );
    assert!(error.to_string().contains("decision id"));
}

/// GATE 5, second part: a decision id that names two requests is refused, and a
/// candidate set that cannot form a distribution is refused.
#[test]
fn a_decision_id_spanning_two_requests_is_refused() {
    let mut snapshot = render(&main_fixture(200), 79);
    let first_request = snapshot[0].outcome_id.clone();
    let first_decision = snapshot[0]
        .decision_id
        .clone()
        .expect("the fixture carries decision ids");

    // Point every attempt row of the *second* request at the first request's
    // decision id, so one decision id ends up naming two requests.
    let mut second_request = None;
    for row in &mut snapshot {
        if row.outcome_id == first_request {
            continue;
        }
        if second_request.is_none() {
            second_request = Some(row.outcome_id.clone());
        }
        if second_request.as_deref() == Some(row.outcome_id.as_str())
            && matches!(row.scope, SampleScope::Attempt { .. })
        {
            row.decision_id = Some(first_decision.clone());
        }
    }
    let second_request = second_request.expect("the fixture has a second request");
    assert!(snapshot.iter().any(|row| {
        row.outcome_id == second_request
            && row.decision_id.as_deref() == Some(first_decision.as_str())
    }));

    let error = run_calibration(&snapshot, &honest_model(), &config())
        .expect_err("one decision id naming two requests must be refused");
    assert!(
        matches!(error, CalibrationError::DecisionScopeSpansRequests { .. }),
        "expected a spanning refusal, got {error:?}"
    );
}

/// GATE 5, third part: a candidate set that cannot form a distribution.
///
/// Three shapes, all refused with a reason rather than coerced into a
/// normalized vector: no candidates, no ranked candidate, and a repeated
/// candidate.
#[test]
fn a_candidate_set_that_cannot_form_a_distribution_is_refused() {
    let empty = DecisionCohort::try_new(probe_context(1), None, Vec::new(), None);
    assert!(
        matches!(empty, Err(CalibrationError::CohortWithoutCandidates { .. })),
        "got {empty:?}"
    );

    let all_unranked = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![
            CandidateInput::Unranked {
                candidate: identity(ALPHA),
                reason: UnrankedReason::NotEligible,
            },
            CandidateInput::Unranked {
                candidate: identity(BRAVO),
                reason: UnrankedReason::ModelCold,
            },
        ],
        None,
    );
    assert!(
        matches!(
            all_unranked,
            Err(CalibrationError::CohortWithoutRankedCandidate { .. })
        ),
        "got {all_unranked:?}"
    );

    let repeated = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.9,
            },
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.4,
            },
        ],
        Some(identity(ALPHA)),
    );
    assert!(
        matches!(
            repeated,
            Err(CalibrationError::DuplicateCohortCandidate { .. })
        ),
        "got {repeated:?}"
    );

    // The same model offered by two providers is two candidates, not a
    // duplicate: the axis is a set of (provider, model) pairs.
    let two_providers = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![
            CandidateInput::Ranked {
                candidate: CandidateIdentity::new(ALPHA.1, ALPHA.0),
                raw_success_probability: 0.9,
            },
            CandidateInput::Ranked {
                candidate: CandidateIdentity::new(ALPHA.1, "p-other"),
                raw_success_probability: 0.4,
            },
        ],
        Some(CandidateIdentity::new(ALPHA.1, ALPHA.0)),
    );
    assert!(
        two_providers.is_ok(),
        "one model on two providers is a set of two"
    );

    // A served candidate that is not on its own axis.
    let foreign = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![CandidateInput::Ranked {
            candidate: identity(ALPHA),
            raw_success_probability: 0.9,
        }],
        Some(identity(CHARLIE)),
    );
    assert!(
        matches!(
            foreign,
            Err(CalibrationError::ServedCandidateNotInCohort { .. })
        ),
        "got {foreign:?}"
    );
}

/// GATE 5, fourth part: a malformed row is a typed refusal, never a silent
/// skip and never a substituted value.
#[test]
fn a_malformed_row_is_a_typed_refusal() {
    // A repeated sample id would put one candidate into a cohort twice. The
    // clone is an *attempt* row, because request-scope rows are not candidate
    // evidence and are ignored by design.
    let mut duplicated = render(&main_fixture(200), 83);
    let victim = duplicated
        .iter()
        .position(|row| matches!(row.scope, SampleScope::Attempt { .. }))
        .expect("the fixture has attempt rows");
    let clone = duplicated[victim].clone();
    duplicated.push(clone);
    let error = run_calibration(&duplicated, &honest_model(), &config())
        .expect_err("a repeated row must be refused");
    assert!(
        matches!(error, CalibrationError::DuplicateSampleId { .. }),
        "got {error:?}"
    );

    // A schema version this build does not accept.
    let mut wrong_schema = render(&main_fixture(200), 89);
    wrong_schema[1].schema_version = FEATURE_SCHEMA_VERSION + 1;
    let error = run_calibration(&wrong_schema, &honest_model(), &config())
        .expect_err("a foreign schema must be refused");
    assert!(
        matches!(
            error,
            CalibrationError::SchemaMismatch {
                component: "sample",
                ..
            }
        ),
        "got {error:?}"
    );

    // A feature vector of the wrong width.
    let mut wrong_width = render(&main_fixture(200), 97);
    wrong_width[1].features.values[0] = f32::NAN;
    let error = run_calibration(&wrong_width, &honest_model(), &config())
        .expect_err("a non-finite feature must be refused");
    assert!(
        matches!(error, CalibrationError::NonFiniteFeature { .. }),
        "got {error:?}"
    );

    // An empty snapshot.
    let error = run_calibration(&[], &honest_model(), &config())
        .expect_err("an empty snapshot must be refused");
    assert!(
        matches!(error, CalibrationError::EmptySnapshot),
        "got {error:?}"
    );
}

/// GATE 5, fifth part: a non-finite or out-of-range prediction is refused, never
/// clamped into range.
///
/// A `NaN` probability is a broken head, not a very confident one, and silently
/// repairing it would be exactly the "calibrated-looking vector" this node is
/// required not to produce.
#[test]
fn a_non_finite_or_out_of_range_prediction_is_refused() {
    let snapshot = render(&main_fixture(200), 101);

    for broken in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.4, -0.2] {
        let error = run_calibration(&snapshot, &BrokenModel(broken), &config()).unwrap_err();
        assert!(
            matches!(
                error,
                CalibrationError::NonFinitePrediction { .. }
                    | CalibrationError::PredictionOutOfRange { .. }
            ),
            "a head returning {broken} must be refused, got {error:?}"
        );
    }

    // And the same refusal surfaces from the bare projection entry point, not
    // only through the full run.
    let error =
        project_cohorts(&snapshot, &BrokenModel(f64::NAN), DEFAULT_PROBABILITY_FLOOR).unwrap_err();
    assert!(matches!(
        error,
        CalibrationError::NonFinitePrediction { .. }
    ));
}

/// GATE 5, sixth part: every configuration that would make a gate meaningless is
/// refused before a single row is read.
#[test]
fn a_meaningless_configuration_is_refused_before_anything_is_read() {
    let snapshot = render(&main_fixture(200), 103);
    let base = config();

    let cases: Vec<(CalibrationConfig, &str)> = vec![
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    bin_count: 0,
                    ..base.reliability
                },
                ..base
            },
            "zero bins is a curve with no bins",
        ),
        (
            CalibrationConfig {
                drift: DriftConfig {
                    bin_count: 0,
                    ..base.drift
                },
                ..base
            },
            "zero drift bins is an index with no bins",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_expected_calibration_error: -0.1,
                    ..base.reliability
                },
                ..base
            },
            "a negative ece ceiling could never be met",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_expected_calibration_error: f64::INFINITY,
                    ..base.reliability
                },
                ..base
            },
            "an infinite ece ceiling could always be met",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_candidate_calibration_error: -1.0,
                    ..base.reliability
                },
                ..base
            },
            "a negative per-candidate ceiling could never be met",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_calibration_error: f64::NAN,
                    ..base.reliability
                },
                ..base
            },
            "a NaN ceiling is not a comparison",
        ),
        (
            CalibrationConfig {
                drift: DriftConfig {
                    max_population_stability_index: -0.5,
                    ..base.drift
                },
                ..base
            },
            "a negative psi ceiling could never be met",
        ),
        (
            CalibrationConfig {
                drift: DriftConfig {
                    max_base_rate_delta: 1.5,
                    ..base.drift
                },
                ..base
            },
            "a base-rate ceiling above one is not a difference of two rates",
        ),
        (
            CalibrationConfig {
                holdout: HoldoutConfig {
                    holdout_cohorts: 0,
                    ..base.holdout
                },
                ..base
            },
            "a holdout of zero decisions decides nothing",
        ),
        (
            CalibrationConfig {
                holdout: HoldoutConfig {
                    min_fit_cohorts: 0,
                    ..base.holdout
                },
                ..base
            },
            "a fit floor of zero would permit fitting on nothing",
        ),
        (
            CalibrationConfig {
                holdout: HoldoutConfig {
                    min_attributed_outcomes: 0,
                    ..base.holdout
                },
                ..base
            },
            "an attributed floor of zero would pass what it exists to fail",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    iterations: 0,
                    ..base.fit
                },
                ..base
            },
            "zero gradient steps would report an unfitted calibrator as fitted",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    learning_rate: 0.0,
                    ..base.fit
                },
                ..base
            },
            "a zero step size never moves the parameters",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    min_temperature: 0.0,
                    ..base.fit
                },
                ..base
            },
            "a zero temperature collapses the vector onto one candidate",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    max_temperature: 0.01,
                    ..base.fit
                },
                ..base
            },
            "a maximum temperature below the floor pins the fit",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    intercept_l2: -1.0,
                    ..base.fit
                },
                ..base
            },
            "negative regularization is an anti-prior",
        ),
        (
            CalibrationConfig {
                probability_floor: 0.0,
                ..base
            },
            "a zero probability floor makes ln(0) reachable",
        ),
        (
            CalibrationConfig {
                probability_floor: 0.5,
                ..base
            },
            "a floor of one half collapses the unit interval",
        ),
    ];

    for (candidate, why) in cases {
        let error = match run_calibration(&snapshot, &honest_model(), &candidate) {
            Ok(outcome) => panic!(
                "{why}: expected a configuration refusal, got a verdict of {:?}",
                outcome.report().verdict
            ),
            Err(error) => error,
        };
        assert!(
            !error.to_string().is_empty(),
            "{why}: the refusal must carry a reason"
        );
        assert!(
            !matches!(error, CalibrationError::DistributionShift { .. }),
            "{why}: a configuration must be refused before any partition is examined"
        );
    }
}

/// GATE 5, seventh part: a measurement asked for over nothing is a refusal, not
/// an empty-but-plausible report.
#[test]
fn measuring_nothing_is_a_refusal() {
    let error = measure_emitted(&[], &ReliabilityConfig::default())
        .expect_err("no observations is not a measurement");
    assert!(
        matches!(error, CalibrationError::NoObservations { .. }),
        "got {error:?}"
    );

    let error = measure_emitted(
        &[],
        &ReliabilityConfig {
            bin_count: 0,
            ..ReliabilityConfig::default()
        },
    )
    .expect_err("zero bins is checked first");
    assert!(
        matches!(error, CalibrationError::ZeroReliabilityBins { .. }),
        "got {error:?}"
    );

    let error = measure_marginal(&[], MarginalView::Calibrated, &ReliabilityConfig::default())
        .expect_err("no observations is not a measurement");
    assert!(
        matches!(error, CalibrationError::NoObservations { .. }),
        "got {error:?}"
    );
}

/// GATE 5, eighth part: the module holds no panic path.
///
/// The whole contract is "fail closed, never panic", so a bare `unwrap`, an
/// `expect`, or a `panic!` in this module would be exactly the surface the
/// contract forbids. The tokens are checked against the module's own source.
#[test]
fn the_calibration_module_holds_no_panic_path() {
    let source = include_str!("../src/ml/calibration.rs");
    for forbidden in [
        "unwrap(",
        ".expect(",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "assert!",
        "assert_eq!",
    ] {
        assert!(
            !source.contains(forbidden),
            "calibration.rs must not contain {forbidden}"
        );
    }
}

// ---------------------------------------------------------------------------
// GATE 6 — production inaccessibility
// ---------------------------------------------------------------------------

/// GATE 6: this module reaches no installation, scheduling, or routing surface.
///
/// The tokens are code-shaped rather than prose-shaped, so a documentation
/// sentence about what the module does not do cannot satisfy or trip the wire.
#[test]
fn the_calibration_source_reaches_no_installation_or_background_surface() {
    let source = include_str!("../src/ml/calibration.rs").to_lowercase();
    for forbidden in [
        // Live-state mutation and the stores that carry it.
        "rwlock",
        "crate::sync::write",
        "crate::sync::read",
        "modelstore",
        "datasetstore",
        "try_commit",
        // The only type that carries an installation call.
        "shadowengine",
        "shadow::shadowstore",
        "swap",
        "try_train_and_swap",
        "from_trained_parts",
        // Server wiring. Path-shaped tokens, so a doc sentence that *describes*
        // what the module does not do can neither satisfy nor trip the wire.
        "appstate",
        "build_app",
        "serverhandle",
        "server::",
        "axum",
        "reqwest",
        "router::",
        "policy::",
        "coordinator::",
        "shadow::",
        "model::shadowengine",
        // Background work, scheduling, and timers.
        "std::thread",
        "thread::spawn",
        "tokio::spawn",
        "spawn(",
        "sleep(",
        "interval(",
        "tokio::time",
        "tokio::task",
        // Durable state and the network.
        "filesystem",
        "std::fs",
        "reqwest",
        "listen",
    ] {
        assert!(
            !source.contains(forbidden),
            "calibration.rs must not reference {forbidden}"
        );
    }
}

/// GATE 6, behavioural: nothing outside this module names it, so the running
/// product cannot reach the distribution.
///
/// The distribution is a diagnostic artifact. If the product cannot name the
/// module, it cannot consume the vector — which is the claim, stated as a
/// property of the repository rather than of a promise in a doc comment.
#[test]
fn nothing_in_the_running_product_can_reach_this_module() {
    // The live surfaces that would have to name it. `include_str!` needs a
    // literal, so the list is spelled out rather than looped.
    for (path, source) in [
        (
            "src-tauri/src/main.rs",
            include_str!("../../../src-tauri/src/main.rs"),
        ),
        ("src/router.rs", include_str!("../src/router.rs")),
        ("src/policy.rs", include_str!("../src/policy.rs")),
        (
            "src/ml/coordinator.rs",
            include_str!("../src/ml/coordinator.rs"),
        ),
    ] {
        let source = source.to_lowercase();
        assert!(
            !source.contains("ml::calibration"),
            "{path} must not reach ml::calibration"
        );
        assert!(
            !source.contains("run_calibration"),
            "{path} must not reach run_calibration"
        );
        assert!(
            !source.contains("decisiondistribution::try_new"),
            "{path} must not construct a distribution"
        );
    }

    // The re-export exists, so the surface is public and consumable by a later
    // node — it is simply not consumed yet.
    assert!(
        include_str!("../src/ml/mod.rs").contains("pub mod calibration;"),
        "the module must be a public, reusable surface for the node that consumes it"
    );

    // And the report says, in words, that the vector is unconsumed.
    let snapshot = render(&main_fixture(200), 107);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    assert_eq!(outcome.report().distribution_role, DISTRIBUTION_ROLE);
    assert!(DISTRIBUTION_ROLE.contains("unconsumed"));
    assert!(DISTRIBUTION_ROLE.contains("no routing effect"));
    assert!(DISTRIBUTION_ROLE.contains("not a live probability"));
}

// ---------------------------------------------------------------------------
// The validating wire form
// ---------------------------------------------------------------------------

/// The 7E-2A type deliberately derives neither `Serialize` nor `Deserialize`,
/// on the stated terms that the node which produces one owns giving it a wire
/// form. This is that wire form, and it is *validating*: a stored record cannot
/// re-enter the measurement path having skipped the arity and normalization
/// checks.
#[test]
fn the_wire_form_revalidates_on_the_way_back_in() {
    let snapshot = render(&main_fixture(200), 109);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let records = outcome.holdout_records();
    assert_eq!(records.len(), 40);

    for (record, decision) in records.iter().zip(outcome.holdout_distributions()) {
        assert_eq!(record.cohort_fingerprint, decision.cohort_fingerprint());
        assert_eq!(record.outcomes, decision.distribution().outcomes());
        assert_eq!(
            record.probabilities,
            decision.distribution().probabilities()
        );
        assert!((record.total_mass - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE);
        assert_eq!(record.unranked, decision.unranked());

        let rebuilt = record
            .try_into_distribution()
            .expect("a well-formed record must rebuild");
        assert_eq!(&rebuilt, decision.distribution());
    }

    // A tampered record is refused on the way back in, not accepted.
    let mut tampered = records[0].clone();
    tampered.probabilities[0] += 0.5;
    let error = tampered
        .try_into_distribution()
        .expect_err("a tampered record must be refused");
    assert!(
        matches!(error, CalibrationError::DistributionRejected { .. }),
        "got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains(DISTRIBUTION_NORMALIZATION_TOLERANCE.to_string().as_str())
            || error.to_string().contains("not normalized")
    );

    let mut wrong_arity = records[0].clone();
    wrong_arity.probabilities.pop();
    assert!(wrong_arity.try_into_distribution().is_err());

    // A subject that is not on its own axis. `CHARLIE` would be accepted — it
    // *is* on this record's axis — so the stand-in is a candidate from
    // elsewhere.
    let mut wrong_subject = records[0].clone();
    wrong_subject.subject = CandidateIdentity::new("model-elsewhere", "p-elsewhere");
    let error = wrong_subject
        .try_into_distribution()
        .expect_err("a foreign subject must be refused");
    assert!(matches!(
        error,
        CalibrationError::DistributionRejected { .. }
    ));

    let mut negative = records[0].clone();
    negative.probabilities[0] = -0.1;
    negative.probabilities[1] = 1.1;
    assert!(negative.try_into_distribution().is_err());

    let mut non_finite = records[0].clone();
    non_finite.probabilities[0] = f64::NAN;
    assert!(non_finite.try_into_distribution().is_err());
}
