use crate::ml::calibration::{CalibrationError, DegeneracyReason, PartitionKind};
use crate::ml::evaluation::{Divergence, NonFiniteComponent};

// ---------------------------------------------------------------------------
// OfflineGateError
// ---------------------------------------------------------------------------

/// Every way the offline gate refuses.
///
/// A refusal is a typed value carrying the numbers or the names that caused it.
/// There is no boolean returned alongside a partially-successful run, no
/// "close enough" variant, and no path that reports a measurement it could not
/// support. The variants are grouped in the order the gate reaches them, which
/// is also the order of the claims.
///
/// Payloads are boxed where a variant would otherwise dominate the enum's
/// size: a large `Err` is expensive to construct and expensive to move, and
/// this type is returned from every fallible entry point here.
#[derive(Debug, Clone, thiserror::Error)]
pub enum OfflineGateError {
    // -- nothing to decide --
    /// No recorded decision was supplied. A release verdict computed from no
    /// evidence is not a verdict.
    #[error(
        "no recorded decision to gate; a release verdict computed from nothing is not a verdict"
    )]
    NothingToGate,

    // -- the artifact --
    /// The commit is absent, or absent in a form that cannot name itself.
    #[error("the model commit is missing or unusable: {reason}")]
    MissingCommit { reason: String },

    /// The commit failed its own identity or checkpoint verification, or did
    /// not survive the bit-exact wire form this workspace needs.
    #[error("model commit {commit_id} could not be verified: {reason}")]
    UnverifiableCommit { commit_id: String, reason: String },

    /// The retained root-to-current lineage did not verify: incomplete,
    /// out of order, duplicated, cyclic, or ending somewhere other than the
    /// held commit.
    #[error("the retained lineage for commit {commit_id} does not verify: {reason}")]
    LineageRejected { commit_id: String, reason: String },

    // -- the retained decision-time input --
    /// The recorded decision carries no retained input to replay from.
    #[error("decision {decision_id} has no retained decision-time input: {reason}")]
    RetainedInputAbsent { decision_id: String, reason: String },

    /// The retained input is not the one the recorded decision was made from.
    /// Its recomputed input checksum disagrees with the recorded one, so the
    /// replay would be re-deriving something other than the original decision.
    #[error(
        "the retained input of decision {decision_id} is not the input the decision was made \
         from: recorded input checksum {recorded} but the retained input re-derives to {replayed}"
    )]
    RetainedInputDrifted {
        decision_id: String,
        recorded: u64,
        replayed: u64,
    },

    /// The retained input does not carry a usable feature vector for every
    /// candidate the Outcome touched. 7E-2D's retention-completeness refusal,
    /// surfaced rather than unwrapped.
    #[error("the retained input of decision {decision_id} is incomplete: {reason}")]
    RetainedInputIncomplete { decision_id: String, reason: String },

    // -- the recorded pair --
    /// The canonical Outcome does not belong to the decision it was paired
    /// with: its request or decision identity names a different one.
    ///
    /// Reached before the replay and before any canonical label exists, because
    /// a foreign Outcome's success, failure, cost and latency would otherwise be
    /// attributed to this decision and carried into the holdout, the calibration
    /// measurement and the release verdict.
    ///
    /// [`DatasetStore::ingest`](crate::ml::dataset::DatasetStore::ingest)
    /// refuses this pairing before it converts a sample; this gate calls the
    /// conversion directly, so it has to make the same correlation check itself
    /// rather than inherit it.
    #[error(
        "the canonical Outcome {outcome_id} does not belong to decision {decision_id}: {detail}"
    )]
    OutcomeDecisionMismatch {
        decision_id: String,
        outcome_id: String,
        detail: String,
    },

    // -- the replay --
    /// The replay produced no decision at all. The accepted seam answers `None`
    /// both when it is disabled and when it contained a fault, so the gate
    /// cannot tell those apart and will not guess which happened.
    #[error("replaying decision {decision_id} produced no decision at all")]
    ReplayProducedNoDecision { decision_id: String },

    /// The accepted seam refused the replay with its own reason.
    #[error("replaying decision {decision_id} was refused: {reason}")]
    ReplayRejected { decision_id: String, reason: String },

    /// **The headline refusal.** The replay reproduced the decision up to a
    /// component and no further. Carries the first differing component in the
    /// documented comparison order, with the recorded and replayed values in
    /// the lossless `{:?}` spelling. There is no tolerance on this variant and
    /// no amount of it would produce a different outcome.
    #[error("decision {decision_id} does not replay: {divergence}")]
    ReplayDivergence {
        decision_id: String,
        divergence: Box<Divergence>,
    },

    /// A value on the replay path is `NaN` or infinite, so no equality verdict
    /// about it would mean anything. Checked *before* any comparison: under
    /// `to_bits` two `NaN`s would otherwise compare equal and be scored a
    /// match, and under `==` they would compare unequal and hide a broken model.
    #[error("decision {decision_id} carries {component}")]
    NonFiniteMeasurement {
        decision_id: String,
        component: Box<NonFiniteComponent>,
    },

    // -- outcome authority --
    /// The replayed decision cannot be reconciled with the canonical Outcome:
    /// either the replay selected a candidate the request never attempted, or
    /// the recorded terminal state contradicts the replay.
    #[error(
        "the replayed decision for {decision_id} disagrees with the canonical Outcome: {detail}"
    )]
    TerminalStateDisagreement { decision_id: String, detail: String },

    /// The Outcome records no identity that served. The evaluation must key on
    /// the identity that actually served, and planned identity is not a
    /// substitute for it, so a decision with no served identity cannot
    /// contribute to a served-keyed evaluation and the gate says so rather
    /// than filling the gap.
    #[error(
        "Outcome {outcome_id} for decision {decision_id} records no served identity; planned \
         identity is not a substitute for the identity that served"
    )]
    ServedIdentityAbsent {
        decision_id: String,
        outcome_id: String,
    },

    // -- the holdout --
    /// The holdout shares a sample or an Outcome with the set the model was
    /// fitted on. A holdout that overlaps the fit set measures the fit.
    #[error("holdout {holdout_index} shares {kind} '{sample_id}' with the model's fit set")]
    HoldoutOverlap {
        kind: &'static str,
        sample_id: String,
        holdout_index: usize,
    },

    /// The holdout cannot support a calibration claim. 7E-2D classifies this;
    /// the gate carries the classification without re-deriving it.
    #[error(
        "the holdout is degenerate: 7E-2D reports {reason} on the {partition} partition ({detail})"
    )]
    DegenerateHoldout {
        partition: PartitionKind,
        reason: DegeneracyReason,
        detail: String,
    },

    /// There is not enough evidence present to make the claim this gate makes.
    ///
    /// This is a **presence** floor, not a significance test. Refusing here
    /// means "we cannot show this"; it never means "this is shown to be
    /// inadequate". Whether the evidence supports a release claim is node 7D's
    /// statistical methodology, not this floor's arithmetic.
    #[error("insufficient evidence to make this claim: {context} is {observed}, at least {required} is required")]
    InsufficientEvidence {
        context: &'static str,
        observed: usize,
        required: usize,
    },

    /// 7E-2D refused the calibration run for a reason of its own. Held whole,
    /// with its own message, so no refusal is flattened into a generic error.
    #[error("7E-2D refused the calibration run: {error}")]
    CalibrationRefused { error: Box<CalibrationError> },
}

impl OfflineGateError {
    /// The commit this refusal names, when the refusal is about a commit.
    ///
    /// Lets a caller attribute a refusal to an artifact without matching on
    /// every variant, and without this gate having to invent an id for the
    /// refusals that are not about one.
    pub fn commit_id(&self) -> Option<String> {
        match self {
            Self::UnverifiableCommit { commit_id, .. }
            | Self::LineageRejected { commit_id, .. } => Some(commit_id.clone()),
            _ => None,
        }
    }
}
