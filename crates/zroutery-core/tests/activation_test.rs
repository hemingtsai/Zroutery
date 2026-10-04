#![cfg(feature = "ml")]

//! Node 7E-2F gate tests: immutable snapshots and atomic, journaled activation.
//!
//! Every accepted ML node in this repository is in-memory or a pure function.
//! This one writes, so each claim below is a property of bytes on disk rather
//! than of a field a handle happens to hold — and, because the mechanism must
//! be unreachable, the last gate is a structural one: a scan of the source tree
//! proving that nothing in the shipped product can name any of it.
//!
//! The gates, in the order this file asserts them:
//!
//! 1. **Verified immutable snapshot.** A snapshot is content-addressed, verifies
//!    on load through the accepted integrity and envelope checks, and one that
//!    fails any of them is refused. Writing never mutates an existing snapshot;
//!    loading never repairs one.
//! 2. **Atomic activation.** The pointer changes atomically, a reader never
//!    observes a partial or missing pointer, and a verification failure at
//!    activation time leaves the previous active snapshot byte-for-byte as it
//!    was. There is no state in which nothing is active and none in which a
//!    corrupt snapshot is.
//! 3. **Rollback.** Returning to the previous snapshot is atomic and journaled,
//!    and is refused when there is no previous snapshot.
//! 4. **Fail-closed.** Every refusal is typed and carries a reason: an
//!    unwritable location, a path that is not a snapshot, an unknown or legacy
//!    envelope, a failed integrity check, a failed lineage, a missing rollback
//!    target, and a journal that refuses. Never a panic, never a default, never
//!    a cold start.
//! 5. **Journaled.** Every activation and rollback is recorded as a *new*
//!    journal record carrying the snapshot identity and the commit, and the
//!    journal's own refusals propagate.
//! 6. **Reachability.** This module calls no live activation seam, and nothing in
//!    `server/`, `src-tauri/`, `ui/`, or `config.rs` names it — while the Tauri
//!    application still does not enable the `ml` feature.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ir::Usage;
use zroutery_core::ml::activation::{
    activation_applied_event_id, activation_plan_event_id, pointer_checksum, snapshot_id_for,
    ActivationAudit, ActivationEntry, ActivationError, ActivationKind, ActivationStage,
    ActivationStore, PointerEntryFile, PointerFile, Snapshot, SnapshotId, ACTIVATION_LOCK_NAME,
    ACTIVATION_POINTER_NAME, ACTIVATION_POINTER_SCHEMA_VERSION, ACTIVATION_POINTER_TMP_NAME,
    ACTIVATION_ROLE, ACTIVATION_SNAPSHOT_SCHEMA_VERSION, JOURNAL_DIR_NAME, PLAN_EVENT_PREFIX,
    SNAPSHOT_FILE_SUFFIX, SNAPSHOT_ID_PREFIX,
};
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::journal::{
    CanonicalEvent, JournalError, JournalMode, LearningJournal, RecordOutcome,
};
use zroutery_core::ml::model_identity::{
    CommitId, LearningEvent, ModelCommit, ModelEnsemble, ModelId, ModelStore,
};
use zroutery_core::outcome::{
    Attempt, CandidateIdentity, FailureFacts, FinalStatus, OutcomeIdentity,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const MODEL: &str = "ensemble";
const PROVIDER: &str = "openai";
const MODEL_NAME: &str = "gpt-x";

/// A temporary directory that removes itself, so even a failing assertion cannot
/// leave artifacts on the shared drive.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn store_root(root: &Path) -> PathBuf {
    root.join("activation-store")
}

fn features(seed: u16) -> RoutingFeatures {
    let mut values = [0.0f32; FEATURE_DIMENSION];
    for (index, value) in values.iter_mut().enumerate() {
        *value = ((seed as usize * 7 + index * 13) % 100) as f32 / 100.0;
    }
    RoutingFeatures {
        values,
        schema_version: FEATURE_SCHEMA_VERSION,
    }
}

