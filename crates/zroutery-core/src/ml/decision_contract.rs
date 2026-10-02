//! The candidate-aware decision contract: typed input, model, state, candidate,
//! and K-way distribution types for one decision.
//!
//! # Relationship to the accepted engine surface
//!
//! [`DecisionEngine`](super::decision_engine::DecisionEngine) and its
//! [`EngineInput`](super::decision_engine::EngineInput) /
//! [`EngineCandidate`](super::decision_engine::EngineCandidate) /
//! [`CandidateOutcome`](super::decision_engine::CandidateOutcome) /
//! [`EngineOutput`](super::decision_engine::EngineOutput) surface is an
//! accepted baseline: an exact, panic-free reproduction of the frozen
//! Coordinator decision semantics. This module **sits beside** that surface and
//! does not wrap, replace, shadow, or re-implement any of it. No type in
//! [`decision_engine`](super::decision_engine) is changed here, and no public
//! signature in it changes.
//!
//! The two surfaces meet at exactly one point, and it is upstream of the engine:
//!
//! ```text
//! retained ShadowInput
//!   -> ModelInput::try_from_shadow_input   (validate; copy vectors verbatim)
//!     -> DecisionModel::try_score_input    (predict per dimension)
//!       -> Vec<CandidateScore>
//!         -> CandidateScore::to_prediction_bundle   (data projection only)
//!           -> EngineCandidate -> DecisionEngine  (classify, score, decide)
//! ```
//!
//! The engine remains the sole authority for candidate classification
//! (`ineligible` / `non-finite prediction` / `non-finite utility`), utility
//! computation, and the guard order. The contract only decides what a candidate
//! *is*: which identity it has, whether it may join the decision set, which
//! exact feature vector it carries, and where the decision is in its own
//! lifecycle. [`CandidateScore::to_prediction_bundle`] is a field-by-field
//! projection onto the bundle shape the engine already accepts; it performs no
//! scoring, no classification, and no selection.
//!
//! What the engine cannot express, and this module supplies:
//!
//! - A feature vector of the wrong dimension or the wrong schema. The engine
//!   never sees one, so it has no way to reject one.
//! - An eligibility state that is typed rather than a `bool` plus an optional
//!   free-text reason, which can otherwise be simultaneously eligible and
//!   rejected.
//! - The planned / last-attempted / served identity distinction, which
//!   [`OutcomeIdentity`](crate::outcome::OutcomeIdentity) keeps in three
//!   separate slots and which a single `current_candidate: &str` collapses.
//! - Any statement about where a decision is in its own lifecycle.
//!
//! # Scope
//!
//! Types, validation, and tests only. This module contains no training, no
//! warmup, no bandit or reinforcement learning, no calibration, no model
//! update, no dataset ingestion, no production wiring, no activation, and no
//! durable journal. [`DecisionDistribution`] is the distribution *type* —
//! shape, normalization, and validation. It does not produce, fit, or
//! calibrate a distribution.

use std::collections::HashSet;
use std::fmt;

use serde::Serialize;
use thiserror::Error;

use super::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use super::model::{
    CostModel, LatencyModel, ModelState, Prediction, RoutingModel, SuccessModel, TtftModel,
};
use super::model_identity::CommitId;
use super::reward::PredictionBundle;
use super::shadow::{ShadowCandidateInput, ShadowInput, ShadowObservation};
use crate::outcome::{CandidateIdentity, OutcomeIdentity};
use crate::policy::{PolicyRevision, TaskProfileSummary};
use crate::session::SessionRoutingMode;

// ---------------------------------------------------------------------------
// DecisionContractError
// ---------------------------------------------------------------------------

/// Every way this contract refuses to produce a value.
///
/// A refusal is always an `Err`, never a repaired or sanitized value: a
/// malformed input is reported, not silently corrected. `Deserialize` is
/// deliberately not derived on the contract types, so a value cannot re-enter
/// through a round trip that skipped this validation; deserialization happens
/// at the [`ShadowInput`] boundary, which [`ModelInput::try_new`] validates.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DecisionContractError {
    #[error("feature dimension {found} does not match the contract dimension {expected}")]
    FeatureDimensionMismatch { found: usize, expected: usize },

    #[error("feature schema {found} is not the supported schema {expected}")]
    UnsupportedFeatureSchema { found: u32, expected: u32 },

    #[error(
        "candidate '{candidate}' feature schema {found} does not match the input schema {expected}"
    )]
    CandidateFeatureSchemaMismatch {
        candidate: String,
        found: u32,
        expected: u32,
    },

    #[error("candidate '{candidate}' feature[{index}] is not finite: {value}")]
    NonFiniteFeature {
        candidate: String,
        index: usize,
        value: f32,
    },

    #[error("model input has no candidates")]
    EmptyCandidateSet,

    #[error("candidate identity is empty (model or provider)")]
    EmptyCandidateIdentity,

    #[error("candidate identity '{0}' appears more than once in the ordered set")]
    DuplicateCandidateIdentity(String),

    #[error("ineligible candidate '{candidate}' carries no rejection evidence")]
    MissingRejectionEvidence { candidate: String },

    #[error("ineligible candidate '{candidate}' carries an empty rejection reason")]
    EmptyRejectionReason { candidate: String },

    #[error("eligible candidate '{candidate}' carries rejection evidence '{reason}'")]
    EligibleWithRejectionEvidence { candidate: String, reason: String },

    #[error("planned identity '{0}' is not among the observed candidates")]
    PlannedIdentityNotObserved(String),

    #[error(
        "supplied planned identity '{supplied}' contradicts the decision-time plan '{recorded}'"
    )]
    PlannedIdentityContradicts { supplied: String, recorded: String },

    #[error("more than one candidate occupies the planned identity slot")]
    MultiplePlannedIdentities,

    #[error("candidate role '{role}' identity is not among the observed candidates")]
    UnobservedRoleIdentity { role: &'static str },

    #[error("candidate role slot '{role}' is already held by a different identity")]
    ContradictoryRoleIdentity { role: &'static str },

    #[error("model commit is empty")]
    EmptyCommitId,

    #[error("model state for dimension {dimension} is invalid: {reason}")]
    InvalidModelState {
        dimension: &'static str,
        reason: String,
    },

    #[error("candidate '{candidate}' produced a non-finite {dimension} prediction: {value}")]
    NonFinitePrediction {
        candidate: String,
        dimension: &'static str,
        value: f64,
    },

    #[error(
        "illegal decision transition {from:?} -> {to:?}: the only legal successor is {expected:?}"
    )]
    IllegalTransition {
        from: DecisionPhase,
        to: DecisionPhase,
        expected: Option<DecisionPhase>,
    },

    #[error("decision transition to {to:?} must be recorded as {expected}")]
    SettledStepMismatch {
        to: DecisionPhase,
        expected: &'static str,
    },

    #[error("scored candidate count {scored} does not match the input's {observed}")]
    ScoredCountMismatch { scored: usize, observed: usize },

    #[error("eligible candidate count {eligible} does not match the input's {observed}")]
    EligibleCountMismatch { eligible: usize, observed: usize },

    #[error("selected identity '{0}' is not among the observed candidates")]
    SelectedIdentityNotObserved(String),

    #[error("selected identity '{0}' is not eligible for this decision")]
    SelectedIdentityIneligible(String),

    #[error("distribution has no outcomes")]
    EmptyDistribution,

    #[error("distribution arity mismatch: {outcomes} outcomes but {probabilities} probabilities")]
    DistributionArity {
        outcomes: usize,
        probabilities: usize,
    },

    #[error("distribution outcome identity is empty (model or provider)")]
    EmptyDistributionOutcome,

    #[error("distribution outcome identity '{0}' appears more than once")]
    DuplicateDistributionOutcome(String),

    #[error("distribution probability at index {index} is not finite: {value}")]
    NonFiniteProbability { index: usize, value: f64 },

    #[error("distribution probability at index {index} is negative: {value}")]
    NegativeProbability { index: usize, value: f64 },

    #[error("distribution total mass {total} is not normalized to 1 within {tolerance}")]
    DistributionNotNormalized { total: f64, tolerance: f64 },

    #[error("distribution subject '{0}' is not among its own outcomes")]
    DistributionSubjectNotAnOutcome(String),
}

