use serde::Serialize;

use super::config::ReliabilityConfig;
use super::errors::CalibrationError;
use super::report::AcceptanceTolerances;
use super::LOG_LOSS_CLAMP;
use crate::ml::decision_contract::DecisionDistribution;
use crate::ml::evaluation::PredictionMetrics;
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// EmittedDecision / DistributionRecord
// ---------------------------------------------------------------------------

/// One emitted K-way distribution and the outcome it is scored against.
///
/// The `distribution` field is private and the type derives neither
/// `Deserialize` nor `Default`, so the only way to obtain an `EmittedDecision`
/// is to have gone through [`DecisionDistribution::try_new`]. That is the
/// structural reason the headline measurement cannot be computed from
/// pre-normalization numbers: there is no constructor that takes one.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedDecision {
    pub(super) cohort_fingerprint: String,
    pub(super) distribution: DecisionDistribution,
    pub(super) served: Option<CandidateIdentity>,
    pub(super) unranked: Vec<CandidateIdentity>,
}

impl EmittedDecision {
    /// The content fingerprint of the decision this came from.
    pub fn cohort_fingerprint(&self) -> &str {
        &self.cohort_fingerprint
    }

    /// The validated distribution.
    pub fn distribution(&self) -> &DecisionDistribution {
        &self.distribution
    }

    /// The candidates that received exactly zero mass, and why they are in the
    /// axis at all.
    pub fn unranked(&self) -> &[CandidateIdentity] {
        &self.unranked
    }

    /// The candidate that actually served.
    pub fn served(&self) -> Option<&CandidateIdentity> {
        self.served.as_ref()
    }

    /// A validating wire form.
    ///
    /// The 7E-2A type deliberately derives neither `Serialize` nor
    /// `Deserialize`, on the stated terms that the node which produces one owns
    /// giving it a wire form. This is that wire form, and it is *validating*:
    /// [`DistributionRecord::try_into_distribution`] runs the same
    /// [`DecisionDistribution::try_new`] on the way back in, so a stored record
    /// cannot re-enter the measurement path having skipped the arity and
    /// normalization checks.
    pub fn to_record(&self) -> DistributionRecord {
        DistributionRecord {
            cohort_fingerprint: self.cohort_fingerprint.clone(),
            subject: self.distribution.subject().clone(),
            outcomes: self.distribution.outcomes().to_vec(),
            probabilities: self.distribution.probabilities().to_vec(),
            total_mass: self.distribution.total_mass(),
            served: self.served.clone(),
            unranked: self.unranked.clone(),
        }
    }
}

/// The serializable form of an emitted distribution.
///
/// `Serialize` only. Deserialization goes through
/// [`DistributionRecord::try_into_distribution`], never through a derive.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DistributionRecord {
    /// The content fingerprint of the decision.
    pub cohort_fingerprint: String,
    /// The decision the distribution is about.
    pub subject: CandidateIdentity,
    /// The ordered K axis.
    pub outcomes: Vec<CandidateIdentity>,
    /// The mass on each axis entry.
    pub probabilities: Vec<f64>,
    /// The recorded total, so a reader can see the check that was run.
    pub total_mass: f64,
    /// The candidate that actually served.
    pub served: Option<CandidateIdentity>,
    /// The candidates that received exactly zero mass.
    pub unranked: Vec<CandidateIdentity>,
}

