use std::fmt;
use std::path::PathBuf;

use crate::ml::dataset::OutcomeTrainingSample;
use crate::ml::journal::{
    CanonicalEvent, JournalError, JournalRecord, LearningJournal, RecordOutcome,
};
use crate::ml::model_identity::{CommitId, ModelId, ModelStore, ReplayEngine};

use super::snapshot::{
    format_checksum, hash_extend, hash_intent_entry, hash_string, ActivationEntry,
    ActivationPointer, SnapshotId,
};
use super::{
    ACTIVATION_POINTER_NAME, ACTIVATION_SOURCE_PREFIX, DONE_EVENT_PREFIX, EVENT_DOMAIN,
    FNV_OFFSET_BASIS, PLAN_EVENT_PREFIX, SNAPSHOT_ID_HEX_DIGITS, SNAPSHOT_ID_PREFIX, STAGE_DONE,
    STAGE_PLAN,
};

// ---------------------------------------------------------------------------
// Requests, outcomes, and the audit
// ---------------------------------------------------------------------------

/// What a caller asks to activate.
///
/// There is deliberately no `reason`, no default, and no optional field: the
/// only thing an activation request carries is the snapshot, and the snapshot
/// must already exist on disk, verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    /// The snapshot to make active.
    pub snapshot: SnapshotId,
}

impl ActivationRequest {
    /// Ask for one existing snapshot.
    pub fn new(snapshot: SnapshotId) -> Self {
        Self { snapshot }
    }
}

/// Which direction a completed operation went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationKind {
    /// A caller asked for a named snapshot.
    Activate,
    /// A caller returned to the previous snapshot.
    Rollback,
}

/// Where inside the journal a refusal happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalContext {
    /// Reading the journal to find the evidence for the activated commit.
    Evidence,
    /// Appending the intent record.
    Intent,
    /// Appending the completion record.
    Completion,
    /// Opening or reading the journal for [`ActivationStore::audit`].
    Audit,
}

impl JournalContext {
    fn label(self) -> &'static str {
        match self {
            JournalContext::Evidence => "reading the activation evidence",
            JournalContext::Intent => "appending the activation intent record",
            JournalContext::Completion => "appending the activation completion record",
            JournalContext::Audit => "opening or reading the activation journal",
        }
    }
}

/// What the pointer is known to hold when an operation fails.
///
/// This is why the intent record is appended before the flip: a refusal to
/// append it cannot have moved the pointer, and the error says so rather than
/// leaving the caller to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerState {
    /// The previous pointer is still in place: the previous snapshot is still
    /// active.
    Unchanged,
    /// The new pointer is in place: the new snapshot is active.
    Flipped,
}

impl PointerState {
    fn label(self) -> &'static str {
        match self {
            PointerState::Unchanged => "the previous snapshot is still active",
            PointerState::Flipped => "the new snapshot is active",
        }
    }
}

/// What a completed activation or rollback did.
#[derive(Debug, Clone)]
pub struct ActivationOutcome {
    /// Whether this was a plain activation or a rollback.
    pub kind: ActivationKind,
    /// The generation after the flip.
    pub generation: u64,
    /// The snapshot that is now active.
    pub activated: ActivationEntry,
    /// The snapshot a rollback would now return to.
    pub rollback_target: Option<ActivationEntry>,
    /// The journal's answer to the intent record. A `Duplicate` here is a retry
    /// after an interrupted activation, not a rewrite.
    pub intent: RecordOutcome,
    /// The journal's answer to the completion record.
    pub completion: RecordOutcome,
}

/// One activation record read back out of the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationTrace {
    /// The journal sequence, which is the order the operations happened in.
    pub sequence: u64,
    /// Whether this record is an intent or a completion.
    pub stage: ActivationStage,
    /// The durable event id.
    pub event_id: String,
    /// The snapshot this record is about.
    pub snapshot: SnapshotId,
    /// The commit it activates.
    pub commit: CommitId,
    /// The model lineage.
    pub model_id: ModelId,
    /// The lineage parent the producing event claimed, verbatim.
    pub parent_commit: Option<CommitId>,
}

