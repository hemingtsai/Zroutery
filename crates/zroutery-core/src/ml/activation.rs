//! Immutable model snapshots and an atomic, journaled activation pointer.
//!
//! `ModelCommit` is a type. This module is the disk.
//!
//! # What this is for
//!
//! Every accepted ML node so far has ended at a type: a checkpoint verifies, a
//! journal records, an ensemble updates in memory. None of them can answer
//! "which model is serving right now, and how do I get the previous one back?".
//! This module adds the two missing artifacts, offline and inert:
//!
//! * a **content-addressed immutable snapshot** of a verified commit, and
//! * a single-file **activation pointer** with a rollback target beside it.
//!
//! # The trap this module refuses
//!
//! An activation mechanism that anything can call is a **live activation
//! seam**, and automatic activation is not authorized. So this module is
//! built to be *complete and unreachable at the same time*:
//!
//! * There is no `Default`, no `open_default`, no environment variable, no
//!   configuration switch, and no location this module will choose. Every entry
//!   point takes an explicit caller-supplied [`Path`](std::path::Path). The
//!   absence is the design: there is nothing to flip to make this reachable.
//! * Nothing here constructs a predictor, a decision engine, or any serving
//!   handle, and nothing here calls the one live activation call in this
//!   repository or its fallible training sibling. `activate` publishes nothing
//!   to any consumer; it moves a pointer that only [`ActivationStore::read_active`]
//!   in this same module reads.
//! * Nothing runs on a timer, on a thread, at startup, on a drop, or on an
//!   HTTP request. There is no `Drop` behaviour beyond releasing the lock.
//! * The Tauri application does not enable the `ml` feature, so none of this
//!   code is even compiled into the desktop binary. The packaging consequence
//!   is stated plainly in [`ACTIVATION_ROLE`] rather than left implied.
//!
//! The mechanism is *inert by construction*, not merely unreferenced: the
//! strongest form of "unreachable" is a mechanism whose output has no reader
//! outside the module that wrote it.
//!
//! # Why the pointer is one file
//!
//! The active snapshot and its rollback target are two facts that must never
//! disagree. A crash between "write the new active" and "write the new
//! previous" would leave a rollback chain that never existed, so they are one
//! file, replaced by one [`std::fs::rename`]. A reader therefore observes
//! exactly two states — the whole old pointer or the whole new one — and never
//! a partial, a missing, or an unpaired pointer.
//!
//! # What flips, and what deliberately does not
//!
//! | Flips | Does not flip |
//! |---|---|
//! | `<root>/activation.pointer`, by rename | the predictor behind the accepted shadow swap |
//! | two appended journal records | any in-memory model, ensemble, or lineage |
//! | the generation counter, inside the pointer | configuration, registry, router, provider, server, or UI state |
//! | | the process's actual routing decisions, which are identical either way |
//!
//! # Two-phase activation, and the crash state it makes nameable
//!
//! ```text
//! verify target snapshot          (no mutation)
//! verify current active snapshot  (no mutation)
//! append   intent record          -> journal is flushed and anchored
//! rename   the pointer            -> the flip, all or nothing
//! append   completion record      -> the journal states the outcome
//! ```
//!
//! A crash between the two appends is a *distinguishable* state, not an
//! inferred one:
//!
//! | On disk | What the reader concludes |
//! |---|---|
//! | pointer verifies, names the old snapshot | the old snapshot is still active; the intent is reported as [`PendingActivation`] |
//! | pointer verifies, names the new snapshot | the new snapshot is active; the completion is pending and is appended by [`ActivationStore::complete_pending_activation`] |
//! | pointer does not verify | [`ActivationError::PointerCorrupt`] — refuse; never repair, never default |
//! | no pointer at all | [`ActivationError::NoActiveSnapshot`] — the distinct "nothing was ever activated" state |
//!
//! Because the intent record is durable *before* the rename, the journal is a
//! redundant witness outside the pointer: a rename whose directory entry was
//! not durable shows up in [`ActivationStore::audit`] as a disagreement rather
//! than as a silent regression to "never activated".
//!
//! # Activation is a NEW record, not a mutation
//!
//! The accepted journal is append-only and refuses last-write-wins, so an
//! intent and its completion are two new records and the producing training
//! record is never rewritten. Both ids are derived from what is on disk
//! ([`activation_plan_event_id`], [`activation_applied_event_id`]), so a retry
//! after a crash is a reported no-op rather than a conflict — idempotency
//! without a last-write-wins.
//!
//! The record itself is a faithful *re-record of the training payload* of the
//! commit being activated: the canonical samples of the journal record that
//! produced that commit, and that record's own `parent_commit` and
//! `result_commit` verbatim, so replaying the activation record yields the same
//! commit as replaying the training record. The accepted learning event
//! validator requires at least one sample, so a sample-less "activation event"
//! is not a thing this journal can store; re-recording the real payload is
//! the honest alternative to inventing one. A commit with no producing record
//! ([`ActivationError::ProvenanceMissing`]) or a degraded one
//! ([`ActivationError::ProvenanceDegraded`]) is refused. The cost is a second
//! copy of the payload per activation; that cost is stated rather than avoided
//! by fabricating evidence.
//!
//! Naming a commit is not the same as producing it, so the producing record is
//! *proved* before it is re-recorded: its samples are replayed through the
//! accepted strict [`ReplayEngine::replay_verified`] against the parent commit
//! it declares, and a record whose replay rebuilds a different commit is
//! [`ActivationError::ProvenanceUnproven`]. Trusting the id reference alone
//! would let a mis-paired sample batch launder itself into durable evidence.
//!
//! # Immutability
//!
//! A snapshot is written once, through a temp file, a flush, and a rename, and
//! [`ActivationStore::write_snapshot`] refuses when the name is taken
//! ([`ActivationError::SnapshotExists`]) so an existing artifact is never
//! overwritten — checked immediately before the rename, because on POSIX a
//! rename replaces. Loading never repairs: every failure is a typed refusal
//! carrying a reason, and a stored envelope whose re-derived identity disagrees
//! with its name is [`ActivationError::IdentityMismatch`] rather than a file
//! that loads anyway.
//!
//! # Why the snapshot has its own wire form
//!
//! This section is a historical record, and it is kept because the reasoning is
//! still worth having. It is NOT a statement about the current workspace.
//!
//! A trained checkpoint **could not** be written as ordinary JSON here and then
//! verified again. `serde_json` was used without its `float_roundtrip` feature,
//! so its float *parsing* was a fast path that is not correctly rounded: a
//! trained ensemble's `f64` parameters came back one or two ULP different after a
//! round trip. The accepted `ModelState::verify_checksum` hashes `f64::to_bits`
//! and the accepted commit identity hashes the checkpoint's content hash, so a
//! round-tripped checkpoint failed both — a snapshot stored as plain JSON refused
//! to load itself, and no amount of care in the surrounding mechanism would have
//! fixed it. That was measured, not assumed, and recorded as E-097.
//!
//! **The workspace now enables `float_roundtrip` workspace-wide, so plain JSON is
//! lossless here too and this is no longer true.** The wire form below is
//! therefore redundancy rather than necessity: it is still guaranteed lossless by
//! construction, and it still carries the schema envelope and the snapshot
//! identity binding that plain JSON of a bare commit would not. Keeping it is
//! deliberate. Claiming it is still *necessary* would not be.
//!
//! So [`SnapshotFile`] carries the commit as [`CommitFile`], whose model
//! parameters are 16 hex digits of their IEEE-754 bits. That is a transport
//! change only: the values are rebuilt into the accepted [`ModelCheckpoint`] and
//! then verified by the accepted checks, which remain the only validators on this
//! path. A stored parameter that is not 16 lowercase hex digits is refused rather
//! than repaired.
//!
//! # Durability
//!
//! The pointer's bytes are `sync_all`ed before the rename, following the
//! `store.rs` precedent: without the flush, a power cut could make the rename
//! durable while the data was not, leaving a new name over nothing. The
//! directory flush POSIX requires is `#[cfg(unix)]`-only, because a Windows
//! directory handle needs `FILE_FLAG_BACKUP_SEMANTICS`. The consequence is
//! bounded and fail-closed: a rename whose directory entry is lost leaves the
//! *previous* pointer in place — the previous snapshot stays active and the
//! intent is reported as pending — and never a pointer that names nothing. On
//! Windows `std::fs::rename` is a `MoveFileExW` with
//! `MOVEFILE_REPLACE_EXISTING`, which is a replace but not a guaranteed atomic
//! one, so the write path re-reads the pointer afterwards and refuses
//! ([`ActivationError::PointerNotDurable`]) rather than assuming the rename
//! landed.
//!
//! # Checksums are not authentication
//!
//! The snapshot identity, the pointer checksum, and the journal's frame
//! checksums are all FNV-1a-64. They detect accidental corruption and they make
//! a second implementation able to re-derive the format; they are not MACs and
//! not authenticity claims against an attacker who can write the directory.
//! That is the same bound the accepted journal states, and the checksum
//! functions are public for the same reason it exports `frame_checksum`: a
//! durable format only its own writer can verify is not a durable format.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::dataset::OutcomeTrainingSample;
use super::journal::{
    CanonicalEvent, JournalError, JournalMode, JournalRecord, LearningJournal, RecordOutcome,
};
use super::model::ModelState;
use super::model_identity::{
    CommitId, ModelCheckpoint, ModelCommit, ModelId, ModelStore, ReplayEngine,
    LEGACY_UNVERSIONED_SCHEMA_VERSION, MODEL_CHECKPOINT_SCHEMA_VERSION,
    MODEL_COMMIT_SCHEMA_VERSION,
};