impl DistributionRecord {
    /// Rebuild the validated distribution, re-running every 7E-2A check.
    pub fn try_into_distribution(&self) -> Result<DecisionDistribution, CalibrationError> {
        DecisionDistribution::try_new(
            self.subject.clone(),
            self.outcomes.clone(),
            self.probabilities.clone(),
        )
        .map_err(|error| CalibrationError::DistributionRejected {
            cohort: self.cohort_fingerprint.clone(),
            reason: error.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Reliability
// ---------------------------------------------------------------------------

/// One bin of a reliability curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ReliabilityBin {
    /// Inclusive lower edge of the predicted-probability bin.
    pub lower: f64,
    /// Exclusive upper edge, except on the last bin.
    pub upper: f64,
    /// How many candidate observations landed here.
    pub count: usize,
    /// Mean predicted mass over the bin.
    pub mean_predicted: f64,
    /// Fraction of the bin's observations that actually served.
    pub observed_frequency: f64,
    /// `mean_predicted - observed_frequency`. Positive is over-confident.
    pub gap: f64,
}

/// A binned reliability curve with per-bin counts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReliabilityCurve {
    /// Bins across `[0, 1]`.
    pub bin_count: usize,
    /// Total observations across every bin.
    pub observations: usize,
    /// The bins, in ascending predicted-probability order.
    pub bins: Vec<ReliabilityBin>,
}

impl ReliabilityCurve {
    /// The count-weighted mean absolute gap.
    pub fn expected_calibration_error(&self) -> f64 {
        if self.observations == 0 {
            return 0.0;
        }
        self.bins
            .iter()
            .filter(|bin| bin.count > 0)
            .map(|bin| (bin.count as f64 / self.observations as f64) * bin.gap.abs())
            .sum()
    }

    /// The largest absolute gap over the populated bins, or `None` when the
    /// curve holds no observation at all.
    pub fn maximum_calibration_error(&self) -> Option<f64> {
        self.bins
            .iter()
            .filter(|bin| bin.count > 0)
            .map(|bin| bin.gap.abs())
            .fold(None, |worst: Option<f64>, gap| {
                Some(worst.map_or(gap, |current: f64| current.max(gap)))
            })
    }

    /// How many bins hold at least one observation.
    pub fn populated_bins(&self) -> usize {
        self.bins.iter().filter(|bin| bin.count > 0).count()
    }
}

/// One candidate's reliability, measured without binning.
///
/// The binned curve is the required artefact and it is what a reader expects to
/// look at, but it has a blind spot that matters here: with K candidates and
/// fewer bins than distinct masses, two candidates whose emitted masses land in
/// the same bin are *averaged into one row*, and their errors can cancel. A
/// vector that is 20 points wrong on one candidate and 20 points wrong in the
/// other direction on another can score a bin gap of zero. On a three-candidate
/// axis with ten bins that is not a hypothetical — it is what this node's own
/// fixture does to the independent route.
///
/// This table is bin-free by construction: one row per candidate, over every
/// observation of that candidate. It cannot hide a collision, and it is
/// reported beside the curve rather than instead of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateCalibration {
    /// Which candidate.
    pub candidate: CandidateIdentity,
    /// How many decisions it appeared in.
    pub observations: usize,
    /// How many of those it served.
    pub served: usize,
    /// Mean mass this candidate was given.
    pub mean_predicted: f64,
    /// Fraction of its decisions it actually served.
    pub observed_frequency: f64,
    /// `mean_predicted - observed_frequency`. Positive is over-confident.
    pub gap: f64,
}

/// The full measurement of one set of emitted distributions.
///
/// This is the headline. `pooled` is the accepted evaluator's own
/// [`PredictionMetrics`] over the flattened K axis, obtained through the
/// fallible constructor, so the probability numbers here and the numbers
/// 7D already trusts are the same arithmetic.
///
/// `PartialEq` is deliberately absent: `PredictionMetrics` does not carry one,
/// and the accepted warmup node set the same precedent by comparing the
/// serialized form instead.
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationMeasure {
    /// Decisions measured.
    pub decisions: usize,
    /// Decisions where somebody served; the denominator of the multiclass loss.
    pub attributed_decisions: usize,
    /// Candidate observations across the K axis.
    pub observations: usize,
    /// The binned reliability curve.
    pub curve: ReliabilityCurve,
    /// The bin-free per-candidate table.
    pub per_candidate: Vec<CandidateCalibration>,
    /// The accepted per-candidate probability metrics.
    pub pooled: PredictionMetrics,
    /// Mean `-ln q` on the observed winner. `None` when nothing was served.
    pub multiclass_log_loss: Option<f64>,
}

impl CalibrationMeasure {
    /// The count-weighted mean absolute gap.
    pub fn expected_calibration_error(&self) -> f64 {
        self.curve.expected_calibration_error()
    }

