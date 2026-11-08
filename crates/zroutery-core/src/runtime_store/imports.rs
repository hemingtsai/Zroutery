//! Durable storage for imported history.
//!
//! Facts are keyed by their source identity — source family, instance and the
//! source's own entity id — so a re-import updates what changed and recognises
//! what did not. Suspected duplicates are stored like any other fact and are
//! excluded from totals by their own column, which keeps the evidence without
//! letting it inflate a number.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, params_from_iter, types::Value, OptionalExtension, TransactionBehavior};

use crate::billing::{Cost, CostTotals};
use crate::history_import::{
    Duplicate, Granularity, ImportCancel, ImportClearScope, ImportOutcome, ImportPreview,
    ImportRange, ImportTotals, ImportedFact,
};
use crate::stats::UsageDay;

use super::{usage_timestamp, RuntimeStore};

/// What one commit changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportCommit {
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub revision: i64,
}

/// A staging import run: facts read from a source but not yet published.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportRun {
    pub id: String,
    pub source: String,
    pub instance: String,
    pub source_version: i64,
    pub range: ImportRange,
    /// How far the source read has durably progressed, in the adapter's own
    /// terms. It moves only when staged data does.
    pub cursor: Option<String>,
    /// Facts staged so far.
    pub staged: u64,
}

impl RuntimeStore {
    /// Store facts in one transaction. Unchanged facts — same key, same content —
    /// are counted rather than rewritten, which is what makes a repeated import
    /// cheap and a resumed one safe.
    pub fn commit_import(&self, facts: &[ImportedFact]) -> Result<ImportCommit, String> {
        if facts.is_empty() {
            return Ok(ImportCommit {
                revision: self.imported_revision()?,
                ..ImportCommit::default()
            });
        }
        for fact in facts {
            fact.validate()?;
        }
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut commit = ImportCommit::default();
        for fact in facts {
            upsert_imported_fact(&tx, fact)?.record(
                &mut commit.inserted,
                &mut commit.updated,
                &mut commit.unchanged,
            );
        }
        commit.revision = tx
            .query_row(
                "SELECT revision FROM imported_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(commit)
    }

    /// Start an isolated scan. An incomplete source scan is never merged with a
    /// later scan of a changed source. Completed previews can be resumed from
    /// `pending_imports` without reopening the source at all.
    pub fn begin_import_run(
        &self,
        run_id: &str,
        source: &str,
        instance: &str,
        source_version: i64,
        range: &ImportRange,
    ) -> Result<ImportRun, String> {
        uuid::Uuid::parse_str(run_id).map_err(|_| "invalid import run id")?;
        if source.trim().is_empty() || instance.trim().is_empty() {
            return Err("an import run needs a source and an instance".into());
        }
        range.validate()?;
        let now = Utc::now().to_rfc3339();
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let revision = read_revision(&tx)?;
        tx.execute(
            "INSERT INTO import_run (id, source, instance, source_version, range_from, range_until,
                                    base_revision, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'staging', ?8, ?8)",
            params![
                run_id,
                source,
                instance,
                source_version,
                range.from.as_ref().map(usage_timestamp),
                range.until.as_ref().map(usage_timestamp),
                revision,
                now
            ],
        )
        .map_err(|error| format!("cannot begin import run (use a new run id): {error}"))?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(ImportRun {
            id: run_id.into(),
            source: source.to_string(),
            instance: instance.to_string(),
            source_version,
            range: range.clone(),
            ..ImportRun::default()
        })
    }

    /// Stage one chunk of facts and advance the cursor in the same
    /// transaction: the cursor only ever moves with durable data, so an
    /// interrupted import never claims progress it would have to re-read.
    ///
    /// Staging is keyed by the source's own entity id, so re-staging a fact —
    /// after a resume, or because the source corrected the row — replaces the
    /// staged copy instead of stacking another one.
    pub fn stage_import(
        &self,
        run_id: &str,
        facts: &[ImportedFact],
        cursor: Option<&str>,
    ) -> Result<u64, String> {
        for fact in facts {
            fact.validate()?;
        }
        let now = Utc::now().to_rfc3339();
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let run = read_run(&tx, run_id, "staging")?;
        for fact in facts {
            validate_run_fact(&run, fact)?;
            let record = serde_json::to_string(fact).map_err(|error| error.to_string())?;
            tx.execute(
                "INSERT INTO import_stage (run_id, entity_id, record) VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id, entity_id) DO UPDATE SET record = excluded.record",
                params![run_id, fact.entity_id, record],
            )
            .map_err(|error| error.to_string())?;
        }
        let staged: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM import_stage WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE import_run SET cursor = COALESCE(?2, cursor), staged = ?3, updated_at = ?4
             WHERE id = ?1",
            params![run_id, cursor, staged, now],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        count(staged).map_err(|error| error.to_string())
    }