/// A *canonical* sample: the lossless shape. This is the evidence an activation
/// record has to re-record, because the accepted learning event refuses an
/// event with no samples.
fn canonical_sample(suffix: &str, served: bool) -> OutcomeTrainingSample {
    let attempt = Attempt {
        attempt_id: format!("att-{suffix}"),
        candidate_model: MODEL_NAME.to_string(),
        candidate_provider: PROVIDER.to_string(),
        started_at: 1_700_000_000,
        completed_at: 1_700_000_200,
        latency_ms: 200.0,
        ttft_ms: Some(50.0),
        success: served,
        failure_class: (!served).then_some(FailureClass::ProviderUnavailable),
        failure_message: (!served).then(|| "scripted failure".to_string()),
        http_status: Some(if served { 200 } else { 503 }),
        rectified: false,
    };
    let identity = CandidateIdentity::new(MODEL_NAME.to_string(), PROVIDER.to_string());
    OutcomeTrainingSample {
        sample_id: format!("samp-{suffix}-request"),
        schema_version: FEATURE_SCHEMA_VERSION,
        timestamp: 1_700_000_000,
        streaming: true,
        dialect: "openai".to_string(),
        features: features(suffix.len() as u16 + suffix.as_bytes()[0] as u16),
        targets: Targets {
            success: served,
            latency_ms: served.then_some(200.0),
            ttft_ms: served.then_some(50.0),
            cost: Some(0.0021),
            failure_class: (!served).then_some("provider_unavailable".to_string()),
            fallback_count: 0,
        },
        provider_id: PROVIDER.to_string(),
        model_id: MODEL_NAME.to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{suffix}"),
        request_id: format!("req-{suffix}"),
        decision_id: Some(format!("dec-{suffix}")),
        response_id: Some(format!("resp-{suffix}")),
        final_status: if served {
            FinalStatus::Success
        } else {
            FinalStatus::Failed
        },
        success: served,
        identity: OutcomeIdentity {
            planned: Some(identity.clone()),
            last_attempted: Some(identity.clone()),
            served: served.then_some(identity),
        },
        scope: SampleScope::Attempt {
            index: 0,
            attempt_id: attempt.attempt_id.clone(),
        },
        attempt_id: Some(attempt.attempt_id.clone()),
        rectified: false,
        attempts: vec![attempt],
        usage: Some(Usage {
            input_tokens: 11,
            output_tokens: 22,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        }),
        estimated_cost: Some(0.002),
        actual_cost: Some(0.0021),
        terminal_error: (!served).then(|| {
            FailureFacts::new(
                FailureClass::ProviderUnavailable,
                Some("scripted".to_string()),
                Some(503),
            )
        }),
        feedback: None,
    }
}

/// A verified commit trained on `samples` real canonical samples, present in the
/// store. Returns the commit itself so a snapshot can be written from it.
///
/// A chained fixture is the *actual* result of applying its samples on top of
/// its parent: activation proves provenance by strict replay now, so a commit
/// that merely points at a parent it never trained from would be refused.
fn trained_commit(
    store: &mut ModelStore,
    parent: Option<CommitId>,
    suffix: &str,
    samples: usize,
) -> (ModelCommit, CommitId) {
    let (mut ensemble, base_count) = match parent.as_ref() {
        Some(id) => {
            let base = store
                .checkout_commit(id)
                .expect("a fixture parent is present in the store");
            (
                ModelEnsemble::load_all(&base.checkpoint).expect("a fixture parent loads"),
                base.learning_event_count,
            )
        }
        None => (ModelEnsemble::new(), 0),
    };
    for index in 0..samples {
        ensemble
            .update_all(&canonical_sample(&format!("{suffix}-train-{index}"), true).into_legacy());
    }
    let commit = ModelCommit::new(
        ModelId::new(MODEL),
        ensemble.save_all(),
        parent,
        base_count + samples as u64,
    );
    assert!(commit.verify(), "the fixture commit must verify");
    let id = store
        .try_insert_commit(commit.clone(), format!("fixture {suffix}"))
        .expect("a verified commit is insertable");
    (commit, id)
}

/// Record the training event that produced `commit`, which is what an
/// activation later re-records. This is the caller's job, not the mechanism's.
fn record_training_event(
    journal_dir: &Path,
    suffix: &str,
    samples: usize,
    parent: Option<CommitId>,
    result: &CommitId,
) {
    let mut journal = LearningJournal::open(journal_dir, JournalMode::Append)
        .expect("the training journal is writable");
    let mut event = CanonicalEvent::new(
        format!("train-{suffix}"),
        ModelId::new(MODEL),
        (0..samples)
            .map(|index| canonical_sample(&format!("{suffix}-train-{index}"), true))
            .collect(),
    );
    event.created_at = 1_700_000_000;
    event.parent_commit = parent;
    event.result_commit = Some(result.clone());
    event.source = Some("trainer".to_string());
    journal
        .record_canonical(event)
        .expect("the training event is recordable");
}