/// Which half of the two-phase record a trace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStage {
    /// The intent, appended before the flip.
    Planned,
    /// The completion, appended after the flip.
    Applied,
}

impl ActivationStage {
    fn label(self) -> &'static str {
        match self {
            ActivationStage::Planned => STAGE_PLAN,
            ActivationStage::Applied => STAGE_DONE,
        }
    }
}

/// An intent record with no completion record: an activation that was cut short
/// between the two appends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingActivation {
    /// The journal sequence of the intent record.
    pub sequence: u64,
    /// The intent record's event id.
    pub event_id: String,
    /// The snapshot that was being activated.
    pub snapshot: SnapshotId,
    /// The commit that was being activated.
    pub commit: CommitId,
}

/// Where the pointer and the journal disagree.
///
/// This is the pointer compared with the last **completed** activation, so it
/// also fires in one specific, well-understood situation: the flip landed and the
/// completion record did not. In that case the pointer names the new snapshot
/// while the last completed record names the old one, and
/// [`ActivationAudit::pending`] carries the intent that explains the difference.
/// Read together, `Some(disagreement)` plus `Some(pending)` means "the flip
/// landed and the record is unfinished"; `Some(disagreement)` without a pending
/// intent means something moved the pointer that the journal does not describe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerDisagreement {
    /// The journal records a completed activation but no pointer exists.
    PointerAbsent {
        /// The snapshot the journal last completed.
        last_completed: SnapshotId,
        /// The commit it completed for.
        commit: CommitId,
    },
    /// The pointer does not name the snapshot the journal last completed.
    PointerNamesOther {
        /// What the pointer names.
        pointer: SnapshotId,
        /// What the journal last completed.
        last_completed: SnapshotId,
    },
    /// The pointer names the right snapshot with the wrong commit.
    CommitDiffers {
        /// The snapshot both name.
        snapshot: SnapshotId,
        /// The commit the pointer names.
        pointer_commit: CommitId,
        /// The commit the journal recorded.
        recorded_commit: CommitId,
    },
}

impl fmt::Display for PointerDisagreement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PointerDisagreement::PointerAbsent {
                last_completed,
                commit,
            } => write!(
                f,
                "the journal completed an activation of '{last_completed}' (commit '{commit}') but \
                 there is no {ACTIVATION_POINTER_NAME} at all"
            ),
            PointerDisagreement::PointerNamesOther {
                pointer,
                last_completed,
            } => write!(
                f,
                "{ACTIVATION_POINTER_NAME} names '{pointer}' but the journal last completed an activation of \
                 '{last_completed}'"
            ),
            PointerDisagreement::CommitDiffers {
                snapshot,
                pointer_commit,
                recorded_commit,
            } => write!(
                f,
                "{ACTIVATION_POINTER_NAME} names snapshot '{snapshot}' with commit '{pointer_commit}' but the \
                 journal recorded commit '{recorded_commit}' for it"
            ),
        }
    }
}

/// An explicit, read-only reconciliation of the pointer and the journal.
///
/// This is the call that makes a crash mid-activation a *named* state instead of
/// an inference. It reads the journal through the accepted reader, which
/// re-verifies every commit a record names, and it reports the pending intent,
/// the ordered activation history, and any disagreement. It repairs nothing.
#[derive(Debug, Clone)]
pub struct ActivationAudit {
    /// The pointer, or `None` when nothing was ever activated.
    pub pointer: Option<ActivationPointer>,
    /// Every activation record, in journal order.
    pub traces: Vec<ActivationTrace>,
    /// The most recent intent with no completion record.
    pub pending: Option<PendingActivation>,
    /// Where the pointer and the journal disagree, if they do.
    pub disagreement: Option<PointerDisagreement>,
    /// How many records the journal holds in total.
    pub journal_records: u64,
}

