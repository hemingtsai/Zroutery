//! Transactional, forward-only migrations; newer schemas are refused before IO.

use rusqlite::{Connection, TransactionBehavior};

pub const SCHEMA_VERSION: i64 = 4;

pub(super) fn migrate(db: &mut Connection) -> Result<(), String> {
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version > SCHEMA_VERSION {
        return Err(format!("runtime schema {version} is newer than supported {SCHEMA_VERSION}; use a compatible binary"));
    }
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .map_err(|error| error.to_string())?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    // Another compatible process may have migrated while this connection was
    // waiting for the writer. Re-read inside the migration transaction.
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version > SCHEMA_VERSION {
        return Err(format!("runtime schema {version} is newer than supported {SCHEMA_VERSION}; use a compatible binary"));
    }
    // Version 0 includes the pre-versioned runtime DB. IF NOT EXISTS preserves
    // its accounting and latches rather than re-creating or clearing tables.
    if version < 1 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage (
                id TEXT PRIMARY KEY, client TEXT NOT NULL, at TEXT NOT NULL, record TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS usage_client ON usage(client);
             CREATE TABLE IF NOT EXISTS usage_revision (
                id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO usage_revision VALUES (1, 0);
             CREATE TRIGGER IF NOT EXISTS usage_insert_revision AFTER INSERT ON usage
                BEGIN UPDATE usage_revision SET revision = revision + 1 WHERE id = 1; END;
             CREATE TRIGGER IF NOT EXISTS usage_delete_revision AFTER DELETE ON usage
                BEGIN UPDATE usage_revision SET revision = revision + 1 WHERE id = 1; END;
             CREATE TABLE IF NOT EXISTS wallet_holds (scope TEXT PRIMARY KEY, record TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS balances (scope TEXT PRIMARY KEY, record TEXT NOT NULL);",
        )
        .map_err(|error| error.to_string())?;
    }
    if version < 2 {
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS usage_time ON usage(at, id);
             CREATE TRIGGER IF NOT EXISTS usage_update_revision AFTER UPDATE ON usage
                BEGIN UPDATE usage_revision SET revision = revision + 1 WHERE id = 1; END;
             CREATE TABLE accounting_baseline (
                id INTEGER PRIMARY KEY CHECK(id = 1), source TEXT NOT NULL,
                at TEXT NOT NULL, usage_watermark INTEGER NOT NULL, ledger TEXT NOT NULL
             );
             CREATE TABLE accounting_ledger (
                id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL, ledger TEXT NOT NULL
             );
             CREATE TABLE request_settlements (
                id TEXT PRIMARY KEY, at TEXT NOT NULL, record TEXT NOT NULL
             );
             CREATE TABLE attempt_facts (
                id TEXT PRIMARY KEY, request_id TEXT, completed INTEGER NOT NULL,
                record TEXT NOT NULL
             );
             CREATE INDEX attempt_request ON attempt_facts(request_id, id);
             CREATE TABLE attempt_charges (
                id TEXT PRIMARY KEY REFERENCES attempt_facts(id), record TEXT NOT NULL
             );"
        ).map_err(|error| error.to_string())?;
    }
    if version < 3 {
        tx.execute_batch(
            "CREATE TABLE binding_publications (
            id TEXT PRIMARY KEY, phase TEXT NOT NULL, record TEXT NOT NULL
        ); CREATE TABLE binding_versions (id TEXT PRIMARY KEY, record TEXT NOT NULL);",
        )
        .map_err(|error| error.to_string())?;
    }
    if version < 4 {
        // Old keys vary in fractional precision/offset spelling. Normalize only
        // the index key, never the historical record or its client identity.
        let mut last_id: Option<String> = None;
        loop {
            let rows = {
                let sql = if last_id.is_some() {
                    "SELECT id, at FROM usage WHERE id > ?1 ORDER BY id LIMIT 512"
                } else {
                    "SELECT id, at FROM usage WHERE ?1 IS NULL ORDER BY id LIMIT 512"
                };
                let mut statement = tx.prepare(sql).map_err(|error| error.to_string())?;
                let rows = statement
                    .query_map([&last_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(|error| error.to_string())?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?
            };
            if rows.is_empty() {
                break;
            }
            last_id = rows.last().map(|(id, _)| id.clone());
            for (id, at) in rows {
                let at = chrono::DateTime::parse_from_rfc3339(&at)
                    .map_err(|error| format!("cannot migrate usage timestamp: {error}"))?
                    .with_timezone(&chrono::Utc);
                tx.execute(
                    "UPDATE usage SET at = ?1 WHERE id = ?2",
                    rusqlite::params![super::usage_timestamp(&at), id],
                )
                .map_err(|error| error.to_string())?;
            }
        }
        tx.execute_batch(
            "CREATE INDEX usage_client_time ON usage(client, at DESC, id DESC);
             CREATE INDEX usage_provider_time ON usage(json_extract(record, '$.provider_id'), at DESC, id DESC);
             CREATE INDEX usage_model_time ON usage(json_extract(record, '$.resolved_model'), at DESC, id DESC);"
        ).map_err(|error| error.to_string())?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}
