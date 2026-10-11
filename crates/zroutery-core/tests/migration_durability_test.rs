//! `MigrationStore` across a process boundary.
//!
//! # Why this file exists
//!
//! `MigrationStore` was a pair of `Mutex`es. Its state machine has seven states,
//! and three of them mean *work was started and not finished*: `Prepared`,
//! `Verified`, `Switched`. A store that lives only in memory loses exactly those
//! states, and it loses them on every restart. Nothing caught it, because every
//! fixture in the repository builds its store with `MigrationStore::new()` and
//! runs the whole story inside one process.
//!
//! Three methods document behaviour that could not happen:
//!
//! * `check_already_migrated` is documented as returning `true` when the migration
//!   has already been fully applied. In a fresh process it read `Detected` and
//!   returned `false`, so the migration ran a second time over a system that was
//!   already switched.
//! * `recover` exists to recover an interrupted migration. A fresh process read
//!   `Detected` and returned `Err("cannot recover from state Detected")` — it
//!   refused the exact states it was written for.
//! * `create_snapshot` captured file contents in memory and `recover` passes
//!   `None` for the snapshot, so a restart had nothing to restore from.
//!
//! A restart is simulated the only way it can honestly be simulated: by dropping
//! every in-memory object and opening a new store over the same path. Reusing one
//! `MigrationStore` and calling a method on it again would not be a restart, and a
//! fixture that pretended to be one would have passed against the old code.
//!
//! # What is deliberately not asserted
//!
//! Nothing here runs a migration end to end. These gates are about whether the
//! *state* survives, which is the property that was missing and the one every
//! other claim about migration depends on.

use std::path::PathBuf;

use zroutery_core::migration::{
    MigrationExecutor, MigrationResult, MigrationState, MigrationStore, SnapshotWriter,
};

fn result(state: MigrationState) -> MigrationResult {
    MigrationResult {
        state,
        steps_completed: 2,
        steps_total: 2,
        errors: Vec::new(),
        warnings: Vec::new(),
        duration_ms: 12,
        rolled_back: false,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    }
}

/// A manifest path inside a temporary directory that does not exist yet.
///
/// Returns the guard as well as the path: the guard owns the directory and
/// removes it when the test ends. The first version of this file built the path
/// by hand and removed the directory only on the way *in*, which left ninety-seven
/// directories behind in the temp folder after one workspace run. A gate that
/// litters is a gate that eventually breaks something that is not the code it
/// was written to test.
fn fresh_path(_name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    // Nested, so the manifest's parent has to be created rather than assumed.
    let path = dir.path().join("nested").join("migration.json");
    (dir, path)
}
/// GATE 1: a store opened with no manifest is new, memory-only-but-durable.
///
/// The fresh case has to behave exactly like the old constructor while being
/// honest about the fact that it will now be written.
#[test]
fn an_absent_manifest_opens_a_fresh_durable_store() {
    let (_dir, path) = fresh_path("fresh");
    let store = MigrationStore::open(&path).expect("a fresh store opens");

    assert!(
        store.is_durable(),
        "open() must persist, or it is just new()"
    );
    assert_eq!(store.current_state(), MigrationState::Detected);
    assert!(store.history().is_empty());
    assert_eq!(
        store.persist_error(),
        None,
        "a store that has not written yet has not failed"
    );
}

/// GATE 2: `new()` is still memory only.
///
/// The in-memory constructor is what every existing fixture uses, and it must not
/// silently start writing files. If it did, eighty fixtures would acquire a
/// dependency on a writable temporary directory.
#[test]
fn the_memory_constructor_stays_memory_only() {
    let store = MigrationStore::new();
    assert!(!store.is_durable());
    store
        .transition(MigrationState::Prepared)
        .expect("transition is allowed");
    store.record_result(result(MigrationState::Prepared));
    assert!(store.persist_error().is_none());
}

