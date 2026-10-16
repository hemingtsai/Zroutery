//! The migration fixtures: state transitions, the store, every action, snapshots and recovery.

use super::result::compute_hash;
use super::*;

// -- State transitions --------------------------------------------------

#[test]
fn happy_path_detected_through_completed() {
    let store = MigrationStore::new();
    assert_eq!(store.current_state(), MigrationState::Detected);

    store.transition(MigrationState::Prepared).unwrap();
    assert_eq!(store.current_state(), MigrationState::Prepared);

    store.transition(MigrationState::Verified).unwrap();
    assert_eq!(store.current_state(), MigrationState::Verified);

    store.transition(MigrationState::Switched).unwrap();
    assert_eq!(store.current_state(), MigrationState::Switched);

    store.transition(MigrationState::Completed).unwrap();
    assert_eq!(store.current_state(), MigrationState::Completed);
}

#[test]
fn failed_state_from_any_state() {
    let start_states = [
        MigrationState::Detected,
        MigrationState::Prepared,
        MigrationState::Verified,
        MigrationState::Switched,
        MigrationState::Completed,
    ];

    for start in start_states {
        let store = MigrationStore::new();
        // Reach `start` if needed (we start at Detected)
        if start != MigrationState::Detected {
            store.transition(MigrationState::Prepared).unwrap();
        }
        if start == MigrationState::Verified
            || start == MigrationState::Switched
            || start == MigrationState::Completed
        {
            store.transition(MigrationState::Verified).unwrap();
        }
        if start == MigrationState::Switched || start == MigrationState::Completed {
            store.transition(MigrationState::Switched).unwrap();
        }
        if start == MigrationState::Completed {
            store.transition(MigrationState::Completed).unwrap();
        }

        store.transition(MigrationState::Failed).unwrap();
        assert_eq!(store.current_state(), MigrationState::Failed);
    }
}

#[test]
fn rolled_back_from_failed() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Failed).unwrap();
    store.transition(MigrationState::RolledBack).unwrap();
    assert_eq!(store.current_state(), MigrationState::RolledBack);
}

#[test]
fn invalid_transition_rejected() {
    let store = MigrationStore::new();
    // Detected -> Verified is not allowed (must go through Prepared)
    let err = store.transition(MigrationState::Verified).unwrap_err();
    assert_eq!(err, MigrationState::Detected);
    // State is unchanged
    assert_eq!(store.current_state(), MigrationState::Detected);
}

#[test]
fn completed_cannot_go_to_prepared() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Verified).unwrap();
    store.transition(MigrationState::Switched).unwrap();
    store.transition(MigrationState::Completed).unwrap();

    let err = store.transition(MigrationState::Prepared).unwrap_err();
    assert_eq!(err, MigrationState::Completed);
}

#[test]
fn rolled_back_cannot_transition_to_anything_except_failed() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Failed).unwrap();
    store.transition(MigrationState::RolledBack).unwrap();

    // RolledBack -> Completed is not allowed
    let err = store.transition(MigrationState::Completed).unwrap_err();
    assert_eq!(err, MigrationState::RolledBack);

    // RolledBack -> Prepared is not allowed
    let err = store.transition(MigrationState::Prepared).unwrap_err();
    assert_eq!(err, MigrationState::RolledBack);

    // RolledBack -> Failed IS allowed (any state -> Failed)
    store.transition(MigrationState::Failed).unwrap();
    assert_eq!(store.current_state(), MigrationState::Failed);
}

// -- Idempotent transition ----------------------------------------------

#[test]
fn idempotent_same_state_is_noop() {
    let store = MigrationStore::new();
    assert_eq!(store.current_state(), MigrationState::Detected);
    // Transitioning to the same state should succeed
    store.transition(MigrationState::Detected).unwrap();
    assert_eq!(store.current_state(), MigrationState::Detected);
}

#[test]
fn idempotent_in_later_state() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Prepared).unwrap();
    assert_eq!(store.current_state(), MigrationState::Prepared);
}

// -- MigrationPlan serde round-trip ------------------------------------

