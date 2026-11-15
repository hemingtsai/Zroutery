use std::fs;

use zroutery_core::ml::activation::{
    activation_applied_event_id, ActivationEntry, ActivationError, ActivationKind, ActivationStage,
    ActivationStore, PointerFile, SnapshotId, ACTIVATION_POINTER_NAME,
    ACTIVATION_POINTER_SCHEMA_VERSION, JOURNAL_DIR_NAME, PLAN_EVENT_PREFIX, SNAPSHOT_ID_PREFIX,
};
use zroutery_core::ml::journal::RecordOutcome;
use zroutery_core::ml::model_identity::{ModelCommit, ModelEnsemble, ModelId, ModelStore};

use super::support::*;

// ---------------------------------------------------------------------------
// Gate 2: atomic activation
// ---------------------------------------------------------------------------

#[test]
fn the_first_activation_flips_one_file_and_appends_two_new_records() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, commit_id) =
        activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the snapshot is written");

    // Before: a named "nothing is active" state, never a cold start.
    assert!(activation.read_pointer().expect("no pointer yet").is_none());
    assert!(matches!(
        activation.read_active(),
        Err(ActivationError::NoActiveSnapshot { .. })
    ));
    assert!(matches!(
        activation.rollback(&store),
        Err(ActivationError::NoActiveSnapshot { .. })
    ));
    assert!(pointer_bytes(&root).is_none(), "no pointer file exists yet");

    // The flip.
    let outcome = activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
            &store,
        )
        .expect("the first activation succeeds");
    assert_eq!(outcome.kind, ActivationKind::Activate);
    assert_eq!(outcome.generation, 1);
    assert_eq!(outcome.activated.snapshot, *snapshot.id());
    assert_eq!(outcome.activated.commit, commit_id);
    assert_eq!(outcome.rollback_target, None);
    assert!(matches!(outcome.intent, RecordOutcome::Appended { .. }));
    assert!(matches!(outcome.completion, RecordOutcome::Appended { .. }));

    // After: exactly one file changed, it is whole, and it verifies.
    let bytes = pointer_bytes(&root).expect("the pointer exists");
    let parsed: PointerFile = serde_json::from_slice(&bytes).expect("the pointer is whole json");
    assert_eq!(parsed.schema_version, ACTIVATION_POINTER_SCHEMA_VERSION);
    assert_eq!(parsed.generation, 1);
    assert_eq!(parsed.active.snapshot, snapshot.id().as_str());
    assert_eq!(parsed.active.commit, commit_id.as_str());
    assert!(parsed.previous.is_none());
    assert_clean_no_tmp(&root);

    // A reader sees a complete, verified active snapshot with a rollback target
    // of "none" rather than a missing one.
    let active = activation
        .read_active()
        .expect("the active snapshot verifies");
    assert_eq!(active.generation(), 1);
    assert_eq!(active.snapshot().id(), snapshot.id());
    assert_eq!(active.commit().commit_id, commit_id);
    assert!(active.rollback_target().is_none());

    // Both records are in the journal, naming the snapshot and the commit.
    let audit = activation
        .audit(&store)
        .expect("the audit reads the journal");
    assert_eq!(
        audit.journal_records, 3,
        "one training event and two activation records"
    );
    let traces: Vec<&zroutery_core::ml::activation::ActivationTrace> =
        audit.traces.iter().collect();
    assert_eq!(traces.len(), 2);
    assert_eq!(traces[0].stage, ActivationStage::Planned);
    assert_eq!(traces[1].stage, ActivationStage::Applied);
    assert!(traces.iter().all(|trace| trace.snapshot == *snapshot.id()));
    assert!(traces.iter().all(|trace| trace.commit == commit_id));
    assert!(
        traces[1].sequence > traces[0].sequence,
        "the order is the sequence"
    );
    assert!(audit.pending.is_none());
    assert!(audit.disagreement.is_none());
    assert!(traces[0].event_id.starts_with(PLAN_EVENT_PREFIX));
    assert_eq!(
        traces[1].event_id,
        activation_applied_event_id(&traces[0].event_id),
        "the completion is derived from the intent, so both paths agree on its identity"
    );
}

