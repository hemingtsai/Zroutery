//! Durable, ordered, idempotent learning-event journal.
//!
//! `LearningEvent` is a type. This module is the disk.
//!
//! # What this is for
//!
//! A [`LearningJournal`] is an append-only, ordered, idempotent record of
//! learning events for **one model lineage**, kept in a directory the caller
//! names. It exists because every accepted ML node so far is pure: an event
//! survives in a `Vec` until the process ends, and nothing about it is ordered
//! across a restart, deduplicated on retry, or verified against the commit it
//! claims. [`LearningEvent::new`] even mints its `event_id` from a process-local
//! counter, so two processes both start at `evt-1`.
//!
//! # The trap this module refuses
//!
//! A durable journal that is automatically replayed into training when it is
//! opened is an **online learning loop**, and that is forbidden. Reading this
//! journal is an explicit call — [`LearningJournal::read_records`] — that
//! returns an explicit result. Nothing here runs on a timer, at startup, on a
//! drop, or on a background thread; there is no `Drop` behaviour beyond releasing
//! the lock file. **The records are inert.** No method in this module trains a
//! model, puts one into service, or consumes an event, and this module never
//! calls the accepted replay entry points or the accepted ensemble updater.
//! Verification is not consumption: [`LearningJournal::read_records`] re-verifies
//! the commits a record names, and returns records — never a model, never a
//! checkpoint, never an update.
//!
//! # On-disk format
//!
//! Three files, all inside the caller-supplied directory, and nothing else:
//!
//! * `journal.log` — the append-only record frames.
//! * `journal.anchor` — a completion anchor (see below).
//! * `journal.lock` — the single-writer lock, present only while a handle is.
//!
//! One frame is one line:
//!
//! ```text
//! <sequence>\t<frame checksum>\t<body JSON>\n
//! ```
//!
//! The body is the compact serialization of a [`JournalRecordBody`]. A literal
//! tab byte cannot occur inside it: `serde_json` escapes control characters
//! inside strings, and compact JSON has no other whitespace. The trailing
//! newline is the only terminator, which is what makes a half-written tail
//! *detectable* rather than merely improbable.
//!
//! The frame checksum is FNV-1a-64 over a domain tag, the **previous frame's**
//! checksum, the sequence, and the body bytes. Chaining the predecessor is what
//! makes a removed or reordered *interior* frame detectable independently of the
//! sequence check. The checksum catches accidental corruption only — it is not a
//! MAC and not an authenticity claim. The cryptographic witness of a commit is
//! its content-addressed `CommitId`, re-verified on read.
//!
//! # Why an anchor and not a rewritten snapshot
//!
//! A rewritten snapshot is wrong here for two reasons. It makes every append an
//! O(n) rewrite of the whole history, and it still cannot tell "the journal
//! ends here" from "the tail was lost". Neither can a bare log: **a prefix of a
//! valid log is itself a valid log**, so no per-record checksum can detect a
//! suffix removed at a frame boundary. Detecting that requires a redundant
//! witness outside the log, which is exactly what `journal.anchor` is. After each
//! frame is `sync_all`ed, the anchor is rewritten through a temporary file, a
//! flush, and a rename, and it records the record count, the next sequence, the
//! log length, a whole-log checksum, and the tail frame checksum.
//!
//! # The states a reader can be in, and what each one means
//!
//! | State | Refusal or report |
//! |---|---|
//! | Final frame has no trailing newline | `TornTail` — a half-written frame |
//! | A complete frame fails its checksum | `ChecksumMismatch` |
//! | A complete frame is unparsable or internally inconsistent | `CorruptRecord` |
//! | Sequence is not exactly previous + 1 | `SequenceConflict { Missing / Duplicate / Rewind }` |
//! | Log is shorter than the anchor, at a frame boundary | `RecordsLost` — deliberate truncation |
//! | Log is longer than the anchor, chain intact | reported as `records_beyond_anchor` |
//! | Same length and count, different checksum | `AnchorDisagrees` |
//! | A named commit no longer verifies | `CommitVerificationFailed` |
//!
//! **None of these is repaired automatically.** A torn tail is not silently
//! dropped, a corrupt interior record refuses the whole read instead of skipping
//! ahead, and a lost suffix is not mistaken for a short journal. Recovery is an
//! operator decision, which is the entire point of the gate.
//!
//! # Ordering
//!
//! The first frame is sequence 1 and every later frame is exactly one more than
//! its predecessor. [`LearningJournal::open`] is the only constructor and it
//! derives the ordering state from the file, not from memory; every append
//! re-scans the file before it writes. When memory and the file disagree, the
//! file is authoritative and the disagreement is *reported* — see
//! [`LearningJournal::expect_next_sequence`], which refuses a caller that still
//! believes the journal is at zero.
//!
//! # Two processes
//!
//! `open` refuses a second handle in either mode while another is live, with
//! `JournalLocked`. Merging two writers into one sequence is deliberately not
//! implemented: deciding which of two records claiming the same sequence came
//! first is last-write-wins, and last-write-wins is exactly what idempotency
//! must not be. The lock can be left stale by a process that dies without
//! unwinding, and that residual is real, so the *format* carries the safety
//! net: a per-frame checksum, a chained predecessor, and a monotonic sequence
//! turn a bypassed lock into a loud typed refusal rather than silent loss. A
//! stale lock is removed by hand, and because every open re-verifies every frame
//! against its bytes, removing it cannot cause data loss.
//!
//! # Cross-platform durability
//!
//! The log frame is `sync_all`ed before the anchor is written, and the anchor is
//! replaced by a rename that `std::fs::rename` performs as a replace on both
//! POSIX and Windows. The *directory* flush that POSIX requires after a rename is
//! `#[cfg(unix)]`-only, because a Windows directory handle needs
//! `FILE_FLAG_BACKUP_SEMANTICS` and this crate cannot reach it without a new
//! dependency. The consequence is bounded and stated rather than hidden: the log
//! is already durable at that point, so a lost anchor can only cause a
//! `NotAJournal` refusal — a fail-closed direction — and never a silently
//! accepted truncated log.
//!
//! # The legacy sample shape
//!
//! [`LearningEvent`] carries `Vec<DatasetTrainingSample>`, the legacy shape,
//! while production collects the canonical [`OutcomeTrainingSample`]. Storing
//! only the legacy event would make this journal a place where the canonical
//! evidence silently degrades: `into_legacy` discards the attempt evidence,
//! usage and cost facts, terminal error facts, identity roles, scope, the
//! rectified flag, the dialect, and the correlation ids.
//!
//! So the record stores the canonical samples *and* the accepted event, and the
//! write path proves the relationship instead of assuming it:
//! [`LearningJournal::record_canonical`] validates every canonical sample with
//! the accepted [`validate_outcome_sample`], projects them through
//! `into_legacy`, and refuses unless that projection is exactly the event's
//! samples. The read path re-runs the same check, so a stored record whose
//! legacy samples were altered is [`JournalError::CorruptRecord`] — the
//! invariant is enforced on the way in *and* on the way out.
//!
//! The lossy path still exists, because the accepted event type is this node's
//! subject, but it is named and labelled: [`LearningJournal::record_legacy`]
//! stores [`JournalEvidence::LegacyProjection`] and
//! [`JournalRecord::is_degraded`] is on the record. A consumer cannot mistake a
//! degraded record for complete evidence, because the degradation is stated
//! rather than inferred.
//!
//! [`LearningEvent::new`]: super::model_identity::LearningEvent::new
//! [`LearningEvent`]: super::model_identity::LearningEvent
//! [`validate_outcome_sample`]: super::dataset::validate_outcome_sample
//! [`OutcomeTrainingSample`]: super::dataset::OutcomeTrainingSample
//! [`DatasetTrainingSample`]: super::dataset::TrainingSample

