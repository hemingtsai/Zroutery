//! Transactional runtime data, separate from configuration and credentials.
//! WAL lets readers, imports and serving share one database without replacing
//! another process's snapshot. No prompt, completion or secret is stored here.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::stats::RequestRecord;

#[derive(Debug)]
pub struct RuntimeStore {
    db: Mutex<Connection>,
}

impl RuntimeStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS usage (
                 id TEXT PRIMARY KEY, client TEXT NOT NULL, at TEXT NOT NULL, record TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS usage_client ON usage(client);
             CREATE TABLE IF NOT EXISTS wallet_holds (
                 scope TEXT PRIMARY KEY, record TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS balances (
                 scope TEXT PRIMARY KEY, record TEXT NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn append_usage(&self, records: &[RequestRecord]) -> Result<usize, String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|e| e.to_string())?;
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
                        safe.at.to_rfc3339(),
                        json
                    ],
                )
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(inserted)
    }

    pub fn usage(&self) -> Result<Vec<RequestRecord>, String> {
        let db = crate::sync::lock(&self.db);
        let mut stmt = db
            .prepare("SELECT record FROM usage ORDER BY at, id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            let json = row.map_err(|e| e.to_string())?;
            serde_json::from_str(&json).map_err(|e| e.to_string())
        })
        .collect()
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
