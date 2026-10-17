//! Locating the CC Switch provider data and parsing it.

use std::path::PathBuf;

use serde_json::Value;

use super::model::{collect_models, CcProvider, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS};

/// Where CC Switch keeps its provider data, in the order we look.
///
/// The primary location is `~/.cc-switch` on every platform — that is where
/// CC Switch itself reads and writes, so it is the one worth following first.
/// The platform application directories are a fallback for the day its data
/// moves: they cost one extra `is_dir` check when the primary is missing.
fn candidate_dirs() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let mut dirs = Vec::new();
    if let Some(home) = &home {
        dirs.push(PathBuf::from(home).join(".cc-switch"));
    }
    // Windows: %APPDATA%\com.ccswitch.desktop — what Tauri's app_config_dir
    // resolves to for CC Switch's identifier.
    if let Ok(appdata) = std::env::var("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("com.ccswitch.desktop"));
    }
    // macOS: ~/Library/Application Support/com.ccswitch.desktop.
    if let Some(home) = &home {
        dirs.push(
            PathBuf::from(home)
                .join("Library/Application Support")
                .join("com.ccswitch.desktop"),
        );
    }
    // Linux and anything else following the XDG convention.
    if let Some(home) = &home {
        dirs.push(
            PathBuf::from(home)
                .join(".config")
                .join("com.ccswitch.desktop"),
        );
    }
    dirs
}

/// The directory holding CC Switch's provider data.
///
/// `ZROUTERY_CC_SWITCH_DIR` wins when it is set, so an import can be pointed at
/// a specific installation (tests and troubleshooting); otherwise the first
/// candidate that actually holds provider data, and failing that the primary
/// location.
///
/// Public so the dashboard can show where the providers came from.
pub fn cc_switch_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ZROUTERY_CC_SWITCH_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let dirs = candidate_dirs();
    dirs.iter()
        .find(|d| d.join("cc-switch.db").exists() || d.join("config.json").exists())
        .cloned()
        .or_else(|| dirs.first().cloned())
}

/// Read every Claude Code provider CC Switch knows about.
///
/// An unreadable or missing installation is not an error: the answer is an
/// empty list plus a reason, which the dashboard can show instead of a stack
/// trace. Only `app_type = 'claude'` rows are considered — CC Switch also
/// manages Codex and Gemini, whose configs are shaped for other clients.
pub fn read_providers() -> Result<Vec<CcProvider>, String> {
    let Some(dir) = cc_switch_dir() else {
        return Err("no CC Switch installation found".into());
    };
    let db = dir.join("cc-switch.db");
    if db.exists() {
        return read_from_db(&db);
    }
    let legacy = dir.join("config.json");
    if legacy.exists() {
        return read_from_legacy_json(&legacy);
    }
    Err(format!(
        "nothing to read in {} (no cc-switch.db, no config.json)",
        dir.display()
    ))
}

fn read_from_db(db: &PathBuf) -> Result<Vec<CcProvider>, String> {
    use rusqlite::{Connection, OpenFlags};

    // Read only, and never take a lock CC Switch might notice.
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match Connection::open_with_flags(db, flags) {
        Ok(conn) => conn,
        Err(first) => {
            // A WAL-mode database whose shared-memory file needs rebuilding
            // cannot be opened read-only — typically after CC Switch was
            // killed rather than closed. Reading it as an immutable snapshot
            // sidesteps the WAL entirely, which is exactly right for an
            // import: a consistent point-in-time copy is all we want.
            let uri = format!("file:{}?immutable=1", db.display());
            match Connection::open_with_flags(&uri, flags | OpenFlags::SQLITE_OPEN_URI) {
                Ok(conn) => {
                    tracing::debug!("cc-switch database opened as an immutable snapshot");
                    conn
                }
                Err(second) => {
                    return Err(format!(
                        "cannot open {} ({}; snapshot read also failed: {})",
                        db.display(),
                        first,
                        second
                    ))
                }
            }
        }
    };

    let mut stmt = conn
        .prepare(
            "SELECT id, name, settings_config, is_current
             FROM providers
             WHERE app_type = 'claude'
             ORDER BY is_current DESC, sort_index",
        )
        .map_err(|e| format!("the providers table is missing or unreadable: {e}"))?;

    let rows: Vec<(String, String, String, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .filter_map(|(id, name, settings_config, is_current)| {
            let config: Value = serde_json::from_str(&settings_config).ok()?;
            let env = config.get("env")?.clone();
            provider_from_env(id, name, &env, is_current != 0)
        })
        .collect())
}

/// The pre-database format: `{"providers": {"claude": [ ... ]}}`, each entry
/// carrying the same `settings_config` shape as the database rows.
pub(super) fn read_from_legacy_json(path: &PathBuf) -> Result<Vec<CcProvider>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read it: {e}"))?;
    let root: Value = serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
    let Some(entries) = root
        .get("providers")
        .and_then(|p| p.get("claude"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("legacy-{i}"));
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let is_current = entry
                .get("is_current")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let env = entry.get("settings_config")?.get("env")?.clone();
            provider_from_env(id, name, &env, is_current)
        })
        .collect())
}

/// Build a provider draft from a Claude Code env block.
///
/// Entries without a base URL (CC Switch ships an "official" row that is
/// empty) are dropped rather than imported half-formed.
pub(super) fn provider_from_env(
    source_id: String,
    name: String,
    env: &Value,
    is_current: bool,
) -> Option<CcProvider> {
    let get = |key: &str| -> Option<String> {
        env.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let base_url = get("ANTHROPIC_BASE_URL")?;
    let api_key = get("ANTHROPIC_AUTH_TOKEN").or_else(|| get("ANTHROPIC_API_KEY"));

    let models = collect_models(env);
    // Claude Code reads this variable as a string, so a real configuration can
    // hold anything from "3000000" to an empty value.
    let timeout_ms = parse_timeout_ms(get("API_TIMEOUT_MS").as_deref());
    Some(CcProvider {
        source_id,
        name,
        base_url,
        api_key,
        models,
        is_current,
        timeout_ms,
    })
}

/// Read CC Switch's `API_TIMEOUT_MS`, which reaches the client as a string.
///
/// A value that is not a positive integer is unusable, and so is one outside
/// the range the gateway accepts: those are treated as "no timeout was
/// configured" rather than being clamped, so the import falls back to the same
/// default it uses when the variable is absent.
fn parse_timeout_ms(raw: Option<&str>) -> Option<u64> {
    let ms: u64 = raw?.trim().parse().ok()?;
    (MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS)
        .contains(&ms)
        .then_some(ms)
}