use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::dataset::{
    validate_outcome_sample, OutcomeTrainingSample, TrainingSample as DatasetTrainingSample,
};
use super::model_identity::{
    CommitId, LearningEvent, ModelId, ModelStore, ReplayEngine, LEARNING_EVENT_SCHEMA_VERSION,
};

/// Schema version of the durable record envelope written by this module.
pub const JOURNAL_SCHEMA_VERSION: u32 = 1;
/// Schema version of the completion anchor.
pub const ANCHOR_SCHEMA_VERSION: u32 = 1;
/// The append-only record log, inside the caller-supplied directory.
pub const JOURNAL_LOG_NAME: &str = "journal.log";
/// The completion anchor, inside the caller-supplied directory.
pub const JOURNAL_ANCHOR_NAME: &str = "journal.anchor";
/// The temporary anchor target, inside the same directory as the anchor.
pub const JOURNAL_ANCHOR_TMP_NAME: &str = "journal.anchor.tmp";
/// The single-writer lock, inside the caller-supplied directory.
pub const JOURNAL_LOCK_NAME: &str = "journal.lock";
/// The sequence of the first frame. There is no sequence zero.
pub const FIRST_SEQUENCE: u64 = 1;
/// Upper bound on one materialized frame, so a malformed file cannot exhaust
/// memory before it is refused.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// What a record in this journal is, stated for a reader and for a reviewer.
pub const JOURNAL_ROLE: &str = "inert durable record of learning events: reading it is an \
explicit call that returns an explicit result; a record is never trained, never \
put into service, and never scheduled by this module";

const FRAME_DOMAIN: &[u8] = b"zroutery-learning-journal-frame-v1\0";
const ANCHOR_DOMAIN: &[u8] = b"zroutery-learning-journal-anchor-v1\0";
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

// ---------------------------------------------------------------------------
// Checksum
// ---------------------------------------------------------------------------

fn hash_extend(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) -> u64 {
    hash_extend(hash, bytes);
    *hash
}

/// The checksum of one journal frame.
///
/// It covers the previous frame's checksum, which is what makes a removed or
/// reordered interior frame detectable on its own, and it is public because a
/// durable format that only its own writer can verify is not a durable format:
/// another implementation must be able to re-derive and check it.
///
/// This is an accidental-corruption checksum, not a MAC.
pub fn frame_checksum(previous_frame_checksum: u64, sequence: u64, body: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, FRAME_DOMAIN);
    hash_extend(&mut hash, &previous_frame_checksum.to_le_bytes());
    hash_extend(&mut hash, &sequence.to_le_bytes());
    hash_bytes(&mut hash, body)
}

fn format_checksum(checksum: u64) -> String {
    let mut text = String::with_capacity(16);
    for index in (0..16).rev() {
        let nibble = ((checksum >> (index * 4)) & 0xf) as usize;
        text.push(char::from(HEX_DIGITS[nibble]));
    }
    text
}

fn parse_checksum(text: &str) -> Option<u64> {
    if text.len() != 16 {
        return None;
    }
    let mut value: u64 = 0;
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

/// A canonical decimal sequence field: digits only, and no leading zero.
fn parse_sequence(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// JournalError
// ---------------------------------------------------------------------------

/// How a sequence disagreed with the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceFault {
    /// The sequence skipped forward: a record is missing.
    Missing,
    /// The sequence repeated: a record was written twice.
    Duplicate,
    /// The sequence went backwards.
    Rewind,
}

impl SequenceFault {
    fn label(self) -> &'static str {
        match self {
            SequenceFault::Missing => "gap",
            SequenceFault::Duplicate => "duplicate",
            SequenceFault::Rewind => "rewind",
        }
    }
}

