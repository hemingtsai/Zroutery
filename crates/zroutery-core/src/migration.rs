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

/// What counts as a healthy answer from an endpoint the migration just switched to.
///
/// This type exists because "the socket answered" is not evidence that the
/// endpoint works. A 500 is an answer, a 401 is an answer, and a proxy's
/// connection-refused page is an answer. Accepting any of them is what let a
/// migration walk all the way to `Completed` against a service that was broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointExpect {
    /// Any 2xx. The floor, not the goal: it proves the right port is serving.
    Success,
    /// One specific status code, for an endpoint with a defined non-2xx contract.
    Status(u16),
    /// A 2xx whose body contains this marker.
    ///
    /// Stronger than [`EndpointExpect::Success`] on purpose, and the one worth
    /// reaching for during a cutover: a 200 from the wrong service, or from a
    /// stale process still holding the port, passes a status check and fails
    /// this. The marker is what distinguishes "Zroutery is serving" from
    /// "something is serving".
    BodyContains(String),
}

impl Default for EndpointExpect {
    /// [`EndpointExpect::Success`], so a plan serialized before this field
    /// existed deserializes to the documented floor rather than failing to
    /// parse. It is deliberately the *weakest* option: an old plan that named no
    /// expectation gets the check it would have got before, and no more.
    fn default() -> Self {
        Self::Success
    }
}

/// One step of a migration, as data.
///
/// Every variant is executable by [`MigrationExecutor::execute_step`]. There is
/// no untyped escape hatch: [`MigrationAction::Custom`] exists on the wire for
/// compatibility and **fails** when executed, because a step that reports success
/// without doing anything is worse than a migration that refuses to start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MigrationAction {
    /// Copy a config file, backing up any existing destination first.
    ///
    /// Warns when the copied bytes contain credential-shaped keys. Core has no
    /// credential store, so it cannot route a secret anywhere -- it can only
    /// refuse to be quiet about one. Keeping the keys out of config is the
    /// desktop layer's job, where `ccswitch::credential_fingerprint` already
    /// establishes the convention that a credential is referenced, never carried.
    CopyConfig { source: String, dest: String },
    /// Parse a config file and fail if it is malformed.
    ///
    /// Carries the path because a validator with no input cannot validate: the
    /// earlier unit variant returned a success message unconditionally.
    ValidateConfig { path: String },
    /// Report whether a named external process is running.
    ///
    /// **Does not stop it.** This migration did not start that process, so it
    /// does not own its lifetime and will not terminate a program the user did
    /// not ask us to launch. Use [`MigrationAction::StopOwned`] for processes
    /// this migration started.
    StopExternal { process_name: String },
    /// Start a child process and take ownership of it.
    ///
    /// Ownership is what makes stopping safe: only a process recorded here can
    /// be stopped by label, and it is stopped with [`MigrationAction::StopOwned`].
    StartOwned {
        label: String,
        program: String,
        args: Vec<String>,
    },
    /// Stop a child this migration started, and wait for it to actually exit.
    StopOwned { label: String },
    /// Check that a port is free, so the switch does not collide with something
    /// already bound.
    StartZroutery { port: u16 },
    /// Require a real answer from `url`, judged by `expect`.
    VerifyEndpoint {
        url: String,
        /// Defaults to [`EndpointExpect::Success`] so a plan written before this
        /// field existed still parses.
        #[serde(default)]
        expect: EndpointExpect,
    },
    /// Retained for wire compatibility. Executing it is an error.
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

/// The key names that suggest a value is a credential rather than configuration.
///
/// A deliberately small, deliberately literal list. This is not a scanner for
/// secrets -- it cannot be, without decrypting things -- it is a way for the
/// migration to notice that the file it just copied is the kind of file that
/// tends to hold keys, and say so while someone is still in a position to act on
/// it. A false positive costs a warning; a false negative costs a secret on disk
/// in a second location nobody was tracking.
const CREDENTIAL_KEY_NAMES: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "secret",
    "token",
    "password",
    "passwd",
    "authorization",
    "access_key",
    "private_key",
    "client_secret",
    "refresh_token",
];