/// How far a K-way probability vector may drift from a total mass of exactly
/// `1.0` and still be considered normalized.
pub const DISTRIBUTION_NORMALIZATION_TOLERANCE: f64 = 1e-9;

// ---------------------------------------------------------------------------
// CandidateRoles / CandidateEligibility / DecisionCandidate
// ---------------------------------------------------------------------------

/// The identity slots one candidate occupies.
///
/// These are the three slots [`OutcomeIdentity`] keeps distinct: the selection
/// made before any attempt, the identity of the final attempt, and the identity
/// that produced a successful terminal response. Each slot is an independent
/// fact, so the same candidate can legitimately hold more than one of them, and
/// a slot that has not been established is `false` rather than guessed.
///
/// Decision time can only establish the planned slot. The other two are
/// post-decision facts; recording them here is what keeps them from being
/// collapsed into, or silently substituted for, the planned selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
pub struct CandidateRoles {
    /// The selection made before any attempt was made.
    pub planned: bool,
    /// The identity of the final attempt the request made, if it got that far.
    pub last_attempted: bool,
    /// The identity that produced a successful terminal response, if any.
    pub served: bool,
}

impl CandidateRoles {
    /// No slot held: an alternative in the ordered decision set about which
    /// nothing has yet been decided.
    pub const fn none() -> Self {
        Self {
            planned: false,
            last_attempted: false,
            served: false,
        }
    }

    /// Whether no slot is held.
    pub const fn is_empty(&self) -> bool {
        !self.planned && !self.last_attempted && !self.served
    }
}

/// Typed candidate eligibility.
///
/// The accepted engine surface carries `eligible: bool` next to an optional
/// free-text `rejection_reason`, a pair that permits the contradictory states
/// "eligible and rejected" and "ineligible with no reason at all". This enum
/// makes the two states exclusive at the type level: an eligible candidate
/// cannot carry a reason, and a rejected candidate cannot omit one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateEligibility {
    /// May join the decision set.
    Eligible,
    /// May not join the decision set, and carries the exact production reason.
    Rejected { reason: String },
}

impl CandidateEligibility {
    /// Whether this candidate may join the decision set.
    pub fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible)
    }

    /// The production rejection reason, or `None` when eligible.
    pub fn rejection_reason(&self) -> Option<&str> {
        match self {
            Self::Eligible => None,
            Self::Rejected { reason } => Some(reason),
        }
    }

    /// Build the eligibility for one production candidate, refusing the
    /// contradictory shapes rather than resolving them.
    fn from_production(
        eligible: bool,
        rejection_reason: Option<&str>,
        candidate: &str,
    ) -> Result<Self, DecisionContractError> {
        if eligible {
            return match rejection_reason {
                None => Ok(Self::Eligible),
                Some(reason) => Err(DecisionContractError::EligibleWithRejectionEvidence {
                    candidate: candidate.to_string(),
                    reason: reason.to_string(),
                }),
            };
        }
        match rejection_reason {
            Some(reason) if !reason.trim().is_empty() => Ok(Self::Rejected {
                reason: reason.to_string(),
            }),
            Some(_) => Err(DecisionContractError::EmptyRejectionReason {
                candidate: candidate.to_string(),
            }),
            None => Err(DecisionContractError::MissingRejectionEvidence {
                candidate: candidate.to_string(),
            }),
        }
    }
}

/// One candidate in a typed decision: who it is, whether it may join the
/// decision set, and the exact feature vector it carries.
///
/// The feature vector is carried verbatim. Nothing in this module re-derives,
/// re-orders, rescales, or imputes a feature: the vector retained at decision
/// time is the vector the model is given, so a replayed decision is given the
/// same vector the original decision was.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionCandidate {
    /// Model/provider pair. The pair, not the model id alone, is the identity.
    pub identity: CandidateIdentity,
    /// Whether this candidate may join the decision set.
    pub eligibility: CandidateEligibility,
    /// The exact decision-time feature vector.
    pub features: RoutingFeatures,
    /// Which identity slots this candidate occupies.
    pub roles: CandidateRoles,
}

impl DecisionCandidate {
    /// Build a candidate that holds no identity slot yet.
    pub fn new(
        identity: CandidateIdentity,
        eligibility: CandidateEligibility,
        features: RoutingFeatures,
    ) -> Self {
        Self {
            identity,
            eligibility,
            features,
            roles: CandidateRoles::none(),
        }
    }

    /// Whether this candidate may join the decision set.
    pub fn is_eligible(&self) -> bool {
        self.eligibility.is_eligible()
    }

