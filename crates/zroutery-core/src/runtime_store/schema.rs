//! Transactional, forward-only migrations; newer schemas are refused before IO.

use rusqlite::{Connection, TransactionBehavior};

pub const SCHEMA_VERSION: i64 = 8;

pub(super) fn migrate(db: &mut Connection) -> Result<(), String> {
    migrate_to(db, SCHEMA_VERSION)
}

/// The same migration sequence builds genuine historical fixtures in tests.
pub(super) fn migrate_to(db: &mut Connection, target: i64) -> Result<(), String> {
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version > target {
        return Err(format!(
            "runtime schema {version} is newer than supported {target}; use a compatible binary"
        ));
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
    if version > target {
        return Err(format!(
            "runtime schema {version} is newer than supported {target}; use a compatible binary"
        ));
    }
    // Version 0 includes the pre-versioned runtime DB. IF NOT EXISTS preserves
    // its accounting and latches rather than re-creating or clearing tables.
    if version < 1 && target >= 1 {
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
    if version < 2 && target >= 2 {
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
    if version < 3 && target >= 3 {
        tx.execute_batch(
            "CREATE TABLE binding_publications (
            id TEXT PRIMARY KEY, phase TEXT NOT NULL, record TEXT NOT NULL
        ); CREATE TABLE binding_versions (id TEXT PRIMARY KEY, record TEXT NOT NULL);",
        )
        .map_err(|error| error.to_string())?;
    }
    if version < 4 && target >= 4 {
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
    if version < 5 && target >= 5 {
        // Imported history is its own fact table: its rows may be day totals
        // rather than requests, and its columns carry provenance the live usage
        // table has no field for. Keeping them apart is what lets the two be
        // added up deliberately instead of by accident.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS imported_fact (
                source TEXT NOT NULL,
                instance TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                revision TEXT NOT NULL,
                at TEXT NOT NULL,
                day TEXT NOT NULL,
                granularity TEXT NOT NULL,
                duplicate TEXT NOT NULL,
                duplicate_of TEXT,
                client TEXT,
                provider_id TEXT,
                model TEXT,
                requests INTEGER NOT NULL,
                failures INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL,
                cache_write_tokens INTEGER NOT NULL,
                cost_usd REAL,
                priced INTEGER NOT NULL,
                record TEXT NOT NULL,
                PRIMARY KEY (source, instance, entity_id)
             );
             CREATE INDEX IF NOT EXISTS imported_fact_time ON imported_fact(at, source, instance, entity_id);
             CREATE INDEX IF NOT EXISTS imported_fact_day ON imported_fact(day, granularity, duplicate);
             CREATE INDEX IF NOT EXISTS imported_fact_client_time ON imported_fact(client, at);
             CREATE INDEX IF NOT EXISTS imported_fact_provider_time ON imported_fact(provider_id, at);
             CREATE INDEX IF NOT EXISTS imported_fact_model_time ON imported_fact(model, at);
             CREATE TABLE IF NOT EXISTS imported_revision (
                id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO imported_revision VALUES (1, 0);
             CREATE TRIGGER IF NOT EXISTS imported_fact_insert_revision AFTER INSERT ON imported_fact
                BEGIN UPDATE imported_revision SET revision = revision + 1 WHERE id = 1; END;
             CREATE TRIGGER IF NOT EXISTS imported_fact_update_revision AFTER UPDATE ON imported_fact
                BEGIN UPDATE imported_revision SET revision = revision + 1 WHERE id = 1; END;
             CREATE TRIGGER IF NOT EXISTS imported_fact_delete_revision AFTER DELETE ON imported_fact
                BEGIN UPDATE imported_revision SET revision = revision + 1 WHERE id = 1; END;",
        )
        .map_err(|error| error.to_string())?;
    }
    if version < 6 && target >= 6 {
        // Imported spend becomes currency-qualified. Schema 5 carried a single
        // `cost_usd` column that a fact reached only when its currency was
        // USD, so a CNY total survived in the record JSON and entered no
        // aggregate. The two new columns carry the amount together with the
        // currency it was billed in; the backfill recovers both from the
        // stored fact, and the USD-only column — whose name promised a
        // conversion it never performed — is dropped rather than left to drift
        // out of date.
        tx.execute_batch(
            "ALTER TABLE imported_fact ADD COLUMN cost_currency TEXT;
             ALTER TABLE imported_fact ADD COLUMN cost_amount REAL;",
        )
        .map_err(|error| error.to_string())?;
        tx.execute_batch(
            "UPDATE imported_fact SET
                cost_currency = json_extract(record, '$.cost.currency'),
                cost_amount = json_extract(record, '$.cost.amount')
              WHERE json_extract(record, '$.cost.currency') IS NOT NULL
                AND json_extract(record, '$.cost.amount') IS NOT NULL;",
        )
        .map_err(|error| error.to_string())?;
        tx.execute_batch("ALTER TABLE imported_fact DROP COLUMN cost_usd;")
            .map_err(|error| error.to_string())?;
        // An import is published whole or not at all: facts read from the
        // source are staged in `import_stage` first, and one transaction moves
        // every staged row into `imported_fact`. The run row carries the
        // cursor, so an interrupted import is recognised and finished instead
        // of silently leaving half of itself published.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS import_run (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                instance TEXT NOT NULL,
                range_from TEXT,
                range_until TEXT,
                cursor TEXT,
                staged INTEGER NOT NULL DEFAULT 0,
                state TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS import_run_active
                ON import_run(source, instance, state);
             CREATE TABLE IF NOT EXISTS import_stage (
                run_id TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                record TEXT NOT NULL,
                PRIMARY KEY (run_id, entity_id)
             );",
        )
        .map_err(|error| error.to_string())?;
    }
    if version < 7 && target >= 7 {
        // Version 6's in-progress scans were not bound to a source snapshot or
        // range. Their unpublished data cannot safely be resumed. Retire only
        // these stages; published history and accounting remain intact.
        tx.execute_batch(
            "DELETE FROM import_stage;
             UPDATE import_run SET state = 'discarded', staged = 0, cursor = NULL
               WHERE state = 'staging';
             ALTER TABLE import_run ADD COLUMN source_version INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE import_run ADD COLUMN base_revision INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE import_run ADD COLUMN fingerprint TEXT;
             ALTER TABLE import_run ADD COLUMN preview TEXT;
             ALTER TABLE import_run ADD COLUMN outcome TEXT;",
        )
        .map_err(|error| error.to_string())?;
    }
    if version < 8 && target >= 8 {
        // The configuration document and the credential store share no
        // transaction. This journal records the staging reference a publication
        // is about to create before the secret exists, so a process death in
        // between leaves evidence the startup sweep can settle from the saved
        // document. References only: no secret is ever stored here.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS inference_publications (
                id TEXT PRIMARY KEY, phase TEXT NOT NULL, record TEXT NOT NULL
             );",
        )
        .map_err(|error| error.to_string())?;
    }
    tx.pragma_update(None, "user_version", target)
        .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}
