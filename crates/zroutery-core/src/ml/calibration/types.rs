use std::fmt;

use serde::Serialize;

use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// UnrankedReason / CandidateInput
// ---------------------------------------------------------------------------

/// Why a candidate is in the axis but carries no prediction.
///
/// Typed rather than free text, because "this candidate has no mass" and "this
/// candidate has a small mass" are different facts and a reader must be able to
/// tell them apart without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnrankedReason {
    /// The candidate was considered and refused before the request ran.
    NotEligible,
    /// The candidate was in the set but no prediction could be formed.
    InsufficientEvidence,
    /// The model behind the candidate has never been trained.
    ModelCold,
}

impl UnrankedReason {
    /// A stable label, used in reports and error text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotEligible => "not_eligible",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::ModelCold => "model_cold",
        }
    }
}

impl fmt::Display for UnrankedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One candidate in one decision's K axis.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "role")]
pub enum CandidateInput {
    /// The model produced a success probability for this candidate.
    Ranked {
        /// Which candidate this is.
        candidate: CandidateIdentity,
        /// The success head's raw output, before any calibration.
        raw_success_probability: f64,
    },
    /// The candidate is in the axis but carries no prediction, and therefore
    /// receives exactly `0.0` mass without moving anyone else's.
    Unranked {
        /// Which candidate this is.
        candidate: CandidateIdentity,
        /// Why there is no prediction.
        reason: UnrankedReason,
    },
}

impl CandidateInput {
    /// The candidate identity, ranked or not.
    pub fn candidate(&self) -> &CandidateIdentity {
        match self {
            Self::Ranked { candidate, .. } | Self::Unranked { candidate, .. } => candidate,
        }
    }

    /// The raw success probability, or `None` when unranked.
    pub fn raw_success_probability(&self) -> Option<f64> {
        match self {
            Self::Ranked {
                raw_success_probability,
                ..
            } => Some(*raw_success_probability),
            Self::Unranked { .. } => None,
        }
    }

    /// Whether this candidate carries a prediction.
    pub const fn is_ranked(&self) -> bool {
        matches!(self, Self::Ranked { .. })
    }

    /// The unranked reason, or `None` when ranked.
    pub fn unranked_reason(&self) -> Option<UnrankedReason> {
        match self {
            Self::Ranked { .. } => None,
            Self::Unranked { reason, .. } => Some(*reason),
        }
    }
}

// ---------------------------------------------------------------------------
// Cohort scope types
// ---------------------------------------------------------------------------

/// Which partition a number or a refusal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionKind {
    /// The partition the calibrator is fitted on.
    Fit,
    /// The partition the calibrator is measured on. Disjoint from the fit
    /// partition by construction.
    Holdout,
}

impl PartitionKind {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Holdout => "holdout",
        }
    }
}

impl fmt::Display for PartitionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a partition cannot support a calibration claim.
///
/// These are the K-way analogues of the degeneracy 7E-2B refused on. The
/// single-winner case is the K-way version of the single-class case: if one
/// candidate serves every cohort, then every other candidate's observed
/// frequency is exactly zero, so a model that is *uniformly and confidently
/// wrong* about them looks calibrated, and a cold answer of `1/K` scores the
/// entropy of the base rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegeneracyReason {
    /// Fewer cohorts than the configuration's floor.
    TooFewCohorts,
    /// No candidate anywhere in the partition carries a prediction.
    NoRankedCandidateAnywhere,
    /// No cohort in the partition was served, so every observed frequency is
    /// zero and any non-zero mass is confidently wrong.
    NoAttributedOutcome,
    /// Only one candidate ever served, so the axis cannot be told apart.
    SingleServedCandidate,
    /// Fewer attributed cohorts than the configured floor.
    TooFewAttributedOutcomes,
}

impl DegeneracyReason {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TooFewCohorts => "too_few_cohorts",
            Self::NoRankedCandidateAnywhere => "no_ranked_candidate_anywhere",
            Self::NoAttributedOutcome => "no_attributed_outcome",
            Self::SingleServedCandidate => "single_served_candidate",
            Self::TooFewAttributedOutcomes => "too_few_attributed_outcomes",
        }
    }
}

impl fmt::Display for DegeneracyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
