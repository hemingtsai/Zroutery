//! Reading and writing a client config file: atomic replacement, permission preservation, and the nested-path walk that makes a dotted manifest path real.

use super::clients::{AgentConfigSnapshot, AgentType, ManagedField};
use super::ownership::OwnershipManifest;

// ---------------------------------------------------------------------------

// Helpers
// ---------------------------------------------------------------------------

// Per-test override for the agent config root.
//
// The adapters below resolve their config path against the user home, and
// the tests used to exercise apply_patch/release against the real
// ~/.claude/settings.json, ~/.codex/config.toml and ~/.gemini/settings.json.
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
pub(crate) fn isolate_agent_home() {
    TEST_CONFIG_DIRS.with(|slot| slot.borrow_mut().clear());
    TEST_AGENT_HOME.with(|slot| {
        *slot.borrow_mut() = Some(tempfile::TempDir::new().expect("temp agent home"));
    });
}

/// Point a client's config directory override at `dir` for this test thread.
#[cfg(test)]
pub(crate) fn isolate_config_dir(env_key: &str, dir: &std::path::Path) {
    TEST_CONFIG_DIRS.with(|slot| {
        let mut dirs = slot.borrow_mut();
        dirs.retain(|(key, _)| key != env_key);
        dirs.push((env_key.to_string(), dir.to_path_buf()));
    });
}

/// The agent config root for this test thread, if one was installed.
#[cfg(test)]
pub(crate) fn isolated_agent_home() -> Option<std::path::PathBuf> {
    TEST_AGENT_HOME.with(|slot| slot.borrow().as_ref().map(|dir| dir.path().to_path_buf()))
}

/// Resolve the current user's home directory.
///
/// Checks `HOME` (Unix) then `USERPROFILE` (Windows).
pub(super) fn home_dir() -> Result<std::path::PathBuf, String> {
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
pub(super) fn config_dir_override(env_key: &str) -> Option<std::path::PathBuf> {
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
pub(crate) fn write_config_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
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
pub(super) fn write_restricted(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
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
pub(super) fn write_restricted(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// A unique temporary file name next to `path`, in the same directory so the
/// final rename cannot cross a filesystem boundary.
pub(super) fn temp_sibling(path: &std::path::Path) -> std::path::PathBuf {
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
pub(super) fn compute_hash(data: &[u8]) -> String {
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
pub(super) fn set_nested(
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
pub(super) fn type_conflict(path: &str, part: &str) -> String {
    format!("cannot set `{path}`: `{part}` already holds a non-table value of a different type")
}

/// Restore a manifest's managed fields into `raw`.
///
/// Fields that already existed at adoption time are set back to their captured
/// value; fields recorded in [`OwnershipManifest::absent_fields`] are removed
/// so a value Zroutery introduced does not outlive the ownership.
pub(super) fn restore_fields(
    raw: &mut serde_json::Value,
    manifest: &OwnershipManifest,
) -> Result<(), String> {
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
pub(super) fn apply_fields(
    raw: &mut serde_json::Value,
    fields: &[ManagedField],
) -> Result<(), String> {
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
pub(super) fn reread_raw(
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
pub(super) fn apply_patch_to_disk(
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
pub(super) fn release_to_disk(
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
pub(super) fn remove_nested(root: &mut serde_json::Value, path: &str) {
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
pub(super) enum ConfigFormat {
    /// JSON, used by Claude and OpenCode.
    Json,
    /// TOML, used by Codex (`config.toml`).
    Toml,
}

/// Parse config text in `format` into JSON.
pub(super) fn parse_config(format: ConfigFormat, text: &str) -> Result<serde_json::Value, String> {
    match format {
        ConfigFormat::Json => serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}")),
        ConfigFormat::Toml => crate::agent_takeover::toml::parse_toml_subset(text),
    }
}

/// Serialize `raw` as config text in `format`.
pub(super) fn serialize_config(
    format: ConfigFormat,
    raw: &serde_json::Value,
) -> Result<String, String> {
    match format {
        ConfigFormat::Json => {
            serde_json::to_string_pretty(raw).map_err(|e| format!("serialize failed: {e}"))
        }
        ConfigFormat::Toml => crate::agent_takeover::toml::serialize_toml_subset(raw),
    }
}

/// Read and parse a client config, hashing the raw bytes for change detection.
pub(super) fn read_snapshot(
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
