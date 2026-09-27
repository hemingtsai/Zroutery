//! Node 7D — attempt-level attribution, and the arithmetic of the unit of
//! independence.
//!
//! # Why this module exists separately from [`crate::ml::statistics`]
//!
//! Within one decision the candidates are **mutually exclusive**: exactly one
//! is served, or none is. Every candidate row in a decision is therefore a
//! coordinate of *one* observation, not an observation of its own. A cohort of
//! arity `K` contributes exactly one categorical draw from
//! `{c₁, …, c_K, none}` to the evidence, no matter how many rows it holds.
//!
//! This module owns three things and delegates everything else:
//!
//! 1. the **credit ledger** — what each candidate is owed, and what it scored;
//! 2. the **unit-of-independence arithmetic** — the raw row count, the
//!    effective decision count, and the inflation between them, all three in the
//!    output rather than one of them in a comment;
//! 3. the **attribution-side refusals** — an axis that is not a composition of
//!    unity, a served identity absent from its own axis, a baseline row missing
//!    for a candidate that needs one.
//!
//! The hypothesis tests live in [`crate::ml::statistics`]. Nothing here decides
//! whether an effect is significant; that is a different claim, made over a
//! different unit, and keeping the two apart is what stops the credit ledger
//! from being mistaken for a p-value.
//!
//! # What this module does not do
//!
//! It does not group rows into decisions. 7E-2D's `project_cohorts` builds the
//! K axis from attempt-scope rows and is the only thing that decides which rows
//! belong to which decision. It does not compute a base rate: the
//! unconditional base rate is 7E-2D's [`MarginalCalibration`], consumed here and
//! cross-checked against the integer counts so a silent divergence is a refusal
//! rather than a quiet disagreement.

use serde::Serialize;

use crate::ml::calibration::{CandidateCalibration, DecisionCohort, EmittedDecision};
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// CandidateCredit
// ---------------------------------------------------------------------------

/// What one candidate is owed, and what it scored, over a holdout partition.
///
/// Every count here is a `usize` sum over **decisions**, never over rows. The
/// distinction is not cosmetic: `axis_decisions` and `ranked_decisions` are
/// both row counts, `served` is a decision count, and the credit is the
/// decision count divided by the decision count. A ledger that let a row count
/// into a numerator would report a candidate as better than it is whenever the
/// axis is wide, which is the specific error this node exists to prevent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateCredit {
    /// Which candidate.
    pub candidate: CandidateIdentity,
    /// Decisions whose K axis contains this candidate. A **row** count: it
    /// counts decisions, each of which holds exactly one row for this
    /// candidate, so it is both the row count and the decision count for the
    /// axis. It is reported separately from `effective_decisions` in
    /// [`CreditLedger`] so the two can never be confused.
    pub axis_decisions: usize,
    /// Decisions in which this candidate carried a prediction.
    ///
    /// Strictly less than or equal to `axis_decisions`: an unranked candidate
    /// is in the axis and receives exactly `0.0` mass. This is the
    /// denominator 7E-2D's marginal route uses, which is why the cross-check
    /// against [`CandidateCalibration::observations`] is exact.
    pub ranked_decisions: usize,
    /// Decisions this candidate won, over the whole partition.
    pub served: usize,
    /// Decisions in which the emitted distribution ranked this candidate first.
    ///
    /// Sums to at most `effective_decisions` across the ledger, because a
    /// distribution has exactly one argmax. That identity is asserted by
    /// [`CreditLedger::selections_total_is_the_decision_count`].
    pub ranked_first: usize,
    /// Decisions in which the model ranked this candidate first **and** it won.
    pub ranked_first_and_won: usize,
    /// 7E-2D's observation count for this candidate, consumed verbatim.
    pub base_rate_observations: usize,
    /// 7E-2D's win count for this candidate, consumed verbatim.
    pub base_rate_served: usize,
    /// 7E-2D's unconditional base rate, `served / observations`, consumed
    /// verbatim from the marginal route. `None` when 7E-2D measured no row for
    /// this candidate, which is a refusal at
    /// [`crate::ml::statistics::StatisticsError::BaselineUnavailable`] for any
    /// candidate the claim actually needs.
    pub unconditional_base_rate: Option<f64>,
    /// 7E-2D's `mean_predicted - observed_frequency` for this candidate,
    /// consumed verbatim. Carried so a reader sees the calibration gap beside
    /// the credit, not instead of it.
    pub calibration_gap: Option<f64>,
}