// ---------------------------------------------------------------------------
// ActivationError
// ---------------------------------------------------------------------------

/// Every way this module refuses. Each carries a reason; none is a panic, none
/// is a silent success, and none falls back to a default or to a cold start.
#[derive(Debug, Clone)]
pub enum ActivationError {
    /// The location could not be created or written.
    Unwritable {
        /// The path.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The path exists and is not a directory.
    NotADirectory {
        /// The path.
        path: PathBuf,
    },
    /// The path is not a snapshot this implementation can read.
    NotASnapshot {
        /// The path.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// Another handle already holds this store.
    StoreLocked {
        /// The lock path.
        path: PathBuf,
        /// Who holds it, when the lock file says.
        holder: String,
    },
    /// A snapshot name is not `snap-` plus 16 lowercase hex digits.
    MalformedSnapshotId {
        /// The rejected name.
        id: String,
    },
    /// No snapshot with that name exists.
    SnapshotNotFound {
        /// The name.
        snapshot: String,
        /// The path that was looked for.
        path: PathBuf,
    },
    /// A snapshot with that name already exists. Writing never overwrites.
    SnapshotExists {
        /// The name.
        snapshot: String,
        /// The path that is already taken.
        path: PathBuf,
    },
    /// A stored snapshot could not be parsed, or is not shaped like one.
    SnapshotUnreadable {
        /// The path.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// A stored snapshot's name does not match its content.
    IdentityMismatch {
        /// The name it was found under.
        snapshot: String,
        /// The name it claims.
        stored: String,
        /// The name its content derives.
        derived: String,
    },
    /// A stored envelope is not a version this implementation reads.
    UnsupportedSchema {
        /// Which envelope.
        component: String,
        /// The version found.
        version: u32,
        /// The version supported.
        supported: u32,
    },
    /// A stored envelope is the unversioned legacy form, which would need an
    /// explicit migration this module does not perform.
    LegacyEnvelope {
        /// Which envelope.
        component: String,
        /// The version found.
        version: u32,
    },
    /// The commit failed the accepted identity check.
    CommitRejected {
        /// The commit.
        commit: CommitId,
        /// Why.
        reason: String,
    },
    /// The commit's checkpoint failed the accepted integrity check.
    CheckpointRejected {
        /// The commit.
        commit: CommitId,
        /// Why.
        reason: String,
    },
    /// The commit's lineage does not verify in the caller's store.
    LineageRejected {
        /// The commit.
        commit: CommitId,
        /// Why.
        reason: String,
    },
    /// The activation pointer does not verify, and is never repaired.
    PointerCorrupt {
        /// The pointer path.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The pointer was written but does not read back as written.
    PointerNotDurable {
        /// The pointer path.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The generation counter cannot be advanced.
    GenerationExhausted {
        /// The pointer path.
        path: PathBuf,
        /// The generation that could not be advanced.
        generation: u64,
    },
    /// Nothing has ever been activated. A distinct, named state: not a corrupt
    /// one, and not a cold start.
    NoActiveSnapshot {
        /// The pointer path.
        path: PathBuf,
    },
    /// There is no previous snapshot to return to, and nothing was done.
    NoPreviousSnapshot {
        /// The pointer path.
        path: PathBuf,
        /// The generation the pointer is at.
        generation: u64,
    },
    /// The named snapshot is already active, so activating it again would make
    /// the rollback chain a self-loop.
    AlreadyActive {
        /// The snapshot that is already active.
        snapshot: String,
    },
    /// The activated commit is not the result of any record in the journal, so
    /// there is no evidence to record and none is invented.
    ProvenanceMissing {
        /// The commit.
        commit: CommitId,
    },
    /// The evidence for the activated commit is a degraded projection rather
    /// than the canonical samples, so the record would not be honest evidence.
    ProvenanceDegraded {
        /// The commit.
        commit: CommitId,
        /// The record whose evidence is degraded.
        event_id: String,
    },
    /// A record *names* the commit as its result but replaying its samples
    /// through the strict accepted engine does not reproduce it. The declared
    /// reference is not proof of production, so the activation is refused.
    ProvenanceUnproven {
        /// The commit the record claims to have produced.
        commit: CommitId,
        /// The record whose claim did not survive replay.
        event_id: String,
        /// What the strict replay said instead.
        reason: String,
    },
    /// A journal record claims to be an activation record but is not one.
    ForgedActivationRecord {
        /// The record's event id.
        event_id: String,
        /// Why it is not a valid activation record.
        reason: String,
    },
    /// The pointer does not name the snapshot a pending intent was about.
    PointerDisagrees {
        /// What the pointer names.
        pointer: String,
        /// What the pending intent was about.
        pending: String,
    },
    /// There is no pending intent to complete.
    NoPendingActivation {
        /// The pointer path.
        path: PathBuf,
    },
    /// The accepted journal refused. Never swallowed, never retried behind the
    /// caller's back.
    Journal {
        /// Where it happened.
        context: JournalContext,
        /// The accepted journal's own refusal.
        cause: JournalError,
        /// What the pointer held when the journal refused.
        pointer: PointerState,
    },
}

impl fmt::Display for ActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActivationError::Unwritable { path, reason } => {
                write!(f, "cannot use {}: {reason}", path.display())
            }
            ActivationError::NotADirectory { path } => {
                write!(f, "{} is not a directory", path.display())
            }
            ActivationError::NotASnapshot { path, reason } => {
                write!(f, "{} is not a snapshot: {reason}", path.display())
            }
            ActivationError::StoreLocked { path, holder } => {
                let detail = if holder.is_empty() {
                    "held by another handle".to_string()
                } else {
                    format!("held by {holder}")
                };
                write!(f, "activation lock {} is {detail}", path.display())
            }
            ActivationError::MalformedSnapshotId { id } => write!(
                f,
                "snapshot name '{id}' is not '{SNAPSHOT_ID_PREFIX}' plus {SNAPSHOT_ID_HEX_DIGITS} \
                 lowercase hex digits"
            ),
            ActivationError::SnapshotNotFound { snapshot, path } => {
                write!(f, "no snapshot '{snapshot}' at {}", path.display())
            }
            ActivationError::SnapshotExists { snapshot, path } => write!(
                f,
                "snapshot '{snapshot}' already exists at {}: a snapshot is immutable and is never \
                 overwritten",
                path.display()
            ),
            ActivationError::SnapshotUnreadable { path, reason } => {
                write!(f, "{} is unreadable: {reason}", path.display())
            }
            ActivationError::IdentityMismatch {
                snapshot,
                stored,
                derived,
            } => write!(
                f,
                "the snapshot stored as '{snapshot}' claims to be '{stored}' but its content \
                 derives '{derived}'"
            ),
            ActivationError::UnsupportedSchema {
                component,
                version,
                supported,
            } => write!(
                f,
                "unsupported {component} version {version} (supported version {supported})"
            ),
            ActivationError::LegacyEnvelope { component, version } => write!(
                f,
                "the {component} is the unversioned legacy form (version {version}); this module \
                 does not migrate artifacts and refuses them instead"
            ),
            ActivationError::CommitRejected { commit, reason } => {
                write!(f, "commit '{commit}' was rejected: {reason}")
            }
            ActivationError::CheckpointRejected { commit, reason } => {
                write!(f, "the checkpoint of commit '{commit}' was rejected: {reason}")
            }
            ActivationError::LineageRejected { commit, reason } => {
                write!(f, "the lineage of commit '{commit}' was rejected: {reason}")
            }
            ActivationError::PointerCorrupt { path, reason } => {
                write!(f, "{} does not verify: {reason}", path.display())
            }
            ActivationError::PointerNotDurable { path, reason } => write!(
                f,
                "{} was written but does not read back as written: {reason}",
                path.display()
            ),
            ActivationError::GenerationExhausted { path, generation } => write!(
                f,
                "{} is at generation {generation}, which cannot be advanced",
                path.display()
            ),
            ActivationError::NoActiveSnapshot { path } => write!(
                f,
                "{} does not exist: no snapshot has ever been activated, and nothing here falls \
                 back to a default",
                path.display()
            ),
            ActivationError::NoPreviousSnapshot { path, generation } => write!(
                f,
                "{} is at generation {generation} with no previous snapshot, so there is nothing \
                 to roll back to",
                path.display()
            ),
            ActivationError::AlreadyActive { snapshot } => write!(
                f,
                "snapshot '{snapshot}' is already active; activating it again would make the \
                 rollback chain a self-loop"
            ),
            ActivationError::ProvenanceMissing { commit } => write!(
                f,
                "commit '{commit}' is not the result of any record in the activation journal, so \
                 there is no evidence to record and none is invented"
            ),
            ActivationError::ProvenanceDegraded { commit, event_id } => write!(
                f,
                "the only record producing commit '{commit}' is event '{event_id}', whose evidence \
                 is a degraded projection rather than the canonical samples"
            ),
            ActivationError::ProvenanceUnproven {
                commit,
                event_id,
                reason,
            } => write!(
                f,
                "record '{event_id}' names commit '{commit}' as its result but the strict replay \
                 does not reproduce it: {reason}"
            ),
            ActivationError::ForgedActivationRecord { event_id, reason } => write!(
                f,
                "record '{event_id}' claims to be an activation record but {reason}"
            ),
            ActivationError::PointerDisagrees { pointer, pending } => write!(
                f,
                "{ACTIVATION_POINTER_NAME} names '{pointer}' but the pending activation was about '{pending}', \
                 so the pending intent cannot be completed"
            ),
            ActivationError::NoPendingActivation { path } => write!(
                f,
                "every activation the journal records is complete, so there is nothing to finish \
                 under {}",
                path.display()
            ),
            ActivationError::Journal {
                context,
                cause,
                pointer,
            } => write!(
                f,
                "{} failed: {cause} ({} was {})",
                context.label(),
                ACTIVATION_POINTER_NAME,
                pointer.label()
            ),
        }
    }
}

impl std::error::Error for ActivationError {}

pub(super) fn journal_error(
    context: JournalContext,
    cause: JournalError,
    pointer: PointerState,
) -> ActivationError {
    ActivationError::Journal {
        context,
        cause,
        pointer,
    }
}

// ---------------------------------------------------------------------------
// The durable identities of the two journal records
// ---------------------------------------------------------------------------

/// The event id of an activation **intent** record.
///
/// Derived from what is on disk — the generation, the entry being left, and the
/// entry being entered — so it is stable across processes, and a retry after an
/// interrupted activation is a reported no-op rather than a conflict. That is
/// idempotency without a last-write-wins, which the accepted journal requires.
/// The wall-clock time is deliberately not part of it: the intent is the
/// transition, not the keystroke.
///
/// Public because a second implementation, or an operator reconciling a
/// half-finished activation, must be able to derive the same id rather than guess
/// at it.
pub fn activation_plan_event_id(
    generation: u64,
    from: Option<&ActivationEntry>,
    to: &ActivationEntry,
) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, EVENT_DOMAIN);
    hash_string(&mut hash, STAGE_PLAN);
    hash_extend(&mut hash, &generation.to_le_bytes());
    match from {
        Some(entry) => {
            hash_extend(&mut hash, &[1]);
            hash_intent_entry(&mut hash, entry);
        }
        None => hash_extend(&mut hash, &[0]),
    }
    hash_intent_entry(&mut hash, to);
    format!("{PLAN_EVENT_PREFIX}{}", format_checksum(hash))
}

