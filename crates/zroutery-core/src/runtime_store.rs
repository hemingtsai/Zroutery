//! Transactional runtime data, separate from configuration and credentials.
//! WAL lets readers, imports and serving share one database without replacing
//! another process's snapshot. No prompt, completion or secret is stored here.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::stats::{DurableTotals, RequestRecord};

mod accounting;
mod attempts;
mod bindings;
mod history;
mod imports;
mod publications;
mod schema;
mod usage_reader;
pub use accounting::SettlementCommit;
pub use bindings::BindingPublication;
pub use imports::{ImportCommit, ImportRun};
pub use publications::{InferencePublication, INFERENCE_PUBLICATION_PUBLISHED};
pub use schema::SCHEMA_VERSION;
pub use usage_reader::UsageReader;

#[derive(Debug)]
pub struct RuntimeStore {
    db: Mutex<Connection>,
}

/// What a stats refresh needs, read in one snapshot.
///
/// `Summary` is the steady state: the durable aggregate row plus the recent
/// window replace the full-table scan that a growing history would otherwise
/// pay on every poll. `Full` is the fallback that also (re)builds the summary
/// row: a database written before the summary existed, an older binary that
/// writes usage rows without it, or a clear that dropped it.
pub(crate) enum UsageSnapshot {
    Unchanged,
    Summary {
        revision: i64,
        totals: DurableTotals,
        recent: Vec<RequestRecord>,
    },
    Full {
        revision: i64,
        records: Vec<RequestRecord>,
    },
}

