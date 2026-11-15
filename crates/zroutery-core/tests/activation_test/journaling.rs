use std::collections::BTreeSet;
use std::fs;

use zroutery_core::ml::activation::{
    snapshot_id_for, ActivationError, ActivationStore, JOURNAL_DIR_NAME, PLAN_EVENT_PREFIX,
};
use zroutery_core::ml::journal::{CanonicalEvent, JournalError, JournalMode, LearningJournal};
use zroutery_core::ml::model_identity::{
    LearningEvent, ModelCommit, ModelEnsemble, ModelId, ModelStore,
};

use super::support::*;

// ---------------------------------------------------------------------------
// Gate 5: journaled, as new records, with the journal's refusals propagated
// ---------------------------------------------------------------------------

#[test]
fn an_activation_appends_new_records_and_never_rewrites_the_producing_one() {
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
    let log_after_first = log_bytes(&root);

    // The producing training record is intact and unchanged: the activation
    // recorded *beside* it rather than editing it in place.
    let journal =
        LearningJournal::open(&root.join(JOURNAL_DIR_NAME), JournalMode::Read).expect("readable");
    let records = journal.read_records(&store).expect("re-verified");
    let producing = &records[0];
    assert_eq!(producing.event.event_id, "train-alpha");
    assert_eq!(producing.event.result_commit.as_ref(), Some(&id_a));
    assert!(producing.event.parent_commit.is_none());
    assert!(
        !producing.is_degraded(),
        "the canonical evidence is retained"
    );
    assert_eq!(producing.canonical_samples().map(<[_]>::len), Some(1));
    drop(journal);

    // A second activation and a rollback only append.
    let (commit_b, id_b) = activatable(
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
    activation.rollback(&store).expect("rolled back");

    let log_final = log_bytes(&root);
    assert!(
        log_final.starts_with(&log_after_first),
        "the journal is append-only: earlier frames are byte-identical"
    );
    assert!(log_final.len() > log_after_first.len());

    // Every record has a distinct id, and the activation records are the
    // accepted canonical form rather than a degraded projection.
    let journal =
        LearningJournal::open(&root.join(JOURNAL_DIR_NAME), JournalMode::Read).expect("readable");
    let records = journal.read_records(&store).expect("re-verified");
    // Released before the audit, which needs the same single-writer lock.
    drop(journal);
    let ids: BTreeSet<&str> = records
        .iter()
        .map(|record| record.event.event_id.as_str())
        .collect();
    assert_eq!(ids.len(), records.len(), "no id was reused or rewritten");
    assert!(records.iter().all(|record| !record.is_degraded()));
    for record in &records {
        if record.event.event_id.starts_with(PLAN_EVENT_PREFIX) {
            let source = record.event.source.as_deref().expect("a source");
            assert!(source.starts_with("activation:"), "{source}");
            // The record names the snapshot it activates and the commit.
            assert!(
                source.contains(snapshot_id_for(&commit_b).as_str())
                    || source.contains(snapshot_id_for(&commit_a).as_str())
            );
            let commit = record
                .event
                .result_commit
                .clone()
                .expect("an activation record names a commit");
            assert!(commit == id_a || commit == id_b);
        }
    }
    // And the activation record replays to the same lineage claims the producing
    // event made: same parent, same result, same samples.
    let audit = activation.audit(&store).expect("the audit reads");
    for trace in audit.traces.iter().filter(|t| t.commit == id_b) {
        assert_eq!(trace.parent_commit, Some(id_a.clone()));
    }
    let activation_record = records
        .iter()
        .find(|record| record.event.event_id.starts_with(PLAN_EVENT_PREFIX))
        .expect("an activation record exists");
    assert_eq!(activation_record.event.parent_commit, None);
    assert_eq!(activation_record.event.result_commit.as_ref(), Some(&id_a));
    assert_eq!(
        activation_record.canonical_samples().map(<[_]>::len),
        Some(1)
    );
}

#[test]
fn a_journal_refusal_propagates_and_leaves_the_previous_activation_in_place() {
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

    let pointer_before = pointer_bytes(&root).expect("the pointer exists");
    let log_before = log_bytes(&root);

    // The journal is held by another handle, so every append in it refuses. The
    // fixture for the attempt is prepared first, because the refusal under test
    // is the activation's, not the fixture's.
    let (commit_b, _) = trained_commit(&mut store, Some(id_a), "beta", 2);
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    fs::write(
        root.join(JOURNAL_DIR_NAME).join("journal.lock"),
        "pid 1 opened it",
    )
    .expect("writable");
    match activation.activate(
        &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
        &store,
    ) {
        Err(ActivationError::Journal {
            context,
            cause,
            pointer,
        }) => {
            assert_eq!(
                context,
                zroutery_core::ml::activation::JournalContext::Intent
            );
            assert!(
                matches!(cause, JournalError::JournalLocked { .. }),
                "the accepted refusal is carried, not replaced: {cause}"
            );
            assert_eq!(
                pointer,
                zroutery_core::ml::activation::PointerState::Unchanged,
                "a refusal before the flip cannot have moved the pointer"
            );
        }
        other => panic!("expected a journal refusal, got {other:?}"),
    }
    assert_eq!(
        pointer_bytes(&root).expect("the pointer exists"),
        pointer_before
    );
    assert_eq!(
        log_bytes(&root),
        log_before,
        "a refused intent writes no frame"
    );
    let active = activation.read_active().expect("still active");
    assert_eq!(active.snapshot().id(), snapshot_a.id());
    assert_clean_no_tmp(&root);
}

#[test]
fn a_commit_with_no_provenance_is_refused_rather_than_given_invented_evidence() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);

    // A verified commit in the store, with a snapshot on disk, and *nothing* in
    // the journal that says how it was produced. This is the cold-start shape:
    // it cannot be activated, because the accepted event validator requires a
    // sample and inventing one would be fabricated feedback.
    let (cold, cold_id) = trained_commit(&mut store, None, "cold", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let cold_snapshot = activation.write_snapshot(&cold).expect("written");

    match activation.activate(
        &zroutery_core::ml::activation::ActivationRequest::new(cold_snapshot.id().clone()),
        &store,
    ) {
        Err(ActivationError::ProvenanceMissing { commit }) => assert_eq!(commit, cold_id),
        other => panic!("expected a ProvenanceMissing refusal, got {other:?}"),
    }
    assert!(
        pointer_bytes(&root).is_none(),
        "no pointer is written for a refused activation, so there is never a state in which \
         nothing is active *and* something claims to be"
    );
    // The mechanism had to open the journal to look for the evidence, so an
    // empty one exists; what matters is that it holds nothing.
    let journal = LearningJournal::open(&journal_dir, JournalMode::Read).expect("readable");
    assert!(
        journal.read_records(&store).expect("readable").is_empty(),
        "no frame was written for a refused activation"
    );

    // A truly cold-start ensemble is refused earlier, by lineage: it is not a
    // commit the store vouches for.
    let bare = ModelCommit::new(
        ModelId::new(MODEL),
        ModelEnsemble::new().save_all(),
        None,
        0,
    );
    let bare_snapshot = activation.write_snapshot(&bare).expect("written");
    assert!(matches!(
        activation.activate(
            &zroutery_core::ml::activation::ActivationRequest::new(bare_snapshot.id().clone()),
            &store
        ),
        Err(ActivationError::LineageRejected { .. })
    ));
}

