//! Node 7D — the statistical release methodology.
//!
//! # The claim being tested
//!
//! One sentence: **the emitted distribution ranks the candidate that actually
//! served more often than the strongest decision-independent policy does, on
//! these same decisions, by more than the stated minimum, with an interval that
//! excludes the null.** Everything in this module exists to make that sentence
//! checkable, or to refuse it.
//!
//! # The design, and what was rejected
//!
//! **The unit of independence is the decision.** See [`crate::ml::attribution`]
//! for the arithmetic. The consequence here is that every sufficient statistic
//! is an integer count over decisions, and every `f64` in the output is a
//! closed-form function of two integers. Nothing sums floats over a row axis,
//! so the bug class where floating-point summation order follows a hash
//! container's iteration order cannot arise here at all — it is not sorted
//! around, it is absent.
//!
//! **The baseline is the strongest uninformed policy, not a strawman.** The
//! comparator is a *fixed* policy: always serve the candidate that won the most
//! holdout decisions. A fixed policy is decision-independent by
//! construction, so it achieves exactly `served(c*)` hits on these very
//! decisions, and a model with no skill **ties** it. Beating it is therefore a
//! real claim. 7E-2D's unconditional base rate is consumed alongside it, its
//! per-candidate counts are cross-checked against the counts measured here (a
//! mismatch is a typed refusal), and its figure is carried into the output —
//! but the *selection* criterion is the hit count, because the policy's score
//! **is** a count. See [`BaselinePolicy`].
//!
//! **McNemar's exact conditional test, not a row-level proportion test.** The
//! two arms are evaluated on the same decisions, so the design is paired and
//! the per-decision difficulty is common to both. An unpaired two-proportion
//! test discards that pairing and is anti-conservative in the presence of
//! decision-level heterogeneity; a chi-squared approximation is invalid at the
//! small discordant counts an under-powered run produces; a bootstrap is
//! unnecessary because the decisions are i.i.d. draws and the exact conditional
//! test is available in closed form.
//!
//! **No resampling anywhere, therefore no seed.** Every number here is
//! analytic. `rand` appears nowhere in this file, so nothing depends on
//! `StdRng`'s algorithm staying stable across crate versions, and there is no
//! seed to record or replay. Where a bootstrap would have been the
//! alternative — the interval and the p-value — a closed form exists, so
//! resampling would add Monte-Carlo error and a cross-version reproducibility
//! hazard for nothing.
//!
//! **Wald on the paired difference, stated as the assumption it is.** The
//! interval is `θ̂ ± z_{1-α/2}·√(n₁₀+n₀₁)/n`. That is a large-sample normal
//! approximation, it is poor at small `n`, and it is *not* hidden: the adequacy
//! criterion is evaluated first and refuses before the approximation is ever
//! asked to carry a decision. The two marginal rates are additionally reported
//! with Wilson score intervals, which behave well at small `n`, so a reader
//! sees both the paired claim and the absolute rates.
//!
//! **Family-wise error is controlled, and the family is named.** Testing `K`
//! candidates is `K` opportunities to be wrong by chance, and an uncorrected
//! family-wise error rate is a defect rather than a simplification.
//! Holm–Bonferroni is used because it is exact under arbitrary dependence —
//! which matters, since the per-candidate tests share decisions and are
//! emphatically not independent — needs no variance estimate, and reduces to
//! Bonferroni at the worst case. The family is [`Family`], its size is
//! printed, and every member's raw and adjusted p-value is in the output.
//!
//! # What the p-value is reported as
//!
//! Both `p_value` and `log_p_value` are carried. The exact conditional tail is
//! computed entirely in log space, so a p-value far below the smallest
//! representable positive `f64` is reported as `0.0` in the linear field and
//! with its true magnitude in the logarithmic one. Reporting only the linear
//! field would claim a p-value of exactly zero, which is never true; reporting
//! only the log would make a reader do arithmetic before they could judge it.

use serde::Serialize;

use crate::ml::attribution::{
    attribute, content_order, label, AttributionError, CandidateCredit, CreditLedger,
    DecisionOutcome, Independence,
};
use crate::ml::calibration::{CandidateCalibration, DecisionCohort, EmittedDecision};
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// What the statistical part of a release verdict is, and what it is not.
///
/// 7E-3's [`crate::ml::offline_gate::RELEASE_SCOPE`] is the scope of the replay
/// and integrity constituents. This is the scope of the statistical one, carried
/// separately on the report so that neither string over-reads the other.
pub const STATISTICAL_SCOPE: &str = "whether the frozen holdout supports the claim that the \
emitted distribution ranks the candidate that actually served more often, and by more than the \
stated minimum, than the strongest decision-independent policy, on the SAME decisions, at the \
stated level, with the family of tests stated and corrected; the unit of independence is the \
DECISION, and the raw candidate-row count is reported beside the effective decision count; it is \
NOT a claim about online traffic, NOT a claim that the effect will persist, NOT a claim that the \
model is calibrated, and NOT a claim that the model can be served";

// ---------------------------------------------------------------------------
// StatisticalConfig
// ---------------------------------------------------------------------------

/// The claim specification. Every number the verdict rests on.
///
/// This is not a switch. There is no value of this type that turns the gate
/// off, and a configuration that would make the claim vacuous is refused by
/// [`StatisticalConfig::checked`] rather than reported. What varies here is the
/// *claim*: what level of evidence, what power, and what smallest effect is
/// worth releasing for. Those are the caller's honest requirements, and they
/// are printed into the output so a reader can see what was actually demanded.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StatisticalConfig {
    /// Two-sided significance level for the headline claim. `0.05` means a 95%
    /// interval.
    pub alpha: f64,
    /// The power the required sample size is computed for. `0.80` is the
    /// conventional floor; a release claim above that is stronger, not weaker.
    pub power: f64,
    /// The smallest paired risk difference the claim is allowed to rest on.
    ///
    /// This is the **minimum detectable effect** and it enters twice: it is the
    /// `δ` in the power formula, and it is the floor the interval's lower bound
    /// must clear. Both uses are the same claim, so both move together.
    pub minimum_effect: f64,
    /// A hard floor on decisions, applied before the power calculation.
    ///
    /// The power formula already refuses small samples; this is the separate
    /// statement that no amount of configurability should let a two-decision
    /// partition be tested.
    pub min_decisions: usize,
    /// A defensive ceiling on the enumerated family.
    pub max_family: usize,
}

impl Default for StatisticalConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            power: 0.80,
            minimum_effect: 0.05,
            min_decisions: 30,
            max_family: 64,
        }
    }
}

impl StatisticalConfig {
    /// Every configuration that would make the claim meaningless.
    ///
    /// Checked before anything is read. A level of zero or one admits no
    /// evidence and demands total evidence respectively; a power of one asks
    /// for an infinite sample and would report a permanent refusal as though it
    /// were a measurement; a non-positive or unit minimum effect would let the
    /// interval criterion clear trivially and make it vacuous; a zero family
    /// ceiling would make the family un-enumerable.
    pub fn checked(&self) -> Result<(), StatisticsError> {
        if !self.alpha.is_finite() || self.alpha <= 0.0 || self.alpha >= 1.0 {
            return Err(StatisticsError::InvalidClaim {
                field: "alpha",
                value: self.alpha,
                reason: "the significance level must be finite and strictly between 0 and 1",
            });
        }
        if !self.power.is_finite() || self.power <= 0.0 || self.power >= 1.0 {
            return Err(StatisticsError::InvalidClaim {
                field: "power",
                value: self.power,
                reason: "the power must be finite and strictly between 0 and 1",
            });
        }
        if !self.minimum_effect.is_finite()
            || self.minimum_effect <= 0.0
            || self.minimum_effect >= 1.0
        {
            return Err(StatisticsError::InvalidClaim {
                field: "minimum_effect",
                value: self.minimum_effect,
                reason: "the minimum effect must be finite and strictly between 0 and 1, or the \
                         interval criterion would be vacuous",
            });
        }
        if self.min_decisions == 0 {
            return Err(StatisticsError::InvalidClaim {
                field: "min_decisions",
                value: 0.0,
                reason: "the decision floor must be at least 1",
            });
        }
        if self.max_family == 0 {
            return Err(StatisticsError::InvalidClaim {
                field: "max_family",
                value: 0.0,
                reason: "the family ceiling must be at least 1",
            });
        }
        // The level has to survive the rounding that gets it from `alpha`, or the
        // gate is asking for an interval with no finite upper bound. `alpha` of
        // 1e-30 is finite and strictly between zero and one, so it passes the
        // checks above, but `1 - alpha/2` rounds to exactly `1.0` in f64 — and a
        // normal quantile at 1.0 is infinite, which would poison every bound
        // downstream. Refused here, with a message that says why, rather than
        // surfacing later as a non-finite measurement.
        let level = self.level();
        if !level.is_finite() || level >= 1.0 || level <= 0.5 {
            return Err(StatisticsError::InvalidClaim {
                field: "alpha",
                value: self.alpha,
                reason: "the two-sided confidence level must be strictly above one half and \
                         strictly below one; a level that rounds to one has no upper bound to \
                         report",
            });
        }
        Ok(())
    }