impl RuntimeStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        schema::migrate(&mut db)?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Open an existing runtime database without migration or write capability.
    pub fn open_read_only(path: &Path) -> Result<Self, String> {
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error.to_string())?;
        db.busy_timeout(Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        db.pragma_update(None, "query_only", "ON")
            .map_err(|error| error.to_string())?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        /*
         * Schema 5 only added the imported-fact tables; every table the usage
         * reader queries exists unchanged in schema 4. Reading that file
         * read-only is both possible and the point of a standby view — the
         * alternative is refusing to show a colleague's database until the
         * owning build opens it. Older layouts are a different matter: before
         * schema 4 the timestamps were not normalized and the indexes the
         * queries assume were absent, so those still need their owning build
         * once.
         */
        if !(4..=SCHEMA_VERSION).contains(&version) {
            return Err(format!(
                "runtime database schema {version} is not readable without migration; \
                 schemas 4 to {SCHEMA_VERSION} are supported"
            ));
        }
        db.query_row(
            "SELECT revision FROM usage_revision WHERE id = 1",
            [],
            |_| Ok(()),
        )
        .map_err(|error| format!("runtime usage index is unavailable: {error}"))?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn usage_revision(&self) -> Result<i64, String> {
        crate::sync::lock(&self.db)
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    pub fn append_usage(&self, records: &[RequestRecord]) -> Result<usize, String> {
        self.append_usage_versioned(records)
            .map(|(_, _, inserted)| inserted)
    }

    /// Revision and insert count from the same write transaction. Another
    /// instance importing the same ids cannot make in-memory totals count twice.
    ///
    /// Rows that were actually inserted are folded into the durable summary in
    /// the same transaction, so the summary always describes exactly the
    /// revision it is stored with.
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
        let mut committed: Vec<RequestRecord> = Vec::new();
        for record in records {
            // Errors can quote a remote body, and decision traces contain more
            // than accounting needs. The durable form deliberately omits both.
            let mut safe = record.clone();
            safe.error = None;
            safe.routing_decision = None;
            let json = serde_json::to_string(&safe).map_err(|e| e.to_string())?;
            let written = tx
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
            if written == 1 {
                inserted += 1;
                committed.push(safe);
            }
        }
        let after: i64 = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !committed.is_empty() {
            advance_usage_summary(&tx, before, after, &committed)?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok((before, after, inserted))
    }

    pub fn usage(&self) -> Result<Vec<RequestRecord>, String> {
        self.usage_since(None)
            .map(|snapshot| snapshot.expect("a full snapshot was requested").1)
    }

    /// Read the revision and the projection inputs in one snapshot so a
    /// concurrent clear/import can never pair old totals with a newer revision.
    ///
    /// Unchanged reads are O(1). When the durable summary row describes the
    /// current revision, the read is one row plus the recent window; a full
    /// table scan remains only the fallback that also (re)builds that summary,
    /// so a history that grows with the life of the process is never re-read on
    /// a poll.
    pub(crate) fn usage_snapshot(
        &self,
        known: Option<i64>,
        recent_limit: usize,
    ) -> Result<UsageSnapshot, String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|e| e.to_string())?;
        let current = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())?;
        if known == Some(current) {
            return Ok(UsageSnapshot::Unchanged);
        }
        if let Some((revision, totals)) = read_usage_summary(&tx)? {
            if revision == current {
                let limit = i64::try_from(recent_limit.max(1)).unwrap_or(i64::MAX);
                let mut statement = tx
                    .prepare("SELECT record FROM usage ORDER BY at DESC, id DESC LIMIT ?1")
                    .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map([limit], |row| row.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                let mut recent = rows
                    .map(|row| {
                        let json = row.map_err(|e| e.to_string())?;
                        serde_json::from_str(&json).map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                // A full scan visits oldest first, and the ring expects that order.
                recent.reverse();
                return Ok(UsageSnapshot::Summary {
                    revision,
                    totals,
                    recent,
                });
            }
        }
        let mut statement = tx
            .prepare("SELECT record FROM usage ORDER BY at, id")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let records = rows
            .map(|row| {
                let json = row.map_err(|e| e.to_string())?;
                serde_json::from_str(&json).map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(UsageSnapshot::Full {
            revision: current,
            records,
        })
    }

    /// Store the durable aggregate for `revision` in its own transaction.
    ///
    /// A summary is only written when the revision still matches what the
    /// caller read: a stale summary must never be paired with a revision it
    /// does not describe, and a write that lost the race is simply retried by
    /// the next refresh that finds the summary missing or behind.
    pub(crate) fn save_usage_summary(
        &self,
        revision: i64,
        totals: &DurableTotals,
    ) -> Result<(), String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current: i64 = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if current != revision {
            return Ok(());
        }
        write_usage_summary(&tx, revision, totals)?;
        tx.commit().map_err(|e| e.to_string())
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
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let deleted = match client {
            Some(client) => tx
                .execute("DELETE FROM usage WHERE client = ?1", [client])
                .map_err(|e| e.to_string())?,
            None => tx
                .execute("DELETE FROM usage", [])
                .map_err(|e| e.to_string())?,
        };
        // The aggregate described rows that no longer exist. Dropping it makes
        // the next read rebuild from what remains and store a fresh summary.
        tx.execute("DELETE FROM usage_summary", [])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(deleted)
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

/// The durable summary row, `None` when it has never been built.
pub(super) fn read_usage_summary(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Option<(i64, DurableTotals)>, String> {
    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision, summary FROM usage_summary WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    row.map(|(revision, json)| {
        serde_json::from_str(&json)
            .map(|totals| (revision, totals))
            .map_err(|e| e.to_string())
    })
    .transpose()
}

/// Write the durable summary inside the caller's transaction, so the row
/// always describes exactly the revision it is stored with.
pub(super) fn write_usage_summary(
    tx: &rusqlite::Transaction<'_>,
    revision: i64,
    totals: &DurableTotals,
) -> Result<(), String> {
    let json = serde_json::to_string(totals).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO usage_summary(id, revision, summary) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET revision = ?1, summary = ?2",
        params![revision, json],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Fold newly committed rows into the durable summary, inside the caller's
/// transaction.
///
/// A summary that does not describe `before` — another binary wrote usage
/// without maintaining it — is left alone: a stale revision makes the next
/// read rebuild from the table, which is the only way to absorb rows this
/// process never saw. A missing summary stays missing for the same reason.
pub(super) fn advance_usage_summary(
    tx: &rusqlite::Transaction<'_>,
    before: i64,
    after: i64,
    inserted: &[RequestRecord],
) -> Result<(), String> {
    let Some((revision, mut totals)) = read_usage_summary(tx)? else {
        return Ok(());
    };
    if revision != before {
        return Ok(());
    }
    for record in inserted {
        totals.observe(record);
    }
    write_usage_summary(tx, after, &totals)
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
