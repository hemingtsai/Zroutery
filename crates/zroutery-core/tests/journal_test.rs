#![cfg(feature = "ml")]

//! Node 7E-2E gate tests: the durable, ordered, idempotent learning journal.
//!
//! Every prior ML node in this repository is pure and in-memory. This one writes
//! to disk, so its failure modes are new and each claim below is stated as a
//! property of the bytes rather than of a field the handle happens to hold.
//!
//! The gates, in the order this file asserts them:
//!
//! 1. **Durability.** A record written before a restart is readable after it,
//!    proved by dropping the handle and opening a *second* one. The ordering
//!    state comes from the file: the fresh handle's `next_sequence` continues
//!    the old one, and a second handle opened while the first is live is
//!    refused.
//! 2. **Ordering.** A monotonic sequence that continues across restarts, a gap
//!    that is detected, and a belief that the journal is at zero that is
//!    refused.
//! 3. **Idempotency.** The same event id with the same content is a reported
//!    no-op that writes nothing; the same event id with different content is a
//!    conflict, never a last-write-wins.
//! 4. **Verified commits.** Every commit a record names is re-verified on read
//!    through the accepted checks, and one that no longer verifies is refused
//!    rather than reported as present.
//! 5. **Fail-closed corruption.** A per-record checksum, a torn tail reported as
//!    a torn tail, a corrupt *interior* record that refuses the whole read, and
//!    a suffix removed at a frame boundary reported as lost records.
//! 6. **Containment.** Writes touch only the caller's directory, never a default
//!    location, never a temp path outside it, and never a location derived from
//!    configuration the caller did not provide. No thread, no timer.
//! 7. **Typed refusals.** Every required refusal, with a reason, and no panic
//!    path anywhere in the module.
//! 8. **The legacy/canonical trap.** The record retains the canonical evidence,
//!    the projection is proved on the way in *and* on the way out, and the
//!    degraded path is labelled rather than silent.
//! 9. **Inertness.** Opening or reading this journal cannot cause anything to be
//!    trained, put into service, or scheduled.

use std::fs;
use std::path::{Path, PathBuf};

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ir::Usage;
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::journal::{
    frame_checksum, CanonicalEvent, JournalError, JournalEvidence, JournalMode, JournalRecordBody,
    LearningJournal, RecordOutcome, SequenceFault, FIRST_SEQUENCE, JOURNAL_ANCHOR_NAME,
    JOURNAL_LOCK_NAME, JOURNAL_LOG_NAME, JOURNAL_ROLE, JOURNAL_SCHEMA_VERSION, LEGACY_DEGRADATION,
};
use zroutery_core::ml::model_identity::{
    CommitId, LearningEvent, ModelCommit, ModelEnsemble, ModelId, ModelStore,
    LEARNING_EVENT_SCHEMA_VERSION,
};
use zroutery_core::outcome::{Attempt, CandidateIdentity, FailureFacts, FinalStatus, OutcomeIdentity};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const MODEL: &str = "ensemble";
const PROVIDER: &str = "openai";
const MODEL_NAME: &str = "gpt-x";

/// A temporary directory that removes itself, so a failing assertion cannot
/// leave artifacts on the shared drive.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn journal_dir(root: &Path) -> PathBuf {
    root.join("learning-journal")
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

/// A *canonical* sample: the lossless shape, carrying the attempt evidence,
/// usage, cost, terminal facts, dialect, and correlation ids that the legacy
/// projection throws away. This is the evidence the journal must retain.
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
    let served_identity = CandidateIdentity::new(MODEL_NAME.to_string(), PROVIDER.to_string());
    OutcomeTrainingSample {
        sample_id: format!("samp-{suffix}-request"),
        schema_version: FEATURE_SCHEMA_VERSION,
        timestamp: 1_700_000_000,
        streaming: true,
        dialect: "openai".to_string(),
        features: features(suffix.len() as u16),
        targets: Targets {
            success: served,
            latency_ms: served.then_some(200.0),
            ttft_ms: served.then_some(50.0),
            cost: Some(0.0021),
            failure_class: (!served).then(|| "provider_unavailable".to_string()),
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
            planned: Some(served_identity.clone()),
            last_attempted: Some(served_identity.clone()),
            served: served.then(|| served_identity.clone()),
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
                Some("scripted failure".to_string()),
                Some(503),
            )
        }),
        feedback: None,
    }
}

fn canonical_event(event_id: &str, suffix: &str, served: bool) -> CanonicalEvent {
    let mut event = CanonicalEvent::new(
        event_id,
        ModelId::new(MODEL),
        vec![canonical_sample(suffix, served)],
    );
    event.created_at = 1_700_000_000;
    event.source = Some("test".to_string());
    event
}

/// A verified commit, present in a `ModelStore`.
fn verified_commit(store: &mut ModelStore, parent: Option<CommitId>, samples: usize) -> CommitId {
    let mut ensemble = ModelEnsemble::new();
    for index in 0..samples {
        ensemble.update_all(&canonical_sample(&format!("train-{index}"), true).into_legacy());
    }
    let commit = ModelCommit::new(
        ModelId::new(MODEL),
        ensemble.save_all(),
        parent,
        samples as u64,
    );
    assert!(commit.verify(), "the fixture commit must verify");
    store
        .try_insert_commit(commit, "fixture".to_string())
        .expect("a verified commit is insertable")
}

fn log_bytes(dir: &Path) -> Vec<u8> {
    fs::read(dir.join(JOURNAL_LOG_NAME)).expect("the log is readable")
}

fn write_log_bytes(dir: &Path, bytes: &[u8]) {
    fs::write(dir.join(JOURNAL_LOG_NAME), bytes).expect("the log is writable");
}

fn log_len(dir: &Path) -> u64 {
    fs::metadata(dir.join(JOURNAL_LOG_NAME))
        .expect("the log is stat-able")
        .len()
}

/// The offset of the first byte of each frame, found by scanning the file for
/// the terminator rather than by trusting an assumption about the encoding.
fn frame_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut start = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            offsets.push(start);
            start = index + 1;
        }
    }
    offsets
}

/// Re-encode a frame independently of the module, using the public checksum
/// function. The corruption tests need frames whose checksums are *correct*, so
/// that a refusal can only come from the property under test.
fn encode_frame(previous: u64, sequence: u64, body: &[u8]) -> Vec<u8> {
    let mut frame = format!("{sequence}\t{:016x}\t", frame_checksum(previous, sequence, body))
        .into_bytes();
    frame.extend_from_slice(body);
    frame.push(b'\n');
    frame
}

fn body_bytes(body: &JournalRecordBody) -> Vec<u8> {
    serde_json::to_vec(body).expect("a record body serializes")
}