    /// The two-sided confidence level, `1 - alpha/2`.
    ///
    /// `1 - alpha` would be the level for a *one-sided* claim, and using it for
    /// a two-sided interval would report a level it does not have. The
    /// distinction is carried in the field so an interval and the confidence
    /// attached to it cannot be separated by whoever serialises the
    /// measurement.
    pub fn level(&self) -> f64 {
        1.0 - self.alpha / 2.0
    }
}

// ---------------------------------------------------------------------------
// StatisticsError
// ---------------------------------------------------------------------------

/// Every way the statistical gate refuses.
///
/// Grouped in the order the gate reaches them, which is the order of the
/// claims. A refusal is a typed value carrying the numbers or names that caused
/// it. There is no `unwrap_or_default`, no zero-filled measurement, and no path
/// that reports an interval it could not compute.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize)]
pub enum StatisticsError {
    /// The claim itself is vacuous.
    #[error("the claim specification is vacuous: {field} is {value}, but {reason}")]
    InvalidClaim {
        /// Which field.
        field: &'static str,
        /// What it was.
        value: f64,
        /// Why it cannot be a claim.
        reason: &'static str,
    },

    /// The partition is too small to make the claim.
    #[error(
        "the holdout partition holds {observed} effective decisions, which cannot support the \
         claim; {required} are required because {reason}"
    )]
    SampleTooSmall {
        /// Decisions actually measured.
        observed: usize,
        /// Decisions required.
        required: usize,
        /// Which requirement, and why it exists.
        reason: &'static str,
    },

    /// The family cannot be enumerated.
    #[error("the family of tests cannot be enumerated: {reason}")]
    FamilyNotEnumerable {
        /// Why.
        reason: String,
    },

    /// The interval cannot be computed.
    #[error("the interval cannot be computed: {reason}")]
    IntervalNotComputable {
        /// Why.
        reason: String,
    },

    /// A number that feeds a decision is not finite.
    #[error("{context} is {value}, which is not finite")]
    NonFiniteMeasurement {
        /// Which measurement.
        context: &'static str,
        /// The offending value.
        value: f64,
    },

    /// The baseline is null, absent, or unusable.
    #[error("the unconditional baseline is unusable: {reason}")]
    BaselineUnavailable {
        /// Why.
        reason: String,
    },

    /// The ledger's own invariants did not hold.
    ///
    /// A defensive check on the walk's arithmetic. It is typed rather than a
    /// panic or a `debug_assert`, because a release gate that aborts a process
    /// on an internal inconsistency is a worse failure mode than one that
    /// records the inconsistency and withholds.
    #[error("the credit ledger is internally inconsistent: {detail}")]
    LedgerInconsistent {
        /// What did not hold.
        detail: String,
    },

    /// The attribution route refused. Carried whole, never flattened.
    #[error("attempt-level attribution refused: {0}")]
    Attribution(Box<AttributionError>),
}

impl StatisticsError {
    /// A stable machine-readable code.
    ///
    /// For an [`StatisticsError::Attribution`] the attribution's own code is
    /// carried through, so a reader is told `served_absent_from_axis` rather
    /// than a generic attribution failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidClaim { .. } => "invalid_claim",
            Self::SampleTooSmall { .. } => "sample_too_small",
            Self::FamilyNotEnumerable { .. } => "family_not_enumerable",
            Self::IntervalNotComputable { .. } => "interval_not_computable",
            Self::NonFiniteMeasurement { .. } => "non_finite_measurement",
            Self::BaselineUnavailable { .. } => "baseline_unavailable",
            Self::LedgerInconsistent { .. } => "ledger_inconsistent",
            Self::Attribution(error) => error.code(),
        }
    }

    /// The wrapped attribution error, if this is one.
    pub fn as_attribution(&self) -> Option<&AttributionError> {
        match self {
            Self::Attribution(error) => Some(error),
            _ => None,
        }
    }
}

/// The serializable face of a refusal.
///
/// [`StatisticsError`] is the `Err` half of the fallible entry point;
/// [`StatisticalRelease::Refused`] is the same information carried *inside* a
/// measurement. A release gate has to keep producing its report when the
/// statistical claim cannot be measured, so the refusal is recorded rather than
/// thrown away, and the conversion is total and lossy in nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatisticalRefusal {
    /// [`StatisticsError::code`].
    pub code: &'static str,
    /// The full message, reasons and numbers included.
    pub reason: String,
}

impl From<&StatisticsError> for StatisticalRefusal {
    fn from(error: &StatisticsError) -> Self {
        Self {
            code: error.code(),
            reason: error.to_string(),
        }
    }
}

/// The single blocker label a refusal contributes.
///
/// One label for every refusal, because a refusal's *specificity* is its typed
/// code and its full message, both of which are carried on
/// [`StatisticalRelease::reasons`] and in the serialized measurement. A label
/// here would have to be built at runtime to name the code, and this list is
/// `&'static str` by design so a caller can match on it.
pub const UNMEASURABLE_LABEL: &str = "the statistical claim could not be measured at all";

// ---------------------------------------------------------------------------
// Interval and comparison types
// ---------------------------------------------------------------------------

/// A confidence interval, at a stated level.
///
/// The level is a field rather than a doc comment so that a number and the
/// confidence attached to it cannot be separated by whoever serialises the
/// measurement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Interval {
    /// The lower bound.
    pub lower: f64,
    /// The upper bound.
    pub upper: f64,
    /// The two-sided confidence level, `1 - alpha/2`.
    pub level: f64,
}

impl Interval {
    /// Whether the interval excludes the null on the lower side, i.e. whether
    /// its lower bound is strictly above `null`.
    pub fn excludes(&self, null: f64) -> bool {
        self.lower > null
    }

    /// Whether the interval contains `null`, inclusive of its bounds.
    pub fn straddles(&self, null: f64) -> bool {
        self.lower <= null && null <= self.upper
    }
}

/// One paired comparison between the model and a comparator, on the same
/// decisions.
///
/// The four cells are the whole of the evidence. `model_only` and
/// `baseline_only` are the discordant pairs, and they are the only cells that
/// carry information about a difference: a decision both arms got right, or
/// both got wrong, says nothing about which arm is better and is counted only
/// so the totals reconcile against the decision count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairedComparison {
    /// What is being compared, in words.
    pub label: String,
    /// Decisions where the two arms agreed, whether both scored or neither did.
    pub concordant: usize,
    /// Decisions where this arm scored and the comparator did not. `n10`.
    pub model_only: usize,
    /// Decisions where the comparator scored and this arm did not. `n01`.
    pub baseline_only: usize,
    /// The decisions the comparison is over. The unit of independence.
    pub decisions: usize,
    /// The paired risk difference, `(n10 - n01) / n`.
    pub effect: f64,
    /// The standard error of the paired difference, `sqrt(n10 + n01) / n`.
    pub standard_error: f64,
    /// The interval on `effect`.
    pub interval: Interval,
    /// `(n10 + n01) / n`. The proportion of decisions on which the two arms
    /// disagree, and the `psi` of the power formula.
    pub discordance_rate: f64,
    /// The exact two-sided conditional binomial p-value, uncorrected.
    ///
    /// Exactly `0.0` when the true value is below the smallest representable
    /// positive `f64`. Read [`PairedComparison::log_p_value`] alongside it.
    pub p_value: f64,
    /// The natural logarithm of the exact p-value, and the authoritative field
    /// when `p_value` has underflowed.
    pub log_p_value: f64,
    /// The Holm-adjusted p-value within the family. Equal to `p_value` for a
    /// family of one.
    pub adjusted_p_value: f64,
}

