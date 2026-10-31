//! Compensatable keychain/config publication journal. Only references/evidence.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::RuntimeStore;
use crate::config::AccountBinding;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingPublication {
    pub id: String,
    pub provider_id: String,
    pub account_id: String,
    pub previous_version: String,
    pub previous_ref: String,
    pub staged_ref: String,
    pub binding: AccountBinding,
    pub phase: String,
}

impl RuntimeStore {
    pub fn journal_binding(&self, publication: &BindingPublication) -> Result<(), String> {
        if publication.id.is_empty() || publication.staged_ref.is_empty() {
            return Err("binding publication has no stable reference".into());
        }
        let json = serde_json::to_string(publication).map_err(|error| error.to_string())?;
        crate::sync::lock(&self.db)
            .execute(
                "INSERT INTO binding_publications(id, phase, record) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET phase=excluded.phase, record=excluded.record",
                params![publication.id, publication.phase, json],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn binding_publication(&self, id: &str) -> Result<Option<BindingPublication>, String> {
        let db = crate::sync::lock(&self.db);
        let json: Option<String> = db
            .query_row(
                "SELECT record FROM binding_publications WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        json.map(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
            .transpose()
    }

    pub fn pending_bindings(&self) -> Result<Vec<BindingPublication>, String> {
        let db = crate::sync::lock(&self.db);
        let mut stmt = db.prepare("SELECT record FROM binding_publications WHERE phase NOT IN ('published','discarded') ORDER BY id")
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())
        })
        .collect()
    }

    pub fn save_binding_version(
        &self,
        provider: &str,
        account: &str,
        binding: &AccountBinding,
    ) -> Result<(), String> {
        let id = format!("{provider}/{account}/{}", binding.version());
        let json = serde_json::to_string(binding).map_err(|error| error.to_string())?;
        crate::sync::lock(&self.db)
            .execute(
                "INSERT OR IGNORE INTO binding_versions VALUES (?1, ?2)",
                params![id, json],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