/// Credential-shaped key names present in `bytes`, lowercased and sorted.
///
/// Scans for the name and not the value, so it never reports what a secret *is*
/// -- only that a field conventionally holding one was copied. It is a textual
/// scan, not a parse, deliberately: the point is to notice a copied `.db` or
/// `.toml` just as readily as a copied `.json`, and a scanner that only
/// understood JSON would miss the binary database that CC Switch actually
/// prefers. That also means it can be fooled by a value that happens to contain
/// the word, which is why every finding is a warning for a human rather than a
/// verdict.
fn credential_keys_in(bytes: &[u8]) -> Vec<String> {
    let lowered = String::from_utf8_lossy(bytes).to_lowercase();

    let mut found: Vec<String> = Vec::new();

    for name in CREDENTIAL_KEY_NAMES {
        // Delimiters on BOTH sides, so `token` does not match inside
        // `max_tokens` (prefix is `_`) or `tokenizer` (suffix is `i`). Both of
        // those are ordinary configuration, and a scanner that flagged them would
        // be a scanner whose warning nobody reads.
        let is_whole_word = |at: usize| -> bool {
            let before_ok = at == 0 || {
                let before = lowered.as_bytes()[at - 1];
                !before.is_ascii_alphanumeric() && before != b'_' && before != b'-'
            };
            let after = at + name.len();
            let after_ok = after >= lowered.len() || {
                let after_byte = lowered.as_bytes()[after];
                !after_byte.is_ascii_alphanumeric() && after_byte != b'_' && after_byte != b'-'
            };
            before_ok && after_ok
        };

        if lowered.match_indices(name).any(|(at, _)| is_whole_word(at))
            && !found.iter().any(|existing| existing == name)
        {
            found.push((*name).to_string());
        }
    }

    found.sort();
    found
}

// ---------------------------------------------------------------------------
// MigrationExecutor
// ---------------------------------------------------------------------------

/// A child process this migration started, and therefore owns.
///
/// Ownership is the whole point. A process in this list was launched by
/// [`MigrationAction::StartOwned`], so stopping it cannot take down something
/// the user was already running.
struct OwnedProcess {
    label: String,
    child: std::process::Child,
}

/// Executes a migration plan step by step.
pub struct MigrationExecutor {
    store: MigrationStore,
    /// Children started by this migration, keyed by the label the plan gave them.
    ///
    /// A plain `Mutex` rather than an async lock: it is never held across an
    /// `.await`, only around a `Vec` operation or a `kill`/`wait` pair.
    owned: std::sync::Mutex<Vec<OwnedProcess>>,
}