    /// Freeze a completed scan. The saved preview and fingerprint refer to the
    /// exact staged metadata the confirmation will publish, even if the source
    /// changes or disappears after this call.
    pub fn ready_import(
        &self,
        run_id: &str,
        mut preview: ImportPreview,
    ) -> Result<ImportPreview, String> {
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let run = read_run(&tx, run_id, "staging")?;
        if preview.source != run.source
            || preview.instance != run.instance
            || preview.source_version != run.source_version
        {
            return Err("preview source does not match the staged run".into());
        }
        let mut hash = 0xcbf2_9ce4_8422_2325;
        let mut rows_count = 0;
        for_each_staged(&tx, &run, |fact, record| {
            fact.validate()?;
            validate_run_fact(&run, fact)?;
            hash_bytes(&mut hash, record.as_bytes());
            hash_bytes(&mut hash, b"\n");
            rows_count += 1;
            Ok(())
        })?;
        if rows_count != preview.facts() || rows_count != run.staged {
            return Err("preview counts do not match its staged facts".into());
        }
        preview.run_id = Some(run_id.into());
        preview.range = run.range;
        preview.fingerprint = Some(format!("v1-{run_id}-{hash:016x}"));
        preview.cancelled = false;
        let record = serde_json::to_string(&preview).map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE import_run SET state = 'ready', fingerprint = ?2, preview = ?3,
                                   updated_at = ?4 WHERE id = ?1 AND state = 'staging'",
            params![run_id, preview.fingerprint, record, Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(preview)
    }

    /// Resume completed previews after a UI reload or process restart. Incomplete
    /// scans require a new snapshot; their last entity is progress, not a safe
    /// source cursor.
    pub fn pending_imports(&self) -> Result<Vec<ImportPreview>, String> {
        let db = crate::sync::lock(&self.db);
        let mut statement = db.prepare(
            "SELECT preview FROM import_run WHERE state = 'ready' ORDER BY created_at DESC, id LIMIT 50"
        ).map_err(|error| error.to_string())?;
        let previews = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .map(|row| {
                serde_json::from_str(&row.map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            })
            .collect();
        previews
    }

    /// Publish every staged fact of a run in one transaction.
    ///
    /// This is the only point where an import becomes visible: the whole run
    /// lands, or — on a validation failure or a crash before this call — none
    /// of it does. An unchanged fact (same key, same content) is recognised
    /// rather than rewritten, so publishing an import the store already holds
    /// advances nothing.
    pub fn publish_import(
        &self,
        run_id: &str,
        fingerprint: &str,
        cancel: &ImportCancel,
    ) -> Result<ImportOutcome, String> {
        let now = Utc::now().to_rfc3339();
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let (state, expected, base_revision, saved): (String, Option<String>, i64, Option<String>) =
            tx.query_row(
                "SELECT state, fingerprint, base_revision, outcome FROM import_run WHERE id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| error.to_string())?;
        if expected.as_deref() != Some(fingerprint) {
            return Err("import preview fingerprint is stale".into());
        }
        if state == "published" {
            return serde_json::from_str(saved.as_deref().ok_or("published import has no report")?)
                .map_err(|error| error.to_string());
        }
        let run = read_run(&tx, run_id, "ready")?;
        if read_revision(&tx)? != base_revision {
            return Err("imported history changed since preview; prepare a new preview".into());
        }
        let mut outcome = ImportOutcome::default();
        let mut hash = 0xcbf2_9ce4_8422_2325;
        for_each_staged(&tx, &run, |fact, record| {
            cancel.check()?;
            hash_bytes(&mut hash, record.as_bytes());
            hash_bytes(&mut hash, b"\n");
            fact.validate()?;
            validate_run_fact(&run, fact)?;
            upsert_imported_fact(&tx, fact)?.record(
                &mut outcome.inserted,
                &mut outcome.updated,
                &mut outcome.unchanged,
            );
            Ok(())
        })?;
        if outcome.facts() != run.staged || fingerprint != format!("v1-{run_id}-{hash:016x}") {
            return Err("staged import changed after preview".into());
        }
        outcome.revision = read_revision(&tx)?;
        tx.execute(
            "DELETE FROM import_stage WHERE run_id = ?1",
            params![run_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE import_run SET state = 'published', cursor = NULL, staged = 0, updated_at = ?2,
                                   outcome = ?3
              WHERE id = ?1",
            params![
                run_id,
                now,
                serde_json::to_string(&outcome).map_err(|error| error.to_string())?
            ],
        )
        .map_err(|error| error.to_string())?;
        cancel.publish(|| tx.commit().map_err(|error| error.to_string()))?;
        Ok(outcome)
    }

    /// Throw away a run's staged facts. A cancelled import calls this before
    /// returning, and nothing it read was ever visible to the history queries.
    pub fn discard_import_run(&self, run_id: &str) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM import_run WHERE id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if state.as_deref() == Some("published") {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM import_stage WHERE run_id = ?1",
            params![run_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE import_run SET state = 'discarded', cursor = NULL, staged = 0, updated_at = ?2
              WHERE id = ?1 AND state IN ('staging', 'ready')",
            params![run_id, now],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(true)
    }

    /// The staging run a source instance currently has, if any: what it has
    /// staged so far and where its cursor stands.
    pub fn active_import_run(
        &self,
        source: &str,
        instance: &str,
    ) -> Result<Option<ImportRun>, String> {
        let db = crate::sync::lock(&self.db);
        let id: Option<String> = db
            .query_row(
                "SELECT id FROM import_run
                  WHERE source = ?1 AND instance = ?2 AND state = 'staging'
                  ORDER BY created_at DESC LIMIT 1",
                params![source, instance],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        id.map(|id| read_run(&db, &id, "staging")).transpose()
    }
    pub fn imported_revision(&self) -> Result<i64, String> {
        crate::sync::lock(&self.db)
            .query_row(
                "SELECT revision FROM imported_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    /// The earliest local day any counted imported fact is filed under.
    ///
    /// The page's period is a claim about what its figures cover, and the
    /// windowed view can only answer for its own window: an import older than
    /// that window would otherwise be described as starting at the window's
    /// edge. Suspected duplicates are excluded, because a row kept out of the
    /// totals must not move the period those totals are reported over.
    pub fn imported_first_day(&self) -> Result<Option<String>, String> {
        crate::sync::lock(&self.db)
            .query_row(
                "SELECT MIN(day) FROM imported_fact WHERE duplicate = 'none'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|error| error.to_string())
    }

    /// Canonical totals over imported facts. Suspected duplicates are reported
    /// separately and never added to the rest. Spend is summed per currency:
    /// the source billed the amount in that currency, and nothing in this
    /// store converts it into another one.
    pub fn imported_totals(
        &self,
        from: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<ImportTotals, String> {
        let from = from.map(|at| usage_timestamp(&at));
        let until = until.map(|at| usage_timestamp(&at));
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|error| error.to_string())?;
        let mut totals = tx
            .query_row(
                "SELECT COALESCE(SUM(requests), 0), COALESCE(SUM(failures), 0),
                        COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cache_read_tokens), 0), COALESCE(SUM(cache_write_tokens), 0),
                        COALESCE(SUM(CASE WHEN priced = 0 THEN requests ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN granularity = 'day' THEN requests ELSE 0 END), 0)
                 FROM imported_fact
                 WHERE duplicate = 'none'
                   AND (?1 IS NULL OR at >= ?1) AND (?2 IS NULL OR at < ?2)",
                params![from, until],
                |row| {
                    Ok(ImportTotals {
                        requests: count(row.get(0)?)?,
                        failures: count(row.get(1)?)?,
                        input_tokens: count(row.get(2)?)?,
                        output_tokens: count(row.get(3)?)?,
                        cache_read_tokens: count(row.get(4)?)?,
                        cache_write_tokens: count(row.get(5)?)?,
                        unpriced_requests: count(row.get(6)?)?,
                        aggregate_requests: count(row.get(7)?)?,
                        suspected_requests: 0,
                        cost: CostTotals::default(),
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        add_imported_cost(&tx, &mut totals.cost, from.as_deref(), until.as_deref())?;
        totals.suspected_requests = tx
            .query_row(
                "SELECT COALESCE(SUM(requests), 0) FROM imported_fact
                 WHERE duplicate = 'suspected'
                   AND (?1 IS NULL OR at >= ?1) AND (?2 IS NULL OR at < ?2)",
                params![from, until],
                |row| count(row.get(0)?),
            )
            .map_err(|error| error.to_string())?;
        Ok(totals)
    }

    /// Imported usage bucketed into local days, for the same charts the local
    /// view uses. Days the source never covered stay out: silence from another
    /// tool is not a measured zero.
    pub fn imported_days(
        &self,
        first: NaiveDate,
        today: NaiveDate,
    ) -> Result<Vec<UsageDay>, String> {
        let mut days: BTreeMap<String, UsageDay> = BTreeMap::new();
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|error| error.to_string())?;
        let mut statement = tx
            .prepare(
                "SELECT day, SUM(requests), SUM(failures), SUM(input_tokens), SUM(output_tokens),
                        SUM(cache_read_tokens), SUM(cache_write_tokens),
                        SUM(CASE WHEN priced = 0 THEN requests ELSE 0 END)
                 FROM imported_fact
                 WHERE duplicate = 'none' AND day >= ?1 AND day <= ?2
                 GROUP BY day ORDER BY day",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![
                    first.format("%Y-%m-%d").to_string(),
                    today.format("%Y-%m-%d").to_string()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        count(row.get(1)?)?,
                        count(row.get(2)?)?,
                        count(row.get(3)?)?,
                        count(row.get(4)?)?,
                        count(row.get(5)?)?,
                        count(row.get(6)?)?,
                        count(row.get(7)?)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (day, requests, failures, input, output, read, write, unpriced) =
                row.map_err(|error| error.to_string())?;
            let entry = UsageDay {
                day,
                requests,
                failures,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: read,
                cache_write_tokens: write,
                unpriced_requests: unpriced,
                ..UsageDay::default()
            };
            days.insert(entry.day.clone(), entry);
        }
        drop(statement);
        // The spend pass runs per currency: a source that bills in CNY keeps
        // its CNY day figure, and no chart silently adds it to a USD total.
        let mut statement = tx
            .prepare(
                "SELECT day, cost_currency, SUM(cost_amount) FROM imported_fact
                 WHERE duplicate = 'none' AND day >= ?1 AND day <= ?2
                   AND cost_currency IS NOT NULL
                 GROUP BY day, cost_currency",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![
                    first.format("%Y-%m-%d").to_string(),
                    today.format("%Y-%m-%d").to_string()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (day, currency, amount) = row.map_err(|error| error.to_string())?;
            if let Some(entry) = days.get_mut(&day) {
                add_cost(&mut entry.cost, currency, amount)?;
            }
        }
        // A day with no row is absent rather than zero: the source did not say
        // anything about it.
        Ok(days.into_values().collect())
    }

    pub fn query_imported_history(
        &self,
        query: &crate::history_import::ImportedHistoryQuery,
    ) -> Result<crate::history_import::ImportedHistoryPage, String> {
        query.validate()?;
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|error| error.to_string())?;
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM imported_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        query.check_revision(revision)?;
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        for (column, value) in [
            ("source", query.source.as_ref()),
            ("instance", query.instance.as_ref()),
            ("client", query.client.as_ref()),
            ("provider_id", query.provider_id.as_ref()),
            ("model", query.model.as_ref()),
        ] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                values.push(value.clone().into());
            }
        }
        if let Some(granularity) = query.granularity {
            clauses.push("granularity = ?".into());
            values.push(granularity_name(granularity).to_string().into());
        }
        if let Some(duplicate) = query.duplicate {
            clauses.push("duplicate = ?".into());
            values.push(duplicate_name(duplicate).to_string().into());
        }
        if let Some(priced) = query.priced {
            clauses.push("priced = ?".into());
            values.push(if priced { 1i64 } else { 0i64 }.into());
        }
        if let Some(currency) = query.currency.as_ref().filter(|s| !s.is_empty()) {
            clauses.push("cost_currency = ?".into());
            values.push(currency.clone().into());
        }
        if let Some(data_source) = query.data_source.as_ref().filter(|s| !s.is_empty()) {
            clauses.push("json_extract(record, '$.data_source') = ?".into());
            values.push(data_source.clone().into());
        }
        if let Some(from) = &query.from {
            clauses.push("at >= ?".into());
            values.push(usage_timestamp(from).into());
        }
        if let Some(until) = &query.until {
            clauses.push("at < ?".into());
            values.push(usage_timestamp(until).into());
        }
        let condition = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let total: i64 = tx
            .query_row(
                &format!("SELECT COUNT(*) FROM imported_fact{condition}"),
                params_from_iter(&values),
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let total = u64::try_from(total).map_err(|_| "invalid negative imported count")?;
        let (page, page_size, offset) = query.pagination(total);
        values.push(i64::from(page_size).into());
        values.push(
            i64::try_from(offset)
                .map_err(|_| "imported history page offset exceeds SQLite's range")?
                .into(),
        );
        let mut statement = tx
            .prepare(&format!(
                "SELECT record FROM imported_fact{condition}
                 ORDER BY at DESC, source, instance, entity_id DESC LIMIT ? OFFSET ?"
            ))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params_from_iter(&values), |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let records = rows
            .map(|row| {
                let json = row.map_err(|error| error.to_string())?;
                serde_json::from_str(&json).map_err(|error| error.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(crate::history_import::ImportedHistoryPage {
            records,
            page,
            page_size,
            total,
            revision,
        })
    }

    /// Return imported facts matching `query` without pagination, for export.
    pub fn export_imported_facts(
        &self,
        query: &crate::history_import::ImportedHistoryQuery,
    ) -> Result<(i64, Vec<ImportedFact>), String> {
        let mut export = query.clone();
        export.page = 1;
        export.page_size = u32::MAX;
        export.expected_revision = None;
        export.validate()?;
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|error| error.to_string())?;
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM imported_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        for (column, value) in [
            ("source", export.source.as_ref()),
            ("instance", export.instance.as_ref()),
            ("client", export.client.as_ref()),
            ("provider_id", export.provider_id.as_ref()),
            ("model", export.model.as_ref()),
        ] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                values.push(value.clone().into());
            }
        }
        if let Some(granularity) = export.granularity {
            clauses.push("granularity = ?".into());
            values.push(granularity_name(granularity).to_string().into());
        }
        if let Some(duplicate) = export.duplicate {
            clauses.push("duplicate = ?".into());
            values.push(duplicate_name(duplicate).to_string().into());
        }
        if let Some(priced) = export.priced {
            clauses.push("priced = ?".into());
            values.push(if priced { 1i64 } else { 0i64 }.into());
        }
        if let Some(currency) = export.currency.as_ref().filter(|s| !s.is_empty()) {
            clauses.push("cost_currency = ?".into());
            values.push(currency.clone().into());
        }
        if let Some(data_source) = export.data_source.as_ref().filter(|s| !s.is_empty()) {
            clauses.push("json_extract(record, '$.data_source') = ?".into());
            values.push(data_source.clone().into());
        }
        if let Some(from) = &export.from {
            clauses.push("at >= ?".into());
            values.push(usage_timestamp(from).into());
        }
        if let Some(until) = &export.until {
            clauses.push("at < ?".into());
            values.push(usage_timestamp(until).into());
        }
        let condition = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let mut statement = tx
            .prepare(&format!(
                "SELECT record FROM imported_fact{condition}
                 ORDER BY at, source, instance, entity_id"
            ))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params_from_iter(&values), |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let records = rows
            .map(|row| {
                let json = row.map_err(|error| error.to_string())?;
                serde_json::from_str(&json).map_err(|error| error.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok((revision, records))
    }

    /// Remove imported facts, optionally only one source family's. A clear is
    /// a history change, so it also retires any staging run that source still
    /// had: facts staged for a cleared source must not be publishable after
    /// the clear, as if it had not happened.
    pub fn clear_imported(&self, source: Option<&str>) -> Result<usize, String> {
        self.clear_imported_scoped(&ImportClearScope {
            source: source.map(str::to_string),
            ..ImportClearScope::default()
        })
    }

    /// Remove imported facts matching `scope`. Local usage, spend, wallet and
    /// health are never affected. Staging runs are discarded only when the scope
    /// can be expressed as source/instance (client-only scopes cannot safely
    /// invalidate staged content by run metadata alone).
    pub fn clear_imported_scoped(&self, scope: &ImportClearScope) -> Result<usize, String> {
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        let mut idx = 1usize;
        if let Some(source) = scope.source.as_ref().filter(|s| !s.is_empty()) {
            clauses.push(format!("source = ?{idx}"));
            values.push(source.clone().into());
            idx += 1;
        }
        if let Some(instance) = scope.instance.as_ref().filter(|s| !s.is_empty()) {
            clauses.push(format!("instance = ?{idx}"));
            values.push(instance.clone().into());
            idx += 1;
        }
        if let Some(client) = scope.client.as_ref().filter(|s| !s.is_empty()) {
            clauses.push(format!("client = ?{idx}"));
            values.push(client.clone().into());
        }
        let condition = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let mut db = crate::sync::lock(&self.db);
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let removed = tx
            .execute(
                &format!("DELETE FROM imported_fact{condition}"),
                params_from_iter(&values),
            )
            .map_err(|error| error.to_string())?;
        // Only discard staging runs when scope is source/instance-addressable.
        // A client-only clear does not prove which staged run records belong to
        // that client, so staged runs are left untouched.
        if scope.client.as_ref().is_none_or(|s| s.is_empty()) {
            let mut run_clauses = vec!["state IN ('staging', 'ready')".to_string()];
            let mut run_values: Vec<Value> = Vec::new();
            let mut ridx = 1usize;
            if let Some(source) = scope.source.as_ref().filter(|s| !s.is_empty()) {
                run_clauses.push(format!("source = ?{ridx}"));
                run_values.push(source.clone().into());
                ridx += 1;
            }
            if let Some(instance) = scope.instance.as_ref().filter(|s| !s.is_empty()) {
                run_clauses.push(format!("instance = ?{ridx}"));
                run_values.push(instance.clone().into());
                ridx += 1;
            }
            let run_condition = format!(" WHERE {}", run_clauses.join(" AND "));
            tx.execute(
                &format!(
                    "DELETE FROM import_stage WHERE run_id IN (SELECT id FROM import_run{run_condition})"
                ),
                params_from_iter(run_values.clone()),
            )
            .map_err(|error| error.to_string())?;
            let mut update_values = run_values;
            let ts_idx = ridx;
            update_values.push(Utc::now().to_rfc3339().into());
            tx.execute(
                &format!(
                    "UPDATE import_run SET state = 'discarded', cursor = NULL, staged = 0, updated_at = ?{ts_idx}
                     {run_condition}"
                ),
                params_from_iter(update_values),
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(removed)
    }
}

fn count(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

fn read_revision(db: &rusqlite::Connection) -> Result<i64, String> {
    db.query_row(
        "SELECT revision FROM imported_revision WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

fn read_run(db: &rusqlite::Connection, run_id: &str, state: &str) -> Result<ImportRun, String> {
    let (mut run, from, until) = db
        .query_row(
            "SELECT source, instance, source_version, range_from, range_until, cursor, staged
         FROM import_run WHERE id = ?1 AND state = ?2",
            params![run_id, state],
            |row| {
                Ok((
                    ImportRun {
                        id: run_id.into(),
                        source: row.get(0)?,
                        instance: row.get(1)?,
                        source_version: row.get(2)?,
                        cursor: row.get(5)?,
                        staged: count(row.get(6)?)?,
                        ..ImportRun::default()
                    },
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(|error| format!("import run is not {state}: {error}"))?;
    let parse = |value: Option<String>| {
        value
            .map(|value| {
                value
                    .parse::<DateTime<Utc>>()
                    .map_err(|error| error.to_string())
            })
            .transpose()
    };
    run.range = ImportRange::new(parse(from)?, parse(until)?)?;
    Ok(run)
}

fn validate_run_fact(run: &ImportRun, fact: &ImportedFact) -> Result<(), String> {
    if fact.source != run.source || fact.instance != run.instance {
        return Err("fact source does not match the staged import".into());
    }
    if !run.range.contains_fact(fact) {
        return Err("fact lies outside the preview's import range".into());
    }
    Ok(())
}

/// Bounded memory even for a multi-million-row publication. All pages share the
/// same write transaction, so no reader can observe a partially published batch.
fn for_each_staged(
    tx: &rusqlite::Transaction,
    run: &ImportRun,
    mut visit: impl FnMut(&ImportedFact, &str) -> Result<(), String>,
) -> Result<(), String> {
    let mut after: Option<String> = None;
    loop {
        let records = {
            let mut statement = tx
                .prepare(
                    "SELECT entity_id, record FROM import_stage
                 WHERE run_id = ?1 AND (?2 IS NULL OR entity_id > ?2)
                 ORDER BY entity_id LIMIT 256",
                )
                .map_err(|error| error.to_string())?;
            let records = statement
                .query_map(params![run.id, after], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            records
        };
        if records.is_empty() {
            break;
        }
        after = records.last().map(|(id, _)| id.clone());
        for (id, record) in records {
            let fact: ImportedFact =
                serde_json::from_str(&record).map_err(|error| error.to_string())?;
            if id != fact.entity_id {
                return Err("staged fact identity mismatch".into());
            }
            visit(&fact, &record)?;
        }
    }
    Ok(())
}

fn add_cost(totals: &mut CostTotals, currency: String, amount: f64) -> Result<(), String> {
    if currency.trim().is_empty() || !amount.is_finite() || amount < 0.0 {
        return Err("invalid imported currency total".into());
    }
    totals.add(&Cost { currency, amount });
    Ok(())
}

/// Add a window's imported spend to per-currency totals.
///
/// One query groups by the currency the source billed in; the amounts stay in
/// it. A currency the fact carried is a source measurement, not a conversion
/// target.
fn add_imported_cost(
    db: &rusqlite::Connection,
    totals: &mut CostTotals,
    from: Option<&str>,
    until: Option<&str>,
) -> Result<(), String> {
    let mut statement = db
        .prepare(
            "SELECT cost_currency, SUM(cost_amount) FROM imported_fact
             WHERE duplicate = 'none' AND cost_currency IS NOT NULL
               AND (?1 IS NULL OR at >= ?1) AND (?2 IS NULL OR at < ?2)
             GROUP BY cost_currency",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![from, until], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (currency, amount) = row.map_err(|error| error.to_string())?;
        add_cost(totals, currency, amount)?;
    }
    Ok(())
}

/// Insert or refresh one fact inside an open transaction, returning what it did.
///
/// Shared by the direct commit path and by staging publish, so the two cannot
/// drift: an unchanged fact is recognised by content revision, and the spend
/// columns carry the amount with the currency it was billed in — never a
/// conversion the store did not perform.
fn upsert_imported_fact(tx: &rusqlite::Transaction, fact: &ImportedFact) -> Result<Upsert, String> {
    let record = serde_json::to_string(fact).map_err(|error| error.to_string())?;
    let revision = digest(record.as_bytes());
    let existing: Option<String> = tx
        .query_row(
            "SELECT revision FROM imported_fact
             WHERE source = ?1 AND instance = ?2 AND entity_id = ?3",
            params![fact.source, fact.instance, fact.entity_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if existing.as_deref() == Some(revision.as_str()) {
        return Ok(Upsert::Unchanged);
    }
    let cost = fact.cost.as_ref();
    let (cost_currency, cost_amount) = (
        cost.map(|cost| cost.currency.as_str()),
        cost.map(|cost| cost.amount),
    );
    tx.execute(
        "INSERT INTO imported_fact (
            source, instance, entity_id, revision, at, day, granularity,
            duplicate, duplicate_of, client, provider_id, model,
            requests, failures, input_tokens, output_tokens,
            cache_read_tokens, cache_write_tokens, cost_currency, cost_amount,
            priced, record
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
         ON CONFLICT(source, instance, entity_id) DO UPDATE SET
            revision = excluded.revision, at = excluded.at, day = excluded.day,
            granularity = excluded.granularity, duplicate = excluded.duplicate,
            duplicate_of = excluded.duplicate_of, client = excluded.client,
            provider_id = excluded.provider_id, model = excluded.model,
            requests = excluded.requests, failures = excluded.failures,
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cache_write_tokens = excluded.cache_write_tokens,
            cost_currency = excluded.cost_currency,
            cost_amount = excluded.cost_amount,
            priced = excluded.priced, record = excluded.record",
        params![
            fact.source,
            fact.instance,
            fact.entity_id,
            revision,
            usage_timestamp(&fact.at),
            fact.day,
            granularity_name(fact.granularity),
            duplicate_name(fact.duplicate),
            fact.duplicate_of,
            fact.client,
            fact.provider_id,
            fact.model,
            i64::try_from(fact.requests).map_err(|_| "imported request count is too large")?,
            i64::try_from(fact.failures).map_err(|_| "imported failure count is too large")?,
            i64::from(fact.tokens.input_tokens),
            i64::from(fact.tokens.output_tokens),
            i64::from(fact.tokens.cache_read_tokens),
            i64::from(fact.tokens.cache_write_tokens),
            cost_currency,
            cost_amount,
            if fact.priced { 1i64 } else { 0i64 },
            record,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(if existing.is_some() {
        Upsert::Updated
    } else {
        Upsert::Inserted
    })
}

/// What storing one fact did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Upsert {
    Inserted,
    Updated,
    Unchanged,
}

impl Upsert {
    fn record(self, inserted: &mut u64, updated: &mut u64, unchanged: &mut u64) {
        match self {
            Upsert::Inserted => *inserted += 1,
            Upsert::Updated => *updated += 1,
            Upsert::Unchanged => *unchanged += 1,
        }
    }
}

fn granularity_name(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Request => "request",
        Granularity::Day => "day",
    }
}

fn duplicate_name(duplicate: Duplicate) -> &'static str {
    match duplicate {
        Duplicate::None => "none",
        Duplicate::Suspected => "suspected",
    }
}

/// FNV-1a over the serialised fact. A change detector, not a security boundary.
fn digest(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    hash_bytes(&mut hash, bytes);
    format!("{hash:016x}")
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_import::{Duplicate, ImportedFact, InputSemantics};
    use crate::ir::Usage;

    fn fact(entity: &str, amount: f64) -> ImportedFact {
        ImportedFact {
            source: "ccswitch".into(),
            instance: "laptop".into(),
            entity_id: entity.into(),
            revision: String::new(),
            granularity: Granularity::Request,
            at: "2026-01-02T10:00:00Z".parse().unwrap(),
            day: "2026-01-02".into(),
            client: Some("claude".into()),
            provider_id: Some("provider".into()),
            model: Some("claude-sonnet-4-5".into()),
            request_model: None,
            pricing_model: None,
            tokens: Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_tokens: 30,
                cache_write_tokens: 10,
                reasoning_tokens: 0,
            },
            input_semantics: Some(InputSemantics::Fresh),
            cost: Some(Cost {
                currency: "USD".into(),
                amount,
            }),
            reported_cost: Some(Cost {
                currency: "USD".into(),
                amount,
            }),
            priced: true,
            latency_ms: Some(40),
            ttft_ms: None,
            status: Some(200),
            ok: Some(true),
            streaming: false,
            requests: 1,
            failures: 0,
            data_source: "proxy".into(),
            duplicate: Duplicate::None,
            duplicate_of: None,
            multiplier: None,
            error: None,
        }
    }

    fn store() -> (tempfile::TempDir, RuntimeStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = RuntimeStore::open(&dir.path().join("runtime.db")).unwrap();
        (dir, store)
    }

    fn begin(store: &RuntimeStore) -> ImportRun {
        store
            .begin_import_run(
                &uuid::Uuid::new_v4().to_string(),
                "ccswitch",
                "laptop",
                21,
                &ImportRange::default(),
            )
            .unwrap()
    }

    fn ready(store: &RuntimeStore, run: &ImportRun, requests: u64) -> ImportPreview {
        store
            .ready_import(
                &run.id,
                ImportPreview {
                    source: run.source.clone(),
                    instance: run.instance.clone(),
                    source_version: run.source_version,
                    requests,
                    ..ImportPreview::default()
                },
            )
            .unwrap()
    }

    fn publish(store: &RuntimeStore, preview: &ImportPreview) -> ImportOutcome {
        store
            .publish_import(
                preview.run_id.as_deref().unwrap(),
                preview.fingerprint.as_deref().unwrap(),
                &ImportCancel::new(),
            )
            .unwrap()
    }

    #[test]
    fn an_unchanged_fact_is_recognised_instead_of_rewritten() {
        let (_dir, store) = store();
        let facts = vec![fact("a", 0.25), fact("b", 0.10)];

        let first = store.commit_import(&facts).unwrap();
        assert_eq!(first.inserted, 2);
        assert_eq!(first.updated, 0);
        assert_eq!(first.revision, 2, "one revision per stored row");

        let second = store.commit_import(&facts).unwrap();
        assert_eq!(second.unchanged, 2);
        assert_eq!(second.inserted, 0);
        assert_eq!(
            second.revision, first.revision,
            "an unchanged fact does not advance the revision"
        );

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 2);
        assert_eq!(totals.cost.get("USD"), 0.35);
    }

    #[test]
    fn the_first_imported_day_is_the_whole_store_not_the_window() {
        let (_dir, store) = store();
        assert_eq!(
            store.imported_first_day().unwrap(),
            None,
            "nothing imported has no period to name"
        );

        let mut older = fact("older", 0.1);
        older.day = "2025-01-31".into();
        older.at = "2025-01-31T10:00:00Z".parse().unwrap();
        let mut newest = fact("newest", 0.1);
        newest.day = "2026-01-02".into();
        newest.at = "2026-01-02T10:00:00Z".parse().unwrap();
        // A suspected duplicate is shown but not counted, so it does not get to
        // claim the period the counted figures are reported over.
        let mut suspected = fact("suspected", 0.1);
        suspected.day = "2024-06-01".into();
        suspected.at = "2024-06-01T10:00:00Z".parse().unwrap();
        suspected.duplicate = Duplicate::Suspected;
        suspected.duplicate_of = Some("older".into());
        store.commit_import(&[newest, older, suspected]).unwrap();

        assert_eq!(
            store.imported_first_day().unwrap().as_deref(),
            Some("2025-01-31")
        );
    }

    #[test]
    fn a_changed_fact_is_updated_and_advances_the_revision() {
        let (_dir, store) = store();
        let original = fact("a", 0.25);
        let first = store
            .commit_import(std::slice::from_ref(&original))
            .unwrap();
        assert_eq!(first.inserted, 1);

        // The source corrected the call after the fact: a late completion, a
        // re-priced row, or a token recount. The key stays, the content moves.
        let mut corrected = original;
        corrected.tokens.output_tokens = 99;
        corrected.cost = None;
        corrected.priced = false;
        let second = store.commit_import(&[corrected]).unwrap();
        assert_eq!(second.updated, 1);
        assert_eq!(second.inserted, 0);
        assert_eq!(second.unchanged, 0);
        assert!(second.revision > first.revision);

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(
            totals.requests, 1,
            "an update does not add a second request"
        );
        assert_eq!(totals.output_tokens, 99);
        assert_eq!(
            totals.unpriced_requests, 1,
            "the correction removed its price"
        );
        assert_eq!(totals.cost.get("USD"), 0.0);
    }

    #[test]
    fn an_impossible_fact_is_refused_before_anything_is_written() {
        let (_dir, store) = store();
        let mut bad = fact("a", 0.25);
        bad.failures = 2;
        let error = store
            .commit_import(&[fact("b", 0.10), bad])
            .expect_err("a fact with more failures than requests is refused");
        assert!(error.contains("failures"), "{error}");
        assert_eq!(
            store.imported_totals(None, None).unwrap().requests,
            0,
            "validation happens before the transaction, so nothing was stored"
        );

        let mut unpriced = fact("c", 0.0);
        unpriced.cost = Some(Cost {
            currency: "   ".into(),
            amount: 0.0,
        });
        assert!(store.commit_import(&[unpriced]).is_err());
    }

    #[test]
    fn a_suspected_duplicate_is_kept_but_not_counted() {
        let (_dir, store) = store();
        let mut twin = fact("session:a", 0.75);
        twin.duplicate = Duplicate::Suspected;
        twin.duplicate_of = Some("proxy:a".into());
        // Without the twin, a suspected row is evidence with no referent.
        assert!(store.commit_import(&[twin.clone()]).is_ok());

        store.commit_import(&[fact("proxy:a", 0.25)]).unwrap();
        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 1, "only the canonical row is counted");
        assert_eq!(totals.suspected_requests, 1);
        assert_eq!(totals.cost.get("USD"), 0.25, "0.75 never reaches the total");

        let mut orphan = fact("session:b", 0.5);
        orphan.duplicate = Duplicate::Suspected;
        assert!(
            store.commit_import(&[orphan]).is_err(),
            "a suspected duplicate names the row it duplicates"
        );
    }

    #[test]
    fn a_day_total_is_told_apart_from_request_detail() {
        let (_dir, store) = store();
        let mut day = fact("rollup:2026-01-02", 0.5);
        day.granularity = Granularity::Day;
        day.requests = 5;
        day.failures = 1;
        store.commit_import(&[day, fact("a", 0.25)]).unwrap();

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 6);
        assert_eq!(totals.aggregate_requests, 5, "how much came from a total");
        assert_eq!(totals.failures, 1);

        let days = store
            .imported_days("2026-01-01".parse().unwrap(), "2026-01-02".parse().unwrap())
            .unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].day, "2026-01-02");
        assert_eq!(days[0].requests, 6);
        assert_eq!(days[0].cost.get("USD"), 0.75);
    }

    #[test]
    fn a_day_the_source_never_covered_is_absent_rather_than_zero() {
        let (_dir, store) = store();
        store.commit_import(&[fact("a", 0.25)]).unwrap();
        let days = store
            .imported_days("2026-01-01".parse().unwrap(), "2026-01-03".parse().unwrap())
            .unwrap();
        assert_eq!(days.len(), 1, "only the day with a fact is returned");
        assert_eq!(days[0].day, "2026-01-02");
    }

    #[test]
    fn clearing_imported_history_advances_the_revision_and_empties_the_totals() {
        let (_dir, store) = store();
        let mut other = fact("a", 0.25);
        other.source = "claudecode".into();
        store
            .commit_import(&[fact("b", 0.10), other.clone()])
            .unwrap();
        let before = store.imported_revision().unwrap();

        let removed = store.clear_imported(Some("claudecode")).unwrap();
        assert_eq!(removed, 1);
        let after = store.imported_revision().unwrap();
        assert!(after > before, "a clear is a history change too");
        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 1, "the other source survived");
        assert_eq!(totals.cost.get("USD"), 0.10);

        assert_eq!(store.clear_imported(None).unwrap(), 1);
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
        assert!(store
            .imported_days("2025-01-01".parse().unwrap(), "2027-01-01".parse().unwrap())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn two_instances_of_the_same_source_do_not_overwrite_each_other() {
        let (_dir, store) = store();
        let mut laptop = fact("a", 0.25);
        let mut desktop = laptop.clone();
        desktop.instance = "desktop".into();
        desktop.tokens.output_tokens = 40;
        laptop.tokens.output_tokens = 20;
        store.commit_import(&[laptop, desktop]).unwrap();

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 2, "same entity id, different copies");
        assert_eq!(totals.output_tokens, 60);
    }

    #[test]
    fn a_schema_five_database_backfills_its_currencies_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.db");
        {
            // A v5-shaped store: the imported_fact table as schema 5 declared
            // it, with a USD amount in `cost_usd` and a CNY amount that only
            // survives inside the fact record — exactly what a v5 file holds
            // after a CNY-billing source was imported into it.
            let mut db = rusqlite::Connection::open(&path).unwrap();
            super::super::schema::migrate_to(&mut db, 5).unwrap();
            let at = "2026-01-02T10:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap();
            let stamp = at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            for (entity, usd, record) in [
                (
                    "usd-row",
                    Some(0.25),
                    r#"{"cost":{"currency":"USD","amount":0.25}}"#,
                ),
                (
                    "cny-row",
                    None,
                    r#"{"cost":{"currency":"CNY","amount":2.5}}"#,
                ),
            ] {
                db.execute(
                    "INSERT INTO imported_fact (source, instance, entity_id, revision, at, day,
                        granularity, duplicate, requests, failures, input_tokens, output_tokens,
                        cache_read_tokens, cache_write_tokens, cost_usd, priced, record)
                     VALUES ('ccswitch', 'laptop', ?1, 'r', ?2, '2026-01-02', 'request', 'none',
                             1, 0, 10, 4, 0, 0, ?3, 1, ?4)",
                    rusqlite::params![entity, stamp, usd, record],
                )
                .unwrap();
            }
        }

        let store = RuntimeStore::open(&path).expect("opening migrates a complete schema 5");
        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.cost.get("USD"), 0.25, "the USD amount survived");
        assert_eq!(
            totals.cost.get("CNY"),
            2.5,
            "the CNY amount left its record and joined the totals"
        );
        let column: i64 = crate::sync::lock(&store.db)
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('imported_fact') WHERE name = 'cost_usd'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(column, 0, "the USD-only column is gone, not stale");
    }

    #[test]
    fn staging_hides_every_fact_until_the_run_is_published() {
        let (_dir, store) = store();
        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("a", 0.25), fact("b", 0.10)], Some("b"))
            .unwrap();

        assert_eq!(
            store.imported_totals(None, None).unwrap().requests,
            0,
            "staged facts are not history yet"
        );
        let progress = store
            .active_import_run("ccswitch", "laptop")
            .unwrap()
            .unwrap();
        assert_eq!(progress.id, run.id);
        assert_eq!(progress.staged, 2);
        assert_eq!(progress.cursor.as_deref(), Some("b"));
        let preview = ready(&store, &run, 2);
        assert_eq!(
            store.pending_imports().unwrap()[0].fingerprint,
            preview.fingerprint
        );
        let outcome = publish(&store, &preview);
        assert_eq!(outcome.inserted, 2);
        assert!(!outcome.cancelled);
        assert_eq!(
            store.imported_totals(None, None).unwrap().requests,
            2,
            "publish is the only point where the facts become history"
        );
        assert!(
            store
                .active_import_run("ccswitch", "laptop")
                .unwrap()
                .is_none(),
            "a published run no longer stages"
        );
    }

    #[test]
    fn a_cancelled_import_publishes_nothing() {
        let (_dir, store) = store();
        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("a", 0.25)], None)
            .unwrap();

        let outcome = publish(&store, &ready(&store, &run, 1));
        assert_eq!(outcome.inserted, 1);

        let second = begin(&store);
        store
            .stage_import(&second.id, &[fact("b", 0.10)], None)
            .unwrap();
        store.discard_import_run(&second.id).unwrap();
        assert_eq!(
            store.imported_totals(None, None).unwrap().requests,
            1,
            "the discarded run's facts never became history"
        );
        assert!(
            store
                .active_import_run("ccswitch", "laptop")
                .unwrap()
                .is_none(),
            "a discarded run is finished"
        );
    }

    #[test]
    fn re_staging_a_staged_entity_replaces_it_instead_of_stacking() {
        let (_dir, store) = store();
        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("a", 0.25)], None)
            .unwrap();
        let mut corrected = fact("a", 0.25);
        corrected.tokens.output_tokens = 99;
        store
            .stage_import(&run.id, &[corrected], Some("a"))
            .unwrap();
        assert_eq!(
            store
                .active_import_run("ccswitch", "laptop")
                .unwrap()
                .unwrap()
                .staged,
            1
        );

        let outcome = publish(&store, &ready(&store, &run, 1));
        assert_eq!(outcome.inserted, 1, "one key, one row");
        assert_eq!(store.imported_totals(None, None).unwrap().output_tokens, 99);
    }

    #[test]
    fn spend_is_totalled_per_currency_not_converted() {
        let (_dir, store) = store();
        let mut cny = fact("cny", 1.0);
        cny.cost = Some(Cost {
            currency: "CNY".into(),
            amount: 2.5,
        });
        store.commit_import(&[fact("usd", 0.25), cny]).unwrap();

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.cost.get("USD"), 0.25);
        assert_eq!(totals.cost.get("CNY"), 2.5, "a CNY figure stays CNY");

        let days = store
            .imported_days("2026-01-01".parse().unwrap(), "2026-01-03".parse().unwrap())
            .unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].cost.get("USD"), 0.25);
        assert_eq!(days[0].cost.get("CNY"), 2.5);
    }

    #[test]
    fn clearing_a_source_retires_its_staging_run() {
        let (_dir, store) = store();
        let mut other = fact("a", 0.25);
        other.source = "claudecode".into();
        store.commit_import(&[other.clone()]).unwrap();

        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("b", 0.10)], None)
            .unwrap();
        store.clear_imported(Some("ccswitch")).unwrap();
        assert!(
            store
                .active_import_run("ccswitch", "laptop")
                .unwrap()
                .is_none(),
            "a cleared source has nothing left to publish"
        );

        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("c", 0.30)], None)
            .unwrap();
        let preview = ready(&store, &run, 1);
        store.clear_imported(None).unwrap();
        assert!(store
            .active_import_run("ccswitch", "laptop")
            .unwrap()
            .is_none());
        assert!(
            store
                .publish_import(
                    &run.id,
                    preview.fingerprint.as_deref().unwrap(),
                    &ImportCancel::new()
                )
                .is_err(),
            "clearing revokes the earlier confirmation"
        );
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
    }