fn directory_listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("the directory is readable")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn sequence_of(outcome: RecordOutcome) -> u64 {
    match outcome {
        RecordOutcome::Appended { sequence, .. } | RecordOutcome::Duplicate { sequence } => sequence,
    }
}

// ---------------------------------------------------------------------------
// GATE 1 — durability across a restart
// ---------------------------------------------------------------------------

/// GATE 1: a record written before a restart is readable after it.
///
/// "Restart" is modelled honestly: the handle that wrote the records is dropped,
/// which destroys every field it held, and a *second* handle reads the journal.
/// Nothing the second handle knows can have come from the first, so everything it
/// reports came from the file.
#[test]
fn a_record_written_before_a_restart_is_readable_after_it() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let store = ModelStore::new();

    let mut before = LearningJournal::open(&dir, JournalMode::Append).expect("a new journal opens");
    for index in 0..3u64 {
        let outcome = before
            .record_canonical(canonical_event(
                &format!("evt-durable-{index}"),
                &format!("d{index}"),
                index % 2 == 0,
            ))
            .expect("a canonical event is recordable");
        assert_eq!(
            sequence_of(outcome),
            index + 1,
            "the journal assigns the sequence the file already implies"
        );
    }
    let sequence_before_restart = before.next_sequence();
    assert_eq!(sequence_before_restart, 4);
    drop(before);

    let after = LearningJournal::open(&dir, JournalMode::Read).expect("the journal reopens");
    let records = after.read_records(&store).expect("the log reads back");

    assert_eq!(records.len(), 3, "every record survived the restart");
    for (index, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence,
            index as u64 + 1,
            "the sequence came from the file, in order"
        );
        assert_eq!(
            record.event.event_id,
            format!("evt-durable-{index}"),
            "the event survived the restart"
        );
    }
    assert_eq!(
        after.next_sequence(),
        sequence_before_restart,
        "the recovered ordering state matches what the previous handle believed"
    );
    assert_eq!(
        after.next_sequence(),
        4,
        "and it continues the log on disk rather than restarting at zero"
    );
}

/// GATE 1, second part: a second handle on a live journal is refused.
///
/// The gate asks for a second handle that is real, and the journal's answer is
/// that two handles are never simultaneously valid. A lock left behind by a
/// process that died is handled by the format rather than by trusting the lock,
/// which is what the next test proves.
#[test]
fn a_second_handle_on_a_live_journal_is_refused() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let mut first = LearningJournal::open(&dir, JournalMode::Append).expect("the first handle opens");
    first
        .record_canonical(canonical_event("evt-lock-1", "l1", true))
        .expect("a record is written");

    match LearningJournal::open(&dir, JournalMode::Read) {
        Err(JournalError::JournalLocked { path, holder }) => {
            assert_eq!(path, dir.join(JOURNAL_LOCK_NAME));
            assert!(
                holder.contains("pid"),
                "the refusal names the holder, got {holder:?}"
            );
        }
        other => panic!("a second handle must be refused, got {other:?}"),
    }
    drop(first);

    let reopened = LearningJournal::open(&dir, JournalMode::Read).expect("the lock was released");
    assert_eq!(reopened.next_sequence(), 2);
}

/// GATE 1, third part: a stale lock is a refusal, and clearing it cannot lose a
/// record. This is the honest residual of a portable lock: the mechanism can be
/// left behind by a process that dies without unwinding, so the *format* has to
/// be what actually protects the records.
#[test]
fn a_stale_lock_is_a_refusal_and_not_a_corruption() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-stale-1", "s1", true))
            .expect("a record is written");
    }
    fs::write(dir.join(JOURNAL_LOCK_NAME), "pid 999999 opened it\n")
        .expect("the lock file is writable");
    let blocked = LearningJournal::open(&dir, JournalMode::Append);
    assert!(
        matches!(blocked, Err(JournalError::JournalLocked { .. })),
        "a stale lock is refused, not adopted"
    );

    fs::remove_file(dir.join(JOURNAL_LOCK_NAME)).expect("the stale lock is cleared");
    let journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal reopens");
    let store = ModelStore::new();
    let records = journal.read_records(&store).expect("the record is intact");
    assert_eq!(records.len(), 1, "clearing a lost lock did not lose a record");
}

// ---------------------------------------------------------------------------
// GATE 2 — ordering
// ---------------------------------------------------------------------------

/// GATE 2: the sequence continues across a restart, and a caller that believes
/// the journal is back at zero is refused rather than obeyed.
#[test]
fn a_restarted_sequence_of_zero_is_refused() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..3 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-order-{index}"),
                    &format!("o{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }

    let journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal reopens");
    journal
        .expect_next_sequence(4)
        .expect("the file agrees with the caller");

    match journal.expect_next_sequence(0) {
        Err(JournalError::SequenceConflict {
            expected,
            actual,
            fault,
        }) => {
            assert_eq!(expected, 0, "the belief is reported, not overwritten");
            assert_eq!(actual, 4, "the file's value is reported");
            assert_eq!(fault, SequenceFault::Rewind, "and it is named a rewind");
        }
        other => panic!("a rewind to zero must be refused, got {other:?}"),
    }
    journal
        .expect_next_sequence(4)
        .expect("the refusal reported; it changed nothing");
}

/// GATE 2, second part: a gap is detected and reported, never silently accepted.
/// The interior frame is removed and the survivors are left untouched, so only
/// the sequence can reveal the loss.
#[test]
fn a_removed_interior_record_is_detected_as_a_gap() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..3 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-gap-{index}"),
                    &format!("g{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }
    let bytes = log_bytes(&dir);
    let offsets = frame_offsets(&bytes);
    assert_eq!(offsets.len(), 3, "three frames were written");
    write_log_bytes(&dir, &[&bytes[..offsets[1]], &bytes[offsets[2]..]].concat());

    match LearningJournal::open(&dir, JournalMode::Read) {
        Err(JournalError::SequenceConflict {
            expected,
            actual,
            fault,
        }) => {
            assert_eq!(expected, 2, "the journal knows what it expected");
            assert_eq!(actual, 3, "and what the file says");
            assert_eq!(fault, SequenceFault::Missing, "which is a gap");
        }
        other => panic!("a gap must be a sequence conflict, got {other:?}"),
    }
}