#[test]
fn migration_plan_serde_round_trip() {
    let plan = MigrationPlan {
        plan_id: "plan-001".to_string(),
        source_description: "Migrate from nginx reverse-proxy".to_string(),
        steps: vec![
            MigrationStep {
                description: "Copy config".to_string(),
                action: MigrationAction::CopyConfig {
                    source: "/etc/nginx/zroutery.conf".to_string(),
                    dest: "zroutery.toml".to_string(),
                },
                reversible: true,
            },
            MigrationStep {
                description: "Validate config".to_string(),
                action: MigrationAction::ValidateConfig {
                    path: valid_json("s1").to_string(),
                },
                reversible: true,
            },
            MigrationStep {
                description: "Stop external".to_string(),
                action: MigrationAction::StopExternal {
                    process_name: "nginx".to_string(),
                },
                reversible: false,
            },
            MigrationStep {
                description: "Start Zroutery".to_string(),
                action: MigrationAction::StartZroutery { port: 443 },
                reversible: false,
            },
            MigrationStep {
                description: "Verify endpoint".to_string(),
                action: MigrationAction::VerifyEndpoint {
                    url: "https://localhost:443/health".to_string(),
                    expect: EndpointExpect::Success,
                },
                reversible: false,
            },
            MigrationStep {
                description: "Custom step".to_string(),
                action: MigrationAction::Custom {
                    description: "Flush DNS cache".to_string(),
                },
                reversible: false,
            },
        ],
        created_at: 1_700_000_000,
    };

    let json = serde_json::to_string(&plan).unwrap();
    let deserialized: MigrationPlan = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.plan_id, "plan-001");
    assert_eq!(deserialized.steps.len(), 6);
    assert_eq!(deserialized.created_at, 1_700_000_000);

    // Verify the tagged enum round-trips correctly
    let first_action = &deserialized.steps[0].action;
    match first_action {
        MigrationAction::CopyConfig { source, dest } => {
            assert_eq!(source, "/etc/nginx/zroutery.conf");
            assert_eq!(dest, "zroutery.toml");
        }
        _ => panic!("expected CopyConfig"),
    }
}

// -- MigrationResult construction --------------------------------------

#[test]
fn migration_result_construction() {
    let result = MigrationResult {
        state: MigrationState::Completed,
        steps_completed: 5,
        steps_total: 5,
        errors: vec![],
        warnings: vec!["Port 443 requires root".to_string()],
        duration_ms: 1234,
        rolled_back: false,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    };

    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 5);
    assert_eq!(result.steps_total, 5);
    assert!(result.errors.is_empty());
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.duration_ms, 1234);
    assert!(!result.rolled_back);
}

// -- MigrationStore history --------------------------------------------

#[test]
fn migration_store_state_persistence() {
    let store = MigrationStore::new();

    store.record_result(MigrationResult {
        state: MigrationState::Prepared,
        steps_completed: 1,
        steps_total: 3,
        errors: vec![],
        warnings: vec![],
        duration_ms: 100,
        rolled_back: false,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    });

    store.record_result(MigrationResult {
        state: MigrationState::Completed,
        steps_completed: 3,
        steps_total: 3,
        errors: vec![],
        warnings: vec!["minor warning".to_string()],
        duration_ms: 500,
        rolled_back: false,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    });

    let history = store.history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].state, MigrationState::Prepared);
    assert_eq!(history[1].state, MigrationState::Completed);
    assert_eq!(history[1].warnings[0], "minor warning");
}

#[test]
fn history_returns_empty_vec_initially() {
    let store = MigrationStore::new();
    assert!(store.history().is_empty());
}

// -- MigrationResult serde round-trip -----------------------------------

#[test]
fn migration_result_serde_round_trip() {
    let result = MigrationResult {
        state: MigrationState::Failed,
        steps_completed: 2,
        steps_total: 5,
        errors: vec!["connection refused".to_string(), "timeout".to_string()],
        warnings: vec!["slow response".to_string()],
        duration_ms: 4200,
        rolled_back: true,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    };

    let json = serde_json::to_string(&result).unwrap();
    let deserialized: MigrationResult = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.state, MigrationState::Failed);
    assert_eq!(deserialized.steps_completed, 2);
    assert_eq!(deserialized.steps_total, 5);
    assert_eq!(deserialized.errors.len(), 2);
    assert_eq!(deserialized.errors[0], "connection refused");
    assert_eq!(deserialized.errors[1], "timeout");
    assert_eq!(deserialized.warnings.len(), 1);
    assert_eq!(deserialized.warnings[0], "slow response");
    assert_eq!(deserialized.duration_ms, 4200);
    assert!(deserialized.rolled_back);
}

// -- State machine forward transitions ----------------------------------

#[test]
fn state_machine_forward_transitions() {
    let store = MigrationStore::new();

    let forward_path = [
        MigrationState::Prepared,
        MigrationState::Verified,
        MigrationState::Switched,
        MigrationState::Completed,
    ];

    for (i, &target) in forward_path.iter().enumerate() {
        let before = store.current_state();
        store.transition(target).unwrap();
        assert_eq!(store.current_state(), target);
        // Each step should be a real transition, not a no-op
        assert_ne!(before, target, "step {i}: state should have changed");
    }
}

// -- Store persists across calls ----------------------------------------