/// A commit trained on real samples, present in the store, *and* recorded as a
/// training event: the state from which an activation is authorized at all.
fn activatable(
    store: &mut ModelStore,
    journal_dir: &Path,
    parent: Option<CommitId>,
    suffix: &str,
    samples: usize,
) -> (ModelCommit, CommitId) {
    let (commit, id) = trained_commit(store, parent.clone(), suffix, samples);
    record_training_event(journal_dir, suffix, samples, parent, &id);
    (commit, id)
}

fn pointer_bytes(root: &Path) -> Option<Vec<u8>> {
    fs::read(root.join(ACTIVATION_POINTER_NAME)).ok()
}

fn log_bytes(root: &Path) -> Vec<u8> {
    fs::read(root.join(JOURNAL_DIR_NAME).join("journal.log")).expect("the journal log is readable")
}

fn snapshot_file(root: &Path, id: &SnapshotId) -> PathBuf {
    root.join("snapshots")
        .join(format!("{id}.{SNAPSHOT_FILE_SUFFIX}"))
}

/// Rewrite a stored snapshot through the documented wire form, so a test can
/// produce an artifact this implementation must refuse.
fn edit_snapshot_file(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let bytes = fs::read(path).expect("the snapshot is readable");
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("the snapshot is valid json");
    edit(&mut value);
    fs::write(
        path,
        serde_json::to_vec_pretty(&value).expect("serializable"),
    )
    .expect("the snapshot is writable");
}

/// The journal record that produced `commit`, with its canonical samples.
fn producing_samples(
    journal_dir: &Path,
    store: &ModelStore,
    commit: &CommitId,
) -> (ModelId, Vec<OutcomeTrainingSample>, Option<CommitId>, i64) {
    let journal = LearningJournal::open(journal_dir, JournalMode::Read)
        .expect("the training journal is readable");
    let records = journal
        .read_records(store)
        .expect("every recorded event re-verifies");
    let record = records
        .iter()
        .find(|record| record.event.result_commit.as_ref() == Some(commit))
        .expect("the commit has a producing event");
    let samples = record
        .canonical_samples()
        .expect("canonical evidence is retained")
        .to_vec();
    (
        record.event.model_id.clone(),
        samples,
        record.event.parent_commit.clone(),
        record.event.created_at,
    )
}

/// Append an activation intent record from *outside* the mechanism, deriving the
/// id the way a second implementation would. Used to stage the interrupted
/// states that a live process cannot be talked into reaching.
fn inject_intent(
    store: &ModelStore,
    journal_dir: &Path,
    generation: u64,
    from: Option<&ActivationEntry>,
    to: &ActivationEntry,
) -> String {
    let event_id = activation_plan_event_id(generation, from, to);
    let (model_id, samples, parent_commit, created_at) =
        producing_samples(journal_dir, store, &to.commit);
    let mut journal = LearningJournal::open(journal_dir, JournalMode::Append)
        .expect("the training journal is writable");
    journal
        .record_canonical(CanonicalEvent {
            event_id: event_id.clone(),
            model_id,
            samples,
            parent_commit,
            result_commit: Some(to.commit.clone()),
            created_at,
            source: Some(format!("activation:plan:{}", to.snapshot)),
        })
        .expect("the intent record is appendable");
    event_id
}

fn entry_for(commit: &ModelCommit) -> ActivationEntry {
    ActivationEntry {
        snapshot: snapshot_id_for(commit),
        commit: commit.commit_id.clone(),
        model_id: commit.model_id.clone(),
        activated_at: 1_700_000_000,
    }
}

/// Write a pointer file from the documented wire form, re-deriving the checksum
/// with the published function. This is how the "the flip landed but the
/// completion did not" state is staged.
fn write_pointer_file(
    root: &Path,
    generation: u64,
    active: &ActivationEntry,
    previous: Option<&ActivationEntry>,
) {
    let entry = |source: &ActivationEntry| PointerEntryFile {
        snapshot: source.snapshot.as_str().to_string(),
        commit: source.commit.as_str().to_string(),
        model_id: source.model_id.as_str().to_string(),
        activated_at: source.activated_at,
    };
    let file = PointerFile {
        schema_version: ACTIVATION_POINTER_SCHEMA_VERSION,
        generation,
        active: entry(active),
        previous: previous.map(entry),
        pointer_checksum: pointer_checksum(generation, active, previous),
    };
    fs::write(
        root.join(ACTIVATION_POINTER_NAME),
        serde_json::to_vec_pretty(&file).expect("serializable"),
    )
    .expect("the pointer is writable");
}

