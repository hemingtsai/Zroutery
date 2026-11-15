use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::calibration::{CalibrationConfig, CohortContext, HoldoutConfig};
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model::{ModelState, Prediction, RoutingModel};
use zroutery_core::outcome::{
    Attempt, CandidateIdentity, FailureFacts, FinalStatus, OutcomeIdentity,
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
pub(super) struct ScriptedModel {
    /// Raw success probability in per-mille, indexed by the candidate tag the
    /// feature vector carries.
    table: Vec<u16>,
}

impl ScriptedModel {
    pub(super) fn new(table: &[u16]) -> Self {
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
pub(super) struct FeatureEchoModel;

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
pub(super) struct ConstantModel(pub(super) f64);
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
pub(super) struct BrokenModel(pub(super) f64);

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

pub(super) const ALPHA: (&str, &str) = ("p-alpha", "model-alpha");
pub(super) const BRAVO: (&str, &str) = ("p-bravo", "model-bravo");
pub(super) const CHARLIE: (&str, &str) = ("p-charlie", "model-charlie");

/// Per-mille the honest head reports for each candidate, by tag order.
pub(super) const HONEST_MILLI: [u16; 3] = [900, 550, 400];
/// A head certain that `alpha` serves, and wrong about it most of the time.
pub(super) const CONFIDENT_WRONG_MILLI: [u16; 3] = [950, 30, 30];
/// A head whose ranking is the exact reverse of the truth, and confident.
pub(super) const REVERSED_MILLI: [u16; 3] = [900, 100, 400];
/// A head that is correctly ranked and merely over-confident.
pub(super) const MILD_MILLI: [u16; 3] = [350, 820, 680];
/// The other regime, for the drift fixture: the same candidates, reported at
/// the opposite end of the unit interval.
pub(super) const SHIFTED_MILLI: [u16; 3] = [120, 880, 610];

/// One decision in the fixture.
#[derive(Clone)]
pub(super) struct Spec {
    /// The candidates the decision considered, in attempt order, each with the
    /// per-mille the scripted head reports for it.
    pub(super) candidates: Vec<(&'static str, &'static str, u16)>,
    /// Who served, or `None` when the request failed outright.
    pub(super) served: Option<(&'static str, &'static str)>,
}

pub(super) fn identity(pair: (&str, &str)) -> CandidateIdentity {
    CandidateIdentity::new(pair.1, pair.0)
}

pub(super) const BASE_TIMESTAMP: i64 = 1_700_000_000;

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
pub(super) fn main_fixture(count: usize) -> Vec<Spec> {
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
pub(super) fn shifted_fixture(specs: &[Spec], from: usize) -> Vec<Spec> {
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
pub(super) fn mild_fixture(count: usize) -> Vec<Spec> {
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
pub(super) fn render(specs: &[Spec], salt: u64) -> Vec<OutcomeTrainingSample> {
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

pub(super) fn scripted_features(tag: u16, milli: u16) -> RoutingFeatures {
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

pub(super) fn honest_model() -> ScriptedModel {
    ScriptedModel::new(&HONEST_MILLI)
}

pub(super) fn mild_model() -> ScriptedModel {
    ScriptedModel::new(&MILD_MILLI)
}

/// The head used for the drift fixture: it reports whatever the snapshot
/// recorded, so the fit partition and the holdout really are two regimes.
///
/// A table-indexed head cannot do this job: it returns the same number for the
/// same candidate tag in both partitions, the shift cancels, and the drift gate
/// would correctly report no shift — for the wrong reason.
pub(super) fn echo_model() -> FeatureEchoModel {
    FeatureEchoModel
}

pub(super) fn mild_config() -> CalibrationConfig {
    CalibrationConfig {
        holdout: HoldoutConfig {
            holdout_cohorts: 60,
            min_fit_cohorts: 20,
            min_attributed_outcomes: 4,
        },
        ..CalibrationConfig::default()
    }
}

pub(super) fn config() -> CalibrationConfig {
    CalibrationConfig {
        holdout: HoldoutConfig {
            holdout_cohorts: 40,
            min_fit_cohorts: 20,
            min_attributed_outcomes: 4,
        },
        ..CalibrationConfig::default()
    }
}

pub(super) fn probe_context(timestamp: i64) -> CohortContext {
    CohortContext {
        timestamp,
        dialect: "anthropic".to_string(),
        streaming: false,
        final_status_rank: 0,
        failure_class_rank: None,
    }
}

/// Every file of the calibration module, spelled out.
///
/// `calibration.rs` became `calibration/` and these tripwires have to follow
/// it. A fence reading only the barrel would stop seeing a write the day it
/// moved, which is indistinguishable from the write not existing.
pub(super) const CALIBRATION_SOURCES: &[(&str, &str)] = &[
    (
        "calibration/mod.rs",
        include_str!("../../src/ml/calibration/mod.rs"),
    ),
    (
        "calibration/cohort.rs",
        include_str!("../../src/ml/calibration/cohort.rs"),
    ),
    (
        "calibration/config.rs",
        include_str!("../../src/ml/calibration/config.rs"),
    ),
    (
        "calibration/drift.rs",
        include_str!("../../src/ml/calibration/drift.rs"),
    ),
    (
        "calibration/errors.rs",
        include_str!("../../src/ml/calibration/errors.rs"),
    ),
    (
        "calibration/kway.rs",
        include_str!("../../src/ml/calibration/kway.rs"),
    ),
    (
        "calibration/marginal.rs",
        include_str!("../../src/ml/calibration/marginal.rs"),
    ),
    (
        "calibration/measure.rs",
        include_str!("../../src/ml/calibration/measure.rs"),
    ),
    (
        "calibration/report.rs",
        include_str!("../../src/ml/calibration/report.rs"),
    ),
    (
        "calibration/run.rs",
        include_str!("../../src/ml/calibration/run.rs"),
    ),
    (
        "calibration/types.rs",
        include_str!("../../src/ml/calibration/types.rs"),
    ),
];
