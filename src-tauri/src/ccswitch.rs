//! Importing providers from CC Switch.
//!
//! CC Switch (https://github.com/farion1231/cc-switch) keeps its provider list
//! in `~/.cc-switch/cc-switch.db` (SQLite; newer versions) or
//! `~/.cc-switch/config.json` (older ones). Each Claude Code provider is a
//! name plus the exact environment block Claude Code runs with — most
//! importantly `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN` and the
//! `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL` tier defaults.
//!
//! Those map onto Zroutery almost one to one: a provider becomes a
//! `ProviderConfig` (Anthropic dialect — every Claude Code relay speaks it),
//! and each tier default becomes a `ModelEntry` with that class. A relay that
//! serves the same model for every tier collapses to one entry, classified as
//! sonnet: that is the tier a conversation actually asks for, and an import
//! that leaves `sonnet-class` empty would be a poor surprise.
//!
//! Nothing is imported in place: reading produces drafts, the dashboard shows
//! them, and only the selected ones are written — providers into the config,
//! API keys straight into the OS credential store, never the config file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zroutery_core::config::{ModelTier, ModelEntry, ProviderConfig, ProviderKind};
use zroutery_core::query::strip_client_model_modifier;

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

/// The directory CC Switch's data was found in, or its primary location.
///
/// Public so the dashboard can show where the providers came from.
pub fn cc_switch_dir() -> Option<PathBuf> {
    let dirs = candidate_dirs();
    dirs.iter()
        .find(|d| d.join("cc-switch.db").exists() || d.join("config.json").exists())
        .cloned()
        .or_else(|| dirs.first().cloned())
}

/// One provider as CC Switch knows it, reduced to what Zroutery needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcProvider {
    /// CC Switch's own provider id, kept so a re-import can be recognised.
    pub source_id: String,
    pub name: String,
    pub base_url: String,
    /// The auth token, present in the preview only so it can be stored on
    /// import; the dashboard must not render it.
    pub api_key: Option<String>,
    /// The tier defaults, `[1M]`/`[1m]` modifiers already stripped.
    pub models: Vec<CcModel>,
    /// The provider CC Switch currently has active.
    pub is_current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcModel {
    pub upstream_model: String,
    pub tier: Option<ModelTier>,
}

/// What the import preview shows for one CC Switch provider: the draft plus
/// what would happen to it. The API key is deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcProviderDraft {
    #[serde(flatten)]
    pub provider: CcProvider,
    /// The Zroutery provider id this would get.
    pub target_id: String,
    /// A provider with this id already exists, so the import is skipped.
    pub already_imported: bool,
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
fn read_from_legacy_json(path: &PathBuf) -> Result<Vec<CcProvider>, String> {
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
fn provider_from_env(
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
    Some(CcProvider {
        source_id,
        name,
        base_url,
        api_key,
        models,
        is_current,
    })
}

/// The tier defaults become class members; the general `ANTHROPIC_MODEL` and
/// the subagent model join as unclassified entries when they differ.
///
/// When every tier names the same model — the usual single-model relay — the
/// entry is classified sonnet: a conversation asks for `sonnet-class` far
/// more than anything else, and the import must leave that class usable.
fn collect_models(env: &Value) -> Vec<CcModel> {
    let model_of = |key: &str| -> Option<String> {
        env.get(key)
            .and_then(Value::as_str)
            .map(strip_client_model_modifier)
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "default")
            .map(str::to_string)
    };

    let opus = model_of("ANTHROPIC_DEFAULT_OPUS_MODEL");
    let sonnet = model_of("ANTHROPIC_DEFAULT_SONNET_MODEL");
    let haiku = model_of("ANTHROPIC_DEFAULT_HAIKU_MODEL");

    let all_same = match (&opus, &sonnet, &haiku) {
        (Some(o), Some(s), Some(h)) => o == s && s == h,
        (Some(o), Some(s), None) | (Some(o), None, Some(s)) | (None, Some(o), Some(s)) => o == s,
        _ => false,
    };

    let mut models: Vec<CcModel> = Vec::new();
    if all_same {
        // Every tier is the same model: one entry, in the tier a conversation
        // actually requests.
        models.push(CcModel {
            upstream_model: sonnet.clone().unwrap(),
            tier: Some(ModelTier::Standard),
        });
    } else {
        for (model, tier) in [(opus, ModelTier::Reasoning), (sonnet, ModelTier::Standard), (haiku, ModelTier::Fast)]
        {
            if let Some(upstream_model) = model {
                if !models
                    .iter()
                    .any(|m: &CcModel| m.upstream_model == upstream_model)
                {
                    models.push(CcModel {
                        upstream_model,
                        tier: Some(tier),
                    });
                }
            }
        }
    }

    // The models Claude Code can ask for by name beyond the tiers: the main
    // model override and the subagent model. They stay callable by exact id.
    for key in ["ANTHROPIC_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"] {
        if let Some(upstream_model) = model_of(key) {
            if !models
                .iter()
                .any(|m: &CcModel| m.upstream_model == upstream_model)
            {
                models.push(CcModel {
                    upstream_model,
                    tier: None,
                });
            }
        }
    }
    models
}