#[test]
fn a_verification_failure_leaves_the_previous_activation_exactly_as_it_was() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("the first activation succeeds");

    // The fixtures for the three refusals are prepared *before* the capture,
    // because preparing one legitimately records a training event, and that
    // append is not what this test is measuring.
    let orphan = ModelCommit::new(
        ModelId::new(MODEL),
        ModelEnsemble::new().save_all(),
        None,
        0,
    );
    let orphan_snapshot = activation.write_snapshot(&orphan).expect("written");
    let (commit_b, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "beta", 2);
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");

    let pointer_before = pointer_bytes(&root).expect("the pointer exists");
    let log_before = log_bytes(&root);

    // (a) A snapshot that is not there.
    let absent = SnapshotId::new(format!("{SNAPSHOT_ID_PREFIX}0123456789abcdef"));
    assert!(matches!(
        activation.activate(
            &zroutery_core::ml::activation::ActivationRequest::new(absent.clone()),
            &store
        ),
        Err(ActivationError::SnapshotNotFound { .. })
    ));

    // (b) A commit the store's lineage does not vouch for: a perfectly verified
    // commit that was never committed to the store.
    match activation.activate(
        &zroutery_core::ml::activation::ActivationRequest::new(orphan_snapshot.id().clone()),
        &store,
    ) {
        Err(ActivationError::LineageRejected { commit, reason }) => {
            assert_eq!(commit, orphan.commit_id);
            assert!(!reason.is_empty());
        }
        other => panic!("expected a LineageRejected refusal, got {other:?}"),
    }

    // (c) The currently active snapshot has been corrupted underneath the
    // pointer. An activation must not paper over that.
    edit_snapshot_file(&snapshot_file(&root, snapshot_a.id()), |value| {
        value["commit"]["algorithm_versions"][0][1] = serde_json::json!("tampered")
    });
    assert!(matches!(
        activation.activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
            &store
        ),
        Err(ActivationError::CommitRejected { .. })
    ));

    // In every case: the pointer and the journal are byte-for-byte what they
    // were, and the active snapshot is still the one the pointer names.
    assert_eq!(
        pointer_bytes(&root).expect("the pointer exists"),
        pointer_before
    );
    assert_eq!(log_bytes(&root), log_before, "no record was appended");
    // The pointer still names the snapshot it always named, and the corruption
    // underneath it is reported rather than papered over: a reader is told the
    // active snapshot does not verify instead of being handed a default.
    let pointer = activation
        .read_pointer()
        .expect("the pointer itself still verifies")
        .expect("present");
    assert_eq!(pointer.active.snapshot, *snapshot_a.id());
    assert_eq!(pointer.generation, 1);
    assert!(matches!(
        activation.read_active(),
        Err(ActivationError::CommitRejected { .. })
    ));
    assert_clean_no_tmp(&root);
}

#[test]
fn a_reader_never_observes_a_partial_or_missing_pointer() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
            &store,
        )
        .expect("activated");

    // State 1: the pointer is there and verifies.
    let pointer = activation.read_pointer().expect("readable");
    assert!(pointer.is_some());
    // State 2: the pointer does not exist, which is a *named* state.
    fs::remove_file(root.join(ACTIVATION_POINTER_NAME)).expect("removable");
    assert!(activation.read_pointer().expect("readable").is_none());
    assert!(matches!(
        activation.read_active(),
        Err(ActivationError::NoActiveSnapshot { .. })
    ));
    // State 3: the pointer exists and does not verify, which is a *refusal*.
    fs::write(root.join(ACTIVATION_POINTER_NAME), b"{ \"generation\": 1").expect("writable");
    assert!(matches!(
        activation.read_pointer(),
        Err(ActivationError::PointerCorrupt { .. })
    ));
    assert!(matches!(
        activation.read_active(),
        Err(ActivationError::PointerCorrupt { .. })
    ));
    // The refusal is not a repair and not a default.
    assert_eq!(
        fs::read(root.join(ACTIVATION_POINTER_NAME)).expect("readable"),
        b"{ \"generation\": 1"
    );

    // A directory where the pointer belongs is corruption, not absence: on
    // Windows a read of a directory is an error, so the distinction is made
    // before the read rather than after it.
    fs::remove_file(root.join(ACTIVATION_POINTER_NAME)).expect("removable");
    fs::create_dir_all(root.join(ACTIVATION_POINTER_NAME)).expect("writable");
    match activation.read_pointer() {
        Err(ActivationError::PointerCorrupt { reason, .. }) => {
            assert!(
                reason.contains("directory"),
                "the reason says why: {reason}"
            )
        }
        other => panic!("expected a PointerCorrupt refusal, got {other:?}"),
    }
    drop(activation);
    assert!(matches!(
        ActivationStore::open(&root),
        Err(ActivationError::PointerCorrupt { .. })
    ));
}