    /// The production rejection reason, or `None` when eligible.
    pub fn rejection_reason(&self) -> Option<&str> {
        self.eligibility.rejection_reason()
    }

    /// The model id half of the identity.
    pub fn model(&self) -> &str {
        self.identity.model()
    }

    /// The provider half of the identity.
    pub fn provider(&self) -> &str {
        self.identity.provider()
    }
}

/// Validate one retained candidate snapshot into a [`DecisionCandidate`].
///
/// The feature vector is copied, never rebuilt: the only checks are the ones
/// that would otherwise let a malformed vector reach a model unnoticed.
fn decision_candidate_from_snapshot(
    snapshot: &ShadowCandidateInput,
    feature_schema: u32,
) -> Result<DecisionCandidate, DecisionContractError> {
    if snapshot.candidate_id.trim().is_empty() || snapshot.provider_id.trim().is_empty() {
        return Err(DecisionContractError::EmptyCandidateIdentity);
    }
    if snapshot.features.schema_version != feature_schema {
        return Err(DecisionContractError::CandidateFeatureSchemaMismatch {
            candidate: snapshot.candidate_id.clone(),
            found: snapshot.features.schema_version,
            expected: feature_schema,
        });
    }
    if let Some(index) = snapshot
        .features
        .values
        .iter()
        .position(|value| !value.is_finite())
    {
        return Err(DecisionContractError::NonFiniteFeature {
            candidate: snapshot.candidate_id.clone(),
            index,
            value: snapshot.features.values[index],
        });
    }
    let eligibility = CandidateEligibility::from_production(
        snapshot.eligible,
        snapshot.rejection_reason.as_deref(),
        &snapshot.candidate_id,
    )?;
    Ok(DecisionCandidate {
        identity: CandidateIdentity::new(
            snapshot.candidate_id.clone(),
            snapshot.provider_id.clone(),
        ),
        eligibility,
        features: snapshot.features.clone(),
        roles: CandidateRoles::none(),
    })
}

/// Whether two task summaries describe the same task.
///
/// [`TaskProfileSummary`] is not `PartialEq` and the policy module is not this
/// node's to change, so the comparison is written out field by field here.
fn task_summary_equals(left: &TaskProfileSummary, right: &TaskProfileSummary) -> bool {
    left.complexity == right.complexity
        && left.task_type == right.task_type
        && left.context_tokens == right.context_tokens
        && left.estimated_output_tokens == right.estimated_output_tokens
        && left.streaming == right.streaming
        && left.has_tools == right.has_tools
        && left.has_vision == right.has_vision
        && left.required_capabilities == right.required_capabilities
}

// ---------------------------------------------------------------------------
// DecisionIdentities / ModelInput
// ---------------------------------------------------------------------------

/// The three routing identities a decision keeps distinct, mirroring
/// [`OutcomeIdentity`] field for field.
///
/// These are three separate slots, not one merged "selected" identity. A
/// decision-time input only ever establishes
/// [`DecisionIdentities::planned`]; the other two stay `None` and are never
/// inferred from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionIdentities {
    /// The selection made before any attempt was made.
    pub planned: CandidateIdentity,
    /// The identity of the final attempt, when one has been established.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempted: Option<CandidateIdentity>,
    /// The identity that produced a successful terminal response, when one has
    /// been established.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served: Option<CandidateIdentity>,
}

impl DecisionIdentities {
    /// The decision-time identities: a plan, and nothing else.
    pub fn planned_only(planned: CandidateIdentity) -> Self {
        Self {
            planned,
            last_attempted: None,
            served: None,
        }
    }
}

/// A validated, replayable decision-time input.
///
/// # Construction
///
/// [`ModelInput::try_new`] is the only constructor. It accepts a retained
/// [`ShadowInput`] and copies the feature vectors that snapshot already
/// contains; it never calls feature extraction and never consults runtime
/// stores. Every rejection is an error, not a repaired value.
///
/// # Replay
///
/// Two inputs built from the same retained snapshot are equal, and two inputs
/// built from the same candidate and session state score identically. The
/// input is therefore a stable description of *what was known when the decision
/// was taken*, which is the precondition for replaying a decision at all.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInput {
    /// Feature schema version every candidate vector in this input was encoded
    /// with.
    pub feature_schema: u32,
    /// Width of every candidate vector in this input.
    ///
    /// Carried explicitly rather than assumed from the compiled-in constant,
    /// so a snapshot whose vectors disagree with the contract's dimension is
    /// refused at construction instead of being re-derived into agreement.
    pub feature_dimension: usize,
    /// Policy identity used at decision time.
    pub policy_id: String,
    /// Client profile which selected the policy, when one did.
    pub client_id: Option<String>,
    /// Exact policy revision used at decision time.
    pub policy_revision: PolicyRevision,
    /// Task identity used at decision time.
    pub task: TaskProfileSummary,
    /// The three identity slots, kept distinct.
    pub identities: DecisionIdentities,
    /// Session routing mode (session constraints outrank utility).
    pub session_mode: SessionRoutingMode,
    /// Switches already performed in this session.
    pub session_switch_count: u32,
    /// Whether the current attempt is a fallback.
    pub is_fallback: bool,
    /// The observed candidate set, in production order. Order is evidence: it
    /// is preserved exactly and never sorted.
    pub candidates: Vec<DecisionCandidate>,
}

impl PartialEq for ModelInput {
    fn eq(&self, other: &Self) -> bool {
        self.feature_schema == other.feature_schema
            && self.feature_dimension == other.feature_dimension
            && self.policy_id == other.policy_id
            && self.client_id == other.client_id
            && self.policy_revision == other.policy_revision
            && task_summary_equals(&self.task, &other.task)
            && self.identities == other.identities
            && self.session_mode == other.session_mode
            && self.session_switch_count == other.session_switch_count
            && self.is_fallback == other.is_fallback
            && self.candidates == other.candidates
    }
}

/// The three identity slots, as an internal handle for the monotone
/// bookkeeping in [`ModelInput::mark_role`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateSlot {
    Planned,
    LastAttempted,
    Served,
}

impl CandidateSlot {
    const fn label(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::LastAttempted => "last_attempted",
            Self::Served => "served",
        }
    }

    fn set(self, roles: &mut CandidateRoles) {
        match self {
            Self::Planned => roles.planned = true,
            Self::LastAttempted => roles.last_attempted = true,
            Self::Served => roles.served = true,
        }
    }
}