/// GATE 3: THE GATE. A completed migration is still completed after a restart.
///
/// This is `check_already_migrated` doing what its own documentation says. Against
/// the previous implementation this failed: a new store read `Detected`, so the
/// idempotence check that exists to stop a migration running twice would have
/// reported "not migrated" about a system that was migrated.
#[test]
fn a_completed_migration_is_still_completed_after_a_restart() {
    let (_dir, path) = fresh_path("completed");

    {
        let store = MigrationStore::open(&path).expect("open");
        store
            .transition(MigrationState::Prepared)
            .expect("prepared");
        store
            .transition(MigrationState::Verified)
            .expect("verified");
        store
            .transition(MigrationState::Switched)
            .expect("switched");
        store
            .transition(MigrationState::Completed)
            .expect("completed");
        store.record_result(result(MigrationState::Completed));
    } // the process ends here, with everything dropped

    let reopened = MigrationStore::open(&path).expect("reopen");
    assert_eq!(
        reopened.current_state(),
        MigrationState::Completed,
        "a restart must not forget that the work was already done"
    );
    assert_eq!(
        reopened.history().len(),
        1,
        "the record of what happened must survive the process that did it"
    );
}

/// GATE 4: THE GATE. An interrupted migration is recoverable after a restart.
///
/// `recover` dispatches on `Prepared | Verified | Switched`. A fresh process read
/// `Detected` and fell through to the error arm, refusing the three states the
/// method exists for. Against the previous implementation this failed with
/// `Err("cannot recover from state Detected")`.
#[test]
fn an_interrupted_migration_is_recoverable_after_a_restart() {
    let (_dir, path) = fresh_path("interrupted");

    {
        let store = MigrationStore::open(&path).expect("open");
        store
            .transition(MigrationState::Prepared)
            .expect("prepared");
        store
            .transition(MigrationState::Verified)
            .expect("verified");
        // The process dies here, mid-switch. `Switched` is the state a half-done
        // migration is left in, and it is the one the old store could not survive.
        store
            .transition(MigrationState::Switched)
            .expect("switched");
    }

    let reopened = MigrationStore::open(&path).expect("reopen");
    assert_eq!(
        reopened.current_state(),
        MigrationState::Switched,
        "the interrupted state is exactly the one that has to survive"
    );
}

/// GATE 5: a manifest that exists but is unreadable is refused, not treated as
/// absent.
///
/// The dangerous failure here is not a crash. It is starting from `Detected`
/// because a file could not be parsed, and then re-running a migration over a
/// system that was already switched. The only record that could have prevented
/// that is the record we just failed to read.
#[test]
fn a_corrupt_manifest_is_refused_rather_than_read_as_absent() {
    let (_dir, path) = fresh_path("corrupt");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create the directory");
    std::fs::write(&path, b"{ this is not json").expect("write the corrupt bytes");

    let error = MigrationStore::open(&path).expect_err("a corrupt manifest must be refused");
    assert!(
        error.contains("unreadable"),
        "the error must say the file was unreadable, got: {error}"
    );
}

/// GATE 6: a corrupt manifest is not silently replaced either.
///
/// An `open` that refused must not also have blanked the file, because the next
/// `open` would then succeed and report a clean `Detected`. The refusal has to be
/// stable across attempts or it is only a warning.
#[test]
fn refusing_a_corrupt_manifest_does_not_overwrite_it() {
    let (_dir, path) = fresh_path("corrupt-stable");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create the directory");
    let corrupt = b"{ this is not json";
    std::fs::write(&path, corrupt).expect("write the corrupt bytes");

    assert!(
        MigrationStore::open(&path).is_err(),
        "first open must refuse"
    );
    let after = std::fs::read(&path).expect("the file must still be there");
    assert_eq!(
        after, corrupt,
        "a refused manifest must be left exactly as it was found"
    );
    assert!(
        MigrationStore::open(&path).is_err(),
        "the refusal must be stable, not a one-shot warning"
    );
}