impl PairedComparison {
    /// The probability that this many of `decisions` was observed at least this
    /// extreme, in the direction of the observed effect, under the null.
    ///
    /// Not a two-sided probability: the family-wise correction is what
    /// addresses multiplicity, and doubling the tail on top of that would be
    /// correcting twice for the same thing.
    pub fn one_sided_p(&self) -> f64 {
        if self.model_only > self.baseline_only {
            1.0
        } else {
            (self.log_p_value + LN_2).exp().min(1.0)
        }
    }
}

// ---------------------------------------------------------------------------
// Criterion
// ---------------------------------------------------------------------------

/// One of the three things the gate requires, as a measurement.
///
/// Deliberately carries **numbers only**. `satisfied` is a function of the
/// measurement, so there is no boolean anywhere in this type that could disagree
/// with the arithmetic — the property 7E-3 established for its verdict, and the
/// one 7D has to keep for its own.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "criterion")]
pub enum Criterion {
    /// There are enough decisions for the power calculation to mean anything.
    Adequacy {
        /// Decisions measured.
        observed: usize,
        /// Decisions required for the stated power and effect.
        required: usize,
    },
    /// The interval on the effect clears the minimum, and therefore does not
    /// straddle the null.
    IntervalExcludesMinimum {
        /// The point estimate.
        effect: f64,
        /// The interval's lower bound.
        lower: f64,
        /// The interval's upper bound.
        upper: f64,
        /// The stated two-sided level.
        level: f64,
    },
    /// The aggregate test survives the family-wise correction.
    FamilyWiseSignificance {
        /// How many tests were corrected together.
        family_size: usize,
        /// The uncorrected p-value.
        p_value: f64,
        /// The natural logarithm of it, so an underflowed p is still readable.
        log_p_value: f64,
        /// The corrected p-value.
        adjusted_p_value: f64,
    },
}

impl Criterion {
    /// Which of the three requirements this is.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Adequacy { .. } => "sample_size_adequacy",
            Self::IntervalExcludesMinimum { .. } => "interval_excludes_the_minimum_effect",
            Self::FamilyWiseSignificance { .. } => "family_wise_significance",
        }
    }

    /// The blocker label, in words, as a `&'static str`.
    ///
    /// Kept apart from [`Criterion::name`] so a caller matching on machine
    /// names and a reader reading a blocker list get the granularity each
    /// wants: three distinct labels, so a list says *which* requirement failed.
    pub const fn blocker_label(&self) -> &'static str {
        match self {
            Self::Adequacy { .. } => "the holdout holds too few decisions to support the \
                                     statistical claim at the stated power and minimum effect",
            Self::IntervalExcludesMinimum { .. } => "the effect's interval does not exclude the \
                                                     null at the stated minimum effect",
            Self::FamilyWiseSignificance { .. } => "the aggregate effect is not significant after \
                                                   correcting the stated family of tests",
        }
    }

    /// Whether this requirement is met, recomputed from the measurement.
    pub fn satisfied(&self, config: &StatisticalConfig) -> bool {
        match self {
            Self::Adequacy { observed, required } => observed >= required,
            Self::IntervalExcludesMinimum { lower, .. } => *lower > config.minimum_effect,
            Self::FamilyWiseSignificance {
                adjusted_p_value, ..
            } => *adjusted_p_value <= config.alpha,
        }
    }

    /// The numbers, as a sentence.
    pub fn detail(&self, config: &StatisticalConfig) -> String {
        match self {
            Self::Adequacy { observed, required } => format!(
                "{observed} effective decisions against {required} required for {} power at \
                 alpha {} to detect a paired difference of {}",
                config.power, config.alpha, config.minimum_effect
            ),
            Self::IntervalExcludesMinimum {
                effect,
                lower,
                upper,
                level,
            } => {
                if *lower > config.minimum_effect {
                    format!(
                        "effect {effect:.6} with a {level} interval [{lower:.6}, {upper:.6}], \
                         above the required minimum {}",
                        config.minimum_effect
                    )
                } else {
                    format!(
                        "effect {effect:.6} with a {level} interval [{lower:.6}, {upper:.6}], \
                         which does not exclude the null at the required minimum {}",
                        config.minimum_effect
                    )
                }
            }
            Self::FamilyWiseSignificance {
                family_size,
                p_value,
                log_p_value,
                adjusted_p_value,
            } => {
                let magnitude = if *p_value > 0.0 {
                    format!("p = {p_value:.8}")
                } else {
                    format!("p = exp({log_p_value:.4})")
                };
                format!(
                    "aggregate {magnitude}, Holm-adjusted over a family of {family_size} to \
                     {adjusted_p_value:.8}, against alpha {}",
                    config.alpha
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BaselinePolicy
// ---------------------------------------------------------------------------

/// The comparator, and why it is a real one.
///
/// The policy is: *always serve this one candidate, whatever the decision.* It
/// is decision-independent, so it is exactly the policy a router with no
/// information about the decision should follow — and it is the **strongest**
/// such policy on this holdout, because a fixed policy's score is precisely the
/// number of decisions that candidate won and nothing else.
///
/// The consequence is the important part: a model with no skill **ties** this
/// comparator exactly, rather than losing to it. Any procedure that let a
/// no-skill model through would therefore be a broken procedure, and the suite
/// carries a test for exactly that.
///
/// 7E-2D's unconditional base rate is consumed, cross-checked and carried, but
/// it is not the selection criterion. The reason is worth stating rather than
/// leaving implicit: a fixed policy's score is a **count**, so `wins` is the
/// right quantity to maximise. Where every candidate appears in every decision
/// the two criteria order the candidates identically; where they differ, `wins`
/// picks the stronger baseline, which is the safe direction to err in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BaselinePolicy {
    /// The candidate this policy always serves.
    pub candidate: CandidateIdentity,
    /// The decisions this candidate won, which is the policy's score.
    pub wins: usize,
    /// The decisions the policy was scored over.
    pub decisions: usize,
    /// `wins / decisions`.
    pub rate: f64,
    /// A Wilson score interval on `rate`, at the stated level.
    ///
    /// Wilson rather than Wald because a small-`n` proportion is exactly where
    /// Wald misbehaves. Closed form, no iteration, no RNG.
    pub interval: Interval,
    /// 7E-2D's observation count for this candidate, carried verbatim.
    pub base_rate_observations: usize,
    /// 7E-2D's unconditional base rate, carried verbatim.
    pub unconditional_base_rate: Option<f64>,
    /// How many candidates were available to be the baseline.
    pub candidates_considered: usize,
}

// ---------------------------------------------------------------------------
// Family
// ---------------------------------------------------------------------------

/// Which member of the family a comparison belongs to.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "candidate")]
pub enum FamilyMemberKind {
    /// The headline decision-level test: the model against the baseline policy.
    Aggregate,
    /// One paired test for a candidate the model ranked first at least once.
    Candidate(CandidateIdentity),
}

/// One member of the family and its comparison.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FamilyMember {
    /// Which member.
    pub member: FamilyMemberKind,
    /// Its comparison, with the Holm-adjusted p-value already applied.
    pub comparison: PairedComparison,
}

/// The family, stated.
///
/// The family is the aggregate test plus one paired test per candidate the model
/// ranked first at least once. That is the honest closure: those are exactly
/// the hypotheses the routing claim rests on, and nothing else was tested, so
/// nothing else can be reported as though it had been corrected.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Family {
    /// The enumeration rule, as a sentence.
    pub rule: String,
    /// The correction applied.
    pub correction: &'static str,
    /// How many tests are corrected together.
    pub size: usize,
    /// The members, aggregate first then candidates in content order.
    pub members: Vec<FamilyMember>,
}

// ---------------------------------------------------------------------------
// EvidenceSupport
// ---------------------------------------------------------------------------

/// A measured statistical claim.
///
/// Every constituent the verdict rests on is a public field, and the verdict is
/// recomputed from them by [`EvidenceSupport::is_supported`]. There is no stored
/// boolean that a caller could set and have the gate believe.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceSupport {
    /// The claim specification actually applied.
    pub config: StatisticalConfig,
    /// Raw row count, effective decision count, and the inflation between them.
    pub independence: Independence,
    /// The per-candidate credit ledger, and the decision-level sufficient
    /// statistic every comparison is computed from.
    pub credit: CreditLedger,
    /// The comparator and its own measured performance.
    pub baseline: BaselinePolicy,
    /// The headline paired comparison.
    pub aggregate: PairedComparison,
    /// The family, with the correction applied.
    pub family: Family,
    /// The decisions the power calculation required.
    pub required_decisions: usize,
    /// The `z` for `1 - alpha/2`, so the interval can be re-derived.
    pub z_two_sided: f64,
    /// The `z` for the configured power, so the required `n` can be
    /// re-derived.
    pub z_power: f64,
}