#[test]
fn a_pointer_whose_checksum_does_not_match_its_fields_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, commit_id) =
        activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
            &store,
        )
        .expect("activated");

    // A hand-edited generation: the identity-bearing field changes but the
    // checksum does not.
    let path = root.join(ACTIVATION_POINTER_NAME);
    let bytes = fs::read(&path).expect("readable");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["generation"] = serde_json::json!(9);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&value).expect("serializable"),
    )
    .expect("writable");
    match activation.read_pointer() {
        Err(ActivationError::PointerCorrupt { reason, .. }) => {
            assert!(
                reason.contains("checksum"),
                "the reason names the check: {reason}"
            )
        }
        other => panic!("expected a PointerCorrupt refusal, got {other:?}"),
    }
    assert!(activation.read_active().is_err(), "a reader refuses it too");

    // A hand-written pointer that carries a correct checksum for different
    // fields is accepted as a pointer, and then fails because the snapshot it
    // names disagrees with it: the two checks are independent on purpose.
    let tampered = ActivationEntry {
        snapshot: SnapshotId::new(format!("{SNAPSHOT_ID_PREFIX}0000000000000000")),
        commit: commit_id,
        model_id: ModelId::new(MODEL),
        activated_at: 1,
    };
    write_pointer_file(&root, 9, &tampered, None);
    let pointer = activation
        .read_pointer()
        .expect("a re-sealed pointer is a pointer");
    assert_eq!(pointer.expect("present").active.snapshot, tampered.snapshot);
    assert!(matches!(
        activation.read_active(),
        Err(ActivationError::SnapshotNotFound { .. })
    ));
}

#[test]
fn an_already_active_snapshot_is_refused_rather_than_looping_the_chain() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");
    let request = zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone());
    activation.activate(&request, &store).expect("activated");

    let pointer_before = pointer_bytes(&root).expect("the pointer exists");
    let log_before = log_bytes(&root);
    match activation.activate(&request, &store) {
        Err(ActivationError::AlreadyActive { snapshot: name }) => {
            assert_eq!(name, snapshot.id().as_str())
        }
        other => panic!("expected an AlreadyActive refusal, got {other:?}"),
    }
    assert_eq!(
        pointer_bytes(&root).expect("the pointer exists"),
        pointer_before
    );
    assert_eq!(log_bytes(&root), log_before);
    assert_eq!(
        activation
            .read_pointer()
            .expect("readable")
            .expect("present")
            .generation,
        1
    );
}

#[test]
fn expect_generation_refuses_a_belief_the_file_disagrees_with() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");

    // Generation zero is the file-derived state before anything is activated.
    activation
        .expect_generation(0)
        .expect("zero agrees with the file");
    assert!(matches!(
        activation.expect_generation(1),
        Err(ActivationError::PointerCorrupt { reason, .. }) if reason.contains("generation 1")
    ));
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
            &store,
        )
        .expect("activated");
    activation
        .expect_generation(1)
        .expect("the file agrees now");
    assert!(
        activation.expect_generation(0).is_err(),
        "a stale belief is refused"
    );
}

/// The store's mutating transitions take `&self`, so two in-process callers can
/// share one handle. The read-modify-write of the pointer must be serialised:
/// without it, both callers can read the same generation and both commit it,
/// losing a flip and reporting two winners for one generation.
#[test]
fn two_competing_transitions_cannot_consume_one_generation() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = Arc::new(ActivationStore::open(&root).expect("the store opens"));
    let snapshot = activation.write_snapshot(&commit).expect("written");
    let store = Arc::new(store);

    // Every caller asks for the *same* transition, released from the same
    // barrier, so the only thing that can order them is the store itself.
    let callers = 4usize;
    let barrier = Arc::new(Barrier::new(callers));
    let mut handles = Vec::new();
    for _ in 0..callers {
        let activation = Arc::clone(&activation);
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let target = snapshot.id().clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            activation.activate(
                &zroutery_core::ml::activation::ActivationRequest::new(target),
                &store,
            )
        }));
    }

    let mut winners = Vec::new();
    let mut losers = 0usize;
    for handle in handles {
        match handle.join().expect("no caller panicked") {
            Ok(outcome) => winners.push(outcome),
            Err(ActivationError::AlreadyActive { snapshot: name }) => {
                assert_eq!(name, snapshot.id().as_str());
                losers += 1;
            }
            other => {
                panic!("a competing transition must be refused as already active, got {other:?}")
            }
        }
    }

    // One winner and a visible refusal for every loser: generation 1 is
    // consumed exactly once, and nobody who lost it can report success.
    assert_eq!(winners.len(), 1, "exactly one caller consumes generation 1");
    assert_eq!(winners[0].generation, 1);
    assert_eq!(losers, callers - 1);
    assert_eq!(
        activation
            .read_pointer()
            .expect("readable")
            .expect("present")
            .generation,
        1,
        "the pointer advanced exactly once"
    );
    let audit = activation.audit(&store).expect("the audit reads");
    assert_eq!(
        audit.traces.len(),
        2,
        "exactly one plan and one completion were recorded"
    );
    assert!(audit.pending.is_none());
    assert!(audit.disagreement.is_none());
    assert_clean_no_tmp(&root);
}