impl fmt::Display for SequenceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Every way this journal refuses. Each carries a reason; none is a panic and
/// none is a silent success.
#[derive(Debug, Clone)]
pub enum JournalError {
    /// The path could not be created, written, or flushed.
    UnwritablePath { path: PathBuf, reason: String },
    /// The path exists and is not a directory.
    NotADirectory { path: PathBuf },
    /// The directory is not a journal this implementation can trust.
    NotAJournal { path: PathBuf, reason: String },
    /// Another handle already holds this journal.
    JournalLocked { path: PathBuf, holder: String },
    /// A write was attempted through a handle opened for reading.
    ReadOnlyJournal { path: PathBuf },
    /// The ordering state disagreed with the file, or the file disagreed with
    /// itself.
    SequenceConflict {
        expected: u64,
        actual: u64,
        fault: SequenceFault,
    },
    /// A frame's recorded checksum does not match its bytes.
    ChecksumMismatch {
        offset: u64,
        sequence: u64,
        recorded: u64,
        computed: u64,
    },
    /// The final frame is not newline-terminated: a half-written tail.
    TornTail { offset: u64, bytes_present: u64 },
    /// A complete frame is unparsable or internally inconsistent.
    CorruptRecord { offset: u64, reason: String },
    /// One frame exceeded [`MAX_FRAME_BYTES`].
    FrameTooLarge { offset: u64, bytes: usize },
    /// A commit reachable from a record no longer verifies.
    CommitVerificationFailed {
        sequence: u64,
        commit_id: CommitId,
        reason: String,
    },
    /// The log is shorter than its anchor: records were removed.
    RecordsLost { expected: u64, found: u64 },
    /// The anchor does not describe this log, and the log is not a longer
    /// continuation of it either.
    AnchorDisagrees { path: PathBuf, reason: String },
    /// The event or its samples were rejected by the accepted validator.
    EventRejected { event_id: String, reason: String },
    /// The event id is the process-local form `LearningEvent::new` mints.
    VolatileEventId { event_id: String },
    /// The same event id was recorded with different content.
    IdempotencyConflict { event_id: String, reason: String },
    /// A stored envelope is not a version this implementation reads.
    UnsupportedSchema {
        component: String,
        version: u32,
        supported: u32,
    },
    /// A filesystem operation failed.
    Io { path: PathBuf, reason: String },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::UnwritablePath { path, reason } => {
                write!(f, "cannot use {}: {reason}", path.display())
            }
            JournalError::NotADirectory { path } => {
                write!(f, "{} is not a directory", path.display())
            }
            JournalError::NotAJournal { path, reason } => {
                write!(f, "{} is not a usable journal: {reason}", path.display())
            }
            JournalError::JournalLocked { path, holder } => {
                let detail = if holder.is_empty() {
                    "held by another handle".to_string()
                } else {
                    format!("held by {holder}")
                };
                write!(f, "journal lock {} is {detail}", path.display())
            }
            JournalError::ReadOnlyJournal { path } => {
                write!(
                    f,
                    "{} is open for reading and cannot be written",
                    path.display()
                )
            }
            JournalError::SequenceConflict {
                expected,
                actual,
                fault,
            } => write!(f, "sequence {fault}: expected {expected}, found {actual}"),
            JournalError::ChecksumMismatch {
                offset,
                sequence,
                recorded,
                computed,
            } => write!(
                f,
                "frame at offset {offset} (sequence {sequence}) records checksum {recorded:016x} \
                 but its bytes hash to {computed:016x}"
            ),
            JournalError::TornTail {
                offset,
                bytes_present,
            } => write!(
                f,
                "the final frame at offset {offset} is not newline-terminated: \
                 a torn tail of {bytes_present} byte(s)"
            ),
            JournalError::CorruptRecord { offset, reason } => {
                write!(f, "frame at offset {offset} is corrupt: {reason}")
            }
            JournalError::FrameTooLarge { offset, bytes } => {
                write!(
                    f,
                    "frame at offset {offset} is {bytes} bytes, over the {MAX_FRAME_BYTES} \
                     byte limit"
                )
            }
            JournalError::CommitVerificationFailed {
                sequence,
                commit_id,
                reason,
            } => write!(
                f,
                "record at sequence {sequence} names commit '{commit_id}' which does not \
                 verify: {reason}"
            ),
            JournalError::RecordsLost { expected, found } => write!(
                f,
                "the anchor accounts for {expected} record(s) but the log holds {found}: \
                 records were removed"
            ),
            JournalError::AnchorDisagrees { path, reason } => {
                write!(f, "the anchor in {} disagrees: {reason}", path.display())
            }
            JournalError::EventRejected { event_id, reason } => {
                write!(f, "event '{event_id}' was rejected: {reason}")
            }
            JournalError::VolatileEventId { event_id } => write!(
                f,
                "event id '{event_id}' is the process-local 'evt-<sequence>' form, which \
                 restarts at one in every process and cannot be a durable identity"
            ),
            JournalError::IdempotencyConflict { event_id, reason } => {
                write!(
                    f,
                    "event '{event_id}' conflicts with a stored record: {reason}"
                )
            }
            JournalError::UnsupportedSchema {
                component,
                version,
                supported,
            } => write!(
                f,
                "unsupported {component} version {version} (supported version {supported})"
            ),
            JournalError::Io { path, reason } => {
                write!(f, "i/o error on {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for JournalError {}

// ---------------------------------------------------------------------------
// JournalEvidence — the anti-degradation claim, on the record
// ---------------------------------------------------------------------------

/// The evidence a record retains.
///
/// This is the answer to the legacy/canonical split, and it is deliberately not
/// hidden in a field: a record states which evidence it carries, so a consumer
/// cannot infer completeness from a well-formed sample list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEvidence {
    /// The lossless canonical samples, kept beside the projected legacy event.
    /// The write path proved that projecting these reproduces the event's
    /// samples exactly, and the read path re-proves it.
    Canonical {
        /// The canonical samples, as collected from the Outcome.
        samples: Vec<OutcomeTrainingSample>,
    },
    /// The caller supplied the legacy `LearningEvent` sample shape, so the
    /// attempt evidence, usage and cost facts, terminal error facts, identity
    /// roles, scope, dialect, and correlation ids were **never retained**.
    /// `degradation` states that on the record itself.
    LegacyProjection {
        /// Why this record is degraded, in plain words.
        degradation: String,
    },
}

/// The degradation a legacy record carries. It is written to disk, so it is
/// part of the durable evidence rather than a comment.
pub const LEGACY_DEGRADATION: &str = "the caller supplied the legacy LearningEvent sample shape: \
the attempt evidence, usage and cost facts, terminal error facts, identity roles, \
scope, the rectified flag, the dialect, and the correlation ids were never retained \
and cannot be reconstructed from this record";

impl JournalEvidence {
    /// Whether this evidence is a lossy projection rather than the canonical
    /// samples.
    pub fn is_degraded(&self) -> bool {
        matches!(self, JournalEvidence::LegacyProjection { .. })
    }

    /// The retained canonical samples, if this record has them.
    pub fn canonical_samples(&self) -> Option<&[OutcomeTrainingSample]> {
        match self {
            JournalEvidence::Canonical { samples } => Some(samples.as_slice()),
            JournalEvidence::LegacyProjection { .. } => None,
        }
    }

    /// The evidence as this module stores it.
    ///
    /// A legacy projection always carries the canonical degradation text, so a
    /// caller that hand-builds a [`JournalRecordBody`] cannot forge the label.
    /// The claim "this record is degraded" is derived here rather than supplied,
    /// which is the whole point of putting it on the record.
    fn normalized(&self) -> JournalEvidence {
        match self {
            JournalEvidence::LegacyProjection { .. } => JournalEvidence::LegacyProjection {
                degradation: LEGACY_DEGRADATION.to_string(),
            },
            canonical => canonical.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// JournalRecordBody — the validating wire form
// ---------------------------------------------------------------------------

/// The serializable form of one record.
///
/// `Deserialize` is a derive because the file is untrusted input and the format
/// has to be readable. It is **not** a way in: [`Self::try_into_record`] re-runs
/// every check the write path runs, re-validating through the accepted
/// `LearningEvent::validate` and `validate_outcome_sample`, and re-proving the
/// canonical-to-legacy projection. A stored body therefore cannot re-enter this
/// module having skipped a check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalRecordBody {
    /// The record envelope version.
    pub schema_version: u32,
    /// The accepted learning-event envelope version this record was written
    /// against. Carried so that a journal written by a binary with a different
    /// event envelope is refused rather than reinterpreted.
    pub event_schema_version: u32,
    /// The accepted learning event, exactly as recorded.
    pub event: LearningEvent,
    /// The evidence retained beside the event.
    pub evidence: JournalEvidence,
    /// Wall-clock time the record was written. Informational; deliberately not
    /// part of a record's content identity.
    pub recorded_at: i64,
}

impl JournalRecordBody {
    /// Rebuild a record, re-running every validation the write path runs.
    ///
    /// This is the only way to obtain a [`JournalRecord`] other than through
    /// [`LearningJournal`], and it is validating on purpose.
    pub fn try_into_record(&self, sequence: u64) -> Result<JournalRecord, JournalError> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema {
                component: "journal record envelope".to_string(),
                version: self.schema_version,
                supported: JOURNAL_SCHEMA_VERSION,
            });
        }
        if self.event_schema_version != LEARNING_EVENT_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema {
                component: format!(
                    "learning event recorded at sequence {sequence} in the journal envelope"
                ),
                version: self.event_schema_version,
                supported: LEARNING_EVENT_SCHEMA_VERSION,
            });
        }
        if self.event.model_id.as_str().is_empty() {
            return Err(JournalError::EventRejected {
                event_id: self.event.event_id.clone(),
                reason: "model id must not be empty".to_string(),
            });
        }
        // Re-validate through the accepted type. The journal is not an
        // alternative validator; it is the accepted one, run again.
        self.event
            .validate()
            .map_err(|error| JournalError::EventRejected {
                event_id: self.event.event_id.clone(),
                reason: error.to_string(),
            })?;
        self.evidence.validate_projection(&self.event)?;
        Ok(JournalRecord {
            sequence,
            event: self.event.clone(),
            evidence: self.evidence.normalized(),
            recorded_at: self.recorded_at,
            frame_checksum: 0,
        })
    }
}

