//! One transaction owns settlement evidence, usage publication and budget spend.

use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

use super::RuntimeStore;
use crate::accounting::{AttemptFact, AttemptState, SettlementFact, SETTLEMENT_SCHEMA_VERSION};
use crate::budget::Ledger;

pub struct SettlementCommit {
    pub ledger: Ledger,
    pub usage_before: i64,
    pub usage_after: i64,
    pub inserted: bool,
}

impl RuntimeStore {
    /// Adopt a legacy opening balance only once. Usage already present before
    /// activation is history, not another debit onto this baseline.
    pub fn initialize_accounting(&self, opening: &Ledger, source: &str) -> Result<Ledger, String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        if let Some(ledger) = read_ledger(&tx)? {
            return Ok(ledger);
        }
        opening.validate()?;
        let json = serde_json::to_string(opening).map_err(|e| e.to_string())?;
        let watermark = usage_revision(&tx)?;
        tx.execute(
            "INSERT INTO accounting_baseline(id, source, at, usage_watermark, ledger) VALUES (1, ?1, ?2, ?3, ?4)",
            params![source, chrono::Utc::now().to_rfc3339(), watermark, json],
        ).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO accounting_ledger VALUES (1, 0, ?1)", [&json])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(opening.clone())
    }

    pub fn accounting_ledger(&self) -> Result<Option<Ledger>, String> {
        let db = crate::sync::lock(&self.db);
        let json: Option<String> = db
            .query_row(
                "SELECT ledger FROM accounting_ledger WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        json.map(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
            .transpose()
    }

    /// Before-send identity evidence and late observed failure charges share the
    /// same stable attempt ID. A disk retry is not an upstream retry.
    pub fn record_attempt(&self, attempt: &AttemptFact) -> Result<Ledger, String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut ledger = read_ledger(&tx)?.ok_or("durable accounting has not been initialized")?;
        write_attempt(&tx, attempt, &mut ledger)?;
        save_ledger(&tx, &ledger)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(ledger)
    }

    pub fn settle(&self, fact: &SettlementFact) -> Result<SettlementCommit, String> {
        if fact.schema_version != SETTLEMENT_SCHEMA_VERSION || fact.request.id.is_empty() {
            return Err("unsupported or invalid settlement identity".into());
        }
        if fact
            .attempts
            .iter()
            .any(|attempt| attempt.request_id.as_deref() != Some(&fact.request.id))
        {
            return Err("an attempt does not belong to its settled request".into());
        }
        let safe = fact.safe();
        let json = serde_json::to_string(&safe).map_err(|e| e.to_string())?;
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let before = usage_revision(&tx)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT record FROM request_settlements WHERE id = ?1",
                [&safe.request.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let mut ledger = read_ledger(&tx)?.ok_or("durable accounting has not been initialized")?;
        let inserted = existing.is_none();
        if let Some(existing) = existing {
            if serde_json::from_str::<serde_json::Value>(&existing).map_err(|e| e.to_string())?
                != serde_json::from_str::<serde_json::Value>(&json).map_err(|e| e.to_string())?
            {
                return Err("a request already has a different terminal settlement".into());
            }
        } else {
            for attempt in &safe.attempts {
                if attempt.state == AttemptState::InFlight {
                    return Err("a terminal request still has an in-flight attempt".into());
                }
                write_attempt(&tx, attempt, &mut ledger)?;
            }
            tx.execute(
                "INSERT INTO request_settlements VALUES (?1, ?2, ?3)",
                params![safe.request.id, safe.request.at.to_rfc3339(), json],
            )
            .map_err(|e| e.to_string())?;
            let record = serde_json::to_string(&safe.request).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO usage(id, client, at, record) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET client = excluded.client, at = excluded.at, record = excluded.record",
                params![safe.request.id, safe.request.client.client_id.as_str(), super::usage_timestamp(&safe.request.at), record],
            ).map_err(|e| e.to_string())?;
            save_ledger(&tx, &ledger)?;
        }
        let after = usage_revision(&tx)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(SettlementCommit {
            ledger,
            usage_before: before,
            usage_after: after,
            inserted,
        })
    }

    /// Recovery facts are not fabricated completions or retried remote actions.
    pub fn unresolved_attempts(&self) -> Result<Vec<AttemptFact>, String> {
        let db = crate::sync::lock(&self.db);
        let mut stmt = db
            .prepare("SELECT record FROM attempt_facts WHERE completed = 0 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
    }

    pub fn settlement(&self, id: &str) -> Result<Option<SettlementFact>, String> {
        let db = crate::sync::lock(&self.db);
        let json: Option<String> = db
            .query_row(
                "SELECT record FROM request_settlements WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        json.map(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
            .transpose()
    }
}

fn usage_revision(tx: &Transaction<'_>) -> Result<i64, String> {
    tx.query_row(
        "SELECT revision FROM usage_revision WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

fn read_ledger(tx: &Transaction<'_>) -> Result<Option<Ledger>, String> {
    let json: Option<String> = tx
        .query_row(
            "SELECT ledger FROM accounting_ledger WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    json.map(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
        .transpose()
}

fn save_ledger(tx: &Transaction<'_>, ledger: &Ledger) -> Result<(), String> {
    ledger.validate()?;
    let json = serde_json::to_string(ledger).map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE accounting_ledger SET ledger = ?1, revision = revision + 1 WHERE id = 1",
        [json],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn write_attempt(
    tx: &Transaction<'_>,
    attempt: &AttemptFact,
    ledger: &mut Ledger,
) -> Result<(), String> {
    attempt.validate()?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT record FROM attempt_facts WHERE id = ?1",
            [&attempt.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(existing) = existing {
        let old: AttemptFact = serde_json::from_str(&existing).map_err(|e| e.to_string())?;
        if !old.same_execution(attempt) || (old.state != AttemptState::InFlight && old != *attempt)
        {
            return Err(
                "an accounting attempt cannot change its bound execution or final observation"
                    .into(),
            );
        }
    }
    let json = serde_json::to_string(attempt).map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO attempt_facts VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(id) DO UPDATE SET completed = excluded.completed, record = excluded.record",
        params![attempt.id, attempt.request_id, attempt.state != AttemptState::InFlight, json]).map_err(|e| e.to_string())?;
    if let Some(cost) = &attempt.cost {
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO attempt_charges VALUES (?1, ?2)",
                params![attempt.id, json],
            )
            .map_err(|e| e.to_string())?;
        if inserted != 0 {
            ledger.charge_windows(
                &attempt.budget_day,
                &attempt.budget_month,
                &attempt.execution.provider_id,
                attempt.execution.tier,
                cost,
            );
        }
    }
    Ok(())
}
