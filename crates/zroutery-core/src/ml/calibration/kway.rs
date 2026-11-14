use serde::Serialize;

use super::cohort::DecisionCohort;
use super::config::CalibrationConfig;
use super::errors::CalibrationError;
use super::measure::EmittedDecision;
use super::LOG_LOSS_CLAMP;
use crate::ml::decision_contract::{DecisionContractError, DecisionDistribution};
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// KWayCalibrator
// ---------------------------------------------------------------------------

/// One candidate's fitted intercept.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateIntercept {
    /// The candidate the intercept belongs to.
    pub candidate: CandidateIdentity,
    /// The fitted shift, in log-probability space.
    pub intercept: f64,
    /// Cohorts in the fit partition where this candidate appeared.
    pub fit_cohorts: usize,
    /// Cohorts in the fit partition where this candidate served.
    pub fit_serving_cohorts: usize,
}

/// The fitted joint K-way calibrator.
///
/// `q_i = softmax_i((ln p_i + b_i) / T)`. The sum-to-zero constraint on `b` is
/// what makes `T` and `b` separately identifiable: adding a constant to every
/// `b` and rescaling `T` leaves `q` unchanged, so without the constraint one of
/// them would be unidentifiable and the other would absorb its drift.
///
/// Nothing here is random. `iterations` is the stopping rule, there is no
/// seed, and no wall clock is read, so the same partition and configuration
/// give the same parameters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KWayCalibrator {
    temperature: f64,
    intercepts: Vec<CandidateIntercept>,
    fit_cohorts: usize,
    fit_attributed_cohorts: usize,
    fit_observations: usize,
    iterations: usize,
    intercept_l2: f64,
    initial_log_loss: f64,
    final_log_loss: f64,
}

impl KWayCalibrator {
    /// The identity joint: temperature one, no intercepts.
    ///
    /// This is the uncalibrated *joint* baseline — the same parameterization
    /// with nothing fitted — so the diagnostic comparison is measured through
    /// the same validated-type path as the claim rather than through a side
    /// channel.
    pub fn uncalibrated() -> Self {
        Self {
            temperature: 1.0,
            intercepts: Vec::new(),
            fit_cohorts: 0,
            fit_attributed_cohorts: 0,
            fit_observations: 0,
            iterations: 0,
            intercept_l2: 0.0,
            initial_log_loss: 0.0,
            final_log_loss: 0.0,
        }
    }

