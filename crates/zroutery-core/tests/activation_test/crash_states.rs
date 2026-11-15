use std::collections::BTreeSet;
use std::fs;

use zroutery_core::ml::activation::{
    ActivationError, ActivationStore, ACTIVATION_POINTER_NAME, JOURNAL_DIR_NAME,
};
use zroutery_core::ml::journal::RecordOutcome;
use zroutery_core::ml::model_identity::{ModelCommit, ModelId, ModelStore};

use super::support::*;

// ---------------------------------------------------------------------------
// Gate 2, part two: the crash states the two-phase order makes nameable
// ---------------------------------------------------------------------------

#[test]
fn an_interrupted_activation_is_reported_as_pending_and_never_guessed_at() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);
    let (commit_a, _) = activatable(&mut store, &journal_dir, None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("activated");

    let (commit_b, id_b) = activatable(&mut store, &journal_dir, None, "beta", 2);
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    let from = activation
        .read_pointer()
        .expect("readable")
        .expect("present")
        .active;
    let to = entry_for(&commit_b);

    // A process that appended the intent and then died: the pointer still names
    // the old snapshot, and the journal says an activation was attempted.
    let intent_id = inject_intent(&store, &journal_dir, 2, Some(&from), &to);
    let log_after_intent = log_bytes(&root);

    let audit = activation.audit(&store).expect("the audit reads");
    let pending = audit
        .pending
        .clone()
        .expect("the intent is reported as pending");
    assert_eq!(pending.event_id, intent_id);
    assert_eq!(pending.snapshot, *snapshot_b.id());
    assert_eq!(pending.commit, id_b);
    assert!(
        audit.disagreement.is_none(),
        "the pointer still names what the journal last completed"
    );
    let active = activation
        .read_active()
        .expect("the old snapshot is still active");
    assert_eq!(
        active.snapshot().id(),
        snapshot_a.id(),
        "an interrupted activation leaves the previous snapshot active"
    );

    // Completing it is refused while the pointer disagrees: the completion is a
    // claim about what is active, not a way to activate.
    match activation.complete_pending_activation(&store) {
        Err(ActivationError::PointerDisagrees {
            pointer,
            pending: about,
        }) => {
            assert_eq!(pointer, snapshot_a.id().as_str());
            assert_eq!(about, snapshot_b.id().as_str());
        }
        other => panic!("expected a PointerDisagrees refusal, got {other:?}"),
    }
    assert_eq!(
        log_bytes(&root),
        log_after_intent,
        "a refused completion writes no frame"
    );

    // Now stage the other crash: the flip landed, the completion did not. The
    // pointer is written from the documented wire form with a re-derived
    // checksum, which is exactly what the crashed process would have left.
    write_pointer_file(&root, 2, &to, Some(&from));
    let audit = activation.audit(&store).expect("the audit reads");
    assert!(audit.pending.is_some(), "still pending");
    // The pointer is compared with the last *completed* activation, so here it
    // reads as ahead: the flip landed and the journal's newest completed record
    // is still the old one. Both facts are reported, which is what makes the
    // state readable rather than merely survivable.
    match audit.disagreement {
        Some(zroutery_core::ml::activation::PointerDisagreement::PointerNamesOther {
            pointer,
            last_completed,
        }) => {
            assert_eq!(
                pointer,
                *snapshot_b.id(),
                "the pointer names the new snapshot"
            );
            assert_eq!(
                last_completed,
                *snapshot_a.id(),
                "the journal last completed the old one"
            );
        }
        other => panic!("expected a PointerNamesOther disagreement, got {other:?}"),
    }
    assert_eq!(
        audit.traces.len(),
        3,
        "two for the first activation, one intent"
    );
    let active = activation
        .read_active()
        .expect("the new snapshot is active and verifies");
    assert_eq!(active.snapshot().id(), snapshot_b.id());
    assert_eq!(active.generation(), 2);

    let outcome = activation
        .complete_pending_activation(&store)
        .expect("the completion is appended");
    assert!(matches!(outcome, RecordOutcome::Appended { .. }));

    let audit = activation.audit(&store).expect("the audit reads");
    assert!(audit.pending.is_none(), "the record is closed");
    assert!(
        audit.disagreement.is_none(),
        "the pointer and the journal now agree"
    );
    assert_eq!(
        audit.traces.len(),
        4,
        "two pairs: the first activation and the one just closed"
    );
    let log_final = log_bytes(&root);
    assert!(
        log_final.starts_with(&log_after_intent),
        "closing the record appends; it never rewrites the intent"
    );
    let active = activation.read_active().expect("readable");
    assert_eq!(active.snapshot().id(), snapshot_b.id());

    // With nothing pending, asking again is a refusal rather than an invented
    // no-op record.
    match activation.complete_pending_activation(&store) {
        Err(ActivationError::NoPendingActivation { .. }) => {}
        other => panic!("expected a NoPendingActivation refusal, got {other:?}"),
    }
    assert_eq!(log_bytes(&root), log_final);
}