impl EvidenceSupport {
    /// The three requirements, as measurements. Recomputed, never stored.
    pub fn criteria(&self) -> Vec<Criterion> {
        vec![
            Criterion::Adequacy {
                observed: self.independence.effective_decisions,
                required: self.required_decisions,
            },
            Criterion::IntervalExcludesMinimum {
                effect: self.aggregate.effect,
                lower: self.aggregate.interval.lower,
                upper: self.aggregate.interval.upper,
                level: self.aggregate.interval.level,
            },
            Criterion::FamilyWiseSignificance {
                family_size: self.family.size,
                p_value: self.aggregate.p_value,
                log_p_value: self.aggregate.log_p_value,
                adjusted_p_value: self.aggregate.adjusted_p_value,
            },
        ]
    }

    /// Whether the evidence supports the claim, recomputed from the criteria.
    pub fn is_supported(&self) -> bool {
        self.criteria()
            .iter()
            .all(|criterion| criterion.satisfied(&self.config))
    }

    /// Every constituent that withholds, as stable labels in a fixed order.
    ///
    /// The measured half of [`StatisticalRelease::blockers`], and derived from
    /// the same criteria in the same order, so the two cannot disagree.
    pub fn blockers(&self) -> Vec<&'static str> {
        self.criteria()
            .iter()
            .filter(|criterion| !criterion.satisfied(&self.config))
            .map(Criterion::blocker_label)
            .collect()
    }

    /// Every requirement that is not met, as sentences carrying the numbers.
    pub fn reasons(&self) -> Vec<String> {
        self.criteria()
            .iter()
            .filter(|criterion| !criterion.satisfied(&self.config))
            .map(|criterion| {
                format!(
                    "{}: {}",
                    criterion.name(),
                    criterion.detail(&self.config)
                )
            })
            .collect()
    }

    /// The headline, in one line.
    pub fn headline(&self) -> String {
        let inflation = self
            .independence
            .inflation
            .map_or_else(|| "unknown".to_string(), |value| format!("{value:.3}"));
        let magnitude = if self.aggregate.p_value > 0.0 {
            format!("{:.8}", self.aggregate.p_value)
        } else {
            format!("exp({:.4})", self.aggregate.log_p_value)
        };
        format!(
            "statistical support={}; effect {effect:.6} with a {level} interval [{lower:.6}, \
             {upper:.6}] against a minimum of {minimum}; raw p = {magnitude}; Holm-adjusted over \
             a family of {size} to {adjusted:.8} against alpha {alpha}; {effective} effective \
             decisions ({raw} raw candidate rows, inflation {inflation}x) against {required} \
             required; baseline {baseline} won {wins} of {scored}",
            if self.is_supported() { "yes" } else { "no" },
            effect = self.aggregate.effect,
            level = self.aggregate.interval.level,
            lower = self.aggregate.interval.lower,
            upper = self.aggregate.interval.upper,
            minimum = self.config.minimum_effect,
            magnitude = magnitude,
            size = self.family.size,
            adjusted = self.aggregate.adjusted_p_value,
            alpha = self.config.alpha,
            effective = self.independence.effective_decisions,
            raw = self.independence.raw_axis_observations,
            inflation = inflation,
            required = self.required_decisions,
            baseline = label(&self.baseline.candidate),
            wins = self.baseline.wins,
            scored = self.baseline.decisions,
        )
    }
}

// ---------------------------------------------------------------------------
// StatisticalRelease
// ---------------------------------------------------------------------------

/// The statistical constituent of a release verdict.
///
/// Two cases and no third, matching 7E-2D's and 7E-3's shape. A measurement
/// that could not be made is [`StatisticalRelease::Refused`] with a typed
/// reason, never `Measured` with a default.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum StatisticalRelease {
    /// The claim was measured. Whether it is supported is recomputed.
    Measured(Box<EvidenceSupport>),
    /// The claim could not be measured at all.
    Refused(StatisticalRefusal),
}

impl StatisticalRelease {
    /// Whether the measured evidence supports the claim, recomputed.
    ///
    /// A refusal is never a support: the whole point of refusing is that "we
    /// cannot show it" and "we have shown it" are different claims, and only one
    /// of them is supported.
    pub fn is_supported(&self) -> bool {
        match self {
            Self::Measured(support) => support.is_supported(),
            Self::Refused(_) => false,
        }
    }

    /// The measured support, if it was measured.
    pub fn support(&self) -> Option<&EvidenceSupport> {
        match self {
            Self::Measured(support) => Some(support),
            Self::Refused(_) => None,
        }
    }

    /// The refusal, if it refused.
    pub fn refusal(&self) -> Option<&StatisticalRefusal> {
        match self {
            Self::Measured(_) => None,
            Self::Refused(refusal) => Some(refusal),
        }
    }

    /// Every constituent that withholds, as stable labels in a fixed order.
    ///
    /// Mirrors [`crate::ml::offline_gate::ReleaseVerdict::blockers`] so the two
    /// halves of the verdict read the same way, and so the statistical half can
    /// be tested in isolation. Every element is a `&'static str`, so a caller
    /// can match on it and nothing is allocated here. A refusal contributes its
    /// one label; a measurement contributes one label per unmet criterion.
    pub fn blockers(&self) -> Vec<&'static str> {
        match self {
            Self::Refused(_) => vec![UNMEASURABLE_LABEL],
            Self::Measured(support) => support.blockers(),
        }
    }

    /// Every constituent that withholds, as sentences carrying the numbers.
    ///
    /// The counterpart to [`StatisticalRelease::blockers`]: a label says
    /// *which* requirement failed, a reason says by how much and against what.
    /// Both are derived from the same criteria in the same order, so neither can
    /// disagree with the other or with [`StatisticalRelease::is_supported`].
    pub fn reasons(&self) -> Vec<String> {
        match self {
            Self::Refused(refusal) => {
                vec![format!("{}: {}", refusal.code, refusal.reason)]
            }
            Self::Measured(support) => support.reasons(),
        }
    }

    /// The headline, in one line.
    pub fn headline(&self) -> String {
        match self {
            Self::Measured(support) => support.headline(),
            Self::Refused(refusal) => format!(
                "statistical support=no; refused: {}: {}",
                refusal.code, refusal.reason
            ),
        }
    }
}

impl From<&StatisticsError> for StatisticalRelease {
    fn from(error: &StatisticsError) -> Self {
        Self::Refused(StatisticalRefusal::from(error))
    }
}

// ---------------------------------------------------------------------------
// StatisticalInput
// ---------------------------------------------------------------------------

/// Everything one statistical measurement consumes.
#[derive(Debug, Clone, Copy)]
pub struct StatisticalInput<'a> {
    /// The holdout partition 7E-2D reserved. Attempt-scope only: the K axis
    /// comes from 7E-2D's `project_cohorts`, never from request-scope rows.
    pub partition: &'a [DecisionCohort],
    /// The distributions 7E-2D emitted over exactly `partition`, in order. The
    /// pairing is verified by fingerprint, not trusted.
    pub emitted: &'a [EmittedDecision],
    /// 7E-2D's per-candidate marginal rows over the same partition.
    pub marginal: &'a [CandidateCalibration],
    /// The claim specification.
    pub config: StatisticalConfig,
}