impl JournalEvidence {
    /// Re-prove that the evidence and the event agree.
    ///
    /// For canonical evidence this is the anti-degradation invariant: the
    /// event's samples must be *exactly* the canonical projection. If they are
    /// not, the record is internally inconsistent — someone edited the legacy
    /// samples, or a writer bypassed the proof — and it is refused rather than
    /// replayed.
    fn validate_projection(&self, event: &LearningEvent) -> Result<(), JournalError> {
        let JournalEvidence::Canonical { samples } = self else {
            return Ok(());
        };
        for (index, sample) in samples.iter().enumerate() {
            validate_outcome_sample(sample).map_err(|reason| JournalError::EventRejected {
                event_id: event.event_id.clone(),
                reason: format!("canonical sample {index}: {reason}"),
            })?;
        }
        let projected: Vec<DatasetTrainingSample> = samples
            .iter()
            .cloned()
            .map(OutcomeTrainingSample::into_legacy)
            .collect();
        if projected == event.samples {
            return Ok(());
        }
        Err(JournalError::EventRejected {
            event_id: event.event_id.clone(),
            reason: format!(
                "the stored legacy samples are not the projection of the {} retained canonical \
                 sample(s): the record's evidence and its event disagree",
                samples.len()
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// JournalRecord
// ---------------------------------------------------------------------------

/// One durably recorded learning event.
#[derive(Debug, Clone)]
pub struct JournalRecord {
    /// The journal-assigned sequence. Monotonic across restarts, starting at 1.
    pub sequence: u64,
    /// The accepted learning event. Inert: nothing here applies it.
    pub event: LearningEvent,
    /// The retained evidence, and whether it is degraded.
    pub evidence: JournalEvidence,
    /// Wall-clock time the record was written.
    pub recorded_at: i64,
    frame_checksum: u64,
}

impl JournalRecord {
    /// Whether this record's evidence is a lossy projection.
    ///
    /// A consumer that requires canonical evidence must check this, and must
    /// refuse the record when it is true. The journal does not decide that for
    /// the consumer; it states it.
    pub fn is_degraded(&self) -> bool {
        self.evidence.is_degraded()
    }

    /// The retained canonical samples, if this record has them.
    pub fn canonical_samples(&self) -> Option<&[OutcomeTrainingSample]> {
        self.evidence.canonical_samples()
    }

    /// The frame checksum, which chains to the next frame. Useful for audit and
    /// for a second implementation re-deriving the log.
    pub fn frame_checksum(&self) -> u64 {
        self.frame_checksum
    }

    /// Whether this record's event names a result commit.
    pub fn is_applied(&self) -> bool {
        self.event.is_applied()
    }
}

// ---------------------------------------------------------------------------
// Mode, outcome, report
// ---------------------------------------------------------------------------

/// What a handle is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    /// Verify and read. Never writes, and never repairs.
    Read,
    /// Verify, read, and append.
    Append,
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// A new frame was written, flushed, and anchored.
    Appended {
        /// The sequence the journal assigned.
        sequence: u64,
        /// The bytes appended to the log.
        bytes: u64,
    },
    /// The same event id with the same content was already recorded. The
    /// journal reported the no-op and wrote nothing.
    Duplicate {
        /// The sequence of the record that was already there.
        sequence: u64,
    },
}

/// The ordering state [`LearningJournal::open`] derived **from the file**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalReport {
    /// Whole frames in the log.
    pub records: u64,
    /// The sequence the next append will use.
    pub next_sequence: u64,
    /// Length of the log in bytes.
    pub log_bytes: u64,
    /// Records the log holds that the anchor never confirmed.
    ///
    /// A non-zero value is the documented crash window between the log flush
    /// and the anchor rename. The chained checksums prove the extra frames are
    /// an intact continuation, so this is reported rather than refused; the next
    /// append rewrites the anchor and closes the window.
    pub records_beyond_anchor: u64,
}

// ---------------------------------------------------------------------------
// CanonicalEvent — the lossless write request
// ---------------------------------------------------------------------------

/// A write request that carries the canonical samples.
///
/// `created_at` is the caller's, not the journal's, so a record is reproducible
/// by the caller that made it.
#[derive(Debug, Clone)]
pub struct CanonicalEvent {
    /// A stable, caller-supplied identity. The process-local `evt-<n>` form is
    /// refused: a deduplication key that restarts in every process is not a
    /// durable identity.
    pub event_id: String,
    /// The model lineage this event belongs to.
    pub model_id: ModelId,
    /// The canonical samples, exactly as collected.
    pub samples: Vec<OutcomeTrainingSample>,
    /// The commit this event was planned against.
    pub parent_commit: Option<CommitId>,
    /// The commit this event produced, if it has been applied.
    pub result_commit: Option<CommitId>,
    /// Wall-clock creation time, supplied by the caller.
    pub created_at: i64,
    /// Where the event came from.
    pub source: Option<String>,
}

impl CanonicalEvent {
    /// The common case: an event with samples and nothing else claimed.
    pub fn new(
        event_id: impl Into<String>,
        model_id: ModelId,
        samples: Vec<OutcomeTrainingSample>,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            model_id,
            samples,
            parent_commit: None,
            result_commit: None,
            created_at: now_seconds(),
            source: None,
        }
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// The completion anchor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Anchor {
    schema_version: u32,
    records: u64,
    next_sequence: u64,
    log_bytes: u64,
    log_checksum: u64,
    tail_frame_checksum: u64,
    anchor_checksum: u64,
}

impl Anchor {
    fn fresh() -> Self {
        Self {
            schema_version: ANCHOR_SCHEMA_VERSION,
            records: 0,
            next_sequence: FIRST_SEQUENCE,
            log_bytes: 0,
            log_checksum: FNV_OFFSET_BASIS,
            tail_frame_checksum: 0,
            anchor_checksum: 0,
        }
    }

    fn from_scan(scan: &LogScan) -> Self {
        Self {
            schema_version: ANCHOR_SCHEMA_VERSION,
            records: scan.records,
            next_sequence: scan.next_sequence(),
            log_bytes: scan.log_bytes,
            log_checksum: scan.log_checksum,
            tail_frame_checksum: scan.tail_frame_checksum,
            anchor_checksum: 0,
        }
    }

    fn content_checksum(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        hash_extend(&mut hash, ANCHOR_DOMAIN);
        hash_extend(&mut hash, &self.schema_version.to_le_bytes());
        hash_extend(&mut hash, &self.records.to_le_bytes());
        hash_extend(&mut hash, &self.next_sequence.to_le_bytes());
        hash_extend(&mut hash, &self.log_bytes.to_le_bytes());
        hash_extend(&mut hash, &self.log_checksum.to_le_bytes());
        hash_extend(&mut hash, &self.tail_frame_checksum.to_le_bytes());
        hash
    }

    fn seal(mut self) -> Self {
        self.anchor_checksum = self.content_checksum();
        self
    }
}

// ---------------------------------------------------------------------------
// Log scan
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FrameEntry {
    sequence: u64,
    body: JournalRecordBody,
    /// The body exactly as it appears on disk.
    ///
    /// The duplicate check compares these bytes against a re-serialization of
    /// the incoming record, never a parsed value against an in-memory one.
    ///
    /// This was NECESSARY when it was written: `serde_json` was used here
    /// WITHOUT its `float_roundtrip` feature, so its float parsing was not
    /// correctly rounded and a value could come back one ULP different from the
    /// bytes that produced it, which made a legitimate byte-identical retry
    /// report as a conflict. That is E-089, and it was reproduced.
    ///
    /// It is now redundancy. The workspace enables `float_roundtrip`, so the
    /// transport is exact and this comparison no longer has a known case it
    /// rescues. It is kept deliberately: idempotency should not depend on the
    /// fidelity of a serializer's float parsing, and this way it does not.
    body_bytes: Vec<u8>,
    frame_checksum: u64,
}

#[derive(Debug, Default)]
struct LogScan {
    records: u64,
    log_bytes: u64,
    log_checksum: u64,
    tail_frame_checksum: u64,
    frames: Vec<FrameEntry>,
}

impl LogScan {
    fn next_sequence(&self) -> u64 {
        match self.records {
            0 => FIRST_SEQUENCE,
            count => count + 1,
        }
    }
}

/// A reader that yields newline-terminated frames, holding whatever it
/// over-read in an explicit carry buffer.
///
/// This deliberately does not lean on a buffered reader's internal rules. A
/// read whose buffer is at least the internal capacity bypasses that buffer
/// entirely, so the bytes after the first newline in one chunk would be dropped
/// on the floor — and this reader is deciding where a record *ends*. The carry
/// is explicit, so an over-read byte is never lost and never silently truncated.
struct FrameReader<R: Read> {
    inner: R,
    carry: Vec<u8>,
    eof: bool,
}

impl<R: Read> FrameReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            carry: Vec::new(),
            eof: false,
        }
    }

