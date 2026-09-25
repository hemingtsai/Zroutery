#![cfg(feature = "ml")]

use zroutery_core::ml::model_identity::*;
use zroutery_core::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model::RoutingModel;
use zroutery_core::feedback::DataOrigin;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_sample(success: bool, seed: usize) -> DatasetTrainingSample {
    let mut feats = [0.0f32; 32];
    for (i, item) in feats.iter_mut().enumerate() {
        *item = ((seed * 7 + i * 13) % 100) as f32 / 100.0;
    }
    DatasetTrainingSample {
        sample_id: format!("s-{}", seed),
        schema_version: 1,
        timestamp: 1000 + seed as i64,
        features: RoutingFeatures {
            values: feats,
            schema_version: 1,
        },
        targets: Targets {
            success,
            latency_ms: if success { Some(200.0 + seed as f64) } else { None },
            ttft_ms: if success { Some(50.0 + seed as f64) } else { None },
            cost: Some(0.01 + seed as f64 * 0.001),
            failure_class: None,
            fallback_count: 0,
        },
        provider_id: "p".into(),
        model_id: "m".into(),
        origin: DataOrigin::Native,
        outcome_id: format!("o-{}", seed),
        feedback: vec![],
    }
}

fn make_events(n: usize) -> Vec<LearningEvent> {
    (0..n)
        .map(|i| {
            LearningEvent::new(
                ModelId::new("ensemble"),
                vec![test_sample(i % 2 == 0, i)],
                None,
                Some("test".into()),
            )
        })
        .collect()
}

fn checkpoint_checksum(cp: &ModelCheckpoint) -> u64 {
    cp.content_hash()
}

fn train_ensemble_from_samples(samples: &[DatasetTrainingSample]) -> ModelEnsemble {
    let mut ensemble = ModelEnsemble::new();
    for s in samples {
        ensemble.update_all(s);
    }
    ensemble
}

// ---------------------------------------------------------------------------
// Gate E0 Integration Tests
// ---------------------------------------------------------------------------

/// GATE E0-1: train ensemble -> save -> reset -> load -> predict same features -> <1e-10 delta
#[test]
fn gate_e0_save_restore_predict() {
    let samples: Vec<_> = (0..20).map(|i| test_sample(i % 2 == 0, i)).collect();
    let mut ensemble = train_ensemble_from_samples(&samples);
    let features = &samples[0].features;

    let pred_before = ensemble.success.predict(features);
    let pred_lat_before = ensemble.latency.predict(features);
    let cp = ensemble.save_all();

    // Reset and reload
    ensemble.reset_all();
    assert_eq!(ensemble.success.sample_count(), 0);

    let loaded = ModelEnsemble::load_all(&cp).unwrap();
    let pred_after = loaded.success.predict(features);
    let pred_lat_after = loaded.latency.predict(features);

    assert!(
        (pred_before.value - pred_after.value).abs() < 1e-10,
        "prediction delta too large: before={}, after={}",
        pred_before.value,
        pred_after.value
    );

    // Also check latency, ttft, cost models
    assert!(
        (pred_lat_before.value - pred_lat_after.value).abs() < 1e-10,
        "latency prediction delta too large: before={}, after={}",
        pred_lat_before.value,
        pred_lat_after.value
    );
}

/// GATE E0-2: create events -> train -> save checksums -> reset -> replay -> same checksums
#[test]
fn gate_e0_replay_determinism() {
    let events = make_events(10);
    let cp1 = ReplayEngine::replay(&events, None).unwrap();
    let cp2 = ReplayEngine::replay(&events, None).unwrap();
    assert_eq!(
        checkpoint_checksum(&cp1),
        checkpoint_checksum(&cp2),
        "replay must be deterministic"
    );
    assert!(cp1.verify());
    assert!(cp2.verify());
}

/// GATE E0-3: E1..5 then E6..10 vs E1..10 from scratch -> same final checksums
#[test]
fn gate_e0_replay_composition() {
    let events = make_events(10);

    // Full replay from scratch
    let full = ReplayEngine::replay(&events, None).unwrap();

    // Two-stage composition
    let stage1 = ReplayEngine::replay(&events[..5], None).unwrap();
    let stage2 = ReplayEngine::replay(&events[5..], Some(&stage1)).unwrap();

    assert_eq!(
        checkpoint_checksum(&full),
        checkpoint_checksum(&stage2),
        "composition property violated: full != replay(replay(E1..5), E6..10)"
    );
}

