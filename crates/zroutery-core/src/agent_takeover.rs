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
        let raw = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: path,
            raw,
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
        Ok(AgentConfigSnapshot {
            agent_type: snapshot.agent_type,
            config_path: snapshot.config_path.clone(),
            raw,
        })
    }

    fn release(
        &self,
        _snapshot: &AgentConfigSnapshot,
        _manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        Ok(())
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
        let raw = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Codex,
            config_path: path,
            raw,
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
        Ok(AgentConfigSnapshot {
            agent_type: snapshot.agent_type,
            config_path: snapshot.config_path.clone(),
            raw,
        })
    }

    fn release(
        &self,
        _snapshot: &AgentConfigSnapshot,
        _manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        Ok(())
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
        let raw = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Gemini,
            config_path: path,
            raw,
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
        Ok(AgentConfigSnapshot {
            agent_type: snapshot.agent_type,
            config_path: snapshot.config_path.clone(),
            raw,
        })
    }

    fn release(
        &self,
        _snapshot: &AgentConfigSnapshot,
        _manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        Ok(())
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
}
