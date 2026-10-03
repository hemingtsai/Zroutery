//! Agent ownership / adopt lifecycle.
//!
//! Manages the lifecycle of ownership over configuration fields that Zroutery
//! can adopt from an external agent and release back. Tracks managed field
//! values, detects external modifications, and supports clean adopt/release
//! cycles without state drift.
//!
//! State machine:
//!
//! ```text
//! Verified ──adopt()──▶ Adopted ──release()──▶ Released
//!    ▲                     │                      │
//!    └─────────────────────┘◀─────────────────────┘
//!
//! release_with_restore(): Adopted ──▶ Releasing ──(config restored)──▶ Released
//! ```

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// OwnershipState
// ---------------------------------------------------------------------------

/// Lifecycle state for field ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipState {
    /// Fields have been verified and are ready to be adopted.
    Verified,
    /// Zroutery owns the managed fields.
    Adopted,
    /// A release is in flight: the state transition has been claimed but the
    /// config restore on disk has not committed yet.
    Releasing,
    /// Ownership has been released back to the external agent.
    Released,
}

// ---------------------------------------------------------------------------
// ExternalModification
// ---------------------------------------------------------------------------

/// A single field that was modified externally since the last apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalModification {
    /// Dotted path to the modified field (e.g. "model.temperature").
    pub field_path: String,
    /// Value that was last applied by Zroutery, if any.
    pub last_applied: Option<serde_json::Value>,
    /// Current value observed externally.
    pub current_value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// FieldConflict / ConflictResolution
// ---------------------------------------------------------------------------

/// Conflict detected when a managed field was externally modified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldConflict {
    /// Dotted path to the conflicting field.
    pub field_path: String,
    /// Value captured at adoption time.
    pub original_value: Option<serde_json::Value>,
    /// Value last applied by Zroutery.
    pub last_applied: Option<serde_json::Value>,
    /// Current external value.
    pub current_external: serde_json::Value,
}

/// Resolution strategy for conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// Keep the user's external modification.
    KeepExternal,
    /// Overwrite with Zroutery's managed value.
    OverwriteWithManaged,
    /// Skip this field (no action).
    Skip,
}

/// Resolve conflicts between managed fields and external modifications.
///
/// Returns `(field_path, resolved_value)` pairs. `None` values indicate the
/// field should be skipped.
pub fn resolve_conflicts(
    conflicts: &[FieldConflict],
    strategy: ConflictResolution,
) -> Vec<(String, Option<serde_json::Value>)> {
    conflicts
        .iter()
        .map(|c| match strategy {
            ConflictResolution::KeepExternal => {
                (c.field_path.clone(), Some(c.current_external.clone()))
            }
            ConflictResolution::OverwriteWithManaged => {
                (c.field_path.clone(), c.last_applied.clone())
            }
            ConflictResolution::Skip => (c.field_path.clone(), None),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// OwnershipManifest
// ---------------------------------------------------------------------------

/// Snapshot of ownership at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnershipManifest {
    /// The ownership state at the time this manifest was created.
    pub state: OwnershipState,
    /// List of field paths under Zroutery's management.
    pub managed_fields: Vec<String>,
    /// Snapshot of field values captured at adoption time.
    ///
    /// Only managed fields that already existed are recorded here; a managed
    /// field that was absent before adoption is listed in `absent_fields`.
    pub field_snapshots: HashMap<String, serde_json::Value>,
    /// Managed field paths that did not exist when ownership was adopted.
    ///
    /// Release must delete these instead of restoring a value, otherwise a
    /// field Zroutery introduced (for example a local proxy `base_url`) would
    /// survive the release and keep redirecting the client.
    #[serde(default)]
    pub absent_fields: Vec<String>,
    /// Unix timestamp (seconds) of when ownership was adopted, if ever.
    pub adopted_at: Option<i64>,
    /// Unix timestamp (seconds) of when ownership was released, if ever.
    pub released_at: Option<i64>,
    /// Monotonic counter for detecting state drift across cycles.
    pub generation: u64,
}

// ---------------------------------------------------------------------------
// TakeoverStore
// ---------------------------------------------------------------------------

struct TakeoverInner {
    state: OwnershipState,
    manifest: Option<OwnershipManifest>,
    /// Values last applied by Zroutery, keyed by field path.
    last_applied: HashMap<String, serde_json::Value>,
    /// Counts completed adopt-release cycles.
    generation: u64,
}

/// Thread-safe store that manages field ownership lifecycle.
///
/// Supports adopt/release cycles and detects external modifications to managed
/// fields.
pub struct TakeoverStore {
    inner: Mutex<TakeoverInner>,
}

impl TakeoverStore {
    /// Creates a new store in the `Verified` state, ready for adoption.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TakeoverInner {
                state: OwnershipState::Verified,
                manifest: None,
                last_applied: HashMap::new(),
                generation: 0,
            }),
        }
    }

    /// Returns the current ownership state.
    pub fn state(&self) -> OwnershipState {
        crate::sync::lock(&self.inner).state
    }

    /// Returns a clone of the current manifest, if one exists.
    pub fn manifest(&self) -> Option<OwnershipManifest> {
        crate::sync::lock(&self.inner).manifest.clone()
    }

    // -- adopt / release ----------------------------------------------------

    /// Adopt: Zroutery takes ownership of managed fields.
    ///
    /// Records `current_values` for the specified `managed_fields` as
    /// `last_applied` and sets the `adopted_at` timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Verified`.
    pub fn adopt(
        &self,
        managed_fields: Vec<String>,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> Result<OwnershipManifest, String> {
        let mut inner = crate::sync::lock(&self.inner);

        if inner.state != OwnershipState::Verified && inner.state != OwnershipState::Released {
            return Err(format!(
                "cannot adopt: current state is {:?}, expected Verified or Released",
                inner.state
            ));
        }

        let now = chrono::Utc::now().timestamp();
        let generation = inner.generation;

        // Build snapshots from managed_fields only (ignore unmanaged keys).
        let field_snapshots: HashMap<String, serde_json::Value> = managed_fields
            .iter()
            .filter_map(|f| current_values.get(f).map(|v| (f.clone(), v.clone())))
            .collect();

        // Managed fields that did not exist yet must be removed on release.
        let absent_fields: Vec<String> = managed_fields
            .iter()
            .filter(|f| !current_values.contains_key(*f))
            .cloned()
            .collect();

        // Populate last_applied with the captured snapshots.
        inner.last_applied = field_snapshots.clone();

        let manifest = OwnershipManifest {
            state: OwnershipState::Adopted,
            managed_fields,
            field_snapshots,
            absent_fields,
            adopted_at: Some(now),
            released_at: None,
            generation,
        };

        inner.state = OwnershipState::Adopted;
        inner.manifest = Some(manifest.clone());

        Ok(manifest)
    }

    /// Release: Zroutery gives back ownership of managed fields.
    ///
    /// Sets the `released_at` timestamp and transitions to `Released` state.
    /// Unmanaged fields are preserved in the snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Adopted`.
    pub fn release(&self) -> Result<OwnershipManifest, String> {
        let mut inner = crate::sync::lock(&self.inner);

        if inner.state != OwnershipState::Adopted {
            return Err(format!(
                "cannot release: current state is {:?}, expected Adopted",
                inner.state
            ));
        }

        let now = chrono::Utc::now().timestamp();

        inner.generation += 1;
        let gen = inner.generation;

        let manifest = inner.manifest.as_mut().unwrap();
        manifest.state = OwnershipState::Released;
        manifest.released_at = Some(now);
        manifest.generation = gen;

        let result = manifest.clone();

        inner.state = OwnershipState::Released;

        Ok(result)
    }

    // -- external modification detection ------------------------------------

    /// Check whether managed fields were externally modified since last apply.
    ///
    /// Compares `current_values` (keyed by field path) against the
    /// `last_applied` snapshot. Returns a list of [`ExternalModification`]s
    /// for every field whose value differs.
    pub fn detect_external_modification(
        &self,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> Vec<ExternalModification> {
        let inner = crate::sync::lock(&self.inner);

        let mut mods = Vec::new();

        for (field, last_val) in &inner.last_applied {
            match current_values.get(field) {
                Some(current_val) if current_val != last_val => {
                    mods.push(ExternalModification {
                        field_path: field.clone(),
                        last_applied: Some(last_val.clone()),
                        current_value: current_val.clone(),
                    });
                }
                None => {
                    // Field was removed externally.
                    mods.push(ExternalModification {
                        field_path: field.clone(),
                        last_applied: Some(last_val.clone()),
                        current_value: serde_json::Value::Null,
                    });
                }
                _ => { /* unchanged */ }
            }
        }

        mods
    }

    // -- real restore ---------------------------------------------------------

    /// Release ownership and restore original config values via the adapter.
    ///
    /// Reads the current config from disk, restores managed fields to their
    /// original values (captured at adoption), writes back, and transitions
    /// to `Released` state.
    ///
    /// If the disk write fails, the internal state is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Adopted`, or if the
    /// adapter fails to read/write config.
    pub fn release_with_restore(
        &self,
        adapter: &dyn AgentAdapter,
    ) -> Result<OwnershipManifest, String> {
        // 1. Claim the transition while holding the lock. Marking the store as
        //    `Releasing` means a competing adopt or release observes an
        //    in-flight operation instead of the stale `Adopted` state, so it
        //    cannot adopt a new generation or mark this manifest released
        //    while the restore below is still writing the config.
        let (manifest_snapshot, expected_generation) = {
            let mut inner = crate::sync::lock(&self.inner);
            if inner.state != OwnershipState::Adopted {
                return Err(format!(
                    "cannot release: current state is {:?}, expected Adopted",
                    inner.state
                ));
            }
            let manifest = inner.manifest.as_ref().unwrap().clone();
            inner.state = OwnershipState::Releasing;
            (manifest, inner.generation)
        };

        // 2. Read current config from disk.
        let current = match adapter.read_config() {
            Ok(current) => current,
            Err(err) => {
                self.abort_release();
                return Err(err);
            }
        };

        // 3. Restore original values and write to disk.
        if let Err(err) = adapter.release(&current, &manifest_snapshot) {
            self.abort_release();
            return Err(err);
        }

        // 4. Commit only if the claim is still the current one.
        let mut inner = crate::sync::lock(&self.inner);
        if inner.state != OwnershipState::Releasing || inner.generation != expected_generation {
            let state = inner.state;
            let generation = inner.generation;
            if inner.state == OwnershipState::Releasing {
                inner.state = OwnershipState::Adopted;
            }
            return Err(format!(
                "cannot release: ownership changed while restoring the config \
                 (state {state:?}, generation {generation}, expected generation {expected_generation})"
            ));
        }

        inner.generation += 1;
        let gen = inner.generation;

        let manifest = inner.manifest.as_mut().unwrap();
        manifest.state = OwnershipState::Released;
        manifest.released_at = Some(chrono::Utc::now().timestamp());
        manifest.generation = gen;

        let result = manifest.clone();
        inner.state = OwnershipState::Released;

        Ok(result)
    }

    /// Undo an in-flight release claim after the config write failed.
    fn abort_release(&self) {
        let mut inner = crate::sync::lock(&self.inner);
        if inner.state == OwnershipState::Releasing {
            inner.state = OwnershipState::Adopted;
        }
    }

    // -- crash recovery -------------------------------------------------------

    /// Check for incomplete ownership state after a crash.
    ///
    /// Returns `true` if the store is in the `Adopted` state, indicating an
    /// adopt was never released (possibly due to a crash).
    pub fn check_orphaned_state(&self) -> bool {
        let inner = crate::sync::lock(&self.inner);
        inner.state == OwnershipState::Adopted
    }

    /// Recover from an orphaned adoption by releasing with config restore.
    ///
    /// Equivalent to calling [`release_with_restore`] but framed as a
    /// crash-recovery operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is not in an orphaned state, or if
    /// the adapter fails.
    pub fn recover_orphaned(
        &self,
        adapter: &dyn AgentAdapter,
    ) -> Result<OwnershipManifest, String> {
        if !self.check_orphaned_state() {
            return Err("no orphaned state to recover".into());
        }
        self.release_with_restore(adapter)
    }
}

impl Default for TakeoverStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AgentType
// ---------------------------------------------------------------------------

/// Supported external agent types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    Claude,
    Codex,
    Gemini,
}