#[test]
fn degraded_evidence_is_refused_rather_than_recorded() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);

    // The producing event is recorded through the lossy legacy path, so the only
    // evidence in the journal is a degraded projection. The activation must not
    // launder it into a record that looks complete.
    let (commit, commit_id) = trained_commit(&mut store, None, "alpha", 1);
    let legacy = LearningEvent {
        event_id: "train-legacy".to_string(),
        model_id: ModelId::new(MODEL),
        samples: vec![canonical_sample("alpha-train-0", true).into_legacy()],
        parent_commit: None,
        result_commit: Some(commit_id.clone()),
        created_at: 1_700_000_000,
        source: Some("trainer".to_string()),
    };
    let mut journal =
        LearningJournal::open(&journal_dir, JournalMode::Append).expect("the journal is writable");
    journal.record_legacy(&legacy).expect("recordable");
    drop(journal);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&commit).expect("written");
    match activation.activate(
        &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
        &store,
    ) {
        Err(ActivationError::ProvenanceDegraded {
            commit: id,
            event_id,
        }) => {
            assert_eq!(id, commit_id);
            assert_eq!(event_id, "train-legacy");
        }
        other => panic!("expected a ProvenanceDegraded refusal, got {other:?}"),
    }
    assert!(pointer_bytes(&root).is_none());
    let journal = LearningJournal::open(&journal_dir, JournalMode::Read).expect("readable");
    let records = journal.read_records(&store).expect("re-verified");
    assert_eq!(records.len(), 1, "the activation appended nothing");
    assert!(
        records[0].is_degraded(),
        "the training record still states its degradation on the record"
    );
    assert_eq!(records[0].event.event_id, "train-legacy");
}