/// Envelope version of the snapshot file written by this module.
pub const ACTIVATION_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
/// Envelope version of the activation pointer.
pub const ACTIVATION_POINTER_SCHEMA_VERSION: u32 = 1;
/// The prefix every snapshot name carries.
pub const SNAPSHOT_ID_PREFIX: &str = "snap-";
/// The file extension of a stored snapshot.
pub const SNAPSHOT_FILE_SUFFIX: &str = "json";
/// The suffix of a half-written snapshot, present only mid-write.
pub const SNAPSHOT_INCOMING_SUFFIX: &str = "incoming";
/// The activation pointer, inside the caller-supplied directory.
pub const ACTIVATION_POINTER_NAME: &str = "activation.pointer";
/// The temporary pointer target, in the same directory as the pointer.
pub const ACTIVATION_POINTER_TMP_NAME: &str = "activation.pointer.tmp";
/// The snapshot directory, inside the caller-supplied directory.
pub const SNAPSHOTS_DIR_NAME: &str = "snapshots";
/// The single-writer lock, present only while a handle is live.
pub const ACTIVATION_LOCK_NAME: &str = "activation.lock";
/// The accepted journal, inside the caller-supplied directory.
pub const JOURNAL_DIR_NAME: &str = "journal";
/// The prefix of an activation *intent* record's event id.
pub const PLAN_EVENT_PREFIX: &str = "act-plan-";
/// The prefix of an activation *completion* record's event id.
pub const DONE_EVENT_PREFIX: &str = "act-done-";
/// The prefix of the `source` field on an activation record.
pub const ACTIVATION_SOURCE_PREFIX: &str = "activation:";
/// Hex digits in a snapshot identity, matching the accepted commit identity.
pub const SNAPSHOT_ID_HEX_DIGITS: usize = 16;

/// What this module is, stated for a reader and for a reviewer.
pub const ACTIVATION_ROLE: &str = "inert snapshot store and activation pointer: it flips one \
pointer file and appends journal records, it constructs no predictor and calls no live activation \
seam, and it has no configuration switch, default location, thread, timer, or endpoint. The Tauri \
application does not enable the ml feature, so the desktop binary cannot reach any of it: \
activation is a library capability with no shipped caller";

const SNAPSHOT_DOMAIN: &[u8] = b"zroutery-model-snapshot-v1\0";
const POINTER_DOMAIN: &[u8] = b"zroutery-activation-pointer-v1\0";
const EVENT_DOMAIN: &[u8] = b"zroutery-model-activation-v1\0";
const STAGE_PLAN: &str = "plan";
const STAGE_DONE: &str = "done";
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const FIRST_GENERATION: u64 = 1;

// ---------------------------------------------------------------------------
// Checksum
// ---------------------------------------------------------------------------

fn hash_extend(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_string(hash: &mut u64, value: &str) {
    hash_extend(hash, &(value.len() as u64).to_le_bytes());
    hash_extend(hash, value.as_bytes());
}

fn hash_optional(hash: &mut u64, value: Option<&str>) {
    match value {
        Some(text) => {
            hash_extend(hash, &[1]);
            hash_string(hash, text);
        }
        None => hash_extend(hash, &[0]),
    }
}

fn format_checksum(checksum: u64) -> String {
    let mut text = String::with_capacity(SNAPSHOT_ID_HEX_DIGITS);
    for index in (0..SNAPSHOT_ID_HEX_DIGITS).rev() {
        let nibble = ((checksum >> (index * 4)) & 0xf) as usize;
        text.push(char::from(HEX_DIGITS[nibble]));
    }
    text
}

// ---------------------------------------------------------------------------
// SnapshotId
// ---------------------------------------------------------------------------

/// The content-addressed name of an immutable snapshot.
///
/// It is always `snap-` followed by [`SNAPSHOT_ID_HEX_DIGITS`] lowercase hex
/// digits, and it is checked at **every** entry point that turns one into a
/// path, because a name that reached [`Path::join`] unchecked would be a path
/// traversal. The constructor is infallible for source compatibility with the
/// accepted `CommitId::new`; the form is enforced by use, not by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Wrap a name. It is validated before it is ever used as a path.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this name is exactly the well-formed form.
    pub fn is_well_formed(&self) -> bool {
        let Some(suffix) = self.0.strip_prefix(SNAPSHOT_ID_PREFIX) else {
            return false;
        };
        suffix.len() == SNAPSHOT_ID_HEX_DIGITS
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A validated id, or a typed refusal naming what was wrong with it.
fn checked_id(id: &SnapshotId) -> Result<&SnapshotId, ActivationError> {
    if id.is_well_formed() {
        Ok(id)
    } else {
        Err(ActivationError::MalformedSnapshotId { id: id.0.clone() })
    }
}

// ---------------------------------------------------------------------------
// Snapshot identity
// ---------------------------------------------------------------------------

/// The checksum a snapshot name is derived from.
///
/// It covers every model-bearing byte: the snapshot envelope version, the
/// commit's envelope version, its content-addressed id, the checkpoint's own
/// content hash, the feature schema, the cumulative learning-event count, the
/// parent reference, and the model lineage. `created_at` and commit metadata are
/// excluded for the reason the accepted identity excludes them — they are
/// persistence detail, not model content.
///
/// Public because a second implementation must be able to re-derive and check
/// the name. It is an accidental-corruption hash, not a MAC.
pub fn snapshot_checksum(commit: &ModelCommit) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, SNAPSHOT_DOMAIN);
    hash_extend(&mut hash, &ACTIVATION_SNAPSHOT_SCHEMA_VERSION.to_le_bytes());
    hash_extend(&mut hash, &commit.schema_version.to_le_bytes());
    hash_string(&mut hash, commit.commit_id.as_str());
    hash_extend(&mut hash, &commit.checkpoint.content_hash().to_le_bytes());
    hash_extend(&mut hash, &commit.feature_schema_version.to_le_bytes());
    hash_extend(&mut hash, &commit.learning_event_count.to_le_bytes());
    hash_optional(&mut hash, commit.parent.as_ref().map(CommitId::as_str));
    hash_string(&mut hash, commit.model_id.as_str());
    hash
}

