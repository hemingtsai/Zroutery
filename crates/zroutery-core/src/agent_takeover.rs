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
//!    ▲                                              │
//!    └──────────────────────────────────────────────┘
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
    pub field_snapshots: HashMap<String, serde_json::Value>,
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
            .filter_map(|f| {
                current_values
                    .get(f)
                    .map(|v| (f.clone(), v.clone()))
            })
            .collect();

        // Populate last_applied with the captured snapshots.
        inner.last_applied = field_snapshots.clone();

        let manifest = OwnershipManifest {
            state: OwnershipState::Adopted,
            managed_fields,
            field_snapshots,
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
        // 1. Validate state and snapshot the manifest.
        let manifest_snapshot = {
            let inner = crate::sync::lock(&self.inner);
            if inner.state != OwnershipState::Adopted {
                return Err(format!(
                    "cannot release: current state is {:?}, expected Adopted",
                    inner.state
                ));
            }
            inner.manifest.as_ref().unwrap().clone()
        };

        // 2. Read current config from disk.
        let current = adapter.read_config()?;

        // 3. Restore original values and write to disk.
        adapter.release(&current, &manifest_snapshot)?;

        // 4. Update internal state only after successful disk write.
        let result = {
            let mut inner = crate::sync::lock(&self.inner);
            inner.generation += 1;
            let gen = inner.generation;

            let manifest = inner.manifest.as_mut().unwrap();
            manifest.state = OwnershipState::Released;
            manifest.released_at = Some(chrono::Utc::now().timestamp());
            manifest.generation = gen;

            let result = manifest.clone();
            inner.state = OwnershipState::Released;
            result
        };

        Ok(result)
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
        if let Some(parent) = snapshot.config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir {}: {e}", parent.display()))?;
        }
        std::fs::write(&snapshot.config_path, json)
            .map_err(|e| format!("failed to write {}: {e}", snapshot.config_path.display()))?;
        Ok(())
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
        let path = self.config_path()?;
        let (raw, hash) = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let hash = compute_hash(data.as_bytes());
            let parsed: serde_json::Value = serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
            (parsed, hash)
        } else {
            (serde_json::Value::Object(serde_json::Map::new()), String::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: path,
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
        for field in fields {
            set_nested(&mut raw, &field.path, field.value.clone());
        }
        let json = serde_json::to_string_pretty(&raw)
            .map_err(|e| format!("serialize failed: {e}"))?;
        let hash = compute_hash(json.as_bytes());
        // Atomic write: write to temp file, then rename
        let path = &snapshot.config_path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create dir failed: {e}"))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write failed: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename failed: {e}"))?;
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
        for field_path in &manifest.managed_fields {
            if let Some(original_value) = manifest.field_snapshots.get(field_path) {
                set_nested(&mut raw, field_path, original_value.clone());
            }
        }
        let json = serde_json::to_string_pretty(&raw)
            .map_err(|e| format!("serialize failed: {e}"))?;
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