/// The event id of an activation **completion** record, derived from the intent
/// record it completes.
///
/// Deriving it from the intent id rather than from the pointer is what lets
/// [`ActivationStore::complete_pending_activation`] and the ordinary activation
/// path agree on the identity without either of them reconstructing the
/// generation.
pub fn activation_applied_event_id(planned_event_id: &str) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, EVENT_DOMAIN);
    hash_string(&mut hash, STAGE_DONE);
    hash_string(&mut hash, planned_event_id);
    format!("{DONE_EVENT_PREFIX}{}", format_checksum(hash))
}

/// The `source` an activation record carries: the stage and the snapshot
/// identity, in one namespaced field, because the accepted event has no snapshot
/// field and the record must state which snapshot it is about.
fn activation_source(stage: ActivationStage, snapshot: &SnapshotId) -> String {
    format!("{ACTIVATION_SOURCE_PREFIX}{}:{snapshot}", stage.label())
}

/// Append one activation record.
///
/// The record is a faithful re-record of the training payload of the commit being
/// activated: the canonical samples of the journal record that produced it, and
/// that record's own `parent_commit` and `result_commit` verbatim. So replaying
/// the activation record yields the same commit as replaying the training record,
/// and the training record itself is never rewritten — which is the only way an
/// append-only journal can state that an activation happened.
///
/// The accepted learning event validator requires at least one sample, so there
/// is no sample-less "activation event" to write; inventing one would be
/// fabricated evidence, so a commit with no producing record is refused.
///
/// The record that names the commit is not taken at its word: its samples are
/// replayed through the strict accepted engine first, and the activation is
/// refused ([`ActivationError::ProvenanceUnproven`]) unless that replay
/// reproduces the commit exactly.
pub(super) fn record_activation(
    journal: &mut LearningJournal,
    store: &ModelStore,
    context: JournalContext,
    pointer: PointerState,
    event_id: &str,
    stage: ActivationStage,
    entry: &ActivationEntry,
) -> Result<RecordOutcome, ActivationError> {
    let records = journal
        .read_records(store)
        .map_err(|cause| journal_error(JournalContext::Evidence, cause, pointer))?;
    let producing = records
        .iter()
        .find(|record| record.event.result_commit.as_ref() == Some(&entry.commit));
    let Some(producing) = producing else {
        return Err(ActivationError::ProvenanceMissing {
            commit: entry.commit.clone(),
        });
    };
    let Some(samples) = producing.canonical_samples() else {
        return Err(ActivationError::ProvenanceDegraded {
            commit: entry.commit.clone(),
            event_id: producing.event.event_id.clone(),
        });
    };
    // Naming a commit as a result is not proof of producing it. Before any
    // evidence is re-recorded, the record is replayed through the strict
    // accepted engine against the parent commit it declares: the parent, the
    // model lineage, the sample batch, the cumulative learning-event count, and
    // the result commit are all re-derived together, and a sample chain that
    // rebuilds anything other than the target is refused rather than recorded.
    prove_provenance(store, producing, &entry.commit)?;
    let samples: Vec<OutcomeTrainingSample> = samples.to_vec();
    let event = CanonicalEvent {
        event_id: event_id.to_string(),
        model_id: producing.event.model_id.clone(),
        samples,
        parent_commit: producing.event.parent_commit.clone(),
        result_commit: Some(entry.commit.clone()),
        created_at: producing.event.created_at,
        source: Some(activation_source(stage, &entry.snapshot)),
    };
    journal
        .record_canonical(event)
        .map_err(|cause| journal_error(context, cause, pointer))
}

