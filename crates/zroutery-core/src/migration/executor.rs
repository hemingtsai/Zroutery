//! Running the plan: the credential scanner, and the executor that steps a migration forward or rolls it back.

use super::result::compute_hash;
use super::result::{MigrationResult, MigrationSnapshot, SnapshotFile};
use super::state::{
    EndpointExpect, MigrationAction, MigrationPlan, MigrationState, MigrationStep, SnapshotWriter,
    StdFsWriter,
};
use super::store::MigrationStore;

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
    pub(super) store: MigrationStore,
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
                restored_files: Vec::new(),
                failed_restores: Vec::new(),
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
                        restored_files: Vec::new(),
                        failed_restores: Vec::new(),
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
            restored_files: Vec::new(),
            failed_restores: Vec::new(),
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
        let snapshot = MigrationSnapshot {
            files,
            created_at: chrono::Utc::now().timestamp(),
        };
        // Persisted when the store is durable, so that a `recover` in a later
        // process has the captured bytes to put back rather than nothing. A store
        // that is memory-only returns `Ok` here and keeps the old behaviour, which
        // is why no existing fixture changed.
        self.store.save_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    /// Rollback a failed migration. If a snapshot is provided, restores
    /// the captured files to their original locations.
    pub fn rollback(&self, snapshot: Option<&MigrationSnapshot>) -> MigrationResult {
        self.rollback_with(snapshot, &StdFsWriter)
    }

    /// Rollback a failed migration, restoring through `writer`.
    ///
    /// Restores every file it can and reports every file it could not. A failure
    /// is recorded in `errors`, not `warnings`, and clears `rolled_back`: before
    /// this, a rollback that restored nothing at all returned
    /// `state: RolledBack, rolled_back: true` with the reason buried in
    /// `warnings` — indistinguishable from success to any caller that checks the
    /// obvious fields, which is all of them.
    pub fn rollback_with(
        &self,
        snapshot: Option<&MigrationSnapshot>,
        writer: &dyn SnapshotWriter,
    ) -> MigrationResult {
        // Never pushed to: restore failures are errors, not warnings. Kept as an
        // empty vec because `MigrationResult` carries it and a caller may be
        // reading a result this produced.
        let warnings: Vec<String> = Vec::new();
        let mut errors = Vec::new();
        let mut restored_files = Vec::new();
        let mut failed_restores = Vec::new();

        if let Some(snap) = snapshot {
            for file in &snap.files {
                match writer.write(&file.path, &file.content) {
                    Ok(()) => {
                        tracing::info!("rollback: restored {}", file.path);
                        restored_files.push(file.path.clone());
                    }
                    Err(e) => {
                        tracing::warn!("rollback: failed to restore {}: {}", file.path, e);
                        errors.push(format!("failed to restore {}: {}", file.path, e));
                        failed_restores.push(file.path.clone());
                    }
                }
            }
        }

        let _ = self.store.transition(MigrationState::RolledBack);
        MigrationResult {
            state: MigrationState::RolledBack,
            steps_completed: 0,
            steps_total: 0,
            errors,
            warnings,
            duration_ms: 0,
            // Only a rollback that actually put something back has rolled back.
            //
            // Both halves matter, and the second was the one missing until a
            // durability gate found it. Testing only `failed_restores.is_empty()`
            // says a rollback with no snapshot rolled back, because restoring
            // nothing fails nothing. That was not hypothetical: `recover` passed
            // `None` on every call, so every interrupted migration reported
            // `rolled_back: true` while putting no file back.
            //
            // A snapshot that captured no files is the same case by another route
            // — `create_snapshot` skips unreadable paths with a warning — and it
            // must not read as a successful restore either.
            rolled_back: !restored_files.is_empty() && failed_restores.is_empty(),
            restored_files,
            failed_restores,
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
                // Restored from the persisted snapshot rather than from `None`.
                // Passing `None` is what made this method a no-op that reported
                // `RolledBack`: the state machine advanced, the files did not come
                // back, and a caller checking only the state saw a recovery that
                // never happened.
                let snapshot = self.store.load_snapshot()?;
                Ok(self.rollback(snapshot.as_ref()))
            }
            MigrationState::Failed => {
                let snapshot = self.store.load_snapshot()?;
                Ok(self.rollback(snapshot.as_ref()))
            }
            _ => Err(format!("cannot recover from state {:?}", state)),
        }
    }

    /// The rollback snapshot as persisted, if this executor's store is durable.
    ///
    /// Exists so a caller driving a real restore — a test, or the desktop layer
    /// when one is built — reads the snapshot the same way [`Self::recover`] does,
    /// rather than reconstructing the decision about whether one exists.
    pub fn snapshot_from_disk(&self) -> Option<MigrationSnapshot> {
        self.store.load_snapshot().ok().flatten()
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