#[test]
fn store_persists_across_calls() {
    let store = MigrationStore::new();

    // State persists across transition calls
    store.transition(MigrationState::Prepared).unwrap();
    assert_eq!(store.current_state(), MigrationState::Prepared);
    store.transition(MigrationState::Verified).unwrap();
    assert_eq!(store.current_state(), MigrationState::Verified);

    // History persists across record_result calls
    store.record_result(MigrationResult {
        state: MigrationState::Verified,
        steps_completed: 2,
        steps_total: 4,
        errors: vec![],
        warnings: vec![],
        duration_ms: 200,
        rolled_back: false,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    });
    assert_eq!(store.history().len(), 1);

    store.record_result(MigrationResult {
        state: MigrationState::Completed,
        steps_completed: 4,
        steps_total: 4,
        errors: vec![],
        warnings: vec![],
        duration_ms: 800,
        rolled_back: false,
        restored_files: Vec::new(),
        failed_restores: Vec::new(),
    });
    assert_eq!(store.history().len(), 2);

    // State and history coexist independently
    assert_eq!(store.current_state(), MigrationState::Verified);
    let h = store.history();
    assert_eq!(h[0].state, MigrationState::Verified);
    assert_eq!(h[1].state, MigrationState::Completed);
}

// -- Requested I2 verification tests ------------------------------------

#[test]
fn state_machine_failure_from_any_state() {
    let all_states = [
        MigrationState::Detected,
        MigrationState::Prepared,
        MigrationState::Verified,
        MigrationState::Switched,
        MigrationState::Completed,
        MigrationState::RolledBack,
    ];

    for start in all_states {
        let store = MigrationStore::new();
        // Navigate to the start state
        match start {
            MigrationState::Detected => {}
            MigrationState::Prepared => {
                store.transition(MigrationState::Prepared).unwrap();
            }
            MigrationState::Verified => {
                store.transition(MigrationState::Prepared).unwrap();
                store.transition(MigrationState::Verified).unwrap();
            }
            MigrationState::Switched => {
                store.transition(MigrationState::Prepared).unwrap();
                store.transition(MigrationState::Verified).unwrap();
                store.transition(MigrationState::Switched).unwrap();
            }
            MigrationState::Completed => {
                store.transition(MigrationState::Prepared).unwrap();
                store.transition(MigrationState::Verified).unwrap();
                store.transition(MigrationState::Switched).unwrap();
                store.transition(MigrationState::Completed).unwrap();
            }
            MigrationState::RolledBack => {
                store.transition(MigrationState::Failed).unwrap();
                store.transition(MigrationState::RolledBack).unwrap();
            }
            MigrationState::Failed => {
                store.transition(MigrationState::Failed).unwrap();
            }
        }

        // Now force into Failed (go through RolledBack if needed)
        if start == MigrationState::RolledBack {
            // RolledBack -> Failed is allowed
            store.transition(MigrationState::Failed).unwrap();
        } else if start != MigrationState::Failed {
            store.transition(MigrationState::Failed).unwrap();
        }
        assert_eq!(
            store.current_state(),
            MigrationState::Failed,
            "should reach Failed from {start:?}"
        );
    }
}

#[test]
fn state_machine_rollback_from_failed() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Verified).unwrap();
    store.transition(MigrationState::Switched).unwrap();
    store.transition(MigrationState::Failed).unwrap();
    assert_eq!(store.current_state(), MigrationState::Failed);

    store.transition(MigrationState::RolledBack).unwrap();
    assert_eq!(store.current_state(), MigrationState::RolledBack);
}

#[test]
fn state_machine_invalid_transition_rejected() {
    // Detected -> Verified (skipping Prepared)
    let store = MigrationStore::new();
    let err = store.transition(MigrationState::Verified).unwrap_err();
    assert_eq!(err, MigrationState::Detected);
    assert_eq!(store.current_state(), MigrationState::Detected);

    // Detected -> Switched
    let err = store.transition(MigrationState::Switched).unwrap_err();
    assert_eq!(err, MigrationState::Detected);

    // Detected -> Completed
    let err = store.transition(MigrationState::Completed).unwrap_err();
    assert_eq!(err, MigrationState::Detected);

    // Prepared -> Switched (skipping Verified)
    let store2 = MigrationStore::new();
    store2.transition(MigrationState::Prepared).unwrap();
    let err = store2.transition(MigrationState::Switched).unwrap_err();
    assert_eq!(err, MigrationState::Prepared);

    // Completed -> Prepared (backwards)
    let store3 = MigrationStore::new();
    store3.transition(MigrationState::Prepared).unwrap();
    store3.transition(MigrationState::Verified).unwrap();
    store3.transition(MigrationState::Switched).unwrap();
    store3.transition(MigrationState::Completed).unwrap();
    let err = store3.transition(MigrationState::Prepared).unwrap_err();
    assert_eq!(err, MigrationState::Completed);

    // RolledBack -> anything except Failed
    let store4 = MigrationStore::new();
    store4.transition(MigrationState::Failed).unwrap();
    store4.transition(MigrationState::RolledBack).unwrap();
    let err = store4.transition(MigrationState::Completed).unwrap_err();
    assert_eq!(err, MigrationState::RolledBack);
    let err = store4.transition(MigrationState::Prepared).unwrap_err();
    assert_eq!(err, MigrationState::RolledBack);
}