impl CandidateCredit {
    /// The rate at which the model ranks this candidate first, over the
    /// decisions it was ranked in.
    pub fn selection_rate(&self) -> f64 {
        if self.ranked_decisions == 0 {
            return 0.0;
        }
        self.ranked_first as f64 / self.ranked_decisions as f64
    }

    /// The rate at which ranking this candidate first was right, over the
    /// decisions it was ranked first in.
    pub fn precision(&self) -> Option<f64> {
        if self.ranked_first == 0 {
            return None;
        }
        Some(self.ranked_first_and_won as f64 / self.ranked_first as f64)
    }

    /// The label used in every message about this candidate.
    pub fn label(&self) -> String {
        format!("{}/{}", self.candidate.provider(), self.candidate.model())
    }
}

// ---------------------------------------------------------------------------
// CreditLedger
// ---------------------------------------------------------------------------

/// The unit-of-independence arithmetic, stated as fields.
///
/// This is the answer to "how many observations is this?", and it deliberately
/// reports three numbers rather than one. An analysis that reports only
/// `raw_axis_observations` is wrong by the inflation factor; an analysis that
/// reports only `effective_decisions` hides how much data it touched. Both are
/// here, and their ratio is named.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Independence {
    /// `Σ arity(cohort)` over the partition: the candidate-row count. This is
    /// the number a row-level analysis would treat as `n`.
    pub raw_axis_observations: usize,
    /// Decisions with somebody served: the number of independent draws. This
    /// is the `n` every interval, test and power calculation in
    /// [`crate::ml::statistics`] is computed on.
    pub effective_decisions: usize,
    /// Decisions where nobody served, and therefore contribute a draw to
    /// nobody's credit.
    pub unserved_decisions: usize,
    /// `raw_axis_observations / effective_decisions`, the factor by which a
    /// row-level analysis would overstate the sample size. Equals `K` for a
    /// uniform axis. `None` when `effective_decisions` is zero, which is a
    /// refusal upstream and never reaches a ledger.
    pub inflation: Option<f64>,
}

impl Independence {
    fn of(raw_axis_observations: usize, effective_decisions: usize, unserved: usize) -> Self {
        let inflation = (effective_decisions > 0).then(|| {
            raw_axis_observations as f64 / effective_decisions as f64
        });
        Self {
            raw_axis_observations,
            effective_decisions,
            unserved_decisions: unserved,
            inflation,
        }
    }
}

/// Everything one candidate is owed over a holdout partition.
///
/// `candidates` is sorted by `(provider, model)`, which is a function of
/// content. It is **not** sorted by first appearance, and it is never built by
/// walking a hash container: a ledger whose row order depended on hash
/// iteration order would make every float derived from it a function of
/// randomness, which is the exact bug class 7E-2D had to add an explicit
/// `sort_by` to escape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreditLedger {
    /// The unit-of-independence arithmetic.
    pub independence: Independence,
    /// One row per candidate, in content order.
    pub candidates: Vec<CandidateCredit>,
    /// The distinct arities observed, sorted, so a reader can see the axis was
    /// not assumed uniform.
    pub observed_arities: Vec<usize>,
    /// Decisions in which the emitted distribution ranked some candidate first.
    ///
    /// Always equal to `effective_decisions` for a well-formed ledger: a
    /// distribution has exactly one argmax. Exposed so the invariant is a
    /// reportable fact rather than a claim in a comment.
    pub decisions_with_a_selection: usize,
    /// One entry per attributed decision, in the partition's content order.
    ///
    /// This is the sufficient statistic every paired comparison in
    /// [`crate::ml::statistics`] is computed from, and its length is the
    /// effective `n`. A decision with no served candidate is not in it: it
    /// carries no outcome to credit, and admitting it would let a request that
    /// failed be scored as a routing mistake.
    pub outcomes: Vec<DecisionOutcome>,
}

impl CreditLedger {
    /// The credit row for one candidate, if it is in the axis.
    pub fn credit_of(&self, candidate: &CandidateIdentity) -> Option<&CandidateCredit> {
        self.candidates
            .iter()
            .find(|row| &row.candidate == candidate)
    }

    /// The summed `ranked_first` across the ledger.
    ///
    /// A function of the content-ordered rows, so the summation order is a
    /// function of content and not of any container's iteration order. The
    /// count is an integer, so the sum is exact regardless — but the order is
    /// pinned anyway, so the invariant test is testing the order and not the
    /// absence of rounding.
    pub fn selections_total(&self) -> usize {
        self.candidates.iter().map(|row| row.ranked_first).sum()
    }