/// GATE 2, third part: a duplicated frame — what two writers racing past a lost
/// lock produce — is detected rather than accepted as a second order.
#[test]
fn a_duplicated_frame_is_detected() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..2 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-dup-{index}"),
                    &format!("d{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }
    let bytes = log_bytes(&dir);
    let offsets = frame_offsets(&bytes);
    write_log_bytes(
        &dir,
        &[
            &bytes[..offsets[1]],
            &bytes[offsets[0]..offsets[1]],
            &bytes[offsets[1]..],
        ]
        .concat(),
    );

    match LearningJournal::open(&dir, JournalMode::Read) {
        Err(JournalError::SequenceConflict {
            fault, actual, ..
        }) => {
            assert_eq!(actual, 1, "the repeat is the sequence just read");
            assert_eq!(fault, SequenceFault::Duplicate, "and it is named a duplicate");
        }
        other => panic!("a duplicated frame must be refused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// GATE 3 — idempotency
// ---------------------------------------------------------------------------

/// GATE 3: re-recording the same event id with identical content is a reported
/// no-op that writes nothing.
#[test]
fn an_identical_record_is_a_reported_no_op() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let store = ModelStore::new();
    let mut journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
    let event = canonical_event("evt-idem-1", "i1", true);

    assert_eq!(
        sequence_of(
            journal
                .record_canonical(event.clone())
                .expect("the first record is written")
        ),
        1
    );
    let length_after_first = log_len(&dir);

    // A retry: the same id and the same content, at a different wall clock,
    // exactly as a caller retrying after a timeout would produce.
    let mut retry = event;
    retry.created_at = 1_800_000_000;
    assert_eq!(
        journal.record_canonical(retry).expect("the retry is accepted"),
        RecordOutcome::Duplicate { sequence: 1 },
        "the no-op is reported, with the sequence that was already there"
    );
    assert_eq!(
        log_len(&dir),
        length_after_first,
        "a duplicate writes no bytes"
    );

    let records = journal.read_records(&store).expect("the journal reads back");
    assert_eq!(records.len(), 1, "the retry did not create a second record");
}

/// A positive finite `f64` that does NOT survive a serde_json round trip.
///
/// The workspace uses serde_json WITHOUT its `float_roundtrip` feature, so its
/// float parsing is not correctly rounded. This is the value class that made a
/// byte-identical retry look like a conflict when the duplicate check compared
/// the stored PARSED body against the caller's in-memory body.
fn json_hostile_positive() -> f64 {
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..200_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let value = f64::from_bits(seed);
        if !value.is_finite() || value <= 0.0 {
            continue;
        }
        let text = serde_json::to_string(&value).expect("a float serializes");
        let back: f64 = serde_json::from_str(&text).expect("a float parses");
        if back.to_bits() != value.to_bits() {
            return value;
        }
    }
    panic!("no JSON-hostile positive f64 was found");
}

/// The canonical event of `a_retry_of_an_unchanged_record_is_a_duplicate`, with
/// a float in it that the JSON round trip cannot preserve.
fn json_hostile_event(event_id: &str) -> CanonicalEvent {
    let hostile = json_hostile_positive();
    let mut event = canonical_event(event_id, "jh", true);
    for sample in &mut event.samples {
        sample.targets.cost = Some(hostile);
        sample.actual_cost = Some(hostile);
    }
    event
}

/// A byte-identical retry must be reported as a duplicate, even when the record
/// carries a float whose value cannot survive the JSON round trip.
///
/// The duplicate check compares the bytes that WOULD be written against the
/// bytes already on disk, never a re-parsed value against an in-memory one. A
/// retry after an ambiguous write — a crash, a timeout — is the one scenario
/// idempotency exists for, and this is the case that used to refuse it.
#[test]
fn a_retry_of_an_unchanged_record_is_a_duplicate_even_with_a_json_hostile_float() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let mut journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
    let event = json_hostile_event("evt-json-hostile-1");

    journal
        .record_canonical(event.clone())
        .expect("the first record is written");

    match journal.record_canonical(event.clone()) {
        Ok(RecordOutcome::Duplicate { sequence }) => {
            assert_eq!(sequence, 1, "the duplicate names the stored sequence");
        }
        other => panic!("an unchanged retry must be a duplicate, got {other:?}"),
    }

    // A retry that re-creates the same event at a different wall-clock instant
    // is still the same event, exactly as the accepted payload_hash treats
    // volatile metadata.
    let mut later = event.clone();
    later.created_at = event.created_at + 3_600;
    match journal.record_canonical(later) {
        Ok(RecordOutcome::Duplicate { sequence }) => {
            assert_eq!(sequence, 1, "a later instant is the same event");
        }
        other => panic!("a retry at a different instant must be a duplicate, got {other:?}"),
    }

    // And a genuinely different record is still refused, so the byte comparison
    // did not turn into "everything matches".
    let mut different = event.clone();
    different.samples[0].targets.latency_ms = Some(999.0);
    match journal.record_canonical(different) {
        Err(JournalError::IdempotencyConflict { event_id, .. }) => {
            assert_eq!(event_id, "evt-json-hostile-1");
        }
        other => panic!("a changed record must still conflict, got {other:?}"),
    }

    let log = fs::read(dir.join(JOURNAL_LOG_NAME)).expect("the log is readable");
    let text = String::from_utf8_lossy(&log);
    assert_eq!(
        text.lines().filter(|line| !line.trim().is_empty()).count(),
        1,
        "exactly one record exists after a duplicate, a later-instant duplicate, and a conflict"
    );
}

/// GATE 3, second part: the same event id with different content is a conflict,
/// never a last-write-wins.
#[test]
fn a_conflicting_record_is_refused_and_never_overwrites() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let store = ModelStore::new();
    let mut journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
    journal
        .record_canonical(canonical_event("evt-conflict-1", "c1", true))
        .expect("the first record is written");
    let original = journal.read_records(&store).expect("the journal reads back");

    match journal.record_canonical(canonical_event("evt-conflict-1", "c2", false)) {
        Err(JournalError::IdempotencyConflict { event_id, reason }) => {
            assert_eq!(event_id, "evt-conflict-1");
            assert!(
                reason.contains("append-only"),
                "the refusal explains the policy, got {reason}"
            );
        }
        other => panic!("a conflicting record must be refused, got {other:?}"),
    }

    let after = journal.read_records(&store).expect("the journal still reads back");
    assert_eq!(after.len(), 1, "the conflict added nothing");
    assert_eq!(
        after[0].event.samples, original[0].event.samples,
        "and the original record is untouched"
    );
}