/// GATE 7: a rejected transition is not written.
///
/// `transition` refuses an illegal move and returns the unchanged state. If the
/// refusal were persisted anyway the manifest would claim something the store
/// does not believe, and the next process would inherit the lie.
#[test]
fn a_rejected_transition_does_not_reach_the_manifest() {
    let (_dir, path) = fresh_path("rejected");

    {
        let store = MigrationStore::open(&path).expect("open");
        // `Detected` cannot jump straight to `Completed`.
        let refused = store.transition(MigrationState::Completed);
        assert!(
            refused.is_err(),
            "the transition should have been refused, got {refused:?}"
        );
        assert_eq!(store.current_state(), MigrationState::Detected);
    }

    let reopened = MigrationStore::open(&path).expect("reopen");
    assert_eq!(
        reopened.current_state(),
        MigrationState::Detected,
        "a refused transition reached the manifest"
    );
}

/// GATE 8: every recorded result survives, in order.
///
/// Order matters: a history that comes back reordered is a history that cannot be
/// read as "what happened, in order", which is the only reason to keep one.
#[test]
fn history_survives_a_restart_in_order() {
    let (_dir, path) = fresh_path("history");

    {
        let store = MigrationStore::open(&path).expect("open");
        for state in [
            MigrationState::Prepared,
            MigrationState::Verified,
            MigrationState::Failed,
        ] {
            store.record_result(result(state));
        }
    }

    let reopened = MigrationStore::open(&path).expect("reopen");
    let history = reopened.history();
    assert_eq!(history.len(), 3);
    let states: Vec<MigrationState> = history.iter().map(|r| r.state).collect();
    assert_eq!(
        states,
        vec![
            MigrationState::Prepared,
            MigrationState::Verified,
            MigrationState::Failed,
        ],
        "history came back reordered or truncated"
    );
}

/// GATE 9: the manifest is created 0600.
///
/// The manifest records which files were copied and the outcome of every step,
/// and a migration's `CopyConfig` step names the user's real configuration paths.
/// A world-readable record of those is a small leak for no benefit, and the
/// takeover manifest already sets the convention this follows.
#[cfg(unix)]
#[test]
fn the_manifest_is_created_with_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, path) = fresh_path("perms");
    let store = MigrationStore::open(&path).expect("open");
    store
        .transition(MigrationState::Prepared)
        .expect("prepared");

    let mode = std::fs::metadata(&path)
        .expect("the manifest must exist")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the manifest must not be readable more widely than its owner"
    );
}

/// A writer that records what it was asked to write, standing in for the disk.
///
/// The point of this gate is *which bytes reach the writer*, not whether a real
/// file changed, so a recorder makes the assertion exact rather than dependent on
/// filesystem timing.
#[derive(Default)]
struct Recorder {
    written: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
}

impl SnapshotWriter for Recorder {
    fn write(&self, path: &str, content: &[u8]) -> Result<(), String> {
        self.written
            .lock()
            .unwrap()
            .push((path.to_string(), content.to_vec()));
        Ok(())
    }
}

/// GATE 10: a snapshot survives the process that captured it.
///
/// Persisting the state without persisting the snapshot would make `recover` an
/// honest-looking no-op: it would find the interrupted state, transition, and
/// restore nothing. This is the gate that distinguishes "the store remembers it
/// was interrupted" from "the store can undo it".
#[test]
fn a_captured_snapshot_survives_a_restart() {
    let (_dir, path) = fresh_path("snapshot");

    {
        let store = MigrationStore::open(&path).expect("open");
        let executor = MigrationExecutor::new(store);
        // A snapshot of files that do not exist captures nothing, so the gate
        // writes one for real rather than trusting an empty vector to round-trip.
        let source = _dir.path().join("source.txt");
        std::fs::write(&source, b"the original bytes").expect("write the source file");

        let snapshot = executor
            .create_snapshot(&[source.to_str().expect("a utf-8 path")])
            .expect("capture a snapshot");
        assert_eq!(
            snapshot.files.len(),
            1,
            "the fixture must actually capture a file, or it proves nothing"
        );
    } // the process ends here

    let reopened = MigrationStore::open(&path).expect("reopen");
    let loaded = reopened
        .load_snapshot()
        .expect("the snapshot must load")
        .expect("a snapshot was captured, so one must be there");
    assert_eq!(loaded.files.len(), 1);
    assert_eq!(loaded.files[0].content, b"the original bytes");
}

