use std::fs;
use std::path::{Path, PathBuf};

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ir::Usage;
use zroutery_core::ml::activation::{
    activation_plan_event_id, pointer_checksum, snapshot_id_for, ActivationEntry, PointerEntryFile,
    PointerFile, SnapshotId, ACTIVATION_POINTER_NAME, ACTIVATION_POINTER_SCHEMA_VERSION,
    ACTIVATION_POINTER_TMP_NAME, JOURNAL_DIR_NAME, SNAPSHOT_FILE_SUFFIX,
};
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::journal::{CanonicalEvent, JournalMode, LearningJournal};
use zroutery_core::ml::model_identity::{
    CommitId, ModelCommit, ModelEnsemble, ModelId, ModelStore,
};
use zroutery_core::outcome::{
    Attempt, CandidateIdentity, FailureFacts, FinalStatus, OutcomeIdentity,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub(super) const MODEL: &str = "ensemble";
pub(super) const PROVIDER: &str = "openai";
pub(super) const MODEL_NAME: &str = "gpt-x";

/// A temporary directory that removes itself, so even a failing assertion cannot
/// leave artifacts on the shared drive.
pub(super) fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

pub(super) fn store_root(root: &Path) -> PathBuf {
    root.join("activation-store")
}

pub(super) fn features(seed: u16) -> RoutingFeatures {
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
pub(super) fn canonical_sample(suffix: &str, served: bool) -> OutcomeTrainingSample {
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
        cost: None,
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
pub(super) fn trained_commit(
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
pub(super) fn record_training_event(
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
pub(super) fn activatable(
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

pub(super) fn pointer_bytes(root: &Path) -> Option<Vec<u8>> {
    fs::read(root.join(ACTIVATION_POINTER_NAME)).ok()
}

pub(super) fn log_bytes(root: &Path) -> Vec<u8> {
    fs::read(root.join(JOURNAL_DIR_NAME).join("journal.log")).expect("the journal log is readable")
}

pub(super) fn snapshot_file(root: &Path, id: &SnapshotId) -> PathBuf {
    root.join("snapshots")
        .join(format!("{id}.{SNAPSHOT_FILE_SUFFIX}"))
}

/// Rewrite a stored snapshot through the documented wire form, so a test can
/// produce an artifact this implementation must refuse.
pub(super) fn edit_snapshot_file(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
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
pub(super) fn producing_samples(
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
pub(super) fn inject_intent(
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

pub(super) fn entry_for(commit: &ModelCommit) -> ActivationEntry {
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
pub(super) fn write_pointer_file(
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

pub(super) fn assert_clean_no_tmp(root: &Path) {
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
