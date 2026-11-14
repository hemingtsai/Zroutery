use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ml::journal::{JournalMode, LearningJournal, RecordOutcome};
use crate::ml::model_identity::{
    ModelCommit, ModelStore, MODEL_CHECKPOINT_SCHEMA_VERSION, MODEL_COMMIT_SCHEMA_VERSION,
};

use super::records::{
    activation_applied_event_id, activation_plan_event_id, journal_error, parse_activation_record,
    pending_intent, record_activation, ActivationAudit, ActivationError, ActivationKind,
    ActivationOutcome, ActivationRequest, ActivationStage, JournalContext, PointerDisagreement,
    PointerState,
};
use super::snapshot::{
    check_envelope_version, checked_id, probe_envelope_versions, snapshot_id_for, verify_commit,
    verify_commit_in_store, ActivationEntry, ActivationPointer, ActiveSnapshot, CommitFile,
    PointerFile, Snapshot, SnapshotFile, SnapshotId,
};
use super::{
    ACTIVATION_LOCK_NAME, ACTIVATION_POINTER_NAME, ACTIVATION_POINTER_TMP_NAME,
    ACTIVATION_SNAPSHOT_SCHEMA_VERSION, FIRST_GENERATION, JOURNAL_DIR_NAME, SNAPSHOTS_DIR_NAME,
    SNAPSHOT_FILE_SUFFIX, SNAPSHOT_INCOMING_SUFFIX,
};

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
/// the handle holds the lock and nothing else: a transition mutex that guards
/// the pointer's read-modify-write within one process, and no model, predictor,
/// or serving handle.
pub struct ActivationStore {
    root: PathBuf,
    // Dropped with the handle, which releases the lock. Unread by design.
    _lock: LockGuard,
    // Serialises the pointer's read-modify-write within this process. Unread by
    // design: it guards the critical section and holds no state of its own.
    transition: Mutex<()>,
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
                transition: Mutex::new(()),
            }),
            Err(error) => {
                drop(lock);
                Err(error)
            }
        }
    }

    /// Enter the in-process pointer transition.
    ///
    /// Every mutating method takes `&self`, so two callers can share one store.
    /// Without serialising the whole read-verify-rename, both can read
    /// generation G and both commit G+1: the pointer is replaced twice and a
    /// flip is silently lost. The guard is held across that whole sequence, so a
    /// generation that has been consumed can never be observed twice. A poisoned
    /// lock is recovered rather than propagated, because the pointer *file* — not
    /// any in-memory state — is the authority, and it is re-read inside the
    /// guard.
    fn enter_transition(&self) -> MutexGuard<'_, ()> {
        self.transition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let _transition = self.enter_transition();
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
    /// The whole sequence runs under the store's in-process transition guard, so
    /// two callers sharing one store cannot both read the same generation and
    /// both commit it: the loser observes the flip the winner made.
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
        let _transition = self.enter_transition();
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
        let _transition = self.enter_transition();
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
        let _transition = self.enter_transition();
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