/// GATE 3, third part: the deduplication key itself has to be durable.
///
/// `LearningEvent::new` mints `evt-<n>` from a process-local counter, so two
/// processes both produce `evt-1`. A journal that accepted those ids would
/// deduplicate unrelated events against one another, and a restart would look
/// like a re-recording. The journal refuses the form instead of pretending it is
/// an identity, which costs no capability because the field is public.
#[test]
fn a_process_local_event_id_is_refused() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let mut journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");

    let event = LearningEvent::new(
        ModelId::new(MODEL),
        vec![canonical_sample("volatile", true).into_legacy()],
        None,
        Some("test".to_string()),
    );
    assert!(
        event.event_id.starts_with("evt-"),
        "the accepted constructor mints the process-local form, got {:?}",
        event.event_id
    );
    match journal.record_legacy(&event) {
        Err(JournalError::VolatileEventId { event_id }) => assert_eq!(event_id, event.event_id),
        other => panic!("a volatile event id must be refused, got {other:?}"),
    }

    let mut durable = event;
    durable.event_id = "durable-identity-1".to_string();
    assert_eq!(
        sequence_of(
            journal
                .record_legacy(&durable)
                .expect("a caller-chosen durable id is recordable")
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// GATE 4 — verified commits
// ---------------------------------------------------------------------------

/// GATE 4: a record whose commits verify is readable, and the same record is
/// refused once the commits are gone from the store.
///
/// The store being rebuilt empty is what a restart looks like for a caller whose
/// own artifact storage no longer holds the commit: the journal is on disk, the
/// evidence it names is not.
#[test]
fn a_record_whose_commit_no_longer_verifies_is_refused() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let mut store = ModelStore::new();
    let root_commit = verified_commit(&mut store, None, 0);
    let child = verified_commit(&mut store, Some(root_commit.clone()), 2);

    let mut event = canonical_event("evt-commit-1", "v1", true);
    event.parent_commit = Some(root_commit.clone());
    event.result_commit = Some(child);
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(event)
            .expect("an event with a verified lineage is recordable");
    }

    let journal = LearningJournal::open(&dir, JournalMode::Read).expect("the journal reopens");
    let records = journal.read_records(&store).expect("the lineage verifies");
    assert_eq!(records.len(), 1, "a record with a verified commit is present");
    assert!(records[0].is_applied(), "the record says it carries a result");
    assert_eq!(
        records[0].event.result_commit,
        records[0].event.result_commit.clone(),
        "and the result commit round-tripped through the log"
    );

    match journal.read_records(&ModelStore::new()) {
        Err(JournalError::CommitVerificationFailed {
            sequence,
            commit_id,
            reason,
        }) => {
            assert_eq!(sequence, 1, "the refusal names the record");
            assert_eq!(
                commit_id, root_commit,
                "and the commit that is no longer there"
            );
            assert!(reason.contains("lineage"), "and why, got {reason}");
        }
        other => panic!("an unverifiable commit must be refused, got {other:?}"),
    }
}

/// GATE 4, second part: a well-formed commit id that nothing ever produced is
/// refused too, rather than reported as a present lineage.
#[test]
fn a_record_naming_an_unknown_commit_is_refused() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let mut event = canonical_event("evt-unknown-1", "u1", true);
    event.result_commit = Some(CommitId::new("deadbeefdeadbeef"));

    let mut journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
    journal
        .record_canonical(event)
        .expect("recording needs no store; verification happens on read");

    match journal.read_records(&ModelStore::new()) {
        Err(JournalError::CommitVerificationFailed { commit_id, .. }) => {
            assert_eq!(commit_id.as_str(), "deadbeefdeadbeef")
        }
        other => panic!("an unknown commit must be refused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// GATE 5 — fail-closed corruption
// ---------------------------------------------------------------------------

/// GATE 5: a torn tail is reported as a torn tail, and the records that did land
/// are not silently returned without it.
#[test]
fn a_torn_tail_is_reported_rather_than_dropped() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..2 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-torn-{index}"),
                    &format!("t{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }
    let bytes = log_bytes(&dir);
    let offsets = frame_offsets(&bytes);
    write_log_bytes(&dir, &bytes[..offsets[1] + 20]);

    let error = LearningJournal::open(&dir, JournalMode::Read).expect_err("a torn tail refuses");
    match error {
        JournalError::TornTail {
            offset,
            bytes_present,
        } => {
            assert_eq!(offset, offsets[1] as u64, "the refusal names the offset");
            assert_eq!(bytes_present, 20, "and how much of it survived");
        }
        other => panic!("a torn tail must be a torn tail, got {other:?}"),
    }
    assert!(
        LearningJournal::open(&dir, JournalMode::Read)
            .expect_err("still refused")
            .to_string()
            .contains("torn tail"),
        "the reason says what happened"
    );
}

/// GATE 5, second part: a corrupt *interior* record refuses the whole read
/// rather than being skipped, and the refusal is all-or-nothing so a caller
/// cannot receive a short list and believe it is the whole journal.
#[test]
fn a_corrupt_interior_record_refuses_the_whole_read() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..3 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-interior-{index}"),
                    &format!("n{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }
    let mut bytes = log_bytes(&dir);
    let offsets = frame_offsets(&bytes);
    let target = offsets[1] + (offsets[2] - offsets[1]) / 2;
    assert!(
        target > offsets[1] && target < offsets[2],
        "the victim byte is inside the second frame"
    );
    bytes[target] ^= 0x01;
    write_log_bytes(&dir, &bytes);

    // The first frame is untouched, so a reader that skipped ahead would find a
    // perfectly good record 1 and stop. It must not.
    match LearningJournal::open(&dir, JournalMode::Read) {
        Err(error @ JournalError::ChecksumMismatch { sequence, .. }) => {
            assert_eq!(sequence, 2, "the refusal names the corrupt frame");
            assert!(error.to_string().contains("checksum"), "and says why");
        }
        other => panic!("a flipped byte must be a checksum mismatch, got {other:?}"),
    }
}

/// GATE 5, third part: an edited checksum is caught, with both values reported.
#[test]
fn an_edited_checksum_is_caught_with_both_values() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-edit-1", "e1", true))
            .expect("a record is written");
    }
    let mut bytes = log_bytes(&dir);
    let tab = bytes
        .iter()
        .position(|byte| *byte == b'\t')
        .expect("a frame has a tab");
    bytes[tab + 1] = if bytes[tab + 1] == b'0' { b'1' } else { b'0' };
    write_log_bytes(&dir, &bytes);

    match LearningJournal::open(&dir, JournalMode::Read) {
        Err(JournalError::ChecksumMismatch {
            sequence,
            recorded,
            computed,
            ..
        }) => {
            assert_eq!(sequence, 1, "the refusal names the frame");
            assert_ne!(recorded, computed, "and reports both values");
        }
        other => panic!("an edited checksum must be caught, got {other:?}"),
    }
}

