//! Ownership for the extra Codex catalogue file. It participates in restore;
//! manual edits are conflicts, never recursively deleted or silently discarded.

use serde::{Deserialize, Serialize};
use zroutery_core::file_transaction::FileChange;

use super::Desktop;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogueOwnership {
    original: Option<String>,
    last_applied: String,
    prepared: Option<String>,
}

impl Desktop {
    fn catalogue_dir(&self) -> std::path::PathBuf {
        self.config_dir.join("takeover")
    }

    pub(super) fn prepare_codex_catalogue(
        &self,
        value: &serde_json::Value,
    ) -> Result<Vec<FileChange>, String> {
        let dir = self.catalogue_dir();
        let path = dir.join("codex-models.json");
        let manifest = dir.join("codex-models-owner.json");
        let current = read_optional(&path)?;
        let current_text = current
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|error| format!("catalogue is not UTF-8: {error}"))?;
        let next = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
        let previous_owner = read_optional(&manifest)?;
        let mut owner = match previous_owner.as_deref() {
            Some(json) => serde_json::from_slice::<CatalogueOwnership>(json)
                .map_err(|error| format!("catalogue ownership is unreadable: {error}"))?,
            None => CatalogueOwnership {
                original: current_text.map(str::to_owned),
                last_applied: String::new(),
                prepared: None,
            },
        };
        if !owner.last_applied.is_empty()
            && current_text != Some(owner.last_applied.as_str())
            && !owner
                .prepared
                .as_deref()
                .is_some_and(|prepared| current_text == Some(prepared))
        {
            return Err("the owned Codex catalogue was changed manually; resolve or restore it before synchronizing".into());
        }
        owner.last_applied = next.clone();
        owner.prepared = None;
        Ok(vec![
            FileChange {
                path,
                before: current,
                after: Some(next.into_bytes()),
            },
            FileChange {
                path: manifest,
                before: previous_owner,
                after: Some(serde_json::to_vec_pretty(&owner).map_err(|error| error.to_string())?),
            },
        ])
    }

    pub(super) fn prepare_codex_restore(
        &self,
        keep_external: bool,
    ) -> Result<Vec<FileChange>, String> {
        let dir = self.catalogue_dir();
        let manifest = dir.join("codex-models-owner.json");
        let previous_owner = read_optional(&manifest)?;
        let owner: CatalogueOwnership = match previous_owner.as_deref() {
            Some(json) => serde_json::from_slice(json)
                .map_err(|error| format!("catalogue ownership is unreadable: {error}"))?,
            None => return Ok(Vec::new()),
        };
        let path = dir.join("codex-models.json");
        let current = read_optional(&path)?;
        let matches = current.as_deref() == Some(owner.last_applied.as_bytes())
            || owner
                .prepared
                .as_deref()
                .is_some_and(|prepared| current.as_deref() == Some(prepared.as_bytes()));
        let mut changes = vec![FileChange {
            path: manifest,
            before: previous_owner,
            after: None,
        }];
        if !matches {
            if keep_external {
                return Ok(changes);
            }
            return Err("the Codex catalogue has external edits; choose to keep them instead of removing the file".into());
        }
        changes.insert(
            0,
            FileChange {
                path,
                before: current,
                after: owner.original.map(String::into_bytes),
            },
        );
        Ok(changes)
    }
}

pub(super) fn read_optional(path: &std::path::Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}