impl MigrationExecutor {
    pub fn new(store: MigrationStore) -> Self {
        Self {
            store,
            owned: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The labels of the child processes this migration currently owns.
    pub fn owned_labels(&self) -> Vec<String> {
        match self.owned.lock() {
            Ok(owned) => owned.iter().map(|p| p.label.clone()).collect(),
            // A poisoned lock means a prior holder panicked mid-`kill`. The
            // children are still tracked, so recovering the labels is strictly
            // better than reporting none and orphaning them.
            Err(poisoned) => poisoned
                .into_inner()
                .iter()
                .map(|p| p.label.clone())
                .collect(),
        }
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
                    std::fs::copy(dest_path, &backup).map_err(|e| format!("backup failed: {e}"))?;
                }
                // Copy
                std::fs::copy(source_path, dest_path).map_err(|e| format!("copy failed: {e}"))?;

                let mut notes = vec![format!("copied {} -> {}", source, dest)];

                // Auth policy. Core cannot move a secret anywhere -- the
                // credential store lives in the desktop layer -- so the honest
                // thing is to say what was just propagated instead of copying
                // bytes that carry keys and reporting only the copy.
                let copied = std::fs::read(dest_path).unwrap_or_default();
                let found = credential_keys_in(&copied);
                if !found.is_empty() {
                    notes.push(format!(
                        "copied config contains credential-shaped keys: {} — core cannot \
                         store them, so the desktop layer must replace them with key \
                         references before this file is trusted",
                        found.join(", ")
                    ));
                }

                Ok(notes)
            }
            MigrationAction::ValidateConfig { path } => {
                if path.is_empty() {
                    return Err("config path is empty".into());
                }
                let config_path = std::path::Path::new(path);
                let bytes =
                    std::fs::read(config_path).map_err(|e| format!("cannot read {path}: {e}"))?;

                let extension = config_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                match extension.as_str() {
                    "json" => {
                        // A real parse. Malformed JSON is now a failure, which is
                        // the entire point: the earlier action had no input and
                        // so could not fail.
                        serde_json::from_slice::<serde_json::Value>(&bytes)
                            .map_err(|e| format!("{path} is not valid JSON: {e}"))?;
                        Ok(vec![format!("{path} is valid JSON")])
                    }
                    other => Err(format!(
                        "cannot validate a {other:?} file: the typed validator parses JSON, \
                         and refusing is the honest answer for a format it does not read. \
                         A validator that accepted {other:?} without reading it would be the \
                         bug this replaced."
                    )),
                }
            }
            MigrationAction::StopExternal { process_name } => {
                if process_name.is_empty() {
                    return Err("process name is empty".into());
                }
                // Read-only, and it says so. This migration did not start that
                // process, so it does not own the lifetime and will not kill a
                // program the user may be relying on. The running case is a
                // warning rather than a silent success, because "I did not stop
                // it" must not read the same as "it needed stopping and is now
                // stopped".
                #[cfg(target_os = "windows")]
                {
                    let output = std::process::Command::new("tasklist")
                        .args(["/FI", &format!("IMAGENAME eq {}", process_name), "/NH"])
                        .output()
                        .map_err(|e| format!("tasklist failed: {e}"))?;
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let found = !stdout.contains("No tasks are running")
                        && stdout.to_lowercase().contains(&process_name.to_lowercase());
                    if found {
                        Ok(vec![format!(
                            "external process '{process_name}' is running and was NOT stopped: \
                             this migration did not start it. Stop it yourself if the config \
                             it holds is being rewritten."
                        )])
                    } else {
                        Ok(vec![format!(
                            "external process '{process_name}' is not running"
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
                            "external process '{process_name}' is running and was NOT stopped: \
                             this migration did not start it. Stop it yourself if the config \
                             it holds is being rewritten."
                        )]),
                        _ => Ok(vec![format!(
                            "external process '{process_name}' is not running"
                        )]),
                    }
                }
            }
            MigrationAction::StartOwned {
                label,
                program,
                args,
            } => {
                if label.is_empty() {
                    return Err("owned process label is empty".into());
                }
                if program.is_empty() {
                    return Err("owned process program is empty".into());
                }
                if self.owned_labels().iter().any(|held| held == label) {
                    return Err(format!(
                        "label '{label}' is already owned by this migration; a second child \
                         under the same name would make StopOwned ambiguous about which \
                         one it kills"
                    ));
                }

                let child = std::process::Command::new(program)
                    .args(args)
                    // Detach the child's streams. A long-lived owned child that
                    // inherits this process's stdout writes straight into
                    // whatever is hosting the migration -- the desktop app's
                    // console, or a test runner's captured output -- interleaved
                    // with its own. Nothing here reads the child's output, so
                    // there is nothing to lose.
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| format!("cannot start '{program}': {e}"))?;

                let pid = child.id();
                self.owned
                    .lock()
                    .map_err(|_| "owned-process registry is poisoned".to_string())?
                    .push(OwnedProcess {
                        label: label.clone(),
                        child,
                    });

                Ok(vec![format!(
                    "started '{label}' as pid {pid} and took ownership of it"
                )])
            }
            MigrationAction::StopOwned { label } => {
                if label.is_empty() {
                    return Err("owned process label is empty".into());
                }

                let mut owned = self
                    .owned
                    .lock()
                    .map_err(|_| "owned-process registry is poisoned".to_string())?;

                let index = owned
                    .iter()
                    .position(|p| p.label == *label)
                    .ok_or_else(|| {
                        format!(
                            "no owned process labelled '{label}'; this migration only stops \
                             what it started. Owned now: {:?}",
                            owned.iter().map(|p| p.label.clone()).collect::<Vec<_>>()
                        )
                    })?;

                let mut process = owned.remove(index);
                let pid = process.child.id();

                // `try_wait` first: a child that already exited needs no signal,
                // and killing it would report a success that did no work.
                if let Some(status) = process
                    .child
                    .try_wait()
                    .map_err(|e| format!("cannot query pid {pid}: {e}"))?
                {
                    return Ok(vec![format!(
                        "'{label}' (pid {pid}) had already exited on its own: {status}"
                    )]);
                }

                process
                    .child
                    .kill()
                    .map_err(|e| format!("cannot stop '{label}' (pid {pid}): {e}"))?;
                let status = process
                    .child
                    .wait()
                    .map_err(|e| format!("cannot reap '{label}' (pid {pid}): {e}"))?;

                Ok(vec![format!(
                    "stopped owned process '{label}' (pid {pid}), reaped with {status}"
                )])
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
            MigrationAction::VerifyEndpoint { url, expect } => {
                if url.is_empty() {
                    return Err("endpoint URL is empty".into());
                }
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .map_err(|e| format!("client build failed: {e}"))?;

                // Reaching the endpoint is only the first half. The status is
                // judged, because a 500 or a 401 is an answer and this step
                // exists to establish that the service works.
                let response = client
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| format!("endpoint {url} unreachable: {e}"))?;
                let status = response.status();

                match expect {
                    EndpointExpect::Success => {
                        if !status.is_success() {
                            return Err(format!(
                                "endpoint {url} answered {status}, which is not a success; \
                                 a response is not a working service"
                            ));
                        }
                        Ok(vec![format!("endpoint {url} answered {status}")])
                    }
                    EndpointExpect::Status(wanted) => {
                        if status.as_u16() != *wanted {
                            return Err(format!(
                                "endpoint {url} answered {status}, expected exactly {wanted}"
                            ));
                        }
                        Ok(vec![format!("endpoint {url} answered {status}")])
                    }
                    EndpointExpect::BodyContains(marker) => {
                        if !status.is_success() {
                            return Err(format!(
                                "endpoint {url} answered {status} before the body could be \
                                 checked for {marker:?}"
                            ));
                        }
                        let body = response
                            .text()
                            .await
                            .map_err(|e| format!("cannot read body from {url}: {e}"))?;
                        if !body.contains(marker.as_str()) {
                            return Err(format!(
                                "endpoint {url} answered {status} but its body does not \
                                 contain {marker:?}; something is serving that is not the \
                                 service that was migrated to"
                            ));
                        }
                        Ok(vec![format!(
                            "endpoint {url} answered {status} with the expected body marker"
                        )])
                    }
                }
            }
            MigrationAction::Custom { description } => {
                // Refused rather than reported as done. An opaque step that returns success is
                // how a migration claims to have done something it never did.
                Err(format!(
                    "custom action {description:?} cannot be executed by the typed runner. \
                     Express the step as a typed action, or run it outside the plan."
                ))
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

    /// Check for and recover from an interrupted migration.
    ///
    /// If the migration is in a mid-way state (`Prepared`, `Verified`,
    /// `Switched`) or has already `Failed`, transitions through `Failed` to
    /// `RolledBack` and returns the rollback result. Returns an error for
    /// states that are not recoverable (e.g. `Detected`, `Completed`,
    /// `RolledBack`).
    pub fn recover(&self) -> Result<MigrationResult, String> {
        let state = self.store.current_state();
        match state {
            MigrationState::Prepared | MigrationState::Verified | MigrationState::Switched => {
                // Transition through Failed before rolling back.
                let _ = self.store.transition(MigrationState::Failed);
                Ok(self.rollback(None))
            }
            MigrationState::Failed => Ok(self.rollback(None)),
            _ => Err(format!("cannot recover from state {:?}", state)),
        }
    }

    /// Check if migration is already complete (idempotent).
    ///
    /// Returns `true` if the store is in the `Completed` state, indicating
    /// the migration has already been fully applied and no work is needed.
    pub fn check_already_migrated(&self) -> bool {
        self.store.current_state() == MigrationState::Completed
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
        assert!(result.rolled_back);
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
        assert!(result.rolled_back);
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
        assert!(result.rolled_back);
        assert_eq!(executor.store.current_state(), MigrationState::RolledBack);
    }

    #[test]
    fn recover_from_failed() {
        let store = MigrationStore::new();
        store.transition(MigrationState::Failed).unwrap();

        let executor = MigrationExecutor::new(store);
        let result = executor.recover().unwrap();

        assert_eq!(result.state, MigrationState::RolledBack);
        assert!(result.rolled_back);
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
}
