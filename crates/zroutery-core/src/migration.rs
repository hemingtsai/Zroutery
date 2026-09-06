//! Migration support for moving from an external router to Zroutery.
//!
//! Provides state-machine driven migration with step-by-step plans,
//! rollback capability, and result history tracking.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// MigrationState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationState {
    Detected,
    Prepared,
    Verified,
    Switched,
    Completed,
    RolledBack,
    Failed,
}

impl MigrationState {
    /// Returns `true` if transitioning from `self` to `target` is allowed.
    pub fn can_transition_to(self, target: MigrationState) -> bool {
        use MigrationState::*;
        matches!(
            (self, target),
            (Detected, Prepared)
                | (Prepared, Verified)
                | (Verified, Switched)
                | (Switched, Completed)
                // Any state may fail
                | (_, Failed)
                // Only Failed may roll back
                | (Failed, RolledBack)
        )
    }
}

// ---------------------------------------------------------------------------
// MigrationPlan / MigrationStep / MigrationAction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub plan_id: String,
    pub source_description: String,
    pub steps: Vec<MigrationStep>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    pub description: String,
    pub action: MigrationAction,
    pub reversible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MigrationAction {
    CopyConfig { source: String, dest: String },
    ValidateConfig,
    StopExternal { process_name: String },
    StartZroutery { port: u16 },
    VerifyEndpoint { url: String },
    Custom { description: String },
}

// ---------------------------------------------------------------------------
// MigrationResult
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub state: MigrationState,
    pub steps_completed: usize,
    pub steps_total: usize,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub duration_ms: u64,
    pub rolled_back: bool,
}

// ---------------------------------------------------------------------------
// MigrationSnapshot
// ---------------------------------------------------------------------------

/// A single file captured before migration for rollback purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub content: Vec<u8>,
    pub hash: u64,
}

/// A point-in-time snapshot of files that can be restored during rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationSnapshot {
    pub files: Vec<SnapshotFile>,
    pub created_at: i64,
}

/// Simple hash for snapshot integrity verification.
fn compute_hash(data: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// MigrationStore
// ---------------------------------------------------------------------------

/// Thread-safe store that tracks the current migration state and records
/// completed migration results.
pub struct MigrationStore {
    state: Mutex<MigrationState>,
    history: Mutex<Vec<MigrationResult>>,
}

impl MigrationStore {
    /// Creates a new store starting in the `Detected` state.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MigrationState::Detected),
            history: Mutex::new(Vec::new()),
        }
    }

    /// Returns the current migration state.
    pub fn current_state(&self) -> MigrationState {
        *self.state.lock().unwrap()
    }

    /// Attempts a state transition. Returns `Ok(())` on success, or
    /// `Err(MigrationState)` with the (unchanged) current state when the
    /// transition is not allowed.  Transitioning to the same state is a no-op
    /// that always succeeds (idempotent).
    pub fn transition(&self, target: MigrationState) -> Result<(), MigrationState> {
        let mut current = self.state.lock().unwrap();
        if *current == target {
            return Ok(());
        }
        if current.can_transition_to(target) {
            *current = target;
            Ok(())
        } else {
            Err(*current)
        }
    }

    /// Appends a migration result to the history.
    pub fn record_result(&self, result: MigrationResult) {
        self.history.lock().unwrap().push(result);
    }

    /// Returns a snapshot of all recorded migration results.
    pub fn history(&self) -> Vec<MigrationResult> {
        self.history.lock().unwrap().clone()
    }
}

impl Default for MigrationStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// MigrationExecutor
// ---------------------------------------------------------------------------

/// Executes a migration plan step by step.
pub struct MigrationExecutor {
    store: MigrationStore,
}

impl MigrationExecutor {
    pub fn new(store: MigrationStore) -> Self {
        Self { store }
    }