impl ModelInput {
    /// Validate a retained decision-time snapshot into a typed model input.
    ///
    /// `feature_dimension` is the width the retained vectors were encoded
    /// against. It is a parameter rather than a constant because a snapshot
    /// restored from storage can disagree with the compiled-in vector width,
    /// and that disagreement is a fact to refuse, not to paper over.
    ///
    /// The snapshot's own feature vectors are copied verbatim.
    pub fn try_new(
        feature_dimension: usize,
        snapshot: ShadowInput,
    ) -> Result<Self, DecisionContractError> {
        if feature_dimension != FEATURE_DIMENSION {
            return Err(DecisionContractError::FeatureDimensionMismatch {
                found: feature_dimension,
                expected: FEATURE_DIMENSION,
            });
        }
        if snapshot.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(DecisionContractError::UnsupportedFeatureSchema {
                found: snapshot.feature_schema,
                expected: FEATURE_SCHEMA_VERSION,
            });
        }
        if snapshot.candidates.is_empty() {
            return Err(DecisionContractError::EmptyCandidateSet);
        }

        let mut candidates = Vec::with_capacity(snapshot.candidates.len());
        let mut seen: HashSet<CandidateIdentity> =
            HashSet::with_capacity(snapshot.candidates.len());
        for candidate in &snapshot.candidates {
            let typed = decision_candidate_from_snapshot(candidate, snapshot.feature_schema)?;
            if !seen.insert(typed.identity.clone()) {
                return Err(DecisionContractError::DuplicateCandidateIdentity(
                    typed.identity.model().to_string(),
                ));
            }
            candidates.push(typed);
        }

        // The snapshot records the planned selection as a model id. The provider
        // half of that identity is resolved from the candidate production
        // actually observed, so the planned slot carries a full identity rather
        // than half of one.
        let planned_model = snapshot.production_selected.clone();
        if planned_model.trim().is_empty() {
            return Err(DecisionContractError::PlannedIdentityNotObserved(
                planned_model,
            ));
        }
        let Some(planned) = candidates
            .iter()
            .find(|candidate| candidate.model() == planned_model)
            .map(|candidate| candidate.identity.clone())
        else {
            return Err(DecisionContractError::PlannedIdentityNotObserved(
                planned_model,
            ));
        };

        let mut input = Self {
            feature_schema: snapshot.feature_schema,
            feature_dimension,
            policy_id: snapshot.policy_id,
            client_id: snapshot.client_id,
            policy_revision: snapshot.policy_revision,
            task: snapshot.task,
            identities: DecisionIdentities::planned_only(planned.clone()),
            session_mode: snapshot.session_mode,
            session_switch_count: snapshot.session_switch_count,
            is_fallback: snapshot.is_fallback,
            candidates,
        };
        input.mark_role(CandidateSlot::Planned, &planned)?;
        Ok(input)
    }

    /// Build a typed model input from a retained decision-time snapshot.
    pub fn try_from_shadow_input(snapshot: &ShadowInput) -> Result<Self, DecisionContractError> {
        Self::try_new(FEATURE_DIMENSION, snapshot.clone())
    }

    /// Build a typed model input from a retained observation, which pairs the
    /// same decision-time snapshot with the pinned model commit.
    pub fn try_from_shadow_observation(
        observation: &ShadowObservation,
    ) -> Result<Self, DecisionContractError> {
        Self::try_from_shadow_input(&observation.input)
    }

    /// The planned selection's model id: the identity the accepted engine's
    /// `EngineInput::current_candidate` is keyed on.
    pub fn planned_model(&self) -> &str {
        self.identities.planned.model()
    }

    /// The number of observed candidates, in production order.
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// The candidates that may join the decision set, in production order.
    pub fn eligible_candidates(&self) -> Vec<&DecisionCandidate> {
        self.candidates
            .iter()
            .filter(|candidate| candidate.is_eligible())
            .collect()
    }

    /// The candidate holding `identity`, if it was observed.
    pub fn candidate(&self, identity: &CandidateIdentity) -> Option<&DecisionCandidate> {
        self.candidates
            .iter()
            .find(|candidate| &candidate.identity == identity)
    }

    /// Record the post-decision identities a terminal [`Outcome`] established.
    ///
    /// The planned selection is immutable: a supplied [`OutcomeIdentity`]
    /// whose `planned` slot disagrees with the decision-time plan is a
    /// contradiction about which decision this is, and is refused rather than
    /// absorbed. The attempted and served slots are recorded in their own
    /// slots, so a served identity is never written over the planned one.
    ///
    /// This is validation and bookkeeping only. It performs no routing, stores
    /// nothing, and reads no runtime state.
    pub fn try_record_outcome_identities(
        &mut self,
        outcome: &OutcomeIdentity,
    ) -> Result<(), DecisionContractError> {
        if let Some(planned) = outcome.planned.as_ref() {
            if planned != &self.identities.planned {
                return Err(DecisionContractError::PlannedIdentityContradicts {
                    supplied: format!("{}/{}", planned.model(), planned.provider()),
                    recorded: format!(
                        "{}/{}",
                        self.identities.planned.model(),
                        self.identities.planned.provider()
                    ),
                });
            }
        }
        if let Some(attempted) = outcome.last_attempted.as_ref() {
            self.check_slot_is_stable(CandidateSlot::LastAttempted, attempted)?;
            self.mark_role(CandidateSlot::LastAttempted, attempted)?;
            self.identities.last_attempted = Some(attempted.clone());
        }
        if let Some(served) = outcome.served.as_ref() {
            self.check_slot_is_stable(CandidateSlot::Served, served)?;
            self.mark_role(CandidateSlot::Served, served)?;
            self.identities.served = Some(served.clone());
        }
        Ok(())
    }

    /// Refuse a record that would move an already established identity slot to
    /// a different identity. An identity slot is a fact about one request; it
    /// is set once and re-stated, never re-assigned.
    fn check_slot_is_stable(
        &self,
        slot: CandidateSlot,
        identity: &CandidateIdentity,
    ) -> Result<(), DecisionContractError> {
        let established = match slot {
            CandidateSlot::Planned => Some(&self.identities.planned),
            CandidateSlot::LastAttempted => self.identities.last_attempted.as_ref(),
            CandidateSlot::Served => self.identities.served.as_ref(),
        };
        match established {
            Some(established) if established != identity => {
                Err(DecisionContractError::ContradictoryRoleIdentity { role: slot.label() })
            }
            _ => Ok(()),
        }
    }

    /// Set one candidate's identity slot, refusing an unobserved identity and,
    /// for the exclusive planned slot, a second holder.
    fn mark_role(
        &mut self,
        slot: CandidateSlot,
        identity: &CandidateIdentity,
    ) -> Result<(), DecisionContractError> {
        let Some(index) = self
            .candidates
            .iter()
            .position(|candidate| &candidate.identity == identity)
        else {
            return Err(DecisionContractError::UnobservedRoleIdentity { role: slot.label() });
        };
        if slot == CandidateSlot::Planned
            && self
                .candidates
                .iter()
                .enumerate()
                .any(|(other, candidate)| other != index && candidate.roles.planned)
        {
            return Err(DecisionContractError::MultiplePlannedIdentities);
        }
        slot.set(&mut self.candidates[index].roles);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DecisionDimension / CandidatePredictions / CandidateScore
// ---------------------------------------------------------------------------

/// The four per-dimension models a decision model is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionDimension {
    /// Probability the request succeeds.
    Success,
    /// Total latency in milliseconds.
    Latency,
    /// Time to first token in milliseconds.
    Ttft,
    /// Request cost.
    Cost,
}