#[test]
fn idempotent_migration_noop() {
    let store = MigrationStore::new();
    assert_eq!(store.current_state(), MigrationState::Detected);

    // Same-state transition is a no-op
    store.transition(MigrationState::Detected).unwrap();
    assert_eq!(store.current_state(), MigrationState::Detected);

    // Advance and repeat
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Prepared).unwrap();
    assert_eq!(store.current_state(), MigrationState::Prepared);

    // History should still be empty (noop transitions don't record)
    assert!(store.history().is_empty());
}

#[test]
fn migration_history_records_all() {
    let store = MigrationStore::new();

    let states = [
        MigrationState::Detected,
        MigrationState::Prepared,
        MigrationState::Verified,
        MigrationState::Switched,
        MigrationState::Completed,
    ];

    for (i, &state) in states.iter().enumerate() {
        store.record_result(MigrationResult {
            state,
            steps_completed: i,
            steps_total: states.len(),
            errors: vec![],
            warnings: vec![],
            duration_ms: (i as u64 + 1) * 100,
            rolled_back: false,
            restored_files: Vec::new(),
            failed_restores: Vec::new(),
        });
    }

    let history = store.history();
    assert_eq!(history.len(), 5);
    for (i, entry) in history.iter().enumerate() {
        assert_eq!(entry.state, states[i]);
        assert_eq!(entry.steps_completed, i);
        assert_eq!(entry.duration_ms, (i as u64 + 1) * 100);
    }
}

// -- MigrationExecutor tests --------------------------------------------

fn make_plan(steps: Vec<MigrationStep>) -> MigrationPlan {
    MigrationPlan {
        plan_id: "test-plan".to_string(),
        source_description: "test".to_string(),
        steps,
        created_at: 1_700_000_000,
    }
}

/// Write `contents` to a uniquely named temp file and return its path.
///
/// `ValidateConfig` parses a real file now, so a test that wants the step to
/// pass has to hand it something real. The pid keeps concurrent tests in the
/// same binary from sharing one path, and `label` keeps two calls in the
/// *same* test from sharing one either.
fn temp_file(label: &str, contents: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "zroutery_migration_{}_{}_{}.json",
        std::process::id(),
        label,
        contents.len()
    ));
    std::fs::write(&path, contents).expect("write temp fixture");
    path.to_string_lossy().into_owned()
}

/// A temp file holding valid JSON, for steps that must succeed.
fn valid_json(label: &str) -> String {
    temp_file(label, r#"{"providers":[]}"#)
}

/// A temp file holding text that is definitely not JSON, for the failure path.
fn malformed_json(label: &str) -> String {
    temp_file(label, r#"{"providers": [ }"#)
}

#[tokio::test]
async fn executor_success_all_steps_pass() {
    // Create a temp source file for CopyConfig
    let tmp = std::env::temp_dir().join("zroutery_migration_test_source.toml");
    std::fs::write(&tmp, "[server]\nport = 8080\n").unwrap();
    let dest = std::env::temp_dir().join("zroutery_migration_test_dest.toml");

    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![
        MigrationStep {
            description: "Validate config".to_string(),
            action: MigrationAction::ValidateConfig {
                path: valid_json("s2").to_string(),
            },
            reversible: true,
        },
        MigrationStep {
            description: "Copy config".to_string(),
            action: MigrationAction::CopyConfig {
                source: tmp.to_str().unwrap().to_string(),
                dest: dest.to_str().unwrap().to_string(),
            },
            reversible: true,
        },
    ]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 2);
    assert_eq!(result.steps_total, 2);
    assert!(result.errors.is_empty());
    assert!(!result.rolled_back);
    assert_eq!(executor.store.current_state(), MigrationState::Completed);

    // Verify the copy actually happened
    assert!(dest.exists());
    let content = std::fs::read_to_string(&dest).unwrap();
    assert!(content.contains("port = 8080"));

    // Cleanup
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&dest);
}