    /// The next frame without its terminator, `None` at a clean end of file,
    /// and a typed refusal otherwise.
    ///
    /// `offset` is the frame's start offset in the log, so every refusal names
    /// where in the file the problem is.
    fn next_frame(&mut self, offset: u64) -> Result<Option<Vec<u8>>, JournalError> {
        let mut frame = std::mem::take(&mut self.carry);
        let mut scratch = [0u8; 8192];
        loop {
            if let Some(position) = frame.iter().position(|byte| *byte == b'\n') {
                self.carry = frame.split_off(position + 1);
                frame.truncate(position);
                return Ok(Some(frame));
            }
            if frame.len() > MAX_FRAME_BYTES {
                return Err(JournalError::FrameTooLarge {
                    offset,
                    bytes: frame.len(),
                });
            }
            if self.eof {
                return if frame.is_empty() {
                    Ok(None)
                } else {
                    Err(JournalError::TornTail {
                        offset,
                        bytes_present: frame.len() as u64,
                    })
                };
            }
            let read = self
                .inner
                .read(&mut scratch)
                .map_err(|error| JournalError::Io {
                    path: PathBuf::from("<log frame>"),
                    reason: error.to_string(),
                })?;
            if read == 0 {
                self.eof = true;
                continue;
            }
            frame.extend_from_slice(&scratch[..read]);
        }
    }
}

/// The three tab-separated fields of one frame: sequence, checksum, body.
type FrameFields<'a> = (&'a [u8], &'a [u8], &'a [u8]);

/// Split one frame into its three fields. Exactly two tabs, and a non-empty
/// body: a literal tab cannot occur inside compact JSON.
fn split_frame(offset: u64, frame: &[u8]) -> Result<FrameFields<'_>, JournalError> {
    let corrupt = |reason: &str| JournalError::CorruptRecord {
        offset,
        reason: reason.to_string(),
    };
    let Some(first) = frame.iter().position(|byte| *byte == b'\t') else {
        return Err(corrupt("a frame is <sequence> TAB <checksum> TAB <body>"));
    };
    let rest = &frame[first + 1..];
    let Some(second) = rest.iter().position(|byte| *byte == b'\t') else {
        return Err(corrupt("a frame is <sequence> TAB <checksum> TAB <body>"));
    };
    let (checksum, body) = rest.split_at(second);
    if body.is_empty() {
        return Err(corrupt("a frame body must not be empty"));
    }
    Ok((&frame[..first], checksum, &body[1..]))
}

fn frame_body_error(offset: u64, sequence: u64, error: serde_json::Error) -> JournalError {
    JournalError::CorruptRecord {
        offset,
        reason: format!("frame at sequence {sequence} does not deserialize: {error}"),
    }
}

/// Verify the whole log. Every refusal here stops the scan: this is the
/// fail-closed read that a corrupt *interior* record must not be able to skip
/// past.
fn scan_log(path: &Path) -> Result<LogScan, JournalError> {
    let file = std::fs::File::open(path).map_err(|error| JournalError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let log_bytes = file
        .metadata()
        .map_err(|error| JournalError::Io {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?
        .len();
    let mut reader = FrameReader::new(file);
    let mut scan = LogScan {
        log_bytes,
        log_checksum: FNV_OFFSET_BASIS,
        ..LogScan::default()
    };
    let mut offset = 0u64;
    while let Some(frame) = reader.next_frame(offset)? {
        let frame_len = frame.len() as u64 + 1;
        let expected_sequence = scan.next_sequence();
        let (sequence_text, checksum_text, body) = split_frame(offset, &frame)?;

        let sequence_text =
            std::str::from_utf8(sequence_text).map_err(|_| JournalError::CorruptRecord {
                offset,
                reason: "a frame sequence must be ASCII decimal digits".to_string(),
            })?;
        let sequence =
            parse_sequence(sequence_text).ok_or_else(|| JournalError::CorruptRecord {
                offset,
                reason: format!("'{sequence_text}' is not a canonical sequence number"),
            })?;
        if sequence != expected_sequence {
            // A repeat of the sequence just read is a duplicate; anything below
            // the expected value is a rewind; anything above it is a gap. The
            // first frame has no predecessor, so a sequence below one is a
            // rewind rather than a duplicate of nothing.
            let fault = if scan.records > 0 && sequence == expected_sequence - 1 {
                SequenceFault::Duplicate
            } else if sequence < expected_sequence {
                SequenceFault::Rewind
            } else {
                SequenceFault::Missing
            };
            return Err(JournalError::SequenceConflict {
                expected: expected_sequence,
                actual: sequence,
                fault,
            });
        }

        let checksum_text =
            std::str::from_utf8(checksum_text).map_err(|_| JournalError::CorruptRecord {
                offset,
                reason: "a frame checksum must be 16 lowercase hex digits".to_string(),
            })?;
        let recorded =
            parse_checksum(checksum_text).ok_or_else(|| JournalError::CorruptRecord {
                offset,
                reason: format!("'{checksum_text}' is not a 16-digit lowercase checksum"),
            })?;
        let computed = frame_checksum(scan.tail_frame_checksum, sequence, body);
        if recorded != computed {
            return Err(JournalError::ChecksumMismatch {
                offset,
                sequence,
                recorded,
                computed,
            });
        }

        // Captured before the parse shadows the byte slice, because the duplicate
        // check later needs the bytes as they are on disk.
        let body_bytes = body.to_vec();
        let body: JournalRecordBody = serde_json::from_slice(body)
            .map_err(|error| frame_body_error(offset, sequence, error))?;
        if body.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema {
                component: format!("journal record at sequence {sequence}"),
                version: body.schema_version,
                supported: JOURNAL_SCHEMA_VERSION,
            });
        }

        let mut hash = scan.log_checksum;
        hash_extend(&mut hash, &frame);
        hash_extend(&mut hash, b"\n");
        scan.log_checksum = hash;
        scan.tail_frame_checksum = computed;
        scan.records += 1;
        scan.frames.push(FrameEntry {
            sequence,
            body,
            body_bytes,
            frame_checksum: computed,
        });
        offset += frame_len;
    }
    Ok(scan)
}

/// Decide whether the anchor and the log can be reconciled.
///
/// The log's own structure has already been verified by the time this runs, so
/// a disagreement here is about *completeness*, not corruption.
fn reconcile_anchor(anchor: &Anchor, scan: &LogScan, path: &Path) -> Result<u64, JournalError> {
    use std::cmp::Ordering;
    match (
        scan.records.cmp(&anchor.records),
        scan.log_bytes.cmp(&anchor.log_bytes),
    ) {
        (Ordering::Equal, Ordering::Equal) => {
            if scan.log_checksum != anchor.log_checksum
                || scan.tail_frame_checksum != anchor.tail_frame_checksum
            {
                return Err(JournalError::AnchorDisagrees {
                    path: path.to_path_buf(),
                    reason: "the same number of records and bytes hash differently".to_string(),
                });
            }
            Ok(0)
        }
        (Ordering::Less, _) | (_, Ordering::Less) => Err(JournalError::RecordsLost {
            expected: anchor.records,
            found: scan.records,
        }),
        (Ordering::Greater, _) | (_, Ordering::Greater) => Ok(scan.records - anchor.records),
    }
}

// ---------------------------------------------------------------------------
// The lock
// ---------------------------------------------------------------------------

/// The single-writer lock. It removes the lock file on drop, which is the only
/// side effect a handle has.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // A failure here leaves a stale lock, which a later open refuses
        // loudly. That is the fail-closed direction.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(dir: &Path) -> Result<LockGuard, JournalError> {
    let path = dir.join(JOURNAL_LOCK_NAME);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let description = format!("pid {} opened it", std::process::id());
            let _ = writeln!(file, "{description}");
            let _ = file.sync_all();
            Ok(LockGuard { path })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let holder = std::fs::read_to_string(&path).unwrap_or_default();
            Err(JournalError::JournalLocked {
                path,
                holder: holder.trim().to_string(),
            })
        }
        Err(error) => Err(JournalError::UnwritablePath {
            path,
            reason: error.to_string(),
        }),
    }
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
/// silently pretending to have run. The consequence is bounded: the log frame is
/// already `sync_all`ed before the rename, so a lost anchor can only produce a
/// `NotAJournal` refusal, never a silently accepted truncated log.
#[cfg(unix)]
fn sync_directory(dir: &Path) -> Result<(), JournalError> {
    match std::fs::File::open(dir) {
        Ok(handle) => handle.sync_all().map_err(|error| JournalError::Io {
            path: dir.to_path_buf(),
            reason: format!("cannot flush the directory entry: {error}"),
        }),
        Err(error) => Err(JournalError::Io {
            path: dir.to_path_buf(),
            reason: format!("cannot open the directory to flush it: {error}"),
        }),
    }
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> Result<(), JournalError> {
    // See the unix variant: a Windows directory cannot be flushed through
    // `std`, and the log frame is already durable before the rename.
    Ok(())
}

fn write_anchor(dir: &Path, anchor: &Anchor) -> Result<(), JournalError> {
    let path = dir.join(JOURNAL_ANCHOR_NAME);
    let tmp = dir.join(JOURNAL_ANCHOR_TMP_NAME);
    let bytes = serde_json::to_vec(anchor).map_err(|error| JournalError::Io {
        path: path.clone(),
        reason: format!("cannot serialize the anchor: {error}"),
    })?;
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|error| JournalError::UnwritablePath {
                path: tmp.clone(),
                reason: error.to_string(),
            })?;
        file.write_all(&bytes).map_err(|error| JournalError::Io {
            path: tmp.clone(),
            reason: error.to_string(),
        })?;
        // Flush before the rename: without it a power cut could make the rename
        // durable while the content was not, leaving a new name over nothing.
        file.sync_all().map_err(|error| JournalError::Io {
            path: tmp.clone(),
            reason: format!("cannot flush the anchor: {error}"),
        })?;
    }
    std::fs::rename(&tmp, &path).map_err(|error| JournalError::Io {
        path,
        reason: error.to_string(),
    })?;
    sync_directory(dir)
}