/// The name a given commit's snapshot is written under.
///
/// Two different contents cannot reach the same name through the ordinary path,
/// and if they somehow did, the loader re-derives the name *and* the accepted
/// `ModelCommit::verify` requires `commit_id == canonical_identity()`, so at
/// most one content can verify under a name: a collision is
/// [`ActivationError::IdentityMismatch`], never a wrong artifact read.
pub fn snapshot_id_for(commit: &ModelCommit) -> SnapshotId {
    SnapshotId::new(format!(
        "{SNAPSHOT_ID_PREFIX}{}",
        format_checksum(snapshot_checksum(commit))
    ))
}

// ---------------------------------------------------------------------------
// The validating wire form
// ---------------------------------------------------------------------------

/// The stored form of one snapshot.
///
/// `Deserialize` is a derive because a stored artifact is untrusted input. It
/// is **not** a way in: [`SnapshotFile::try_into_snapshot`] re-runs every check
/// the write path runs, re-validating through the accepted `ModelCommit::verify`
/// and `ReplayEngine::verify_checkpoint_integrity`, refusing the accepted
/// envelope versions, and re-deriving the content address.
///
/// The commit is carried as [`CommitFile`], not as a `ModelCommit`, because of
/// the reason in that type's documentation: a plain JSON encoding of a
/// checkpoint did not survive a round trip when this module was written, and
/// this form is guaranteed to rather than incidentally so.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    /// The snapshot envelope version.
    pub schema_version: u32,
    /// The name the writer claims. Re-derived and compared on load.
    pub snapshot_id: String,
    /// The complete immutable commit, bit-exactly.
    pub commit: CommitFile,
    /// Wall-clock time the snapshot was written. Informational.
    pub created_at: i64,
}

impl SnapshotFile {
    /// Rebuild a snapshot, re-running every validation the write path runs.
    pub fn try_into_snapshot(&self, claimed: &SnapshotId) -> Result<Snapshot, ActivationError> {
        if self.schema_version != ACTIVATION_SNAPSHOT_SCHEMA_VERSION {
            return Err(ActivationError::UnsupportedSchema {
                component: "snapshot envelope".to_string(),
                version: self.schema_version,
                supported: ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
            });
        }
        if self.snapshot_id != claimed.as_str() {
            return Err(ActivationError::IdentityMismatch {
                snapshot: claimed.as_str().to_string(),
                stored: self.snapshot_id.clone(),
                derived: snapshot_id_for(&self.rebuilt_commit()?)
                    .as_str()
                    .to_string(),
            });
        }
        let commit = self.rebuilt_commit()?;
        verify_commit(&commit)?;
        Ok(Snapshot {
            id: claimed.clone(),
            commit,
            created_at: self.created_at,
        })
    }

    /// Rebuild the accepted commit from the bit-exact wire form.
    ///
    /// This is the whole reason this module has a wire form: the accepted types
    /// stay the validators, and this is only the transport.
    fn rebuilt_commit(&self) -> Result<ModelCommit, ActivationError> {
        Ok(ModelCommit {
            schema_version: self.commit.schema_version,
            commit_id: CommitId::new(self.commit.commit_id.clone()),
            parent: self.commit.parent.clone().map(CommitId::new),
            model_id: ModelId::new(self.commit.model_id.clone()),
            checkpoint: self.commit.checkpoint.rebuilt()?,
            learning_event_count: self.commit.learning_event_count,
            algorithm_versions: self.commit.algorithm_versions.clone(),
            feature_schema_version: self.commit.feature_schema_version,
            created_at: self.commit.created_at,
            metadata: self
                .commit
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        })
    }
}

/// The stored form of a commit, in which the checkpoint is bit-exact.
///
/// # Why this type exists at all
///
/// It existed because a `ModelCheckpoint` **could not** be written as ordinary
/// JSON here and then verified again. `serde_json` was used without its
/// `float_roundtrip` feature, so its float parsing was a fast path that is not
/// correctly rounded: a trained ensemble's `f64` parameters came back **1–2 ULP
/// different** after a round trip. The accepted `ModelState::verify_checksum`
/// hashes `f64::to_bits`, and the accepted commit identity hashes the
/// checkpoint's content hash, so a round-tripped checkpoint failed both — a
/// snapshot stored as plain JSON refused to load itself.
///
/// **That is no longer true of this workspace.** `float_roundtrip` is enabled
/// workspace-wide, so ordinary JSON is lossless here and this type is
/// redundancy rather than necessity. It is kept because it is guaranteed
/// lossless by construction rather than by the current configuration of a
/// dependency, and because it carries the schema envelope the bare commit does
/// not.
///
/// Writing the parameters as 16 hex digits of their IEEE-754 bits removes the
/// decimal round trip entirely, and it is a transport change only: the values are
/// rebuilt into the accepted [`ModelCheckpoint`] and then verified by the
/// accepted checks, which remain the only validators in this path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFile {
    /// The accepted commit envelope version, refused unless it is current.
    pub schema_version: u32,
    /// The accepted content-addressed commit id.
    pub commit_id: String,
    /// The lineage parent, if any.
    pub parent: Option<String>,
    /// The model lineage.
    pub model_id: String,
    /// The four model states, bit-exactly.
    pub checkpoint: CheckpointFile,
    /// The cumulative learning-event count.
    pub learning_event_count: u64,
    /// The algorithm table.
    pub algorithm_versions: Vec<(String, String)>,
    /// The accepted feature schema version.
    pub feature_schema_version: u32,
    /// Wall-clock creation time. Not part of the identity.
    pub created_at: i64,
    /// Stable metadata, carried as an ordered map so the bytes are reproducible.
    pub metadata: BTreeMap<String, String>,
}