/// GATE 11: `recover` after a restart restores the captured bytes.
///
/// This is the whole point of the two halves together. Before this change `recover`
/// passed `None` to `rollback`, so it transitioned to `RolledBack` and wrote
/// nothing: the state machine said recovered and no file came back.
#[test]
fn recover_after_a_restart_actually_restores_the_files() {
    let (_dir, path) = fresh_path("recover");
    let source = _dir.path().join("source.txt");
    std::fs::write(&source, b"the original bytes").expect("write the source file");
    let source_path = source.to_str().expect("a utf-8 path").to_string();

    {
        let store = MigrationStore::open(&path).expect("open");
        // The executor holds the store, so the transitions that follow the capture
        // are made through a second handle to the same manifest rather than by
        // keeping a clone alive for the sake of the test.
        MigrationExecutor::new(MigrationStore::open(&path).expect("reopen"))
            .create_snapshot(&[&source_path])
            .expect("capture a snapshot");
        store
            .transition(MigrationState::Prepared)
            .expect("prepared");
        store
            .transition(MigrationState::Verified)
            .expect("verified");
        store
            .transition(MigrationState::Switched)
            .expect("switched");
    } // the process dies mid-migration

    let reopened = MigrationStore::open(&path).expect("reopen");
    let executor = MigrationExecutor::new(reopened);

    let recorder = Recorder::default();
    let snapshot = executor.snapshot_from_disk();
    let result = executor.rollback_with(snapshot.as_ref(), &recorder);

    assert!(
        result.rolled_back,
        "a rollback that restored the captured file must report it"
    );
    assert_eq!(
        result.restored_files,
        vec![source_path.clone()],
        "the captured file must be the one restored"
    );
    let written = recorder.written.lock().unwrap().clone();
    assert_eq!(written.len(), 1, "exactly one restore write");
    assert_eq!(written[0].0, source_path);
    assert_eq!(
        written[0].1, b"the original bytes",
        "the restored bytes must be the captured ones, not empty"
    );
}

/// GATE 12: recovery with no captured snapshot restores nothing and says so.
///
/// The control for GATE 11. If this also reported `rolled_back`, then GATE 11
/// would be asserting nothing and the distinction the two halves draw would be
/// untested.
#[test]
fn a_recovery_with_no_snapshot_restores_nothing_and_says_so() {
    let (_dir, path) = fresh_path("no-snapshot");

    {
        let store = MigrationStore::open(&path).expect("open");
        store
            .transition(MigrationState::Prepared)
            .expect("prepared");
        store
            .transition(MigrationState::Verified)
            .expect("verified");
        store
            .transition(MigrationState::Switched)
            .expect("switched");
    }

    let reopened = MigrationStore::open(&path).expect("reopen");
    let executor = MigrationExecutor::new(reopened);
    let recorder = Recorder::default();

    let snapshot = executor.snapshot_from_disk();
    let result = executor.rollback_with(snapshot.as_ref(), &recorder);

    assert!(
        !result.rolled_back,
        "a rollback that restored nothing must not report that it did"
    );
    assert!(result.restored_files.is_empty());
    assert!(
        recorder.written.lock().unwrap().is_empty(),
        "nothing must be written when nothing was captured"
    );
}