// ---------------------------------------------------------------------------
// measure_release_evidence
// ---------------------------------------------------------------------------

/// Measure the claim, or refuse.
///
/// The claim: the emitted distribution ranks the candidate that actually served
/// more often than the strongest decision-independent policy, on these same
/// decisions, by more than `config.minimum_effect`, at `1 - config.alpha/2`,
/// with the family of tests corrected.
///
/// **The partition is the attributed decisions only.** A decision where nobody
/// served carries no outcome to credit and contributes no draw; the count of
/// such decisions is reported as
/// [`Independence::unserved_decisions`] so a reader sees the ratio rather than
/// having to infer it. Whether the model *caused* those failures is not this
/// claim: it is 7E-3's terminal-agreement measurement, and the two are reported
/// side by side rather than conflated.
///
/// Pure: it reads three slices and a config and returns a measurement. No RNG,
/// no clock, no file, no thread, no global state. Two runs of the same input
/// produce two identical values, which is a property of the construction
/// (integer counts, closed forms, content order) rather than of a test.
pub fn measure_release_evidence(
    input: &StatisticalInput<'_>,
) -> Result<StatisticalRelease, StatisticsError> {
    input.config.checked()?;

    let credit = attribute(input.partition, input.emitted, input.marginal)
        .map_err(|error| StatisticsError::Attribution(Box::new(error)))?;

    let decisions = credit.independence.effective_decisions;
    if decisions == 0 {
        return Err(StatisticsError::SampleTooSmall {
            observed: 0,
            required: input.config.min_decisions,
            reason: "no decision in the partition had anybody served, so there is no outcome to \
                     credit and no draw to test",
        });
    }
    if decisions < input.config.min_decisions {
        return Err(StatisticsError::SampleTooSmall {
            observed: decisions,
            required: input.config.min_decisions,
            reason: "the decision floor is a hard limit that no configuration change may lift",
        });
    }
    if !credit.selections_total_is_the_decision_count() {
        return Err(StatisticsError::LedgerInconsistent {
            detail: format!(
                "{} candidates were ranked first across {} decisions that each have exactly one \
                 argmax",
                credit.selections_total(),
                credit.decisions_with_a_selection
            ),
        });
    }

    // -- the comparator --
    let baseline = build_baseline(&credit, input.config)?;
    if baseline.decisions != decisions {
        return Err(StatisticsError::LedgerInconsistent {
            detail: format!(
                "the baseline was scored over {} decisions but the claim is over {decisions}",
                baseline.decisions
            ),
        });
    }

    // -- the headline paired comparison, on decisions --
    let mut comparisons: Vec<PairedComparison> =
        vec![compare_on_decisions(credit.outcomes.as_slice(), &baseline.candidate, input.config)?];

    // -- the family: the aggregate plus one test per candidate ranked first --
    let mut members: Vec<FamilyMemberKind> = vec![FamilyMemberKind::Aggregate];
    for row in &credit.candidates {
        if row.ranked_first > 0 {
            members.push(FamilyMemberKind::Candidate(row.candidate.clone()));
        }
    }
    if members.len() > input.config.max_family {
        return Err(StatisticsError::FamilyNotEnumerable {
            reason: format!(
                "the model ranked {} distinct candidates first, which is above the ceiling of {}; \
                 a family this wide cannot be corrected for in a way a reader can audit",
                members.len() - 1,
                input.config.max_family
            ),
        });
    }
    for member in &members {
        let comparison = match member {
            FamilyMemberKind::Aggregate => comparisons[0].clone(),
            FamilyMemberKind::Candidate(candidate) => {
                compare_candidate(credit.outcomes.as_slice(), candidate, input.config)?
            }
        };
        comparisons.push(comparison);
    }
    // The aggregate was seeded above and then re-derived identically here; drop
    // the duplicate so the family is exactly the enumerated members.
    comparisons.remove(0);

    let p_values: Vec<f64> = comparisons
        .iter()
        .map(|comparison| comparison.p_value)
        .collect();
    let adjusted = holm_adjust(&p_values);
    for (comparison, adjusted_p) in comparisons.iter_mut().zip(adjusted) {
        comparison.adjusted_p_value = adjusted_p;
    }
    let aggregate = comparisons
        .first()
        .cloned()
        .ok_or_else(|| StatisticsError::FamilyNotEnumerable {
            reason: "the family enumerated to nothing, so the aggregate test cannot be corrected"
                .to_string(),
        })?;

    // -- power, with the n it took and the n it needs --
    let z_two_sided = finite(
        "the two-sided normal quantile",
        normal_quantile(input.config.level()),
    )?;
    let z_power = finite(
        "the power normal quantile",
        normal_quantile(input.config.power),
    )?;
    let required_decisions = required_decisions(
        aggregate.discordance_rate,
        input.config.minimum_effect,
        z_two_sided,
        z_power,
        decisions,
    )?;

    let family = Family {
        rule: "the aggregate decision-level test, plus one paired test per candidate the emitted \
               distribution ranked first at least once"
            .to_string(),
        correction: "holm-bonferroni, exact under arbitrary dependence",
        size: comparisons.len(),
        members: members
            .into_iter()
            .zip(comparisons)
            .map(|(member, comparison)| FamilyMember { member, comparison })
            .collect(),
    };

    Ok(StatisticalRelease::Measured(Box::new(EvidenceSupport {
        config: input.config,
        independence: credit.independence.clone(),
        baseline,
        credit,
        aggregate,
        family,
        required_decisions,
        z_two_sided,
        z_power,
    })))
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// The strongest decision-independent policy on this partition.
fn build_baseline(
    credit: &CreditLedger,
    config: StatisticalConfig,
) -> Result<BaselinePolicy, StatisticsError> {
    if credit.candidates.is_empty() {
        return Err(StatisticsError::BaselineUnavailable {
            reason: "the axis held no candidate, so no decision-independent policy exists"
                .to_string(),
        });
    }
    // The strongest uninformed policy is the fixed policy whose score is
    // highest, and a fixed policy's score is exactly the number of decisions
    // that candidate won. Ties break on content order, never on position.
    let best: &CandidateCredit = credit
        .candidates
        .iter()
        .reduce(|left, right| {
            if right.served > left.served
                || (right.served == left.served
                    && content_order(&right.candidate, &left.candidate) == std::cmp::Ordering::Less)
            {
                right
            } else {
                left
            }
        })
        .ok_or_else(|| StatisticsError::BaselineUnavailable {
            reason: "the axis enumerated to no candidate".to_string(),
        })?;

    if best.served == 0 {
        return Err(StatisticsError::BaselineUnavailable {
            reason: format!(
                "no candidate in the axis won a single one of the {} attributed decisions, so \
                 every fixed policy scores zero and there is no baseline to beat",
                credit.independence.effective_decisions
            ),
        });
    }

    let decisions = credit.independence.effective_decisions;
    let (lower, upper) = wilson_interval(best.served, decisions, normal_quantile(config.level()));
    Ok(BaselinePolicy {
        candidate: best.candidate.clone(),
        wins: best.served,
        decisions,
        rate: best.served as f64 / decisions as f64,
        interval: Interval {
            lower: finite("the baseline interval's lower bound", lower)?,
            upper: finite("the baseline interval's upper bound", upper)?,
            level: config.level(),
        },
        base_rate_observations: best.base_rate_observations,
        unconditional_base_rate: best.unconditional_base_rate,
        candidates_considered: credit.candidates.len(),
    })
}

/// The headline comparison: the model against one fixed comparator.
fn compare_on_decisions(
    outcomes: &[DecisionOutcome],
    baseline: &CandidateIdentity,
    config: StatisticalConfig,
) -> Result<PairedComparison, StatisticsError> {
    let mut model_only = 0usize;
    let mut baseline_only = 0usize;
    let mut concordant = 0usize;
    for outcome in outcomes {
        match (outcome.model_scored, outcome.served.as_ref() == Some(baseline)) {
            (true, true) | (false, false) => concordant += 1,
            (true, false) => model_only += 1,
            (false, true) => baseline_only += 1,
        }
    }
    build_comparison(
        "the emitted distribution against the strongest decision-independent policy",
        concordant,
        model_only,
        baseline_only,
        outcomes.len(),
        config,
    )
}

/// One candidate's paired test: "did the model rank this candidate first more
/// often than this candidate won?"
///
/// The same exact conditional procedure as the aggregate, on the same
/// decisions. It is a member of the family, and its adjusted p-value is what
/// says whether that candidate's credit is distinguishable from noise.
fn compare_candidate(
    outcomes: &[DecisionOutcome],
    candidate: &CandidateIdentity,
    config: StatisticalConfig,
) -> Result<PairedComparison, StatisticsError> {
    let mut model_only = 0usize;
    let mut baseline_only = 0usize;
    let mut concordant = 0usize;
    for outcome in outcomes {
        let model = outcome.selection.as_ref() == Some(candidate);
        let comparator = outcome.served.as_ref() == Some(candidate);
        match (model, comparator) {
            (true, true) | (false, false) => concordant += 1,
            (true, false) => model_only += 1,
            (false, true) => baseline_only += 1,
        }
    }
    build_comparison(
        &format!(
            "the ranking of {} against its own win count",
            label(candidate)
        ),
        concordant,
        model_only,
        baseline_only,
        outcomes.len(),
        config,
    )
}

/// Assemble one comparison from its four cells.
///
/// Every `f64` below is a function of two integers, so the arithmetic is
/// bit-reproducible by construction: there is no accumulation whose order could
/// vary.
fn build_comparison(
    label: &str,
    concordant: usize,
    model_only: usize,
    baseline_only: usize,
    decisions: usize,
    config: StatisticalConfig,
) -> Result<PairedComparison, StatisticsError> {
    let discordant = model_only + baseline_only;
    if discordant == 0 {
        return Err(StatisticsError::IntervalNotComputable {
            reason: format!(
                "{label}: the two arms agreed on all {decisions} decisions, so the paired \
                 difference has no sampling variability and no interval can be computed for it"
            ),
        });
    }
    if decisions == 0 {
        return Err(StatisticsError::SampleTooSmall {
            observed: 0,
            required: config.min_decisions,
            reason: "no decision was available to compare on",
        });
    }
    if concordant + discordant != decisions {
        return Err(StatisticsError::IntervalNotComputable {
            reason: format!(
                "{label}: {concordant} concordant plus {discordant} discordant decisions do not \
                 reconcile with the {decisions} measured"
            ),
        });
    }

    let n = decisions as f64;
    // Signed arithmetic on purpose. The paired difference is negative whenever
    // the comparator won more discordant decisions than the model did, which is
    // the ordinary case for a model with no skill, and subtracting two `usize`
    // there would wrap to a near-`usize::MAX` positive number and report a
    // spectacular effect instead of a negative one.
    let difference = model_only as i64 - baseline_only as i64;
    let effect = finite("the paired risk difference", difference as f64 / n)?;
    let standard_error = finite("the paired difference's standard error", (discordant as f64).sqrt() / n)?;
    let z = finite("the two-sided normal quantile", normal_quantile(config.level()))?;
    let half_width = finite("the interval's half width", z * standard_error)?;
    let discordance_rate = finite("the discordance rate", discordant as f64 / n)?;
    let log_p_value = finite(
        "the exact conditional log p-value",
        mcnemar_exact_log_p(model_only, baseline_only),
    )?;
    let p_value = finite("the exact conditional p-value", log_p_value.exp().min(1.0))?;

    Ok(PairedComparison {
        label: label.to_string(),
        concordant,
        model_only,
        baseline_only,
        decisions,
        effect,
        standard_error,
        interval: Interval {
            lower: finite("the interval's lower bound", effect - half_width)?,
            upper: finite("the interval's upper bound", effect + half_width)?,
            level: config.level(),
        },
        discordance_rate,
        p_value,
        log_p_value,
        adjusted_p_value: p_value,
    })
}

// ---------------------------------------------------------------------------
// Closed-form statistics
// ---------------------------------------------------------------------------

/// The exact two-sided conditional binomial p-value for McNemar's test.
///
/// `P(Bin(m, 1/2) <= min(n10, n01))` doubled, capped at 1, where `m = n10 +
/// n01`. Conditioning on the discordant count is the standard exact McNemar
/// construction: under the null of no paired difference the discordant cells
/// are Binomial(`m`, 1/2), and the concordant cells carry no information about
/// the difference at all.
pub fn mcnemar_exact_p(model_only: usize, baseline_only: usize) -> f64 {
    mcnemar_exact_log_p(model_only, baseline_only).exp().min(1.0)
}

/// The same p-value, in log space.
///
/// The tail is accumulated entirely in log space, so a discordant count large
/// enough to drive the true p-value below the smallest representable positive
/// `f64` still reports its magnitude instead of collapsing to zero.
pub fn mcnemar_exact_log_p(model_only: usize, baseline_only: usize) -> f64 {
    let discordant = model_only + baseline_only;
    if discordant == 0 {
        return 0.0;
    }
    let smaller = model_only.min(baseline_only);
    let (tail, log_tail) = binomial_half_lower_tail(discordant, smaller);
    if !log_tail.is_finite() {
        return f64::NAN;
    }
    // The cap is `2 * tail >= 1`, and it is decided on the *linear* tail
    // because that is where the comparison is exact. Deciding it in log space
    // turns an exact `0.5` into `0.5 - 6e-15` and the cap stops being a cap.
    // A tail sitting precisely on the boundary resolves to within a few ulps
    // either way, which is four orders of magnitude below any level a release
    // gate decides at, and the reported p is `1.0` to thirteen decimal places.
    if tail > 0.0 && 2.0 * tail >= 1.0 {
        0.0
    } else {
        LN_2 + log_tail
    }
}

/// `P(Bin(m, 1/2) <= k)` and its logarithm, for `k <= m`.
///
/// The terms are walked downward from `k`, which is the largest term whenever
/// `k <= m/2` and the caller always passes `k = min(n10, n01) <= m/2`. Because
/// every later term is smaller, the sum is taken against the first term as a
/// common factor, so `scaled` lies between `1` and `k + 1` and is
/// well-conditioned whatever `m` is.
///
/// Both forms are returned because they fail in different places. The linear
/// product is the accurate one and is what the cap is decided on; the
/// logarithm is what survives when the largest term underflows, which happens
/// above roughly 1074 discordant pairs. Returning only the linear value would
/// report `p = 0.0` — a probability of exactly zero, which is never true — for
/// an effect that is merely enormous.
fn binomial_half_lower_tail(m: usize, k: usize) -> (f64, f64) {
    if k >= m {
        return (1.0, 0.0);
    }
    let base = ln_binomial_half_pmf(m, k);
    if !base.is_finite() {
        return (f64::NAN, f64::NAN);
    }
    let mut scaled = 1.0f64;
    let mut log_term = base;
    for j in (1..=k).rev() {
        log_term += (j as f64).ln() - ((m - j + 1) as f64).ln();
        scaled += (log_term - base).exp();
    }
    let log_tail = base + scaled.ln();
    let head = base.exp();
    if head == 0.0 {
        (0.0, log_tail)
    } else {
        (head * scaled, log_tail)
    }
}

/// `ln(C(m, k) * 2^{-m})`, via the Lanczos log gamma.
fn ln_binomial_half_pmf(m: usize, k: usize) -> f64 {
    let mf = m as f64;
    let kf = k as f64;
    ln_gamma(mf + 1.0) - ln_gamma(kf + 1.0) - ln_gamma(mf - kf + 1.0) - mf * LN_2
}

/// The number of decisions required to detect a paired difference of `delta`
/// with `power`, given the observed discordance rate.
///
/// The standard McNemar power formula, `n = (z_{1-α/2} + z_β)² · ψ / δ²`, with
/// `ψ` the discordance rate. The discordance rate is estimated from the data,
/// which is the usual and documented approximation: `ψ` is a property of the
/// decision population, not of the hypothesis, so it is not the quantity being
/// tested and estimating it from the same run does not bias the test it feeds.
///
/// The result is clamped to `at_least` so a caller cannot produce a required
/// `n` below the number of decisions it already measured, which would make the
/// adequacy criterion vacuous.
pub fn required_decisions(
    discordance_rate: f64,
    delta: f64,
    z_two_sided: f64,
    z_power: f64,
    at_least: usize,
) -> Result<usize, StatisticsError> {
    if !discordance_rate.is_finite() || discordance_rate <= 0.0 {
        return Err(StatisticsError::IntervalNotComputable {
            reason: "the two arms never disagreed on any decision, so no sample size can detect \
                     a difference and no required n exists"
                .to_string(),
        });
    }
    let z_sum = finite("the combined normal quantiles", z_two_sided + z_power)?;
    let numerator = finite("the power numerator", z_sum * z_sum * discordance_rate)?;
    let denominator = finite("the squared minimum effect", delta * delta)?;
    if !denominator.is_finite() || denominator <= 0.0 {
        return Err(StatisticsError::NonFiniteMeasurement {
            context: "the squared minimum effect",
            value: denominator,
        });
    }
    let required = finite("the required decision count", numerator / denominator)?;
    if required > usize::MAX as f64 {
        return Err(StatisticsError::NonFiniteMeasurement {
            context: "the required decision count",
            value: required,
        });
    }
    Ok((required.ceil() as usize).max(at_least).max(1))
}

/// The Wilson score interval on a binomial proportion.
///
/// Wilson rather than Wald because Wald's coverage collapses towards the
/// boundary as `n` shrinks and as `p` approaches 0 or 1, which is exactly the
/// regime a small holdout is in. Closed form, no iteration, no RNG.
pub fn wilson_interval(successes: usize, total: usize, z: f64) -> (f64, f64) {
    if total == 0 {
        return (f64::NAN, f64::NAN);
    }
    let n = total as f64;
    let p = successes as f64 / n;
    let z2 = z * z;
    let denominator = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denominator;
    let half_width = z / denominator * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    (centre - half_width, centre + half_width)
}

/// Holm–Bonferroni adjusted p-values, in the order the input was given.
///
/// Step-down, and therefore always at least as powerful as Bonferroni, and
/// exact under arbitrary dependence. The adjusted value at rank `i` is the
/// running `max` over all `j <= i` of `(m - j) * p_(j)`, capped at 1, which is
/// what makes the sequence monotone. Ties in `p` break on the input index, so
/// the result does not depend on a sort's stability.
pub fn holm_adjust(p_values: &[f64]) -> Vec<f64> {
    let m = p_values.len();
    if m == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|left, right| {
        p_values[*left]
            .partial_cmp(&p_values[*right])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.cmp(right))
    });
    let mut adjusted = vec![0.0f64; m];
    let mut running = 0.0f64;
    for (rank, index) in order.iter().enumerate() {
        let scaled = (m - rank) as f64 * p_values[*index];
        running = running.max(scaled);
        adjusted[*index] = running.min(1.0);
    }
    adjusted
}

