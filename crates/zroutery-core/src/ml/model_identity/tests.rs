// ---------------------------------------------------------------------------
// Tests (stub — populated by test agents)
// ---------------------------------------------------------------------------

use super::*;
use crate::feedback::DataOrigin;
use crate::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
use crate::ml::model::RoutingModel;

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

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
            latency_ms: if success {
                Some(200.0 + seed as f64)
            } else {
                None
            },
            ttft_ms: if success {
                Some(50.0 + seed as f64)
            } else {
                None
            },
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

// -----------------------------------------------------------------------
// ModelId tests (4)
// -----------------------------------------------------------------------

#[test]
fn model_id_construction() {
    assert_eq!(ModelId::success().as_str(), "success");
    assert_eq!(ModelId::latency().as_str(), "latency");
    assert_eq!(ModelId::ttft().as_str(), "ttft");
    assert_eq!(ModelId::cost().as_str(), "cost");
    assert_eq!(ModelId::new("custom").as_str(), "custom");
}

#[test]
fn model_id_display() {
    let id = ModelId::new("test_model");
    assert_eq!(format!("{}", id), "test_model");
    assert_eq!(format!("{}", id), id.as_str());
}

#[test]
fn model_id_equality() {
    let a = ModelId::new("same");
    let b = ModelId::new("same");
    let c = ModelId::new("different");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn model_id_serde_round_trip() {
    let id = ModelId::new("serde_test");
    let json = serde_json::to_string(&id).unwrap();
    let restored: ModelId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, restored);
}

// -----------------------------------------------------------------------
// CommitId tests (3)
// -----------------------------------------------------------------------

#[test]
fn commit_id_from_hash_deterministic() {
    let a = CommitId::from_hash(42);
    let b = CommitId::from_hash(42);
    assert_eq!(a, b);
    assert_eq!(a.as_str(), "000000000000002a");
}

#[test]
fn commit_id_display() {
    let id = CommitId::from_hash(0xff);
    let s = format!("{}", id);
    assert_eq!(s, "00000000000000ff");
}

#[test]
fn commit_id_serde_round_trip() {
    let id = CommitId::from_hash(12345);
    let json = serde_json::to_string(&id).unwrap();
    let restored: CommitId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, restored);
}

// -----------------------------------------------------------------------
// ModelCheckpoint tests (5)
// -----------------------------------------------------------------------

#[test]
fn checkpoint_creation_from_ensemble() {
    let ensemble = ModelEnsemble::new();
    let cp = ensemble.save_all();
    assert!(cp.verify());
    assert_eq!(cp.feature_schema_version, FEATURE_SCHEMA_VERSION);
}

#[test]
fn checkpoint_content_hash_deterministic() {
    let ensemble = ModelEnsemble::new();
    let cp = ensemble.save_all();
    let h1 = cp.content_hash();
    let h2 = cp.content_hash();
    assert_eq!(h1, h2);
}

#[test]
fn checkpoint_content_hash_differs() {
    let cp1 = ModelEnsemble::new().save_all();
    let mut ensemble2 = ModelEnsemble::new();
    for i in 0..10 {
        ensemble2.update_all(&test_sample(true, i));
    }
    let cp2 = ensemble2.save_all();
    assert_ne!(cp1.content_hash(), cp2.content_hash());
}

#[test]
fn checkpoint_verify_passes() {
    let ensemble = ModelEnsemble::new();
    let cp = ensemble.save_all();
    assert!(cp.verify());
}

#[test]
fn checkpoint_serde_round_trip() {
    let cp = ModelEnsemble::new().save_all();
    let json = serde_json::to_string(&cp).unwrap();
    let restored: ModelCheckpoint = serde_json::from_str(&json).unwrap();
    assert!(restored.verify());
    assert_eq!(cp.content_hash(), restored.content_hash());
}

// -----------------------------------------------------------------------
// ModelCommit tests (4)
// -----------------------------------------------------------------------

#[test]
fn commit_creation() {
    let cp = ModelEnsemble::new().save_all();
    let commit = ModelCommit::new(ModelId::new("test"), cp.clone(), None, 0);
    assert_eq!(commit.commit_id, commit.canonical_identity());
    assert!(commit.verify());
}

#[test]
fn commit_verify() {
    let mut ensemble = ModelEnsemble::new();
    for i in 0..5 {
        ensemble.update_all(&test_sample(true, i));
    }
    let cp = ensemble.save_all();
    let commit = ModelCommit::new(ModelId::new("test"), cp, None, 5);
    assert!(commit.verify());
}