/// GATE E0-4: 3 commits verify parent chain
#[test]
fn gate_e0_commit_lineage() {
    let mut store = ModelStore::new();

    let cp0 = ModelEnsemble::new().save_all();
    let id0 = store.commit(cp0, "init".into());

    let mut ens1 = ModelEnsemble::new();
    ens1.update_all(&test_sample(true, 0));
    let cp1 = ens1.save_all();
    let id1 = store.commit(cp1, "second".into());

    let mut ens2 = ModelEnsemble::new();
    for i in 0..5 {
        ens2.update_all(&test_sample(true, i));
    }
    let cp2 = ens2.save_all();
    let id2 = store.commit(cp2, "third".into());

    let log = store.log();
    assert_eq!(log.len(), 3);
    // Parent chain
    assert!(log[2].parent.is_none()); // first commit
    assert_eq!(log[1].parent, Some(id0));
    assert_eq!(log[0].parent, Some(id1));
    assert_eq!(log[0].commit_id, id2);
}

/// GATE E0-5: commit -> checkout -> load -> predict matches pre-commit
#[test]
fn gate_e0_checkout_load_predict() {
    let samples: Vec<_> = (0..15).map(|i| test_sample(i % 2 == 0, i)).collect();
    let ensemble = train_ensemble_from_samples(&samples);
    let features = &samples[0].features;

    let pred_before = ensemble.success.predict(features);
    let cp = ensemble.save_all();

    let mut store = ModelStore::new();
    let id = store.commit(cp, "test".into());

    let checked_out = store.checkout(&id).unwrap();
    let loaded = ModelEnsemble::load_all(&checked_out).unwrap();
    let pred_after = loaded.success.predict(features);

    assert!(
        (pred_before.value - pred_after.value).abs() < 1e-10,
        "predict after checkout/load differs: before={}, after={}",
        pred_before.value,
        pred_after.value
    );
}

/// GATE E0-6: tag v1 -> new commit -> checkout v1 -> same checkpoint
#[test]
fn gate_e0_tag_checkout() {
    let mut store = ModelStore::new();

    let cp1 = ModelEnsemble::new().save_all();
    let id1 = store.commit(cp1.clone(), "v1".into());
    store.tag(&id1, "v1").unwrap();

    // Make a new commit on top
    let mut ens = ModelEnsemble::new();
    ens.update_all(&test_sample(true, 0));
    let cp2 = ens.save_all();
    store.commit(cp2, "v2".into());

    // Checkout the tagged version
    let resolved = store.resolve_tag("v1").unwrap();
    let checked_out = store.checkout(resolved).unwrap();
    assert_eq!(
        checkpoint_checksum(&cp1),
        checkpoint_checksum(&checked_out),
        "tag v1 should resolve to original checkpoint"
    );
}

/// GATE E0-7: replay twice independently -> identical checksums
#[test]
fn gate_e0_determinism_across_restarts() {
    let events = make_events(10);

    // First independent replay
    let cp1 = ReplayEngine::replay(&events, None).unwrap();
    let chk1 = checkpoint_checksum(&cp1);

    // Second independent replay (simulating a restart)
    let cp2 = ReplayEngine::replay(&events, None).unwrap();
    let chk2 = checkpoint_checksum(&cp2);

    assert_eq!(chk1, chk2, "determinism across restarts violated");
    assert!(cp1.verify());
    assert!(cp2.verify());
}

