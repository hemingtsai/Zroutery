//! Configuration file persistence.
//!
//! The document lives in the app config directory as plain JSON. It contains no
//! secrets, only a `key_ref` per provider.

use std::path::{Path, PathBuf};

use zroutery_core::budget::Ledger;
use zroutery_core::config::AppConfig;

pub const FILE_NAME: &str = "config.json";
/// Spend sits beside the configuration but not inside it: it is data the proxy
/// produced, not a setting the user wrote.
pub const LEDGER_FILE: &str = "spend.json";

/// Read the configuration, falling back to defaults when the file is missing.
///
/// A corrupt file is preserved as `config.broken.json` so the user can recover
/// their provider list by hand instead of silently losing it.
pub fn load(dir: &Path) -> (AppConfig, Option<String>) {
    let path = dir.join(FILE_NAME);
    if !path.exists() {
        return (with_defaults(AppConfig::default()), None);
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<AppConfig>(&text) {
            Ok(cfg) => {
                let mut cfg = with_defaults(cfg);
                // Configurations written before ids were derived from the
                // provider keep working: their old ids become aliases.
                let notes = cfg.normalize();
                let warning = if notes.is_empty() {
                    None
                } else {
                    Some(format!(
                        "Model ids are now `<provider>-<model>`. {}",
                        notes.join(" ")
                    ))
                };
                (cfg, warning)
            }
            Err(e) => {
                let backup = dir.join("config.broken.json");
                let _ = std::fs::write(&backup, &text);
                (
                    with_defaults(AppConfig::default()),
                    Some(format!(
                        "config.json could not be parsed ({e}); it was moved to {} and defaults were loaded",
                        backup.display()
                    )),
                )
            }
        },
        Err(e) => (
            with_defaults(AppConfig::default()),
            Some(format!("cannot read {}: {e}", path.display())),
        ),
    }
}

/// Fill in anything that must exist before the server can run.
pub fn with_defaults(mut cfg: AppConfig) -> AppConfig {
    if cfg.server.auth_token.trim().is_empty() {
        cfg.server.auth_token = generate_token();
    }
    cfg
}

pub fn generate_token() -> String {
    format!("zr-{}", uuid::Uuid::new_v4().simple())
}

/// Read the spend ledger, pruning what has aged out of every budget window.
///
/// A missing or unreadable file is an empty ledger rather than an error: losing the
/// history is bad, and refusing to start because of it is worse.
pub fn load_ledger(dir: &Path) -> Ledger {
    let mut ledger = std::fs::read_to_string(dir.join(LEDGER_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Ledger>(&text).ok())
        .unwrap_or_default();
    ledger.prune(chrono::Local::now());
    ledger
}

pub fn save_ledger(dir: &Path, ledger: &Ledger) -> Result<(), String> {
    let text = serde_json::to_string(ledger).map_err(|e| e.to_string())?;
    write_atomically(dir, LEDGER_FILE, &text).map(|_| ())
}

/// Write atomically: a crash mid-save must not truncate the config.
pub fn save(dir: &Path, cfg: &AppConfig) -> Result<PathBuf, String> {
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    write_atomically(dir, FILE_NAME, &text)
}

/// Through a temporary file and a rename, so a crash cannot leave a half written
/// file where a whole one used to be.
///
/// The temporary file is flushed to disk before the rename: without that, a
/// power cut could make the rename durable while the data was not, leaving a
/// new name over empty bytes.
fn write_atomically(dir: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    use std::io::Write;

    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    file.sync_all()
        .map_err(|e| format!("cannot flush {}: {e}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::config::{ModelTier, ModelEntry, ProviderConfig, ProviderKind};

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zroutery-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_yields_defaults_with_a_token() {
        let dir = tmpdir();
        let (cfg, warning) = load(&dir);
        assert!(warning.is_none());
        assert!(cfg.server.auth_token.starts_with("zr-"));
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tmpdir();
        let mut cfg = AppConfig::default();
        cfg.server.auth_token = "zr-fixed".into();
        cfg.providers.push(ProviderConfig::new(
            "deepseek",
            "DeepSeek",
            ProviderKind::OpenAICompatible,
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-chat",
            Some(ModelTier::Standard),
        ));
        save(&dir, &cfg).unwrap();

        let (back, warning) = load(&dir);
        assert!(warning.is_none());
        assert_eq!(back, cfg);
        assert_eq!(back.exposed_ids(), vec!["deepseek-deepseek-chat"]);
        // No secret material on disk.
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(text.contains("key_ref"));
        assert!(!text.contains("sk-"));
    }

    #[test]
    fn loading_a_pre_0_2_file_migrates_ids_and_explains_itself() {
        let dir = tmpdir();
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{"server":{"auth_token":"zr-fixed"},
                "providers":[{"id":"deepseek","name":"DeepSeek","kind":"openai_compatible",
                              "base_url":"https://api.deepseek.com/v1"}],
                "models":[{"id":"deepseek-v4-pro","provider_id":"deepseek",
                           "upstream_model":"deepseek-v4-pro","class":"sonnet"}]}"#,
        )
        .unwrap();

        let (cfg, warning) = load(&dir);
        assert_eq!(cfg.exposed_ids(), vec!["deepseek-deepseek-v4-pro"]);
        assert_eq!(cfg.models[0].aliases, vec!["deepseek-v4-pro"]);
        let warning = warning.unwrap();
        assert!(warning.contains("<provider>-<model>"));
        assert!(warning.contains("deepseek-v4-pro"));

        // Saving and loading again is a no-op: the migration is not repeated.
        save(&dir, &cfg).unwrap();
        let (again, warning) = load(&dir);
        assert_eq!(again, cfg);
        assert!(warning.is_none());
    }

    #[test]
    fn corrupt_file_is_preserved_and_defaults_load() {
        let dir = tmpdir();
        std::fs::write(dir.join(FILE_NAME), "{not json").unwrap();
        let (cfg, warning) = load(&dir);
        assert!(warning.unwrap().contains("config.broken.json"));
        assert!(dir.join("config.broken.json").exists());
        assert!(!cfg.server.auth_token.is_empty());
    }

    #[test]
    fn tokens_are_unique() {
        assert_ne!(generate_token(), generate_token());
    }
}