#[test]
fn commit_parent_chain() {
    let cp1 = ModelEnsemble::new().save_all();
    let c1 = ModelCommit::new(ModelId::new("test"), cp1, None, 0);

    let mut ensemble2 = ModelEnsemble::new();
    ensemble2.update_all(&test_sample(true, 0));
    let cp2 = ensemble2.save_all();
    let c2 = ModelCommit::new(ModelId::new("test"), cp2, Some(c1.commit_id.clone()), 1);

    assert_eq!(c2.parent, Some(c1.commit_id));
}

#[test]
fn commit_different_checkpoints_different_ids() {
    let cp1 = ModelEnsemble::new().save_all();
    let mut ensemble2 = ModelEnsemble::new();
    ensemble2.update_all(&test_sample(true, 0));
    let cp2 = ensemble2.save_all();
    let c1 = ModelCommit::new(ModelId::new("m"), cp1, None, 0);
    let c2 = ModelCommit::new(ModelId::new("m"), cp2, None, 1);
    assert_ne!(c1.commit_id, c2.commit_id);
}

// -----------------------------------------------------------------------
// ModelRef tests (2)
// -----------------------------------------------------------------------

#[test]
fn model_ref_from_commit() {
    let cp = ModelEnsemble::new().save_all();
    let commit = ModelCommit::new(ModelId::new("test"), cp, None, 0);
    let r = ModelRef::from_commit(&commit, Some("v1".into()));
    assert_eq!(r.commit_id, commit.commit_id);
    assert_eq!(r.model_id, commit.model_id);
    assert_eq!(r.tag, Some("v1".to_string()));
}

#[test]
fn model_ref_serde_round_trip() {
    let cp = ModelEnsemble::new().save_all();
    let commit = ModelCommit::new(ModelId::new("test"), cp, None, 0);
    let r = ModelRef::from_commit(&commit, None);
    let json = serde_json::to_string(&r).unwrap();
    let restored: ModelRef = serde_json::from_str(&json).unwrap();
    assert_eq!(r.commit_id, restored.commit_id);
    assert_eq!(r.model_id, restored.model_id);
}

// -----------------------------------------------------------------------
// LearningEvent tests (4)
// -----------------------------------------------------------------------

#[test]
fn learning_event_creation() {
    let samples = vec![test_sample(true, 0), test_sample(false, 1)];
    let event = LearningEvent::new(
        ModelId::new("test"),
        samples.clone(),
        None,
        Some("test".into()),
    );
    assert_eq!(event.model_id.as_str(), "test");
    assert_eq!(event.samples.len(), 2);
    assert_eq!(event.source, Some("test".to_string()));
    assert!(event.result_commit.is_none());
}

#[test]
fn learning_event_sample_count() {
    let event = LearningEvent::new(
        ModelId::new("test"),
        vec![
            test_sample(true, 0),
            test_sample(true, 1),
            test_sample(false, 2),
        ],
        None,
        None,
    );
    assert_eq!(event.sample_count(), 3);
}

#[test]
fn learning_event_is_applied() {
    let mut event =
        LearningEvent::new(ModelId::new("test"), vec![test_sample(true, 0)], None, None);
    assert!(!event.is_applied());
    event.mark_applied(CommitId::from_hash(42));
    assert!(event.is_applied());
}

#[test]
fn learning_event_ids_unique() {
    let e1 = LearningEvent::new(ModelId::new("test"), vec![test_sample(true, 0)], None, None);
    let e2 = LearningEvent::new(ModelId::new("test"), vec![test_sample(true, 0)], None, None);
    assert_ne!(e1.event_id, e2.event_id);
}

// -----------------------------------------------------------------------
// ModelEnsemble tests (8)
// -----------------------------------------------------------------------

#[test]
fn ensemble_new_cold_start() {
    let ensemble = ModelEnsemble::new();
    assert_eq!(ensemble.success.sample_count(), 0);
    assert_eq!(ensemble.latency.sample_count(), 0);
    assert_eq!(ensemble.ttft.sample_count(), 0);
    assert_eq!(ensemble.cost.sample_count(), 0);
}

#[test]
fn ensemble_save_all_checkpoint() {
    let ensemble = ModelEnsemble::new();
    let cp = ensemble.save_all();
    assert!(cp.verify());
    assert_eq!(cp.feature_schema_version, FEATURE_SCHEMA_VERSION);
}

#[test]
fn ensemble_load_all_round_trip() {
    let mut ensemble = ModelEnsemble::new();
    for i in 0..20 {
        ensemble.update_all(&test_sample(i % 2 == 0, i));
    }
    let cp = ensemble.save_all();
    let loaded = ModelEnsemble::load_all(&cp).unwrap();

    let features = &test_sample(true, 99).features;
    let p_orig = ensemble.success.predict(features);
    let p_loaded = loaded.success.predict(features);
    assert!(
        (p_orig.value - p_loaded.value).abs() < 1e-10,
        "success predictions differ: orig={}, loaded={}",
        p_orig.value,
        p_loaded.value
    );
}