#[tokio::test]
async fn executor_failure_step_fails() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![
        MigrationStep {
            description: "Validate".to_string(),
            action: MigrationAction::ValidateConfig {
                path: valid_json("s3").to_string(),
            },
            reversible: true,
        },
        MigrationStep {
            description: "Bad copy".to_string(),
            action: MigrationAction::CopyConfig {
                source: "".to_string(),
                dest: "to.toml".to_string(),
            },
            reversible: true,
        },
    ]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 1);
    assert_eq!(result.steps_total, 2);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("Step 1"));
    assert!(result.errors[0].contains("source or dest is empty"));
    assert!(!result.rolled_back);
    assert_eq!(executor.store.current_state(), MigrationState::Failed);
}

#[tokio::test]
async fn executor_rollback_from_failed() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    // Force into a failed state via a failing plan
    let plan = make_plan(vec![MigrationStep {
        description: "Fail".to_string(),
        action: MigrationAction::CopyConfig {
            source: "".to_string(),
            dest: "".to_string(),
        },
        reversible: true,
    }]);
    executor.execute(&plan).await;
    assert_eq!(executor.store.current_state(), MigrationState::Failed);

    let result = executor.rollback(None);
    assert_eq!(result.state, MigrationState::RolledBack);
    // No snapshot was captured, so no file came back. See the field's
    // documentation: Rolled_back records whether files were restored, and a
    // rollback that restored nothing has not rolled anything back.
    assert!(
        !result.rolled_back,
        "a rollback with nothing captured must not report a successful restore"
    );
    assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
}

#[tokio::test]
async fn executor_empty_source_dest_is_step_error() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Empty paths".to_string(),
        action: MigrationAction::CopyConfig {
            source: "".to_string(),
            dest: "".to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 0);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("source or dest is empty"));
}

#[tokio::test]
async fn executor_empty_endpoint_url_is_step_error() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Empty URL".to_string(),
        action: MigrationAction::VerifyEndpoint {
            url: "".to_string(),
            expect: EndpointExpect::Success,
        },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 0);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("endpoint URL is empty"));
}

#[tokio::test]
async fn executor_records_duration() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Validate".to_string(),
        action: MigrationAction::ValidateConfig {
            path: valid_json("s4").to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    // Duration should be recorded (>= 0 is always true, but it should be non-overflowing)
    // We just verify the field is set; it will be a small number for a no-op.
    let _ = result.duration_ms;
}

// -- I2 real execution tests ---------------------------------------------

#[tokio::test]
async fn copy_config_real_file() {
    let tmp = std::env::temp_dir().join("zroutery_copy_real_source.toml");
    std::fs::write(&tmp, "key = \"value\"\n").unwrap();
    let dest = std::env::temp_dir().join("zroutery_copy_real_dest.toml");

    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Copy".to_string(),
        action: MigrationAction::CopyConfig {
            source: tmp.to_str().unwrap().to_string(),
            dest: dest.to_str().unwrap().to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 1);
    assert!(dest.exists());
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "key = \"value\"\n");

    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&dest);
}

#[tokio::test]
async fn copy_config_missing_source_errors() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Copy".to_string(),
        action: MigrationAction::CopyConfig {
            source: "/nonexistent/path/zroutery_missing.toml".to_string(),
            dest: "dest.toml".to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 0);
    assert!(result.errors[0].contains("source file does not exist"));
}

#[tokio::test]
async fn copy_config_creates_backup_of_existing_dest() {
    let tmp = std::env::temp_dir().join("zroutery_backup_source.toml");
    let dest = std::env::temp_dir().join("zroutery_backup_dest.toml");
    let bak = std::env::temp_dir().join("zroutery_backup_dest.toml.bak");

    std::fs::write(&tmp, "new_content\n").unwrap();
    std::fs::write(&dest, "old_content\n").unwrap();

    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Copy with backup".to_string(),
        action: MigrationAction::CopyConfig {
            source: tmp.to_str().unwrap().to_string(),
            dest: dest.to_str().unwrap().to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);

    // Dest should have new content
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new_content\n");
    // Backup should have old content
    assert!(bak.exists());
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), "old_content\n");

    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&dest);
    let _ = std::fs::remove_file(&bak);
}

#[tokio::test]
async fn validate_config_parses_real_json() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Validate".to_string(),
        action: MigrationAction::ValidateConfig {
            path: valid_json("valid"),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 1);
}

/// The test this replaces was `validate_config_always_passes`, and its name
/// was the specification: a step with no input could not fail, so a green
/// suite asserted that validation was a no-op. The action now carries the
/// path it must parse, which gives malformed input somewhere to land.
#[tokio::test]
async fn validate_config_fails_on_malformed_json() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Validate".to_string(),
        action: MigrationAction::ValidateConfig {
            path: malformed_json("broken"),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 0);
    assert!(
        result.errors[0].contains("not valid JSON"),
        "the failure must name the real problem, got: {}",
        result.errors[0]
    );
}