    /// The largest absolute bin gap, or `None` on an empty curve.
    pub fn maximum_calibration_error(&self) -> Option<f64> {
        self.curve.maximum_calibration_error()
    }

    /// The largest absolute per-candidate gap. Bin-free, so it cannot be
    /// averaged away by two candidates sharing a bin.
    pub fn maximum_candidate_calibration_error(&self) -> Option<f64> {
        self.per_candidate
            .iter()
            .filter(|row| row.observations > 0)
            .map(|row| row.gap.abs())
            .fold(None, |worst: Option<f64>, gap| {
                Some(worst.map_or(gap, |current: f64| current.max(gap)))
            })
    }

    /// The pooled Brier score, or `None` when it was not computable.
    pub fn brier_score(&self) -> Option<f64> {
        self.pooled.brier_score
    }

    /// The whole acceptance rule, as a pure function of this measurement and
    /// the ceilings.
    ///
    /// The same three-ceiling rule the verdict uses, exposed so a reader can
    /// apply it to a *marginal* measurement and see for themselves that the
    /// per-candidate ceiling is what refuses a vector the bins would have
    /// accepted. It is a method on both measurement types precisely so the two
    /// can be compared under one rule.
    pub fn passes(&self, tolerances: &AcceptanceTolerances) -> bool {
        matches!(
            (self.maximum_calibration_error(), self.maximum_candidate_calibration_error()),
            (Some(worst_bin), Some(worst_candidate))
                if self.expected_calibration_error() <= tolerances.max_expected_calibration_error
                    && worst_bin <= tolerances.max_calibration_error
                    && worst_candidate <= tolerances.max_candidate_calibration_error
        )
    }

    /// The pooled per-candidate log loss.
    pub fn log_loss(&self) -> Option<f64> {
        self.pooled.log_loss
    }

    /// Mean predicted mass over the K axis.
    pub fn mean_predicted(&self) -> f64 {
        self.pooled.mean_prediction
    }

