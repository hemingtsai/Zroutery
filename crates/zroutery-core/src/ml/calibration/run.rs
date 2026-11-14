use super::cohort::{project_cohorts, unordered_at, DecisionCohort};
use super::config::CalibrationConfig;
use super::drift::{attributed_rate, measure_drift};
use super::errors::CalibrationError;
use super::kway::{log_probability, KWayCalibrator};
use super::marginal::{
    collect_marginal_observations, measure_marginal, MarginalCalibrator, MarginalView,
    NormalizationDamage,
};
use super::measure::measure_emitted;
use super::report::{
    AcceptanceTolerances, CalibrationOutcome, CalibrationReport, CalibrationVerdict,
};
use super::types::{DegeneracyReason, PartitionKind};
use super::DISTRIBUTION_ROLE;
use crate::ml::dataset::OutcomeTrainingSample;
use crate::ml::model::RoutingModel;
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// run_calibration
// ---------------------------------------------------------------------------

/// Fit a joint K-way calibrator, measure the vector it emits, and report
/// honestly.
///
/// The run is pure. The snapshot, the model, and the configuration are the only
/// inputs; a fitted calibrator, one emitted distribution per holdout decision,
/// and the report are the only outputs. Nothing is installed, and nothing in
/// the running product calls this.
///
/// The order of the gates is the order of the claims. The configuration is
/// checked first, because a vacuous gate must be refused before it can report.
/// The partition is taken on the canonical content order, so fit and holdout
/// are disjoint and the split does not depend on row arrival. Both partitions
/// are checked for degeneracy before anything is fitted, so a degenerate
/// holdout cannot produce a fitted calibrator. The drift gate runs on the two
/// partitions and **refuses the whole run** when it is out of tolerance,
/// carrying the measurement in the error. Only then is the final vector
/// measured and judged.
pub fn run_calibration(
    snapshot: &[OutcomeTrainingSample],
    model: &dyn RoutingModel,
    config: &CalibrationConfig,
) -> Result<CalibrationOutcome, CalibrationError> {
    config.checked()?;

    let cohorts = project_cohorts(snapshot, model, config.probability_floor)?;
    let needed = config
        .holdout
        .holdout_cohorts
        .saturating_add(config.holdout.min_fit_cohorts);
    if cohorts.len() < needed {
        return Err(CalibrationError::SnapshotTooSmall {
            cohorts: cohorts.len(),
            holdout: config.holdout.holdout_cohorts,
            min_fit: config.holdout.min_fit_cohorts,
        });
    }

    // The split is taken in **arrival** order, so the holdout is a later slice of
    // history rather than whichever content cluster sorted last.
    let split = cohorts.len() - config.holdout.holdout_cohorts;
    let mut fit: Vec<DecisionCohort> = cohorts[..split].to_vec();
    let mut holdout: Vec<DecisionCohort> = cohorts[split..].to_vec();
    // Each partition is then ordered on content, which is where invariance to tie
    // permutation is what matters: two cohorts that tie are indistinguishable to
    // every sum this module computes, so exchanging them cannot move a gradient,
    // a log-loss, a reliability bin, a drift bin, the base rate or the arity.
    //
    // Ordering *before* the split would satisfy that and break the property above,
    // which is why it happens here.
    for partition in [&mut fit, &mut holdout] {
        partition.sort_by(|left, right| left.order_key.cmp(&right.order_key));
        if let Some(index) = unordered_at(partition) {
            return Err(CalibrationError::UnorderedCohorts { index });
        }
    }
    let fit = fit.as_slice();
    let holdout = holdout.as_slice();
    check_partition(PartitionKind::Fit, fit, config)?;
    check_partition(PartitionKind::Holdout, holdout, config)?;

    // Drift before fitting: a calibrator fitted across a regime change and
    // reported on both sides of it is not a measurement of anything, so there
    // is no point spending a fit on it.
    let drift = measure_drift(fit, holdout, &config.drift)?;
    if !drift.is_acceptable() {
        return Err(CalibrationError::DistributionShift {
            measurement: Box::new(drift),
        });
    }

    let calibrator = KWayCalibrator::fit(fit, config, config.probability_floor)?;
    let marginal_calibrator = MarginalCalibrator::fit(
        &fit_marginal_pairs(fit, config.probability_floor)?,
        &config.marginal,
        config.probability_floor,
    )?;

    let mut emitted = Vec::with_capacity(holdout.len());
    for cohort in holdout {
        emitted.push(calibrator.distribution(cohort, config.probability_floor)?);
    }
    let final_vector = measure_emitted(&emitted, &config.reliability)?;

    let uncalibrated = KWayCalibrator::uncalibrated();
    let mut baseline = Vec::with_capacity(holdout.len());
    for cohort in holdout {
        baseline.push(uncalibrated.distribution(cohort, config.probability_floor)?);
    }
    let uncalibrated_joint = measure_emitted(&baseline, &config.reliability)?;

    let marginal_observations =
        collect_marginal_observations(holdout, &marginal_calibrator, config.probability_floor)?;
    let marginal_calibrated = measure_marginal(
        &marginal_observations,
        MarginalView::Calibrated,
        &config.reliability,
    )?;
    let marginal_normalized = measure_marginal(
        &marginal_observations,
        MarginalView::Normalized,
        &config.reliability,
    )?;
    let normalization_damage =
        NormalizationDamage::measure(&marginal_calibrated, &marginal_normalized);

    let tolerances = AcceptanceTolerances::from(&config.reliability);
    let report = CalibrationReport {
        cohorts_total: cohorts.len(),
        fit_cohorts: fit.len(),
        holdout_cohorts: holdout.len(),
        fit_attributed_cohorts: fit.iter().filter(|cohort| cohort.is_attributed()).count(),
        holdout_attributed_cohorts: holdout
            .iter()
            .filter(|cohort| cohort.is_attributed())
            .count(),
        ranked_observations: final_vector.observations,
        unranked_candidates_emitted: emitted
            .iter()
            .map(|decision| decision.unranked().len())
            .sum(),
        fit_attributed_rate: attributed_rate(fit),
        holdout_attributed_rate: attributed_rate(holdout),
        verdict: CalibrationVerdict::from_measurements(&final_vector, &tolerances),
        final_vector,
        uncalibrated_joint,
        marginal_calibrated,
        marginal_normalized,
        normalization_damage,
        drift,
        tolerances,
        distribution_role: DISTRIBUTION_ROLE.to_string(),
    };

    Ok(CalibrationOutcome {
        calibrator,
        marginal_calibrator,
        holdout: emitted,
        report,
    })
}