// ---------------------------------------------------------------------------
// AgentConfigSnapshot
// ---------------------------------------------------------------------------

/// A snapshot of an agent's configuration, captured at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigSnapshot {
    /// The agent this config belongs to.
    pub agent_type: AgentType,
    /// Path to the config file on disk.
    pub config_path: std::path::PathBuf,
    /// Raw config parsed as a JSON value.
    pub raw: serde_json::Value,
    /// Hex-encoded hash of the serialized config for change detection.
    pub config_hash: String,
}

// ---------------------------------------------------------------------------
// ManagedField
// ---------------------------------------------------------------------------

/// A single configuration field under Zroutery's management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedField {
    /// Dotted path to the field (e.g. "model.temperature").
    pub path: String,
    /// Desired value for the field.
    pub value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// AgentAdapter
// ---------------------------------------------------------------------------

/// Trait for agent-specific configuration management.
///
/// Each external agent (Claude, Codex, Gemini) implements this trait to
/// provide config discovery, reading, patching, and release semantics.
pub trait AgentAdapter: Send + Sync {
    /// Agent type identifier.
    fn agent_type(&self) -> AgentType;

    /// Find the agent config file path.
    fn config_path(&self) -> Result<std::path::PathBuf, String>;

    /// Read and parse the current config.
    fn read_config(&self) -> Result<AgentConfigSnapshot, String>;

    /// Apply managed field patches to the config.
    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String>;

    /// Restore original values for managed fields.
    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String>;

    /// Write a config snapshot back to disk.
    fn write_config(&self, snapshot: &AgentConfigSnapshot) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&snapshot.raw)
            .map_err(|e| format!("failed to serialize config: {e}"))?;
        write_config_atomic(&snapshot.config_path, json.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Claude CLI.
///
/// Config location: `~/.claude.json`
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Claude
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        let home = home_dir()?;
        Ok(home.join(".claude.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        read_snapshot(AgentType::Claude, self.config_path()?, ConfigFormat::Json)
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        apply_patch_to_disk(snapshot, fields, ConfigFormat::Json)
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        release_to_disk(snapshot, manifest, ConfigFormat::Json)
    }
}

// ---------------------------------------------------------------------------
// CodexAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Codex CLI.
///
/// Config location: `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`),
/// the TOML file Codex itself reads.
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Codex
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        if let Some(dir) = config_dir_override("CODEX_HOME") {
            return Ok(dir.join("config.toml"));
        }
        let home = home_dir()?;
        Ok(home.join(".codex").join("config.toml"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        read_snapshot(AgentType::Codex, self.config_path()?, ConfigFormat::Toml)
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        apply_patch_to_disk(snapshot, fields, ConfigFormat::Toml)
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        release_to_disk(snapshot, manifest, ConfigFormat::Toml)
    }

    fn write_config(&self, snapshot: &AgentConfigSnapshot) -> Result<(), String> {
        let toml = serialize_config(ConfigFormat::Toml, &snapshot.raw)?;
        write_config_atomic(&snapshot.config_path, toml.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// GeminiAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Gemini CLI.
///
/// Config location: `$GEMINI_CLI_HOME/.gemini/settings.json`
/// (default `~/.gemini/settings.json`), the user settings file Gemini CLI
/// documents.
pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Gemini
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        if let Some(home) = config_dir_override("GEMINI_CLI_HOME") {
            return Ok(home.join(".gemini").join("settings.json"));
        }
        let home = home_dir()?;
        Ok(home.join(".gemini").join("settings.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        read_snapshot(AgentType::Gemini, self.config_path()?, ConfigFormat::Json)
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        apply_patch_to_disk(snapshot, fields, ConfigFormat::Json)
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        release_to_disk(snapshot, manifest, ConfigFormat::Json)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Per-test override for the agent config root.
//
// The adapters below resolve their config path against the user home, and
// the tests used to exercise apply_patch/release against the real
// ~/.claude.json, ~/.codex/config.json and ~/.config/gemini/config.json.
// That clobbered the developer own agent configuration and raced other
// tests reading the same files, so under cfg(test) a test can point the
// adapters at a private temp directory instead. The override is
// thread-local, matching how the test harness runs tests in parallel.
#[cfg(test)]
thread_local! {
    static TEST_AGENT_HOME: std::cell::RefCell<Option<tempfile::TempDir>> =
        const { std::cell::RefCell::new(None) };
    /// Per-test overrides for the config directories a client's own
    /// environment variable selects (see `config_dir_override`).
    static TEST_CONFIG_DIRS: std::cell::RefCell<Vec<(String, std::path::PathBuf)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Give the current test thread its own agent config root.
#[cfg(test)]
fn isolate_agent_home() {
    TEST_CONFIG_DIRS.with(|slot| slot.borrow_mut().clear());
    TEST_AGENT_HOME.with(|slot| {
        *slot.borrow_mut() = Some(tempfile::TempDir::new().expect("temp agent home"));
    });
}

/// Point a client's config directory override at `dir` for this test thread.
#[cfg(test)]
fn isolate_config_dir(env_key: &str, dir: &std::path::Path) {
    TEST_CONFIG_DIRS.with(|slot| {
        let mut dirs = slot.borrow_mut();
        dirs.retain(|(key, _)| key != env_key);
        dirs.push((env_key.to_string(), dir.to_path_buf()));
    });
}

/// The agent config root for this test thread, if one was installed.
#[cfg(test)]
fn isolated_agent_home() -> Option<std::path::PathBuf> {
    TEST_AGENT_HOME.with(|slot| slot.borrow().as_ref().map(|dir| dir.path().to_path_buf()))
}

/// Resolve the current user's home directory.
///
/// Checks `HOME` (Unix) then `USERPROFILE` (Windows).
fn home_dir() -> Result<std::path::PathBuf, String> {
    #[cfg(test)]
    if let Some(root) = isolated_agent_home() {
        return Ok(root);
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(std::path::PathBuf::from(home));
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return Ok(std::path::PathBuf::from(profile));
    }
    Err("could not determine home directory (HOME/USERPROFILE not set)".into())
}

/// Resolve a client config directory from the environment variable that client
/// documents for relocating its configuration.
fn config_dir_override(env_key: &str) -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_DIRS.with(|slot| {
        slot.borrow()
            .iter()
            .find(|(key, _)| key == env_key)
            .map(|(_, dir)| dir.clone())
    }) {
        return Some(dir);
    }

    match std::env::var(env_key) {
        Ok(value) if !value.trim().is_empty() => Some(std::path::PathBuf::from(value)),
        _ => None,
    }
}

/// Write `bytes` to `path` atomically, preserving the permissions of the file
/// being replaced.
///
/// The replacement is staged in a temporary file in the same directory so the
/// rename is atomic, and that temporary file is created with restrictive
/// permissions (0600 on Unix) so a config holding credentials is never
/// readable more widely than the file it replaces. When the destination
/// already exists, its permission bits are re-applied to the staged file
/// before the rename: replacing an existing 0600 config must not silently
/// widen it to the umask default of 0644.
fn write_config_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create dir {}: {e}", parent.display()))?;
    }

    let original_permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    let tmp = temp_sibling(path);

    if let Err(err) = write_restricted(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }

    if let Some(permissions) = original_permissions {
        if let Err(e) = std::fs::set_permissions(&tmp, permissions) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "failed to preserve permissions for {}: {e}",
                path.display()
            ));
        }
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("failed to replace {}: {e}", path.display()));
    }

    Ok(())
}

/// Create `path` with restrictive permissions and write `bytes` into it.
#[cfg(unix)]
fn write_restricted(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("failed to flush {}: {e}", path.display()))?;
    Ok(())
}