#[test]
fn a_retry_after_an_interrupted_activation_is_idempotent_not_a_last_write_wins() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);
    let (commit_a, _) = activatable(&mut store, &journal_dir, None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("activated");

    let (commit_b, id_b) = activatable(&mut store, &journal_dir, None, "beta", 2);
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    let from = activation
        .read_pointer()
        .expect("readable")
        .expect("present")
        .active;
    let to = entry_for(&commit_b);

    // The interrupted attempt left its intent record and nothing else.
    inject_intent(&store, &journal_dir, 2, Some(&from), &to);
    let log_after_intent = log_bytes(&root);

    // Retrying derives the same intent id from the same on-disk facts, so the
    // journal reports a no-op instead of a second record or a conflict.
    let outcome = activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
            &store,
        )
        .expect("the retry succeeds");
    match outcome.intent {
        RecordOutcome::Duplicate { sequence } => assert!(sequence > 0),
        other => panic!("expected a reported duplicate, got {other:?}"),
    }
    assert!(matches!(outcome.completion, RecordOutcome::Appended { .. }));
    assert_eq!(outcome.generation, 2);
    assert_eq!(outcome.activated.commit, id_b);

    let log_after_retry = log_bytes(&root);
    assert!(
        log_after_retry.starts_with(&log_after_intent),
        "the intent record on disk is byte-identical: a retry is not a rewrite"
    );
    let audit = activation.audit(&store).expect("the audit reads");
    assert!(audit.pending.is_none());
    assert!(audit.disagreement.is_none());
    assert_eq!(audit.traces.len(), 4);
    let ids: BTreeSet<&str> = audit.traces.iter().map(|t| t.event_id.as_str()).collect();
    assert_eq!(ids.len(), 4, "no intent id was duplicated in the ledger");
}

#[test]
fn a_lost_rename_after_a_first_activation_is_detected_not_silently_accepted() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);
    let (commit, commit_id) = activatable(&mut store, &journal_dir, None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
            &store,
        )
        .expect("activated");

    // The journal is the redundant witness outside the pointer file. If the
    // rename's directory entry were not durable, the pointer would be gone while
    // the journal still records a completed activation — and that is a reported
    // disagreement, not a silent regression to "never activated".
    drop(activation);
    fs::remove_file(root.join(ACTIVATION_POINTER_NAME)).expect("removable");
    let reopened = ActivationStore::open(&root).expect("an absent pointer is not corruption");
    assert!(reopened.read_pointer().expect("readable").is_none());
    let audit = reopened.audit(&store).expect("the audit reads");
    match audit.disagreement {
        Some(zroutery_core::ml::activation::PointerDisagreement::PointerAbsent {
            last_completed,
            commit,
        }) => {
            assert_eq!(last_completed, *snapshot.id());
            assert_eq!(commit, commit_id);
        }
        other => panic!("expected a PointerAbsent disagreement, got {other:?}"),
    }
    // The disagreement is reported, not repaired: the pointer is still absent.
    assert!(reopened.read_pointer().expect("readable").is_none());
    assert!(matches!(
        reopened.read_active(),
        Err(ActivationError::NoActiveSnapshot { .. })
    ));
}

