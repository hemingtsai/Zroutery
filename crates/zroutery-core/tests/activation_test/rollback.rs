use std::fs;

use zroutery_core::ml::activation::{
    ActivationError, ActivationKind, ActivationStore, PointerFile, JOURNAL_DIR_NAME,
};
use zroutery_core::ml::journal::RecordOutcome;
use zroutery_core::ml::model_identity::{ModelId, ModelStore};

use super::support::*;

// ---------------------------------------------------------------------------
// Gate 3: rollback
// ---------------------------------------------------------------------------

#[test]
fn rollback_is_atomic_journaled_and_refused_without_a_previous_snapshot() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, id_a) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("activated");

    // No previous snapshot: refused, and nothing at all happens.
    let pointer_before = pointer_bytes(&root).expect("the pointer exists");
    let log_before = log_bytes(&root);
    match activation.rollback(&store) {
        Err(ActivationError::NoPreviousSnapshot { generation, .. }) => assert_eq!(generation, 1),
        other => panic!("expected a NoPreviousSnapshot refusal, got {other:?}"),
    }
    assert_eq!(
        pointer_bytes(&root).expect("the pointer exists"),
        pointer_before
    );
    assert_eq!(
        log_bytes(&root),
        log_before,
        "a refused rollback records nothing"
    );

    // A second activation gives the chain a previous snapshot.
    let (commit_b, id_b) = activatable(
        &mut store,
        &root.join(JOURNAL_DIR_NAME),
        Some(id_a.clone()),
        "beta",
        2,
    );
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    let forward = activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
            &store,
        )
        .expect("the second activation succeeds");
    assert_eq!(forward.generation, 2);
    assert_eq!(forward.activated.commit, id_b);
    let previous = forward.rollback_target.expect("a rollback target exists");
    assert_eq!(previous.snapshot, *snapshot_a.id());
    assert_eq!(previous.commit, id_a);

    // The rollback: atomic, journaled, and it swaps rather than shortens.
    let back = activation.rollback(&store).expect("the rollback succeeds");
    assert_eq!(back.kind, ActivationKind::Rollback);
    assert_eq!(back.generation, 3);
    assert_eq!(back.activated.snapshot, *snapshot_a.id());
    assert_eq!(back.activated.commit, id_a);
    let forward_again = back.rollback_target.expect("the chain ping-pongs");
    assert_eq!(forward_again.snapshot, *snapshot_b.id());
    assert_eq!(forward_again.commit, id_b);
    assert!(matches!(back.intent, RecordOutcome::Appended { .. }));
    assert!(matches!(back.completion, RecordOutcome::Appended { .. }));
    assert_clean_no_tmp(&root);

    // The reader agrees, and the pointer is one whole file.
    let active = activation.read_active().expect("readable");
    assert_eq!(active.generation(), 3);
    assert_eq!(active.snapshot().id(), snapshot_a.id());
    assert_eq!(
        active.rollback_target().expect("a target").snapshot,
        *snapshot_b.id()
    );
    let bytes = pointer_bytes(&root).expect("the pointer exists");
    let parsed: PointerFile = serde_json::from_slice(&bytes).expect("whole json");
    assert_eq!(parsed.generation, 3);
    assert_eq!(parsed.active.snapshot, snapshot_a.id().as_str());
    assert_eq!(
        parsed.previous.expect("a previous entry").snapshot,
        snapshot_b.id().as_str()
    );

    // The journal holds the whole order as new records, and no disagreement.
    let audit = activation.audit(&store).expect("the audit reads");
    assert_eq!(audit.traces.len(), 6, "three operations, two records each");
    let order: Vec<&str> = audit
        .traces
        .iter()
        .map(|trace| trace.snapshot.as_str())
        .collect();
    let a = snapshot_a.id().as_str();
    let b = snapshot_b.id().as_str();
    assert_eq!(
        order,
        vec![a, a, b, b, a, a],
        "activate a, activate b, roll back to a"
    );
    assert!(audit.pending.is_none());
    assert!(audit.disagreement.is_none());
    assert_eq!(
        audit.journal_records, 8,
        "two training events and six activation records"
    );
}

#[test]
fn a_rollback_whose_target_no_longer_verifies_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, id_a) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("activated");
    let (commit_b, _) = activatable(
        &mut store,
        &root.join(JOURNAL_DIR_NAME),
        Some(id_a.clone()),
        "beta",
        2,
    );
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
            &store,
        )
        .expect("activated");

    // The rollback target's bytes are damaged. Rolling back to it would install
    // something that does not verify, so it is refused and the active snapshot
    // does not move.
    edit_snapshot_file(&snapshot_file(&root, snapshot_a.id()), |value| {
        value["commit"]["algorithm_versions"][0][1] = serde_json::json!("tampered")
    });
    let pointer_before = pointer_bytes(&root).expect("the pointer exists");
    let log_before = log_bytes(&root);
    assert!(matches!(
        activation.rollback(&store),
        Err(ActivationError::CommitRejected { .. })
    ));
    assert_eq!(
        pointer_bytes(&root).expect("the pointer exists"),
        pointer_before
    );
    assert_eq!(log_bytes(&root), log_before);
    let active = activation
        .read_active()
        .expect("b is still active and verifies");
    assert_eq!(active.snapshot().id(), snapshot_b.id());
}

#[test]
fn a_pointer_that_names_a_missing_snapshot_is_refused_rather_than_repointed() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, id_a) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("activated");
    let (commit_b, _) = activatable(
        &mut store,
        &root.join(JOURNAL_DIR_NAME),
        Some(id_a.clone()),
        "beta",
        2,
    );
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");

    // The active snapshot's file is removed underneath the pointer.
    fs::remove_file(snapshot_file(&root, snapshot_a.id())).expect("removable");
    assert!(matches!(
        activation.read_active(),
        Err(ActivationError::SnapshotNotFound { .. })
    ));
    let pointer_before = pointer_bytes(&root).expect("the pointer exists");
    assert!(matches!(
        activation.activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
            &store
        ),
        Err(ActivationError::SnapshotNotFound { .. })
    ));
    assert_eq!(
        pointer_bytes(&root).expect("the pointer exists"),
        pointer_before,
        "an activation must not paper over a missing active snapshot"
    );
}