#[test]
fn ensemble_reset_all() {
    let mut ensemble = ModelEnsemble::new();
    for i in 0..10 {
        ensemble.update_all(&test_sample(true, i));
    }
    assert!(ensemble.success.sample_count() > 0);
    ensemble.reset_all();
    assert_eq!(ensemble.success.sample_count(), 0);
    assert_eq!(ensemble.latency.sample_count(), 0);
    assert_eq!(ensemble.ttft.sample_count(), 0);
    assert_eq!(ensemble.cost.sample_count(), 0);
}

#[test]
fn ensemble_update_all_success() {
    let mut ensemble = ModelEnsemble::new();
    let sample = test_sample(true, 0);
    ensemble.update_all(&sample);
    assert_eq!(ensemble.success.sample_count(), 1);
    assert_eq!(ensemble.latency.sample_count(), 1);
    assert_eq!(ensemble.ttft.sample_count(), 1);
    assert_eq!(ensemble.cost.sample_count(), 1);
}

#[test]
fn ensemble_update_all_failure() {
    let mut ensemble = ModelEnsemble::new();
    let sample = test_sample(false, 0); // failure: latency_ms=None, ttft_ms=None
    ensemble.update_all(&sample);
    assert_eq!(ensemble.success.sample_count(), 1); // success model always trained
    assert_eq!(ensemble.latency.sample_count(), 0); // latency not trained (None)
    assert_eq!(ensemble.ttft.sample_count(), 0); // ttft not trained (None)
    assert_eq!(ensemble.cost.sample_count(), 1); // cost trained (Some)
}

#[test]
fn ensemble_update_all_partial() {
    let mut ensemble = ModelEnsemble::new();
    let mut sample = test_sample(true, 0);
    sample.targets.latency_ms = None; // latency_ms missing
    ensemble.update_all(&sample);
    assert_eq!(ensemble.success.sample_count(), 1);
    assert_eq!(ensemble.latency.sample_count(), 0); // not updated
    assert_eq!(ensemble.ttft.sample_count(), 1);
    assert_eq!(ensemble.cost.sample_count(), 1);
}

#[test]
fn ensemble_double_save_idempotent() {
    let mut ensemble = ModelEnsemble::new();
    for i in 0..10 {
        ensemble.update_all(&test_sample(true, i));
    }
    let cp1 = ensemble.save_all();
    let cp2 = ensemble.save_all();
    assert_eq!(cp1.content_hash(), cp2.content_hash());
}

// -----------------------------------------------------------------------
// ReplayEngine tests (12)
// -----------------------------------------------------------------------

#[test]
fn replay_determinism_same_events() {
    let events = make_events(10);
    let cp1 = ReplayEngine::replay(&events, None).unwrap();
    let cp2 = ReplayEngine::replay(&events, None).unwrap();
    assert_eq!(cp1.content_hash(), cp2.content_hash());
    assert!(cp1.verify());
    assert!(cp2.verify());
}

#[test]
fn replay_cold_start_no_base() {
    let events = make_events(3);
    let cp = ReplayEngine::replay(&events, None).unwrap();
    assert!(cp.verify());
    // Should have trained 3 samples
    let ensemble = ModelEnsemble::load_all(&cp).unwrap();
    assert_eq!(ensemble.success.sample_count(), 3);
}

#[test]
fn replay_with_base_checkpoint() {
    let base_events = make_events(5);
    let base = ReplayEngine::replay(&base_events, None).unwrap();

    let more_events = make_events(3);
    let cp = ReplayEngine::replay(&more_events, Some(&base)).unwrap();
    assert!(cp.verify());
    let ensemble = ModelEnsemble::load_all(&cp).unwrap();
    assert_eq!(ensemble.success.sample_count(), 8); // 5 + 3
}

#[test]
fn replay_composition_property() {
    let events = make_events(10);

    // Full replay
    let full = ReplayEngine::replay(&events, None).unwrap();

    // Two-stage replay
    let stage1 = ReplayEngine::replay(&events[..5], None).unwrap();
    let stage2 = ReplayEngine::replay(&events[5..], Some(&stage1)).unwrap();

    assert_eq!(full.content_hash(), stage2.content_hash());
}

#[test]
fn replay_empty_events_error() {
    let result = ReplayEngine::replay(&[], None);
    assert!(matches!(result, Err(ReplayError::EmptyEventList)));
}

#[test]
fn replay_empty_events_with_base_error() {
    let base = ModelEnsemble::new().save_all();
    let result = ReplayEngine::replay(&[], Some(&base));
    assert!(matches!(result, Err(ReplayError::EmptyEventList)));
}