/// Create `path` and write `bytes` into it.
#[cfg(not(unix))]
fn write_restricted(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// A unique temporary file name next to `path`, in the same directory so the
/// final rename cannot cross a filesystem boundary.
fn temp_sibling(path: &std::path::Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    path.with_file_name(format!(".{name}.{}.{sequence}.tmp", std::process::id()))
}

/// Compute a hex-encoded hash of the given byte slice for change detection.
fn compute_hash(data: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Set a nested JSON value by dotted path (e.g. "model.temperature").
///
/// Missing intermediate tables are created, but an existing non-table on the
/// path is a type conflict and reported as an error. Quietly skipping the write
/// would report success while changing nothing, and replacing the value with a
/// table would destroy an existing setting.
fn set_nested(
    root: &mut serde_json::Value,
    path: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(format!("invalid field path `{path}`"));
    }

    if !root.is_object() {
        return Err(format!(
            "cannot set `{path}`: the config root is not a table"
        ));
    }

    let mut current = root;

    // Navigate to the parent, creating intermediate tables as needed.
    for part in &parts[..parts.len() - 1] {
        let obj = current.as_object_mut().unwrap();
        if !obj.contains_key(*part) {
            obj.insert(
                part.to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        current = obj.get_mut(*part).unwrap();
        if !current.is_object() {
            return Err(type_conflict(path, part));
        }
    }

    // Set the leaf value.
    let leaf = parts.last().unwrap();
    current
        .as_object_mut()
        .unwrap()
        .insert(leaf.to_string(), value);

    Ok(())
}

/// Describe a patch that would have to overwrite an existing non-table value.
fn type_conflict(path: &str, part: &str) -> String {
    format!("cannot set `{path}`: `{part}` already holds a non-table value of a different type")
}

/// Restore a manifest's managed fields into `raw`.
///
/// Fields that already existed at adoption time are set back to their captured
/// value; fields recorded in [`OwnershipManifest::absent_fields`] are removed
/// so a value Zroutery introduced does not outlive the ownership.
fn restore_fields(raw: &mut serde_json::Value, manifest: &OwnershipManifest) -> Result<(), String> {
    for field_path in &manifest.managed_fields {
        if manifest
            .absent_fields
            .iter()
            .any(|absent| absent == field_path)
        {
            remove_nested(raw, field_path);
        } else if let Some(original_value) = manifest.field_snapshots.get(field_path) {
            set_nested(raw, field_path, original_value.clone())?;
        }
    }
    Ok(())
}

/// Apply managed field patches to `raw`.
fn apply_fields(raw: &mut serde_json::Value, fields: &[ManagedField]) -> Result<(), String> {
    for field in fields {
        set_nested(raw, &field.path, field.value.clone())?;
    }
    Ok(())
}

/// Re-read the config a snapshot was taken from, refusing to touch a file that
/// changed underneath the caller.
///
/// `apply_patch` used to serialize the patched *old* snapshot over the file, so
/// an external edit to a field Zroutery does not manage (another tool, the user
/// editing by hand) was silently reverted. The hash recorded by `read_config`
/// is the version the caller based its patch on; when the file no longer
/// matches, the write is refused and the caller has to re-read and retry.
fn reread_raw(
    snapshot: &AgentConfigSnapshot,
    format: ConfigFormat,
) -> Result<serde_json::Value, String> {
    let path = &snapshot.config_path;

    if !path.exists() {
        if snapshot.config_hash.is_empty() {
            return Ok(serde_json::Value::Object(serde_json::Map::new()));
        }
        return Err(format!(
            "config {} disappeared since it was read; refusing to write it",
            path.display()
        ));
    }

    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    if compute_hash(data.as_bytes()) != snapshot.config_hash {
        return Err(format!(
            "config {} changed on disk since it was read; refusing to overwrite it",
            path.display()
        ));
    }

    parse_config(format, &data).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

/// Patch managed fields into the current on-disk config and write it back.
fn apply_patch_to_disk(
    snapshot: &AgentConfigSnapshot,
    fields: &[ManagedField],
    format: ConfigFormat,
) -> Result<AgentConfigSnapshot, String> {
    let mut raw = reread_raw(snapshot, format)?;
    apply_fields(&mut raw, fields)?;

    let text = serialize_config(format, &raw)?;
    let hash = compute_hash(text.as_bytes());
    write_config_atomic(&snapshot.config_path, text.as_bytes())?;

    Ok(AgentConfigSnapshot {
        agent_type: snapshot.agent_type,
        config_path: snapshot.config_path.clone(),
        raw,
        config_hash: hash,
    })
}

/// Restore a manifest's original values into the current config and write it.
fn release_to_disk(
    snapshot: &AgentConfigSnapshot,
    manifest: &OwnershipManifest,
    format: ConfigFormat,
) -> Result<(), String> {
    let mut raw = snapshot.raw.clone();
    restore_fields(&mut raw, manifest)?;

    let text = serialize_config(format, &raw)?;
    write_config_atomic(&snapshot.config_path, text.as_bytes())
}

/// Remove a nested JSON value by dotted path, leaving anything else untouched.
fn remove_nested(root: &mut serde_json::Value, path: &str) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return;
    }

    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        match current.get_mut(*part) {
            Some(next) if next.is_object() => current = next,
            _ => return,
        }
    }

    if let Some(obj) = current.as_object_mut() {
        obj.remove(*parts.last().unwrap());
    }
}

/// Get a nested JSON value by dotted path (e.g. "model.temperature").
#[allow(dead_code)]
fn get_nested<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root;
    for part in &parts {
        current = current.get(*part)?;
    }
    Some(current)
}

// ---------------------------------------------------------------------------
// Config formats
// ---------------------------------------------------------------------------

/// On-disk syntax of a client's configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFormat {
    /// JSON, used by Claude (`.claude.json`) and Gemini (`settings.json`).
    Json,
    /// TOML, used by Codex (`config.toml`).
    Toml,
}

/// Parse config text in `format` into JSON.
fn parse_config(format: ConfigFormat, text: &str) -> Result<serde_json::Value, String> {
    match format {
        ConfigFormat::Json => serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}")),
        ConfigFormat::Toml => parse_toml_subset(text),
    }
}

/// Serialize `raw` as config text in `format`.
fn serialize_config(format: ConfigFormat, raw: &serde_json::Value) -> Result<String, String> {
    match format {
        ConfigFormat::Json => {
            serde_json::to_string_pretty(raw).map_err(|e| format!("serialize failed: {e}"))
        }
        ConfigFormat::Toml => serialize_toml_subset(raw),
    }
}

/// Read and parse a client config, hashing the raw bytes for change detection.
fn read_snapshot(
    agent_type: AgentType,
    path: std::path::PathBuf,
    format: ConfigFormat,
) -> Result<AgentConfigSnapshot, String> {
    let (raw, hash) = if path.exists() {
        let data = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let hash = compute_hash(data.as_bytes());
        let parsed = parse_config(format, &data)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        (parsed, hash)
    } else {
        (
            serde_json::Value::Object(serde_json::Map::new()),
            String::new(),
        )
    };

    Ok(AgentConfigSnapshot {
        agent_type,
        config_path: path,
        raw,
        config_hash: hash,
    })
}

// ---------------------------------------------------------------------------
// Minimal TOML support for Codex `config.toml`
// ---------------------------------------------------------------------------
//
// Codex reads TOML, and this crate has no TOML dependency available to this
// module, so the subset below covers the shapes a Codex config uses: comments,
// `[table]` headers, dotted or quoted keys, basic and literal strings,
// integers, floats, booleans and single-line arrays of those scalars. Anything
// else (arrays of tables, inline tables, multi-line strings, dates) is rejected
// with an explicit error instead of being silently misread, so `read_config`
// fails loudly rather than letting the adapter write a file the client cannot
// parse.

/// Parse the supported TOML subset into a JSON value.
fn parse_toml_subset(text: &str) -> Result<serde_json::Value, String> {
    let mut root = serde_json::Value::Object(serde_json::Map::new());
    let mut table_path: Vec<String> = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("[[") {
            return Err(format!(
                "line {line_number}: arrays of tables are not supported"
            ));
        }

        if let Some(inner) = line.strip_prefix('[') {
            let inner = inner
                .strip_suffix(']')
                .ok_or_else(|| format!("line {line_number}: unterminated table header"))?
                .trim();
            let path = parse_toml_key(inner, line_number)?;
            toml_table_mut(&mut root, &path, line_number)?;
            table_path = path;
            continue;
        }

        let split = find_unquoted(line, '=')
            .ok_or_else(|| format!("line {line_number}: expected `key = value`"))?;
        let mut key = parse_toml_key(&line[..split], line_number)?;
        let value = parse_toml_value(line[split + 1..].trim(), line_number)?;

        let mut path = table_path.clone();
        path.append(&mut key);
        insert_toml_value(&mut root, &path, value, line_number)?;
    }

    Ok(root)
}

/// Remove a `#` comment, ignoring `#` inside quoted strings.
fn strip_toml_comment(line: &str) -> &str {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if in_basic {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_basic = false;
            }
        } else if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
        } else if ch == '"' {
            in_basic = true;
        } else if ch == '\'' {
            in_literal = true;
        } else if ch == '#' {
            return &line[..index];
        }
    }

    line
}

/// Find the first occurrence of `needle` outside of quoted strings.
fn find_unquoted(text: &str, needle: char) -> Option<usize> {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if in_basic {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_basic = false;
            }
        } else if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
        } else if ch == '"' {
            in_basic = true;
        } else if ch == '\'' {
            in_literal = true;
        } else if ch == needle {
            return Some(index);
        }
    }

    None
}

/// Parse a dotted, optionally quoted TOML key into its segments.
fn parse_toml_key(text: &str, line_number: usize) -> Result<Vec<String>, String> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;

    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }

        let segment = if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                if quote == b'"' && bytes[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            if end >= bytes.len() {
                return Err(format!("line {line_number}: unterminated quoted key"));
            }
            index = end + 1;
            text[start..end].to_string()
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'.'
            {
                index += 1;
            }
            if start == index {
                return Err(format!("line {line_number}: empty key segment"));
            }
            text[start..index].to_string()
        };

        if segment.is_empty() {
            return Err(format!("line {line_number}: empty key segment"));
        }
        parts.push(segment);

        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index] != b'.' {
            return Err(format!("line {line_number}: unexpected character in key"));
        }
        index += 1;
    }

    if parts.is_empty() {
        return Err(format!("line {line_number}: empty key"));
    }

    Ok(parts)
}

/// Navigate to a table, creating missing tables and rejecting conflicts.
fn toml_table_mut<'a>(
    root: &'a mut serde_json::Value,
    path: &[String],
    line_number: usize,
) -> Result<&'a mut serde_json::Value, String> {
    let mut current = root;

    for part in path {
        if !current.is_object() {
            return Err(format!("line {line_number}: `{part}` is not a table"));
        }
        let object = current.as_object_mut().unwrap();
        current = object
            .entry(part.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !current.is_object() {
            return Err(format!(
                "line {line_number}: `{part}` is a value, not a table"
            ));
        }
    }

    Ok(current)
}

/// Insert a key/value pair at `path`, rejecting duplicate keys.
fn insert_toml_value(
    root: &mut serde_json::Value,
    path: &[String],
    value: serde_json::Value,
    line_number: usize,
) -> Result<(), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| format!("line {line_number}: empty key"))?;
    let table = toml_table_mut(root, parents, line_number)?;
    let object = table.as_object_mut().unwrap();
    if object.contains_key(last) {
        return Err(format!("line {line_number}: duplicate key `{last}`"));
    }
    object.insert(last.clone(), value);
    Ok(())
}

/// Parse a single-line TOML value.
fn parse_toml_value(text: &str, line_number: usize) -> Result<serde_json::Value, String> {
    let mut parser = TomlValueParser {
        input: text,
        pos: 0,
    };
    let value = parser.parse_value(line_number)?;
    parser.skip_whitespace();
    if parser.pos != text.len() {
        return Err(format!(
            "line {line_number}: unexpected trailing text `{}`",
            &text[parser.pos..]
        ));
    }
    Ok(value)
}

/// Cursor over the text of one TOML value.
struct TomlValueParser<'a> {
    input: &'a str,
    pos: usize,
}