/// Read and verify the completion anchor. Takes the anchor's own path, not the
/// journal directory, so a caller cannot pass the wrong one.
fn read_anchor(path: &Path) -> Result<Anchor, JournalError> {
    let bytes = std::fs::read(path).map_err(|error| JournalError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let anchor: Anchor =
        serde_json::from_slice(&bytes).map_err(|error| JournalError::NotAJournal {
            path: path.to_path_buf(),
            reason: format!("the completion anchor is unreadable: {error}"),
        })?;
    if anchor.schema_version != ANCHOR_SCHEMA_VERSION {
        return Err(JournalError::UnsupportedSchema {
            component: "journal completion anchor".to_string(),
            version: anchor.schema_version,
            supported: ANCHOR_SCHEMA_VERSION,
        });
    }
    if anchor.anchor_checksum != anchor.content_checksum() {
        return Err(JournalError::NotAJournal {
            path: path.to_path_buf(),
            reason: "the completion anchor fails its own checksum".to_string(),
        });
    }
    Ok(anchor)
}

// ---------------------------------------------------------------------------
// LearningJournal
// ---------------------------------------------------------------------------

/// A durable, ordered, idempotent learning-event journal for one model lineage.
///
/// One directory is one journal and one lineage. The handle holds no event, no
/// model, and no schedule; it holds the lock and the ordering report that
/// [`Self::open`] derived from the file.
pub struct LearningJournal {
    dir: PathBuf,
    mode: JournalMode,
    report: JournalReport,
    // Dropped with the handle, which releases the lock. Unread by design.
    _lock: LockGuard,
}

impl fmt::Debug for LearningJournal {
    /// Shows the directory, the mode, and the ordering state read from the
    /// file. The lock is a file path and adds nothing a reader needs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LearningJournal")
            .field("dir", &self.dir)
            .field("mode", &self.mode)
            .field("report", &self.report)
            .finish()
    }
}

impl LearningJournal {
    /// Open the journal in `dir`, deriving all ordering state from the file.
    ///
    /// This is the only constructor, and it is where a restart is recovered: the
    /// whole log is verified and the next sequence is taken from the last frame
    /// on disk, never from a field the previous process left behind. Every
    /// refusal is typed and carries a reason.
    ///
    /// In [`JournalMode::Append`] a missing journal in an existing directory is
    /// created; a missing path is created as a directory. A directory that holds
    /// something this implementation cannot vouch for is refused rather than
    /// adopted.
    pub fn open(dir: &Path, mode: JournalMode) -> Result<Self, JournalError> {
        prepare_directory(dir, mode)?;
        // The lock moves into `open_locked`, so it is released on every refusal
        // as well as on a clean return.
        let lock = acquire_lock(dir)?;
        Self::open_locked(dir, mode, lock)
    }