#[test]
fn replay_preserves_sample_counts() {
    let events = make_events(7);
    let cp = ReplayEngine::replay(&events, None).unwrap();
    let ensemble = ModelEnsemble::load_all(&cp).unwrap();
    // Each event has 1 sample, 7 events, alternating success/failure
    // success model always updated => 7
    assert_eq!(ensemble.success.sample_count(), 7);
}

#[test]
fn replay_checkpoint_schema_version() {
    let events = make_events(3);
    let cp = ReplayEngine::replay(&events, None).unwrap();
    assert_eq!(cp.feature_schema_version, FEATURE_SCHEMA_VERSION);
}

#[test]
fn replay_checkpoint_verify() {
    let events = make_events(5);
    let cp = ReplayEngine::replay(&events, None).unwrap();
    assert!(cp.verify());
}

#[test]
fn replay_event_order_matters() {
    let events_a = make_events(5);
    let mut events_b = events_a.clone();
    events_b.reverse();

    let result = ReplayEngine::replay(&events_b, None);
    assert!(matches!(result, Err(ReplayError::EventOutOfOrder { .. })));
}

#[test]
fn replay_determinism_100_events() {
    let events = make_events(100);
    let cp1 = ReplayEngine::replay(&events, None).unwrap();
    let cp2 = ReplayEngine::replay(&events, None).unwrap();
    assert_eq!(cp1.content_hash(), cp2.content_hash());
    assert!(cp1.verify());
}

#[test]
fn replay_with_corrupted_base_fails() {
    let mut base = ModelEnsemble::new().save_all();
    // Corrupt by modifying a parameter
    base.success.parameters[0] = 9999.0;
    let events = make_events(3);
    let result = ReplayEngine::replay(&events, Some(&base));
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// ModelStore tests (10)
// -----------------------------------------------------------------------

#[test]
fn store_commit_returns_id() {
    let mut store = ModelStore::new();
    let cp = ModelEnsemble::new().save_all();
    let id = store.commit(cp, "first".into());
    assert!(!id.as_str().is_empty());
}

#[test]
fn store_checkout_round_trip() {
    let mut store = ModelStore::new();
    let cp = ModelEnsemble::new().save_all();
    let id = store.commit(cp.clone(), "test".into());
    let checked_out = store.checkout(&id).unwrap();
    assert_eq!(cp.content_hash(), checked_out.content_hash());
}

#[test]
fn store_checkout_not_found() {
    let store = ModelStore::new();
    let result = store.checkout(&CommitId::new("nonexistent"));
    assert!(matches!(result, Err(ReplayError::CheckpointNotFound(_))));
}

#[test]
fn store_head_initially_none() {
    let store = ModelStore::new();
    assert!(store.get_head().is_none());
}

#[test]
fn store_head_after_commit() {
    let mut store = ModelStore::new();
    let cp = ModelEnsemble::new().save_all();
    let id = store.commit(cp, "first".into());
    assert_eq!(store.get_head(), Some(id));
}

#[test]
fn store_head_updates_on_subsequent_commits() {
    let mut store = ModelStore::new();
    let cp1 = ModelEnsemble::new().save_all();
    let id1 = store.commit(cp1, "first".into());

    let mut ens2 = ModelEnsemble::new();
    ens2.update_all(&test_sample(true, 0));
    let cp2 = ens2.save_all();
    let id2 = store.commit(cp2, "second".into());

    assert_eq!(store.get_head(), Some(id2));
    assert_ne!(store.get_head(), Some(id1));
}

#[test]
fn store_log_walks_parents() {
    let mut store = ModelStore::new();
    for i in 0..3 {
        let cp = ModelEnsemble::new().save_all();
        store.commit(cp, format!("commit-{}", i));
    }
    let log = store.log();
    assert_eq!(log.len(), 3);
    // Newest first
    assert_eq!(log[0].message, "commit-2");
    assert_eq!(log[1].message, "commit-1");
    assert_eq!(log[2].message, "commit-0");
    // Parent chain
    assert!(log[0].parent.is_some());
    assert!(log[1].parent.is_some());
    assert!(log[2].parent.is_none()); // first commit has no parent
}

#[test]
fn store_log_from_empty() {
    let store = ModelStore::new();
    let log = store.log();
    assert!(log.is_empty());
}

#[test]
fn store_tag_and_resolve() {
    let mut store = ModelStore::new();
    let cp = ModelEnsemble::new().save_all();
    let id = store.commit(cp, "test".into());
    store.tag(&id, "v1").unwrap();
    let resolved = store.resolve_tag("v1");
    assert_eq!(resolved, Some(&id));
}

#[test]
fn store_tag_duplicate_error() {
    let mut store = ModelStore::new();
    let cp = ModelEnsemble::new().save_all();
    let id = store.commit(cp, "test".into());
    store.tag(&id, "v1").unwrap();
    let result = store.tag(&id, "v1");
    assert!(matches!(result, Err(ReplayError::TagAlreadyExists(_))));
}