/// The fit partition's per-candidate `(raw probability, served)` pairs.
fn fit_marginal_pairs(
    fit: &[DecisionCohort],
    probability_floor: f64,
) -> Result<Vec<(f64, bool)>, CalibrationError> {
    let mut pairs = Vec::new();
    for cohort in fit {
        for input in cohort.candidates() {
            if let Some(raw) = input.raw_success_probability() {
                log_probability(raw, probability_floor)?;
                pairs.push((raw, cohort.served() == Some(input.candidate())));
            }
        }
    }
    if pairs.is_empty() {
        return Err(CalibrationError::NoObservations {
            context: "the fit partition's candidate marginals",
        });
    }
    Ok(pairs)
}

/// Refuse a partition that cannot support a calibration claim.
fn check_partition(
    kind: PartitionKind,
    cohorts: &[DecisionCohort],
    config: &CalibrationConfig,
) -> Result<(), CalibrationError> {
    let floor = match kind {
        PartitionKind::Fit => config.holdout.min_fit_cohorts,
        PartitionKind::Holdout => config.holdout.holdout_cohorts,
    };
    if cohorts.len() < floor {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::TooFewCohorts,
            detail: format!("{} decisions, floor {floor}", cohorts.len()),
        });
    }

    let ranked: usize = cohorts.iter().map(|cohort| cohort.ranked_count()).sum();
    if ranked == 0 {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::NoRankedCandidateAnywhere,
            detail: "no candidate in the partition carries a prediction".to_string(),
        });
    }

    let attributed: Vec<&CandidateIdentity> = cohorts
        .iter()
        .filter_map(|cohort| cohort.served())
        .collect();
    if attributed.is_empty() {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::NoAttributedOutcome,
            detail: "no decision was served, so every observed frequency would be zero".to_string(),
        });
    }
    if attributed.len() < config.holdout.min_attributed_outcomes {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::TooFewAttributedOutcomes,
            detail: format!(
                "{} served decisions, floor {}",
                attributed.len(),
                config.holdout.min_attributed_outcomes
            ),
        });
    }

    let mut winners: Vec<&CandidateIdentity> = Vec::new();
    for identity in &attributed {
        if !winners.contains(identity) {
            winners.push(identity);
        }
    }
    if winners.len() < 2 {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::SingleServedCandidate,
            detail: format!(
                "only {}/{} ever served, so the axis cannot be told apart and a uniform 1/K answer would score the base rate",
                winners[0].provider(),
                winners[0].model()
            ),
        });
    }
    Ok(())
}