/// A validator that accepted a format it cannot read would be the bug this
/// change set exists to remove, so an unreadable format is refused outright.
#[tokio::test]
async fn validate_config_refuses_a_format_it_cannot_parse() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    // CC Switch's preferred store is a SQLite database and core has no
    // SQLite dependency. Saying so is the honest outcome; reporting
    // "config validated" would not be.
    let path =
        std::env::temp_dir().join(format!("zroutery_migration_db_{}.db", std::process::id()));
    std::fs::write(&path, b"SQLite format 3\0").expect("write db fixture");

    let plan = make_plan(vec![MigrationStep {
        description: "Validate".to_string(),
        action: MigrationAction::ValidateConfig {
            path: path.to_string_lossy().into_owned(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("cannot validate a"),
        "the refusal must say what it cannot do, got: {}",
        result.errors[0]
    );
}

#[tokio::test]
async fn validate_config_fails_when_the_file_is_absent() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Validate".to_string(),
        action: MigrationAction::ValidateConfig {
            path: "zroutery_no_such_config_file_98765.json".to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert!(
        result.errors[0].contains("cannot read"),
        "got: {}",
        result.errors[0]
    );
}

#[tokio::test]
async fn start_zroutery_checks_port_availability() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    // Use a high port that is very likely free
    let plan = make_plan(vec![MigrationStep {
        description: "Start".to_string(),
        action: MigrationAction::StartZroutery { port: 59123 },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    // Port should be available in test environment
}

#[tokio::test]
async fn verify_endpoint_unreachable_errors() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    // Use a URL that will not connect (non-routable IP with short timeout)
    let plan = make_plan(vec![MigrationStep {
        description: "Verify".to_string(),
        action: MigrationAction::VerifyEndpoint {
            url: "http://192.0.2.1:1/health".to_string(), // TEST-NET, non-routable
            expect: EndpointExpect::Success,
        },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 0);
    assert!(result.errors[0].contains("unreachable"));
}

#[tokio::test]
async fn stop_external_reports_a_process_that_is_not_running() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    // A name that almost certainly does not exist.
    let plan = make_plan(vec![MigrationStep {
        description: "Stop".to_string(),
        action: MigrationAction::StopExternal {
            process_name: "zroutery_nonexistent_process_12345".to_string(),
        },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 1);
}

/// The ownership rule, stated as a test because it is the part of this change
/// most likely to be quietly relaxed later: `StopExternal` observes, and
/// reports that it did not act. It must never be a euphemism for a kill.
#[tokio::test]
async fn stop_external_does_not_own_what_it_did_not_start() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Stop".to_string(),
        action: MigrationAction::StopExternal {
            process_name: "zroutery_nonexistent_process_12345".to_string(),
        },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    let message = result.warnings.join(" ");
    assert_eq!(
        executor.owned_labels().len(),
        0,
        "observing a process must never make this migration its owner"
    );
    assert!(
        !message.contains("was stopped"),
        "StopExternal must not claim to have stopped anything, got: {message:?}"
    );
}

#[tokio::test]
async fn stop_external_rejects_an_empty_process_name() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Stop".to_string(),
        action: MigrationAction::StopExternal {
            process_name: String::new(),
        },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert!(result.errors[0].contains("process name is empty"));
}

/// The test this replaces asserted that a custom step completed. It did,
/// in the sense that a `format!` ran: the action did nothing and reported
/// success, which is the exact failure mode the typed runner exists to
/// prevent. It is now refused, so a plan cannot quietly contain a step
/// nobody performs.
#[tokio::test]
async fn a_custom_action_is_refused_rather_than_reported_as_done() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Custom action".to_string(),
        action: MigrationAction::Custom {
            description: "flush DNS cache".to_string(),
        },
        reversible: false,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(result.steps_completed, 0);
    assert!(
        result.errors[0].contains("cannot be executed by the typed runner"),
        "got: {}",
        result.errors[0]
    );
}

/// A refused custom step must not be able to smuggle a later real step
/// through as completed: the plan stops at the first failure.
#[tokio::test]
async fn a_refused_custom_action_stops_the_plan() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![
        MigrationStep {
            description: "Validate".to_string(),
            action: MigrationAction::ValidateConfig {
                path: valid_json("before_custom"),
            },
            reversible: true,
        },
        MigrationStep {
            description: "Custom".to_string(),
            action: MigrationAction::Custom {
                description: "do nothing".to_string(),
            },
            reversible: false,
        },
        MigrationStep {
            description: "Validate again".to_string(),
            action: MigrationAction::ValidateConfig {
                path: valid_json("after_custom"),
            },
            reversible: true,
        },
    ]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Failed);
    assert_eq!(
        result.steps_completed, 1,
        "the step after the refusal must not run"
    );
    assert_eq!(result.steps_total, 3);
}

#[test]
fn create_snapshot_captures_file_contents() {
    let tmp = std::env::temp_dir().join("zroutery_snapshot_test.toml");
    std::fs::write(&tmp, "snapshot_content\n").unwrap();

    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let snapshot = executor.create_snapshot(&[tmp.to_str().unwrap()]).unwrap();
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].path, tmp.to_str().unwrap());
    assert_eq!(snapshot.files[0].content, b"snapshot_content\n");
    assert!(snapshot.files[0].hash != 0);
    assert!(snapshot.created_at > 0);

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn create_snapshot_skips_missing_files() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let snapshot = executor
        .create_snapshot(&["/nonexistent/zroutery_file.toml"])
        .unwrap();
    assert_eq!(snapshot.files.len(), 0);
}