/// ML-12: a record that merely *names* the target commit as its result is not
/// proof that its samples produced it. The strict accepted replay engine is the
/// authority, and a mismatch is an activation refusal, not a recorded fact.
#[test]
fn a_record_whose_samples_do_not_produce_the_commit_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);

    // A legitimate commit the store's lineage vouches for. It was trained on
    // one sample batch, but the journal record that claims it carries a
    // *different* batch — the shape a mis-filled `result_commit` produces.
    let (victim, victim_id) = trained_commit(&mut store, None, "victim", 1);
    let mut journal =
        LearningJournal::open(&journal_dir, JournalMode::Append).expect("the journal is writable");
    let mut impostor = CanonicalEvent::new(
        "train-impostor".to_string(),
        ModelId::new(MODEL),
        vec![canonical_sample("impostor-train-0", true)],
    );
    impostor.created_at = 1_700_000_000;
    impostor.parent_commit = None;
    impostor.result_commit = Some(victim_id.clone());
    impostor.source = Some("trainer".to_string());
    journal
        .record_canonical(impostor)
        .expect("the journal accepts it: only replay can tell");
    drop(journal);

    // Independent witness: the strict engine refuses exactly this record, so
    // the activation below must refuse it for the same reason.
    let records = LearningJournal::open(&journal_dir, JournalMode::Read)
        .expect("readable")
        .read_records(&store)
        .expect("re-verified");
    assert_eq!(records.len(), 1);
    assert!(
        matches!(
            zroutery_core::ml::model_identity::ReplayEngine::replay_verified(
                std::slice::from_ref(&records[0].event),
                None
            ),
            Err(zroutery_core::ml::model_identity::ReplayError::EventResultMismatch { .. })
        ),
        "the fixture must be the mismatch the fix is about"
    );
    drop(records);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation.write_snapshot(&victim).expect("written");
    let pointer_before = pointer_bytes(&root);

    match activation.activate(
        &zroutery_core::ml::activation::ActivationRequest::new(snapshot.id().clone()),
        &store,
    ) {
        Err(ActivationError::ProvenanceUnproven {
            commit,
            event_id,
            reason,
        }) => {
            assert_eq!(commit, victim_id);
            assert_eq!(event_id, "train-impostor");
            assert!(!reason.is_empty(), "the refusal says what replay found");
        }
        other => panic!("expected a ProvenanceUnproven refusal, got {other:?}"),
    }
    assert_eq!(
        pointer_bytes(&root),
        pointer_before,
        "a refused activation moves no pointer"
    );
    // Nothing was appended by the refused activation: the journal still holds
    // only the impostor's own record.
    let records = LearningJournal::open(&journal_dir, JournalMode::Read)
        .expect("readable")
        .read_records(&store)
        .expect("re-verified");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event.event_id, "train-impostor");
}

#[test]
fn a_record_claiming_to_be_an_activation_record_must_be_one() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let journal_dir = root.join(JOURNAL_DIR_NAME);
    let (_, id) = activatable(&mut store, &journal_dir, None, "alpha", 1);

    // A record whose event id claims the activation namespace but whose source
    // does not say which snapshot it is about cannot pass as one.
    let (model_id, samples, parent_commit, created_at) =
        producing_samples(&journal_dir, &store, &id);
    let mut journal =
        LearningJournal::open(&journal_dir, JournalMode::Append).expect("the journal is writable");
    journal
        .record_canonical(CanonicalEvent {
            event_id: format!("{PLAN_EVENT_PREFIX}0000000000000000"),
            model_id,
            samples,
            parent_commit,
            result_commit: Some(id.clone()),
            created_at,
            source: Some("activation:plan:not-a-snapshot-name".to_string()),
        })
        .expect("the journal accepts it: only this module knows better");
    drop(journal);

    let activation = ActivationStore::open(&root).expect("the store opens");
    match activation.audit(&store) {
        Err(ActivationError::ForgedActivationRecord { event_id, reason }) => {
            assert!(event_id.starts_with(PLAN_EVENT_PREFIX));
            assert!(
                reason.contains("well-formed"),
                "the reason says why: {reason}"
            );
        }
        other => panic!("expected a ForgedActivationRecord refusal, got {other:?}"),
    }
}