impl CommitFile {
    /// The wire form of a verified commit.
    pub fn from_commit(commit: &ModelCommit) -> Self {
        Self {
            schema_version: commit.schema_version,
            commit_id: commit.commit_id.as_str().to_string(),
            parent: commit
                .parent
                .as_ref()
                .map(|parent| parent.as_str().to_string()),
            model_id: commit.model_id.as_str().to_string(),
            checkpoint: CheckpointFile::from_checkpoint(&commit.checkpoint),
            learning_event_count: commit.learning_event_count,
            algorithm_versions: commit.algorithm_versions.clone(),
            feature_schema_version: commit.feature_schema_version,
            created_at: commit.created_at,
            metadata: commit
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

/// The stored form of a checkpoint whose four model states carry no lossy float.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    /// The accepted checkpoint envelope version, refused unless it is current.
    pub schema_version: u32,
    /// The success model.
    pub success: StateFile,
    /// The latency model.
    pub latency: StateFile,
    /// The time-to-first-token model.
    pub ttft: StateFile,
    /// The cost model.
    pub cost: StateFile,
    /// The accepted feature schema version.
    pub feature_schema_version: u32,
    /// Wall-clock creation time. Not part of the identity.
    pub created_at: i64,
}

impl CheckpointFile {
    fn from_checkpoint(checkpoint: &ModelCheckpoint) -> Self {
        Self {
            schema_version: checkpoint.schema_version,
            success: StateFile::from_state(&checkpoint.success),
            latency: StateFile::from_state(&checkpoint.latency),
            ttft: StateFile::from_state(&checkpoint.ttft),
            cost: StateFile::from_state(&checkpoint.cost),
            feature_schema_version: checkpoint.feature_schema_version,
            created_at: checkpoint.created_at,
        }
    }

    fn rebuilt(&self) -> Result<ModelCheckpoint, ActivationError> {
        Ok(ModelCheckpoint {
            schema_version: self.schema_version,
            success: self.success.rebuilt()?,
            latency: self.latency.rebuilt()?,
            ttft: self.ttft.rebuilt()?,
            cost: self.cost.rebuilt()?,
            feature_schema_version: self.feature_schema_version,
            created_at: self.created_at,
        })
    }
}

/// The stored form of one model state, with its parameters as raw bits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    /// The accepted model-state envelope version.
    pub schema_version: u32,
    /// The algorithm identifier.
    pub algorithm: String,
    /// How many updates produced it.
    pub update_count: u64,
    /// Each parameter as 16 lowercase hex digits of its IEEE-754 bits.
    pub parameters: Vec<String>,
    /// The accepted parameter checksum, over the same bits.
    pub checksum: u64,
}

impl StateFile {
    fn from_state(state: &ModelState) -> Self {
        Self {
            schema_version: state.schema_version,
            algorithm: state.algorithm.clone(),
            update_count: state.update_count,
            parameters: state
                .parameters
                .iter()
                .map(|parameter| format_checksum(parameter.to_bits()))
                .collect(),
            checksum: state.checksum,
        }
    }

    fn rebuilt(&self) -> Result<ModelState, ActivationError> {
        let mut parameters = Vec::with_capacity(self.parameters.len());
        for (index, text) in self.parameters.iter().enumerate() {
            parameters.push(f64::from_bits(parse_checksum(text).ok_or_else(|| {
                ActivationError::SnapshotUnreadable {
                    path: PathBuf::from("<model state>"),
                    reason: format!(
                        "parameter {index} is '{text}', which is not 16 lowercase hex digits of a \
                         64-bit value"
                    ),
                }
            })?));
        }
        Ok(ModelState {
            schema_version: self.schema_version,
            algorithm: self.algorithm.clone(),
            update_count: self.update_count,
            parameters,
            checksum: self.checksum,
        })
    }
}

/// Parse 16 lowercase hex digits into a 64-bit value.
fn parse_checksum(text: &str) -> Option<u64> {
    if text.len() != SNAPSHOT_ID_HEX_DIGITS {
        return None;
    }
    let mut value = 0u64;
    for byte in text.bytes() {
        let nibble = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            b'a'..=b'f' => u64::from(byte - b'a') + 10,
            _ => return None,
        };
        value = (value << 4) | nibble;
    }
    Some(value)
}

/// A verified, immutable snapshot in memory.
#[derive(Debug, Clone)]
pub struct Snapshot {
    id: SnapshotId,
    commit: ModelCommit,
    created_at: i64,
}

impl Snapshot {
    /// The content-addressed name.
    pub fn id(&self) -> &SnapshotId {
        &self.id
    }

    /// The commit this snapshot holds. Inert: it is installed nowhere.
    pub fn commit(&self) -> &ModelCommit {
        &self.commit
    }

    /// When the snapshot was written.
    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    /// The commit id this snapshot activates.
    pub fn commit_id(&self) -> &CommitId {
        &self.commit.commit_id
    }
}

/// The stored form of one activation pointer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerFile {
    /// The pointer envelope version.
    pub schema_version: u32,
    /// Strictly increasing; the first activation is generation 1.
    pub generation: u64,
    /// The active snapshot.
    pub active: PointerEntryFile,
    /// The snapshot an operator can return to, if there is one.
    pub previous: Option<PointerEntryFile>,
    /// Checksum over every other field.
    pub pointer_checksum: u64,
}

/// The stored form of one pointer entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerEntryFile {
    /// The snapshot name.
    pub snapshot: String,
    /// The commit that snapshot activates.
    pub commit: String,
    /// The model lineage.
    pub model_id: String,
    /// When this snapshot became active.
    pub activated_at: i64,
}

/// A verified activation pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPointer {
    /// Strictly increasing across activations, starting at 1.
    pub generation: u64,
    /// The active snapshot.
    pub active: ActivationEntry,
    /// The rollback target, if there is one.
    pub previous: Option<ActivationEntry>,
}

impl ActivationPointer {
    /// Rebuild the pointer, re-running every check the write path runs.
    fn try_from_file(file: PointerFile) -> Result<Self, ActivationError> {
        if file.schema_version != ACTIVATION_POINTER_SCHEMA_VERSION {
            return Err(ActivationError::UnsupportedSchema {
                component: "activation pointer".to_string(),
                version: file.schema_version,
                supported: ACTIVATION_POINTER_SCHEMA_VERSION,
            });
        }
        if file.generation < FIRST_GENERATION {
            return Err(ActivationError::PointerCorrupt {
                path: PathBuf::from(ACTIVATION_POINTER_NAME),
                reason: format!(
                    "generation {} is below the first activation generation {FIRST_GENERATION}",
                    file.generation
                ),
            });
        }
        let active = ActivationEntry::try_from_file(file.active)?;
        let previous = file
            .previous
            .map(ActivationEntry::try_from_file)
            .transpose()?;
        let computed = pointer_checksum(file.generation, &active, previous.as_ref());
        if file.pointer_checksum != computed {
            return Err(ActivationError::PointerCorrupt {
                path: PathBuf::from(ACTIVATION_POINTER_NAME),
                reason: format!(
                    "it records checksum {:016x} but its fields hash to {computed:016x}",
                    file.pointer_checksum
                ),
            });
        }
        Ok(Self {
            generation: file.generation,
            active,
            previous,
        })
    }

    /// The stored form, for the write path.
    fn to_file(&self) -> PointerFile {
        PointerFile {
            schema_version: ACTIVATION_POINTER_SCHEMA_VERSION,
            generation: self.generation,
            active: self.active.to_file(),
            previous: self.previous.as_ref().map(ActivationEntry::to_file),
            pointer_checksum: pointer_checksum(
                self.generation,
                &self.active,
                self.previous.as_ref(),
            ),
        }
    }
}

/// The checksum over a pointer's identity-bearing fields.
///
/// Public for the same reason [`snapshot_checksum`] is: a second implementation
/// must be able to check the format, and the checksum is what makes a partial
/// or hand-edited pointer loud instead of silently accepted.
pub fn pointer_checksum(
    generation: u64,
    active: &ActivationEntry,
    previous: Option<&ActivationEntry>,
) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, POINTER_DOMAIN);
    hash_extend(&mut hash, &ACTIVATION_POINTER_SCHEMA_VERSION.to_le_bytes());
    hash_extend(&mut hash, &generation.to_le_bytes());
    hash_entry(&mut hash, active);
    match previous {
        Some(entry) => {
            hash_extend(&mut hash, &[1]);
            hash_entry(&mut hash, entry);
        }
        None => hash_extend(&mut hash, &[0]),
    }
    hash
}