    /// Fit the joint calibrator on one partition.
    ///
    /// Only attributed cohorts contribute: the target is "who serves", and a
    /// request nobody served has no target. Those rows are still counted in the
    /// measurement, where they correctly push every candidate's observed
    /// frequency down.
    pub fn fit(
        cohorts: &[DecisionCohort],
        config: &CalibrationConfig,
        probability_floor: f64,
    ) -> Result<Self, CalibrationError> {
        let (indexed, attributed) = fit_index(cohorts)?;
        if attributed.is_empty() {
            return Err(CalibrationError::FitDiverged {
                reason:
                    "the fit partition holds no attributed decision, so there is no target to fit"
                        .to_string(),
            });
        }

        let mut temperature = 1.0_f64;
        let mut intercepts = vec![0.0_f64; indexed.len()];

        // The objective measured below is the **mean** log loss over the
        // attributed cohorts, so its gradient is the mean of the per-cohort
        // residuals, not their sum. Accumulating the raw sum made the step size
        // - and therefore the fitted map - a function of how many rows the
        // snapshot happened to hold: replicating the same distribution enough
        // times drove the intercepts to their clamp while the reported mean
        // loss got worse. Dividing the accumulated residual by the cohort count
        // is what makes the optimizer solve the loss that is reported, and it
        // leaves the L2 term at a fixed strength relative to that mean loss.
        let cohort_scale = 1.0 / attributed.len() as f64;

        for _ in 0..config.fit.iterations {
            let mut gradient_intercept = vec![0.0_f64; indexed.len()];
            let mut gradient_temperature = 0.0_f64;

            for cohort in &attributed {
                let scores = cohort_scores(
                    cohort,
                    &intercepts,
                    &indexed,
                    temperature,
                    probability_floor,
                );
                let Some(mass) = softmax(&scores.utility) else {
                    return Err(CalibrationError::FitDiverged {
                        reason: "the joint utility produced no finite mass".to_string(),
                    });
                };
                let Some(winner) = scores.winner else {
                    return Err(CalibrationError::FitDiverged {
                        reason: "an attributed decision has no ranked winner".to_string(),
                    });
                };
                if winner >= mass.len() {
                    return Err(CalibrationError::FitDiverged {
                        reason: "the joint mass has no slot for the observed winner".to_string(),
                    });
                }

                for (slot, &share) in mass.iter().enumerate() {
                    let residual = share - f64::from(slot == winner);
                    if let Some(global) = scores.slot_to_global[slot] {
                        gradient_intercept[global] += residual / temperature;
                    }
                    gradient_temperature -= residual * scores.utility[slot] / temperature;
                }
            }

            for (slot, value) in intercepts.iter_mut().enumerate() {
                *value -= config.fit.learning_rate
                    * (cohort_scale * gradient_intercept[slot] + config.fit.intercept_l2 * *value);
                *value = value.clamp(-config.fit.max_abs_intercept, config.fit.max_abs_intercept);
            }
            project_to_zero_mean(&mut intercepts);
            temperature = (temperature
                - config.fit.learning_rate * cohort_scale * gradient_temperature)
                .clamp(config.fit.min_temperature, config.fit.max_temperature);
        }

        // Both losses are measured the same way, on the same partition, with
        // the same code: once at the identity parameterization and once at the
        // fitted one. Neither is read from the gradient loop, so neither can be
        // an artefact of when the accumulation stopped.
        let identity = vec![0.0_f64; indexed.len()];
        let initial_loss = mean_log_loss(&attributed, &identity, &indexed, 1.0, probability_floor);
        let final_loss = mean_log_loss(
            &attributed,
            &intercepts,
            &indexed,
            temperature,
            probability_floor,
        );

        if !final_loss.is_finite() {
            return Err(CalibrationError::FitDiverged {
                reason: "the fitted joint log loss is not finite".to_string(),
            });
        }

        let observations = cohorts
            .iter()
            .map(|cohort| cohort.ranked_count())
            .sum::<usize>();
        let mut records = Vec::with_capacity(indexed.len());
        for (slot, candidate) in indexed.iter().enumerate() {
            let mut seen = 0usize;
            let mut won = 0usize;
            for cohort in cohorts {
                if cohort
                    .candidates()
                    .iter()
                    .any(|input| input.is_ranked() && input.candidate() == candidate)
                {
                    seen += 1;
                }
                if cohort.served() == Some(candidate) {
                    won += 1;
                }
            }
            records.push(CandidateIntercept {
                candidate: candidate.clone(),
                intercept: intercepts[slot],
                fit_cohorts: seen,
                fit_serving_cohorts: won,
            });
        }

        Ok(Self {
            temperature,
            intercepts: records,
            fit_cohorts: cohorts.len(),
            fit_attributed_cohorts: attributed.len(),
            fit_observations: observations,
            iterations: config.fit.iterations,
            intercept_l2: config.fit.intercept_l2,
            initial_log_loss: initial_loss,
            final_log_loss: final_loss,
        })
    }

    /// The fitted temperature.
    pub fn temperature(&self) -> f64 {
        self.temperature
    }

    /// The per-candidate intercepts, in the canonical candidate order.
    pub fn intercepts(&self) -> &[CandidateIntercept] {
        &self.intercepts
    }

    /// One candidate's intercept, or `0.0` for a candidate the fit never saw.
    ///
    /// Returning zero rather than refusing is deliberate: a candidate that
    /// appears only in the holdout has no fitted shift, and inventing one would
    /// be a claim about data the fit never saw.
    pub fn intercept_of(&self, candidate: &CandidateIdentity) -> f64 {
        self.intercepts
            .iter()
            .find(|record| &record.candidate == candidate)
            .map_or(0.0, |record| record.intercept)
    }

    /// Cohorts this calibrator was fitted on.
    pub fn fit_cohorts(&self) -> usize {
        self.fit_cohorts
    }

    /// Attributed cohorts this calibrator was fitted on.
    pub fn fit_attributed_cohorts(&self) -> usize {
        self.fit_attributed_cohorts
    }

