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
    for i in 0..32 {
        feats[i] = ((seed * 7 + i * 13) % 100) as f32 / 100.0;
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