fn assert_clean_no_tmp(root: &Path) {
    assert!(
        !root.join(ACTIVATION_POINTER_TMP_NAME).exists(),
        "the temporary pointer must not survive a completed flip"
    );
    let leftovers: Vec<PathBuf> = fs::read_dir(root.join("snapshots"))
        .expect("the snapshot directory is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext != SNAPSHOT_FILE_SUFFIX)
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "a half-written snapshot must not survive: {leftovers:?}"
    );
}

// ---------------------------------------------------------------------------
// Gate 1: a verified, immutable, content-addressed snapshot
// ---------------------------------------------------------------------------

#[test]
fn a_snapshot_is_content_addressed_and_verifies_on_load() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, commit_id) =
        activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("a verified commit is writable as a snapshot");

    // The name is derived from the content, not chosen by the caller.
    assert_eq!(snapshot.id(), &snapshot_id_for(&commit));
    assert!(snapshot.id().is_well_formed());
    assert!(snapshot.id().as_str().starts_with(SNAPSHOT_ID_PREFIX));
    assert_eq!(snapshot.commit_id(), &commit_id);

    // The file name is the identity.
    let path = snapshot_file(&root, snapshot.id());
    assert!(path.is_file(), "{} is on disk", path.display());

    // Loading re-verifies through the accepted checks and re-derives the name.
    let loaded = activation
        .read_snapshot(snapshot.id())
        .expect("a stored snapshot verifies");
    assert_eq!(loaded.id(), snapshot.id());
    assert_eq!(&loaded.commit().commit_id, loaded.commit_id());
    assert!(loaded.commit().verify(), "the accepted verify still passes");
    assert_eq!(
        snapshot_id_for(loaded.commit()),
        *snapshot.id(),
        "the name is a pure function of the content"
    );

    // A different commit gets a different name, so one name cannot hold two
    // models through the ordinary path.
    let (other, _) = trained_commit(&mut store, Some(commit_id), "beta", 2);
    assert_ne!(snapshot_id_for(&other), *snapshot.id());
    assert_clean_no_tmp(&root);
}

#[test]
fn writing_a_snapshot_never_mutates_an_existing_one() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the first write succeeds");
    let path = snapshot_file(&root, snapshot.id());
    let before = fs::read(&path).expect("the snapshot is readable");

    // Writing the same commit again is refused, not absorbed.
    match activation.write_snapshot(&commit) {
        Err(ActivationError::SnapshotExists { snapshot: name, .. }) => {
            assert_eq!(name, snapshot.id().as_str());
        }
        other => panic!("expected a SnapshotExists refusal, got {other:?}"),
    }

    // The immutability guarantee holds even against a *corrupt* existing
    // snapshot: the refusal is about the name being taken, not about validity.
    fs::write(&path, b"{ not a snapshot").expect("the snapshot is writable");
    let corrupt = fs::read(&path).expect("the snapshot is readable");
    assert!(matches!(
        activation.write_snapshot(&commit),
        Err(ActivationError::SnapshotExists { .. })
    ));
    assert_eq!(
        fs::read(&path).expect("readable"),
        corrupt,
        "a refused write must not touch the bytes that are there"
    );
    assert_ne!(
        corrupt, before,
        "the corruption is in place for the next assertion"
    );

    // And loading refuses it rather than repairing it.
    assert!(matches!(
        activation.read_snapshot(snapshot.id()),
        Err(ActivationError::SnapshotUnreadable { .. })
    ));
    assert_eq!(
        fs::read(&path).expect("readable"),
        corrupt,
        "a refused load must not repair anything"
    );
    assert_clean_no_tmp(&root);
}

#[test]
fn a_snapshot_that_fails_the_accepted_integrity_checks_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the write succeeds");
    let path = snapshot_file(&root, snapshot.id());

    // The algorithm table is inside the commit's canonical identity but outside
    // the snapshot's name derivation, so this is an integrity failure rather
    // than a name mismatch — and it is caught by the accepted `verify`.
    edit_snapshot_file(&path, |value| {
        value["commit"]["algorithm_versions"][0][1] = serde_json::json!("tampered");
    });
    let tampered = fs::read(&path).expect("readable");
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::CommitRejected { commit: id, reason }) => {
            assert_eq!(&id, snapshot.commit_id());
            assert!(
                reason.contains("verify"),
                "the reason names the check: {reason}"
            );
        }
        other => panic!("expected a CommitRejected refusal, got {other:?}"),
    }
    assert_eq!(
        fs::read(&path).expect("readable"),
        tampered,
        "loading never repairs"
    );
}