#[test]
fn rollback_restores_from_snapshot() {
    let tmp = std::env::temp_dir().join("zroutery_rollback_test.toml");
    std::fs::write(&tmp, "original_content\n").unwrap();

    // Overwrite with new content
    std::fs::write(&tmp, "modified_content\n").unwrap();

    // Create snapshot with the "original" content
    let snapshot = MigrationSnapshot {
        files: vec![SnapshotFile {
            path: tmp.to_str().unwrap().to_string(),
            content: b"original_content\n".to_vec(),
            hash: compute_hash(b"original_content\n"),
        }],
        created_at: 1_700_000_000,
    };

    let store = MigrationStore::new();
    store.transition(MigrationState::Failed).unwrap();
    let executor = MigrationExecutor::new(store);

    let result = executor.rollback(Some(&snapshot));
    assert_eq!(result.state, MigrationState::RolledBack);
    assert!(result.rolled_back);
    assert!(result.warnings.is_empty());

    // Verify the file was restored
    assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "original_content\n");

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn rollback_without_snapshot_transitions_state() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Failed).unwrap();
    let executor = MigrationExecutor::new(store);

    let result = executor.rollback(None);
    assert_eq!(result.state, MigrationState::RolledBack);
    // No snapshot was captured, so no file came back. See the field's
    // documentation: Rolled_back records whether files were restored, and a
    // rollback that restored nothing has not rolled anything back.
    assert!(
        !result.rolled_back,
        "a rollback with nothing captured must not report a successful restore"
    );
    assert!(result.warnings.is_empty());
    assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
}

#[test]
fn compute_hash_is_deterministic() {
    let data = b"test data for hashing";
    let h1 = compute_hash(data);
    let h2 = compute_hash(data);
    assert_eq!(h1, h2);
}

#[test]
fn compute_hash_differs_for_different_data() {
    let h1 = compute_hash(b"aaa");
    let h2 = compute_hash(b"bbb");
    assert_ne!(h1, h2);
}

#[tokio::test]
async fn full_migration_plan_execute_verify() {
    // End-to-end: create files, plan, execute, verify results
    let source = std::env::temp_dir().join("zroutery_full_source.toml");
    let dest = std::env::temp_dir().join("zroutery_full_dest.toml");

    std::fs::write(&source, "[routes]\npath = \"/api\"\n").unwrap();

    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![
        MigrationStep {
            description: "Validate config".to_string(),
            action: MigrationAction::ValidateConfig {
                path: valid_json("s6").to_string(),
            },
            reversible: true,
        },
        MigrationStep {
            description: "Copy config".to_string(),
            action: MigrationAction::CopyConfig {
                source: source.to_str().unwrap().to_string(),
                dest: dest.to_str().unwrap().to_string(),
            },
            reversible: true,
        },
        MigrationStep {
            description: "Start Zroutery".to_string(),
            action: MigrationAction::StartZroutery { port: 59124 },
            reversible: false,
        },
    ]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    assert_eq!(result.steps_completed, 3);
    assert_eq!(result.steps_total, 3);
    assert!(result.errors.is_empty());
    assert!(!result.rolled_back);
    assert_eq!(executor.store.current_state(), MigrationState::Completed);

    // Verify dest was created
    assert!(dest.exists());
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "[routes]\npath = \"/api\"\n"
    );

    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&dest);
}

// -- Recovery and idempotence tests (I2) ---------------------------------