impl TomlValueParser<'_> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(format!("line {line_number}: expected a value")),
            Some('"') => self
                .parse_basic_string(line_number)
                .map(serde_json::Value::String),
            Some('\'') => self
                .parse_literal_string(line_number)
                .map(serde_json::Value::String),
            Some('[') => self.parse_array(line_number),
            Some('{') => Err(format!(
                "line {line_number}: inline tables are not supported"
            )),
            Some(_) => self.parse_bare_value(line_number),
        }
    }

    fn parse_basic_string(&mut self, line_number: usize) -> Result<String, String> {
        self.bump(); // opening quote
        let mut out = String::new();

        loop {
            match self.bump() {
                None => return Err(format!("line {line_number}: unterminated string")),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some(marker @ ('u' | 'U')) => {
                        let digits = if marker == 'u' { 4 } else { 8 };
                        let mut code = 0u32;
                        for _ in 0..digits {
                            let ch = self.bump().ok_or_else(|| {
                                format!("line {line_number}: unterminated unicode escape")
                            })?;
                            let digit = ch.to_digit(16).ok_or_else(|| {
                                format!("line {line_number}: invalid unicode escape digit {ch:?}")
                            })?;
                            code = code * 16 + digit;
                        }
                        let ch = char::from_u32(code).ok_or_else(|| {
                            format!("line {line_number}: invalid unicode scalar {code:#x}")
                        })?;
                        out.push(ch);
                    }
                    Some(other) => {
                        return Err(format!(
                            "line {line_number}: unsupported escape `\\{other}`"
                        ))
                    }
                    None => return Err(format!("line {line_number}: unterminated string")),
                },
                Some(ch) => out.push(ch),
            }
        }
    }

    fn parse_literal_string(&mut self, line_number: usize) -> Result<String, String> {
        self.bump(); // opening quote
        let start = self.pos;
        while let Some(ch) = self.bump() {
            if ch == '\'' {
                return Ok(self.input[start..self.pos - 1].to_string());
            }
        }
        Err(format!("line {line_number}: unterminated string"))
    }

    fn parse_array(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.bump(); // '['
        let mut items = Vec::new();

        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(format!("line {line_number}: unterminated array")),
                Some(']') => {
                    self.bump();
                    return Ok(serde_json::Value::Array(items));
                }
                _ => {}
            }

            items.push(self.parse_value(line_number)?);
            self.skip_whitespace();

            match self.bump() {
                Some(',') => continue,
                Some(']') => return Ok(serde_json::Value::Array(items)),
                Some(other) => {
                    return Err(format!(
                        "line {line_number}: expected `,` or `]` in array, found {other:?}"
                    ))
                }
                None => return Err(format!("line {line_number}: unterminated array")),
            }
        }
    }

    fn parse_bare_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_' | '.') {
                self.bump();
            } else {
                break;
            }
        }

        let token = &self.input[start..self.pos];
        match token {
            "true" => return Ok(serde_json::Value::Bool(true)),
            "false" => return Ok(serde_json::Value::Bool(false)),
            "" => {
                return Err(format!(
                    "line {line_number}: expected a value, found {:?}",
                    self.peek()
                ))
            }
            _ => {}
        }

        let cleaned = token.replace('_', "");
        if let Ok(integer) = cleaned.parse::<i64>() {
            return Ok(serde_json::Value::Number(integer.into()));
        }
        if let Ok(float) = cleaned.parse::<f64>() {
            if let Some(number) = serde_json::Number::from_f64(float) {
                return Ok(serde_json::Value::Number(number));
            }
        }

        Err(format!(
            "line {line_number}: unsupported TOML value `{token}`"
        ))
    }
}

/// Serialize a JSON object as the supported TOML subset.
fn serialize_toml_subset(value: &serde_json::Value) -> Result<String, String> {
    let table = value
        .as_object()
        .ok_or_else(|| "TOML config must be a table".to_string())?;
    let mut out = String::new();
    write_toml_table(&mut out, &[], table)?;
    Ok(out)
}

/// Write one table: its scalar keys first, then its child sections.
fn write_toml_table(
    out: &mut String,
    path: &[String],
    table: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if !path.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push('[');
        out.push_str(
            &path
                .iter()
                .map(|segment| quote_toml_key(segment))
                .collect::<Vec<_>>()
                .join("."),
        );
        out.push_str("]\n");
    }

    let mut nested = Vec::new();
    for (key, value) in table {
        if value.is_object() {
            nested.push((key, value));
            continue;
        }
        out.push_str(&quote_toml_key(key));
        out.push_str(" = ");
        out.push_str(&format_toml_value(value)?);
        out.push('\n');
    }

    for (key, value) in nested {
        let mut child = path.to_vec();
        child.push(key.clone());
        write_toml_table(out, &child, value.as_object().unwrap())?;
    }

    Ok(())
}

fn format_toml_value(value: &serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::Null => Err("TOML has no null value".to_string()),
        serde_json::Value::Bool(flag) => Ok(flag.to_string()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        serde_json::Value::String(text) => Ok(quote_toml_string(text)),
        serde_json::Value::Array(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                if item.is_object() {
                    return Err("arrays of tables are not supported".to_string());
                }
                rendered.push(format_toml_value(item)?);
            }
            Ok(format!("[{}]", rendered.join(", ")))
        }
        serde_json::Value::Object(_) => {
            Err("nested tables must be written as sections".to_string())
        }
    }
}

/// Quote a key unless it is a bare key.
fn quote_toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if bare {
        key.to_string()
    } else {
        quote_toml_string(key)
    }
}

