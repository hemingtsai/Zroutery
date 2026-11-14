use super::error::OfflineGateError;
use super::replay::{build_holdout, holdout_decision_count, replay_one, summarise_holdout};
use super::report::{
    CommitTransport, GateInput, JournalFloatFidelity, ReleaseMeasurements, ReleaseReport,
    ReleaseVerdict, RetentionAblation,
};
use super::{RELEASE_SCOPE, STATISTICAL_SCOPE};
use crate::ml::calibration::{
    project_cohorts, run_calibration, CalibrationConfig, CalibrationError, CalibrationOutcome,
};
use crate::ml::dataset::OutcomeTrainingSample;
use crate::ml::decision_engine::DecisionEngine;
use crate::ml::evaluation::f64_identical;
use crate::ml::model_identity::{ModelCheckpoint, ModelCommit, ModelEnsemble, ReplayError};
use crate::ml::shadow::{ModelEnsemblePredictor, ShadowEngine};
use crate::ml::statistics::{
    measure_release_evidence, StatisticalConfig, StatisticalInput, StatisticalRefusal,
    StatisticalRelease,
};

// ---------------------------------------------------------------------------
// 7D's measurement, carried from its own module
// ---------------------------------------------------------------------------

/// Measure 7D's statistical constituent over the holdout 7E-2D already
/// reserved, consuming only 7E-2D's own outputs.
///
/// Three things are taken, none of them recomputed:
///
/// * the holdout partition, from the accepted `project_cohorts`, split at the
///   same trailing count 7E-2D's `run_calibration` documents. The K axis comes
///   from attempt-scope rows, so a partition built from request-scope rows
///   alone cannot produce a one-candidate axis here: it produces the refusal
///   7E-2D raises first;
/// * the emitted distributions, from the accepted [`CalibrationOutcome`], whose
///   per-row fingerprint is checked against the partition's, so a misaligned
///   pair is refused rather than cross-attributed;
/// * the unconditional base rate, from the accepted marginal route's
///   per-candidate table, whose counts are cross-checked against the counts this
///   walk measures.
fn measure_statistics(
    holdout_samples: &[OutcomeTrainingSample],
    ensemble: &ModelEnsemble,
    calibration: &CalibrationOutcome,
    calibration_config: &CalibrationConfig,
    config: StatisticalConfig,
) -> StatisticalRelease {
    let cohorts = match project_cohorts(
        holdout_samples,
        &ensemble.success,
        calibration_config.probability_floor,
    ) {
        Ok(cohorts) => cohorts,
        Err(error) => {
            return StatisticalRelease::Refused(StatisticalRefusal {
                code: "calibration_projection_refused",
                reason: format!("7E-2D's projection could not be repeated: {error}"),
            });
        }
    };
    // 7E-2D split at `len - holdout_cohorts` and its own report carries the
    // numbers. If the two disagree, the partition 7D measured is not the one
    // 7E-2D's verdict was about, so it is refused rather than assumed.
    let report = calibration.report();
    let split = cohorts
        .len()
        .saturating_sub(calibration_config.holdout.holdout_cohorts);
    if report.holdout_cohorts != cohorts.len() - split
        || report.fit_cohorts != split
        || report.cohorts_total != cohorts.len()
    {
        return StatisticalRelease::Refused(StatisticalRefusal {
            code: "partition_disagrees_with_calibration",
            reason: format!(
                "7E-2D reported {} cohorts split {}/{} but the projection yields {} split {split}/{}",
                report.cohorts_total,
                report.fit_cohorts,
                report.holdout_cohorts,
                cohorts.len(),
                cohorts.len() - split
            ),
        });
    }
    let partition = &cohorts[split..];
    match measure_release_evidence(&StatisticalInput {
        partition,
        emitted: calibration.holdout_distributions(),
        marginal: &report.marginal_calibrated.per_candidate,
        config,
    }) {
        Ok(measured) => measured,
        Err(error) => StatisticalRelease::Refused(StatisticalRefusal::from(&error)),
    }
}

// ---------------------------------------------------------------------------
// run_offline_gate
// ---------------------------------------------------------------------------

/// Everything one offline gate run produced.
pub struct GateOutcome {
    predictor: ModelEnsemblePredictor,
    report: ReleaseReport,
}

impl std::fmt::Debug for GateOutcome {
    /// Structural, without dumping every replayed scalar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateOutcome")
            .field("model_commit", &self.report.model_commit)
            .field("verdict", &self.report.recomputed_verdict())
            .field("scope", &RELEASE_SCOPE)
            .finish()
    }
}

impl GateOutcome {
    /// The honest report.
    pub fn report(&self) -> &ReleaseReport {
        &self.report
    }

    /// The predictor the replay ran against, pinned to the verified commit.
    pub fn predictor(&self) -> &ModelEnsemblePredictor {
        &self.predictor
    }

    /// Whether the evidence supports considering the model, recomputed.
    pub fn is_considerable(&self) -> bool {
        self.report.is_considerable()
    }
}