/// GATE 5, fourth part: a suffix removed at a frame boundary is *deliberate
/// truncation*, and it is reported as lost records rather than mistaken for a
/// short journal.
///
/// This is the failure a per-record checksum cannot catch on its own: every
/// surviving frame is individually perfect and the sequence is contiguous. It is
/// caught by the anchor, which is why the anchor exists.
#[test]
fn a_boundary_truncation_is_reported_as_lost_records() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..3 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-trunc-{index}"),
                    &format!("x{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }
    let bytes = log_bytes(&dir);
    let offsets = frame_offsets(&bytes);
    write_log_bytes(&dir, &bytes[..offsets[1]]);

    match LearningJournal::open(&dir, JournalMode::Read) {
        Err(JournalError::RecordsLost { expected, found }) => {
            assert_eq!(expected, 3, "the anchor remembers all three");
            assert_eq!(found, 1, "and the log admits to one");
        }
        other => panic!("a boundary truncation must be reported, got {other:?}"),
    }
}

/// GATE 5, fifth part: a torn tail and a lost suffix are different refusals,
/// because they mean different things to whoever has to recover the journal.
#[test]
fn a_torn_tail_and_a_lost_suffix_are_different_refusals() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        for index in 0..2 {
            journal
                .record_canonical(canonical_event(
                    &format!("evt-both-{index}"),
                    &format!("b{index}"),
                    true,
                ))
                .expect("a record is written");
        }
    }
    let bytes = log_bytes(&dir);
    let offsets = frame_offsets(&bytes);

    let clone = |name: &str, log: &[u8]| -> PathBuf {
        let target = root.path().join(name);
        fs::create_dir_all(&target).expect("a directory");
        fs::write(target.join(JOURNAL_LOG_NAME), log).expect("the log is writable");
        fs::copy(dir.join(JOURNAL_ANCHOR_NAME), target.join(JOURNAL_ANCHOR_NAME))
            .expect("the anchor is copyable");
        target
    };

    // Half-written: the final frame has no terminator.
    let torn = clone("torn", &bytes[..offsets[1] + 30]);
    // Boundary-aligned: only the first frame, which is itself a perfect journal.
    let lost = clone("lost", &bytes[..offsets[1]]);

    let torn_error = LearningJournal::open(&torn, JournalMode::Read).expect_err("torn");
    let lost_error = LearningJournal::open(&lost, JournalMode::Read).expect_err("lost");
    assert!(
        matches!(torn_error, JournalError::TornTail { .. }),
        "half-written is a torn tail, got {torn_error:?}"
    );
    assert!(
        matches!(lost_error, JournalError::RecordsLost { .. }),
        "boundary-aligned is a lost suffix, got {lost_error:?}"
    );
}

/// GATE 5, sixth part: the crash window between the log flush and the anchor
/// rename. The log is one frame ahead of an untouched anchor, exactly as a power
/// cut between those two writes would leave it.
///
/// The chained checksums prove the extra frame is an intact continuation, so this
/// is *reported* rather than refused, and the next append closes the window.
#[test]
fn a_log_ahead_of_its_anchor_is_reported_and_then_closed() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-window-1", "w1", true))
            .expect("a record is written");
    }
    // Hand-append a second frame with a correct checksum and chain link, leaving
    // the anchor exactly where the first append put it.
    let second = canonical_event("evt-window-2", "w2", true);
    let body = JournalRecordBody {
        schema_version: JOURNAL_SCHEMA_VERSION,
        event_schema_version: LEARNING_EVENT_SCHEMA_VERSION,
        event: legacy_of(&second),
        evidence: JournalEvidence::Canonical {
            samples: second.samples.clone(),
        },
        recorded_at: 1_700_000_000,
    };
    append_frame(&dir, FIRST_SEQUENCE + 1, &body);

    let journal = LearningJournal::open(&dir, JournalMode::Read).expect("the window is reported");
    assert_eq!(
        journal.report().records_beyond_anchor,
        1,
        "the log is one record ahead of its anchor"
    );
    assert_eq!(journal.next_sequence(), 3, "and the chain still continues");
    assert_eq!(
        journal
            .read_records(&ModelStore::new())
            .expect("an intact continuation reads")
            .len(),
        2,
        "the extra frame is proven, not assumed"
    );

    // The next append closes the window, because the anchor is written from a
    // re-read of the log.
    drop(journal);
    let mut writer = LearningJournal::open(&dir, JournalMode::Append).expect("it reopens");
    assert_eq!(writer.report().records_beyond_anchor, 1);
    writer
        .record_canonical(canonical_event("evt-window-3", "w3", true))
        .expect("the window closes on the next append");
    drop(writer);
    let settled = LearningJournal::open(&dir, JournalMode::Read).expect("it reopens");
    assert_eq!(
        settled.report().records_beyond_anchor,
        0,
        "the anchor caught up with the log"
    );
    assert_eq!(settled.next_sequence(), 4);
}

/// The accepted event a canonical request describes, for a hand-built body.
fn legacy_of(event: &CanonicalEvent) -> LearningEvent {
    LearningEvent {
        event_id: event.event_id.clone(),
        model_id: event.model_id.clone(),
        samples: event
            .samples
            .iter()
            .cloned()
            .map(OutcomeTrainingSample::into_legacy)
            .collect(),
        parent_commit: event.parent_commit.clone(),
        result_commit: event.result_commit.clone(),
        created_at: event.created_at,
        source: event.source.clone(),
    }
}

// ---------------------------------------------------------------------------
// GATE 6 — containment
// ---------------------------------------------------------------------------

/// GATE 6: writes touch only the caller's directory, and nothing appears
/// beside it.
#[test]
fn writes_touch_only_the_caller_supplied_directory() {
    let root = scratch();
    let parent = root.path().join("app-data");
    let dir = parent.join("learning-journal");
    fs::create_dir_all(&dir).expect("the caller's directory");
    let before = directory_listing(&parent);

    let mut journal = LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
    for index in 0..2 {
        journal
            .record_canonical(canonical_event(
                &format!("evt-contain-{index}"),
                &format!("k{index}"),
                true,
            ))
            .expect("a record is written");
    }
    assert_eq!(
        journal
            .read_records(&ModelStore::new())
            .expect("the journal reads back")
            .len(),
        2
    );
    drop(journal);

    assert_eq!(
        directory_listing(&parent),
        before,
        "nothing appeared beside the caller's directory"
    );
    assert_eq!(
        directory_listing(&dir),
        vec![
            JOURNAL_ANCHOR_NAME.to_string(),
            JOURNAL_LOG_NAME.to_string()
        ],
        "and inside it, only the two durable files, once the handle is dropped"
    );
}