/// Prove that a journal record really produced the commit it names.
///
/// This reuses the accepted strict replay engine — the same path whose
/// `EventResultMismatch` the rest of the crate relies on — instead of trusting
/// the record's own `result_commit` field. The base is the parent commit the
/// record declares, checked out of the caller's store, so the replayed child is
/// the commit that batch of samples actually produces on top of that parent:
/// parent, model lineage, sample batch, cumulative learning-event count, and
/// result are all re-derived together.
fn prove_provenance(
    store: &ModelStore,
    producing: &JournalRecord,
    commit: &CommitId,
) -> Result<(), ActivationError> {
    let unproven = |reason: String| ActivationError::ProvenanceUnproven {
        commit: commit.clone(),
        event_id: producing.event.event_id.clone(),
        reason,
    };
    let base = match producing.event.parent_commit.as_ref() {
        Some(parent) => Some(store.checkout_commit(parent).map_err(|error| {
            unproven(format!(
                "its declared parent commit '{parent}' cannot be checked out: {error}"
            ))
        })?),
        None => None,
    };
    let replayed =
        ReplayEngine::replay_verified(std::slice::from_ref(&producing.event), base.as_ref())
            .map_err(|error| unproven(error.to_string()))?;
    if replayed.commit_id != *commit {
        return Err(unproven(format!(
            "replaying its samples yields commit '{}'",
            replayed.commit_id
        )));
    }
    Ok(())
}