#[test]
fn recover_from_interrupted_prepared() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    assert_eq!(store.current_state(), MigrationState::Prepared);

    let executor = MigrationExecutor::new(store);
    let result = executor.recover().unwrap();

    assert_eq!(result.state, MigrationState::RolledBack);
    // No snapshot was captured, so no file came back. See the field's
    // documentation: Rolled_back records whether files were restored, and a
    // rollback that restored nothing has not rolled anything back.
    assert!(
        !result.rolled_back,
        "a rollback with nothing captured must not report a successful restore"
    );
    assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
}

#[test]
fn recover_from_interrupted_verified() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Verified).unwrap();
    assert_eq!(store.current_state(), MigrationState::Verified);

    let executor = MigrationExecutor::new(store);
    let result = executor.recover().unwrap();

    assert_eq!(result.state, MigrationState::RolledBack);
    // No snapshot was captured, so no file came back. See the field's
    // documentation: Rolled_back records whether files were restored, and a
    // rollback that restored nothing has not rolled anything back.
    assert!(
        !result.rolled_back,
        "a rollback with nothing captured must not report a successful restore"
    );
    assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
}

#[test]
fn recover_from_interrupted_switched() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Verified).unwrap();
    store.transition(MigrationState::Switched).unwrap();
    assert_eq!(store.current_state(), MigrationState::Switched);

    let executor = MigrationExecutor::new(store);
    let result = executor.recover().unwrap();

    assert_eq!(result.state, MigrationState::RolledBack);
    // No snapshot was captured, so no file came back. See the field's
    // documentation: Rolled_back records whether files were restored, and a
    // rollback that restored nothing has not rolled anything back.
    assert!(
        !result.rolled_back,
        "a rollback with nothing captured must not report a successful restore"
    );
    assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
}

#[test]
fn recover_from_failed() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Failed).unwrap();

    let executor = MigrationExecutor::new(store);
    let result = executor.recover().unwrap();

    assert_eq!(result.state, MigrationState::RolledBack);
    // No snapshot was captured, so no file came back. See the field's
    // documentation: Rolled_back records whether files were restored, and a
    // rollback that restored nothing has not rolled anything back.
    assert!(
        !result.rolled_back,
        "a rollback with nothing captured must not report a successful restore"
    );
    assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
}

#[test]
fn recover_from_detected_errors() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let err = executor.recover().unwrap_err();
    assert!(err.contains("cannot recover from state Detected"));
}

#[test]
fn recover_from_completed_errors() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Verified).unwrap();
    store.transition(MigrationState::Switched).unwrap();
    store.transition(MigrationState::Completed).unwrap();

    let executor = MigrationExecutor::new(store);
    let err = executor.recover().unwrap_err();
    assert!(err.contains("cannot recover from state Completed"));
}

#[test]
fn recover_from_rolled_back_errors() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Failed).unwrap();
    store.transition(MigrationState::RolledBack).unwrap();

    let executor = MigrationExecutor::new(store);
    let err = executor.recover().unwrap_err();
    assert!(err.contains("cannot recover from state RolledBack"));
}

#[test]
fn check_already_migrated_false_initially() {
    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);
    assert!(!executor.check_already_migrated());
}

#[test]
fn check_already_migrated_false_midway() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    let executor = MigrationExecutor::new(store);
    assert!(!executor.check_already_migrated());
}

#[test]
fn check_already_migrated_true_after_completion() {
    let store = MigrationStore::new();
    store.transition(MigrationState::Prepared).unwrap();
    store.transition(MigrationState::Verified).unwrap();
    store.transition(MigrationState::Switched).unwrap();
    store.transition(MigrationState::Completed).unwrap();

    let executor = MigrationExecutor::new(store);
    assert!(executor.check_already_migrated());
}

#[tokio::test]
async fn idempotent_migration_full_lifecycle() {
    // Execute once -> Completed
    let tmp = std::env::temp_dir().join("zroutery_idempotent_source.toml");
    let dest = std::env::temp_dir().join("zroutery_idempotent_dest.toml");
    std::fs::write(&tmp, "idempotent = true\n").unwrap();

    let store = MigrationStore::new();
    let executor = MigrationExecutor::new(store);

    let plan = make_plan(vec![MigrationStep {
        description: "Copy".to_string(),
        action: MigrationAction::CopyConfig {
            source: tmp.to_str().unwrap().to_string(),
            dest: dest.to_str().unwrap().to_string(),
        },
        reversible: true,
    }]);

    let result = executor.execute(&plan).await;
    assert_eq!(result.state, MigrationState::Completed);
    assert!(executor.check_already_migrated());

    // Execute again -> already migrated, no-op (caller should check first)
    assert!(executor.check_already_migrated());

    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&dest);
}