#[test]
fn a_legacy_or_unknown_envelope_is_refused_by_name() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the write succeeds");
    let path = snapshot_file(&root, snapshot.id());

    // A legacy commit envelope: the unversioned form that needs an explicit
    // migration, which this module does not perform.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(0);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::LegacyEnvelope { component, version }) => {
            assert_eq!(component, "commit envelope");
            assert_eq!(version, 0);
        }
        other => panic!("expected a LegacyEnvelope refusal, got {other:?}"),
    }

    // A legacy checkpoint envelope, inside an otherwise current commit.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(1);
        value["commit"]["checkpoint"]["schema_version"] = serde_json::json!(0);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::LegacyEnvelope { component, version }) => {
            assert_eq!(component, "checkpoint envelope");
            assert_eq!(version, 0);
        }
        other => panic!("expected a LegacyEnvelope refusal, got {other:?}"),
    }

    // An unknown commit envelope version.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(1);
        value["commit"]["checkpoint"]["schema_version"] = serde_json::json!(1);
        value["commit"]["schema_version"] = serde_json::json!(7);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::UnsupportedSchema {
            component,
            version,
            supported,
        }) => {
            assert_eq!(component, "commit envelope");
            assert_eq!(version, 7);
            assert_eq!(supported, 1);
        }
        other => panic!("expected an UnsupportedSchema refusal, got {other:?}"),
    }

    // An unknown snapshot envelope version of its own.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(1);
        value["commit"]["checkpoint"]["schema_version"] = serde_json::json!(1);
        value["schema_version"] = serde_json::json!(9);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::UnsupportedSchema {
            component,
            version,
            supported,
        }) => {
            assert_eq!(component, "snapshot envelope");
            assert_eq!(version, 9);
            assert_eq!(supported, ACTIVATION_SNAPSHOT_SCHEMA_VERSION);
        }
        other => panic!("expected an UnsupportedSchema refusal, got {other:?}"),
    }
}

#[test]
fn a_snapshot_whose_stored_name_disagrees_with_its_content_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the write succeeds");
    let path = snapshot_file(&root, snapshot.id());

    // A colliding name is not a silent overwrite and not a wrong read: the
    // loader re-derives the address and refuses.
    let other_name = format!("{SNAPSHOT_ID_PREFIX}0000000000000000");
    edit_snapshot_file(&path, |value| {
        value["snapshot_id"] = serde_json::json!(other_name);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::IdentityMismatch {
            snapshot: found,
            stored,
            derived,
        }) => {
            assert_eq!(found, snapshot.id().as_str());
            assert_eq!(stored, other_name);
            assert_eq!(derived, snapshot_id_for(&commit).as_str());
        }
        other => panic!("expected an IdentityMismatch refusal, got {other:?}"),
    }
}

/// ML-16: the loader must compare the *content-derived* name, not only the
/// header the file declares. A file holding snapshot B's content with the header
/// rewritten to A used to load as A, which makes a name no longer bind content.
#[test]
fn a_snapshot_whose_content_derives_a_different_name_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, _) = trained_commit(&mut store, None, "alpha", 1);
    let (commit_b, commit_b_id) = trained_commit(&mut store, None, "beta", 2);
    let derived_a = snapshot_id_for(&commit_a);
    let derived_b = snapshot_id_for(&commit_b);
    assert_ne!(
        derived_a, derived_b,
        "the two fixtures must have different content addresses"
    );

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    assert_eq!(snapshot_a.id(), &derived_a);
    assert_eq!(snapshot_b.id(), &derived_b);

    // B's whole legal payload, with only its header rewritten to A: the header
    // agrees with the requested (and file) name, so only the re-derived content
    // address can catch it. This is a missing comparison, not a hash collision.
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(snapshot_file(&root, snapshot_b.id())).expect("readable"))
            .expect("the snapshot is valid json");
    assert_eq!(value["snapshot_id"], serde_json::json!(derived_b.as_str()));
    value["snapshot_id"] = serde_json::json!(derived_a.as_str());
    let rewritten = serde_json::to_vec_pretty(&value).expect("serializable");
    let path_a = snapshot_file(&root, snapshot_a.id());
    fs::write(&path_a, &rewritten).expect("the snapshot is writable");

    match activation.read_snapshot(snapshot_a.id()) {
        Err(ActivationError::IdentityMismatch {
            snapshot,
            stored,
            derived,
        }) => {
            assert_eq!(snapshot, derived_a.as_str());
            assert_eq!(
                stored,
                derived_a.as_str(),
                "the header was rewritten to agree with the path"
            );
            assert_eq!(
                derived,
                derived_b.as_str(),
                "the content is what derives the address"
            );
        }
        other => panic!("expected an IdentityMismatch refusal, got {other:?}"),
    }
    assert_eq!(
        fs::read(&path_a).expect("readable"),
        rewritten,
        "loading never repairs the bytes it refused"
    );

    // The untouched B snapshot still loads under its own, correct name.
    assert_eq!(
        activation
            .read_snapshot(snapshot_b.id())
            .expect("B still verifies")
            .commit_id(),
        &commit_b_id
    );
}