    #[test]
    fn a_ready_snapshot_survives_reopen_and_confirmation_is_idempotent() {
        let (dir, store) = store();
        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("a", 0.25)], Some("a"))
            .unwrap();
        let preview = ready(&store, &run, 1);
        drop(store);
        let reopened = RuntimeStore::open(&dir.path().join("runtime.db")).unwrap();
        let saved = reopened.pending_imports().unwrap();
        assert_eq!(saved[0].fingerprint, preview.fingerprint);
        let first = publish(&reopened, &saved[0]);
        assert_eq!(publish(&reopened, &preview), first);
        assert_eq!(reopened.imported_totals(None, None).unwrap().requests, 1);
        assert!(reopened.pending_imports().unwrap().is_empty());
    }

    #[test]
    fn source_range_state_and_history_revision_are_enforced() {
        let (_dir, store) = store();
        let range = ImportRange::new(
            Some("2026-01-02T00:00:00Z".parse().unwrap()),
            Some("2026-01-03T00:00:00Z".parse().unwrap()),
        )
        .unwrap();
        let run = store
            .begin_import_run(
                &uuid::Uuid::new_v4().to_string(),
                "ccswitch",
                "laptop",
                21,
                &range,
            )
            .unwrap();
        assert_eq!(
            store
                .active_import_run("ccswitch", "laptop")
                .unwrap()
                .unwrap()
                .range,
            range
        );
        let mut wrong = fact("wrong", 0.1);
        wrong.instance = "another-machine".into();
        assert!(store.stage_import(&run.id, &[wrong], None).is_err());
        let mut outside = fact("outside", 0.1);
        outside.at = range.until.unwrap();
        assert!(store.stage_import(&run.id, &[outside], None).is_err());
        assert!(store
            .stage_import("missing", &[fact("missing", 0.1)], None)
            .is_err());
        store
            .stage_import(&run.id, &[fact("a", 0.1)], None)
            .unwrap();
        let preview = ready(&store, &run, 1);
        assert!(store
            .stage_import(&run.id, &[fact("late", 0.1)], None)
            .is_err());
        assert!(store
            .publish_import(&run.id, "wrong-fingerprint", &ImportCancel::new())
            .is_err());
        store.commit_import(&[fact("other-writer", 0.2)]).unwrap();
        assert!(store
            .publish_import(
                &run.id,
                preview.fingerprint.as_deref().unwrap(),
                &ImportCancel::new()
            )
            .unwrap_err()
            .contains("changed since preview"));
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 1);
    }

    #[test]
    fn publication_failure_rolls_back_every_page_and_can_retry() {
        let (_dir, store) = store();
        let run = begin(&store);
        let facts: Vec<_> = (0..600)
            .map(|i| fact(&format!("row-{i:04}"), 0.001))
            .collect();
        for chunk in facts.chunks(100) {
            store.stage_import(&run.id, chunk, None).unwrap();
        }
        let preview = ready(&store, &run, 600);
        crate::sync::lock(&store.db).execute_batch(
            "CREATE TRIGGER fail_import BEFORE INSERT ON imported_fact
             WHEN NEW.entity_id = 'row-0300' BEGIN SELECT RAISE(ABORT, 'fixture disk failure'); END;"
        ).unwrap();
        assert!(store
            .publish_import(
                &run.id,
                preview.fingerprint.as_deref().unwrap(),
                &ImportCancel::new()
            )
            .is_err());
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
        assert_eq!(store.imported_revision().unwrap(), 0);
        assert_eq!(store.pending_imports().unwrap().len(), 1);
        crate::sync::lock(&store.db)
            .execute_batch("DROP TRIGGER fail_import")
            .unwrap();
        assert_eq!(publish(&store, &preview).inserted, 600);
    }

    #[test]
    fn cancellation_before_publication_keeps_the_revision_unchanged() {
        let (_dir, store) = store();
        let run = begin(&store);
        store
            .stage_import(&run.id, &[fact("a", 0.1)], None)
            .unwrap();
        let preview = ready(&store, &run, 1);
        let cancel = ImportCancel::new();
        assert!(cancel.cancel());
        assert!(store
            .publish_import(&run.id, preview.fingerprint.as_deref().unwrap(), &cancel)
            .is_err());
        assert_eq!(store.imported_revision().unwrap(), 0);
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
        assert!(store.discard_import_run(&run.id).unwrap());
    }

    #[test]
    fn a_failed_migration_rolls_back_schema_and_rejects_old_writers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        super::super::schema::migrate_to(&mut db, 5).unwrap();
        db.execute(
            "INSERT INTO imported_fact VALUES
           ('ccswitch','laptop','bad','r','2026-01-02T00:00:00Z','2026-01-02','request','none',NULL,
            NULL,NULL,NULL,1,0,0,0,0,0,NULL,0,'{bad-json')",
            [],
        )
        .unwrap();
        assert!(super::super::schema::migrate(&mut db).is_err());
        let version: i64 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let added: i64 = db.query_row("SELECT COUNT(*) FROM pragma_table_info('imported_fact') WHERE name = 'cost_currency'", [], |row| row.get(0)).unwrap();
        assert_eq!(added, 0);
        db.execute("DELETE FROM imported_fact", []).unwrap();
        super::super::schema::migrate(&mut db).unwrap();
        assert!(super::super::schema::migrate_to(&mut db, 5)
            .unwrap_err()
            .contains("newer"));
    }

    #[test]
    fn query_imported_history_filters_pages_and_respects_revision() {
        let (_dir, store) = store();
        let mut a = fact("a", 1.0);
        a.source = "ccswitch".into();
        a.instance = "laptop".into();
        a.client = Some("claude".into());
        a.provider_id = Some("p1".into());
        a.model = Some("m1".into());
        a.at = "2026-10-31T00:00:00Z".parse().unwrap();
        a.day = "2026-10-31".into();
        let mut b = fact("b", 2.0);
        b.source = "ccswitch".into();
        b.instance = "laptop".into();
        b.client = Some("codex".into());
        b.provider_id = Some("p2".into());
        b.model = Some("m2".into());
        b.at = "2026-10-31T00:00:01Z".parse().unwrap();
        b.day = "2026-10-31".into();
        let mut c = fact("c", 3.0);
        c.source = "claudecode".into();
        c.instance = "home".into();
        c.client = Some("claude".into());
        c.at = "2026-11-01T00:00:00Z".parse().unwrap();
        c.day = "2026-11-01".into();
        store
            .commit_import(&[a.clone(), b.clone(), c.clone()])
            .unwrap();

        let mut query = crate::history_import::ImportedHistoryQuery {
            page: 1,
            page_size: 1,
            client: Some("claude".into()),
            ..Default::default()
        };
        let page = store.query_imported_history(&query).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].entity_id, "c");
        query.expected_revision = Some(page.revision);
        query.page = 2;
        let page2 = store.query_imported_history(&query).unwrap();
        assert_eq!(page2.records[0].entity_id, "a");

        let by_source = crate::history_import::ImportedHistoryQuery {
            source: Some("ccswitch".into()),
            ..Default::default()
        };
        assert_eq!(store.query_imported_history(&by_source).unwrap().total, 2);
        let by_provider = crate::history_import::ImportedHistoryQuery {
            provider_id: Some("p1".into()),
            ..Default::default()
        };
        assert_eq!(store.query_imported_history(&by_provider).unwrap().total, 1);
        let priced = crate::history_import::ImportedHistoryQuery {
            priced: Some(true),
            ..Default::default()
        };
        assert_eq!(store.query_imported_history(&priced).unwrap().total, 3);

        // Revision conflict after a clear.
        store.clear_imported(None).unwrap();
        assert!(store
            .query_imported_history(&query)
            .unwrap_err()
            .contains("changed"));
    }

    #[test]
    fn query_imported_history_filters_by_currency() {
        let (_dir, store) = store();
        let usd = fact("usd", 1.0);
        let mut cny = fact("cny", 2.0);
        cny.cost = Some(Cost {
            currency: "CNY".into(),
            amount: 12.0,
        });
        cny.reported_cost = cny.cost.clone();
        store.commit_import(&[usd, cny]).unwrap();
        let query = crate::history_import::ImportedHistoryQuery {
            currency: Some("CNY".into()),
            ..Default::default()
        };
        let page = store.query_imported_history(&query).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.records[0].entity_id, "cny");
    }

    #[test]
    fn query_imported_history_filters_by_data_source() {
        let (_dir, store) = store();
        let mut proxy = fact("proxy", 1.0);
        proxy.data_source = "proxy".into();
        let mut codex = fact("codex", 1.0);
        codex.data_source = "codex_session".into();
        store.commit_import(&[proxy, codex]).unwrap();
        let query = crate::history_import::ImportedHistoryQuery {
            data_source: Some("codex_session".into()),
            ..Default::default()
        };
        let page = store.query_imported_history(&query).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.records[0].entity_id, "codex");
        assert_eq!(page.records[0].data_source, "codex_session");
    }

    #[test]
    fn export_imported_facts_returns_filtered_records_in_stable_order() {
        let (_dir, store) = store();
        let mut a = fact("a", 1.0);
        a.at = "2026-10-31T00:00:01Z".parse().unwrap();
        a.day = "2026-10-31".into();
        let mut b = fact("b", 1.0);
        b.source = "other".into();
        b.at = "2026-10-31T00:00:00Z".parse().unwrap();
        b.day = "2026-10-31".into();
        store.commit_import(&[a, b]).unwrap();
        let query = crate::history_import::ImportedHistoryQuery {
            source: Some("ccswitch".into()),
            ..Default::default()
        };
        let (revision, records) = store.export_imported_facts(&query).unwrap();
        assert!(revision >= 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].entity_id, "a");
    }

    #[test]
    fn clear_imported_scoped_respects_source_instance_client() {
        let (_dir, store) = store();
        let mut a = fact("a", 1.0);
        a.source = "ccswitch".into();
        a.instance = "laptop".into();
        a.client = Some("claude".into());
        let mut b = fact("b", 1.0);
        b.source = "ccswitch".into();
        b.instance = "desktop".into();
        b.client = Some("codex".into());
        let mut c = fact("c", 1.0);
        c.source = "claudecode".into();
        c.instance = "laptop".into();
        c.client = Some("claude".into());
        store.commit_import(&[a, b, c]).unwrap();

        // Client-only clear removes matching facts but, by design, does not discard
        // source/instance-addressable staging runs.
        let removed = store
            .clear_imported_scoped(&crate::history_import::ImportClearScope {
                client: Some("claude".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 1);

        // Source+instance scope.
        let dir2 = tempfile::tempdir().unwrap();
        let store2 = RuntimeStore::open(&dir2.path().join("runtime.db")).unwrap();
        let mut x = fact("x", 1.0);
        x.source = "ccswitch".into();
        x.instance = "laptop".into();
        let mut y = fact("y", 1.0);
        y.source = "ccswitch".into();
        y.instance = "desktop".into();
        store2.commit_import(&[x, y]).unwrap();
        let removed = store2
            .clear_imported_scoped(&crate::history_import::ImportClearScope {
                source: Some("ccswitch".into()),
                instance: Some("laptop".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store2.imported_totals(None, None).unwrap().requests, 1);
    }
}