/// Read one record as an activation trace, or `None` when it is not one.
///
/// A record that *claims* to be an activation record, by its event id, and is not
/// one is [`ActivationError::ForgedActivationRecord`], so a hand-built record
/// cannot masquerade as one in an audit.
pub(super) fn parse_activation_record(
    record: &JournalRecord,
) -> Result<Option<ActivationTrace>, ActivationError> {
    let event_id = record.event.event_id.as_str();
    let stage = if event_id.starts_with(PLAN_EVENT_PREFIX) {
        ActivationStage::Planned
    } else if event_id.starts_with(DONE_EVENT_PREFIX) {
        ActivationStage::Applied
    } else {
        return Ok(None);
    };
    let refuse = |reason: &str| ActivationError::ForgedActivationRecord {
        event_id: event_id.to_string(),
        reason: reason.to_string(),
    };
    let source = record.event.source.as_deref().ok_or_else(|| {
        refuse("it has no source, so it does not say which snapshot it activates")
    })?;
    let rest = source
        .strip_prefix(ACTIVATION_SOURCE_PREFIX)
        .ok_or_else(|| refuse("its source does not begin with 'activation:'"))?;
    let (stage_text, snapshot_text) = rest
        .split_once(':')
        .ok_or_else(|| refuse("its source is not 'activation:<stage>:<snapshot>'"))?;
    if stage_text != stage.label() {
        return Err(refuse(&format!(
            "its source says stage '{stage_text}' but its event id says stage '{}'",
            stage.label()
        )));
    }
    let snapshot = SnapshotId::new(snapshot_text);
    if !snapshot.is_well_formed() {
        return Err(refuse(&format!(
            "its source names snapshot '{snapshot_text}', which is not a well-formed name"
        )));
    }
    let commit = record
        .event
        .result_commit
        .clone()
        .ok_or_else(|| refuse("it names no commit to activate"))?;
    Ok(Some(ActivationTrace {
        sequence: record.sequence,
        stage,
        event_id: event_id.to_string(),
        snapshot,
        commit,
        model_id: record.event.model_id.clone(),
        parent_commit: record.event.parent_commit.clone(),
    }))
}

/// The most recent intent record with no completion record.
pub(super) fn pending_intent(traces: &[ActivationTrace]) -> Option<PendingActivation> {
    traces
        .iter()
        .rev()
        .find(|trace| {
            trace.stage == ActivationStage::Planned
                && !traces
                    .iter()
                    .any(|other| other.event_id == activation_applied_event_id(&trace.event_id))
        })
        .map(|trace| PendingActivation {
            sequence: trace.sequence,
            event_id: trace.event_id.clone(),
            snapshot: trace.snapshot.clone(),
            commit: trace.commit.clone(),
        })
}