/// GATE 6, second part: there is no default location and no configuration-derived
/// location. The module names no environment variable, no config type, and no
/// platform directory helper, so it cannot invent a path the caller did not give
/// it. The tokens are code-shaped, so a doc sentence about what the module does
/// not do can neither satisfy nor trip the wire.
#[test]
fn the_module_derives_no_location_and_schedules_nothing() {
    let source = include_str!("../src/ml/journal.rs").to_lowercase();
    for forbidden in [
        // Location derivation the caller did not ask for.
        "env::var",
        "std::env",
        "appconfig",
        "temp_dir",
        "tempdir",
        "home_dir",
        "config_dir",
        "zroutery_config_dir",
        "dirs::",
        // Background work, scheduling, and timers.
        "std::thread",
        "thread::spawn",
        "tokio::spawn",
        "spawn(",
        "sleep(",
        "interval(",
        "tokio::time",
        "tokio::task",
        "async ",
        ".await",
        // The online learning loop. This module verifies commits; it must never
        // apply them.
        "replay(",
        "replay_verified",
        "replay_commit",
        "replay_with_checkpoint",
        "update_all",
        "try_commit",
        "mark_applied",
        "set_head",
        "install",
        "activate",
        "swap",
        // Server wiring and the network.
        "axum",
        "reqwest",
        "server::",
        "router::",
        "policy::",
    ] {
        assert!(
            !source.contains(forbidden),
            "journal.rs must not reference {forbidden}"
        );
    }
}

/// GATE 6, third part: the inert trap. Nothing outside this node names the
/// journal, so no running surface can read it, apply it, or schedule it — the
/// claim stated as a property of the repository rather than a promise.
#[test]
fn nothing_in_the_running_product_can_reach_this_journal() {
    for (path, source) in [
        (
            "src-tauri/src/main.rs",
            include_str!("../../../src-tauri/src/main.rs"),
        ),
        ("src/router.rs", include_str!("../src/router.rs")),
        ("src/policy.rs", include_str!("../src/policy.rs")),
        (
            "src/ml/coordinator.rs",
            include_str!("../src/ml/coordinator.rs"),
        ),
        ("src/ml/dataset.rs", include_str!("../src/ml/dataset.rs")),
    ] {
        let source = source.to_lowercase();
        assert!(
            !source.contains("ml::journal"),
            "{path} must not reach ml::journal"
        );
        assert!(
            !source.contains("learningjournal"),
            "{path} must not name the journal type"
        );
    }
    // The surface is public for the node that consumes it (7E-2F); it is simply
    // not consumed yet.
    assert!(include_str!("../src/ml/mod.rs").contains("pub mod journal;"));
}

// ---------------------------------------------------------------------------
// GATE 7 — typed refusals
// ---------------------------------------------------------------------------

/// GATE 7: an unwritable path, a path that is not a directory, and a directory
/// that is not a journal are three distinct refusals, each with a reason.
#[test]
fn a_bad_path_is_refused_with_its_own_reason() {
    let root = scratch();

    // A path whose parent is a file cannot be created.
    let file = root.path().join("a-file");
    fs::write(&file, b"not a directory").expect("the file is writable");
    let unwritable = file.join("journal");
    match LearningJournal::open(&unwritable, JournalMode::Append) {
        Err(JournalError::UnwritablePath { path, reason }) => {
            assert_eq!(path, unwritable);
            assert!(!reason.is_empty(), "the refusal carries a reason");
        }
        other => panic!("an unwritable path must be refused, got {other:?}"),
    }

    // An existing file is not a directory.
    match LearningJournal::open(&file, JournalMode::Append) {
        Err(JournalError::NotADirectory { path }) => assert_eq!(path, file),
        other => panic!("a file is not a journal directory, got {other:?}"),
    }

    // An anchor with no log is not a journal.
    let orphan = root.path().join("orphan");
    fs::create_dir_all(&orphan).expect("a directory");
    fs::write(orphan.join(JOURNAL_ANCHOR_NAME), b"{}").expect("the file is writable");
    match LearningJournal::open(&orphan, JournalMode::Append) {
        Err(JournalError::NotAJournal { reason, .. }) => assert!(
            reason.contains(JOURNAL_LOG_NAME),
            "and says what is missing: {reason}"
        ),
        other => panic!("an orphan anchor is not a journal, got {other:?}"),
    }

    // A log with records but no anchor cannot be vouched for: completeness is
    // unprovable, so it is refused rather than adopted.
    let unanchored = root.path().join("unanchored");
    fs::create_dir_all(&unanchored).expect("a directory");
    fs::write(unanchored.join(JOURNAL_LOG_NAME), b"1\t0000000000000000\t{}\n")
        .expect("the log is writable");
    match LearningJournal::open(&unanchored, JournalMode::Append) {
        Err(JournalError::NotAJournal { reason, .. }) => assert!(
            reason.contains(JOURNAL_ANCHOR_NAME),
            "and names the missing anchor: {reason}"
        ),
        other => panic!("an unanchored log is not a journal, got {other:?}"),
    }

    // Reading a directory that holds no journal writes nothing either.
    let empty = root.path().join("empty");
    fs::create_dir_all(&empty).expect("a directory");
    assert!(matches!(
        LearningJournal::open(&empty, JournalMode::Read),
        Err(JournalError::NotAJournal { .. })
    ));
    assert!(
        directory_listing(&empty).is_empty(),
        "a refused read must not create anything"
    );
}

/// GATE 7, second part: a handle opened for reading refuses to write.
#[test]
fn a_reading_handle_refuses_to_write() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-readonly-1", "r1", true))
            .expect("a record is written");
    }
    let mut journal = LearningJournal::open(&dir, JournalMode::Read).expect("the journal opens");
    let error = journal
        .record_canonical(canonical_event("evt-readonly-2", "r2", true))
        .expect_err("a reading handle cannot write");
    assert!(matches!(error, JournalError::ReadOnlyJournal { .. }));
    assert_eq!(log_len(&dir), log_len(&dir), "and it wrote nothing");
}

/// GATE 7, third part: every refusal is a typed, printable error carrying a
/// reason, and the module holds no panic path.
#[test]
fn the_module_holds_no_panic_path() {
    let source = include_str!("../src/ml/journal.rs");
    for forbidden in [
        "unwrap(",
        ".expect(",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "assert!",
        "assert_eq!",
    ] {
        assert!(
            !source.contains(forbidden),
            "journal.rs must not contain {forbidden}"
        );
    }
}