    fn open_locked(dir: &Path, mode: JournalMode, lock: LockGuard) -> Result<Self, JournalError> {
        let log_path = dir.join(JOURNAL_LOG_NAME);
        let anchor_path = dir.join(JOURNAL_ANCHOR_NAME);
        let log_exists = log_path.exists();
        let anchor_exists = anchor_path.exists();

        if log_exists && !anchor_exists {
            // Completeness cannot be proven without the anchor, so a log that
            // holds records is never adopted. An empty log may be: it carries
            // nothing that could have been lost.
            let length = std::fs::metadata(&log_path)
                .map_err(|error| JournalError::Io {
                    path: log_path.clone(),
                    reason: error.to_string(),
                })?
                .len();
            if length != 0 {
                return Err(JournalError::NotAJournal {
                    path: dir.to_path_buf(),
                    reason: format!(
                        "{} holds records but {} is missing, so its completeness cannot be proven",
                        JOURNAL_LOG_NAME, JOURNAL_ANCHOR_NAME
                    ),
                });
            }
            if mode == JournalMode::Read {
                return Err(JournalError::NotAJournal {
                    path: dir.to_path_buf(),
                    reason: format!("{} is missing", JOURNAL_ANCHOR_NAME),
                });
            }
            write_anchor(dir, &Anchor::fresh().seal())?;
        } else if !log_exists && anchor_exists {
            return Err(JournalError::NotAJournal {
                path: dir.to_path_buf(),
                reason: format!(
                    "{} exists without {}",
                    JOURNAL_ANCHOR_NAME, JOURNAL_LOG_NAME
                ),
            });
        } else if !log_exists {
            if mode == JournalMode::Read {
                return Err(JournalError::NotAJournal {
                    path: dir.to_path_buf(),
                    reason: format!("{} does not exist", JOURNAL_LOG_NAME),
                });
            }
            std::fs::File::create(&log_path).map_err(|error| JournalError::UnwritablePath {
                path: log_path.clone(),
                reason: error.to_string(),
            })?;
            write_anchor(dir, &Anchor::fresh().seal())?;
        }

        let (report, _) = self_scan(&log_path, &anchor_path)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            mode,
            report,
            _lock: lock,
        })
    }

    /// The caller-supplied directory. Every path this module writes is inside it.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What this handle may do.
    pub fn mode(&self) -> JournalMode {
        self.mode
    }

    /// The ordering state derived from the file when the journal was opened.
    pub fn report(&self) -> &JournalReport {
        &self.report
    }

    /// The sequence the next append will use, as read from the file.
    pub fn next_sequence(&self) -> u64 {
        self.report.next_sequence
    }

    /// Refuse to continue unless the file-derived next sequence is `expected`.
    ///
    /// This is the answer to "what happens when the file disagrees with what
    /// memory believed": the file is authoritative, and a caller whose belief
    /// disagrees is told so instead of being allowed to write a record that
    /// would reorder the journal. A restored in-memory sequence of zero against
    /// a journal that holds records is exactly this refusal.
    pub fn expect_next_sequence(&self, expected: u64) -> Result<(), JournalError> {
        let actual = self.report.next_sequence;
        if actual == expected {
            return Ok(());
        }
        Err(JournalError::SequenceConflict {
            expected,
            actual,
            fault: if expected < actual {
                SequenceFault::Rewind
            } else {
                SequenceFault::Missing
            },
        })
    }

    /// Read every record, re-verifying the file and every commit it names.
    ///
    /// This is the only way to obtain a [`JournalRecord`], and it is explicit by
    /// construction: nothing calls it for you, at startup, on a timer, or on
    /// drop. It re-reads the log from disk on every call — the handle caches no
    /// records — and it returns an all-or-nothing result, so a corrupt interior
    /// record refuses the whole read rather than yielding a short list. The
    /// open-time [`Self::report`] is left as it was: this call does not mutate
    /// the handle.
    ///
    /// `store` is required because verification is not optional: every commit a
    /// record names is re-verified through the accepted `ModelCommit::verify`,
    /// `ModelStore::verify_lineage`, and
    /// `ReplayEngine::verify_checkpoint_integrity`, and a record whose commit no
    /// longer verifies is refused rather than reported as present. Nothing from
    /// the store is applied to anything.
    pub fn read_records(&self, store: &ModelStore) -> Result<Vec<JournalRecord>, JournalError> {
        let log_path = self.dir.join(JOURNAL_LOG_NAME);
        let anchor_path = self.dir.join(JOURNAL_ANCHOR_NAME);
        let (_, frames) = self_scan(&log_path, &anchor_path)?;
        let mut records = Vec::with_capacity(frames.len());
        for entry in frames {
            let mut record = entry.body.try_into_record(entry.sequence)?;
            record.frame_checksum = entry.frame_checksum;
            verify_record_commits(&record, store)?;
            records.push(record);
        }
        Ok(records)
    }

    /// Append an event whose samples are the canonical projection of the
    /// canonical evidence retained beside it.
    ///
    /// Every canonical sample is validated with the accepted
    /// `validate_outcome_sample` before anything is written, the projection is
    /// proved, and the accepted `LearningEvent::validate` is run on the result.
    /// Nothing is consumed: the event is written and forgotten.
    pub fn record_canonical(
        &mut self,
        input: CanonicalEvent,
    ) -> Result<RecordOutcome, JournalError> {
        let event_id = input.event_id.clone();
        if is_volatile_event_id(&event_id) {
            return Err(JournalError::VolatileEventId { event_id });
        }
        if event_id.trim().is_empty() {
            return Err(JournalError::EventRejected {
                event_id,
                reason: "event id must not be empty".to_string(),
            });
        }
        if input.model_id.as_str().is_empty() {
            return Err(JournalError::EventRejected {
                event_id,
                reason: "model id must not be empty".to_string(),
            });
        }
        let evidence = JournalEvidence::Canonical {
            samples: input.samples.clone(),
        };
        let samples: Vec<DatasetTrainingSample> = input
            .samples
            .iter()
            .cloned()
            .map(OutcomeTrainingSample::into_legacy)
            .collect();
        let event = LearningEvent {
            event_id,
            model_id: input.model_id,
            samples,
            parent_commit: input.parent_commit,
            result_commit: input.result_commit,
            created_at: input.created_at,
            source: input.source,
        };
        self.append_body(JournalRecordBody {
            schema_version: JOURNAL_SCHEMA_VERSION,
            event_schema_version: LEARNING_EVENT_SCHEMA_VERSION,
            event,
            evidence,
            recorded_at: now_seconds(),
        })
    }

    /// Append a legacy `LearningEvent` whose canonical evidence was not
    /// supplied.
    ///
    /// This is the degraded path, and it is named that way. The record states
    /// its own degradation on disk, so `JournalRecord::is_degraded` is true for
    /// it and a consumer that requires canonical evidence can refuse it. Use
    /// [`Self::record_canonical`] wherever the canonical samples exist.
    pub fn record_legacy(&mut self, event: &LearningEvent) -> Result<RecordOutcome, JournalError> {
        if is_volatile_event_id(&event.event_id) {
            return Err(JournalError::VolatileEventId {
                event_id: event.event_id.clone(),
            });
        }
        self.append_body(JournalRecordBody {
            schema_version: JOURNAL_SCHEMA_VERSION,
            event_schema_version: LEARNING_EVENT_SCHEMA_VERSION,
            event: event.clone(),
            evidence: JournalEvidence::LegacyProjection {
                degradation: LEGACY_DEGRADATION.to_string(),
            },
            recorded_at: now_seconds(),
        })
    }

    /// The validating write path, shared by every entry point.
    ///
    /// The file is re-scanned first, so the ordering state, the idempotency
    /// index, and the frame chain all come from the bytes on disk rather than
    /// from anything this handle remembers.
    pub fn append_body(&mut self, body: JournalRecordBody) -> Result<RecordOutcome, JournalError> {
        if self.mode == JournalMode::Read {
            return Err(JournalError::ReadOnlyJournal {
                path: self.dir.clone(),
            });
        }
        // The shared append boundary enforces the durable-identity rule itself,
        // so a caller that reaches it directly cannot write the process-local
        // `evt-<n>` form that the named entry points already refuse.
        if is_volatile_event_id(&body.event.event_id) {
            return Err(JournalError::VolatileEventId {
                event_id: body.event.event_id.clone(),
            });
        }
        let log_path = self.dir.join(JOURNAL_LOG_NAME);
        let anchor_path = self.dir.join(JOURNAL_ANCHOR_NAME);
        let (report, frames) = self_scan(&log_path, &anchor_path)?;
        // Every stored record is re-validated through the accepted type before
        // anything is appended, so a journal whose history no longer validates
        // cannot be extended.
        for entry in &frames {
            entry.body.try_into_record(entry.sequence)?;
        }

        // Serialize once, up front: these are the bytes that will be written, and
        // the duplicate check below compares against them.
        let body_bytes = serde_json::to_vec(&body).map_err(|error| JournalError::Io {
            path: log_path.clone(),
            reason: format!("cannot serialize the record: {error}"),
        })?;

        if let Some(existing) = frames
            .iter()
            .find(|frame| frame.body.event.event_id == body.event.event_id)
        {
            let sequence = existing.sequence;
            // Compare the bytes this record WOULD have, with the stored
            // record's own volatile timestamps substituted, against the bytes
            // already on disk.
            //
            // Byte comparison is what makes this immune to the JSON float round
            // trip. It HAD to be: `serde_json` was used here without
            // `float_roundtrip`, so the stored body's parsed floats could be one
            // ULP off and comparing parsed values would refuse a legitimate retry
            // of an unchanged record. The workspace now enables `float_roundtrip`,
            // so there is no known drift for this to absorb, and it is kept as
            // defence in depth: idempotency should not rest on how a serializer
            // parses a float.
            //
            // Substituting the stored timestamps keeps the other half of the
            // contract, which is that a retry re-creating the same event at a
            // different wall-clock instant is still the same event.
            let mut comparable = body.clone();
            comparable.recorded_at = existing.body.recorded_at;
            comparable.event.created_at = existing.body.event.created_at;
            let comparable_bytes =
                serde_json::to_vec(&comparable).map_err(|error| JournalError::Io {
                    path: log_path.clone(),
                    reason: format!("cannot serialize the record: {error}"),
                })?;
            if existing.body_bytes == comparable_bytes {
                return Ok(RecordOutcome::Duplicate { sequence });
            }
            return Err(JournalError::IdempotencyConflict {
                event_id: body.event.event_id.clone(),
                reason: format!(
                    "sequence {sequence} already records this event id with different content; \
                     the journal is append-only and never overwrites a record"
                ),
            });
        }

        let sequence = frames
            .last()
            .map_or(FIRST_SEQUENCE, |frame| frame.sequence + 1);
        // Validate before a single byte is written.
        body.try_into_record(sequence)?;

        // The reader refuses a materialized frame above MAX_FRAME_BYTES. The
        // writer has to refuse the identical byte count here: a legal but
        // oversized event would otherwise be appended and leave an append-only
        // log that can never be opened again. The count is the frame the reader
        // materializes: sequence, tab, checksum, tab, body, minus the newline.
        let frame_bytes = sequence.to_string().len() + 1 + 16 + 1 + body_bytes.len();
        if frame_bytes > MAX_FRAME_BYTES {
            return Err(JournalError::FrameTooLarge {
                offset: report.log_bytes,
                bytes: frame_bytes,
            });
        }

        let previous = frames
            .last()
            .map_or(0, |frame: &FrameEntry| frame.frame_checksum);
        let checksum = frame_checksum(previous, sequence, &body_bytes);
        let mut line = Vec::with_capacity(body_bytes.len() + 48);
        line.extend_from_slice(sequence.to_string().as_bytes());
        line.push(b'\t');
        line.extend_from_slice(format_checksum(checksum).as_bytes());
        line.push(b'\t');
        line.extend_from_slice(&body_bytes);
        line.push(b'\n');

        {
            // Append-only, and a single write: a frame is never split across two
            // calls, so a second process appending concurrently cannot interleave
            // bytes inside it. It can duplicate a sequence, which the scan then
            // refuses.
            let mut file = OpenOptions::new()
                .append(true)
                .open(&log_path)
                .map_err(|error| JournalError::UnwritablePath {
                    path: log_path.clone(),
                    reason: error.to_string(),
                })?;
            file.write_all(&line).map_err(|error| JournalError::Io {
                path: log_path.clone(),
                reason: error.to_string(),
            })?;
            file.sync_all().map_err(|error| JournalError::Io {
                path: log_path.clone(),
                reason: format!("cannot flush the log: {error}"),
            })?;
        }

        // The anchor is written from a re-read of the log, not from what the
        // writer believes it wrote. The re-read also proves the new frame landed
        // intact: its checksum and its chain link are verified here, and a frame
        // that did not survive is an error rather than a silent anchor update.
        let verified = scan_log(&log_path)?;
        write_anchor(&self.dir, &Anchor::from_scan(&verified).seal())?;
        self.report = JournalReport {
            records: verified.records,
            next_sequence: verified.next_sequence(),
            log_bytes: verified.log_bytes,
            records_beyond_anchor: 0,
        };
        Ok(RecordOutcome::Appended {
            sequence,
            bytes: line.len() as u64,
        })
    }
}