    /// Whether the argmax identity holds: exactly one candidate was ranked
    /// first in every decision that has a selection.
    pub fn selections_total_is_the_decision_count(&self) -> bool {
        self.selections_total() == self.decisions_with_a_selection
    }

    /// The candidates in the axis, in content order.
    pub fn axis(&self) -> Vec<CandidateIdentity> {
        self.candidates
            .iter()
            .map(|row| row.candidate.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// AttributionError
// ---------------------------------------------------------------------------

/// Every way the attribution refuses.
///
/// A refusal is a typed value carrying the numbers or names that caused it.
/// There is no `ok_or_default`, no zero-filled row and no "assume nobody
/// served" path: an axis that is not a composition of unity, or a served
/// identity that is not in the axis it is supposed to be served from, is
/// refused rather than guessed at.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize)]
pub enum AttributionError {
    /// The partition held no decision at all.
    #[error("the holdout partition is empty, so no candidate can be credited")]
    EmptyPartition,

    /// A cohort's K axis held fewer than two candidates.
    ///
    /// One candidate cannot be ranked, so "the distribution ranked the winner
    /// first" is true in every decision by construction and carries no
    /// information. 7E-2D has its own degeneracy vocabulary for the
    /// calibration route; this is the attribution route's, and it refuses
    /// rather than reporting a perfect score.
    #[error("decision {cohort} has an axis of arity {arity}, which cannot be ranked")]
    DegenerateAxis {
        /// The content fingerprint of the decision.
        cohort: String,
        /// How many candidates it held.
        arity: usize,
    },

    /// The Outcome records a served identity that is not in the decision's
    /// own axis.
    ///
    /// This is a contradiction, not a gap: the cohort was built from the
    /// attempt rows of that decision, so the identity that served must be
    /// among them. Guessing which axis entry was meant would silently invent
    /// credit.
    #[error("decision {cohort} records '{served}' as served, which is not in its own axis")]
    ServedAbsentFromAxis {
        /// The content fingerprint of the decision.
        cohort: String,
        /// The offending identity.
        served: String,
    },

    /// More than one candidate was served in a single decision.
    ///
    /// The K axis is a composition of unity, so this is the violation that
    /// makes a row-level analysis invalid rather than merely optimistic. It is
    /// refused, not averaged.
    #[error("decision {cohort} has {winners} served candidates, but the axis is a composition of unity")]
    MultipleServedInOneDecision {
        /// The content fingerprint of the decision.
        cohort: String,
        /// How many were served.
        winners: usize,
    },

    /// The emitted distribution and the partition are not aligned.
    ///
    /// The caller must supply the distributions emitted over exactly this
    /// partition. The fingerprints are compared index for index rather than
    /// trusting the order, because a misaligned pair would attribute one
    /// decision's win to another decision's ranking.
    #[error(
        "the emitted distribution {emitted} is not the distribution for partition row {index} \
         ({expected})"
    )]
    PartitionMisaligned {
        /// The index the check was at.
        index: usize,
        /// The partition row's fingerprint.
        expected: String,
        /// The distribution's fingerprint.
        emitted: String,
    },

    /// A candidate the claim needs has no 7E-2D marginal row.
    #[error(
        "candidate '{candidate}' carries no 7E-2D marginal row, so its unconditional base rate \
         cannot be used as a baseline"
    )]
    BaselineUnavailable {
        /// The offending identity.
        candidate: String,
    },

    /// 7E-2D's marginal row disagrees with the counts measured here.
    ///
    /// A mismatch means the two routes are not describing the same
    /// observations, and every downstream interval would be attached to the
    /// wrong denominator. It is a refusal rather than a preference for one
    /// side.
    #[error(
        "candidate '{candidate}' has 7E-2D observations {theirs} and served {their_served}, but \
         this ledger measured {ours} and {our_served}"
    )]
    BaselineDisagrees {
        /// The offending identity.
        candidate: String,
        /// 7E-2D's observation count.
        theirs: usize,
        /// 7E-2D's win count.
        their_served: usize,
        /// The rankable decision count measured here.
        ours: usize,
        /// The win count measured here.
        our_served: usize,
    },
}