impl DecisionDimension {
    /// Every dimension, in the fixed order the models are stored.
    pub const ALL: [DecisionDimension; 4] = [Self::Success, Self::Latency, Self::Ttft, Self::Cost];

    /// The per-dimension model name, matching [`RoutingModel::name`].
    pub const fn model_name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Latency => "latency",
            Self::Ttft => "ttft",
            Self::Cost => "cost",
        }
    }
}

/// The four per-dimension predictions for one candidate, kept beside the
/// identity they were produced for.
#[derive(Debug, Clone, Serialize)]
pub struct CandidatePredictions {
    /// Probability the request succeeds.
    pub success: Prediction,
    /// Expected total latency in milliseconds.
    pub latency: Prediction,
    /// Expected time to first token in milliseconds.
    pub ttft: Prediction,
    /// Expected request cost.
    pub cost: Prediction,
}

impl CandidatePredictions {
    /// The prediction for one dimension.
    pub fn get(&self, dimension: DecisionDimension) -> &Prediction {
        match dimension {
            DecisionDimension::Success => &self.success,
            DecisionDimension::Latency => &self.latency,
            DecisionDimension::Ttft => &self.ttft,
            DecisionDimension::Cost => &self.cost,
        }
    }

    /// The first non-finite prediction, as `(dimension, value)`, or `None`.
    fn first_non_finite(&self) -> Option<(DecisionDimension, f64)> {
        for dimension in DecisionDimension::ALL {
            let prediction = self.get(dimension);
            if !prediction.value.is_finite() {
                return Some((dimension, prediction.value));
            }
            if !prediction.confidence.is_finite() {
                return Some((dimension, prediction.confidence));
            }
        }
        None
    }
}

/// One candidate's typed score: its identity, its eligibility, and the four
/// per-dimension predictions.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateScore {
    /// The identity the predictions were produced for.
    pub identity: CandidateIdentity,
    /// The eligibility carried through from the candidate.
    pub eligibility: CandidateEligibility,
    /// The per-dimension predictions.
    pub predictions: CandidatePredictions,
}