#[test]
fn a_snapshot_survives_the_disk_bit_exactly_and_so_does_plain_json_now() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, commit_id) =
        activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 2);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("a verified commit is writable as a snapshot");

    // The reason this module HAS its own wire form, restated because it changed.
    //
    // It used to be necessity. `serde_json` was used without its
    // `float_roundtrip` feature, so its parsing was not correctly rounded, a
    // plain JSON encoding of the commit came back with `f64` parameters one or
    // two ULP different, the accepted checksum hashes `to_bits`, and such a
    // checkpoint no longer verified: a snapshot stored that way refused to load
    // itself. That was measured, not assumed, and it was E-097.
    //
    // `float_roundtrip` is now enabled workspace-wide, so that is no longer true
    // and the assertion below has flipped with it. This test is kept, and kept
    // honest in both directions: it now pins that the transport is exact AND
    // that this module's own form is still exact, because the second is no
    // longer implied by the first. The custom form is now redundancy rather
    // than necessity, and saying otherwise would overstate what it buys.
    let plain = serde_json::to_vec(&commit).expect("serializable");
    let round_tripped: ModelCommit =
        serde_json::from_slice(&plain).expect("the accepted type still deserializes");
    assert_eq!(
        round_tripped.commit_id, commit_id,
        "the identity claim is copied verbatim"
    );
    assert_eq!(
        round_tripped.checkpoint.content_hash(),
        commit.checkpoint.content_hash(),
        "the parameters survive a plain JSON round trip now that parsing is \
         correctly rounded; this assertion FAILED before `float_roundtrip` was \
         enabled, and its failure is what proved the hazard was real"
    );
    assert!(
        round_tripped.verify(),
        "and a plain JSON round trip no longer produces a checkpoint that \
         refuses to load itself"
    );

    // This module's form does survive, which is the gate: the name, the identity
    // and every parameter are exactly what was written.
    let loaded = activation
        .read_snapshot(snapshot.id())
        .expect("the stored snapshot verifies");
    assert_eq!(loaded.commit_id(), &commit_id);
    assert_eq!(
        loaded.commit().checkpoint.content_hash(),
        commit.checkpoint.content_hash(),
        "the checkpoint is bit-identical to the one that was written"
    );
    assert_eq!(
        loaded.commit().checkpoint.success.parameters,
        commit.checkpoint.success.parameters,
        "every parameter is bit-identical"
    );
    assert!(loaded.commit().verify());
    assert!(
        zroutery_core::ml::model_identity::ReplayEngine::verify_checkpoint_integrity(
            &loaded.commit().checkpoint
        )
    );
    // The stored form is a hex encoding of the same bits, not a second truth.
    let stored: zroutery_core::ml::activation::SnapshotFile =
        serde_json::from_slice(&fs::read(snapshot_file(&root, snapshot.id())).expect("readable"))
            .expect("the stored envelope parses");
    assert_eq!(
        stored.commit.checkpoint.success.parameters.len(),
        commit.checkpoint.success.parameters.len()
    );
    for (text, parameter) in stored
        .commit
        .checkpoint
        .success
        .parameters
        .iter()
        .zip(commit.checkpoint.success.parameters.iter())
    {
        assert_eq!(text, &format!("{:016x}", parameter.to_bits()));
    }
    assert_eq!(stored.commit.commit_id, commit_id.as_str());
}

#[test]
fn a_stored_parameter_that_is_not_raw_bits_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");
    let path = snapshot_file(&root, snapshot.id());

    // A decimal parameter, which is what a plain-JSON writer would have left.
    edit_snapshot_file(&path, |value| {
        value["commit"]["checkpoint"]["success"]["parameters"][0] = serde_json::json!("0.5");
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::SnapshotUnreadable { reason, .. }) => {
            assert!(
                reason.contains("hex digits"),
                "the reason says what is wrong: {reason}"
            )
        }
        other => panic!("expected a SnapshotUnreadable refusal, got {other:?}"),
    }

    // An uppercase hex digit is equally refused: the form is exact, because a
    // loader that accepted either spelling would have two names for one value.
    edit_snapshot_file(&path, |value| {
        value["commit"]["checkpoint"]["success"]["parameters"][0] =
            serde_json::json!("0000000000000000");
    });
    assert!(activation.read_snapshot(snapshot.id()).is_err());
    edit_snapshot_file(&path, |value| {
        value["commit"]["checkpoint"]["success"]["parameters"][0] =
            serde_json::json!("3FF0000000000000");
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::SnapshotUnreadable { reason, .. }) => {
            assert!(reason.contains("hex digits"), "{reason}")
        }
        other => panic!("expected a SnapshotUnreadable refusal, got {other:?}"),
    }
}