    /// Ranked candidate observations this calibrator was fitted on.
    pub fn fit_observations(&self) -> usize {
        self.fit_observations
    }

    /// Mean multiclass log loss over the fit partition after fitting.
    pub fn final_log_loss(&self) -> f64 {
        self.final_log_loss
    }

    /// Mean multiclass log loss over the fit partition before the first step.
    pub fn initial_log_loss(&self) -> f64 {
        self.initial_log_loss
    }

    /// Whether this calibrator was fitted at all.
    pub fn is_fitted(&self) -> bool {
        self.iterations > 0
    }

    /// Emit the K-way distribution for one decision.
    ///
    /// The softmax runs over the ranked candidates only; an
    /// [`CandidateInput::Unranked`] candidate is placed at exactly `0.0`
    /// afterwards, so it takes no mass and the ranked candidates keep exactly
    /// the vector the fit gave them.
    pub fn distribution(
        &self,
        cohort: &DecisionCohort,
        probability_floor: f64,
    ) -> Result<EmittedDecision, CalibrationError> {
        let mut ranked_utility = Vec::with_capacity(cohort.ranked_count());
        for input in cohort.candidates() {
            if let Some(raw) = input.raw_success_probability() {
                let shifted =
                    log_probability(raw, probability_floor)? + self.intercept_of(input.candidate());
                ranked_utility.push(shifted / self.temperature);
            }
        }

        let Some(mass) = softmax(&ranked_utility) else {
            return Err(CalibrationError::DistributionRejected {
                cohort: cohort.fingerprint().to_string(),
                reason: "the joint utility produced no finite mass".to_string(),
            });
        };
        let total: f64 = mass.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return Err(CalibrationError::DistributionRejected {
                cohort: cohort.fingerprint().to_string(),
                reason: format!("the joint mass summed to {total}"),
            });
        }

        // The softmax covered the ranked candidates only. The unranked ones are
        // placed at exactly 0.0 here, so they take no mass and the ranked
        // candidates keep the vector the fit gave them.
        let mut probabilities = Vec::with_capacity(cohort.arity());
        let mut unranked: Vec<CandidateIdentity> = Vec::new();
        let mut cursor = 0usize;
        for input in cohort.candidates() {
            if input.is_ranked() {
                probabilities.push(mass.get(cursor).copied().unwrap_or(0.0) / total);
                cursor += 1;
            } else {
                probabilities.push(0.0);
                unranked.push(input.candidate().clone());
            }
        }

        let outcomes: Vec<CandidateIdentity> = cohort
            .candidates()
            .iter()
            .map(|input| input.candidate().clone())
            .collect();
        let distribution =
            DecisionDistribution::try_new(cohort.subject().clone(), outcomes, probabilities)
                .map_err(
                    |error: DecisionContractError| CalibrationError::DistributionRejected {
                        cohort: cohort.fingerprint().to_string(),
                        reason: error.to_string(),
                    },
                )?;

        Ok(EmittedDecision {
            cohort_fingerprint: cohort.fingerprint().to_string(),
            distribution,
            served: cohort.served().cloned(),
            unranked,
        })
    }
}

/// The candidate vocabulary of a partition, in a canonical order, plus the
/// attributed cohorts the fit may use.
fn fit_index(
    cohorts: &[DecisionCohort],
) -> Result<(Vec<CandidateIdentity>, Vec<&DecisionCohort>), CalibrationError> {
    let mut vocabulary: Vec<CandidateIdentity> = Vec::new();
    for cohort in cohorts {
        for input in cohort.candidates() {
            if input.is_ranked() {
                let identity = input.candidate().clone();
                if !vocabulary.contains(&identity) {
                    vocabulary.push(identity);
                }
            }
        }
    }
    vocabulary.sort_by(|left, right| {
        left.provider()
            .cmp(right.provider())
            .then_with(|| left.model().cmp(right.model()))
    });
    let attributed: Vec<&DecisionCohort> = cohorts
        .iter()
        .filter(|cohort| cohort.is_attributed())
        .collect();
    Ok((vocabulary, attributed))
}