impl CandidateScore {
    /// Project the four predictions onto the accepted engine's bundle shape.
    ///
    /// This is a field-by-field data projection and nothing more: it computes
    /// no utility, performs no classification, and selects nothing. The
    /// accepted engine remains the sole authority for all three.
    pub fn to_prediction_bundle(&self) -> PredictionBundle {
        PredictionBundle {
            candidate_model: self.identity.model().to_string(),
            candidate_provider: self.identity.provider().to_string(),
            success: self.predictions.success.clone(),
            latency: self.predictions.latency.clone(),
            ttft: self.predictions.ttft.clone(),
            cost: self.predictions.cost.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// DecisionModel
// ---------------------------------------------------------------------------

/// The typed decision model contract: four per-dimension models, one pinned
/// artifact identity, and no training surface whatsoever.
///
/// # No training
///
/// This type has no `update`, `reset`, `train`, `fit`, or `&mut self` method.
/// A contract that exposes no mutation cannot be trained, warm-started, or
/// updated by accident; learning belongs to a later node and must be given its
/// own seam rather than smuggled through this one. The per-dimension
/// [`RoutingModel`] types it holds do expose `update`, but this type holds them
/// privately and never hands out a mutable reference, so that training surface
/// is unreachable from here.
pub struct DecisionModel {
    dimension: usize,
    feature_schema: u32,
    commit: CommitId,
    success: SuccessModel,
    latency: LatencyModel,
    ttft: TtftModel,
    cost: CostModel,
}

impl fmt::Debug for DecisionModel {
    /// Structural, without dumping the weight vectors. The held
    /// per-dimension models are not `Debug`, and a diagnostics line printing
    /// thousands of weights would not be useful anyway.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecisionModel")
            .field("dimension", &self.dimension)
            .field("feature_schema", &self.feature_schema)
            .field("commit", &self.commit.as_str())
            .field("sample_counts", &self.sample_counts())
            .finish()
    }
}

impl DecisionModel {
    /// A cold contract over the four per-dimension models at `dimension`.
    ///
    /// Refuses any dimension other than [`FEATURE_DIMENSION`]: the
    /// per-dimension models would otherwise accept a weight vector of the wrong
    /// width, and a decision model scored over the wrong number of features is
    /// not a decision model.
    pub fn try_cold(
        dimension: usize,
        feature_schema: u32,
        commit: CommitId,
    ) -> Result<Self, DecisionContractError> {
        Self::validate_contract_header(dimension, feature_schema, &commit)?;
        Ok(Self {
            dimension,
            feature_schema,
            commit,
            success: SuccessModel::new(dimension),
            latency: LatencyModel::new(dimension),
            ttft: TtftModel::new(dimension),
            cost: CostModel::new(dimension),
        })
    }

    /// Load a contract from four per-dimension states.
    ///
    /// This is a load boundary, not a training one: each state is validated by
    /// its own model, which refuses a wrong algorithm, a wrong dimension, a bad
    /// checksum, and any non-finite parameter.
    pub fn try_from_states(
        dimension: usize,
        feature_schema: u32,
        commit: CommitId,
        states: &DecisionModelStates,
    ) -> Result<Self, DecisionContractError> {
        Self::validate_contract_header(dimension, feature_schema, &commit)?;
        Ok(Self {
            dimension,
            feature_schema,
            commit,
            success: SuccessModel::load(&states.success).map_err(|reason| {
                DecisionContractError::InvalidModelState {
                    dimension: DecisionDimension::Success.model_name(),
                    reason,
                }
            })?,
            latency: LatencyModel::load(&states.latency).map_err(|reason| {
                DecisionContractError::InvalidModelState {
                    dimension: DecisionDimension::Latency.model_name(),
                    reason,
                }
            })?,
            ttft: TtftModel::load(&states.ttft).map_err(|reason| {
                DecisionContractError::InvalidModelState {
                    dimension: DecisionDimension::Ttft.model_name(),
                    reason,
                }
            })?,
            cost: CostModel::load(&states.cost).map_err(|reason| {
                DecisionContractError::InvalidModelState {
                    dimension: DecisionDimension::Cost.model_name(),
                    reason,
                }
            })?,
        })
    }

    /// The shared dimension / schema / commit header every contract must
    /// satisfy before any model is built or loaded.
    fn validate_contract_header(
        dimension: usize,
        feature_schema: u32,
        commit: &CommitId,
    ) -> Result<(), DecisionContractError> {
        if dimension != FEATURE_DIMENSION {
            return Err(DecisionContractError::FeatureDimensionMismatch {
                found: dimension,
                expected: FEATURE_DIMENSION,
            });
        }
        if feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(DecisionContractError::UnsupportedFeatureSchema {
                found: feature_schema,
                expected: FEATURE_SCHEMA_VERSION,
            });
        }
        if commit.as_str().is_empty() {
            return Err(DecisionContractError::EmptyCommitId);
        }
        Ok(())
    }

    /// The feature dimension this contract scores.
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// The feature schema this contract scores.
    pub fn feature_schema(&self) -> u32 {
        self.feature_schema
    }

    /// The pinned artifact identity of the models this contract holds.
    pub fn commit(&self) -> &CommitId {
        &self.commit
    }

    /// The serialized state of one per-dimension model.
    ///
    /// Read-only: a state is what this contract *is*, and nothing here turns a
    /// state back into a mutation.
    pub fn state(&self, dimension: DecisionDimension) -> ModelState {
        match dimension {
            DecisionDimension::Success => self.success.save(),
            DecisionDimension::Latency => self.latency.save(),
            DecisionDimension::Ttft => self.ttft.save(),
            DecisionDimension::Cost => self.cost.save(),
        }
    }

    /// The training sample count seen by each per-dimension model.
    pub fn sample_counts(&self) -> [u64; 4] {
        [
            self.success.sample_count(),
            self.latency.sample_count(),
            self.ttft.sample_count(),
            self.cost.sample_count(),
        ]
    }

    /// Score one candidate.
    ///
    /// The candidate's feature schema must match this contract's, and every
    /// produced prediction must be finite. A non-finite prediction is refused,
    /// not replaced with a cold default.
    pub fn try_score_candidate(
        &self,
        candidate: &DecisionCandidate,
    ) -> Result<CandidateScore, DecisionContractError> {
        if candidate.features.schema_version != self.feature_schema {
            return Err(DecisionContractError::CandidateFeatureSchemaMismatch {
                candidate: candidate.identity.model().to_string(),
                found: candidate.features.schema_version,
                expected: self.feature_schema,
            });
        }
        let predictions = CandidatePredictions {
            success: self.success.predict(&candidate.features),
            latency: self.latency.predict(&candidate.features),
            ttft: self.ttft.predict(&candidate.features),
            cost: self.cost.predict(&candidate.features),
        };
        if let Some((dimension, value)) = predictions.first_non_finite() {
            return Err(DecisionContractError::NonFinitePrediction {
                candidate: candidate.identity.model().to_string(),
                dimension: dimension.model_name(),
                value,
            });
        }
        Ok(CandidateScore {
            identity: candidate.identity.clone(),
            eligibility: candidate.eligibility.clone(),
            predictions,
        })
    }

    /// Score every candidate of a typed input, in production order.
    ///
    /// Every candidate is scored, including ineligible ones: ineligibility is
    /// evidence about a candidate, not a reason to score it differently, and
    /// the resulting row set is what a K-way distribution over the observed
    /// candidates would be built from. The contract scores; it does not select,
    /// rank, or decide.
    pub fn try_score_input(
        &self,
        input: &ModelInput,
    ) -> Result<Vec<CandidateScore>, DecisionContractError> {
        if input.feature_schema != self.feature_schema {
            return Err(DecisionContractError::UnsupportedFeatureSchema {
                found: input.feature_schema,
                expected: self.feature_schema,
            });
        }
        if input.feature_dimension != self.dimension {
            return Err(DecisionContractError::FeatureDimensionMismatch {
                found: input.feature_dimension,
                expected: self.dimension,
            });
        }
        input
            .candidates
            .iter()
            .map(|candidate| self.try_score_candidate(candidate))
            .collect()
    }
}

/// The four per-dimension states a [`DecisionModel`] is loaded from.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionModelStates {
    /// State of the success model.
    pub success: ModelState,
    /// State of the latency model.
    pub latency: ModelState,
    /// State of the time-to-first-token model.
    pub ttft: ModelState,
    /// State of the cost model.
    pub cost: ModelState,
}

impl DecisionModelStates {
    /// The state of one dimension.
    pub fn get(&self, dimension: DecisionDimension) -> &ModelState {
        match dimension {
            DecisionDimension::Success => &self.success,
            DecisionDimension::Latency => &self.latency,
            DecisionDimension::Ttft => &self.ttft,
            DecisionDimension::Cost => &self.cost,
        }
    }

    /// The states a cold contract at `dimension` reports, so a caller can hold
    /// a well-formed but untrained state set.
    pub fn cold(dimension: usize) -> Result<Self, DecisionContractError> {
        let contract = DecisionModel::try_cold(
            dimension,
            FEATURE_SCHEMA_VERSION,
            CommitId::new("decision-contract-cold"),
        )?;
        Ok(Self {
            success: contract.state(DecisionDimension::Success),
            latency: contract.state(DecisionDimension::Latency),
            ttft: contract.state(DecisionDimension::Ttft),
            cost: contract.state(DecisionDimension::Cost),
        })
    }
}