fn hash_entry(hash: &mut u64, entry: &ActivationEntry) {
    hash_string(hash, entry.snapshot.as_str());
    hash_string(hash, entry.commit.as_str());
    hash_string(hash, entry.model_id.as_str());
    hash_extend(hash, &entry.activated_at.to_le_bytes());
}

/// The identity part of a pointer entry, without the wall-clock time.
///
/// An activation *intent* names a transition — "from this snapshot, to that
/// one, at this generation" — and not the instant somebody typed it. Leaving
/// `activated_at` out of the intent id is what makes a retry after an
/// interrupted activation derive the same id and therefore report a no-op
/// instead of a second record or a conflict. The wall-clock time is still
/// covered, where it belongs, by the pointer checksum.
fn hash_intent_entry(hash: &mut u64, entry: &ActivationEntry) {
    hash_string(hash, entry.snapshot.as_str());
    hash_string(hash, entry.commit.as_str());
    hash_string(hash, entry.model_id.as_str());
}

/// One snapshot's place in the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationEntry {
    /// The snapshot name.
    pub snapshot: SnapshotId,
    /// The commit it activates.
    pub commit: CommitId,
    /// The model lineage.
    pub model_id: ModelId,
    /// When it became active.
    pub activated_at: i64,
}

impl ActivationEntry {
    fn try_from_file(file: PointerEntryFile) -> Result<Self, ActivationError> {
        let snapshot = SnapshotId::new(file.snapshot);
        if !snapshot.is_well_formed() {
            return Err(ActivationError::MalformedSnapshotId { id: snapshot.0 });
        }
        if file.model_id.is_empty() {
            return Err(ActivationError::PointerCorrupt {
                path: PathBuf::from(ACTIVATION_POINTER_NAME),
                reason: "a pointer entry names an empty model lineage".to_string(),
            });
        }
        Ok(Self {
            snapshot,
            commit: CommitId::new(file.commit),
            model_id: ModelId::new(file.model_id),
            activated_at: file.activated_at,
        })
    }

    fn to_file(&self) -> PointerEntryFile {
        PointerEntryFile {
            snapshot: self.snapshot.as_str().to_string(),
            commit: self.commit.as_str().to_string(),
            model_id: self.model_id.as_str().to_string(),
            activated_at: self.activated_at,
        }
    }

    /// The entry naming this snapshot, for a new activation.
    fn for_snapshot(snapshot: &Snapshot, activated_at: i64) -> Self {
        Self {
            snapshot: snapshot.id.clone(),
            commit: snapshot.commit.commit_id.clone(),
            model_id: snapshot.commit.model_id.clone(),
            activated_at,
        }
    }
}

/// The verified active snapshot and the pointer that named it.
#[derive(Debug, Clone)]
pub struct ActiveSnapshot {
    pointer: ActivationPointer,
    snapshot: Snapshot,
}

impl ActiveSnapshot {
    /// The pointer, including the rollback target.
    pub fn pointer(&self) -> &ActivationPointer {
        &self.pointer
    }

    /// The verified active snapshot.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The verified active commit.
    pub fn commit(&self) -> &ModelCommit {
        &self.snapshot.commit
    }

    /// The activation generation, which increases on every flip.
    pub fn generation(&self) -> u64 {
        self.pointer.generation
    }

    /// The snapshot a rollback would return to, if there is one.
    pub fn rollback_target(&self) -> Option<&ActivationEntry> {
        self.pointer.previous.as_ref()
    }
}

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

fn journal_error(
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
// Verification through the accepted types
// ---------------------------------------------------------------------------

/// Re-validate a commit through the accepted checks, in the order that gives
/// the most specific refusal.
fn verify_commit(commit: &ModelCommit) -> Result<(), ActivationError> {
    check_envelope_version(
        commit.schema_version,
        "commit envelope",
        MODEL_COMMIT_SCHEMA_VERSION,
    )?;
    check_envelope_version(
        commit.checkpoint.schema_version,
        "checkpoint envelope",
        MODEL_CHECKPOINT_SCHEMA_VERSION,
    )?;
    if !commit.verify() {
        return Err(ActivationError::CommitRejected {
            commit: commit.commit_id.clone(),
            reason: "the accepted ModelCommit::verify rejected its identity, schema, algorithm \
                     table, or checkpoint"
                .to_string(),
        });
    }
    if !ReplayEngine::verify_checkpoint_integrity(&commit.checkpoint) {
        return Err(ActivationError::CheckpointRejected {
            commit: commit.commit_id.clone(),
            reason: "the accepted ReplayEngine::verify_checkpoint_integrity rejected it"
                .to_string(),
        });
    }
    Ok(())
}

/// Re-verify a commit in the caller's store, so a snapshot can never name a
/// commit the store's lineage does not vouch for.
fn verify_commit_in_store(commit: &ModelCommit, store: &ModelStore) -> Result<(), ActivationError> {
    verify_commit(commit)?;
    store
        .verify_lineage(&commit.commit_id)
        .map_err(|error| ActivationError::LineageRejected {
            commit: commit.commit_id.clone(),
            reason: error.to_string(),
        })
}

/// A legacy or unknown envelope is named as such, never interpreted.
fn check_envelope_version(
    version: u32,
    component: &str,
    supported: u32,
) -> Result<(), ActivationError> {
    if version == LEGACY_UNVERSIONED_SCHEMA_VERSION {
        return Err(ActivationError::LegacyEnvelope {
            component: component.to_string(),
            version,
        });
    }
    if version != supported {
        return Err(ActivationError::UnsupportedSchema {
            component: component.to_string(),
            version,
            supported,
        });
    }
    Ok(())
}

/// Read the nested envelope versions out of unparsed bytes.
///
/// The accepted deserializers already refuse an unknown version, but they
/// refuse it as a *serde* error, which loses the difference between "this file
/// is not a snapshot at all" and "this snapshot is a legacy artifact". So the
/// versions are probed first and both refusals get their own name. A probe that
/// cannot answer falls through to the typed path, which refuses anyway.
fn probe_envelope_versions(bytes: &[u8]) -> Option<(u32, u32)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let commit = value.get("commit")?;
    let commit_version = u32::try_from(commit.get("schema_version")?.as_u64()?).ok()?;
    let checkpoint_version =
        u32::try_from(commit.get("checkpoint")?.get("schema_version")?.as_u64()?).ok()?;
    Some((commit_version, checkpoint_version))
}

// ---------------------------------------------------------------------------
// Durability primitives
// ---------------------------------------------------------------------------

/// Flush the directory entry so a rename is durable.
///
/// POSIX requires this after a rename; without it a power cut can make the new
/// name durable while the content it names is not. A Windows directory handle
/// needs `FILE_FLAG_BACKUP_SEMANTICS`, which this crate cannot obtain without a
/// dependency it is not allowed to take, so the call is unix-only rather than
/// silently pretending to have run. The consequence is bounded and fail-closed:
/// the file's own bytes are `sync_all`ed before the rename, so a lost directory
/// entry leaves the *previous* pointer in place rather than a pointer that names
/// nothing.
#[cfg(unix)]
fn sync_directory(dir: &Path) -> Result<(), ActivationError> {
    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| ActivationError::Unwritable {
            path: dir.to_path_buf(),
            reason: format!("cannot flush the directory entry: {error}"),
        })
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> Result<(), ActivationError> {
    // See the unix variant: the file is already flushed before the rename, so a
    // lost directory entry can only leave the previous pointer in place.
    Ok(())
}