fn finite(context: &'static str, value: f64) -> Result<f64, StatisticsError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(StatisticsError::NonFiniteMeasurement { context, value })
    }
}

// ---------------------------------------------------------------------------
// Normal quantile
// ---------------------------------------------------------------------------

const LN_2: f64 = std::f64::consts::LN_2;
const LN_2PI_HALF: f64 = 0.918_938_533_204_672_7;

/// The inverse standard normal CDF.
///
/// Acklam's rational approximation: one central rational piece and two mirrored
/// tail pieces, accurate to about `1.15e-9` in the quantile. That is far below
/// any threshold this gate decides on — the closest the criteria come to a
/// decision boundary is `lower > minimum_effect`, where a `1e-9` error in `z`
/// moves the bound by `1e-9 * SE` — and it is deterministic, which matters more
/// here than the last decimal would: the interval bounds are a function of the
/// input and of nothing else.
///
/// `statrs` is not a dependency of this workspace, so there is no alternative to
/// hand-rolling. The alternative would be to carry a normal CDF forward and
/// invert it, which is strictly more code for strictly less accuracy. An
/// implementation error in the last digit would be invisible to a reader and
/// not to a test, so
/// `the_hand_rolled_normal_quantile_matches_the_published_values` pins it
/// against the published values for the quantiles this node uses.
pub fn normal_quantile(probability: f64) -> f64 {
    if probability.is_nan() {
        return f64::NAN;
    }
    if probability <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if probability >= 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239e0,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838e0,
        -2.549_732_539_343_734e0,
        4.374_664_141_464_968e0,
        2.938_163_982_698_783e0,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996e0,
        3.754_408_661_907_416e0,
    ];
    const LOW: f64 = 0.02425;
    const HIGH: f64 = 1.0 - LOW;

    if probability < LOW {
        let q = (-2.0 * probability.ln()).sqrt();
        tail_ratio(&C, &D, q)
    } else if probability <= HIGH {
        let q = probability - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - probability).ln()).sqrt();
        -tail_ratio(&C, &D, q)
    }
}