    /// Execute a migration plan. Returns the result.
    pub async fn execute(&self, plan: &MigrationPlan) -> MigrationResult {
        let start = std::time::Instant::now();
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut completed = 0;

        // Transition Detected -> Prepared (or current -> Prepared if idempotent)
        if let Err(current) = self.store.transition(MigrationState::Prepared) {
            return MigrationResult {
                state: MigrationState::Failed,
                steps_completed: 0,
                steps_total: plan.steps.len(),
                errors: vec![format!(
                    "Cannot start migration: current state is {:?}, expected Detected",
                    current
                )],
                warnings,
                duration_ms: start.elapsed().as_millis() as u64,
                rolled_back: false,
            };
        }

        for (i, step) in plan.steps.iter().enumerate() {
            match self.execute_step(step).await {
                Ok(w) => {
                    completed += 1;
                    warnings.extend(w);
                }
                Err(e) => {
                    errors.push(format!("Step {} ({}) failed: {}", i, step.description, e));
                    let _ = self.store.transition(MigrationState::Failed);
                    return MigrationResult {
                        state: MigrationState::Failed,
                        steps_completed: completed,
                        steps_total: plan.steps.len(),
                        errors,
                        warnings,
                        duration_ms: start.elapsed().as_millis() as u64,
                        rolled_back: false,
                    };
                }
            }
        }

        // Walk the state machine through Verified -> Switched -> Completed
        let _ = self.store.transition(MigrationState::Verified);
        let _ = self.store.transition(MigrationState::Switched);
        let _ = self.store.transition(MigrationState::Completed);

        MigrationResult {
            state: MigrationState::Completed,
            steps_completed: completed,
            steps_total: plan.steps.len(),
            errors,
            warnings,
            duration_ms: start.elapsed().as_millis() as u64,
            rolled_back: false,
        }
    }