/// The per-cohort quantities one gradient step needs.
struct CohortScores {
    /// The joint utility of each ranked candidate, in axis order.
    utility: Vec<f64>,
    /// Axis-order index of the observed winner among the ranked candidates.
    winner: Option<usize>,
    /// Maps a ranked slot back to its slot in the global candidate vocabulary.
    slot_to_global: Vec<Option<usize>>,
}

fn cohort_scores(
    cohort: &DecisionCohort,
    intercepts: &[f64],
    vocabulary: &[CandidateIdentity],
    temperature: f64,
    probability_floor: f64,
) -> CohortScores {
    let mut utility = Vec::with_capacity(cohort.ranked_count());
    let mut slot_to_global = Vec::with_capacity(cohort.ranked_count());
    let mut winner = None;
    for input in cohort.candidates() {
        let Some(raw) = input.raw_success_probability() else {
            continue;
        };
        let global = vocabulary
            .iter()
            .position(|identity| identity == input.candidate())
            .unwrap_or(0);
        let shift = intercepts.get(global).copied().unwrap_or(0.0);
        utility
            .push((log_probability(raw, probability_floor).unwrap_or(0.0) + shift) / temperature);
        slot_to_global.push(Some(global));
        if cohort.served() == Some(input.candidate()) {
            winner = Some(utility.len() - 1);
        }
    }
    CohortScores {
        utility,
        winner,
        slot_to_global,
    }
}

/// A numerically stable softmax. `None` when the input cannot produce a
/// distribution, which is a refusal rather than a uniform fallback.
fn softmax(utility: &[f64]) -> Option<Vec<f64>> {
    if utility.is_empty() {
        return None;
    }
    let mut largest = f64::NEG_INFINITY;
    for value in utility {
        if !value.is_finite() {
            return None;
        }
        if *value > largest {
            largest = *value;
        }
    }
    let mut mass = Vec::with_capacity(utility.len());
    let mut total = 0.0;
    for value in utility {
        let exponent = (value - largest).exp();
        if !exponent.is_finite() {
            return None;
        }
        total += exponent;
        mass.push(exponent);
    }
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    for value in &mut mass {
        *value /= total;
    }
    Some(mass)
}

/// Mean multiclass log loss of one parameterization over the attributed
/// decisions of a partition.
///
/// `None` for a decision whose utility cannot produce a mass, so a broken
/// parameterization is reported as a refusal by the caller rather than as a
/// flattering number.
fn mean_log_loss(
    attributed: &[&DecisionCohort],
    intercepts: &[f64],
    vocabulary: &[CandidateIdentity],
    temperature: f64,
    probability_floor: f64,
) -> f64 {
    if attributed.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for cohort in attributed {
        let scores = cohort_scores(
            cohort,
            intercepts,
            vocabulary,
            temperature,
            probability_floor,
        );
        if let (Some(mass), Some(winner)) = (softmax(&scores.utility), scores.winner) {
            let probability = mass.get(winner).copied().unwrap_or(0.0).max(LOG_LOSS_CLAMP);
            total -= probability.ln();
        }
    }
    total / attributed.len() as f64
}

fn project_to_zero_mean(values: &mut [f64]) {
    if values.is_empty() {
        return;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    for value in values.iter_mut() {
        *value -= mean;
    }
}

/// `ln(p / (1 - p))` for a finite probability inside `[0, 1]`.
///
/// The log-odds, not `ln p`. A Platt map is a map *on the log-odds*: feeding it
/// `ln p` instead silently changes what the fitted slope means, and the
/// resulting map is not the monotone recalibration the method describes.
pub(super) fn log_odds(raw: f64, floor: f64) -> Result<f64, CalibrationError> {
    let log_p = log_probability(raw, floor)?;
    let log_q = log_probability(1.0 - raw, floor)?;
    Ok(log_p - log_q)
}

pub(super) fn log_probability(raw: f64, floor: f64) -> Result<f64, CalibrationError> {
    if !raw.is_finite() {
        return Err(CalibrationError::NonFinitePrediction {
            cohort: String::new(),
            candidate: String::new(),
            value: raw,
        });
    }
    if !(0.0..=1.0).contains(&raw) {
        return Err(CalibrationError::PredictionOutOfRange {
            cohort: String::new(),
            candidate: String::new(),
            value: raw,
        });
    }
    Ok(raw.clamp(floor, 1.0 - floor).ln())
}