/// Run the offline release gate.
///
/// The order of the gates is the order of the claims:
///
/// 1. something to decide, or there is no verdict;
/// 2. the artifact is present and transports bit-exactly, and its retained
///    lineage verifies — because a decision attributed to a commit nobody can
///    reproduce is not evidence about that commit;
/// 3. each recorded decision replays **bit-identically** from its retained
///    input, and the retention is shown to be load-bearing;
/// 4. each replay sits consistently with its canonical Outcome, is classified
///    through the accepted impact table, and is keyed on the identity that
///    actually served;
/// 5. the holdout is proven disjoint from the fit set and large enough to be
///    measured at all;
/// 6. the holdout's float fidelity across this workspace's read path is
///    measured;
/// 7. 7E-2D's calibration measurement runs over that holdout and its verdict
///    is carried whole;
/// 8. the release verdict is recomputed from every constituent above.
///
/// Pure with respect to routing state. Nothing is installed, no live engine is
/// touched, no thread is started, and the [`ShadowEngine`] this builds is local
/// to the call and discarded when it returns.
pub fn run_offline_gate(input: &GateInput) -> Result<GateOutcome, OfflineGateError> {
    if input.recorded.is_empty() {
        return Err(OfflineGateError::NothingToGate);
    }
    if input.commit.commit_id.as_str().is_empty() {
        return Err(OfflineGateError::MissingCommit {
            reason: "the supplied commit does not name itself".to_string(),
        });
    }

    // -- 2. the artifact --
    let transport = measure_commit_transport(&input.commit)?;
    let commit = input.commit.clone();
    let predictor = build_predictor(&commit, &input.lineage)?;

    // -- 3/4. per decision --
    let engine = ShadowEngine::new(
        DecisionEngine::new(
            input.config.engine.clone(),
            input.config.reward_policy.clone(),
        ),
        true,
    );
    let mut replay = Vec::with_capacity(input.recorded.len());
    for recorded in &input.recorded {
        replay.push(replay_one(&engine, &predictor, recorded, &input.config)?);
    }

    // -- 5. the holdout --
    let holdout_samples = build_holdout(&input.recorded, &replay, &input.fit_sample_ids)?;
    // The floor is on independent decisions, and the holdout carries several
    // canonical rows per decision. Comparing it against `holdout_samples.len()`
    // would satisfy a floor of N decisions with as few as N/(attempts + 1)
    // decisions, which is not the evidence the caller asked to be shown.
    let holdout_decisions = holdout_decision_count(&input.recorded);
    if holdout_decisions < input.config.floors.min_holdout_decisions {
        return Err(OfflineGateError::InsufficientEvidence {
            context: "the holdout decision count",
            observed: holdout_decisions,
            required: input.config.floors.min_holdout_decisions,
        });
    }
    if input.recorded.len() < input.config.floors.min_replayed_decisions {
        return Err(OfflineGateError::InsufficientEvidence {
            context: "the replayed decision count",
            observed: input.recorded.len(),
            required: input.config.floors.min_replayed_decisions,
        });
    }

    let holdout = summarise_holdout(&input.recorded, &replay, holdout_samples.len());
    // Measured over exactly the rows the calibration gate is about to consume,
    // so the fidelity number describes the measurements rather than a parallel
    // set that happens to look similar.
    let float_fidelity = JournalFloatFidelity::measure(&holdout_samples);

    // -- 6/7. 7E-2D's calibration gate, consumed whole --
    let ensemble = ModelEnsemble::load_all(&commit.checkpoint).map_err(|error| {
        OfflineGateError::UnverifiableCommit {
            commit_id: commit.commit_id.as_str().to_string(),
            reason: format!("the verified checkpoint did not load: {error}"),
        }
    })?;
    let calibration = run_calibration(
        &holdout_samples,
        &ensemble.success,
        &input.config.calibration,
    )
    .map_err(map_calibration_refusal)?;
    let calibration_verdict = calibration.report().recomputed_verdict();

    // -- 7D's statistical constituent, measured over the same partition --
    //
    // Everything here is 7E-2D's: the cohorts come from the accepted
    // `project_cohorts`, the emitted distributions come from the accepted
    // `CalibrationOutcome`, the unconditional base rate is the accepted
    // marginal route's per-candidate table, and the partition is the same
    // trailing holdout 7E-2D already reserved, recomputed by the same
    // arithmetic it documents. Nothing about the measurement is re-derived here.
    //
    // A refusal is recorded rather than propagated. This gate has to keep
    // producing its report when the statistical claim cannot be measured — a
    // thin holdout is a finding, not a crash — and recording the refusal is
    // what makes it a blocker instead of a silence. The refusal is carried
    // whole, with 7E-2D's own error nested inside it if that is what happened,
    // so nothing is flattened into a generic error.
    let statistics = measure_statistics(
        &holdout_samples,
        &ensemble,
        &calibration,
        &input.config.calibration,
        input.config.statistics,
    );

    let measurements = ReleaseMeasurements {
        commit_verified: true,
        lineage_verified: true,
        lineage_len: predictor.lineage().len(),
        decisions_replayed: replay.len(),
        decisions_equivalent: replay.len(),
        terminal_agreements: replay.len(),
        retention: RetentionAblation {
            probed: replay
                .iter()
                .map(|evidence| evidence.retention.ablation.probed)
                .sum(),
            load_bearing: replay
                .iter()
                .map(|evidence| evidence.retention.ablation.load_bearing)
                .sum(),
        },
        served_identities: replay
            .iter()
            .filter(|evidence| evidence.served.is_some())
            .count(),
        absent_served_identities: 0,
        holdout,
        calibration: calibration_verdict,
        float_fidelity: float_fidelity.clone(),
        statistics: statistics.clone(),
    };
    let verdict = ReleaseVerdict::from_measurements(&measurements);

    let report = ReleaseReport {
        model_commit: commit.commit_id.clone(),
        model_id: commit.model_id.as_str().to_string(),
        feature_schema: input.recorded[0].input().feature_schema,
        transport,
        replay,
        holdout: measurements.holdout.clone(),
        calibration: calibration.report().clone(),
        float_fidelity,
        statistics,
        measurements,
        verdict,
        scope: RELEASE_SCOPE,
        statistical_scope: STATISTICAL_SCOPE,
    };

    Ok(GateOutcome { predictor, report })
}