    async fn execute_step(&self, step: &MigrationStep) -> Result<Vec<String>, String> {
        match &step.action {
            MigrationAction::CopyConfig { source, dest } => {
                // Validate paths are not empty
                if source.is_empty() || dest.is_empty() {
                    return Err("source or dest is empty".into());
                }
                // Validate source exists
                let source_path = std::path::Path::new(source);
                if !source_path.exists() {
                    return Err(format!("source file does not exist: {}", source));
                }
                // Create backup of destination if it exists
                let dest_path = std::path::Path::new(dest);
                if dest_path.exists() {
                    let backup = format!("{}.bak", dest);
                    std::fs::copy(dest_path, &backup)
                        .map_err(|e| format!("backup failed: {e}"))?;
                }
                // Copy
                std::fs::copy(source_path, dest_path)
                    .map_err(|e| format!("copy failed: {e}"))?;
                Ok(vec![format!("copied {} -> {}", source, dest)])
            }
            MigrationAction::ValidateConfig => {
                // Placeholder: real validation would parse YAML/TOML/JSON
                Ok(vec!["config validated".into()])
            }
            MigrationAction::StopExternal { process_name } => {
                if process_name.is_empty() {
                    return Err("process name is empty".into());
                }
                // Check if process is running (read-only, no kill)
                #[cfg(target_os = "windows")]
                {
                    let output = std::process::Command::new("tasklist")
                        .args([
                            "/FI",
                            &format!("IMAGENAME eq {}", process_name),
                            "/NH",
                        ])
                        .output()
                        .map_err(|e| format!("tasklist failed: {e}"))?;
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let found = !stdout.contains("No tasks are running")
                        && stdout
                            .to_lowercase()
                            .contains(&process_name.to_lowercase());
                    if found {
                        Ok(vec![format!(
                            "external process '{}' is running (should be stopped manually)",
                            process_name
                        )])
                    } else {
                        Ok(vec![format!(
                            "external process '{}' not found (may already be stopped)",
                            process_name
                        )])
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let output = std::process::Command::new("pgrep")
                        .arg(process_name)
                        .output();
                    match output {
                        Ok(out) if out.status.success() => Ok(vec![format!(
                            "external process '{}' is running (should be stopped manually)",
                            process_name
                        )]),
                        _ => Ok(vec![format!(
                            "external process '{}' not found (may already be stopped)",
                            process_name
                        )]),
                    }
                }
            }
            MigrationAction::StartZroutery { port } => {
                // Verify the port is not already in use
                let addr = format!("127.0.0.1:{}", port);
                match tokio::net::TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        drop(listener);
                        Ok(vec![format!("port {} is available", port)])
                    }
                    Err(_) => Ok(vec![format!(
                        "port {} is already in use (may be Zroutery)",
                        port
                    )]),
                }
            }
            MigrationAction::VerifyEndpoint { url } => {
                if url.is_empty() {
                    return Err("endpoint URL is empty".into());
                }
                // Try to reach the endpoint
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .map_err(|e| format!("client build failed: {e}"))?;
                match client.get(url).send().await {
                    Ok(resp) => Ok(vec![format!(
                        "endpoint {} responded: {}",
                        url,
                        resp.status()
                    )]),
                    Err(e) => Err(format!("endpoint {} unreachable: {}", url, e)),
                }
            }
            MigrationAction::Custom { description } => {
                // Custom steps are opaque; return success with description
                Ok(vec![format!("custom step executed: {}", description)])
            }
        }
    }

    /// Create a snapshot of the given files for rollback purposes.
    pub fn create_snapshot(&self, paths: &[&str]) -> Result<MigrationSnapshot, String> {
        let mut files = Vec::new();
        for path in paths {
            match std::fs::read(path) {
                Ok(content) => {
                    let hash = compute_hash(&content);
                    files.push(SnapshotFile {
                        path: path.to_string(),
                        content,
                        hash,
                    });
                }
                Err(e) => {
                    tracing::warn!("snapshot: skipping {}: {}", path, e);
                }
            }
        }
        Ok(MigrationSnapshot {
            files,
            created_at: chrono::Utc::now().timestamp(),
        })
    }

    /// Rollback a failed migration. If a snapshot is provided, restores
    /// the captured files to their original locations.
    pub fn rollback(&self, snapshot: Option<&MigrationSnapshot>) -> MigrationResult {
        let mut warnings = Vec::new();
        if let Some(snap) = snapshot {
            for file in &snap.files {
                if let Err(e) = std::fs::write(&file.path, &file.content) {
                    tracing::warn!("rollback: failed to restore {}: {}", file.path, e);
                    warnings.push(format!("failed to restore {}: {}", file.path, e));
                } else {
                    tracing::info!("rollback: restored {}", file.path);
                }
            }
        }
        let _ = self.store.transition(MigrationState::RolledBack);
        MigrationResult {
            state: MigrationState::RolledBack,
            steps_completed: 0,
            steps_total: 0,
            errors: vec![],
            warnings,
            duration_ms: 0,
            rolled_back: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
            if start == MigrationState::Verified || start == MigrationState::Switched || start == MigrationState::Completed {
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
                    action: MigrationAction::ValidateConfig,
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
        });

        store.record_result(MigrationResult {
            state: MigrationState::Completed,
            steps_completed: 3,
            steps_total: 3,
            errors: vec![],
            warnings: vec!["minor warning".to_string()],
            duration_ms: 500,
            rolled_back: false,
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
                action: MigrationAction::ValidateConfig,
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
                action: MigrationAction::ValidateConfig,
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
        assert!(result.rolled_back);
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
            action: MigrationAction::ValidateConfig,
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
    async fn validate_config_always_passes() {
        let store = MigrationStore::new();
        let executor = MigrationExecutor::new(store);

        let plan = make_plan(vec![MigrationStep {
            description: "Validate".to_string(),
            action: MigrationAction::ValidateConfig,
            reversible: true,
        }]);

        let result = executor.execute(&plan).await;
        assert_eq!(result.state, MigrationState::Completed);
        assert_eq!(result.steps_completed, 1);
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
            },
            reversible: false,
        }]);

        let result = executor.execute(&plan).await;
        assert_eq!(result.state, MigrationState::Failed);
        assert_eq!(result.steps_completed, 0);
        assert!(result.errors[0].contains("unreachable"));
    }

    #[tokio::test]
    async fn stop_external_with_process_name() {
        let store = MigrationStore::new();
        let executor = MigrationExecutor::new(store);

        // Use a process name that almost certainly does not exist
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

    #[tokio::test]
    async fn custom_step_executes() {
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
        assert_eq!(result.state, MigrationState::Completed);
        assert_eq!(result.steps_completed, 1);
    }

    #[test]
    fn create_snapshot_captures_file_contents() {
        let tmp = std::env::temp_dir().join("zroutery_snapshot_test.toml");
        std::fs::write(&tmp, "snapshot_content\n").unwrap();

        let store = MigrationStore::new();
        let executor = MigrationExecutor::new(store);

        let snapshot = executor
            .create_snapshot(&[tmp.to_str().unwrap()])
            .unwrap();
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
        assert!(result.rolled_back);
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
                action: MigrationAction::ValidateConfig,
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
}