    /// Fraction of candidate observations that served.
    pub fn observed_positive_rate(&self) -> f64 {
        self.pooled.mean_actual
    }
}

/// Measure the calibration of emitted distributions.
///
/// The only input is [`EmittedDecision`], and the only way to build one is
/// through [`DecisionDistribution::try_new`]. This function therefore cannot
/// be handed pre-normalization per-candidate numbers: the API does not exist,
/// and the marginal measurements it would need live in a different type it does
/// not accept.
pub fn measure_emitted(
    decisions: &[EmittedDecision],
    config: &ReliabilityConfig,
) -> Result<CalibrationMeasure, CalibrationError> {
    if config.bin_count == 0 {
        return Err(CalibrationError::ZeroReliabilityBins {
            bins: config.bin_count,
        });
    }
    if decisions.is_empty() {
        return Err(CalibrationError::NoObservations {
            context: "the emitted distribution",
        });
    }

    let mut predictions: Vec<f64> = Vec::new();
    let mut actuals: Vec<bool> = Vec::new();
    let mut attributed = 0usize;
    let mut multiclass_loss = 0.0_f64;

    for decision in decisions {
        let distribution = decision.distribution();
        for (index, outcome) in distribution.outcomes().iter().enumerate() {
            let Some(&mass) = distribution.probabilities().get(index) else {
                return Err(CalibrationError::Measurement {
                    context: "the emitted distribution",
                    reason:
                        "the validated distribution has no probability for one of its own outcomes"
                            .to_string(),
                });
            };
            predictions.push(mass);
            actuals.push(decision.served() == Some(outcome));
        }
        if let Some(served) = decision.served() {
            attributed += 1;
            let mass = distribution
                .probability_of(served)
                .unwrap_or(0.0)
                .max(LOG_LOSS_CLAMP);
            multiclass_loss -= mass.ln();
        }
    }

    let pooled =
        PredictionMetrics::try_compute_classification(&predictions, &actuals).map_err(|error| {
            CalibrationError::Measurement {
                context: "the emitted distribution",
                reason: error.to_string(),
            }
        })?;

    Ok(CalibrationMeasure {
        decisions: decisions.len(),
        attributed_decisions: attributed,
        observations: predictions.len(),
        curve: build_curve(&predictions, &actuals, config.bin_count),
        per_candidate: build_candidate_rows(decisions),
        pooled,
        multiclass_log_loss: (attributed > 0).then_some(multiclass_loss / attributed as f64),
    })
}

/// The bin-free per-candidate table.
///
/// Walks the axis of every decision, so a candidate that appears in some
/// decisions and not others is reported over the decisions it actually appeared
/// in, and the counts say so.
fn build_candidate_rows(decisions: &[EmittedDecision]) -> Vec<CandidateCalibration> {
    let mut order: Vec<CandidateIdentity> = Vec::new();
    let mut rows: Vec<CandidateCalibration> = Vec::new();

    for decision in decisions {
        let distribution = decision.distribution();
        for (index, outcome) in distribution.outcomes().iter().enumerate() {
            let mass = distribution
                .probabilities()
                .get(index)
                .copied()
                .unwrap_or(0.0);
            let served = decision.served() == Some(outcome);
            let slot = match order.iter().position(|known| known == outcome) {
                Some(slot) => slot,
                None => {
                    order.push(outcome.clone());
                    rows.push(CandidateCalibration {
                        candidate: outcome.clone(),
                        observations: 0,
                        served: 0,
                        mean_predicted: 0.0,
                        observed_frequency: 0.0,
                        gap: 0.0,
                    });
                    rows.len() - 1
                }
            };
            let row = &mut rows[slot];
            row.observations += 1;
            row.mean_predicted += mass;
            if served {
                row.served += 1;
            }
        }
    }

    for row in &mut rows {
        if row.observations > 0 {
            row.mean_predicted /= row.observations as f64;
            row.observed_frequency = row.served as f64 / row.observations as f64;
            row.gap = row.mean_predicted - row.observed_frequency;
        }
    }
    // A canonical candidate order, so two runs serialize identically.
    rows.sort_by(|left, right| {
        left.candidate
            .provider()
            .cmp(right.candidate.provider())
            .then_with(|| left.candidate.model().cmp(right.candidate.model()))
    });
    rows
}

pub(super) fn build_curve(
    predictions: &[f64],
    actuals: &[bool],
    bin_count: usize,
) -> ReliabilityCurve {
    let bins = bin_count.max(1);
    let mut counts = vec![0usize; bins];
    let mut predicted_sum = vec![0.0_f64; bins];
    let mut positive = vec![0usize; bins];

    for (prediction, actual) in predictions.iter().zip(actuals) {
        let slot = bin_index(*prediction, bins);
        counts[slot] += 1;
        predicted_sum[slot] += *prediction;
        if *actual {
            positive[slot] += 1;
        }
    }

    let width = 1.0 / bins as f64;
    let entries = counts
        .iter()
        .enumerate()
        .map(|(slot, &count)| {
            let lower = slot as f64 * width;
            let upper = if slot + 1 == bins { 1.0 } else { lower + width };
            if count == 0 {
                ReliabilityBin {
                    lower,
                    upper,
                    count: 0,
                    mean_predicted: 0.0,
                    observed_frequency: 0.0,
                    gap: 0.0,
                }
            } else {
                let mean_predicted = predicted_sum[slot] / count as f64;
                let observed_frequency = positive[slot] as f64 / count as f64;
                ReliabilityBin {
                    lower,
                    upper,
                    count,
                    mean_predicted,
                    observed_frequency,
                    gap: mean_predicted - observed_frequency,
                }
            }
        })
        .collect();

    ReliabilityCurve {
        bin_count: bins,
        observations: predictions.len(),
        bins: entries,
    }
}

pub(super) fn bin_index(value: f64, bins: usize) -> usize {
    // A `NaN` prediction lands in the first bin rather than reaching the cast
    // below, and the accepted fallible metric constructor is what keeps a `NaN`
    // out of the vectors this is called with in the first place.
    if value.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return 0;
    }
    if value >= 1.0 {
        return bins - 1;
    }
    let slot = (value * bins as f64).floor() as usize;
    if slot >= bins {
        bins - 1
    } else {
        slot
    }
}
