//! Transactional runtime data, separate from configuration and credentials.
//! WAL lets readers, imports and serving share one database without replacing
//! another process's snapshot. No prompt, completion or secret is stored here.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::stats::RequestRecord;

mod accounting;
mod bindings;
mod history;
mod schema;
pub use accounting::SettlementCommit;
pub use bindings::BindingPublication;
pub use schema::SCHEMA_VERSION;

#[derive(Debug)]
pub struct RuntimeStore {
    db: Mutex<Connection>,
}

impl RuntimeStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        schema::migrate(&mut db)?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn append_usage(&self, records: &[RequestRecord]) -> Result<usize, String> {
        self.append_usage_versioned(records)
            .map(|(_, _, inserted)| inserted)
    }

    /// Revision and insert count from the same write transaction. Another
    /// instance importing the same ids cannot make in-memory totals count twice.
    pub(crate) fn append_usage_versioned(
        &self,
        records: &[RequestRecord],
    ) -> Result<(i64, i64, usize), String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let before: i64 = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let mut inserted = 0;
        for record in records {
            // Errors can quote a remote body, and decision traces contain more
            // than accounting needs. The durable form deliberately omits both.
            let mut safe = record.clone();
            safe.error = None;
            safe.routing_decision = None;
            let json = serde_json::to_string(&safe).map_err(|e| e.to_string())?;
            inserted += tx
                .execute(
                    "INSERT OR IGNORE INTO usage(id, client, at, record) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        safe.id,
                        safe.client.client_id.as_str(),
                        usage_timestamp(&safe.at),
                        json
                    ],
                )
                .map_err(|e| e.to_string())?;
        }
        let after: i64 = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok((before, after, inserted))
    }

    pub fn usage(&self) -> Result<Vec<RequestRecord>, String> {
        self.usage_since(None)
            .map(|snapshot| snapshot.expect("a full snapshot was requested").1)
    }

    /// Read the revision and rows in one snapshot so a concurrent clear/import
    /// can never pair old totals with a newer revision. Unchanged reads are O(1).
    pub(crate) fn usage_since(
        &self,
        revision: Option<i64>,
    ) -> Result<Option<(i64, Vec<RequestRecord>)>, String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|e| e.to_string())?;
        let current = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())?;
        if revision == Some(current) {
            return Ok(None);
        }
        let mut stmt = tx
            .prepare("SELECT record FROM usage ORDER BY at, id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let records = rows
            .map(|row| {
                let json = row.map_err(|e| e.to_string())?;
                serde_json::from_str(&json).map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some((current, records)))
    }

    pub fn clear_usage(&self, client: Option<&str>) -> Result<usize, String> {
        let db = crate::sync::lock(&self.db);
        match client {
            Some(client) => db.execute("DELETE FROM usage WHERE client = ?1", [client]),
            None => db.execute("DELETE FROM usage", []),
        }
        .map_err(|e| e.to_string())
    }

    pub fn hold_wallet(&self, scope: &str, json: &str) -> Result<(), String> {
        crate::sync::lock(&self.db)
            .execute(
                "INSERT OR IGNORE INTO wallet_holds(scope, record) VALUES (?1, ?2)",
                params![scope, json],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn release_wallet(&self, scope: &str) -> Result<(), String> {
        crate::sync::lock(&self.db)
            .execute("DELETE FROM wallet_holds WHERE scope = ?1", [scope])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn wallet_held(&self, scope: &str) -> Result<bool, String> {
        crate::sync::lock(&self.db)
            .query_row(
                "SELECT 1 FROM wallet_holds WHERE scope = ?1",
                [scope],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(|e| e.to_string())
    }

    pub fn wallet_holds(&self) -> Result<Vec<String>, String> {
        let db = crate::sync::lock(&self.db);
        let mut stmt = db
            .prepare("SELECT record FROM wallet_holds ORDER BY scope")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut holds = Vec::new();
        for row in rows {
            holds.push(row.map_err(|e| e.to_string())?);
        }
        Ok(holds)
    }

    pub fn save_balance(&self, scope: &str, json: &str) -> Result<(), String> {
        crate::sync::lock(&self.db)
            .execute(
                "INSERT INTO balances(scope, record) VALUES (?1, ?2)
             ON CONFLICT(scope) DO UPDATE SET record = excluded.record",
                params![scope, json],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn balances(&self) -> Result<Vec<(String, String)>, String> {
        let db = crate::sync::lock(&self.db);
        let mut stmt = db
            .prepare("SELECT scope, record FROM balances ORDER BY scope")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?;
        let mut balances = Vec::new();
        for row in rows {
            balances.push(row.map_err(|e| e.to_string())?);
        }
        Ok(balances)
    }
}

/// A fixed-width UTC index key retains nanoseconds and sorts chronologically.
/// The original RFC3339 spelling remains untouched inside historical records.
fn usage_timestamp(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientId;
    use crate::ir::{Dialect, Usage};
    use crate::stats::RecordBuilder;

    const WALLET: &str = r#"{"provider_id":"provider","account_id":"account","key_id":null,"held_at":1,"reason":"balance_exhausted"}"#;

    fn record(id: &str, client: &str) -> RequestRecord {
        let mut builder = RecordBuilder::new(Dialect::Anthropic, "sonnet-class", false);
        builder
            .resolved("model", "provider")
            .usage(Usage::default())
            .attempt();
        let mut record = builder.finish(12);
        record.id = id.to_string();
        record.client.client_id = ClientId::trusted(client);
        record
    }

    fn wallet_scope() -> String {
        serde_json::to_string(&("provider", Some("account"), None::<&str>)).unwrap()
    }

    #[test]
    fn accounting_and_latches_are_shared_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.sqlite3");
        let scope = wallet_scope();
        {
            let first = RuntimeStore::open(&path).unwrap();
            let second = RuntimeStore::open(&path).unwrap();

            first
                .append_usage(&[record("one", "client-a"), record("two", "client-b")])
                .unwrap();
            assert_eq!(second.usage().unwrap().len(), 2);

            first.hold_wallet(&scope, WALLET).unwrap();
            assert!(second.wallet_held(&scope).unwrap());
            second.release_wallet(&scope).unwrap();
            assert!(!first.wallet_held(&scope).unwrap());
            first.hold_wallet(&scope, WALLET).unwrap();

            first
                .save_balance("provider", r#"{"balance":null,"error":null}"#)
                .unwrap();
            assert_eq!(second.balances().unwrap().len(), 1);

            assert_eq!(second.clear_usage(Some("client-a")).unwrap(), 1);
            assert_eq!(second.usage().unwrap().len(), 1);
            assert_eq!(second.clear_usage(None).unwrap(), 1);
            assert!(second.usage().unwrap().is_empty());
        }

        let reopened = RuntimeStore::open(&path).unwrap();
        assert!(reopened.usage().unwrap().is_empty());
        assert!(reopened.wallet_held(&scope).unwrap());
        assert_eq!(reopened.balances().unwrap().len(), 1);
    }
}
