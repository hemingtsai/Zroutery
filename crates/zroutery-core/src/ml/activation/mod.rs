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

mod records;
mod snapshot;
mod store;

pub use records::*;
pub use snapshot::*;
pub use store::*;

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
