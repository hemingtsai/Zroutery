//! The durable record: which state the migration reached, and what it did on the way there.

use super::result::{MigrationResult, MigrationSnapshot};
use super::state::MigrationState;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

// MigrationStore
// ---------------------------------------------------------------------------

/// The durable record of where a migration got to.
///
/// Written as a whole file rather than mutated in place, because a migration that
/// claims to be `Switched` while the file that records it is half-written is worse
/// than one that admits it does not know.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigrationManifest {
    state: MigrationState,
    history: Vec<MigrationResult>,
}

/// Thread-safe store that tracks the current migration state and records
/// completed migration results.
///
/// # Why this can be durable
///
/// The state machine has seven states and three of them mean "work was started and
/// not finished": `Prepared`, `Verified` and `Switched`. A store that lives only
/// in a `Mutex` loses exactly the states that need recovering. After a restart it
/// reports `Detected`, and:
/// * [`MigrationExecutor::check_already_migrated`] returns `false` for a system
///   that was already migrated, so the migration runs a second time;
/// * [`MigrationExecutor::recover`] returns `Err("cannot recover from state
///   Detected")` for the interrupted state it exists to recover from;
/// * the snapshot that rollback restores from is gone, because it lived in the
///   same process memory.
///
/// None of those is a hypothetical: they are what the in-memory-only store does
/// on every restart, and no gate covered it, because every fixture builds its store
/// with [`MigrationStore::new`] inside one process.
///
/// [`MigrationStore::new`] keeps the in-memory behaviour, so a caller that does
/// not opt in pays nothing. [`MigrationStore::open`] persists.
pub struct MigrationStore {
    state: Mutex<MigrationState>,
    history: Mutex<Vec<MigrationResult>>,
    /// Where the manifest lives. `None` means this store is memory only and every
    /// mutation is a no-op on disk.
    path: Option<std::path::PathBuf>,
    /// The last persistence failure, if any.
    ///
    /// A store cannot make `record_result` fallible without changing every caller,
    /// so a failed write would otherwise be invisible: the in-memory state advances
    /// and the file does not, and the next process disagrees with this one. That
    /// divergence is exactly what this field exists to make visible.
    last_persist_error: Mutex<Option<String>>,
}