impl AttributionError {
    /// A stable machine-readable code, so the release gate can name the refusal
    /// without matching on prose.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptyPartition => "empty_partition",
            Self::DegenerateAxis { .. } => "degenerate_axis",
            Self::ServedAbsentFromAxis { .. } => "served_absent_from_axis",
            Self::MultipleServedInOneDecision { .. } => "multiple_served_in_one_decision",
            Self::PartitionMisaligned { .. } => "partition_misaligned",
            Self::BaselineUnavailable { .. } => "baseline_unavailable",
            Self::BaselineDisagrees { .. } => "baseline_disagrees",
        }
    }
}

// ---------------------------------------------------------------------------
// DecisionOutcome
// ---------------------------------------------------------------------------

/// One decision's contribution to the evidence: a single categorical draw.
///
/// This type is the unit of independence, made explicit. A decision of arity
/// `K` produces exactly one of these, not `K` of them, and every statistic in
/// [`crate::ml::statistics`] is computed over a `Vec` of these. Making the unit
/// a data structure rather than a comment is the point: a future change that
/// tried to push one row per candidate into a count would have to change this
/// type, and the type says what a draw is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionOutcome {
    /// The content fingerprint of the decision this draw came from.
    ///
    /// Present so the ledger's own order is a function of content: the walk
    /// sorts on it, which is what makes the whole measurement a function of the
    /// *set* of decisions rather than of the order they arrived in. Without it a
    /// permuted partition would produce a permuted `outcomes` list, and a reader
    /// diffing two reports could not tell a reordering from a change.
    pub fingerprint: String,
    /// The candidate the emitted distribution ranked first, or `None` when the
    /// decision's distribution ranked nobody — which 7E-2D's normalisation
    /// check makes unreachable, and which is carried rather than invented.
    pub selection: Option<CandidateIdentity>,
    /// The candidate that actually served, or `None` when the request failed.
    /// A `None` here is why the decision contributes no [`DecisionOutcome`] to
    /// the ledger: there is no outcome to credit.
    pub served: Option<CandidateIdentity>,
    /// Whether the model got this decision right: it ranked the served
    /// candidate first.
    pub model_scored: bool,
}

// ---------------------------------------------------------------------------
// attribute
// ---------------------------------------------------------------------------

/// A per-candidate mutable accumulator, private to the walk.
struct Acc {
    candidate: CandidateIdentity,
    axis_decisions: usize,
    ranked_decisions: usize,
    served: usize,
    ranked_first: usize,
    ranked_first_and_won: usize,
}