/// Write bytes through a temporary file, a flush, and a rename.
///
/// `id` is carried only so an immutability refusal can name the snapshot. The
/// existence check is repeated immediately before the rename, because on POSIX a
/// rename replaces: this module must never replace a snapshot, and a check made
/// only at the start of the call would be a check made too early to be a
/// guarantee.
fn write_through_rename(
    dir: &Path,
    target: &Path,
    tmp: &Path,
    id: &SnapshotId,
    bytes: &[u8],
) -> Result<(), ActivationError> {
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp)
            .map_err(|error| ActivationError::Unwritable {
                path: tmp.to_path_buf(),
                reason: error.to_string(),
            })?;
        file.write_all(bytes)
            .map_err(|error| ActivationError::Unwritable {
                path: tmp.to_path_buf(),
                reason: error.to_string(),
            })?;
        // Flush before the rename: without it a power cut could make the rename
        // durable while the content was not, leaving a new name over nothing.
        file.sync_all()
            .map_err(|error| ActivationError::Unwritable {
                path: tmp.to_path_buf(),
                reason: format!("cannot flush the file: {error}"),
            })?;
    }
    if target.exists() {
        let _ = std::fs::remove_file(tmp);
        return Err(ActivationError::SnapshotExists {
            snapshot: id.as_str().to_string(),
            path: target.to_path_buf(),
        });
    }
    if let Err(error) = std::fs::rename(tmp, target) {
        let _ = std::fs::remove_file(tmp);
        return Err(ActivationError::Unwritable {
            path: target.to_path_buf(),
            reason: error.to_string(),
        });
    }
    sync_directory(dir)?;
    Ok(())
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// The store lock
// ---------------------------------------------------------------------------

/// The single-writer lock. It removes the lock file on drop, which is the only
/// side effect a handle has.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // A failure here leaves a stale lock, which a later open refuses loudly.
        // That is the fail-closed direction.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(dir: &Path) -> Result<LockGuard, ActivationError> {
    let path = dir.join(ACTIVATION_LOCK_NAME);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let description = format!("pid {} opened it", std::process::id());
            let _ = writeln!(file, "{description}");
            let _ = file.sync_all();
            Ok(LockGuard { path })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let holder = std::fs::read_to_string(&path).unwrap_or_default();
            Err(ActivationError::StoreLocked {
                path,
                holder: holder.trim().to_string(),
            })
        }
        Err(error) => Err(ActivationError::Unwritable {
            path,
            reason: error.to_string(),
        }),
    }
}

