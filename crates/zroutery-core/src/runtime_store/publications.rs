//! Durable journal for inference-key secret publication.
//!
//! The configuration document and the credential store share no transaction.
//! A publication therefore records the reference it is about to create before
//! that secret exists, and completes or clears the record afterwards. A process
//! death in between leaves a row a startup sweep can settle from the saved
//! document. Only references and identity live here; never a secret.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{usage_timestamp, RuntimeStore};

/// A publication is in flight: the staging reference may or may not exist yet.
pub const INFERENCE_PUBLICATION_PENDING: &str = "pending";
/// The saved document names the staging reference; the secret stays in use.
pub const INFERENCE_PUBLICATION_PUBLISHED: &str = "published";

/// One inference-key secret publication, durable across a process death.
///
/// `id` is the staging reference itself: it is unique per publication and the
/// same value the recovery sweep compares against the saved document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferencePublication {
    pub id: String,
    pub provider_id: String,
    pub account_id: String,
    pub key_id: String,
    pub staged_ref: String,
    pub created_at: String,
    pub phase: String,
}

impl InferencePublication {
    /// A pending row for a secret about to be staged at `staged_ref`.
    pub fn pending(provider_id: &str, account_id: &str, key_id: &str, staged_ref: &str) -> Self {
        Self {
            id: staged_ref.to_string(),
            provider_id: provider_id.to_string(),
            account_id: account_id.to_string(),
            key_id: key_id.to_string(),
            staged_ref: staged_ref.to_string(),
            created_at: usage_timestamp(&chrono::Utc::now()),
            phase: INFERENCE_PUBLICATION_PENDING.to_string(),
        }
    }
}

impl RuntimeStore {
    pub fn journal_inference_publication(
        &self,
        publication: &InferencePublication,
    ) -> Result<(), String> {
        if publication.id.is_empty() || publication.staged_ref.is_empty() {
            return Err("inference publication has no stable reference".into());
        }
        let json = serde_json::to_string(publication).map_err(|error| error.to_string())?;
        crate::sync::lock(&self.db)
            .execute(
                "INSERT INTO inference_publications(id, phase, record) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET phase=excluded.phase, record=excluded.record",
                params![publication.id, publication.phase, json],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn inference_publication(&self, id: &str) -> Result<Option<InferencePublication>, String> {
        let db = crate::sync::lock(&self.db);
        let json: Option<String> = db
            .query_row(
                "SELECT record FROM inference_publications WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        json.map(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
            .transpose()
    }

    pub fn pending_inference_publications(&self) -> Result<Vec<InferencePublication>, String> {
        let db = crate::sync::lock(&self.db);
        let mut stmt = db
            .prepare(
                "SELECT record FROM inference_publications
                 WHERE phase NOT IN (?1, 'discarded') ORDER BY id",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([INFERENCE_PUBLICATION_PUBLISHED], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| error.to_string())?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())
        })
        .collect()
    }

    /// Mark a publication completed. The saved document names its reference,
    /// so the secret behind it stays in use. Idempotent: a missing or already
    /// completed row is not an error.
    pub fn complete_inference_publication(&self, id: &str) -> Result<(), String> {
        crate::sync::lock(&self.db)
            .execute(
                "UPDATE inference_publications
                 SET phase=?2, record=json_set(record, '$.phase', ?2)
                 WHERE id=?1",
                params![id, INFERENCE_PUBLICATION_PUBLISHED],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Forget a publication whose staging secret was compensated or never
    /// landed. Idempotent: a missing row is not an error.
    pub fn discard_inference_publication(&self, id: &str) -> Result<(), String> {
        crate::sync::lock(&self.db)
            .execute("DELETE FROM inference_publications WHERE id=?1", [id])
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(staged_ref: &str) -> InferencePublication {
        InferencePublication::pending("relay", "a", "k1", staged_ref)
    }

    #[test]
    fn a_legacy_database_upgrades_to_the_publication_schema_and_rejects_old_writers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        super::super::schema::migrate_to(&mut db, 7).unwrap();
        db.execute(
            "INSERT INTO usage_revision(id, revision) VALUES (1, 41)
             ON CONFLICT(id) DO UPDATE SET revision=excluded.revision",
            [],
        )
        .unwrap();
        drop(db);

        let store = RuntimeStore::open(&path).unwrap();
        assert_eq!(super::super::SCHEMA_VERSION, 9);
        store
            .journal_inference_publication(&publication("staged"))
            .unwrap();
        assert_eq!(store.pending_inference_publications().unwrap().len(), 1);

        let version: i64 = crate::sync::lock(&store.db)
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 9);
        assert_eq!(store.usage_revision().unwrap(), 41, "history is preserved");

        let mut old_writer = rusqlite::Connection::open(&path).unwrap();
        assert!(super::super::schema::migrate_to(&mut old_writer, 7)
            .unwrap_err()
            .contains("newer"));
    }

    #[test]
    fn completing_and_discarding_a_publication_are_idempotent_across_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        {
            let store = RuntimeStore::open(&path).unwrap();
            let pending = publication("staged");
            store.journal_inference_publication(&pending).unwrap();
            assert_eq!(store.pending_inference_publications().unwrap().len(), 1);

            store.complete_inference_publication(&pending.id).unwrap();
            assert!(store.pending_inference_publications().unwrap().is_empty());
            let completed = store.inference_publication(&pending.id).unwrap().unwrap();
            assert_eq!(completed.phase, INFERENCE_PUBLICATION_PUBLISHED);
            store.complete_inference_publication(&pending.id).unwrap();

            let other = publication("other");
            store.journal_inference_publication(&other).unwrap();
            store.discard_inference_publication(&other.id).unwrap();
            assert!(store.inference_publication(&other.id).unwrap().is_none());
            store.discard_inference_publication(&other.id).unwrap();
        }

        let reopened = RuntimeStore::open(&path).unwrap();
        assert!(reopened
            .pending_inference_publications()
            .unwrap()
            .is_empty());
        assert_eq!(
            reopened
                .inference_publication("staged")
                .unwrap()
                .unwrap()
                .phase,
            INFERENCE_PUBLICATION_PUBLISHED
        );
        assert!(reopened.inference_publication("other").unwrap().is_none());
    }

    #[test]
    fn a_publication_without_a_stable_reference_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let store = RuntimeStore::open(&directory.path().join("runtime.db")).unwrap();
        let mut empty = publication("staged");
        empty.id.clear();
        assert!(store.journal_inference_publication(&empty).is_err());
        empty = publication("");
        assert!(store.journal_inference_publication(&empty).is_err());
    }
}
