//! What a run produced: the outcome, and the file snapshots a rollback restores from.

use super::state::MigrationState;
use serde::{Deserialize, Serialize};

// MigrationResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub state: MigrationState,
    pub steps_completed: usize,
    pub steps_total: usize,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub duration_ms: u64,
    /// Whether the pre-migration state was actually put back.
    ///
    /// Not the same question as `state`, and the distinction is the point. The
    /// state machine records that a rollback *transition* happened. This records
    /// whether the files were restored. They diverge exactly when a restore
    /// fails, which is the case a caller most needs to notice: the transition says
    /// `RolledBack` while the config on disk is half-migrated.
    ///
    /// **CORRECTED.** This documentation previously said "a rollback that
    /// restored nothing, or restored everything, reports `true`", which
    /// contradicted the two sentences above it and undid the field's own reason
    /// for existing. Under that wording an interrupted migration recovered with no
    /// snapshot reported `true`: the transition said `RolledBack`, no file came
    /// back, and a caller asking "is the installation safe" was told yes.
    ///
    /// It now reports `true` only when at least one file was restored and none
    /// failed. A rollback with a failed restore reports `false` and carries the
    /// reason in `errors` — a warning is the wrong place for it, because every
    /// caller checks `errors.is_empty()` and none of them are obliged to read
    /// warnings. **This is a public-field semantic change** and the seven fixtures
    /// that asserted the old wording were updated rather than deleted; each keeps
    /// the state-transition assertion it was actually written for.
    pub rolled_back: bool,
    /// Snapshot files a rollback put back successfully.
    #[serde(default)]
    pub restored_files: Vec<String>,
    /// Snapshot files a rollback could not put back.
    ///
    /// Non-empty leaves the installation in a state nobody can describe from the
    /// code alone, which is why it is a first-class field rather than something
    /// to be inferred from the length of `warnings`.
    #[serde(default)]
    pub failed_restores: Vec<String>,
}

// ---------------------------------------------------------------------------
// MigrationSnapshot
// ---------------------------------------------------------------------------

/// A single file captured before migration for rollback purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub content: Vec<u8>,
    pub hash: u64,
}

/// A point-in-time snapshot of files that can be restored during rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationSnapshot {
    pub files: Vec<SnapshotFile>,
    pub created_at: i64,
}

/// Simple hash for snapshot integrity verification.
pub(crate) fn compute_hash(data: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