// ---------------------------------------------------------------------------
// DecisionState
// ---------------------------------------------------------------------------

/// The work a decision still has open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenStep {
    /// Score every observed candidate.
    ScoreCandidates,
    /// Select one eligible candidate.
    SelectCandidate,
    /// Commit the selection.
    Commit,
}

/// The phases a decision passes through, in the only order it may take them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPhase {
    /// The typed input is accepted. Nothing has been scored.
    InputAccepted,
    /// Every observed candidate has been scored. None has been selected.
    CandidatesScored,
    /// One eligible candidate has been selected. The selection is not final.
    CandidateSelected,
    /// The decision is final. No transition out of this phase exists.
    Committed,
}

impl DecisionPhase {
    /// Every phase, in the only legal order.
    pub const ORDER: [DecisionPhase; 4] = [
        Self::InputAccepted,
        Self::CandidatesScored,
        Self::CandidateSelected,
        Self::Committed,
    ];

    /// Position in [`DecisionPhase::ORDER`].
    pub const fn index(self) -> usize {
        match self {
            Self::InputAccepted => 0,
            Self::CandidatesScored => 1,
            Self::CandidateSelected => 2,
            Self::Committed => 3,
        }
    }

    /// The single legal successor of this phase, or `None` at the terminal
    /// phase.
    pub fn successor(self) -> Option<Self> {
        Self::ORDER.get(self.index() + 1).copied()
    }

    /// The phases this phase may legally become next. At most one: a decision
    /// advances one phase at a time.
    pub fn allowed_transitions(self) -> &'static [Self] {
        match self {
            Self::InputAccepted => &[Self::CandidatesScored],
            Self::CandidatesScored => &[Self::CandidateSelected],
            Self::CandidateSelected => &[Self::Committed],
            Self::Committed => &[],
        }
    }

    /// The work still open at this phase.
    pub fn open_steps(self) -> &'static [OpenStep] {
        match self {
            Self::InputAccepted => &[
                OpenStep::ScoreCandidates,
                OpenStep::SelectCandidate,
                OpenStep::Commit,
            ],
            Self::CandidatesScored => &[OpenStep::SelectCandidate, OpenStep::Commit],
            Self::CandidateSelected => &[OpenStep::Commit],
            Self::Committed => &[],
        }
    }
}

/// What has been settled at one step of a decision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "step")]
pub enum SettledStep {
    /// The typed input was accepted, with this many observed candidates.
    InputAccepted {
        /// Number of candidates in the accepted input.
        candidates: usize,
    },
    /// Every candidate was scored, with this many scores and eligible
    /// candidates.
    CandidatesScored {
        /// Number of candidates scored.
        scored: usize,
        /// Number of those candidates that were eligible.
        eligible: usize,
    },
    /// This identity was selected.
    CandidateSelected {
        /// The selected identity.
        selected: CandidateIdentity,
    },
    /// The decision was committed.
    Committed,
}

/// The settled step name a phase must be recorded as.
const fn settled_step_name(phase: DecisionPhase) -> &'static str {
    match phase {
        DecisionPhase::InputAccepted => "InputAccepted",
        DecisionPhase::CandidatesScored => "CandidatesScored",
        DecisionPhase::CandidateSelected => "CandidateSelected",
        DecisionPhase::Committed => "Committed",
    }
}

/// Whether `step` is the settled step belonging to `phase`.
fn settled_step_matches(phase: DecisionPhase, step: &SettledStep) -> bool {
    matches!(
        (phase, step),
        (
            DecisionPhase::InputAccepted,
            SettledStep::InputAccepted { .. }
        ) | (
            DecisionPhase::CandidatesScored,
            SettledStep::CandidatesScored { .. }
        ) | (
            DecisionPhase::CandidateSelected,
            SettledStep::CandidateSelected { .. }
        ) | (DecisionPhase::Committed, SettledStep::Committed)
    )
}

/// Explicit state for one decision in progress.
///
/// The state answers three questions at once and cannot be made to lie about
/// any of them:
///
/// - *What has been decided*: the ordered [`DecisionState::settled_steps`]
///   log, each entry carrying the evidence that settled it.
/// - *What is still open*: [`DecisionState::open_steps`], derived from the
///   single phase field, so it can never drift from what was decided.
/// - *What the state may legally become next*:
///   [`DecisionState::allowed_transitions`], also derived from the phase.
///
/// A decision advances one phase at a time. A skipped, repeated, reversed, or
/// post-terminal transition is refused with
/// [`DecisionContractError::IllegalTransition`]; it is never ignored, and the
/// state is left exactly as it was.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionState {
    input: ModelInput,
    phase: DecisionPhase,
    settled: Vec<SettledStep>,
}

impl DecisionState {
    /// Begin a decision from a validated input.
    ///
    /// This is the only constructor: a decision state cannot exist without a
    /// typed input, so a state can never be attached to an unvalidated input,
    /// or to a differently shaped one than the decision it describes.
    pub fn begin(input: ModelInput) -> Self {
        let candidates = input.candidate_count();
        Self {
            input,
            phase: DecisionPhase::InputAccepted,
            settled: vec![SettledStep::InputAccepted { candidates }],
        }
    }

    /// The typed input this decision is about.
    pub fn input(&self) -> &ModelInput {
        &self.input
    }

    /// The identity the decision is about.
    pub fn planned(&self) -> &CandidateIdentity {
        &self.input.identities.planned
    }

    /// Where the decision currently is.
    pub fn phase(&self) -> DecisionPhase {
        self.phase
    }

    /// What has been decided, in order.
    pub fn settled_steps(&self) -> &[SettledStep] {
        &self.settled
    }

