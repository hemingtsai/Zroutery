//! Configuration file persistence.
//!
//! The document lives in the app config directory as plain JSON. It contains no
//! secrets, only a `key_ref` per provider.

use std::path::{Path, PathBuf};

use zroutery_core::config::AppConfig;

pub const FILE_NAME: &str = "config.json";

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
            Ok(cfg) => (with_defaults(cfg), None),
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

/// Write atomically: a crash mid-save must not truncate the config.
pub fn save(dir: &Path, cfg: &AppConfig) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::config::{ModelClass, ModelEntry, ProviderConfig, ProviderKind};

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
        cfg.models.push(ModelEntry::new(
            "deepseek-v4-pro",
            "deepseek",
            Some(ModelClass::Sonnet),
        ));
        save(&dir, &cfg).unwrap();

        let (back, warning) = load(&dir);
        assert!(warning.is_none());
        assert_eq!(back, cfg);
        // No secret material on disk.
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(text.contains("key_ref"));
        assert!(!text.contains("sk-"));
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