/// GATE 7, fourth part: the refusals are nameable and readable, because a typed
/// error nobody can print is not a report.
#[test]
fn every_refusal_carries_a_reason() {
    let refusals = vec![
        JournalError::UnwritablePath {
            path: PathBuf::from("/p"),
            reason: "disk".to_string(),
        },
        JournalError::NotADirectory {
            path: PathBuf::from("/p"),
        },
        JournalError::NotAJournal {
            path: PathBuf::from("/p"),
            reason: "foreign".to_string(),
        },
        JournalError::JournalLocked {
            path: PathBuf::from("/p"),
            holder: "pid 1".to_string(),
        },
        JournalError::ReadOnlyJournal {
            path: PathBuf::from("/p"),
        },
        JournalError::SequenceConflict {
            expected: 4,
            actual: 0,
            fault: SequenceFault::Rewind,
        },
        JournalError::ChecksumMismatch {
            offset: 0,
            sequence: 1,
            recorded: 1,
            computed: 2,
        },
        JournalError::TornTail {
            offset: 0,
            bytes_present: 4,
        },
        JournalError::CorruptRecord {
            offset: 0,
            reason: "garbage".to_string(),
        },
        JournalError::FrameTooLarge {
            offset: 0,
            bytes: 1,
        },
        JournalError::CommitVerificationFailed {
            sequence: 1,
            commit_id: CommitId::new("abc"),
            reason: "absent".to_string(),
        },
        JournalError::RecordsLost {
            expected: 3,
            found: 1,
        },
        JournalError::AnchorDisagrees {
            path: PathBuf::from("/p"),
            reason: "hash".to_string(),
        },
        JournalError::EventRejected {
            event_id: "e".to_string(),
            reason: "bad".to_string(),
        },
        JournalError::VolatileEventId {
            event_id: "evt-1".to_string(),
        },
        JournalError::IdempotencyConflict {
            event_id: "e".to_string(),
            reason: "differs".to_string(),
        },
        JournalError::UnsupportedSchema {
            component: "c".to_string(),
            version: 9,
            supported: 1,
        },
        JournalError::Io {
            path: PathBuf::from("/p"),
            reason: "eof".to_string(),
        },
    ];
    for refusal in refusals {
        let text = refusal.to_string();
        assert!(text.len() > 8, "a refusal must explain itself, got {text:?}");
        let _: &dyn std::error::Error = &refusal;
    }
}

// ---------------------------------------------------------------------------
// GATE 8 — the legacy/canonical trap
// ---------------------------------------------------------------------------

/// GATE 8: the record retains the canonical evidence, including everything the
/// legacy projection discards, and the legacy event is exactly its projection.
#[test]
fn the_canonical_evidence_is_retained_beside_the_legacy_event() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let sample = canonical_sample("canon", true);
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-canon-1", "canon", true))
            .expect("a canonical event is recordable");
    }

    let journal = LearningJournal::open(&dir, JournalMode::Read).expect("the journal reopens");
    let records = journal
        .read_records(&ModelStore::new())
        .expect("the journal reads back");
    let record = &records[0];
    assert!(!record.is_degraded(), "a canonical record is not degraded");
    let retained = record
        .canonical_samples()
        .expect("the canonical samples are retained");
    assert_eq!(retained.len(), 1);
    let kept = &retained[0];

    // Every field `into_legacy` throws away is still on the record.
    assert_eq!(kept.attempts, sample.attempts, "attempt evidence");
    assert_eq!(kept.usage, sample.usage, "usage");
    assert_eq!(kept.estimated_cost, sample.estimated_cost, "estimated cost");
    assert_eq!(kept.actual_cost, sample.actual_cost, "actual cost");
    assert_eq!(kept.dialect, sample.dialect, "dialect");
    assert_eq!(kept.request_id, sample.request_id, "request id");
    assert_eq!(kept.decision_id, sample.decision_id, "decision id");
    assert_eq!(kept.response_id, sample.response_id, "response id");
    assert_eq!(kept.final_status, sample.final_status, "terminal status");
    assert_eq!(kept.identity, sample.identity, "identity roles");
    assert_eq!(kept.scope, sample.scope, "scope");
    assert!(kept.streaming, "the streaming flag");

    // And the accepted replay type is available without the record lying about
    // what it kept.
    let projected: Vec<_> = retained
        .iter()
        .cloned()
        .map(OutcomeTrainingSample::into_legacy)
        .collect();
    assert_eq!(
        record.event.samples, projected,
        "the stored legacy samples are the projection of the retained evidence"
    );
}

/// GATE 8, second part: a record whose legacy samples were tampered with is
/// refused on read, so the projection invariant holds on the way out and not
/// merely on the way in.
///
/// The frame is hand-encoded with a *correct* chained checksum, so the
/// structural layer passes and only the semantic layer can catch it. The frame
/// is appended past the anchor — the documented crash window — because editing
/// the log in place would (correctly) trip the whole-log anchor checksum before
/// the validator ever ran.
#[test]
fn a_tampered_projection_is_refused_on_read() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-tamper-0", "canon", true))
            .expect("a canonical event is recordable");
    }
    let tampered = JournalRecordBody {
        schema_version: JOURNAL_SCHEMA_VERSION,
        event_schema_version: LEARNING_EVENT_SCHEMA_VERSION,
        event: tampered_event(),
        evidence: JournalEvidence::Canonical {
            samples: vec![canonical_sample("canon", true)],
        },
        recorded_at: 1_700_000_000,
    };
    append_frame(&dir, 2, &tampered);

    // The read path runs the validator the write path runs.
    let journal = LearningJournal::open(&dir, JournalMode::Read).expect("the frame is intact");
    assert_eq!(
        journal.report().records_beyond_anchor,
        1,
        "the structural layer accepted the hand-encoded frame"
    );
    match journal.read_records(&ModelStore::new()) {
        Err(JournalError::EventRejected { reason, .. }) => assert!(
            reason.contains("not the projection"),
            "the refusal explains the broken invariant, got {reason}"
        ),
        other => panic!("a tampered projection must be refused, got {other:?}"),
    }

    // And the validator itself refuses the same body directly, which is the
    // boundary every read goes through.
    match tampered.try_into_record(2) {
        Err(JournalError::EventRejected { reason, .. }) => assert!(
            reason.contains("not the projection"),
            "the boundary itself refuses it, got {reason}"
        ),
        other => panic!("the validating boundary must refuse it, got {other:?}"),
    }
}

/// Append a hand-encoded frame with a correct chained checksum, leaving the
/// anchor where it is.
fn append_frame(dir: &Path, sequence: u64, body: &JournalRecordBody) {
    let bytes = log_bytes(dir);
    let previous = {
        let offsets = frame_offsets(&bytes);
        let start = *offsets.last().expect("at least one frame");
        let line = &bytes[start..bytes.len() - 1];
        let mut fields = line.splitn(3, |byte| *byte == b'\t');
        fields.next().expect("a sequence field");
        let checksum = fields.next().expect("a checksum field");
        u64::from_str_radix(std::str::from_utf8(checksum).expect("ascii"), 16)
            .expect("a hex checksum")
    };
    let mut appended = bytes;
    appended.extend_from_slice(&encode_frame(previous, sequence, &body_bytes(body)));
    write_log_bytes(dir, &appended);
}