// ---------------------------------------------------------------------------
// 2. the artifact
// ---------------------------------------------------------------------------

/// Verify the commit with the accepted check and measure its plain-JSON
/// transport.
///
/// The verification is the gate: a commit whose checkpoint parameters do not
/// hash to the content hash its identity was derived from is refused, and there
/// is no tolerance on that either — the accepted check hashes `f64::to_bits`.
fn measure_commit_transport(commit: &ModelCommit) -> Result<CommitTransport, OfflineGateError> {
    if !commit.verify() {
        return Err(OfflineGateError::UnverifiableCommit {
            commit_id: commit.commit_id.as_str().to_string(),
            reason: "the commit failed its own identity and artifact verification".to_string(),
        });
    }
    let mut f64_parameters = 0usize;
    let mut moved_parameters = 0usize;
    if let Ok(bytes) = serde_json::to_vec(commit) {
        if let Ok(reparsed) = serde_json::from_slice::<ModelCommit>(&bytes) {
            for (original, round_tripped) in checkpoint_states(&commit.checkpoint)
                .into_iter()
                .zip(checkpoint_states(&reparsed.checkpoint))
            {
                for (before, after) in original.iter().zip(round_tripped.iter()) {
                    f64_parameters += 1;
                    if !f64_identical(*before, *after) {
                        moved_parameters += 1;
                    }
                }
            }
        }
    }
    Ok(CommitTransport {
        commit_id: commit.commit_id.as_str().to_string(),
        f64_parameters,
        moved_parameters,
        plain_json_exact: moved_parameters == 0,
    })
}

/// A checkpoint's four model-state parameter vectors, in a fixed order, so the
/// transport measurement is a function of the checkpoint alone.
fn checkpoint_states(checkpoint: &ModelCheckpoint) -> [&[f64]; 4] {
    [
        &checkpoint.success.parameters,
        &checkpoint.latency.parameters,
        &checkpoint.ttft.parameters,
        &checkpoint.cost.parameters,
    ]
}

/// Build the predictor, mapping the accepted artifact refusals onto typed ones.
fn build_predictor(
    commit: &ModelCommit,
    lineage: &[ModelCommit],
) -> Result<ModelEnsemblePredictor, OfflineGateError> {
    ModelEnsemblePredictor::from_model_commit_with_lineage(commit, lineage).map_err(|error| {
        let reason = error.to_string();
        match error {
            ReplayError::InvalidCommit { .. } => OfflineGateError::UnverifiableCommit {
                commit_id: commit.commit_id.as_str().to_string(),
                reason,
            },
            _ => OfflineGateError::LineageRejected {
                commit_id: commit.commit_id.as_str().to_string(),
                reason,
            },
        }
    })
}

// ---------------------------------------------------------------------------
// 7E-2D's refusals, carried rather than re-derived
// ---------------------------------------------------------------------------

/// Carry 7E-2D's refusal, singling out the two this gate has its own reason for.
///
/// The two mapped cases keep 7E-2D's own classification — the gate does not
/// decide what "degenerate" means — and the rest is held whole so no refusal
/// is flattened into a generic error. The catch-all is deliberate: this gate
/// is sequenced before 7D and before any later revision of 7E-2D, and it must
/// not be the thing that breaks when a refusal is added upstream.
fn map_calibration_refusal(error: CalibrationError) -> OfflineGateError {
    match error {
        CalibrationError::DegeneratePartition {
            partition,
            reason,
            detail,
        } => OfflineGateError::DegenerateHoldout {
            partition,
            reason,
            detail,
        },
        other => OfflineGateError::CalibrationRefused {
            error: Box::new(other),
        },
    }
}