/// A Zroutery id that is unique among `taken` and still recognisable.
pub fn unique_provider_id(name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let base: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() { "provider".to_string() } else { base };

    let mut candidate = base.clone();
    let mut n = 2;
    while taken(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

/// Turn a draft into the pieces Zroutery stores: a provider plus its models,
/// with the API key kept apart for the credential store.
///
/// `provider_id` is decided by the caller, who knows the existing
/// configuration and can guarantee uniqueness. The models take `priority`
/// as their class priority — the current CC Switch provider lands at 0 so it
/// is the primary after import, the rest follow in CC Switch's own order.
pub fn to_zroutery(
    draft: &CcProvider,
    provider_id: String,
    priority: i32,
    timeout_ms: Option<u64>,
) -> (ProviderConfig, Vec<ModelEntry>) {
    let mut provider = ProviderConfig::new(provider_id, draft.name.clone(), ProviderKind::Anthropic);
    provider.base_url = draft.base_url.trim_end_matches('/').to_string();
    // Claude Code relays expect the client fingerprint; that is Zroutery's
    // default for Anthropic providers, and an import that silently disabled
    // it would break strict gateways.
    provider.impersonate_claude_code = true;
    // CC Switch's API_TIMEOUT_MS is milliseconds against our seconds, and the
    // values users configure there (3000000) are "effectively forever".
    provider.timeout_secs = timeout_ms
        .map(|ms| (ms / 1000).clamp(60, 3600))
        .unwrap_or(600);

    let models = draft
        .models
        .iter()
        .map(|m| {
            let mut entry = ModelEntry::for_upstream(&provider.id, &m.upstream_model, m.tier);
            entry.priority = priority;
            entry
        })
        .collect();
    (provider, models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_block(map: Value) -> Value {
        map
    }

    #[test]
    fn tier_defaults_become_tier_members() {
        let env = env_block(json!({
            "ANTHROPIC_BASE_URL": "https://relay.example/v1",
            "ANTHROPIC_AUTH_TOKEN": "sk-x",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "big-model[1M]",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "mid-model",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "small-model",
            "ANTHROPIC_MODEL": "mid-model"
        }));
        let p = provider_from_env("id".into(), "Relay".into(), &env, false).unwrap();
        assert_eq!(p.base_url, "https://relay.example/v1");
        assert_eq!(p.api_key.as_deref(), Some("sk-x"));
        // [1M] is a window modifier, not part of the model name.
        let find = |name: &str| {
            p.models
                .iter()
                .find(|m| m.upstream_model == name)
                .and_then(|m| m.tier)
        };
        assert_eq!(find("big-model"), Some(ModelTier::Reasoning));
        assert_eq!(find("mid-model"), Some(ModelTier::Standard));
        assert_eq!(find("small-model"), Some(ModelTier::Fast));
        // ANTHROPIC_MODEL matched an existing entry, so no duplicate.
        assert_eq!(p.models.len(), 3);
    }

    #[test]
    fn a_single_model_relay_collapses_to_sonnet() {
        let env = env_block(json!({
            "ANTHROPIC_BASE_URL": "https://relay.example/v1",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.3[1M]",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.3[1M]",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-5.3",
            "CLAUDE_CODE_SUBAGENT_MODEL": "glm-5.3[1m]"
        }));
        let p = provider_from_env("id".into(), "Relay".into(), &env, true).unwrap();
        assert_eq!(p.models.len(), 1);
        assert_eq!(p.models[0].upstream_model, "glm-5.3");
        // The class a conversation asks for, so sonnet-class works on day one.
        assert_eq!(p.models[0].tier, Some(ModelTier::Standard));
    }

    #[test]
    fn the_subagent_model_joins_when_it_differs() {
        let env = env_block(json!({
            "ANTHROPIC_BASE_URL": "https://relay.example/v1",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.3",
            "CLAUDE_CODE_SUBAGENT_MODEL": "glm-5.3-air[1M]"
        }));
        let p = provider_from_env("id".into(), "Relay".into(), &env, false).unwrap();
        assert_eq!(p.models.len(), 2);
        assert!(p.models.iter().any(|m| m.upstream_model == "glm-5.3-air"
            && m.tier.is_none()));
    }

    #[test]
    fn providers_without_a_base_url_are_dropped() {
        // CC Switch ships an empty "official" row.
        let env = env_block(json!({"ANTHROPIC_AUTH_TOKEN": "sk-x"}));
        assert!(provider_from_env("id".into(), "Empty".into(), &env, false).is_none());
    }

    #[test]
    fn ids_are_slugified_and_made_unique() {
        let taken = |id: &str| id == "deepseek" || id == "deepseek-2";
        assert_eq!(unique_provider_id("DeepSeek", &taken), "deepseek-3");
        assert_eq!(unique_provider_id("Xiaomi MiMo", &taken), "xiaomi-mimo");
        assert_eq!(unique_provider_id("---", &taken), "provider");
    }

    #[test]
    fn drafts_become_provider_configs_with_the_relay_defaults() {
        let draft = CcProvider {
            source_id: "abc".into(),
            name: "StepFun".into(),
            base_url: "https://api.stepfun.com/step_plan/".into(),
            api_key: Some("k".into()),
            models: vec![CcModel {
                upstream_model: "step-3.7-flash".into(),
                tier: Some(ModelTier::Standard),
            }],
            is_current: true,
        };
        let (provider, models) = to_zroutery(&draft, "stepfun".into(), 0, Some(3_000_000));
        assert_eq!(provider.id, "stepfun");
        assert_eq!(provider.base_url, "https://api.stepfun.com/step_plan");
        assert_eq!(provider.kind, ProviderKind::Anthropic);
        assert!(provider.impersonate_claude_code);
        // 3,000,000 ms is 3000 s: within the clamp, carried over as-is.
        assert_eq!(provider.timeout_secs, 3000);
        // Beyond the hour it is clamped, not carried literally.
        let (provider, _) = to_zroutery(&draft, "stepfun".into(), 0, Some(9_000_000));
        assert_eq!(provider.timeout_secs, 3600);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].upstream_model, "step-3.7-flash");
        assert_eq!(models[0].tier, Some(ModelTier::Standard));
        assert_eq!(models[0].exposed_id(), "stepfun-step-3.7-flash");
    }

    #[test]
    fn legacy_json_shape_parses() {
        let dir = std::env::temp_dir().join(format!("ccswitch-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"providers": {"claude": [
                {"id": "p1", "name": "Relay One",
                 "settings_config": {"env": {
                    "ANTHROPIC_BASE_URL": "https://one.example/v1",
                    "ANTHROPIC_AUTH_TOKEN": "sk-1",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "m1"}}},
                {"id": "p2", "name": "Empty", "settings_config": {"env": {}}}
            ]}}"#,
        )
        .unwrap();
        let providers = read_from_legacy_json(&dir.join("config.json")).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "Relay One");
        assert_eq!(providers[0].models.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Reads this machine's real CC Switch database. Opt-in —
    /// `cargo test -p zroutery ccswitch -- --ignored` — because it depends
    /// on a local installation. What it checks is the parsing contract: every
    /// provider that has a base URL yields at least one model, and no key
    /// material appears in the model list.
    #[test]
    #[ignore = "needs a local CC Switch installation"]
    fn the_real_local_database_parses() {
        let providers = read_providers().expect("cc-switch is installed on this machine");
        assert!(!providers.is_empty(), "found no providers at all");
        for p in &providers {
            assert!(!p.base_url.is_empty(), "{} has no base url", p.name);
            for m in &p.models {
                assert!(!m.upstream_model.contains('['), "modifier survived: {m:?}");
                assert!(!m.upstream_model.contains(']'), "modifier survived: {m:?}");
            }
            println!(
                "[{}] {} -> {} models: [{}]",
                p.source_id,
                p.name,
                p.base_url,
                p.models
                    .iter()
                    .map(|m| format!(
                        "{}{}",
                        m.upstream_model,
                        m.tier
                            .map(|c| format!("({})", c.as_str()))
                            .unwrap_or_default()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}