    /// What is still open, in order.
    pub fn open_steps(&self) -> &'static [OpenStep] {
        self.phase.open_steps()
    }

    /// The phases this state may legally become next.
    pub fn allowed_transitions(&self) -> &'static [DecisionPhase] {
        self.phase.allowed_transitions()
    }

    /// Whether this decision has reached its final phase.
    pub fn is_terminal(&self) -> bool {
        self.phase == DecisionPhase::Committed
    }

    /// Whether `to` is a legal next phase.
    pub fn can_advance(&self, to: DecisionPhase) -> bool {
        self.phase.successor() == Some(to)
    }

    /// Advance to `to`, recording `step` as the evidence for it.
    ///
    /// Three independent refusals apply, and none of them mutates the state:
    ///
    /// 1. `to` must be the immediate successor of the current phase.
    /// 2. `step` must be the settled step belonging to `to`; a decision cannot
    ///    record one step's evidence under another phase's name.
    /// 3. The step's evidence must match this decision's input: the scored and
    ///    eligible counts must be the input's, and the selected identity must
    ///    be an observed, eligible candidate.
    pub fn advance(
        &mut self,
        to: DecisionPhase,
        step: SettledStep,
    ) -> Result<(), DecisionContractError> {
        let expected_phase = self.phase.successor();
        if expected_phase != Some(to) {
            return Err(DecisionContractError::IllegalTransition {
                from: self.phase,
                to,
                expected: expected_phase,
            });
        }
        if !settled_step_matches(to, &step) {
            return Err(DecisionContractError::SettledStepMismatch {
                to,
                expected: settled_step_name(to),
            });
        }
        match &step {
            SettledStep::CandidatesScored { scored, eligible } => {
                let observed = self.input.candidate_count();
                if *scored != observed {
                    return Err(DecisionContractError::ScoredCountMismatch {
                        scored: *scored,
                        observed,
                    });
                }
                let expected_eligible = self.input.eligible_candidates().len();
                if *eligible != expected_eligible {
                    return Err(DecisionContractError::EligibleCountMismatch {
                        eligible: *eligible,
                        observed: expected_eligible,
                    });
                }
            }
            SettledStep::CandidateSelected { selected } => {
                let Some(candidate) = self.input.candidate(selected) else {
                    return Err(DecisionContractError::SelectedIdentityNotObserved(format!(
                        "{}/{}",
                        selected.model(),
                        selected.provider()
                    )));
                };
                if !candidate.is_eligible() {
                    return Err(DecisionContractError::SelectedIdentityIneligible(format!(
                        "{}/{}",
                        selected.model(),
                        selected.provider()
                    )));
                }
            }
            SettledStep::InputAccepted { .. } | SettledStep::Committed => {}
        }
        self.phase = to;
        self.settled.push(step);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DecisionDistribution
// ---------------------------------------------------------------------------

/// The K-way decision distribution type: shape, normalization, and validation.
///
/// This type defines what a K-way decision distribution *is*, not how one is
/// obtained. It does not fit, learn, sample, or calibrate anything, and it
/// provides no temperature or smoothing parameter. What it guarantees is that
/// any distribution which exists at all is a finite, non-negative, exactly
/// normalized probability vector over a fixed, explicitly named,
/// duplicate-free candidate set — and that it is addressed to the decision it
/// was produced for.
///
/// Nothing here derives `Serialize` or `Deserialize`: a round-tripped
/// distribution would bypass the very arity and normalization checks this type
/// exists to enforce, and a distribution nobody can yet produce is not
/// something to record. A later node that produces one owns giving it a
/// validating wire form.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionDistribution {
    subject: CandidateIdentity,
    outcomes: Vec<CandidateIdentity>,
    probabilities: Vec<f64>,
}

impl DecisionDistribution {
    /// Validate a K-way distribution.
    ///
    /// `outcomes` is the ordered K axis; `probabilities[i]` is the mass on
    /// `outcomes[i]`. `subject` is the decision this distribution is about and
    /// must be one of the outcomes, so a distribution can never be attached to
    /// a decision that did not consider the candidate it ranks first.
    pub fn try_new(
        subject: CandidateIdentity,
        outcomes: Vec<CandidateIdentity>,
        probabilities: Vec<f64>,
    ) -> Result<Self, DecisionContractError> {
        if outcomes.is_empty() {
            return Err(DecisionContractError::EmptyDistribution);
        }
        if outcomes.len() != probabilities.len() {
            return Err(DecisionContractError::DistributionArity {
                outcomes: outcomes.len(),
                probabilities: probabilities.len(),
            });
        }
        let mut seen: HashSet<&CandidateIdentity> = HashSet::with_capacity(outcomes.len());
        for outcome in &outcomes {
            if outcome.model().trim().is_empty() || outcome.provider().trim().is_empty() {
                return Err(DecisionContractError::EmptyDistributionOutcome);
            }
            if !seen.insert(outcome) {
                return Err(DecisionContractError::DuplicateDistributionOutcome(
                    format!("{}/{}", outcome.model(), outcome.provider()),
                ));
            }
        }
        for (index, probability) in probabilities.iter().enumerate() {
            if !probability.is_finite() {
                return Err(DecisionContractError::NonFiniteProbability {
                    index,
                    value: *probability,
                });
            }
            if *probability < 0.0 {
                return Err(DecisionContractError::NegativeProbability {
                    index,
                    value: *probability,
                });
            }
        }
        let total: f64 = probabilities.iter().sum();
        if (total - 1.0).abs() > DISTRIBUTION_NORMALIZATION_TOLERANCE {
            return Err(DecisionContractError::DistributionNotNormalized {
                total,
                tolerance: DISTRIBUTION_NORMALIZATION_TOLERANCE,
            });
        }
        if !outcomes.contains(&subject) {
            return Err(DecisionContractError::DistributionSubjectNotAnOutcome(
                format!("{}/{}", subject.model(), subject.provider()),
            ));
        }
        Ok(Self {
            subject,
            outcomes,
            probabilities,
        })
    }

    /// The decision this distribution is about.
    pub fn subject(&self) -> &CandidateIdentity {
        &self.subject
    }

    /// The ordered K axis.
    pub fn outcomes(&self) -> &[CandidateIdentity] {
        &self.outcomes
    }

    /// The probability vector, aligned index for index with
    /// [`DecisionDistribution::outcomes`].
    pub fn probabilities(&self) -> &[f64] {
        &self.probabilities
    }

    /// The arity K.
    pub fn k(&self) -> usize {
        self.outcomes.len()
    }

    /// The total mass, which a validated distribution reports as `1.0` within
    /// [`DISTRIBUTION_NORMALIZATION_TOLERANCE`].
    pub fn total_mass(&self) -> f64 {
        self.probabilities.iter().sum()
    }

    /// The mass on one outcome.
    pub fn probability_of(&self, outcome: &CandidateIdentity) -> Option<f64> {
        self.outcomes
            .iter()
            .position(|candidate| candidate == outcome)
            .map(|index| self.probabilities[index])
    }
}
