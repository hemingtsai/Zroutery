use std::fs;

use zroutery_core::ml::activation::{
    ActivationError, ActivationStore, SnapshotId, ACTIVATION_LOCK_NAME, SNAPSHOT_ID_PREFIX,
};

use super::support::*;

// ---------------------------------------------------------------------------
// Gate 4, part one: unwritable and wrong-shaped locations
// ---------------------------------------------------------------------------

#[test]
fn an_unwritable_or_wrong_shaped_location_is_refused() {
    let temp = scratch();
    let blocker = temp.path().join("blocker");
    fs::write(&blocker, "not a directory").expect("writable");

    // The root is a file.
    match ActivationStore::open(&blocker) {
        Err(ActivationError::NotADirectory { path }) => assert_eq!(path, blocker),
        other => panic!("expected a NotADirectory refusal, got {other:?}"),
    }

    // The root cannot be created because its parent is a file.
    match ActivationStore::open(&blocker.join("store")) {
        Err(ActivationError::Unwritable { reason, .. }) => assert!(!reason.is_empty()),
        other => panic!("expected an Unwritable refusal, got {other:?}"),
    }

    // The snapshot location is a file rather than a directory.
    let root = store_root(temp.path());
    fs::create_dir_all(&root).expect("writable");
    fs::write(root.join("snapshots"), "not a directory").expect("writable");
    match ActivationStore::open(&root) {
        Err(ActivationError::NotADirectory { path }) => assert!(path.ends_with("snapshots")),
        other => panic!("expected a NotADirectory refusal, got {other:?}"),
    }

    // A path that is a directory where a snapshot file belongs is not a
    // snapshot. This is a second store, in a clean place.
    let root = temp.path().join("second-store");
    let activation = ActivationStore::open(&root).expect("the store opens");
    let absent = SnapshotId::new(format!("{SNAPSHOT_ID_PREFIX}0123456789abcdef"));
    fs::create_dir_all(snapshot_file(&root, &absent)).expect("writable");
    match activation.read_snapshot(&absent) {
        Err(ActivationError::NotASnapshot { reason, .. }) => {
            assert!(
                reason.contains("directory"),
                "the reason says why: {reason}"
            )
        }
        other => panic!("expected a NotASnapshot refusal, got {other:?}"),
    }
}

#[test]
fn a_store_held_by_another_handle_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let first = ActivationStore::open(&root).expect("the first handle opens");
    assert!(
        root.join(ACTIVATION_LOCK_NAME).is_file(),
        "the lock is present while a handle is live"
    );

    match ActivationStore::open(&root) {
        Err(ActivationError::StoreLocked { path, .. }) => {
            assert!(path.ends_with(ACTIVATION_LOCK_NAME))
        }
        other => panic!("expected a StoreLocked refusal, got {other:?}"),
    }

    // A stale lock left by a process that died is refused loudly rather than
    // being broken, because deciding which of two writers came first would be
    // last-write-wins on the pointer.
    drop(first);
    assert!(
        !root.join(ACTIVATION_LOCK_NAME).exists(),
        "the lock is released with the handle"
    );
    let _second = ActivationStore::open(&root).expect("the lock is free again");

    fs::write(root.join(ACTIVATION_LOCK_NAME), "pid 1 died here").expect("writable");
    match ActivationStore::open(&root) {
        Err(ActivationError::StoreLocked { holder, .. }) => assert!(holder.contains("pid 1")),
        other => panic!("expected a StoreLocked refusal, got {other:?}"),
    }
}