/// Build the credit ledger over one holdout partition and the distributions
/// emitted over exactly it.
///
/// `emitted` must be the distributions 7E-2D produced for `partition`, in the
/// same order. The pairing is **verified** by comparing
/// [`DecisionCohort::fingerprint`] against [`EmittedDecision::cohort_fingerprint`]
/// index for index, so a caller that hands over two unrelated partitions is
/// refused rather than silently cross-attributed.
///
/// The base rate is not computed here. `marginal` is 7E-2D's
/// [`MarginalCalibration`], measured by `collect_marginal_observations` over the
/// same partition, and its per-candidate counts are carried through and
/// cross-checked against the integer counts this walk accumulates. The
/// cross-check is what makes "we used the accepted unconditional base rate" a
/// checkable statement rather than a claim.
pub fn attribute(
    partition: &[DecisionCohort],
    emitted: &[EmittedDecision],
    marginal: &[CandidateCalibration],
) -> Result<CreditLedger, AttributionError> {
    if partition.is_empty() {
        return Err(AttributionError::EmptyPartition);
    }
    if partition.len() != emitted.len() {
        let index = partition.len().min(emitted.len());
        return Err(AttributionError::PartitionMisaligned {
            index,
            expected: partition
                .get(index)
                .map_or_else(String::new, |cohort| cohort.fingerprint().to_string()),
            emitted: emitted
                .get(index)
                .map_or_else(String::new, |decision| {
                    decision.cohort_fingerprint().to_string()
                }),
        });
    }

    // Identity-keyed accumulation would be faster, but the walk is over a
    // content-sorted partition and the ledger is rebuilt in content order at
    // the end, so nothing here is a function of a hash iteration order.
    let mut accumulators: Vec<Acc> = Vec::new();
    let mut raw_axis_observations = 0usize;
    let mut effective_decisions = 0usize;
    let mut unserved_decisions = 0usize;
    let mut decisions_with_a_selection = 0usize;
    let mut arities: Vec<usize> = Vec::with_capacity(partition.len());
    let mut outcomes: Vec<DecisionOutcome> = Vec::with_capacity(partition.len());

    for (index, cohort) in partition.iter().enumerate() {
        let arity = cohort.arity();
        raw_axis_observations += arity;
        if arity < 2 {
            return Err(AttributionError::DegenerateAxis {
                cohort: cohort.fingerprint().to_string(),
                arity,
            });
        }
        arities.push(arity);

        // -- the served candidate must be in this decision's own axis --
        let mut winners = Vec::new();
        for input in cohort.candidates() {
            let served = cohort.served() == Some(input.candidate());
            if served {
                winners.push(input.candidate().clone());
            }
        }
        let served = match (cohort.served(), winners.len()) {
            (Some(identity), 1) => {
                effective_decisions += 1;
                identity.clone()
            }
            (Some(identity), 0) => {
                return Err(AttributionError::ServedAbsentFromAxis {
                    cohort: cohort.fingerprint().to_string(),
                    served: label(identity),
                })
            }
            (Some(_), other) => {
                return Err(AttributionError::MultipleServedInOneDecision {
                    cohort: cohort.fingerprint().to_string(),
                    winners: other,
                })
            }
            (None, 0) => {
                unserved_decisions += 1;
                continue;
            }
            (None, other) => {
                return Err(AttributionError::MultipleServedInOneDecision {
                    cohort: cohort.fingerprint().to_string(),
                    winners: other,
                })
            }
        };

        // -- the distribution must be this cohort's --
        let distribution = emitted.get(index).ok_or_else(|| {
            AttributionError::PartitionMisaligned {
                index,
                expected: cohort.fingerprint().to_string(),
                emitted: String::from("<absent>"),
            }
        })?;
        if distribution.cohort_fingerprint() != cohort.fingerprint() {
            return Err(AttributionError::PartitionMisaligned {
                index,
                expected: cohort.fingerprint().to_string(),
                emitted: distribution.cohort_fingerprint().to_string(),
            });
        }
        if distribution.distribution().outcomes().len() != arity {
            return Err(AttributionError::PartitionMisaligned {
                index,
                expected: cohort.fingerprint().to_string(),
                emitted: format!(
                    "{} carries a {}-candidate axis over a {arity}-candidate decision",
                    distribution.cohort_fingerprint(),
                    distribution.distribution().outcomes().len()
                ),
            });
        }

        // -- the argmax, tie-broken by content and never by slice position --
        let selection = top_of(distribution).cloned();
        decisions_with_a_selection += usize::from(selection.is_some());

        for input in cohort.candidates() {
            let identity = input.candidate();
            let slot = match accumulators
                .iter()
                .position(|acc| &acc.candidate == identity)
            {
                Some(slot) => slot,
                None => {
                    accumulators.push(Acc {
                        candidate: identity.clone(),
                        axis_decisions: 0,
                        ranked_decisions: 0,
                        served: 0,
                        ranked_first: 0,
                        ranked_first_and_won: 0,
                    });
                    accumulators.len() - 1
                }
            };
            let acc = &mut accumulators[slot];
            acc.axis_decisions += 1;
            if input.is_ranked() {
                acc.ranked_decisions += 1;
            }
            if cohort.served() == Some(identity) {
                acc.served += 1;
            }
            if selection.as_ref() == Some(identity) {
                acc.ranked_first += 1;
                if cohort.served() == Some(identity) {
                    acc.ranked_first_and_won += 1;
                }
            }
        }

        outcomes.push(DecisionOutcome {
            fingerprint: cohort.fingerprint().to_string(),
            model_scored: selection.as_ref() == Some(&served),
            selection,
            served: Some(served),
        });
    }

    // Content order, so the ledger is a function of its content and not of the
    // order the walk happened to discover candidates — or decisions — in.
    accumulators.sort_by(|left, right| content_order(&left.candidate, &right.candidate));
    // 7E-2D's own `project_cohorts` already sorts its output, so a partition
    // that came through the accepted path is in this order already. Sorting here
    // as well means a caller that assembled a partition by some other route
    // still gets a ledger whose order is a function of content, and the
    // bit-reproducibility test can permute its input and demand an identical
    // measurement rather than a merely equivalent one.
    outcomes.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
    arities.sort_unstable();
    arities.dedup();

    let independence = Independence::of(
        raw_axis_observations,
        effective_decisions,
        unserved_decisions,
    );

    let mut candidates = Vec::with_capacity(accumulators.len());
    for acc in &accumulators {
        let theirs = marginal.iter().find(|row| row.candidate == acc.candidate);
        // The marginal route counts only candidates that carried a prediction,
        // so the comparison is against `ranked_decisions` and not against
        // `axis_decisions`.
        if let Some(row) = theirs {
            if row.observations != acc.ranked_decisions || row.served != acc.served {
                return Err(AttributionError::BaselineDisagrees {
                    candidate: label(&acc.candidate),
                    theirs: row.observations,
                    their_served: row.served,
                    ours: acc.ranked_decisions,
                    our_served: acc.served,
                });
            }
        } else if acc.ranked_decisions > 0 {
            // A candidate that carried a prediction must have a marginal row:
            // the marginal route is measured over exactly the ranked axis
            // entries, so a missing row means the two routes are not describing
            // the same observations. Refused rather than substituted, because
            // substituting a self-computed rate here is precisely the
            // "invent a baseline" failure the contract forbids.
            return Err(AttributionError::BaselineUnavailable {
                candidate: label(&acc.candidate),
            });
        }
        // A candidate with no ranked observation anywhere legitimately has no
        // marginal row: it was in the axis and received exactly `0.0` mass. Its
        // `unconditional_base_rate` stays `None` and the output says so.
        candidates.push(CandidateCredit {
            candidate: acc.candidate.clone(),
            axis_decisions: acc.axis_decisions,
            ranked_decisions: acc.ranked_decisions,
            served: acc.served,
            ranked_first: acc.ranked_first,
            ranked_first_and_won: acc.ranked_first_and_won,
            base_rate_observations: theirs.map_or(0, |row| row.observations),
            base_rate_served: theirs.map_or(0, |row| row.served),
            unconditional_base_rate: theirs.map(|row| row.observed_frequency),
            calibration_gap: theirs.map(|row| row.gap),
        });
    }

    Ok(CreditLedger {
        independence,
        candidates,
        observed_arities: arities,
        decisions_with_a_selection,
        outcomes,
    })
}