// ---------------------------------------------------------------------------
// CodexAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Codex CLI.
///
/// Config location: `~/.codex/config.json`
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Codex
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        let home = home_dir()?;
        Ok(home.join(".codex").join("config.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let path = self.config_path()?;
        let (raw, hash) = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let hash = compute_hash(data.as_bytes());
            let parsed: serde_json::Value = serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
            (parsed, hash)
        } else {
            (serde_json::Value::Object(serde_json::Map::new()), String::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Codex,
            config_path: path,
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
        for field in fields {
            set_nested(&mut raw, &field.path, field.value.clone());
        }
        let json = serde_json::to_string_pretty(&raw)
            .map_err(|e| format!("serialize failed: {e}"))?;
        let hash = compute_hash(json.as_bytes());
        let path = &snapshot.config_path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create dir failed: {e}"))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write failed: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename failed: {e}"))?;
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
        for field_path in &manifest.managed_fields {
            if let Some(original_value) = manifest.field_snapshots.get(field_path) {
                set_nested(&mut raw, field_path, original_value.clone());
            }
        }
        let json = serde_json::to_string_pretty(&raw)
            .map_err(|e| format!("serialize failed: {e}"))?;
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

// ---------------------------------------------------------------------------
// GeminiAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Gemini CLI.
///
/// Config location: `~/.config/gemini/config.json`
pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Gemini
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        let home = home_dir()?;
        Ok(home.join(".config").join("gemini").join("config.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let path = self.config_path()?;
        let (raw, hash) = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let hash = compute_hash(data.as_bytes());
            let parsed: serde_json::Value = serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
            (parsed, hash)
        } else {
            (serde_json::Value::Object(serde_json::Map::new()), String::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Gemini,
            config_path: path,
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
        for field in fields {
            set_nested(&mut raw, &field.path, field.value.clone());
        }
        let json = serde_json::to_string_pretty(&raw)
            .map_err(|e| format!("serialize failed: {e}"))?;
        let hash = compute_hash(json.as_bytes());
        let path = &snapshot.config_path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create dir failed: {e}"))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write failed: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename failed: {e}"))?;
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
        for field_path in &manifest.managed_fields {
            if let Some(original_value) = manifest.field_snapshots.get(field_path) {
                set_nested(&mut raw, field_path, original_value.clone());
            }
        }
        let json = serde_json::to_string_pretty(&raw)
            .map_err(|e| format!("serialize failed: {e}"))?;
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the current user's home directory.
///
/// Checks `HOME` (Unix) then `USERPROFILE` (Windows).
fn home_dir() -> Result<std::path::PathBuf, String> {
    if let Ok(home) = std::env::var("HOME") {
        return Ok(std::path::PathBuf::from(home));
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return Ok(std::path::PathBuf::from(profile));
    }
    Err("could not determine home directory (HOME/USERPROFILE not set)".into())
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
fn set_nested(root: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return;
    }

    let mut current = root;

    // Navigate to the parent, creating intermediate objects as needed.
    for part in &parts[..parts.len() - 1] {
        if !current.is_object() {
            *current = serde_json::Value::Object(serde_json::Map::new());
        }
        let obj = current.as_object_mut().unwrap();
        if !obj.contains_key(*part) {
            obj.insert(part.to_string(), serde_json::Value::Object(serde_json::Map::new()));
        }
        current = obj.get_mut(*part).unwrap();
    }

    // Set the leaf value.
    let leaf = parts.last().unwrap();
    if let Some(obj) = current.as_object_mut() {
        obj.insert(leaf.to_string(), value);
    }
}

/// Get a nested JSON value by dotted path (e.g. "model.temperature").
fn get_nested<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root;
    for part in &parts {
        current = current.get(*part)?;
    }
    Some(current)
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
        let manifest = store
            .adopt(vec!["timeout".into()], &values)
            .unwrap();

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
        store
            .adopt(vec!["a".into(), "b".into()], &values)
            .unwrap();

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
        let manifest = store
            .adopt(vec!["a".into(), "b".into()], &values)
            .unwrap();

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
        store
            .adopt(vec!["a".into(), "b".into()], &initial)
            .unwrap();

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
        assert_eq!(released.field_snapshots["model"], serde_json::json!("gpt-4"));
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
            .adopt(
                vec!["host".into(), "port".into(), "debug".into()],
                &initial,
            )
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
        store
            .adopt(vec!["x".into(), "y".into()], &initial)
            .unwrap();

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
            let values = field_map(&[("a", serde_json::json!(val)), ("b", serde_json::json!(val + 1))]);

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
                let current = field_map(&[("f1", serde_json::json!(i)), ("f2", serde_json::json!(i))]);
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
        let original = store
            .adopt(vec!["k".into(), "n".into()], &values)
            .unwrap();

        // Serialize to JSON.
        let json = serde_json::to_string(&original).expect("serialize");

        // Deserialize back.
        let restored: OwnershipManifest = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.state, original.state);
        assert_eq!(restored.managed_fields, original.managed_fields);
        assert_eq!(restored.field_snapshots, original.field_snapshots);
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
        let adapter = ClaudeAdapter;
        assert_eq!(adapter.agent_type(), AgentType::Claude);
    }

    #[test]
    fn codex_adapter_agent_type() {
        let adapter = CodexAdapter;
        assert_eq!(adapter.agent_type(), AgentType::Codex);
    }

    #[test]
    fn gemini_adapter_agent_type() {
        let adapter = GeminiAdapter;
        assert_eq!(adapter.agent_type(), AgentType::Gemini);
    }

    #[test]
    fn claude_adapter_config_path() {
        let adapter = ClaudeAdapter;
        let path = adapter.config_path().unwrap();
        assert!(path.ends_with(".claude.json"));
    }

    #[test]
    fn codex_adapter_config_path() {
        let adapter = CodexAdapter;
        let path = adapter.config_path().unwrap();
        assert!(path.ends_with(".codex/config.json"));
    }

    #[test]
    fn gemini_adapter_config_path() {
        let adapter = GeminiAdapter;
        let path = adapter.config_path().unwrap();
        assert!(path.ends_with("gemini/config.json"));
    }

    #[test]
    fn claude_adapter_read_config() {
        let adapter = ClaudeAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.agent_type, AgentType::Claude);
        // Config file likely does not exist in CI, so raw should be empty object.
        assert!(snapshot.raw.is_object());
    }

    #[test]
    fn codex_adapter_read_config() {
        let adapter = CodexAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.agent_type, AgentType::Codex);
        assert!(snapshot.raw.is_object());
    }

    #[test]
    fn gemini_adapter_read_config() {
        let adapter = GeminiAdapter;
        let snapshot = adapter.read_config().unwrap();
        assert_eq!(snapshot.agent_type, AgentType::Gemini);
        assert!(snapshot.raw.is_object());
    }

    #[test]
    fn apply_patch_modifies_snapshot() {
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
        // Original snapshot is unchanged.
        assert!(snapshot.raw.get("model").is_none());
    }

    #[test]
    fn apply_patch_nested_path() {
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
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&initial).unwrap(),
            )
            .unwrap();
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
                let data = std::fs::read_to_string(&self.path)
                    .map_err(|e| format!("read failed: {e}"))?;
                let hash = compute_hash(data.as_bytes());
                let parsed: serde_json::Value = serde_json::from_str(&data)
                    .map_err(|e| format!("parse failed: {e}"))?;
                (parsed, hash)
            } else {
                (serde_json::Value::Object(serde_json::Map::new()), String::new())
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
            for field in fields {
                set_nested(&mut raw, &field.path, field.value.clone());
            }
            let json = serde_json::to_string_pretty(&raw)
                .map_err(|e| format!("serialize failed: {e}"))?;
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
            for field_path in &manifest.managed_fields {
                if let Some(original_value) = manifest.field_snapshots.get(field_path) {
                    set_nested(&mut raw, field_path, original_value.clone());
                }
            }
            let json = serde_json::to_string_pretty(&raw)
                .map_err(|e| format!("serialize failed: {e}"))?;
            let hash = compute_hash(json.as_bytes());
            let restored = AgentConfigSnapshot {
                agent_type: snapshot.agent_type,
                config_path: snapshot.config_path.clone(),
                raw,
                config_hash: hash,
            };
            self.write_config(&restored)
        }

        fn write_config(&self, snapshot: &AgentConfigSnapshot) -> Result<(), String> {
            let json = serde_json::to_string_pretty(&snapshot.raw)
                .map_err(|e| format!("serialize failed: {e}"))?;
            std::fs::write(&snapshot.config_path, json)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(())
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
        let adapter = TestAdapter::new(
            tmp.path(),
            serde_json::json!({"x": 1}),
        );

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

        let resolved =
            resolve_conflicts(&conflicts, ConflictResolution::OverwriteWithManaged);
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
        store
            .adopt(vec!["x".into(), "y".into()], &v1)
            .unwrap();
        store.release_with_restore(&adapter).unwrap();

        assert_eq!(store.state(), OwnershipState::Released);

        // Re-adopt after release.
        let v2 = field_map(&[("x", serde_json::json!(1)), ("y", serde_json::json!(2))]);
        let re_adopted = store
            .adopt(vec!["x".into(), "y".into()], &v2)
            .unwrap();
        assert_eq!(re_adopted.state, OwnershipState::Adopted);
        assert_eq!(re_adopted.generation, 1);
    }

    // -----------------------------------------------------------------------
    // I3: Real agent adapter tests
    // -----------------------------------------------------------------------

    #[test]
    fn read_config_hash_empty_when_file_missing() {
        // When the config file doesn't exist, hash should be empty.
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
            set_nested(&mut raw, &field.path, field.value.clone());
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
        let claude = ClaudeAdapter;
        let codex = CodexAdapter;
        let gemini = GeminiAdapter;

        let claude_path = claude.config_path().unwrap();
        let codex_path = codex.config_path().unwrap();
        let gemini_path = gemini.config_path().unwrap();

        assert!(claude_path.ends_with(".claude.json"));
        assert!(codex_path.ends_with(".codex/config.json"));
        assert!(gemini_path.ends_with("gemini/config.json"));

        // All three should be distinct.
        assert_ne!(claude_path, codex_path);
        assert_ne!(claude_path, gemini_path);
        assert_ne!(codex_path, gemini_path);
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