/// GATE E0-8: train 50 samples -> save -> commit -> tag -> reset -> checkout tag -> load -> predict -> match
#[test]
fn gate_e0_full_lifecycle() {
    let samples: Vec<_> = (0..50).map(|i| test_sample(true, i)).collect();
    let mut ensemble = train_ensemble_from_samples(&samples);
    let features = &samples[0].features;

    // Record prediction from trained ensemble
    let pred_trained = ensemble.success.predict(features);

    // Save -> commit -> tag
    let cp = ensemble.save_all();
    let mut store = ModelStore::new();
    let id = store.commit(cp, "trained-v1".into());
    store.tag(&id, "v1").unwrap();

    // Reset ensemble to cold
    ensemble.reset_all();
    let pred_cold = ensemble.success.predict(features);
    assert!(
        (pred_trained.value - pred_cold.value).abs() > 0.01,
        "reset should produce different prediction"
    );

    // Checkout tag -> load -> predict
    let resolved = store.resolve_tag("v1").unwrap();
    let checked_out = store.checkout(resolved).unwrap();
    let loaded = ModelEnsemble::load_all(&checked_out).unwrap();
    let pred_restored = loaded.success.predict(features);

    assert!(
        (pred_trained.value - pred_restored.value).abs() < 1e-10,
        "full lifecycle prediction mismatch: trained={}, restored={}",
        pred_trained.value,
        pred_restored.value
    );

    // Verify the checkpoint
    assert!(checked_out.verify());
    assert_eq!(checked_out.feature_schema_version, FEATURE_SCHEMA_VERSION);
}

// ---------------------------------------------------------------------------
// Gate 7E-1A — verified identity, lineage, and replay
// ---------------------------------------------------------------------------

#[test]
fn gate_7e1a_canonical_identity_includes_model_schema_parent_and_lineage() {
    let checkpoint = ModelEnsemble::new().save_all();
    let root = ModelCommit::new(ModelId::new("ensemble-a"), checkpoint.clone(), None, 0);
    let other_model = ModelCommit::new(ModelId::new("ensemble-b"), checkpoint.clone(), None, 0);
    assert_ne!(root.commit_id, other_model.commit_id);

    let parent = ModelCommit::new(ModelId::new("ensemble-a"), checkpoint.clone(), None, 0);
    let child = ModelCommit::new(
        ModelId::new("ensemble-a"),
        checkpoint.clone(),
        Some(parent.commit_id.clone()),
        1,
    );
    assert_ne!(root.commit_id, child.commit_id);

    let longer_lineage = ModelCommit::new(
        ModelId::new("ensemble-a"),
        checkpoint,
        Some(parent.commit_id),
        2,
    );
    assert_ne!(child.commit_id, longer_lineage.commit_id);

    let tagged_a = ModelCommit::new_with_lineage(
        ModelId::new("ensemble-a"),
        ModelEnsemble::new().save_all(),
        None,
        0,
        11,
    );
    let tagged_b = ModelCommit::new_with_lineage(
        ModelId::new("ensemble-a"),
        ModelEnsemble::new().save_all(),
        None,
        0,
        12,
    );
    assert_ne!(tagged_a.commit_id, tagged_b.commit_id);
    assert!(tagged_a.verify() && tagged_b.verify());

    let mut other_schema = ModelEnsemble::new().save_all();
    other_schema.feature_schema_version = FEATURE_SCHEMA_VERSION + 1;
    let schema_commit = ModelCommit::new(ModelId::new("ensemble-a"), other_schema, None, 0);
    assert_ne!(root.commit_id, schema_commit.commit_id);
    assert!(!schema_commit.verify());
}

#[test]
fn gate_7e1a_model_store_uses_content_addressed_ids() {
    let checkpoint = ModelEnsemble::new().save_all();
    let expected = ModelCommit::new(ModelId::new("ensemble"), checkpoint.clone(), None, 0);
    let mut store = ModelStore::new();
    let id = store.try_commit(checkpoint.clone(), "root".into()).unwrap();
    assert_eq!(id, expected.commit_id);
    assert!(!id.as_str().starts_with("cmt-"));
    assert!(store.checkout_commit(&id).unwrap().verify());

    let child = store.try_commit(checkpoint, "child".into()).unwrap();
    assert_ne!(child, id);
    assert_eq!(store.log_checked().unwrap().len(), 2);
}

#[test]
fn gate_7e1a_replay_composition_and_commit_determinism() {
    let events = make_events(6);
    let full = ReplayEngine::replay_commit(&events, None).unwrap();
    let first = ReplayEngine::replay_commit(&events[..3], None).unwrap();
    let staged = ReplayEngine::replay_commit(&events[3..], Some(&first)).unwrap();
    assert_eq!(full.commit_id, staged.commit_id);
    assert_eq!(full.checkpoint.content_hash(), staged.checkpoint.content_hash());

    let repeated = ReplayEngine::replay_commit(&events, None).unwrap();
    assert_eq!(full.commit_id, repeated.commit_id);
    assert!(full.verify());
}