/// The index of the highest-mass outcome, ties broken by content order.
///
/// Ties are broken on `(provider, model)` ascending rather than on the lower
/// index, so the selection cannot depend on the order the axis happened to be
/// built in. Two distributions with equal mass therefore always select the same
/// candidate.
fn top_of(emitted: &EmittedDecision) -> Option<&CandidateIdentity> {
    let distribution = emitted.distribution();
    let mut best: Option<(f64, &CandidateIdentity)> = None;
    for (index, outcome) in distribution.outcomes().iter().enumerate() {
        let mass = distribution
            .probabilities()
            .get(index)
            .copied()
            .unwrap_or(0.0);
        let replace = match best {
            None => true,
            Some((best_mass, best_identity)) => {
                mass > best_mass
                    || (mass == best_mass
                        && content_order(outcome, best_identity) == std::cmp::Ordering::Less)
            }
        };
        if replace {
            best = Some((mass, outcome));
        }
    }
    best.map(|(_, identity)| identity)
}

/// The one total order every content-derived sequence in this node uses.
///
/// Declared rather than derived, so it never depends on a `#[derive]` and never
/// on the layout of a struct.
pub(crate) fn content_order(
    left: &CandidateIdentity,
    right: &CandidateIdentity,
) -> std::cmp::Ordering {
    left.provider()
        .cmp(right.provider())
        .then_with(|| left.model().cmp(right.model()))
}

pub(crate) fn label(identity: &CandidateIdentity) -> String {
    format!("{}/{}", identity.provider(), identity.model())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(model: &str, provider: &str) -> CandidateIdentity {
        CandidateIdentity::new(model, provider)
    }

    #[test]
    fn content_order_is_a_total_order_on_content() {
        let a = identity("alpha", "prov-a");
        let b = identity("bravo", "prov-b");
        assert_eq!(content_order(&a, &b), std::cmp::Ordering::Less);
        assert_eq!(content_order(&b, &a), std::cmp::Ordering::Greater);
        assert_eq!(content_order(&a, &a), std::cmp::Ordering::Equal);
        // Same model, different provider: the provider decides, so the order is
        // not a function of the model name alone.
        let c = identity("alpha", "prov-z");
        assert_eq!(content_order(&a, &c), std::cmp::Ordering::Less);
    }

    #[test]
    fn the_inflation_ratio_is_named_and_is_k_for_a_uniform_axis() {
        let independence = Independence::of(72, 24, 0);
        assert_eq!(independence.raw_axis_observations, 72);
        assert_eq!(independence.effective_decisions, 24);
        assert_eq!(independence.inflation, Some(3.0));
        assert!(Independence::of(9, 0, 4).inflation.is_none());
    }
}