#[test]
fn a_malformed_snapshot_name_is_refused_before_it_becomes_a_path() {
    let temp = scratch();
    let root = store_root(temp.path());
    let activation = ActivationStore::open(&root).expect("the store opens");
    let store = ModelStore::with_model_id(ModelId::new(MODEL));

    // A name that reached `Path::join` unchecked would be a path traversal, so
    // every entry point that turns one into a path checks it first.
    for hostile in [
        "../../evil",
        "snap-",
        "snap-0123456789ABCDEF",
        "snap-0123456789abcde",
        "snap-0123456789abcdefff",
        "snap-zzzzzzzzzzzzzzzz",
        "",
        "snap-0123456789abcdef/../..",
        "snap-0123456789abcdef.json",
    ] {
        let id = SnapshotId::new(hostile);
        assert!(!id.is_well_formed(), "{hostile} must not be well formed");
        assert!(matches!(
            activation.read_snapshot(&id),
            Err(ActivationError::MalformedSnapshotId { id: refused }) if refused == hostile
        ));
        assert!(matches!(
            activation.snapshot_path(&id),
            Err(ActivationError::MalformedSnapshotId { .. })
        ));
        assert!(matches!(
            activation.activate(
                &zroutery_core::ml::activation::ActivationRequest::new(id.clone()),
                &store
            ),
            Err(ActivationError::MalformedSnapshotId { .. })
        ));
    }

    // A well-formed name that simply is not there is a different refusal.
    let absent = SnapshotId::new(format!("{SNAPSHOT_ID_PREFIX}0123456789abcdef"));
    assert!(matches!(
        activation.read_snapshot(&absent),
        Err(ActivationError::SnapshotNotFound { .. })
    ));
}

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

// ---------------------------------------------------------------------------
// Containment, and the reachability gate
// ---------------------------------------------------------------------------