#[test]
fn gate_7e1a_replay_rejects_wrong_model_parent_result_and_order() {
    let mut wrong_model = make_events(2);
    wrong_model[1].model_id = ModelId::new("other-model");
    assert!(matches!(
        ReplayEngine::replay_commit(&wrong_model, None),
        Err(ReplayError::EventModelMismatch { .. })
    ));

    let mut wrong_parent = make_events(1);
    wrong_parent[0].parent_commit = Some(CommitId::new("deadbeef"));
    assert!(matches!(
        ReplayEngine::replay_commit(&wrong_parent, None),
        Err(ReplayError::EventParentMismatch { .. })
    ));

    let mut wrong_result = make_events(1);
    let expected = ReplayEngine::replay_commit(&wrong_result, None).unwrap();
    wrong_result[0].result_commit = Some(CommitId::new("0123456789abcdef"));
    assert_ne!(wrong_result[0].result_commit.as_ref(), Some(&expected.commit_id));
    assert!(matches!(
        ReplayEngine::replay_commit(&wrong_result, None),
        Err(ReplayError::EventResultMismatch { .. })
    ));

    let mut reversed = make_events(2);
    reversed.reverse();
    assert!(matches!(
        ReplayEngine::replay_commit(&reversed, None),
        Err(ReplayError::EventOutOfOrder { .. })
    ));
}

#[test]
fn gate_7e1a_corrupt_checkpoint_event_and_lineage_fail_closed() {
    let events = make_events(1);
    let mut corrupt_checkpoint = ModelEnsemble::new().save_all();
    corrupt_checkpoint.success.parameters[0] = 12345.0;
    assert!(matches!(
        ReplayEngine::replay(&events, Some(&corrupt_checkpoint)),
        Err(ReplayError::ChecksumMismatch { .. })
    ));

    let mut wrong_algorithm = ModelEnsemble::new().save_all();
    wrong_algorithm.latency.algorithm = "unknown_algorithm".to_string();
    assert!(!wrong_algorithm.verify());

    let mut corrupt_event = make_events(1);
    corrupt_event[0].samples[0].features.values[0] = f32::NAN;
    assert!(matches!(
        ReplayEngine::replay_commit(&corrupt_event, None),
        Err(ReplayError::InvalidEvent { .. })
    ));

    let mut corrupt_target = make_events(1);
    corrupt_target[0].samples[0].targets.cost = Some(f64::NAN);
    assert!(matches!(
        ReplayEngine::replay_commit(&corrupt_target, None),
        Err(ReplayError::InvalidEvent { .. })
    ));

    let root = ModelCommit::new(
        ModelId::new("ensemble"),
        ModelEnsemble::new().save_all(),
        None,
        0,
    );
    let mut store = ModelStore::new();
    store.try_insert_commit(root.clone(), "root".into()).unwrap();
    let mut child = ModelCommit::new(
        ModelId::new("ensemble"),
        ModelEnsemble::new().save_all(),
        Some(root.commit_id.clone()),
        1,
    );
    child.parent = Some(CommitId::new("aaaaaaaaaaaaaaaa"));
    assert!(matches!(
        store.try_insert_commit(child, "tampered".into()),
        Err(ReplayError::InvalidCommit { .. })
    ));

    let orphan = ModelCommit::new(
        ModelId::new("ensemble"),
        ModelEnsemble::new().save_all(),
        Some(CommitId::new("0123456789abcdef")),
        1,
    );
    assert!(matches!(
        store.try_insert_commit(orphan, "orphan".into()),
        Err(ReplayError::LineageCorrupt { .. })
    ));
}

#[test]
fn gate_7e1a_unknown_schema_versions_are_not_reinterpreted() {
    let mut checkpoint = ModelEnsemble::new().save_all();
    checkpoint.feature_schema_version = FEATURE_SCHEMA_VERSION + 1;
    let events = make_events(1);
    assert!(matches!(
        ReplayEngine::replay(&events, Some(&checkpoint)),
        Err(ReplayError::UnsupportedSchema { .. })
    ));

    let mut event = make_events(1);
    event[0].samples[0].schema_version = FEATURE_SCHEMA_VERSION + 1;
    assert!(matches!(
        ReplayEngine::replay_commit(&event, None),
        Err(ReplayError::UnsupportedSchema { .. })
    ));
}

