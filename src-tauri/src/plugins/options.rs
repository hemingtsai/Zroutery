//! Host-authoritative options: the versioned envelope and its storage.
//! The plugin proposes, the schema and the worker hook gate, this module
//! persists. Options are non-secret plugin preferences: they never enter the
//! canonical config document and never carry credentials.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use zroutery_plugin_api::options::OptionsSchemaV1;
use zroutery_plugin_api::strict;

pub const MAX_OPTIONS_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum StoredOptions {
    /// Envelope version equals the package schema version and still validates.
    Current(BTreeMap<String, Value>),
    /// Persisted version is behind the package; migration owns the move.
    Stale {
        version: u32,
        options: BTreeMap<String, Value>,
    },
    /// Persisted version is ahead of a rolled-back package, the document is
    /// unusable (legacy bare map, corrupt, oversized) or the package declares
    /// no schema. The file stays untouched; the host reads options as absent.
    Inactive,
    Absent,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    options: BTreeMap<String, Value>,
}

pub fn options_path(config_dir: &Path, installation_id: &str) -> PathBuf {
    config_dir
        .join("plugins")
        .join("options")
        .join(format!("{installation_id}.json"))
}

/// Reads and classifies the persisted envelope against the package schema.
/// Deterministic and side-effect free: classification never rewrites anything.
pub fn load(
    config_dir: &Path,
    installation_id: &str,
    schema: Option<&OptionsSchemaV1>,
) -> StoredOptions {
    let Ok(bytes) = std::fs::read(options_path(config_dir, installation_id)) else {
        return StoredOptions::Absent;
    };
    let Some(schema) = schema else {
        return StoredOptions::Inactive;
    };
    let Ok(envelope) = strict::decode::<Envelope>(&bytes, MAX_OPTIONS_BYTES) else {
        return StoredOptions::Inactive;
    };
    match envelope.schema_version.cmp(&schema.schema_version) {
        std::cmp::Ordering::Equal => {
            if schema.validate_value(&envelope.options).is_empty() {
                StoredOptions::Current(envelope.options)
            } else {
                StoredOptions::Inactive
            }
        }
        std::cmp::Ordering::Less => StoredOptions::Stale {
            version: envelope.schema_version,
            options: envelope.options,
        },
        std::cmp::Ordering::Greater => StoredOptions::Inactive,
    }
}

/// Atomic write of an already-validated envelope. Callers run the host schema
/// and the worker hook first; storage is the last gate, not the first.
pub fn store(
    config_dir: &Path,
    installation_id: &str,
    schema_version: u32,
    options: &BTreeMap<String, Value>,
) -> Result<(), &'static str> {
    let bytes = serde_json::to_vec(&Envelope {
        schema_version,
        options: options.clone(),
    })
    .map_err(|_| "invalid_result")?;
    if bytes.len() > MAX_OPTIONS_BYTES {
        return Err("invalid_result");
    }
    let dir = config_dir.join("plugins").join("options");
    std::fs::create_dir_all(&dir).map_err(|_| "storage_unavailable")?;
    let path = dir.join(format!("{installation_id}.json"));
    let tmp = dir.join(format!(
        ".{installation_id}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file_options = std::fs::OpenOptions::new();
        file_options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            file_options.mode(0o600);
        }
        let mut file = file_options.open(&tmp).map_err(|_| "storage_unavailable")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "storage_unavailable")?;
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|_| "storage_unavailable")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|_| "storage_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zroutery_plugin_api::options::OptionsSchemaV1;

    fn schema() -> OptionsSchemaV1 {
        OptionsSchemaV1::decode(
            br#"{"schema_version":2,"fields":{"note":{"type":"string","required":true}}}"#,
        )
        .unwrap()
    }

    fn fixture_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zroutery-options-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stored_envelopes_classify_by_version_and_stay_current_only_when_valid() {
        let dir = fixture_dir();
        let installation = "0197ffff-1111-7111-8111-111111111111";
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Absent
        );
        store(
            &dir,
            installation,
            2,
            &BTreeMap::from([("note".into(), json!("x"))]),
        )
        .unwrap();
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Current(BTreeMap::from([("note".into(), json!("x"))]))
        );
        // A version behind the package is migration material, not an error.
        store(
            &dir,
            installation,
            1,
            &BTreeMap::from([("note".into(), json!("y"))]),
        )
        .unwrap();
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Stale {
                version: 1,
                options: BTreeMap::from([("note".into(), json!("y"))])
            }
        );
        // A version ahead of a rolled-back package is inactive, file preserved.
        store(
            &dir,
            installation,
            9,
            &BTreeMap::from([("note".into(), json!("z"))]),
        )
        .unwrap();
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Inactive
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_bare_maps_and_oversized_documents_read_as_inactive() {
        let dir = fixture_dir();
        let installation = "0197ffff-2222-7222-8222-222222222222";
        let path = options_path(&dir, installation);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"note":"legacy bare map"}"#).unwrap();
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Inactive
        );
        assert!(
            path.is_file(),
            "the unusable file is preserved, not deleted"
        );
        let oversized = format!(
            r#"{{"schema_version":2,"options":{{"note":"{}"}}}}"#,
            "x".repeat(MAX_OPTIONS_BYTES)
        );
        std::fs::write(&path, oversized).unwrap();
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Inactive
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn store_refuses_documents_over_the_envelope_cap() {
        let dir = fixture_dir();
        let installation = "0197ffff-3333-7333-8333-333333333333";
        let oversized = BTreeMap::from([("note".into(), json!("x".repeat(MAX_OPTIONS_BYTES)))]);
        assert_eq!(
            store(&dir, installation, 2, &oversized),
            Err("invalid_result")
        );
        assert!(!options_path(&dir, installation).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_current_envelope_that_no_longer_validates_is_inactive() {
        let dir = fixture_dir();
        let installation = "0197ffff-4444-7444-8444-444444444444";
        // The envelope claims the current version but breaks the schema.
        store(
            &dir,
            installation,
            2,
            &BTreeMap::from([("note".into(), json!(7))]),
        )
        .unwrap();
        assert_eq!(
            load(&dir, installation, Some(&schema())),
            StoredOptions::Inactive
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
