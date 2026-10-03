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

/// The outcome of the startup read.
///
/// `token_to_write` is `Some` exactly when startup is allowed to write the
/// loaded document back. A corrupt file whose backup could not be made does
/// not grant that permission: overwriting it would destroy the only copy of a
/// configuration the user can still repair by hand.
pub struct StartupConfig {
    pub config: AppConfig,
    pub token_to_write: Option<String>,
    pub warning: Option<String>,
}

/// Read the configuration for startup, preparing the file for a write of
/// whatever is loaded.
///
/// A missing file yields defaults that must be written; a readable one is
/// normalised and does not need rewriting. A corrupt one is copied aside first,
/// and only a verified copy allows the overwrite — otherwise the answer is an
/// error and the original file is left exactly as it was.
pub fn startup_load(dir: &Path) -> Result<StartupConfig, String> {
    let path = dir.join(FILE_NAME);
    if !path.exists() {
        let config = with_defaults(AppConfig::default());
        let token = config.server.auth_token.clone();
        return Ok(StartupConfig {
            config,
            token_to_write: Some(token),
            warning: None,
        });
    }

    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match serde_json::from_str::<AppConfig>(&text) {
        Ok(cfg) => {
            let had_token = !cfg.server.auth_token.trim().is_empty();
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
            // A folded-in legacy id and a freshly generated token both have to
            // reach disk, so the normalised document is written back; an
            // untouched document has nothing to rewrite.
            let token_to_write = if had_token && notes.is_empty() {
                None
            } else {
                Some(cfg.server.auth_token.clone())
            };
            Ok(StartupConfig {
                config: cfg,
                token_to_write,
                warning,
            })
        }
        Err(e) => {
            // The original is the only copy of the provider list, so it is not
            // replaced unless a verified copy of it exists.
            let backup = backup_file(&path, &text)?;
            Ok(StartupConfig {
                config: with_defaults(AppConfig::default()),
                token_to_write: None,
                warning: Some(format!(
                    "config.json could not be parsed ({e}); it was copied to {} and defaults were loaded. \
                     The copied file is left untouched, and fixing it and restarting restores the configuration",
                    backup.display(),
                )),
            })
        }
    }
}

/// Copy `path` to a new, never-reused sibling and verify the copy byte for
/// byte.
///
/// The new name carries a timestamp and a random suffix, so an earlier backup
/// of a previous bad file is never overwritten. The copy is created with the
/// same owner-only permissions as the configuration itself and then read back:
/// a write that appeared to succeed but did not store the bytes is not a backup.
fn backup_file(path: &Path, text: &str) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(FILE_NAME);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let unique = uuid::Uuid::new_v4().simple();
    let backup = path.with_file_name(format!("{name}.broken-{stamp}-{unique}"));
    if backup.exists() {
        return Err(format!(
            "cannot preserve {}: {} already exists",
            path.display(),
            backup.display(),
        ));
    }

    let write = || -> std::io::Result<()> {
        let mut source = std::fs::File::open(path)?;
        let mut target = create_private(&backup)?;
        std::io::copy(&mut source, &mut target)?;
        target.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&backup);
        return Err(format!(
            "cannot preserve {} as {}: {e}; it was left as it is, so fix or move it and start again",
            path.display(),
            backup.display(),
        ));
    }

    // Verify what is on disk, not what the write claimed to do.
    match std::fs::read(&backup) {
        Ok(read_back) if read_back == text.as_bytes() => Ok(backup),
        Ok(_) => {
            let _ = std::fs::remove_file(&backup);
            Err(format!(
                "the copy of {} in {} does not match it; {} was left as it is",
                path.display(),
                backup.display(),
                path.display(),
            ))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&backup);
            Err(format!(
                "the copy of {} in {} cannot be read back ({e}); {} was left as it is",
                path.display(),
                backup.display(),
                path.display(),
            ))
        }
    }
}

/// Read the configuration for startup, falling back to defaults when the file
/// is missing or unusable.
///
/// The second value is what to tell the user, and both startup paths show it
/// instead of a claim about a backup that was never made.
pub fn load_startup(dir: &Path) -> (AppConfig, Option<String>) {
    match startup_load(dir) {
        Ok(startup) => (startup.config, startup.warning),
        Err(e) => (with_defaults(AppConfig::default()), Some(e)),
    }
}