fn prepare_directory(path: &Path) -> Result<(), ActivationError> {
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_dir() => Err(ActivationError::NotADirectory {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(path)
            .map_err(|error| ActivationError::Unwritable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }),
        Err(error) => Err(ActivationError::Unwritable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// ActivationStore
// ---------------------------------------------------------------------------

/// An immutable snapshot store with an atomic, journaled activation pointer
/// over a single caller-supplied directory.
///
/// The layout, and nothing else, is:
///
/// ```text
/// <root>/activation.pointer          the active snapshot and the rollback target
/// <root>/snapshots/snap-<hex>.json   the immutable snapshots
/// <root>/journal/                   the accepted 7E-2E learning journal
/// <root>/activation.lock            present only while a handle is live
/// ```
///
/// Nothing outside `<root>` is ever created, there is no default location, and
/// the handle holds the lock and nothing else.
pub struct ActivationStore {
    root: PathBuf,
    // Dropped with the handle, which releases the lock. Unread by design.
    _lock: LockGuard,
}

impl fmt::Debug for ActivationStore {
    /// Shows the directory. The lock is a file path and adds nothing a reader
    /// needs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActivationStore")
            .field("root", &self.root)
            .finish()
    }
}

impl ActivationStore {
    /// Open the store in `root`, taking the single-writer lock.
    ///
    /// This is the only constructor, and there is no other: a caller must name
    /// the location, because automatic activation is not authorized and there is
    /// no configuration switch to flip.
    ///
    /// The root and the snapshot directory are created when missing and refused
    /// when they are not directories, and an existing pointer is verified here —
    /// so a corrupt pointer refuses the open rather than being discovered later
    /// by a reader that has already decided what is active.
    pub fn open(root: &Path) -> Result<Self, ActivationError> {
        prepare_directory(root)?;
        prepare_directory(&root.join(SNAPSHOTS_DIR_NAME))?;
        // The lock moves into the returned handle, so it is released on every
        // refusal as well as on a clean return.
        let lock = acquire_lock(root)?;
        match read_pointer_file(&root.join(ACTIVATION_POINTER_NAME)) {
            Ok(_) => Ok(Self {
                root: root.to_path_buf(),
                _lock: lock,
            }),
            Err(error) => {
                drop(lock);
                Err(error)
            }
        }
    }

    /// The caller-supplied directory. Every path this module writes is inside it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The activation pointer's path.
    pub fn pointer_path(&self) -> PathBuf {
        self.root.join(ACTIVATION_POINTER_NAME)
    }

    /// The snapshot directory's path.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.root.join(SNAPSHOTS_DIR_NAME)
    }

    /// A snapshot's path, after checking the name's form.
    pub fn snapshot_path(&self, id: &SnapshotId) -> Result<PathBuf, ActivationError> {
        checked_id(id)?;
        Ok(self
            .snapshots_dir()
            .join(format!("{id}.{SNAPSHOT_FILE_SUFFIX}")))
    }

    /// The accepted journal's directory.
    pub fn journal_dir(&self) -> PathBuf {
        self.root.join(JOURNAL_DIR_NAME)
    }

    // -----------------------------------------------------------------------
    // Snapshots
    // -----------------------------------------------------------------------

    /// Write a verified commit as an immutable, content-addressed snapshot.
    ///
    /// The commit is verified before anything is written, through the accepted
    /// checks, so a snapshot on disk is a snapshot that verifies. An existing name
    /// is refused, never overwritten: the immutability guarantee holds even when
    /// the existing bytes are themselves corrupt.
    pub fn write_snapshot(&self, commit: &ModelCommit) -> Result<Snapshot, ActivationError> {
        verify_commit(commit)?;
        let id = snapshot_id_for(commit);
        let target = self.snapshot_path(&id)?;
        if target.exists() {
            return Err(ActivationError::SnapshotExists {
                snapshot: id.as_str().to_string(),
                path: target,
            });
        }
        let file = SnapshotFile {
            schema_version: ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
            snapshot_id: id.as_str().to_string(),
            commit: CommitFile::from_commit(commit),
            created_at: now_seconds(),
        };
        let bytes =
            serde_json::to_vec(&file).map_err(|error| ActivationError::SnapshotUnreadable {
                path: target.clone(),
                reason: format!("cannot serialize the snapshot: {error}"),
            })?;
        let incoming = self
            .snapshots_dir()
            .join(format!("{id}.{SNAPSHOT_INCOMING_SUFFIX}"));
        write_through_rename(&self.snapshots_dir(), &target, &incoming, &id, &bytes)?;
        // Read back and re-verify what actually landed, so a write that did not
        // survive is a typed refusal rather than a snapshot believed to exist.
        self.read_snapshot(&id)
    }

    /// Load and verify a snapshot from disk. Never repairs, never migrates.
    pub fn read_snapshot(&self, id: &SnapshotId) -> Result<Snapshot, ActivationError> {
        let path = self.snapshot_path(id)?;
        if path.is_dir() {
            return Err(ActivationError::NotASnapshot {
                path,
                reason: "there is a directory where a snapshot file belongs".to_string(),
            });
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ActivationError::SnapshotNotFound {
                    snapshot: id.as_str().to_string(),
                    path,
                })
            }
            Err(error) => {
                return Err(ActivationError::SnapshotUnreadable {
                    path,
                    reason: error.to_string(),
                })
            }
        };
        // The versions are probed *before* the typed deserialization, because the
        // accepted deserializer refuses an unknown version as a serde error; the
        // probe is what lets "this is a legacy artifact" be named rather than
        // collapsed into "unreadable".
        if let Some((commit_version, checkpoint_version)) = probe_envelope_versions(&bytes) {
            check_envelope_version(
                commit_version,
                "commit envelope",
                MODEL_COMMIT_SCHEMA_VERSION,
            )?;
            check_envelope_version(
                checkpoint_version,
                "checkpoint envelope",
                MODEL_CHECKPOINT_SCHEMA_VERSION,
            )?;
        }
        let file: SnapshotFile = serde_json::from_slice(&bytes).map_err(|error| {
            ActivationError::SnapshotUnreadable {
                path: path.clone(),
                reason: error.to_string(),
            }
        })?;
        file.try_into_snapshot(id)
    }

    // -----------------------------------------------------------------------
    // The pointer
    // -----------------------------------------------------------------------

    /// Read the verified pointer, or `None` when nothing was ever activated.
    ///
    /// A pointer that exists and does not verify is
    /// [`ActivationError::PointerCorrupt`], never a default and never a repair.
    pub fn read_pointer(&self) -> Result<Option<ActivationPointer>, ActivationError> {
        read_pointer_file(&self.pointer_path())
    }

    /// Read the active snapshot, verified end to end.
    ///
    /// This is the only reader of the pointer in the whole repository. It returns
    /// the rollback target beside the active snapshot, and it never returns
    /// anything it has not verified.
    pub fn read_active(&self) -> Result<ActiveSnapshot, ActivationError> {
        let Some(pointer) = self.read_pointer()? else {
            return Err(ActivationError::NoActiveSnapshot {
                path: self.pointer_path(),
            });
        };
        let snapshot = self.read_snapshot(&pointer.active.snapshot)?;
        if snapshot.commit.commit_id != pointer.active.commit
            || snapshot.commit.model_id != pointer.active.model_id
        {
            return Err(ActivationError::PointerCorrupt {
                path: self.pointer_path(),
                reason: format!(
                    "{} names snapshot '{}' with commit '{}' but that snapshot holds commit '{}'",
                    ACTIVATION_POINTER_NAME,
                    pointer.active.snapshot,
                    pointer.active.commit,
                    snapshot.commit.commit_id
                ),
            });
        }
        Ok(ActiveSnapshot { pointer, snapshot })
    }

    /// Refuse to continue unless the pointer is at `expected`.
    ///
    /// The file is the authority. A caller whose belief about the generation
    /// disagrees with the file is told so instead of being allowed to append an
    /// operation the file does not describe. Generation zero means nothing has
    /// ever been activated.
    pub fn expect_generation(&self, expected: u64) -> Result<(), ActivationError> {
        let actual = self.read_pointer()?.map_or(0, |pointer| pointer.generation);
        if actual == expected {
            return Ok(());
        }
        Err(ActivationError::PointerCorrupt {
            path: self.pointer_path(),
            reason: format!(
                "expected the pointer to be at generation {expected} but it is at {actual}"
            ),
        })
    }

    // -----------------------------------------------------------------------
    // Activation
    // -----------------------------------------------------------------------

    /// Make an existing snapshot active, atomically and with two journal records.
    ///
    /// The order is the whole design, and nothing is mutated until the target and
    /// the current active snapshot have both verified:
    ///
    /// 1. verify the target snapshot, and the commit's lineage in `store`;
    /// 2. verify the currently active snapshot, so the `previous` entry about to
    ///    be recorded is a snapshot that really exists and verifies;
    /// 3. append the **intent** record — the journal is now durable and says an
    ///    activation was attempted;
    /// 4. rename the pointer — the flip, all or nothing;
    /// 5. append the **completion** record.
    ///
    /// A verification failure at any point before step 3 leaves the previous
    /// active snapshot and the journal exactly as they were. A crash between
    /// steps 3 and 5 is a *named* state: the pointer is whole either way, and
    /// [`Self::audit`] reports the unmatched intent.
    ///
    /// This installs the model nowhere. It moves a pointer that only
    /// [`Self::read_active`] reads, and it constructs no predictor and no serving
    /// handle.
    pub fn activate(
        &self,
        request: &ActivationRequest,
        store: &ModelStore,
    ) -> Result<ActivationOutcome, ActivationError> {
        checked_id(&request.snapshot)?;
        self.flip(&request.snapshot, ActivationKind::Activate, store)
    }

    /// Return to the previous snapshot, atomically and with two journal records.
    ///
    /// A rollback is an activation in the opposite direction: the snapshot that
    /// was active becomes the new rollback target, so the chain ping-pongs rather
    /// than shortening. It is refused outright when there is no previous snapshot
    /// — [`ActivationError::NoPreviousSnapshot`] — rather than silently doing
    /// nothing or advancing the generation.
    pub fn rollback(&self, store: &ModelStore) -> Result<ActivationOutcome, ActivationError> {
        let pointer = self
            .read_pointer()?
            .ok_or_else(|| ActivationError::NoActiveSnapshot {
                path: self.pointer_path(),
            })?;
        let previous =
            pointer
                .previous
                .clone()
                .ok_or_else(|| ActivationError::NoPreviousSnapshot {
                    path: self.pointer_path(),
                    generation: pointer.generation,
                })?;
        self.flip(&previous.snapshot, ActivationKind::Rollback, store)
    }

    /// The two-phase flip, shared by activation and rollback.
    fn flip(
        &self,
        target: &SnapshotId,
        kind: ActivationKind,
        store: &ModelStore,
    ) -> Result<ActivationOutcome, ActivationError> {
        // 1. The target, verified before anything is touched.
        let snapshot = self.read_snapshot(target)?;
        verify_commit_in_store(snapshot.commit(), store)?;

        // 2. The current active snapshot, so the `previous` entry is truthful.
        let current = self.read_pointer()?;
        if let Some(pointer) = &current {
            if &pointer.active.snapshot == target {
                return Err(ActivationError::AlreadyActive {
                    snapshot: target.as_str().to_string(),
                });
            }
            // Verifying the current active snapshot is what stops an activation
            // from quietly papering over a corrupt active state.
            self.read_snapshot(&pointer.active.snapshot)?;
        }
        let generation = match &current {
            None => FIRST_GENERATION,
            Some(pointer) => pointer.generation.checked_add(1).ok_or_else(|| {
                ActivationError::GenerationExhausted {
                    path: self.pointer_path(),
                    generation: pointer.generation,
                }
            })?,
        };
        let from = current.as_ref().map(|pointer| pointer.active.clone());
        let activated = ActivationEntry::for_snapshot(&snapshot, now_seconds());

        // 3. The intent, flushed and anchored before the flip.
        let mut journal = self.open_journal(JournalContext::Intent)?;
        let plan_id = activation_plan_event_id(generation, from.as_ref(), &activated);
        let intent = record_activation(
            &mut journal,
            store,
            JournalContext::Intent,
            PointerState::Unchanged,
            &plan_id,
            ActivationStage::Planned,
            &activated,
        )?;

        // 4. The flip.
        let pointer = ActivationPointer {
            generation,
            active: activated.clone(),
            previous: from.clone(),
        };
        self.write_pointer(&pointer)?;

        // 5. The completion, which now has to report the flipped pointer.
        let completion_id = activation_applied_event_id(&plan_id);
        let completion = record_activation(
            &mut journal,
            store,
            JournalContext::Completion,
            PointerState::Flipped,
            &completion_id,
            ActivationStage::Applied,
            &activated,
        )?;

        Ok(ActivationOutcome {
            kind,
            generation,
            activated,
            rollback_target: from,
            intent,
            completion,
        })
    }

    /// Replace the pointer, and refuse unless it reads back as written.
    fn write_pointer(&self, pointer: &ActivationPointer) -> Result<(), ActivationError> {
        let path = self.pointer_path();
        let tmp = self.root.join(ACTIVATION_POINTER_TMP_NAME);
        let bytes = serde_json::to_vec(&pointer.to_file()).map_err(|error| {
            ActivationError::PointerNotDurable {
                path: path.clone(),
                reason: format!("cannot serialize the pointer: {error}"),
            }
        })?;
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|error| ActivationError::Unwritable {
                    path: tmp.clone(),
                    reason: error.to_string(),
                })?;
            file.write_all(&bytes)
                .map_err(|error| ActivationError::Unwritable {
                    path: tmp.clone(),
                    reason: error.to_string(),
                })?;
            // Flush before the rename, for the reason the accepted journal and
            // `store.rs` both give: without it a power cut could make the rename
            // durable while the data was not.
            file.sync_all()
                .map_err(|error| ActivationError::Unwritable {
                    path: tmp.clone(),
                    reason: format!("cannot flush the pointer: {error}"),
                })?;
        }
        // `std::fs::rename` is a replace on both POSIX and Windows. On Windows it
        // is not guaranteed atomic, so what landed is checked rather than assumed.
        if let Err(error) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(ActivationError::Unwritable {
                path,
                reason: format!("cannot replace the activation pointer: {error}"),
            });
        }
        sync_directory(&self.root)?;
        match read_pointer_file(&path) {
            Ok(Some(read_back)) if read_back == *pointer => Ok(()),
            Ok(read_back) => Err(ActivationError::PointerNotDurable {
                path,
                reason: format!("it reads back as {read_back:?}"),
            }),
            Err(error) => Err(ActivationError::PointerNotDurable {
                path,
                reason: error.to_string(),
            }),
        }
    }

    fn open_journal(&self, context: JournalContext) -> Result<LearningJournal, ActivationError> {
        LearningJournal::open(&self.journal_dir(), JournalMode::Append)
            .map_err(|cause| journal_error(context, cause, PointerState::Unchanged))
    }

    // -----------------------------------------------------------------------
    // Audit
    // -----------------------------------------------------------------------

    /// Reconcile the pointer and the journal, explicitly and without repair.
    ///
    /// This is what turns "a crash happened somewhere in an activation" into a
    /// reportable state: a pending intent, a disagreement, or neither. It reads
    /// the journal through the accepted reader, which re-verifies every commit a
    /// record names, and it returns records — never a model.
    pub fn audit(&self, store: &ModelStore) -> Result<ActivationAudit, ActivationError> {
        let pointer = self.read_pointer()?;
        let journal =
            LearningJournal::open(&self.journal_dir(), JournalMode::Read).map_err(|cause| {
                journal_error(JournalContext::Audit, cause, PointerState::Unchanged)
            })?;
        let records = journal.read_records(store).map_err(|cause| {
            journal_error(JournalContext::Audit, cause, PointerState::Unchanged)
        })?;
        let journal_records = records.len() as u64;
        let mut traces = Vec::new();
        for record in &records {
            if let Some(trace) = parse_activation_record(record)? {
                traces.push(trace);
            }
        }
        let pending = pending_intent(&traces);
        let last_completed = traces
            .iter()
            .rev()
            .find(|trace| trace.stage == ActivationStage::Applied)
            .cloned();
        let disagreement = match (pointer.as_ref(), last_completed) {
            (Some(pointer), Some(last)) => {
                if pointer.active.snapshot != last.snapshot {
                    Some(PointerDisagreement::PointerNamesOther {
                        pointer: pointer.active.snapshot.clone(),
                        last_completed: last.snapshot,
                    })
                } else if pointer.active.commit != last.commit {
                    Some(PointerDisagreement::CommitDiffers {
                        snapshot: last.snapshot,
                        pointer_commit: pointer.active.commit.clone(),
                        recorded_commit: last.commit,
                    })
                } else {
                    None
                }
            }
            (None, Some(last)) => Some(PointerDisagreement::PointerAbsent {
                last_completed: last.snapshot,
                commit: last.commit,
            }),
            (_, None) => None,
        };
        Ok(ActivationAudit {
            pointer,
            traces,
            pending,
            disagreement,
            journal_records,
        })
    }

    /// Append the completion record for a pending intent.
    ///
    /// This is the explicit, operator-level way to close the two-phase record of
    /// an activation whose flip landed but whose completion did not. It appends a
    /// **new** record and never rewrites one, it refuses unless the pointer
    /// already names the pending target (so it cannot be used to activate
    /// anything), and it refuses when there is nothing pending rather than
    /// inventing a no-op.
    pub fn complete_pending_activation(
        &self,
        store: &ModelStore,
    ) -> Result<RecordOutcome, ActivationError> {
        let audit = self.audit(store)?;
        let pending =
            audit
                .pending
                .clone()
                .ok_or_else(|| ActivationError::NoPendingActivation {
                    path: self.pointer_path(),
                })?;
        let pointer = audit
            .pointer
            .clone()
            .ok_or_else(|| ActivationError::PointerDisagrees {
                pointer: "<no pointer>".to_string(),
                pending: pending.snapshot.as_str().to_string(),
            })?;
        if pointer.active.snapshot != pending.snapshot || pointer.active.commit != pending.commit {
            return Err(ActivationError::PointerDisagrees {
                pointer: pointer.active.snapshot.as_str().to_string(),
                pending: pending.snapshot.as_str().to_string(),
            });
        }
        // The target must still verify: a completion is a claim about a real
        // snapshot, not about a name.
        let snapshot = self.read_snapshot(&pending.snapshot)?;
        verify_commit_in_store(snapshot.commit(), store)?;
        let mut journal = self.open_journal(JournalContext::Completion)?;
        let completion_id = activation_applied_event_id(&pending.event_id);
        record_activation(
            &mut journal,
            store,
            JournalContext::Completion,
            PointerState::Flipped,
            &completion_id,
            ActivationStage::Applied,
            &pointer.active,
        )
    }
}

// ---------------------------------------------------------------------------
// Loading the pointer, on disk and off it
// ---------------------------------------------------------------------------

/// Read and verify the pointer file. `None` means it does not exist, which is
/// the distinct "nothing was ever activated" state.
fn read_pointer_file(path: &Path) -> Result<Option<ActivationPointer>, ActivationError> {
    if path.is_dir() {
        return Err(ActivationError::PointerCorrupt {
            path: path.to_path_buf(),
            reason: "there is a directory where the pointer file belongs".to_string(),
        });
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ActivationError::PointerCorrupt {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })
        }
    };
    let file: PointerFile =
        serde_json::from_slice(&bytes).map_err(|error| ActivationError::PointerCorrupt {
            path: path.to_path_buf(),
            reason: format!("it is not a readable activation pointer: {error}"),
        })?;
    ActivationPointer::try_from_file(file).map(Some)
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
fn record_activation(
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
fn parse_activation_record(
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
fn pending_intent(traces: &[ActivationTrace]) -> Option<PendingActivation> {
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