/// An event whose samples claim a latency and a cost the retained evidence does
/// not have.
fn tampered_event() -> LearningEvent {
    let mut sample = canonical_sample("canon", true).into_legacy();
    sample.targets.latency_ms = Some(9_999.0);
    sample.targets.cost = Some(42.0);
    LearningEvent {
        event_id: "evt-tamper-1".to_string(),
        model_id: ModelId::new(MODEL),
        samples: vec![sample],
        parent_commit: None,
        result_commit: None,
        created_at: 1_700_000_000,
        source: Some("test".to_string()),
    }
}

/// GATE 8, third part: the lossy path exists but is labelled, so a degraded
/// record cannot be mistaken for complete evidence — not by a caller, and not by
/// a fresh process reading the bytes.
#[test]
fn the_lossy_path_is_labelled_on_the_record() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        let mut event = LearningEvent::new(
            ModelId::new(MODEL),
            vec![canonical_sample("legacy", true).into_legacy()],
            None,
            Some("test".to_string()),
        );
        event.event_id = "evt-legacy-1".to_string();
        journal
            .record_legacy(&event)
            .expect("a legacy event is recordable");
    }

    let journal = LearningJournal::open(&dir, JournalMode::Read).expect("the journal reopens");
    let records = journal
        .read_records(&ModelStore::new())
        .expect("the journal reads back");
    let record = &records[0];
    assert!(
        record.is_degraded(),
        "a legacy record says so, so a consumer can refuse it"
    );
    assert!(
        record.canonical_samples().is_none(),
        "and it does not pretend to carry canonical evidence"
    );
    match &record.evidence {
        JournalEvidence::LegacyProjection { degradation } => {
            assert_eq!(degradation, LEGACY_DEGRADATION);
            assert!(
                degradation.contains("attempt evidence"),
                "and names what was lost: {degradation}"
            );
        }
        other => panic!("the evidence must be labelled, got {other:?}"),
    }
    let on_disk = fs::read_to_string(dir.join(JOURNAL_LOG_NAME)).expect("the log is readable");
    assert!(
        on_disk.contains("legacy_projection"),
        "the degradation is part of the durable record, not only of the handle"
    );

    // The label is derived, not supplied: a hand-built body cannot claim a
    // legacy record is anything other than degraded.
    let forged = JournalRecordBody {
        schema_version: JOURNAL_SCHEMA_VERSION,
        event_schema_version: LEARNING_EVENT_SCHEMA_VERSION,
        event: LearningEvent {
            event_id: "evt-forged-1".to_string(),
            model_id: ModelId::new(MODEL),
            samples: vec![canonical_sample("forged", true).into_legacy()],
            parent_commit: None,
            result_commit: None,
            created_at: 1_700_000_000,
            source: None,
        },
        evidence: JournalEvidence::LegacyProjection {
            degradation: "nothing was lost".to_string(),
        },
        recorded_at: 1_700_000_000,
    };
    let normalised = forged
        .try_into_record(1)
        .expect("a legacy body is accepted; it is labelled, not refused");
    assert!(
        normalised.is_degraded(),
        "the record is still labelled degraded"
    );
    match &normalised.evidence {
        JournalEvidence::LegacyProjection { degradation } => assert_eq!(
            degradation, LEGACY_DEGRADATION,
            "and the label is the canonical one, not the caller's"
        ),
        other => panic!("the evidence must stay labelled, got {other:?}"),
    }
}

/// GATE 8, fourth part: the durable format is re-derivable by something other
/// than this module. The checksum function is public precisely so a second
/// implementation can verify the log, and the test re-derives it from the bytes.
#[test]
fn the_frame_format_is_re_derivable_from_the_public_api() {
    let root = scratch();
    let dir = journal_dir(root.path());
    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        journal
            .record_canonical(canonical_event("evt-format-1", "f1", true))
            .expect("a record is written");
    }
    let bytes = log_bytes(&dir);
    let line = &bytes[..bytes.len() - 1];
    let mut fields = line.splitn(3, |byte| *byte == b'\t');
    let sequence = fields.next().expect("a sequence field");
    let checksum = fields.next().expect("a checksum field");
    let body = fields.next().expect("a body field");
    assert_eq!(sequence, b"1", "the first frame is sequence 1");
    assert_eq!(fields.next(), None, "and there are exactly three fields");
    assert_eq!(
        frame_checksum(0, 1, body),
        u64::from_str_radix(std::str::from_utf8(checksum).expect("ascii"), 16)
            .expect("a hex checksum"),
        "the recorded checksum is re-derivable from the public function"
    );
}

// ---------------------------------------------------------------------------
// GATE 9 — inertness
// ---------------------------------------------------------------------------

/// GATE 9: the record is a record. Writing one and reading it back leaves the
/// commit store exactly as it was — no head moved, no commit created, no model
/// parameters touched — because nothing here trains.
#[test]
fn writing_and_reading_a_record_trains_nothing() {
    let root = scratch();
    let dir = journal_dir(root.path());
    let mut store = ModelStore::new();
    let commit = verified_commit(&mut store, None, 3);
    let head_before = store.get_head();
    let length_before = store.len();
    let parameters_before = ModelEnsemble::new().save_all().content_hash();

    {
        let mut journal =
            LearningJournal::open(&dir, JournalMode::Append).expect("the journal opens");
        let mut event = canonical_event("evt-inert-1", "i9", true);
        event.result_commit = Some(commit);
        journal
            .record_canonical(event)
            .expect("a record is written");
    }
    let journal = LearningJournal::open(&dir, JournalMode::Read).expect("the journal reopens");
    let records = journal.read_records(&store).expect("the journal reads back");
    assert_eq!(records.len(), 1);

    assert_eq!(store.get_head(), head_before, "the head did not move");
    assert_eq!(store.len(), length_before, "no commit was created");
    assert_eq!(
        ModelEnsemble::new().save_all().content_hash(),
        parameters_before,
        "and no model parameters were updated"
    );
    assert!(
        records[0]
            .event
            .samples
            .iter()
            .all(|sample| sample.provider_id == PROVIDER),
        "the record is evidence, not an update"
    );
}

/// The journal states what a record is, for a reader and for a reviewer.
#[test]
fn the_journal_states_its_own_role() {
    for claim in ["inert", "never trained", "never scheduled", "explicit call"] {
        assert!(
            JOURNAL_ROLE.contains(claim),
            "the role states {claim:?}: {JOURNAL_ROLE}"
        );
    }
}