#[test]
fn gate_7e1a_strict_replay_requires_explicit_result_lineage() {
    let events = make_events(1);
    assert!(matches!(
        ReplayEngine::replay_verified(&events, None),
        Err(ReplayError::EventResultMissing { .. })
    ));
}

#[test]
fn gate_7e1a_verified_replay_accepts_explicit_parent_result_chain() {
    let mut events = make_events(2);
    let first = ReplayEngine::replay_commit(&events[..1], None).unwrap();
    events[0].result_commit = Some(first.commit_id.clone());
    let second = ReplayEngine::replay_commit(&events[1..], Some(&first)).unwrap();
    events[1].parent_commit = Some(first.commit_id.clone());
    events[1].result_commit = Some(second.commit_id.clone());

    let replayed = ReplayEngine::replay_verified(&events, None).unwrap();
    assert_eq!(replayed.commit_id, second.commit_id);
    assert!(replayed.verify());
}

#[test]
fn gate_7e1a_volatile_event_metadata_does_not_change_commit() {
    let first = ReplayEngine::replay_commit(&make_events(4), None).unwrap();
    let second = ReplayEngine::replay_commit(&make_events(4), None).unwrap();
    assert_eq!(first.commit_id, second.commit_id);
    assert_eq!(first.checkpoint.content_hash(), second.checkpoint.content_hash());
}

#[test]
fn gate_7e1a_envelope_versions_are_explicit_and_legacy_is_explicitly_migrated() {
    let checkpoint = ModelEnsemble::new().save_all();
    let mut checkpoint_json = serde_json::to_value(&checkpoint).unwrap();
    checkpoint_json
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    let legacy_checkpoint: ModelCheckpoint = serde_json::from_value(checkpoint_json).unwrap();
    assert_eq!(
        legacy_checkpoint.schema_version,
        LEGACY_UNVERSIONED_SCHEMA_VERSION
    );
    assert!(!legacy_checkpoint.verify());
    let migrated_checkpoint = legacy_checkpoint.migrate_legacy().unwrap();
    assert_eq!(
        migrated_checkpoint.schema_version,
        MODEL_CHECKPOINT_SCHEMA_VERSION
    );
    assert!(migrated_checkpoint.verify());

    let mut unknown_checkpoint = serde_json::to_value(&checkpoint).unwrap();
    unknown_checkpoint["schema_version"] = serde_json::Value::from(99_u32);
    assert!(serde_json::from_value::<ModelCheckpoint>(unknown_checkpoint).is_err());

    let commit = ModelCommit::new(
        ModelId::new("ensemble"),
        checkpoint.clone(),
        None,
        0,
    );
    let mut legacy_commit_json = serde_json::to_value(&commit).unwrap();
    legacy_commit_json
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    let mut legacy_checkpoint_json = serde_json::to_value(&checkpoint).unwrap();
    legacy_checkpoint_json
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    legacy_commit_json["checkpoint"] = legacy_checkpoint_json;
    let mut legacy_commit: ModelCommit = serde_json::from_value(legacy_commit_json).unwrap();
    assert_eq!(legacy_commit.schema_version, LEGACY_UNVERSIONED_SCHEMA_VERSION);
    assert!(!legacy_commit.verify());
    legacy_commit.commit_id = CommitId::from_hash(legacy_commit.checkpoint.content_hash());
    let migrated_commit = legacy_commit.migrate_legacy().unwrap();
    assert!(migrated_commit.verify());
    assert_eq!(
        migrated_commit.schema_version,
        MODEL_COMMIT_SCHEMA_VERSION
    );

    let mut unknown_commit = serde_json::to_value(&commit).unwrap();
    unknown_commit["schema_version"] = serde_json::Value::from(99_u32);
    assert!(serde_json::from_value::<ModelCommit>(unknown_commit).is_err());

    let mut wrong_envelope = commit.clone();
    wrong_envelope.schema_version = LEGACY_UNVERSIONED_SCHEMA_VERSION;
    assert_ne!(wrong_envelope.canonical_identity(), commit.commit_id);
    assert!(!wrong_envelope.verify());
}