impl MigrationStore {
    /// Creates a new store starting in the `Detected` state.
    ///
    /// Memory only. Use [`MigrationStore::open`] when the state has to outlive the
    /// process, which for any migration that switches a system it means always.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MigrationState::Detected),
            history: Mutex::new(Vec::new()),
            path: None,
            last_persist_error: Mutex::new(None),
        }
    }

    /// Open a store backed by a manifest at `path`, creating it if absent.
    ///
    /// A manifest that exists but cannot be read is **refused**, not treated as
    /// absent. Reading it as absent would start from `Detected` and re-run a
    /// migration over a system that may already be switched — the one mistake this
    /// type exists to prevent, reached by ignoring the only record that could have
    /// prevented it.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let recovered = match std::fs::read(&path) {
            Ok(bytes) => Some(serde_json::from_slice::<MigrationManifest>(&bytes).map_err(
                |e| {
                    format!(
                        "migration manifest at {} is unreadable: {e}. Refusing to start with no \
                     record of what was already migrated.",
                        path.display()
                    )
                },
            )?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };

        Ok(Self {
            state: Mutex::new(
                recovered
                    .as_ref()
                    .map(|m| m.state)
                    .unwrap_or(MigrationState::Detected),
            ),
            history: Mutex::new(recovered.map(|m| m.history).unwrap_or_default()),
            path: Some(path),
            last_persist_error: Mutex::new(None),
        })
    }

    /// Whether this store persists its manifest.
    ///
    /// A caller that is about to change a system should be able to say out loud
    /// whether the fact is being written down. One that cannot say yes is running
    /// a migration it will not remember.
    pub fn is_durable(&self) -> bool {
        self.path.is_some()
    }

    /// The most recent persistence failure, if the last write did not land.
    ///
    /// `record_result` returns `()`, so a failed write has nowhere else to go. A
    /// store whose file is unwritable must not look identical to one that is being
    /// written.
    pub fn persist_error(&self) -> Option<String> {
        self.last_persist_error.lock().unwrap().clone()
    }

    /// Where a durable store keeps its rollback snapshot.
    ///
    /// Beside the manifest rather than inside it: a snapshot holds the verbatim
    /// bytes of the user's configuration files, and a manifest that is rewritten
    /// on every state transition should not have to rewrite those bytes each time.
    fn snapshot_path(&self) -> Option<std::path::PathBuf> {
        self.path
            .as_ref()
            .map(|p| p.with_extension("snapshot.json"))
    }

    /// Write a rollback snapshot beside the manifest, if this store is durable.
    ///
    /// The snapshot is what [`MigrationExecutor::recover`] restores from after a
    /// restart. Persisting the state without it would turn recovery from an honest
    /// refusal into a silent no-op: the store would know it was interrupted and
    /// restore none of the files it had captured. `rolled_back` would be `false`,
    /// so it would not *claim* success — but nothing would come back either, and a
    /// caller that only checks the state machine would see `RolledBack`.
    pub fn save_snapshot(&self, snapshot: &MigrationSnapshot) -> Result<(), String> {
        let Some(path) = self.snapshot_path() else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
        crate::agent_takeover::write_config_atomic(&path, &bytes)
    }

    /// Read the persisted rollback snapshot, if there is one.
    ///
    /// A snapshot that exists but cannot be read returns `Err` rather than `None`:
    /// returning `None` would mean "nothing was captured", which sends
    /// [`MigrationExecutor::recover`] down the same path as having captured
    /// nothing, when in fact files were captured and the record of them is damaged.
    pub fn load_snapshot(&self) -> Result<Option<MigrationSnapshot>, String> {
        let Some(path) = self.snapshot_path() else {
            return Ok(None);
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<MigrationSnapshot>(&bytes)
                .map(Some)
                .map_err(|e| {
                    format!(
                        "rollback snapshot at {} is unreadable: {e}. Refusing to report a \
                         recovery that restores nothing.",
                        path.display()
                    )
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Write the current state and history, if this store is durable.
    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let manifest = MigrationManifest {
            state: *self.state.lock().unwrap(),
            history: self.history.lock().unwrap().clone(),
        };
        let outcome = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| e.to_string())
            .and_then(|bytes| crate::agent_takeover::write_config_atomic(path, &bytes));
        let mut slot = self.last_persist_error.lock().unwrap();
        match outcome {
            Ok(()) => *slot = None,
            Err(error) => {
                tracing::error!(
                    path = %path.display(),
                    %error,
                    "migration state advanced in memory but was not written; a restart will \
                     disagree with this process"
                );
                *slot = Some(format!("{}: {error}", path.display()));
            }
        }
    }

    /// Returns the current migration state.
    pub fn current_state(&self) -> MigrationState {
        *self.state.lock().unwrap()
    }

    /// Attempts a state transition. Returns `Ok(())` on success, or
    /// `Err(MigrationState)` with the (unchanged) current state when the
    /// transition is not allowed.  Transitioning to the same state is a no-op
    /// that always succeeds (idempotent).
    pub fn transition(&self, target: MigrationState) -> Result<(), MigrationState> {
        let mut current = self.state.lock().unwrap();
        if *current == target {
            return Ok(());
        }
        if current.can_transition_to(target) {
            *current = target;
            drop(current);
            self.persist();
            Ok(())
        } else {
            Err(*current)
        }
    }

    /// Appends a migration result to the history.
    pub fn record_result(&self, result: MigrationResult) {
        self.history.lock().unwrap().push(result);
        self.persist();
    }

    /// Returns a snapshot of all recorded migration results.
    pub fn history(&self) -> Vec<MigrationResult> {
        self.history.lock().unwrap().clone()
    }
}

impl std::fmt::Debug for MigrationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationStore")
            .field("state", &self.current_state())
            .field("results", &self.history.lock().unwrap().len())
            .field("durable", &self.path.is_some())
            .field("persist_error", &self.persist_error())
            .finish()
    }
}

impl Default for MigrationStore {
    fn default() -> Self {
        Self::new()
    }
}