#[test]
fn nothing_is_created_outside_the_caller_directory() {
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
    activation.rollback(&store).expect("rolled back");

    // Exactly the documented layout, and the locks while the handles are live.
    let expected: BTreeSet<String> = [
        ACTIVATION_POINTER_NAME.to_string(),
        "snapshots".to_string(),
        JOURNAL_DIR_NAME.to_string(),
        ACTIVATION_LOCK_NAME.to_string(),
    ]
    .into_iter()
    .collect();
    let present: BTreeSet<String> = fs::read_dir(&root)
        .expect("the root is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        present, expected,
        "the layout is exactly what is documented"
    );
    // The store lock is held for the handle's lifetime; the journal's own lock
    // exists only inside one operation, so nothing is left holding it between
    // calls.
    assert!(root.join(ACTIVATION_LOCK_NAME).is_file());
    assert!(!root.join(JOURNAL_DIR_NAME).join("journal.lock").exists());

    // Nothing was created beside the root, and no default location was used.
    let siblings: BTreeSet<String> = fs::read_dir(temp.path())
        .expect("the parent is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(siblings, BTreeSet::from(["activation-store".to_string()]));

    // With the handles dropped, only the durable artifacts remain.
    drop(activation);
    let present: BTreeSet<String> = fs::read_dir(&root)
        .expect("the root is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        present,
        BTreeSet::from([
            ACTIVATION_POINTER_NAME.to_string(),
            "snapshots".to_string(),
            JOURNAL_DIR_NAME.to_string(),
        ])
    );
    assert_eq!(
        fs::read_dir(root.join("snapshots"))
            .expect("readable")
            .filter_map(Result::ok)
            .count(),
        2,
        "both snapshots are still there: a rollback deletes nothing"
    );
    assert!(ACTIVATION_ROLE.contains("does not enable the ml feature"));
    assert!(ACTIVATION_ROLE.contains("no shipped caller"));
}

#[test]
fn this_module_reaches_no_live_activation_seam() {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ml/activation.rs"))
            .expect("this module's own source is readable");

    // Call-shaped patterns, so prose that *describes* the boundary does not
    // count as violating it: what must not exist is code that can reach the
    // accepted swap, its training sibling, the cold-start ensemble, a thread, a
    // timer, or the network. `ModelState` is the persisted weight vector and is
    // the one model-layer type this module needs; the four live model types are
    // named individually so the allowance cannot quietly widen.
    for forbidden in [
        "super::shadow",
        "crate::ml::shadow",
        "zroutery_core::ml::shadow",
        "ShadowEngine::",
        ".swap(",
        "try_train(",
        ".train(",
        "ModelEnsemble",
        "ModelEnsemblePredictor",
        "EnsemblePredictor",
        "DecisionEngine",
        "SuccessModel",
        "LatencyModel",
        "TtftModel",
        "CostModel",
        "RoutingModel",
        "thread::",
        "tokio::",
        "reqwest::",
        "set_head(",
        "migrate_legacy",
        "update_all(",
    ] {
        assert!(
            !source.contains(forbidden),
            "activation.rs must not contain {forbidden:?}"
        );
    }
    assert!(
        source.contains("use super::model::ModelState;"),
        "the only model-layer type this module may name is the persisted state"
    );

    // No configuration switch, and no location this module would choose itself.
    for forbidden in [
        "std::env",
        "var(\"",
        "ZROUTERY_",
        "ShadowConfig",
        "::default()",
    ] {
        assert!(
            !source.contains(forbidden),
            "activation.rs must not contain {forbidden:?}"
        );
    }
}

#[test]
fn nothing_in_the_shipped_product_can_name_this_module() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = core
        .parent()
        .and_then(Path::parent)
        .expect("the crate sits two levels below the workspace root");

    // The names this node introduces. A bare "activation" is not one of them: the
    // accepted bandit and decision-contract modules use the word in prose about
    // a mathematical activation function, and a scan that flagged those would be
    // measuring the wrong thing.
    let symbols = [
        "ml::activation",
        "activation::",
        "ActivationStore",
        "ActivationRequest",
        "ActivationOutcome",
        "ActivationError",
        "ActivationAudit",
        "ActivationPointer",
        "ActivationEntry",
        "ActivationTrace",
        "ActivationKind",
        "ActivationStage",
        "ActiveSnapshot",
        "SnapshotId",
        "SnapshotFile",
        "PointerFile",
        "PointerEntryFile",
        "PendingActivation",
        "PointerDisagreement",
        "ACTIVATION_POINTER_NAME",
        "ACTIVATION_ROLE",
        "ACTIVATION_SNAPSHOT_SCHEMA_VERSION",
        "POINTER_TMP_NAME",
        "SNAPSHOTS_DIR_NAME",
        "SNAPSHOT_FILE_SUFFIX",
        "ACTIVATION_LOCK_NAME",
        "JOURNAL_DIR_NAME",
        "activation_plan_event_id",
        "activation_applied_event_id",
        "snapshot_id_for",
        "snapshot_checksum",
        "pointer_checksum",
    ];

    let mut scanned = 0usize;
    for (label, dir) in [
        ("server", core.join("src/server")),
        ("src-tauri", repo.join("src-tauri/src")),
        ("ui", repo.join("ui")),
    ] {
        assert!(
            dir.is_dir(),
            "{} is expected to exist for this gate: {}",
            label,
            dir.display()
        );
        for path in rust_and_manifest_files(&dir) {
            let text = fs::read_to_string(&path).expect("readable");
            scanned += 1;
            for symbol in symbols {
                assert!(
                    !text.contains(symbol),
                    "{} must not name {symbol:?}",
                    path.display()
                );
            }
        }
    }

    // The desktop application ships the learning stack, since ADR-0006 replaced
    // the prohibition on a model serving with a gate, a deterministic fallback
    // and a rollback. Two things are still worth asserting about that, and they
    // are the opposite of the assertion that used to be here.
    //
    // First, the escape hatch has to remain: `--no-default-features` must still
    // produce a desktop binary with no ML stack, because "ship without
    // learning" is a legitimate choice for someone who does not want their
    // routing history on disk.
    let manifest = fs::read_to_string(repo.join("src-tauri/Cargo.toml")).expect("readable");
    assert!(
        manifest.contains("ml = [\"zroutery-core/ml\"]"),
        "the desktop app must expose the ml feature as a named package feature, \
         so it can be turned off; the manifest is:\n{manifest}"
    );
    assert!(
        manifest.contains("default = [\"ml\"]"),
        "the desktop app ships with the learning stack by default"
    );
    // And it is not enabled by a bare feature reference on the dependency, which
    // would make it impossible to turn off without editing the manifest.
    for line in manifest.lines() {
        if line.contains("zroutery-core") && line.contains("features") {
            panic!(
                "the core dependency must not name features directly; the package \
                 feature is the switch, so it can be turned off. Offending line: {line}"
            );
        }
    }

    // Second, and the property that actually matters now: enabling the stack is
    // not the same as changing behaviour. A desktop install that has never
    // promoted a model must route exactly as it did before. That is asserted
    // behaviourally in `config.rs`, against a default-constructed and a
    // document-deserialised configuration, because the property is about what
    // those configurations *do* rather than about how the default is written.

    // The crate still gates the whole ml module, and its own re-exports do not
    // widen the surface. Line endings are normalized so the check is about the
    // text rather than about how a checkout happens to be stored.
    let lib = fs::read_to_string(core.join("src/lib.rs"))
        .expect("readable")
        .replace("\r\n", "\n");
    let module_line = lib
        .lines()
        .position(|line| line.trim() == "pub mod ml;")
        .expect("lib.rs declares the ml module");
    let gate = lib.lines().nth(module_line - 1).unwrap_or_default();
    assert_eq!(
        gate.trim(),
        "#[cfg(feature = \"ml\")]",
        "the whole ml module stays behind the feature flag"
    );
    let re_export = lib
        .find("pub use ml::{")
        .expect("lib.rs re-exports a curated set of ml items");
    let ml_block = lib[re_export..]
        .split("};")
        .next()
        .expect("the block is terminated");
    for symbol in symbols {
        assert!(
            !ml_block.contains(symbol),
            "lib.rs must not re-export {symbol:?}"
        );
    }

    // Nothing else inside the ml module names this one either, so there is no
    // sibling caller waiting for a feature flag. `mod.rs` is the registration
    // point and is expected to mention it exactly twice: the declaration and the
    // re-export list.
    for entry in fs::read_dir(core.join("src/ml")).expect("readable") {
        let path = entry.expect("readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let text = fs::read_to_string(&path).expect("readable");
        if name == "mod.rs" {
            assert_eq!(
                text.matches("pub mod activation;").count(),
                1,
                "the module is declared once"
            );
            assert_eq!(
                text.matches("pub use activation::{").count(),
                1,
                "and re-exported once"
            );
            continue;
        }
        if name == "activation.rs" {
            continue;
        }
        for symbol in [
            "activation::",
            "ActivationStore",
            "SnapshotId",
            "ACTIVATION_ROLE",
        ] {
            assert!(
                !text.contains(symbol),
                "{} must not name {symbol:?}",
                path.display()
            );
        }
    }
    assert!(scanned >= 3, "the scan covered the shipped trees");
}

/// Every `.rs` file under a directory, plus the manifests, in a stable order.
fn rust_and_manifest_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("readable") {
            let path = entry.expect("readable").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.ends_with(".rs") || name == "Cargo.toml" {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// A small compile-time reminder of what the mechanism is, and is not
// ---------------------------------------------------------------------------

/// A snapshot verifies and carries a commit. That is the whole of it: there is
/// no predicate, no engine, and no serving handle anywhere in this type's
/// surface, which is why an activation here cannot install a model even if a
/// caller asked it to.
#[test]
fn a_snapshot_carries_a_verified_commit_and_nothing_else() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot: Snapshot = activation.write_snapshot(&commit).expect("written");
    assert!(snapshot.commit().verify());
    assert_eq!(
        snapshot.commit().checkpoint.feature_schema_version,
        FEATURE_SCHEMA_VERSION
    );
    // The audit is the reporting surface, and it reports records, not models.
    // It must not mutate anything either.
    let log_before = log_bytes(&root);
    let audit: ActivationAudit = activation.audit(&store).expect("readable");
    assert!(
        audit.traces.is_empty(),
        "no activation has happened, so there is nothing to report"
    );
    assert!(audit.pending.is_none());
    assert!(audit.disagreement.is_none());
    assert_eq!(log_bytes(&root), log_before, "an audit writes nothing");
    assert!(
        pointer_bytes(&root).is_none(),
        "an audit never brings a snapshot into being"
    );
}