/// Acklam's tail piece: a degree-5 numerator over a degree-4 denominator.
fn tail_ratio(numerator: &[f64; 6], denominator: &[f64; 4], q: f64) -> f64 {
    let top = ((((numerator[0] * q + numerator[1]) * q + numerator[2]) * q + numerator[3]) * q
        + numerator[4])
        * q
        + numerator[5];
    let bottom =
        (((denominator[0] * q + denominator[1]) * q + denominator[2]) * q + denominator[3]) * q + 1.0;
    top / bottom
}

// ---------------------------------------------------------------------------
// log gamma
// ---------------------------------------------------------------------------

/// The natural logarithm of the gamma function, by the Lanczos approximation
/// with `g = 7`.
///
/// `ln C(m, k) = ln_gamma(m+1) - ln_gamma(k+1) - ln_gamma(m-k+1)`, and the
/// binomial tail needs that in log space because `C(m, k) * 2^{-m}` underflows
/// for `m` above about 1074. Lanczos to nine coefficients is accurate to
/// roughly `1e-15` relative over the range used here.
///
/// Only called with `z >= 1`. Below `0.5` the reflection formula would be needed
/// and is deliberately not implemented: returning a non-finite value instead
/// makes a future out-of-range call a typed refusal rather than a silently wrong
/// tail.
pub fn ln_gamma(z: f64) -> f64 {
    const COEFFICIENTS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    const G: f64 = 7.0;

    if !z.is_finite() || z < 0.5 {
        return f64::NAN;
    }
    let shifted = z - 1.0;
    let mut series = COEFFICIENTS[0];
    for (index, coefficient) in COEFFICIENTS.iter().enumerate().skip(1) {
        series += coefficient / (shifted + index as f64);
    }
    let t = shifted + G + 0.5;
    LN_2PI_HALF + (shifted + 0.5) * t.ln() - t + series.ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exact_conditional_p_value_matches_hand_computed_binomial_tails() {
        // No discordant evidence at all: the tail is one and the doubled value
        // is capped at one.
        assert_eq!(mcnemar_exact_p(0, 0), 1.0);
        // m = 1: P(X <= 0) = 1/2, doubled = 1. No possible pair of cells is
        // significant at any conventional level with a single discordant pair.
        assert!((mcnemar_exact_p(1, 0) - 1.0).abs() < 1e-15);
        // m = 2, split 2/0: P(X <= 0) = 1/4, doubled = 1/2.
        assert!((mcnemar_exact_p(2, 0) - 0.5).abs() < 1e-15);
        // m = 3, split 3/0: 1/8, doubled = 1/4.
        assert!((mcnemar_exact_p(3, 0) - 0.25).abs() < 1e-15);
        // m = 4, split 4/0: 1/16, doubled = 1/8.
        assert!((mcnemar_exact_p(4, 0) - 0.125).abs() < 1e-15);
        // m = 6, split 6/0: 1/64, doubled = 1/32.
        assert!((mcnemar_exact_p(6, 0) - 0.03125).abs() < 1e-15);
        // A symmetric or near-symmetric split is never significant. The cap
        // boundary is exact here: 2 * P(Bin(10, 1/2) <= 5) = 1.246.
        assert_eq!(mcnemar_exact_p(5, 5), 1.0);
        // 2 * P(Bin(19, 1/2) <= 9) = 1 exactly, so the cap is decided on the
        // boundary. It resolves to one to thirteen decimal places either way.
        assert!(
            (mcnemar_exact_p(10, 9) - 1.0).abs() < 1e-13,
            "{}",
            mcnemar_exact_p(10, 9)
        );
    }

    #[test]
    fn the_exact_conditional_p_value_survives_a_discordant_count_that_would_overflow() {
        // 2000 discordant pairs, all in one cell: 2 * 2^-2000 is around 1e-601,
        // far below the smallest positive f64. A naive term would underflow to
        // zero and then stay zero for the whole recursion, and a naive `p`
        // would report exactly zero and claim that is the answer.
        let log_p = mcnemar_exact_log_p(2000, 0);
        assert!(
            log_p < -1380.0 && log_p > -1400.0,
            "the magnitude must survive: {log_p}"
        );
        assert_eq!(mcnemar_exact_p(2000, 0), 0.0, "and the linear field admits it");
        // A split far from symmetric is still a real number in log space. The
        // dominant term alone is `ln C(2000,200) - 2000 ln 2 = -739.64`; the
        // 200 smaller terms lift it to about -738.8, and the lift is the sign
        // that the recursion actually summed rather than truncated.
        let split = mcnemar_exact_log_p(1800, 200);
        assert!(split < -738.0 && split > -739.7, "{split}");
    }

    #[test]
    fn the_hand_rolled_normal_quantile_matches_the_published_values() {
        let cases = [
            (0.975, 1.959_963_984_540_054),
            (0.95, 1.644_853_626_951_472),
            (0.80, 0.841_621_233_572_914_3),
            (0.99, 2.326_347_874_040_841),
            (0.995, 2.575_829_303_548_900_4),
            (0.5, 0.0),
            (0.025, -1.959_963_984_540_054),
        ];
        for (probability, expected) in cases {
            let got = normal_quantile(probability);
            assert!(
                (got - expected).abs() < 1e-8,
                "normal_quantile({probability}) = {got}, expected {expected}"
            );
        }
        assert_eq!(normal_quantile(0.0), f64::NEG_INFINITY);
        assert_eq!(normal_quantile(1.0), f64::INFINITY);
        assert!(normal_quantile(f64::NAN).is_nan());
    }

    #[test]
    fn the_hand_rolled_log_gamma_matches_the_published_values() {
        for (z, expected) in [
            (1.0, 0.0_f64),
            (2.0, 0.0_f64),
            (3.0, std::f64::consts::LN_2),
            (4.0, 1.791_759_469_228_055_f64),
            (10.0, 12.801_827_480_081_469_f64),
        ] {
            let got = ln_gamma(z);
            assert!(
                (got - expected).abs() < 1e-12,
                "ln_gamma({z}) = {got}, expected {expected}"
            );
        }
        assert!(
            ln_gamma(0.25).is_nan(),
            "out of range is non-finite, not wrong"
        );
    }

    #[test]
    fn the_wilson_interval_brackets_the_point_estimate_and_stays_inside_zero_to_one() {
        for (successes, total) in [(0usize, 10usize), (10, 10), (1, 10), (5, 10), (0, 1), (3, 7)] {
            let (lower, upper) = wilson_interval(successes, total, 1.959_963_984_540_054);
            let p = successes as f64 / total as f64;
            // The tolerance is a few ulps, not a statistical one: at `p = 1`
            // Wilson's algebra gives an upper bound of `1 - 1e-16` rather than
            // `1`, which is rounding and not a narrower interval.
            assert!(
                lower <= p + 1e-12 && p <= upper + 1e-12,
                "{successes}/{total}: [{lower}, {upper}] must bracket {p}"
            );
            assert!(lower >= 0.0, "Wilson never goes below zero");
            assert!(upper <= 1.0, "Wilson never goes above one");
        }
        assert!(wilson_interval(0, 0, 1.96).0.is_nan());
    }

    #[test]
    fn holm_adjust_is_monotone_and_never_below_bonferroni_at_its_own_rank() {
        let p = [0.001, 0.02, 0.04, 0.5];
        let adjusted = holm_adjust(&p);
        assert_eq!(adjusted, vec![0.004, 0.06, 0.08, 0.5]);

        // The smallest p is Bonferroni-exact: `4 * 0.001 = 0.004`.
        let m = p.len() as f64;
        assert!((adjusted[0] - m * p[0]).abs() < 1e-15);

        // Every hypothesis is held to Bonferroni *at its own rank*, which is the
        // step-down: the k-th smallest is judged against `alpha / (m - k)`. So
        // adjusted is at least `(m - rank) * p` for the rank that hypothesis
        // occupies. It is deliberately *below* plain `m * p` at later ranks —
        // that is exactly where Holm is more powerful than Bonferroni, and
        // asserting otherwise would be asserting a false property.
        let mut order: Vec<usize> = (0..p.len()).collect();
        order.sort_by(|left, right| {
            p[*left]
                .partial_cmp(&p[*right])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (rank, index) in order.iter().enumerate() {
            assert!(
                adjusted[*index] >= (m - rank as f64) * p[*index] - 1e-15,
                "rank {rank}: {} must be at least {}",
                adjusted[*index],
                (m - rank as f64) * p[*index]
            );
        }
        // The order in which p-values arrive does not change the value attached
        // to a p-value.
        let shuffled = [3usize, 1, 2, 0];
        let reordered: Vec<f64> = shuffled.iter().map(|index| p[*index]).collect();
        let reordered_adjusted = holm_adjust(&reordered);
        for (slot, index) in shuffled.iter().enumerate() {
            assert!((reordered_adjusted[slot] - adjusted[*index]).abs() < 1e-15);
        }
        assert!(holm_adjust(&[]).is_empty());
        assert_eq!(holm_adjust(&[0.25]), vec![0.25]);
    }

    #[test]
    fn the_required_sample_size_grows_as_the_effect_shrinks() {
        let z = 1.959_963_984_540_054_f64;
        let power_z = normal_quantile(0.80);
        let big = required_decisions(0.6, 0.10, z, power_z, 1).expect("computes");
        let small = required_decisions(0.6, 0.05, z, power_z, 1).expect("computes");
        let tinier = required_decisions(0.6, 0.01, z, power_z, 1).expect("computes");
        assert!(small > big, "{small} must exceed {big}");
        assert!(tinier > small, "{tinier} must exceed {small}");
        // A null effect is not detectable at any sample size, so there is no
        // required n to report.
        assert!(required_decisions(0.0, 0.05, z, power_z, 1).is_err());
        // The floor stops a caller from making the criterion vacuous.
        assert_eq!(required_decisions(0.6, 0.9, z, power_z, 12_345).unwrap(), 12_345);
    }

    #[test]
    fn the_refusal_code_names_the_variant() {
        let error = StatisticsError::SampleTooSmall {
            observed: 1,
            required: 2,
            reason: "a test",
        };
        assert_eq!(error.code(), "sample_too_small");
        let wrapped = StatisticsError::Attribution(Box::new(AttributionError::EmptyPartition));
        assert_eq!(wrapped.code(), "empty_partition");
    }

    #[test]
    fn a_vacuous_claim_specification_is_refused_rather_than_run() {
        for config in [
            StatisticalConfig {
                alpha: 0.0,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                alpha: 1.0,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                power: 1.0,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                power: f64::NAN,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                minimum_effect: 0.0,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                minimum_effect: -0.1,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                minimum_effect: 1.0,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                min_decisions: 0,
                ..StatisticalConfig::default()
            },
            StatisticalConfig {
                max_family: 0,
                ..StatisticalConfig::default()
            },
        ] {
            let error = config.checked().expect_err("a vacuous claim is refused");
            assert_eq!(error.code(), "invalid_claim", "{error}");
        }
        assert!(StatisticalConfig::default().checked().is_ok());
    }

    #[test]
    fn the_stated_level_is_two_sided() {
        let config = StatisticalConfig::default();
        assert!((config.level() - 0.975).abs() < 1e-15);
        assert!(
            config.level() > 0.95,
            "1 - alpha would be the one-sided level and would report a confidence this \
             interval does not have"
        );
    }
}