/// Quote a string as a TOML basic string.
fn quote_toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 || ch == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn field_map(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn adopt_sets_adopted_at() {
        let store = TakeoverStore::new();
        let values = field_map(&[("timeout", 30.into())]);
        let manifest = store.adopt(vec!["timeout".into()], &values).unwrap();

        assert!(manifest.adopted_at.is_some());
        assert!(manifest.adopted_at.unwrap() > 0);
        assert_eq!(manifest.state, OwnershipState::Adopted);
    }

    #[test]
    fn release_sets_released_at() {
        let store = TakeoverStore::new();
        let values = field_map(&[("timeout", 30.into())]);
        store.adopt(vec!["timeout".into()], &values).unwrap();

        let manifest = store.release().unwrap();

        assert!(manifest.released_at.is_some());
        assert!(manifest.released_at.unwrap() > 0);
        assert_eq!(manifest.state, OwnershipState::Released);
    }

    #[test]
    fn adopt_requires_verified_state() {
        let store = TakeoverStore::new();
        let values = field_map(&[("x", 1.into())]);

        // First adopt succeeds.
        store.adopt(vec!["x".into()], &values).unwrap();

        // Second adopt fails — state is Adopted, not Verified.
        let err = store.adopt(vec!["x".into()], &values).unwrap_err();
        assert!(err.contains("Adopted"), "error: {err}");
        assert!(err.contains("Verified or Released"), "error: {err}");
    }

    #[test]
    fn release_requires_adopted_state() {
        let store = TakeoverStore::new();

        // Cannot release from Verified state.
        let err = store.release().unwrap_err();
        assert!(err.contains("Verified"), "error: {err}");
    }

    #[test]
    fn detect_external_modification_finds_changes() {
        let store = TakeoverStore::new();
        let original = field_map(&[("a", 1.into()), ("b", 2.into())]);
        store
            .adopt(vec!["a".into(), "b".into()], &original)
            .unwrap();

        let changed = field_map(&[("a", 99.into()), ("b", 2.into())]);
        let mods = store.detect_external_modification(&changed);

        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].field_path, "a");
        assert_eq!(mods[0].last_applied, Some(serde_json::json!(1)));
        assert_eq!(mods[0].current_value, serde_json::json!(99));
    }

    #[test]
    fn detect_external_modification_no_change() {
        let store = TakeoverStore::new();
        let values = field_map(&[("a", 1.into()), ("b", 2.into())]);
        store.adopt(vec!["a".into(), "b".into()], &values).unwrap();

        let same = field_map(&[("a", 1.into()), ("b", 2.into())]);
        let mods = store.detect_external_modification(&same);
        assert!(mods.is_empty());
    }

    #[test]
    fn adopt_release_cycle_x10_no_drift() {
        let store = TakeoverStore::new();
        let fields = vec!["f1".into(), "f2".into()];

        for i in 0..10u64 {
            let values = field_map(&[
                ("f1", serde_json::json!(i)),
                ("f2", serde_json::json!(i * 10)),
            ]);

            let manifest = store.adopt(fields.clone(), &values).unwrap();
            assert_eq!(manifest.state, OwnershipState::Adopted);
            assert_eq!(manifest.generation, i);
            assert_eq!(manifest.managed_fields, fields);
            assert_eq!(manifest.field_snapshots.len(), 2);

            let released = store.release().unwrap();
            assert_eq!(released.state, OwnershipState::Released);
        }

        // After 10 cycles the generation should be 10.
        let final_manifest = store.manifest().unwrap();
        assert_eq!(final_manifest.generation, 10);
        assert_eq!(final_manifest.managed_fields, fields);
    }

    #[test]
    fn unmanaged_fields_not_in_manifest() {
        let store = TakeoverStore::new();
        let values = field_map(&[
            ("a", 1.into()),
            ("b", 2.into()),
            ("c", 3.into()), // not managed
        ]);

        // Only "a" and "b" are managed.
        let manifest = store.adopt(vec!["a".into(), "b".into()], &values).unwrap();

        assert_eq!(manifest.managed_fields.len(), 2);
        assert!(manifest.managed_fields.contains(&"a".into()));
        assert!(manifest.managed_fields.contains(&"b".into()));
        assert!(!manifest.managed_fields.contains(&"c".into()));

        // Snapshot only contains managed fields.
        assert!(manifest.field_snapshots.contains_key("a"));
        assert!(manifest.field_snapshots.contains_key("b"));
        assert!(!manifest.field_snapshots.contains_key("c"));
    }

    #[test]
    fn conflict_detection_on_managed_field_change() {
        let store = TakeoverStore::new();

        // Adopt with initial values.
        let initial = field_map(&[("a", 1.into()), ("b", 2.into())]);
        store.adopt(vec!["a".into(), "b".into()], &initial).unwrap();

        // Externally change managed field "a".
        let current = field_map(&[("a", 42.into()), ("b", 2.into())]);
        let mods = store.detect_external_modification(&current);

        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].field_path, "a");
        assert_eq!(mods[0].last_applied, Some(serde_json::json!(1)));
        assert_eq!(mods[0].current_value, serde_json::json!(42));

        // Field "b" is unchanged — no conflict.
        assert!(!mods.iter().any(|m| m.field_path == "b"));
    }

    // -----------------------------------------------------------------------
    // I4 verification tests
    // -----------------------------------------------------------------------

    #[test]
    fn full_lifecycle_detected_to_adopted_to_released() {
        let store = TakeoverStore::new();

        // Initial state is Verified.
        assert_eq!(store.state(), OwnershipState::Verified);

        // Adopt: Verified -> Adopted.
        let values = field_map(&[("model", "gpt-4".into()), ("temp", 0.7.into())]);
        let adopted = store
            .adopt(vec!["model".into(), "temp".into()], &values)
            .unwrap();

        assert_eq!(adopted.state, OwnershipState::Adopted);
        assert_eq!(store.state(), OwnershipState::Adopted);
        assert!(adopted.adopted_at.is_some());
        assert!(adopted.released_at.is_none());
        assert_eq!(adopted.generation, 0);
        assert_eq!(adopted.managed_fields.len(), 2);
        assert_eq!(adopted.field_snapshots["model"], serde_json::json!("gpt-4"));
        assert_eq!(adopted.field_snapshots["temp"], serde_json::json!(0.7));

        // Release: Adopted -> Released.
        let released = store.release().unwrap();

        assert_eq!(released.state, OwnershipState::Released);
        assert_eq!(store.state(), OwnershipState::Released);
        assert!(released.released_at.is_some());
        assert!(released.released_at.unwrap() >= released.adopted_at.unwrap());
        assert_eq!(released.generation, 1);
        // Managed fields and snapshots persist through release.
        assert_eq!(released.managed_fields, vec!["model", "temp"]);
        assert_eq!(
            released.field_snapshots["model"],
            serde_json::json!("gpt-4")
        );
    }

    #[test]
    fn external_modification_detected_and_reported() {
        let store = TakeoverStore::new();

        let initial = field_map(&[
            ("host", "localhost".into()),
            ("port", 8080.into()),
            ("debug", false.into()),
        ]);
        store
            .adopt(vec!["host".into(), "port".into(), "debug".into()], &initial)
            .unwrap();

        // Simulate external modification: host changed, port removed, debug unchanged.
        let current = field_map(&[("host", "0.0.0.0".into()), ("debug", false.into())]);
        let mods = store.detect_external_modification(&current);

        assert_eq!(mods.len(), 2);

        // host was changed.
        let host_mod = mods.iter().find(|m| m.field_path == "host").unwrap();
        assert_eq!(host_mod.last_applied, Some(serde_json::json!("localhost")));
        assert_eq!(host_mod.current_value, serde_json::json!("0.0.0.0"));

        // port was removed (reported as null).
        let port_mod = mods.iter().find(|m| m.field_path == "port").unwrap();
        assert_eq!(port_mod.last_applied, Some(serde_json::json!(8080)));
        assert_eq!(port_mod.current_value, serde_json::Value::Null);
    }

    #[test]
    fn user_modification_preserved_on_release() {
        let store = TakeoverStore::new();

        let initial = field_map(&[("x", 1.into()), ("y", 2.into())]);
        store.adopt(vec!["x".into(), "y".into()], &initial).unwrap();

        // User externally modifies "x" while Adopted.
        let current = field_map(&[("x", 100.into()), ("y", 2.into())]);
        let mods = store.detect_external_modification(&current);
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].field_path, "x");

        // Release still succeeds — external changes do not block release.
        let released = store.release().unwrap();
        assert_eq!(released.state, OwnershipState::Released);

        // Re-adopt with the user's modified values (x=100 preserved).
        let user_values = field_map(&[("x", 100.into()), ("y", 2.into())]);
        let re_adopted = store
            .adopt(vec!["x".into(), "y".into()], &user_values)
            .unwrap();

        assert_eq!(re_adopted.field_snapshots["x"], serde_json::json!(100));
        assert_eq!(re_adopted.field_snapshots["y"], serde_json::json!(2));

        // No further external modifications detected — user values are now baseline.
        let mods = store.detect_external_modification(&user_values);
        assert!(mods.is_empty());
    }

    #[test]
    fn repeated_adopt_release_no_drift() {
        let store = TakeoverStore::new();
        let fields = vec!["a".into(), "b".into()];

        for cycle in 0..50u64 {
            let val = cycle * 3;
            let values = field_map(&[
                ("a", serde_json::json!(val)),
                ("b", serde_json::json!(val + 1)),
            ]);

            let adopted = store.adopt(fields.clone(), &values).unwrap();
            assert_eq!(adopted.state, OwnershipState::Adopted);
            assert_eq!(adopted.generation, cycle);
            assert_eq!(adopted.managed_fields, fields);
            assert_eq!(adopted.field_snapshots.len(), 2);
            assert_eq!(adopted.field_snapshots["a"], serde_json::json!(val));

            let released = store.release().unwrap();
            assert_eq!(released.state, OwnershipState::Released);
            assert_eq!(released.generation, cycle + 1);
            // Managed fields survive across cycles.
            assert_eq!(released.managed_fields, fields);
        }

        // Final manifest should reflect last generation.
        let manifest = store.manifest().unwrap();
        assert_eq!(manifest.generation, 50);
        assert_eq!(manifest.state, OwnershipState::Released);
        assert_eq!(manifest.managed_fields, fields);
    }

    #[test]
    fn concurrent_access_safety() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(TakeoverStore::new());
        let fields: Vec<String> = vec!["f1".into(), "f2".into()];

        // First adopt so multiple threads can detect modifications concurrently.
        let init = field_map(&[("f1", 0.into()), ("f2", 0.into())]);
        store.adopt(fields.clone(), &init).unwrap();

        let mut handles = Vec::new();

        // Spawn readers that call detect_external_modification concurrently.
        for i in 0..8 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let current =
                    field_map(&[("f1", serde_json::json!(i)), ("f2", serde_json::json!(i))]);
                let mods = s.detect_external_modification(&current);
                // Both fields differ from baseline (0) when i != 0.
                if i != 0 {
                    assert!(!mods.is_empty());
                }
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }

        // Release after concurrent reads — must still be in Adopted state.
        let released = store.release().unwrap();
        assert_eq!(released.state, OwnershipState::Released);
    }

    #[test]
    fn ownership_manifest_serde_round_trip() {
        let store = TakeoverStore::new();
        let values = field_map(&[("k", serde_json::json!("v")), ("n", serde_json::json!(42))]);
        let original = store.adopt(vec!["k".into(), "n".into()], &values).unwrap();

        // Serialize to JSON.
        let json = serde_json::to_string(&original).expect("serialize");

        // Deserialize back.
        let restored: OwnershipManifest = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.state, original.state);
        assert_eq!(restored.managed_fields, original.managed_fields);
        assert_eq!(restored.field_snapshots, original.field_snapshots);
        assert_eq!(restored.absent_fields, original.absent_fields);
        assert_eq!(restored.adopted_at, original.adopted_at);
        assert_eq!(restored.released_at, original.released_at);
        assert_eq!(restored.generation, original.generation);

        // Also round-trip a released manifest.
        let released = store.release().unwrap();
        let json2 = serde_json::to_string(&released).expect("serialize released");
        let restored2: OwnershipManifest =
            serde_json::from_str(&json2).expect("deserialize released");

        assert_eq!(restored2.state, OwnershipState::Released);
        assert!(restored2.released_at.is_some());
        assert_eq!(restored2.generation, 1);
    }

    // -----------------------------------------------------------------------
    // AgentAdapter tests
    // -----------------------------------------------------------------------

    #[test]
    fn claude_adapter_agent_type() {
        super::isolate_agent_home();
        let adapter = ClaudeAdapter;
        assert_eq!(adapter.agent_type(), AgentType::Claude);
    }

    #[test]
    fn codex_adapter_agent_type() {
        super::isolate_agent_home();
        let adapter = CodexAdapter;
        assert_eq!(adapter.agent_type(), AgentType::Codex);
    }

    #[test]
    fn gemini_adapter_agent_type() {
        super::isolate_agent_home();
        let adapter = GeminiAdapter;
        assert_eq!(adapter.agent_type(), AgentType::Gemini);
    }

    #[test]
    fn claude_adapter_config_path() {
        super::isolate_agent_home();
        let adapter = ClaudeAdapter;
        let path = adapter.config_path().unwrap();
        assert!(path.ends_with(".claude.json"));
    }

    #[test]
    fn codex_adapter_config_path() {
        super::isolate_agent_home();
        let adapter = CodexAdapter;
        let path = adapter.config_path().unwrap();
        assert!(path.ends_with(".codex/config.toml"));
    }

    #[test]
    fn gemini_adapter_config_path() {
        super::isolate_agent_home();
        let adapter = GeminiAdapter;
        let path = adapter.config_path().unwrap();
        assert!(path.ends_with(".gemini/settings.json"));
    }

    #[test]
    fn codex_adapter_honours_config_dir_override() {
        super::isolate_agent_home();
        let custom = tempfile::tempdir().unwrap();
        super::isolate_config_dir("CODEX_HOME", custom.path());

        let adapter = CodexAdapter;
        assert_eq!(
            adapter.config_path().unwrap(),
            custom.path().join("config.toml")
        );
    }

    #[test]
    fn gemini_adapter_honours_config_dir_override() {
        super::isolate_agent_home();
        let custom = tempfile::tempdir().unwrap();
        super::isolate_config_dir("GEMINI_CLI_HOME", custom.path());

        let adapter = GeminiAdapter;
        assert_eq!(
            adapter.config_path().unwrap(),
            custom.path().join(".gemini").join("settings.json")
        );
    }

    #[test]
    fn claude_adapter_read_config() {
        super::isolate_agent_home();
        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.agent_type, AgentType::Claude);
        // Config file likely does not exist in CI, so raw should be empty object.
        assert!(snapshot.raw.is_object());
    }

    #[test]
    fn codex_adapter_read_config() {
        super::isolate_agent_home();
        let adapter = CodexAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.agent_type, AgentType::Codex);
        assert!(snapshot.raw.is_object());
    }

    #[test]
    fn codex_apply_patch_writes_toml_config() {
        super::isolate_agent_home();
        let home = super::isolated_agent_home().expect("isolated agent home");
        let path = home.join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            concat!(
                "model = \"gpt-5\"\n",
                "approval_policy = \"never\" # keep this\n",
                "\n",
                "[model_providers.zroutery]\n",
                "base_url = \"http://127.0.0.1:1/v1\"\n",
            ),
        )
        .unwrap();

        let adapter = CodexAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.raw["model"], serde_json::json!("gpt-5"));
        assert_eq!(
            snapshot.raw["model_providers"]["zroutery"]["base_url"],
            serde_json::json!("http://127.0.0.1:1/v1")
        );

        let patched = adapter
            .apply_patch(
                &snapshot,
                &[
                    ManagedField {
                        path: "model".into(),
                        value: serde_json::json!("zroutery-proxy"),
                    },
                    ManagedField {
                        path: "model_providers.zroutery.base_url".into(),
                        value: serde_json::json!("http://127.0.0.1:9999/v1"),
                    },
                ],
            )
            .unwrap();
        assert_eq!(patched.raw["model"], serde_json::json!("zroutery-proxy"));

        // The file on disk is TOML, not JSON, and unmanaged keys survive.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.trim_start().starts_with('{'),
            "codex config is not TOML: {text}"
        );
        assert!(text.contains("model = \"zroutery-proxy\""), "text: {text}");
        assert!(text.contains("approval_policy = \"never\""), "text: {text}");
        assert!(text.contains("[model_providers.zroutery]"), "text: {text}");
        assert!(
            text.contains("base_url = \"http://127.0.0.1:9999/v1\""),
            "text: {text}"
        );

        // What was written round-trips through the adapter's own reader.
        let reread = adapter.read_config().unwrap();
        assert_eq!(reread.raw["model"], serde_json::json!("zroutery-proxy"));
        assert_eq!(
            reread.raw["model_providers"]["zroutery"]["base_url"],
            serde_json::json!("http://127.0.0.1:9999/v1")
        );
        assert_eq!(reread.raw["approval_policy"], serde_json::json!("never"));
    }

    #[test]
    fn codex_read_config_rejects_unsupported_toml() {
        super::isolate_agent_home();
        let home = super::isolated_agent_home().expect("isolated agent home");
        let path = home.join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[[profiles]]\nname = \"work\"\n").unwrap();

        let adapter = CodexAdapter;
        let err = adapter.read_config().unwrap_err();
        assert!(err.contains("arrays of tables"), "error: {err}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("[[profiles]]"),
            "the unreadable config must be left untouched"
        );
    }

    #[test]
    fn gemini_adapter_read_config() {
        super::isolate_agent_home();
        let adapter = GeminiAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.agent_type, AgentType::Gemini);
        assert!(snapshot.raw.is_object());
    }

    #[test]
    fn apply_patch_modifies_snapshot() {
        super::isolate_agent_home();
        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();

        let fields = vec![
            ManagedField {
                path: "model".into(),
                value: serde_json::json!("claude-3-opus"),
            },
            ManagedField {
                path: "temperature".into(),
                value: serde_json::json!(0.7),
            },
        ];

        let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

        assert_eq!(patched.raw["model"], serde_json::json!("claude-3-opus"));
        assert_eq!(patched.raw["temperature"], serde_json::json!(0.7));
        // Note: apply_patch writes to the real config file on disk, so the
        // original snapshot may already contain the patched fields if the
        // test has run before. We verify the patched values are correct above.
        // A full immutability assertion is covered by the TestAdapter-based
        // tests below which use isolated temp directories.
    }

    #[test]
    fn apply_patch_nested_path() {
        super::isolate_agent_home();
        let adapter = CodexAdapter;
        let snapshot = adapter.read_config().unwrap();

        let fields = vec![ManagedField {
            path: "model.temperature".into(),
            value: serde_json::json!(0.9),
        }];

        let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

        assert_eq!(patched.raw["model"]["temperature"], serde_json::json!(0.9));
    }

    #[test]
    fn release_returns_ok() {
        super::isolate_agent_home();
        let adapter = GeminiAdapter;
        let snapshot = adapter.read_config().unwrap();

        let store = TakeoverStore::new();
        let values = field_map(&[("key", serde_json::json!("val"))]);
        store.adopt(vec!["key".into()], &values).unwrap();
        let manifest = store.release().unwrap();

        assert!(adapter.release(&snapshot, &manifest).is_ok());
    }

    #[test]
    fn all_adapters_release_ok() {
        super::isolate_agent_home();
        let store = TakeoverStore::new();
        let values = field_map(&[("x", 1.into())]);
        store.adopt(vec!["x".into()], &values).unwrap();
        let manifest = store.release().unwrap();

        let adapters: Vec<Box<dyn AgentAdapter>> = vec![
            Box::new(ClaudeAdapter),
            Box::new(CodexAdapter),
            Box::new(GeminiAdapter),
        ];

        for adapter in &adapters {
            let snapshot = adapter.read_config().unwrap();
            assert!(adapter.release(&snapshot, &manifest).is_ok());
        }
    }

    // -----------------------------------------------------------------------
    // TestAdapter (temp-file backed)
    // -----------------------------------------------------------------------

    /// Agent adapter backed by a temp file for deterministic tests.
    struct TestAdapter {
        path: std::path::PathBuf,
    }

    impl TestAdapter {
        fn new(dir: &std::path::Path, initial: serde_json::Value) -> Self {
            let path = dir.join("config.json");
            std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();
            Self { path }
        }
    }

    impl AgentAdapter for TestAdapter {
        fn agent_type(&self) -> AgentType {
            AgentType::Claude
        }

        fn config_path(&self) -> Result<std::path::PathBuf, String> {
            Ok(self.path.clone())
        }

        fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
            let (raw, hash) = if self.path.exists() {
                let data =
                    std::fs::read_to_string(&self.path).map_err(|e| format!("read failed: {e}"))?;
                let hash = compute_hash(data.as_bytes());
                let parsed: serde_json::Value =
                    serde_json::from_str(&data).map_err(|e| format!("parse failed: {e}"))?;
                (parsed, hash)
            } else {
                (
                    serde_json::Value::Object(serde_json::Map::new()),
                    String::new(),
                )
            };
            Ok(AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: self.path.clone(),
                raw,
                config_hash: hash,
            })
        }

        fn apply_patch(
            &self,
            snapshot: &AgentConfigSnapshot,
            fields: &[ManagedField],
        ) -> Result<AgentConfigSnapshot, String> {
            let mut raw = snapshot.raw.clone();
            apply_fields(&mut raw, fields)?;
            let json =
                serde_json::to_string_pretty(&raw).map_err(|e| format!("serialize failed: {e}"))?;
            let hash = compute_hash(json.as_bytes());
            Ok(AgentConfigSnapshot {
                agent_type: snapshot.agent_type,
                config_path: snapshot.config_path.clone(),
                raw,
                config_hash: hash,
            })
        }

        fn release(
            &self,
            snapshot: &AgentConfigSnapshot,
            manifest: &OwnershipManifest,
        ) -> Result<(), String> {
            let mut raw = snapshot.raw.clone();
            restore_fields(&mut raw, manifest)?;
            let json =
                serde_json::to_string_pretty(&raw).map_err(|e| format!("serialize failed: {e}"))?;
            let hash = compute_hash(json.as_bytes());
            let restored = AgentConfigSnapshot {
                agent_type: snapshot.agent_type,
                config_path: snapshot.config_path.clone(),
                raw,
                config_hash: hash,
            };
            self.write_config(&restored)
        }
    }

    /// Read a JSON config file from disk.
    fn read_json_file(path: &std::path::Path) -> serde_json::Value {
        let data = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&data).unwrap()
    }

    // -----------------------------------------------------------------------
    // I4: release_with_restore
    // -----------------------------------------------------------------------

    #[test]
    fn release_with_restore_restores_config() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({
            "model": "gpt-4",
            "temperature": 0.7,
            "unmanaged": "preserved"
        });
        let adapter = TestAdapter::new(tmp.path(), initial.clone());

        let store = TakeoverStore::new();

        // Adopt: snapshot the current values.
        let current = field_map(&[
            ("model", serde_json::json!("gpt-4")),
            ("temperature", serde_json::json!(0.7)),
        ]);
        store
            .adopt(vec!["model".into(), "temperature".into()], &current)
            .unwrap();

        // Simulate Zroutery patching the config.
        let patched = serde_json::json!({
            "model": "claude-3-opus",
            "temperature": 0.9,
            "unmanaged": "preserved"
        });
        adapter
            .write_config(&AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: adapter.path.clone(),
                raw: patched,
                config_hash: String::new(),
            })
            .unwrap();

        // Release with restore.
        let manifest = store.release_with_restore(&adapter).unwrap();
        assert_eq!(manifest.state, OwnershipState::Released);
        assert!(manifest.released_at.is_some());
        assert_eq!(manifest.generation, 1);

        // Verify config on disk was restored.
        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["model"], serde_json::json!("gpt-4"));
        assert_eq!(disk["temperature"], serde_json::json!(0.7));
        assert_eq!(disk["unmanaged"], serde_json::json!("preserved"));
    }

    #[test]
    fn release_with_restore_no_manifest_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new(tmp.path(), serde_json::json!({"x": 1}));

        let store = TakeoverStore::new();

        // Not adopted yet — should error.
        let err = store.release_with_restore(&adapter).unwrap_err();
        assert!(err.contains("Adopted"), "error: {err}");
    }

    #[test]
    fn release_with_restore_preserves_unmanaged_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({
            "managed_a": "original_a",
            "managed_b": 42,
            "unmanaged": "keep_me"
        });
        let adapter = TestAdapter::new(tmp.path(), initial);

        let store = TakeoverStore::new();
        let current = field_map(&[
            ("managed_a", serde_json::json!("original_a")),
            ("managed_b", serde_json::json!(42)),
        ]);
        store
            .adopt(vec!["managed_a".into(), "managed_b".into()], &current)
            .unwrap();

        // Simulate Zroutery changing managed fields.
        let patched = serde_json::json!({
            "managed_a": "changed",
            "managed_b": 99,
            "unmanaged": "keep_me"
        });
        adapter
            .write_config(&AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: adapter.path.clone(),
                raw: patched,
                config_hash: String::new(),
            })
            .unwrap();

        store.release_with_restore(&adapter).unwrap();

        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["managed_a"], serde_json::json!("original_a"));
        assert_eq!(disk["managed_b"], serde_json::json!(42));
        assert_eq!(disk["unmanaged"], serde_json::json!("keep_me"));
    }

    #[test]
    fn release_removes_fields_absent_before_adoption() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new(tmp.path(), serde_json::json!({"model": "gpt-4"}));

        let store = TakeoverStore::new();
        // "base_url" is managed but does not exist in the config yet.
        let current = field_map(&[("model", serde_json::json!("gpt-4"))]);
        let manifest = store
            .adopt(vec!["model".into(), "base_url".into()], &current)
            .unwrap();
        assert_eq!(manifest.absent_fields, vec!["base_url".to_string()]);

        // Zroutery points the client at the local proxy.
        let patched = serde_json::json!({
            "model": "zroutery-proxy",
            "base_url": "http://127.0.0.1:9999"
        });
        adapter
            .write_config(&AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: adapter.path.clone(),
                raw: patched,
                config_hash: String::new(),
            })
            .unwrap();

        store.release_with_restore(&adapter).unwrap();

        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["model"], serde_json::json!("gpt-4"));
        assert!(
            disk.get("base_url").is_none(),
            "field absent before adoption survived release: {}",
            disk["base_url"]
        );
    }

    #[test]
    fn manifest_without_absent_fields_deserializes() {
        let json = r#"{
            "state": "released",
            "managed_fields": ["model"],
            "field_snapshots": {"model": "gpt-4"},
            "adopted_at": 1700000000,
            "released_at": 1700000100,
            "generation": 1
        }"#;

        let manifest: OwnershipManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.absent_fields.is_empty());
    }

    /// Adapter that re-enters the store while a release is being restored, to
    /// prove the in-flight transition is not observable as `Adopted`.
    struct RacingAdapter {
        path: std::path::PathBuf,
        store: std::sync::Arc<TakeoverStore>,
        competing_release: std::sync::Mutex<Option<Result<OwnershipManifest, String>>>,
        competing_adopt: std::sync::Mutex<Option<Result<OwnershipManifest, String>>>,
    }

    impl RacingAdapter {
        fn new(
            dir: &std::path::Path,
            initial: serde_json::Value,
            store: std::sync::Arc<TakeoverStore>,
        ) -> Self {
            let path = dir.join("config.json");
            std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();
            Self {
                path,
                store,
                competing_release: std::sync::Mutex::new(None),
                competing_adopt: std::sync::Mutex::new(None),
            }
        }
    }

    impl AgentAdapter for RacingAdapter {
        fn agent_type(&self) -> AgentType {
            AgentType::Claude
        }

        fn config_path(&self) -> Result<std::path::PathBuf, String> {
            Ok(self.path.clone())
        }

        fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
            // A competing release must not observe the pre-transition state.
            *self.competing_release.lock().unwrap() = Some(self.store.release());

            let (raw, hash) = if self.path.exists() {
                let data =
                    std::fs::read_to_string(&self.path).map_err(|e| format!("read failed: {e}"))?;
                let hash = compute_hash(data.as_bytes());
                let parsed: serde_json::Value =
                    serde_json::from_str(&data).map_err(|e| format!("parse failed: {e}"))?;
                (parsed, hash)
            } else {
                (
                    serde_json::Value::Object(serde_json::Map::new()),
                    String::new(),
                )
            };

            Ok(AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: self.path.clone(),
                raw,
                config_hash: hash,
            })
        }

        fn apply_patch(
            &self,
            snapshot: &AgentConfigSnapshot,
            fields: &[ManagedField],
        ) -> Result<AgentConfigSnapshot, String> {
            apply_patch_to_disk(snapshot, fields, ConfigFormat::Json)
        }

        fn release(
            &self,
            snapshot: &AgentConfigSnapshot,
            manifest: &OwnershipManifest,
        ) -> Result<(), String> {
            // The second phase of the restore: an adopt must not be able to
            // start from the stale `Adopted` state either.
            let values = field_map(&[("model", serde_json::json!("gpt-4"))]);
            *self.competing_adopt.lock().unwrap() =
                Some(self.store.adopt(vec!["model".into()], &values));

            release_to_disk(snapshot, manifest, ConfigFormat::Json)
        }
    }

    #[test]
    fn release_with_restore_blocks_concurrent_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(TakeoverStore::new());
        let adapter = RacingAdapter::new(
            tmp.path(),
            serde_json::json!({"model": "gpt-4"}),
            std::sync::Arc::clone(&store),
        );

        let values = field_map(&[("model", serde_json::json!("gpt-4"))]);
        store.adopt(vec!["model".into()], &values).unwrap();

        // Zroutery patched the config before releasing.
        adapter
            .write_config(&AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: adapter.path.clone(),
                raw: serde_json::json!({"model": "zroutery-proxy"}),
                config_hash: String::new(),
            })
            .unwrap();

        let manifest = store.release_with_restore(&adapter).unwrap();
        assert_eq!(manifest.state, OwnershipState::Released);
        assert_eq!(manifest.generation, 1);

        let release_attempt = adapter
            .competing_release
            .lock()
            .unwrap()
            .take()
            .expect("competing release was attempted");
        let err = release_attempt.expect_err("a competing release must be rejected");
        assert!(err.contains("Releasing"), "error: {err}");

        let adopt_attempt = adapter
            .competing_adopt
            .lock()
            .unwrap()
            .take()
            .expect("competing adopt was attempted");
        let err = adopt_attempt.expect_err("a competing adopt must be rejected");
        assert!(err.contains("Releasing"), "error: {err}");

        // The in-flight release still committed and restored the original value.
        assert_eq!(store.state(), OwnershipState::Released);
        assert_eq!(store.manifest().unwrap().generation, 1);
        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["model"], serde_json::json!("gpt-4"));
    }

    // -----------------------------------------------------------------------
    // I4: resolve_conflicts
    // -----------------------------------------------------------------------
    #[test]
    fn resolve_conflicts_keep_external() {
        let conflicts = vec![
            FieldConflict {
                field_path: "a".into(),
                original_value: Some(serde_json::json!(1)),
                last_applied: Some(serde_json::json!(1)),
                current_external: serde_json::json!(99),
            },
            FieldConflict {
                field_path: "b".into(),
                original_value: Some(serde_json::json!(2)),
                last_applied: Some(serde_json::json!(2)),
                current_external: serde_json::json!("changed"),
            },
        ];

        let resolved = resolve_conflicts(&conflicts, ConflictResolution::KeepExternal);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], ("a".into(), Some(serde_json::json!(99))));
        assert_eq!(
            resolved[1],
            ("b".into(), Some(serde_json::json!("changed")))
        );
    }

    #[test]
    fn resolve_conflicts_overwrite_with_managed() {
        let conflicts = vec![FieldConflict {
            field_path: "x".into(),
            original_value: Some(serde_json::json!(1)),
            last_applied: Some(serde_json::json!(10)),
            current_external: serde_json::json!(999),
        }];

        let resolved = resolve_conflicts(&conflicts, ConflictResolution::OverwriteWithManaged);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], ("x".into(), Some(serde_json::json!(10))));
    }

    #[test]
    fn resolve_conflicts_skip() {
        let conflicts = vec![
            FieldConflict {
                field_path: "a".into(),
                original_value: Some(serde_json::json!(1)),
                last_applied: Some(serde_json::json!(1)),
                current_external: serde_json::json!(99),
            },
            FieldConflict {
                field_path: "b".into(),
                original_value: Some(serde_json::json!(2)),
                last_applied: Some(serde_json::json!(2)),
                current_external: serde_json::json!("changed"),
            },
        ];

        let resolved = resolve_conflicts(&conflicts, ConflictResolution::Skip);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], ("a".into(), None));
        assert_eq!(resolved[1], ("b".into(), None));
    }

    #[test]
    fn resolve_conflicts_empty() {
        let resolved = resolve_conflicts(&[], ConflictResolution::KeepExternal);
        assert!(resolved.is_empty());
    }

    // -----------------------------------------------------------------------
    // I4: check_orphaned_state / recover_orphaned
    // -----------------------------------------------------------------------

    #[test]
    fn check_orphaned_state_finds_adopted_not_released() {
        let store = TakeoverStore::new();
        assert!(!store.check_orphaned_state());

        let values = field_map(&[("x", 1.into())]);
        store.adopt(vec!["x".into()], &values).unwrap();
        assert!(store.check_orphaned_state());

        store.release().unwrap();
        assert!(!store.check_orphaned_state());
    }

    #[test]
    fn recover_orphaned_releases_orphaned_state() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({"k": "v"});
        let adapter = TestAdapter::new(tmp.path(), initial);

        let store = TakeoverStore::new();
        let values = field_map(&[("k", serde_json::json!("v"))]);
        store.adopt(vec!["k".into()], &values).unwrap();

        // Simulate crash: store is Adopted, never released.
        assert!(store.check_orphaned_state());

        let manifest = store.recover_orphaned(&adapter).unwrap();
        assert_eq!(manifest.state, OwnershipState::Released);
        assert!(!store.check_orphaned_state());
    }

    #[test]
    fn recover_orphaned_errors_when_not_orphaned() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = TestAdapter::new(tmp.path(), serde_json::json!({}));

        let store = TakeoverStore::new();
        let err = store.recover_orphaned(&adapter).unwrap_err();
        assert!(err.contains("orphaned"), "error: {err}");
    }

    // -----------------------------------------------------------------------
    // I4: full scenario — adopt, user modifies, detect, resolve, release
    // -----------------------------------------------------------------------

    #[test]
    fn full_scenario_detect_resolve_release() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({
            "model": "gpt-4",
            "temperature": 0.7,
            "api_key": "sk-123"
        });
        let adapter = TestAdapter::new(tmp.path(), initial);

        let store = TakeoverStore::new();

        // 1. Adopt managed fields.
        let values = field_map(&[
            ("model", serde_json::json!("gpt-4")),
            ("temperature", serde_json::json!(0.7)),
        ]);
        store
            .adopt(vec!["model".into(), "temperature".into()], &values)
            .unwrap();

        // 2. Zroutery patches config.
        let patched = serde_json::json!({
            "model": "claude-3-opus",
            "temperature": 0.9,
            "api_key": "sk-123"
        });
        adapter
            .write_config(&AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: adapter.path.clone(),
                raw: patched,
                config_hash: String::new(),
            })
            .unwrap();

        // 3. User externally changes "temperature".
        let user_modified = serde_json::json!({
            "model": "claude-3-opus",
            "temperature": 0.3,
            "api_key": "sk-123"
        });
        adapter
            .write_config(&AgentConfigSnapshot {
                agent_type: AgentType::Claude,
                config_path: adapter.path.clone(),
                raw: user_modified,
                config_hash: String::new(),
            })
            .unwrap();

        // 4. Detect external modifications.
        // Both fields differ from adopt-time last_applied (gpt-4/0.7).
        let current_snapshot = adapter.read_config().unwrap();
        let current_map = field_map(&[
            ("model", current_snapshot.raw["model"].clone()),
            ("temperature", current_snapshot.raw["temperature"].clone()),
        ]);
        let mods = store.detect_external_modification(&current_map);
        assert_eq!(mods.len(), 2);

        // temperature: user changed from 0.7 to 0.3.
        let temp_mod = mods.iter().find(|m| m.field_path == "temperature").unwrap();
        assert_eq!(temp_mod.last_applied, Some(serde_json::json!(0.7)));
        assert_eq!(temp_mod.current_value, serde_json::json!(0.3));

        // model: Zroutery changed from gpt-4 to claude-3-opus.
        let model_mod = mods.iter().find(|m| m.field_path == "model").unwrap();
        assert_eq!(model_mod.last_applied, Some(serde_json::json!("gpt-4")));
        assert_eq!(model_mod.current_value, serde_json::json!("claude-3-opus"));

        // 5. Resolve conflicts: keep user's external values.
        let conflicts: Vec<FieldConflict> = mods
            .iter()
            .map(|m| FieldConflict {
                field_path: m.field_path.clone(),
                original_value: None,
                last_applied: m.last_applied.clone(),
                current_external: m.current_value.clone(),
            })
            .collect();

        let resolved = resolve_conflicts(&conflicts, ConflictResolution::KeepExternal);
        assert_eq!(resolved.len(), 2);
        // Both fields resolved with external values.
        let resolved_temp = resolved.iter().find(|(k, _)| k == "temperature").unwrap();
        assert_eq!(resolved_temp.1, Some(serde_json::json!(0.3)));
        let resolved_model = resolved.iter().find(|(k, _)| k == "model").unwrap();
        assert_eq!(resolved_model.1, Some(serde_json::json!("claude-3-opus")));

        // 6. Release with restore — restores original values from snapshot.
        let manifest = store.release_with_restore(&adapter).unwrap();
        assert_eq!(manifest.state, OwnershipState::Released);

        // 7. Verify: managed fields restored to adopt-time values.
        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["model"], serde_json::json!("gpt-4"));
        assert_eq!(disk["temperature"], serde_json::json!(0.7));
        assert_eq!(disk["api_key"], serde_json::json!("sk-123"));
    }

    #[test]
    fn adopt_after_release_with_restore_works() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({"x": 1, "y": 2});
        let adapter = TestAdapter::new(tmp.path(), initial);

        let store = TakeoverStore::new();

        // First cycle.
        let v1 = field_map(&[("x", serde_json::json!(1)), ("y", serde_json::json!(2))]);
        store.adopt(vec!["x".into(), "y".into()], &v1).unwrap();
        store.release_with_restore(&adapter).unwrap();

        assert_eq!(store.state(), OwnershipState::Released);

        // Re-adopt after release.
        let v2 = field_map(&[("x", serde_json::json!(1)), ("y", serde_json::json!(2))]);
        let re_adopted = store.adopt(vec!["x".into(), "y".into()], &v2).unwrap();
        assert_eq!(re_adopted.state, OwnershipState::Adopted);
        assert_eq!(re_adopted.generation, 1);
    }

    // -----------------------------------------------------------------------
    // I3: Real agent adapter tests
    // -----------------------------------------------------------------------

    #[test]
    fn read_config_hash_empty_when_file_missing() {
        // When the config file doesn't exist, hash should be empty.
        super::isolate_agent_home();
        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();
        // Config file likely doesn't exist in CI.
        if !snapshot.config_path.exists() {
            assert!(snapshot.config_hash.is_empty());
        }
    }

    #[test]
    fn read_config_hash_populated_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{"key": "value"}"#).unwrap();

        let adapter = TestAdapter::new(tmp.path(), serde_json::json!({"key": "value"}));
        let snapshot = adapter.read_config().unwrap();

        assert!(!snapshot.config_hash.is_empty());
        // Hash should be deterministic.
        let snapshot2 = adapter.read_config().unwrap();
        assert_eq!(snapshot.config_hash, snapshot2.config_hash);
    }

    #[test]
    fn apply_patch_atomic_write() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({"existing": "value"});
        let adapter = TestAdapter::new(tmp.path(), initial);

        let snapshot = adapter.read_config().unwrap();
        let fields = vec![ManagedField {
            path: "new_key".into(),
            value: serde_json::json!("new_value"),
        }];

        // TestAdapter's apply_patch doesn't write to disk (in-memory only),
        // so test the real adapters with a file that exists.
        // Use ClaudeAdapter-style logic directly on the temp path.
        let mut raw = snapshot.raw.clone();
        for field in &fields {
            set_nested(&mut raw, &field.path, field.value.clone()).unwrap();
        }
        let json = serde_json::to_string_pretty(&raw).unwrap();
        let hash = compute_hash(json.as_bytes());

        // Atomic write: temp + rename
        let config_path = &adapter.path;
        let tmp_file = config_path.with_extension("json.tmp");
        std::fs::write(&tmp_file, &json).unwrap();
        std::fs::rename(&tmp_file, config_path).unwrap();

        // Verify file content.
        let disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(disk["existing"], serde_json::json!("value"));
        assert_eq!(disk["new_key"], serde_json::json!("new_value"));

        // Verify temp file was renamed (no longer exists).
        assert!(!tmp_file.exists());

        // Verify hash is non-empty and deterministic.
        assert!(!hash.is_empty());
        assert_eq!(hash, compute_hash(json.as_bytes()));
    }

    #[test]
    fn apply_patch_config_hash_changes_after_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({"a": 1});
        let adapter = TestAdapter::new(tmp.path(), initial);

        let snapshot = adapter.read_config().unwrap();
        let original_hash = snapshot.config_hash.clone();

        let fields = vec![ManagedField {
            path: "a".into(),
            value: serde_json::json!(2),
        }];

        let patched = adapter.apply_patch(&snapshot, &fields).unwrap();
        assert_ne!(patched.config_hash, original_hash);
        assert!(!patched.config_hash.is_empty());
    }

    #[test]
    fn release_restores_original_values_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({"model": "gpt-4", "temp": 0.5});
        let adapter = TestAdapter::new(tmp.path(), initial.clone());

        let snapshot = adapter.read_config().unwrap();

        // Apply a patch to change "model".
        let fields = vec![ManagedField {
            path: "model".into(),
            value: serde_json::json!("claude-3-opus"),
        }];
        let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

        // Verify patch was applied.
        assert_eq!(patched.raw["model"], serde_json::json!("claude-3-opus"));

        // Create manifest with original values.
        let manifest = OwnershipManifest {
            state: OwnershipState::Adopted,
            managed_fields: vec!["model".into()],
            field_snapshots: [("model".into(), serde_json::json!("gpt-4"))]
                .into_iter()
                .collect(),
            absent_fields: Vec::new(),
            adopted_at: Some(1_700_000_000),
            released_at: None,
            generation: 0,
        };

        // Release should restore original "model" value.
        adapter.release(&patched, &manifest).unwrap();

        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["model"], serde_json::json!("gpt-4"));
        // "temp" should be unchanged.
        assert_eq!(disk["temp"], serde_json::json!(0.5));
    }

    #[test]
    fn different_adapters_have_different_config_paths() {
        super::isolate_agent_home();
        let claude = ClaudeAdapter;
        let codex = CodexAdapter;
        let gemini = GeminiAdapter;

        let claude_path = claude.config_path().unwrap();
        let codex_path = codex.config_path().unwrap();
        let gemini_path = gemini.config_path().unwrap();

        assert!(claude_path.ends_with(".claude.json"));
        assert!(codex_path.ends_with(".codex/config.toml"));
        assert!(gemini_path.ends_with(".gemini/settings.json"));

        // All three should be distinct.
        assert_ne!(claude_path, codex_path);
        assert_ne!(claude_path, gemini_path);
        assert_ne!(codex_path, gemini_path);
    }

    #[cfg(unix)]
    #[test]
    fn apply_patch_preserves_restrictive_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        super::isolate_agent_home();
        let home = super::isolated_agent_home().expect("isolated agent home");
        let path = home.join(".claude.json");
        std::fs::write(&path, r#"{"model":"gpt-4"}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();
        adapter
            .apply_patch(
                &snapshot,
                &[ManagedField {
                    path: "model".into(),
                    value: serde_json::json!("claude-3-opus"),
                }],
            )
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "existing mode was widened to {mode:o}");

        let disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk["model"], serde_json::json!("claude-3-opus"));
    }

    #[cfg(unix)]
    #[test]
    fn new_config_is_created_with_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;

        super::isolate_agent_home();
        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert!(!snapshot.config_path.exists());

        adapter
            .apply_patch(
                &snapshot,
                &[ManagedField {
                    path: "model".into(),
                    value: serde_json::json!("claude-3-opus"),
                }],
            )
            .unwrap();

        let mode = std::fs::metadata(&snapshot.config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "new config mode was {mode:o}");
    }

    #[test]
    fn apply_patch_refuses_stale_snapshot() {
        super::isolate_agent_home();
        let home = super::isolated_agent_home().expect("isolated agent home");
        let path = home.join(".claude.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "managed": "original",
                "unmanaged": "original"
            }))
            .unwrap(),
        )
        .unwrap();

        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();

        // Another tool edits a field Zroutery does not manage while the
        // snapshot is held.
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "managed": "original",
                "unmanaged": "user-new"
            }))
            .unwrap(),
        )
        .unwrap();

        let err = adapter
            .apply_patch(
                &snapshot,
                &[ManagedField {
                    path: "managed".into(),
                    value: serde_json::json!("zroutery"),
                }],
            )
            .unwrap_err();
        assert!(err.contains("changed on disk"), "error: {err}");

        // The write was refused, so the external edit is still there.
        let disk = read_json_file(&path);
        assert_eq!(disk["unmanaged"], serde_json::json!("user-new"));
        assert_eq!(disk["managed"], serde_json::json!("original"));

        // Re-reading and retrying applies the patch on top of the current file.
        let fresh = adapter.read_config().unwrap();
        adapter
            .apply_patch(
                &fresh,
                &[ManagedField {
                    path: "managed".into(),
                    value: serde_json::json!("zroutery"),
                }],
            )
            .unwrap();

        let disk = read_json_file(&path);
        assert_eq!(disk["managed"], serde_json::json!("zroutery"));
        assert_eq!(disk["unmanaged"], serde_json::json!("user-new"));
    }

    #[test]
    fn apply_patch_reports_type_conflict_on_scalar_parent() {
        super::isolate_agent_home();
        let home = super::isolated_agent_home().expect("isolated agent home");
        let path = home.join(".claude.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({"model": "existing-scalar"})).unwrap(),
        )
        .unwrap();

        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();

        // `model.temperature` cannot be set when `model` is a scalar; the old
        // code reported success and changed nothing.
        let err = adapter
            .apply_patch(
                &snapshot,
                &[ManagedField {
                    path: "model.temperature".into(),
                    value: serde_json::json!(0.5),
                }],
            )
            .unwrap_err();
        assert!(err.contains("model.temperature"), "error: {err}");
        assert!(err.contains("non-table"), "error: {err}");

        let disk = read_json_file(&path);
        assert_eq!(disk["model"], serde_json::json!("existing-scalar"));
    }

    #[test]
    fn apply_patch_refuses_to_replace_existing_non_table() {
        super::isolate_agent_home();
        let home = super::isolated_agent_home().expect("isolated agent home");
        let path = home.join(".claude.json");
        let initial = serde_json::json!({"a": {"b": 5}});
        std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();

        // The old code replaced the existing scalar `a.b` with a table.
        let err = adapter
            .apply_patch(
                &snapshot,
                &[ManagedField {
                    path: "a.b.c.d".into(),
                    value: serde_json::json!(1),
                }],
            )
            .unwrap_err();
        assert!(err.contains("a.b.c.d"), "error: {err}");

        let disk = read_json_file(&path);
        assert_eq!(disk, initial, "the existing value must not be replaced");
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

    #[test]
    fn apply_patch_preserves_unmanaged_fields_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({
            "managed": "original",
            "unmanaged": "keep_me"
        });
        let adapter = TestAdapter::new(tmp.path(), initial);

        let snapshot = adapter.read_config().unwrap();
        let fields = vec![ManagedField {
            path: "managed".into(),
            value: serde_json::json!("changed"),
        }];

        let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

        // Write to disk (TestAdapter's apply_patch is in-memory only).
        adapter.write_config(&patched).unwrap();

        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["managed"], serde_json::json!("changed"));
        assert_eq!(disk["unmanaged"], serde_json::json!("keep_me"));
    }

    #[test]
    fn release_with_hash_tracking() {
        let tmp = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({"x": 1});
        let adapter = TestAdapter::new(tmp.path(), initial);

        let store = TakeoverStore::new();
        let values = field_map(&[("x", serde_json::json!(1))]);
        store.adopt(vec!["x".into()], &values).unwrap();

        // Patch config.
        let snapshot = adapter.read_config().unwrap();
        let original_hash = snapshot.config_hash.clone();

        let fields = vec![ManagedField {
            path: "x".into(),
            value: serde_json::json!(99),
        }];
        let patched = adapter.apply_patch(&snapshot, &fields).unwrap();
        assert_ne!(patched.config_hash, original_hash);

        // Release restores original.
        let manifest = store.release().unwrap();
        adapter.release(&patched, &manifest).unwrap();

        let disk = read_json_file(&adapter.path);
        assert_eq!(disk["x"], serde_json::json!(1));
    }
}