/// Read the configuration, falling back to defaults when the file is missing.
///
/// A corrupt file is preserved beside itself so the user can recover their
/// provider list by hand instead of silently losing it.
pub fn load(dir: &Path) -> (AppConfig, Option<String>) {
    load_startup(dir)
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

    #[cfg(unix)]
    let existed = dir.is_dir();
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // The directory holds the local auth token, so make it owner-only on unix.
    // Only tighten a directory we had to create: an explicit ZROUTERY_CONFIG_DIR
    // lives where the user already chose the permissions.
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file =
        create_private(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
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

/// Create `path` so that only its owner can read it.
///
/// The configuration document carries the local access token, so a
/// world-readable file would let any other user on the machine spend the
/// proxy credit. The mode is set at creation time so the file is never even
/// briefly world-readable; other platforms rely on their own default ACLs.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::config::{ModelEntry, ModelTier, ProviderConfig, ProviderKind};

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

    /// The backup files made for this directory, oldest first.
    fn backups(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("config.json.broken-"))
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn corrupt_file_is_copied_aside_verified_and_defaults_load() {
        let dir = tmpdir();
        let broken = "{not json with a unique provider marker";
        std::fs::write(dir.join(FILE_NAME), broken).unwrap();

        let (cfg, warning) = load(&dir);
        let warning = warning.unwrap();
        assert!(warning.contains("could not be parsed"), "{warning}");
        assert!(warning.contains("copied to"), "{warning}");
        assert!(
            !warning.contains("moved"),
            "the message must not claim a move that did not happen: {warning}"
        );

        // A verified copy exists, in a new file that the original was not
        // renamed to.
        let made = backups(&dir);
        assert_eq!(made.len(), 1, "expected exactly one backup: {made:?}");
        assert_eq!(std::fs::read_to_string(&made[0]).unwrap(), broken);
        assert!(warning.contains(&made[0].file_name().unwrap().to_string_lossy().to_string()));
        // The source of the copy is still there.
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            broken
        );
        assert!(!cfg.server.auth_token.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_backup_carries_the_owner_only_permissions_of_the_configuration() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmpdir();
        std::fs::write(dir.join(FILE_NAME), "{not json").unwrap();
        load(&dir);

        let made = backups(&dir);
        assert_eq!(made.len(), 1);
        let mode = std::fs::metadata(&made[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a backup must not be more readable than the original"
        );
    }

    #[test]
    fn an_earlier_backup_is_never_overwritten() {
        let dir = tmpdir();
        std::fs::write(dir.join(FILE_NAME), "first broken document").unwrap();
        load(&dir);
        let first = backups(&dir);

        std::fs::write(dir.join(FILE_NAME), "second broken document").unwrap();
        load(&dir);
        let all = backups(&dir);

        assert_eq!(all.len(), 2, "the second load overwrote the first backup");
        assert_eq!(
            std::fs::read_to_string(&first[0]).unwrap(),
            "first broken document"
        );
        assert!(
            all.iter().all(|p| p != &first[0]
                || std::fs::read_to_string(p).unwrap() == "first broken document"),
            "the earlier backup changed"
        );
    }

    /// A directory that cannot hold a new file is the failure this protects
    /// against: the original must survive and startup must be told not to
    /// write defaults over it.
    #[cfg(unix)]
    #[test]
    fn a_failed_backup_blocks_the_overwrite_and_keeps_the_original() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmpdir();
        // Not parseable as a document, and carrying a marker that proves the
        // exact original survived.
        let source = r#"{"providers": [ {"id": "kept-provider"} , TRUNCATED"#;
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, source).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // Nothing can be created in the directory any more, but the existing
        // document is still readable, exactly like a full or read-only disk.
        let read_only = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&dir, read_only).unwrap();

        let startup = startup_load(&dir);
        // Restore the mode before asserting, so a failing assertion can still
        // clean up and later tests are not affected.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = startup
            .err()
            .expect("a backup that cannot be made must be reported");
        assert!(err.contains("cannot preserve"), "{err}");
        assert!(err.contains("fix or move it and start again"), "{err}");

        // The only copy of the configuration is untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(backups(&dir).is_empty());
    }

    /// Startup may only write the loaded document back when it read one whole
    /// or created it; a kept corrupt file is never replaced.
    #[test]
    fn startup_does_not_grant_a_write_after_a_corrupt_read() {
        let dir = tmpdir();
        std::fs::write(dir.join(FILE_NAME), "{not json").unwrap();
        let startup = startup_load(&dir).unwrap();
        assert!(
            startup.token_to_write.is_none(),
            "defaults must not be written over a kept corrupt file"
        );

        // A missing file, by contrast, has to be created.
        let empty = tmpdir();
        let startup = startup_load(&empty).unwrap();
        assert_eq!(
            startup.token_to_write.as_deref(),
            Some(startup.config.server.auth_token.as_str()),
        );
    }

    #[test]
    fn a_normalised_document_is_written_back_at_startup() {
        let dir = tmpdir();
        // A configuration written before ids were derived: the free-form `id`
        // is folded into `aliases` and the file has a token already.
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{"server": {"auth_token": "zr-existing"},
                "models": [{"id": "legacy-name", "provider_id": "p",
                            "upstream_model": "gpt-legacy"}]}"#,
        )
        .unwrap();
        let startup = startup_load(&dir).unwrap();
        assert_eq!(
            startup.token_to_write.as_deref(),
            Some(startup.config.server.auth_token.as_str()),
            "the migrated aliases must reach disk"
        );
        save(&dir, &startup.config).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(FILE_NAME)).unwrap()).unwrap();
        let model = &written["models"][0];
        assert_eq!(model["aliases"], serde_json::json!(["legacy-name"]));
        assert!(model.get("id").is_none(), "{model}");

        // Nothing changed since the last write, so nothing is rewritten.
        let again = startup_load(&dir).unwrap();
        assert!(
            again.token_to_write.is_none(),
            "an already-normalised document needs no write"
        );
    }

    #[test]
    fn a_generated_token_is_written_back_at_startup() {
        let dir = tmpdir();
        std::fs::write(dir.join(FILE_NAME), r#"{"models": []}"#).unwrap();
        let startup = startup_load(&dir).unwrap();
        assert_eq!(
            startup.token_to_write.as_deref(),
            Some(startup.config.server.auth_token.as_str()),
            "a token generated at startup must reach disk"
        );
    }

    #[test]
    fn tokens_are_unique() {
        assert_ne!(generate_token(), generate_token());
    }
}