/// Verify every commit a record names, through the accepted checks.
fn verify_record_commits(record: &JournalRecord, store: &ModelStore) -> Result<(), JournalError> {
    let candidates = [
        record.event.parent_commit.as_ref(),
        record.event.result_commit.as_ref(),
    ];
    for commit_id in candidates.into_iter().flatten() {
        store.verify_lineage(commit_id).map_err(|error| {
            JournalError::CommitVerificationFailed {
                sequence: record.sequence,
                commit_id: commit_id.clone(),
                reason: format!("lineage does not verify: {error}"),
            }
        })?;
        let commit = store.checkout_commit(commit_id).map_err(|error| {
            JournalError::CommitVerificationFailed {
                sequence: record.sequence,
                commit_id: commit_id.clone(),
                reason: error.to_string(),
            }
        })?;
        if !commit.verify() {
            return Err(JournalError::CommitVerificationFailed {
                sequence: record.sequence,
                commit_id: commit_id.clone(),
                reason: "ModelCommit::verify rejected it".to_string(),
            });
        }
        if !ReplayEngine::verify_checkpoint_integrity(&commit.checkpoint) {
            return Err(JournalError::CommitVerificationFailed {
                sequence: record.sequence,
                commit_id: commit_id.clone(),
                reason: "verify_checkpoint_integrity rejected its checkpoint".to_string(),
            });
        }
        if commit.model_id != record.event.model_id {
            return Err(JournalError::CommitVerificationFailed {
                sequence: record.sequence,
                commit_id: commit_id.clone(),
                reason: format!(
                    "the commit belongs to model '{}' but the event belongs to '{}'",
                    commit.model_id, record.event.model_id
                ),
            });
        }
    }
    Ok(())
}

/// The process-local id form `LearningEvent::new` mints.
///
/// A deduplication key that restarts at one in every process is not a durable
/// identity: two processes would collide on `evt-1` and a restart would make a
/// journal look like it had already recorded an unrelated event.
fn is_volatile_event_id(event_id: &str) -> bool {
    match event_id.strip_prefix("evt-") {
        Some(suffix) => !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()),
        None => false,
    }
}

/// Scan the log and reconcile it with the anchor.
fn self_scan(
    log_path: &Path,
    anchor_path: &Path,
) -> Result<(JournalReport, Vec<FrameEntry>), JournalError> {
    let scan = scan_log(log_path)?;
    let anchor = read_anchor(anchor_path)?;
    let beyond = reconcile_anchor(&anchor, &scan, anchor_path)?;
    Ok((
        JournalReport {
            records: scan.records,
            next_sequence: scan.next_sequence(),
            log_bytes: scan.log_bytes,
            records_beyond_anchor: beyond,
        },
        scan.frames,
    ))
}

/// Prepare the caller's directory. Nothing outside it is ever created.
fn prepare_directory(dir: &Path, mode: JournalMode) -> Result<(), JournalError> {
    match std::fs::metadata(dir) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(JournalError::NotADirectory {
                    path: dir.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if mode == JournalMode::Read {
                return Err(JournalError::NotAJournal {
                    path: dir.to_path_buf(),
                    reason: "the path does not exist and the journal is opened for reading"
                        .to_string(),
                });
            }
            std::fs::create_dir_all(dir).map_err(|error| JournalError::UnwritablePath {
                path: dir.to_path_buf(),
                reason: error.to_string(),
            })?;
        }
        Err(error) => {
            return Err(JournalError::UnwritablePath {
                path: dir.to_path_buf(),
                reason: error.to_string(),
            })
        }
    }
    Ok(())
}
