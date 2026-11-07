//! cc-switch's usage history, read from a copy of its SQLite database.
//!
//! The source is opened read-only and `query_only`: importing must never change
//! another tool's records. Two shapes arrive from it:
//!
//! * `proxy_request_logs` — one row per proxied call or per imported session
//!   message. These rows can be updated after they are written, and the table
//!   keeps no `updated_at`, so the importer compares content instead.
//! * `usage_daily_rollups` — a materialised *replacement* for detail that
//!   cc-switch has pruned, one row per local day and dimension. A day with
//!   detail still present is skipped rather than added to it.
//!
//! cc-switch itself suppresses a session row when an equivalent proxy row exists
//! within ten minutes. That is a heuristic, not an identity, so the same row is
//! kept here and marked as a *suspected* duplicate: visible, and excluded from
//! totals until a person confirms it.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{TimeZone, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};

use crate::billing::Cost;
use crate::ir::Usage;
use crate::runtime_store::RuntimeStore;

use super::{
    day_start_utc, finish_import, local_day, normalise_input, stage_chunk, Duplicate, Granularity,
    ImportCancel, ImportOutcome, ImportPreview, ImportRange, ImportedFact, InputSemantics,
};

/// The source family recorded on every fact from this adapter.
pub const SOURCE: &str = "ccswitch";

/// The oldest cc-switch schema this adapter reads. `input_token_semantics`
/// arrived in version 13; a database older than that cannot say what its input
/// counts mean, and guessing would silently change every total.
pub const MIN_SCHEMA_VERSION: i64 = 13;

/// The window cc-switch itself uses to match a session row against a proxy row.
const SESSION_PROXY_WINDOW_SECONDS: i64 = 600;

/// Apps whose recorded input includes the cache counts.
const CACHE_INCLUSIVE_APPS: &[&str] = &["codex", "gemini", "grokbuild"];

/// One readable copy of a cc-switch database.
pub struct CcSwitchSource {
    db: Connection,
    instance: String,
    version: i64,
    has_rollups: bool,
}

/// Counters gathered by one scan, used by both preview and import.
#[derive(Default)]
struct ScanReport {
    requests: u64,
    aggregates: u64,
    suspected: u64,
    covered_totals: u64,
    priced: u64,
    unpriced: u64,
    failures: u64,
    /// Rows the source cannot explain: no id, an impossible timestamp, or a
    /// negative count. Counted so a preview can say they were left out.
    skipped: u64,
    days: BTreeSet<String>,
}

type TokenKey = (u32, u32, u32, u32);

impl CcSwitchSource {
    /// Open a source database without any write capability.
    pub fn open(path: &Path, instance: impl Into<String>) -> Result<Self, String> {
        let instance = instance.into();
        if instance.trim().is_empty() {
            return Err("an import needs the source instance's name".into());
        }
        if !path.is_file() {
            return Err(format!("no cc-switch database at {}", path.display()));
        }
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("cannot open the cc-switch database read-only: {error}"))?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        db.pragma_update(None, "query_only", "ON")
            .map_err(|error| error.to_string())?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if version < MIN_SCHEMA_VERSION {
            return Err(format!(
                "cc-switch schema {version} predates {MIN_SCHEMA_VERSION}; its input token counts have no declared meaning here"
            ));
        }
        if !table_exists(&db, "proxy_request_logs")? {
            return Err("this database has no proxy_request_logs table; it is not a cc-switch usage database".into());
        }
        let has_rollups = table_exists(&db, "usage_daily_rollups")?;
        Ok(Self {
            db,
            instance,
            version,
            has_rollups,
        })
    }

    pub fn version(&self) -> i64 {
        self.version
    }

    /// What an import would store, without writing anything.
    pub fn preview(&self, range: &ImportRange, samples: usize) -> Result<ImportPreview, String> {
        range.validate()?;
        let _snapshot = self
            .db
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        self.preview_scan(range, samples.min(20), &ImportCancel::new(), |_| Ok(()))
    }

    fn preview_scan(
        &self,
        range: &ImportRange,
        samples: usize,
        cancel: &ImportCancel,
        mut visit: impl FnMut(ImportedFact) -> Result<(), String>,
    ) -> Result<ImportPreview, String> {
        let mut preview = ImportPreview {
            source: SOURCE.into(),
            instance: self.instance.clone(),
            source_version: self.version,
            ..ImportPreview::default()
        };
        if !self.has_rollups {
            preview.warnings.push(
                "the source has no usage_daily_rollups table; pruned days cannot be recovered"
                    .into(),
            );
        }
        let report = self.scan(range, cancel, |fact| {
            if preview.samples.len() < samples {
                preview.samples.push(fact.clone());
            }
            visit(fact)
        })?;
        preview.requests = report.requests;
        preview.aggregates = report.aggregates;
        preview.suspected = report.suspected;
        preview.covered_totals = report.covered_totals;
        preview.priced = report.priced;
        preview.unpriced = report.unpriced;
        preview.failures = report.failures;
        preview.days = report.days.into_iter().collect();
        if report.skipped > 0 {
            preview.warnings.push(format!(
                "{} source row(s) were left out because their id, timestamp or counts are unusable",
                report.skipped
            ));
        }
        Ok(preview)
    }

    /// Store every fact in the window.
    ///
    /// The read is chunked and staged; the publish is one transaction at the
    /// end. An incomplete scan is restarted from a fresh snapshot; only a
    /// completed preview can be resumed without reopening the source.
    pub fn import(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportOutcome, String> {
        let preview = self.stage_chunks(
            store,
            range,
            &uuid::Uuid::new_v4().to_string(),
            chunk,
            cancel,
        )?;
        finish_import(store, &preview, cancel)
    }

    pub fn stage(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        run_id: &str,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        self.stage_chunks(store, range, run_id, 500, cancel)
    }

    fn stage_chunks(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        run_id: &str,
        chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        range.validate()?;
        if cancel.is_cancelled() {
            return Ok(ImportPreview {
                cancelled: true,
                ..ImportPreview::default()
            });
        }
        // All tables, including WAL changes, belong to one SQLite read snapshot.
        let _snapshot = self
            .db
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        let run = store.begin_import_run(run_id, SOURCE, &self.instance, self.version, range)?;
        let result = (|| {
            let chunk = chunk.clamp(1, 500);
            let mut pending = Vec::with_capacity(chunk);
            let preview = self.preview_scan(range, 0, cancel, |fact| {
                pending.push(fact);
                if pending.len() >= chunk {
                    stage_chunk(store, &run, &mut pending, cancel)?;
                }
                Ok(())
            })?;
            stage_chunk(store, &run, &mut pending, cancel)?;
            store.ready_import(run_id, preview)
        })();
        if result.is_err() {
            store.discard_import_run(run_id)?;
            if cancel.is_cancelled() {
                return Ok(ImportPreview {
                    cancelled: true,
                    ..ImportPreview::default()
                });
            }
        }
        result
    }

    fn scan(
        &self,
        range: &ImportRange,
        cancel: &ImportCancel,
        mut visit: impl FnMut(ImportedFact) -> Result<(), String>,
    ) -> Result<ScanReport, String> {
        let mut report = ScanReport::default();
        cancel.check()?;
        let covered_days = if self.has_rollups {
            self.detail_days(cancel)?
        } else {
            BTreeSet::new()
        };

        let from = range
            .from
            .map(|at| at.timestamp() - SESSION_PROXY_WINDOW_SECONDS);
        let until = range.until.map(|at| at.timestamp() + 1);
        let mut statement = self
            .db
            .prepare(
                "SELECT request_id, provider_id, app_type, model, request_model, pricing_model,
                        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                        input_token_semantics, total_cost_usd, latency_ms, first_token_ms,
                        status_code, error_message, is_streaming, cost_multiplier, created_at,
                        COALESCE(data_source, 'proxy')
                 FROM proxy_request_logs
                 WHERE (?1 IS NULL OR created_at >= ?1) AND (?2 IS NULL OR created_at < ?2)
                 ORDER BY created_at, request_id",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query(rusqlite::params![from, until])
            .map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            cancel.check()?;
            let Some(mut fact) = self.detail_fact(row)? else {
                report.skipped += 1;
                continue;
            };
            if !range.contains(fact.at) {
                continue;
            }
            // A session row may describe the same call a proxy row already
            // recorded. cc-switch's own rule is a ten-minute, same-tokens match;
            // that is evidence, not identity, so it is kept and marked.
            if fact.data_source != "proxy" {
                if let Some(twin) = self.proxy_twin(&fact, cancel)? {
                    fact.duplicate = Duplicate::Suspected;
                    fact.duplicate_of = Some(twin);
                }
            }
            tally(&mut report, &fact);
            visit(fact)?;
        }

        if self.has_rollups {
            let mut statement = self
                .db
                .prepare(
                    "SELECT date, app_type, provider_id, model, request_model, pricing_model,
                            request_count, success_count, input_tokens, output_tokens,
                            cache_read_tokens, cache_creation_tokens, input_token_semantics,
                            total_cost_usd, avg_latency_ms
                     FROM usage_daily_rollups
                     ORDER BY date, app_type, provider_id, model, request_model, pricing_model",
                )
                .map_err(|error| error.to_string())?;
            let mut rows = statement.query([]).map_err(|error| error.to_string())?;
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                cancel.check()?;
                let Some(fact) = rollup_fact(&self.instance, row)? else {
                    continue;
                };
                if !range.contains_fact(&fact) {
                    continue;
                }
                // A day that still has detail is not also represented by its
                // pruned total: the total replaced that detail, it did not
                // follow it.
                if covered_days.contains(&fact.day) {
                    report.covered_totals += 1;
                    continue;
                }
                report.aggregates += 1;
                tally(&mut report, &fact);
                visit(fact)?;
            }
        }
        Ok(report)
    }

    /// Search only the source's matching window. Never load a whole archive's
    /// proxy index into memory merely to match one session message.
    fn proxy_twin(
        &self,
        fact: &ImportedFact,
        cancel: &ImportCancel,
    ) -> Result<Option<String>, String> {
        let from = fact.at.timestamp() - SESSION_PROXY_WINDOW_SECONDS;
        let until = fact.at.timestamp() + SESSION_PROXY_WINDOW_SECONDS;
        let mut statement = self
            .db
            .prepare(
                "SELECT request_id, app_type, model, input_tokens, output_tokens,
                        cache_read_tokens, cache_creation_tokens, input_token_semantics,
                        created_at
                 FROM proxy_request_logs
                 WHERE COALESCE(data_source, 'proxy') = 'proxy'
                    AND created_at >= ?1 AND created_at <= ?2
                  ORDER BY created_at, request_id",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query(rusqlite::params![from, until])
            .map_err(|error| error.to_string())?;
        let expected_app = fold_app(fact.client.as_deref().unwrap_or_default());
        let expected_model = model_key(fact.model.as_deref().unwrap_or_default());
        let expected_tokens = token_key(&fact.tokens);
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            cancel.check()?;
            let id = text(row, 0)?;
            if id.trim().is_empty() {
                continue;
            }
            let app = text(row, 1)?;
            let model = text(row, 2)?;
            let tokens = tokens(
                count(row, 3)?,
                count(row, 4)?,
                count(row, 5)?,
                count(row, 6)?,
                InputSemantics::from_ccswitch(count(row, 7)?)?,
                &app,
            )?;
            let model = model_key(&model);
            if fold_app(&app) == expected_app
                && token_key(&tokens) == expected_tokens
                && (model == expected_model || model == "unknown" || expected_model == "unknown")
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Local days that still carry request detail.
    fn detail_days(&self, cancel: &ImportCancel) -> Result<BTreeSet<String>, String> {
        let mut statement = self
            .db
            .prepare(
                "SELECT DISTINCT date(created_at, 'unixepoch', 'localtime')
                 FROM proxy_request_logs",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .map_err(|error| error.to_string())?;
        let mut days = BTreeSet::new();
        for row in rows {
            cancel.check()?;
            if let Some(day) = row.map_err(|error| error.to_string())? {
                days.insert(day);
            }
        }
        Ok(days)
    }

    fn detail_fact(&self, row: &rusqlite::Row<'_>) -> Result<Option<ImportedFact>, String> {
        let entity_id = text(row, 0)?;
        if entity_id.trim().is_empty() {
            return Ok(None);
        }
        let app = text(row, 2)?;
        let Some(at) = Utc.timestamp_opt(count(row, 18)?, 0).single() else {
            return Ok(None);
        };
        let semantics = InputSemantics::from_ccswitch(count(row, 10)?)?;
        let tokens = tokens(
            count(row, 6)?,
            count(row, 7)?,
            count(row, 8)?,
            count(row, 9)?,
            semantics,
            &app,
        )?;
        let reported = amount(row, 11)?;
        let latency = positive(numeric(row, 12)?);
        let ttft = positive(numeric(row, 13)?);
        let status =
            numeric(row, 14)?.and_then(|status| u16::try_from(status).ok().filter(|s| *s > 0));
        // A source error can contain response bodies or credentials. The status
        // is sufficient accounting evidence; never persist arbitrary error text.
        let streaming = count(row, 16)?;
        let multiplier = optional_text(row, 17)?.as_deref().and_then(finite);
        let data_source = text(row, 19)?;
        let priced_amount = reported.filter(|amount| *amount > 0.0);
        Ok(Some(ImportedFact {
            source: SOURCE.into(),
            instance: self.instance.clone(),
            entity_id,
            revision: String::new(),
            granularity: Granularity::Request,
            at,
            day: local_day(at),
            client: Some(app),
            provider_id: optional_text(row, 1)?,
            model: optional_text(row, 3)?,
            request_model: optional_text(row, 4)?,
            pricing_model: optional_text(row, 5)?,
            tokens,
            input_semantics: Some(semantics),
            cost: priced_amount.map(|amount| Cost {
                currency: "USD".into(),
                amount,
            }),
            reported_cost: reported.map(|amount| Cost {
                currency: "USD".into(),
                amount,
            }),
            priced: priced_amount.is_some(),
            latency_ms: latency,
            ttft_ms: ttft,
            status,
            ok: status.map(|status| (200..300).contains(&status)),
            streaming: streaming != 0,
            requests: 1,
            failures: u64::from(status.is_some_and(|status| !(200..300).contains(&status))),
            data_source,
            duplicate: Duplicate::None,
            duplicate_of: None,
            multiplier,
            error: None,
        }))
    }
}

fn rollup_fact(instance: &str, row: &rusqlite::Row<'_>) -> Result<Option<ImportedFact>, String> {
    let day = text(row, 0)?;
    let Some(at) = day_start_utc(&day) else {
        return Ok(None);
    };
    let app = text(row, 1)?;
    let requests = count(row, 6)?;
    if requests <= 0 {
        return Ok(None);
    }
    let success = count(row, 7)?;
    let semantics = InputSemantics::from_ccswitch(count(row, 12)?)?;
    let tokens = tokens(
        count(row, 8)?,
        count(row, 9)?,
        count(row, 10)?,
        count(row, 11)?,
        semantics,
        &app,
    )?;
    let reported = amount(row, 13)?;
    let priced_amount = reported.filter(|amount| *amount > 0.0);
    let requests = u64::try_from(requests).map_err(|_| "negative rollup count")?;
    let success = u64::try_from(success).map_err(|_| "negative rollup success count")?;
    let failures = requests
        .checked_sub(success)
        .ok_or("rollup successes exceed requests")?;
    Ok(Some(ImportedFact {
        source: SOURCE.into(),
        instance: instance.into(),
        entity_id: format!(
            "rollup:{day}:{app}:{}:{}:{}:{}",
            text(row, 2)?,
            text(row, 3)?,
            text(row, 4)?,
            text(row, 5)?,
        ),
        revision: String::new(),
        granularity: Granularity::Day,
        at,
        day,
        client: Some(app),
        provider_id: optional_text(row, 2)?,
        model: optional_text(row, 3)?,
        request_model: optional_text(row, 4)?,
        pricing_model: optional_text(row, 5)?,
        tokens,
        input_semantics: Some(semantics),
        cost: priced_amount.map(|amount| Cost {
            currency: "USD".into(),
            amount,
        }),
        reported_cost: reported.map(|amount| Cost {
            currency: "USD".into(),
            amount,
        }),
        priced: priced_amount.is_some(),
        latency_ms: positive(numeric(row, 14)?),
        ttft_ms: None,
        status: None,
        ok: None,
        streaming: false,
        requests,
        failures,
        data_source: "rollup".into(),
        duplicate: Duplicate::None,
        duplicate_of: None,
        multiplier: None,
        error: None,
    }))
}

fn tally(report: &mut ScanReport, fact: &ImportedFact) {
    match fact.granularity {
        Granularity::Request => report.requests += 1,
        Granularity::Day => {}
    }
    report.days.insert(fact.day.clone());
    if fact.duplicate == Duplicate::Suspected {
        report.suspected += 1;
    }
    report.failures += fact.failures;
    if fact.priced {
        report.priced += 1;
    } else {
        report.unpriced += 1;
    }
}

fn tokens(
    input: i64,
    output: i64,
    read: i64,
    write: i64,
    semantics: InputSemantics,
    app: &str,
) -> Result<Usage, String> {
    let input = u32::try_from(input).map_err(|_| "negative input token count")?;
    let output = u32::try_from(output).map_err(|_| "negative output token count")?;
    let read = u32::try_from(read).map_err(|_| "negative cache read count")?;
    let write = u32::try_from(write).map_err(|_| "negative cache write count")?;
    let cache_inclusive = CACHE_INCLUSIVE_APPS
        .iter()
        .any(|name| name.eq_ignore_ascii_case(app));
    Ok(Usage {
        input_tokens: normalise_input(input, read, write, semantics, cache_inclusive)?,
        output_tokens: output,
        cache_read_tokens: read,
        cache_write_tokens: write,
        reasoning_tokens: 0,
    })
}

fn token_key(usage: &Usage) -> TokenKey {
    (
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
    )
}

fn fold_app(app: &str) -> String {
    match app {
        "claude-desktop" => "claude".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn model_key(model: &str) -> String {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() {
        "unknown".into()
    } else {
        model
    }
}

/// Read a numeric column as an integer, tolerating the storage class another
/// project's schema may produce.
///
/// `usage_daily_rollups.avg_latency_ms` is an average, so cc-switch stores it as
/// REAL even though every latency was whole; rusqlite refuses to hand a REAL to
/// an `i64` getter, and one such advisory column failed a whole import. Textual
/// digits are accepted for the same reason, and absence stays `None` so callers
/// choose their own default. Non-finite values are treated as absent.
fn numeric(row: &rusqlite::Row<'_>, index: usize) -> Result<Option<i64>, String> {
    let value = row
        .get::<_, Option<Value>>(index)
        .map_err(|error| error.to_string())?;
    Ok(match value {
        Some(Value::Integer(value)) => Some(value),
        Some(Value::Real(value)) if value.is_finite() => Some(value.round() as i64),
        Some(Value::Text(text)) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| value.round() as i64),
        _ => None,
    })
}

/// A numeric column where absence means the source's own zero. A value that is
/// present but is not a number is refused rather than counted as zero: a token
/// count that cannot be read must not become a free call.
fn count(row: &rusqlite::Row<'_>, index: usize) -> Result<i64, String> {
    let value = row
        .get::<_, Option<Value>>(index)
        .map_err(|error| error.to_string())?;
    match value {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Integer(value)) => Ok(value),
        Some(Value::Real(value))
            if value.is_finite()
                && value.fract() == 0.0
                && value >= i64::MIN as f64
                && value < i64::MAX as f64 =>
        {
            Ok(value as i64)
        }
        Some(Value::Text(text)) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("column {index} is not an integer count")),
        Some(_) => Err(format!("column {index} holds a value that is not a count")),
    }
}

/// Read a text column, keeping absence as `None`. A numeric column is rendered
/// as its text so a development schema that widened a column still reads.
fn optional_text(row: &rusqlite::Row<'_>, index: usize) -> Result<Option<String>, String> {
    let value = row
        .get::<_, Option<Value>>(index)
        .map_err(|error| error.to_string())?;
    Ok(match value {
        Some(Value::Text(text)) => Some(text),
        Some(Value::Integer(value)) => Some(value.to_string()),
        Some(Value::Real(value)) => Some(value.to_string()),
        _ => None,
    })
}

/// A text column whose absence means the empty string.
fn text(row: &rusqlite::Row<'_>, index: usize) -> Result<String, String> {
    Ok(optional_text(row, index)?.unwrap_or_default())
}

/// A money column. cc-switch writes the USD amount as text; a development
/// schema may write it as a number, so both are read through their text form.
fn amount(row: &rusqlite::Row<'_>, index: usize) -> Result<Option<f64>, String> {
    Ok(optional_text(row, index)?.as_deref().and_then(usd))
}

fn positive(value: Option<i64>) -> Option<u64> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .filter(|v| *v > 0)
}

fn usd(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|amount| amount.is_finite() && *amount >= 0.0)
}

fn finite(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn table_exists(db: &Connection, name: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_import::{day_start_utc, ImportRange};
    use rusqlite::params;
    use std::path::PathBuf;

    const DAY: &str = "2026-01-02";
    const OLDER: &str = "2025-12-30";

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn path(&self) -> PathBuf {
            self.dir.path().join("cc-switch.db")
        }

        fn db(&self) -> Connection {
            let db = Connection::open(self.path()).unwrap();
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS proxy_request_logs (
                    request_id TEXT PRIMARY KEY,
                    provider_id TEXT NOT NULL,
                    app_type TEXT NOT NULL,
                    model TEXT NOT NULL,
                    request_model TEXT,
                    pricing_model TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                    input_token_semantics INTEGER NOT NULL DEFAULT 0,
                    input_cost_usd TEXT NOT NULL DEFAULT '0',
                    output_cost_usd TEXT NOT NULL DEFAULT '0',
                    cache_read_cost_usd TEXT NOT NULL DEFAULT '0',
                    cache_creation_cost_usd TEXT NOT NULL DEFAULT '0',
                    total_cost_usd TEXT NOT NULL DEFAULT '0',
                    latency_ms INTEGER NOT NULL,
                    first_token_ms INTEGER,
                    duration_ms INTEGER,
                    status_code INTEGER NOT NULL,
                    error_message TEXT,
                    session_id TEXT,
                    provider_type TEXT,
                    is_streaming INTEGER NOT NULL DEFAULT 0,
                    cost_multiplier TEXT NOT NULL DEFAULT '1.0',
                    created_at INTEGER NOT NULL,
                    data_source TEXT NOT NULL DEFAULT 'proxy'
                );
                CREATE TABLE IF NOT EXISTS usage_daily_rollups (
                    date TEXT NOT NULL,
                    app_type TEXT NOT NULL,
                    provider_id TEXT NOT NULL,
                    model TEXT NOT NULL,
                    request_model TEXT NOT NULL DEFAULT '',
                    pricing_model TEXT NOT NULL DEFAULT '',
                    request_count INTEGER NOT NULL DEFAULT 0,
                    success_count INTEGER NOT NULL DEFAULT 0,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                    input_token_semantics INTEGER NOT NULL DEFAULT 0,
                    total_cost_usd TEXT NOT NULL DEFAULT '0',
                    avg_latency_ms INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (date, app_type, provider_id, model, request_model, pricing_model)
                );",
            )
            .unwrap();
            db.pragma_update(None, "user_version", 21i64).unwrap();
            db
        }
    }

    struct Detail<'a> {
        id: &'a str,
        app: &'a str,
        model: &'a str,
        input: i64,
        output: i64,
        read: i64,
        write: i64,
        semantics: i64,
        cost: &'a str,
        status: i64,
        offset: i64,
        data_source: &'a str,
    }

    fn insert_detail(db: &Connection, day: &str, row: Detail<'_>) {
        let at = day_start_utc(day).unwrap().timestamp() + row.offset;
        db.execute(
            "INSERT INTO proxy_request_logs
                (request_id, provider_id, app_type, model, input_tokens, output_tokens,
                 cache_read_tokens, cache_creation_tokens, input_token_semantics,
                 total_cost_usd, latency_ms, status_code, is_streaming, created_at, data_source)
             VALUES (?1, 'p', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 40, ?10, 0, ?11, ?12)",
            params![
                row.id,
                row.app,
                row.model,
                row.input,
                row.output,
                row.read,
                row.write,
                row.semantics,
                row.cost,
                row.status,
                at,
                row.data_source,
            ],
        )
        .unwrap();
    }

    fn insert_rollup(db: &Connection, day: &str, app: &str, requests: i64, success: i64) {
        db.execute(
            "INSERT INTO usage_daily_rollups
                (date, app_type, provider_id, model, request_count, success_count,
                 input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                 input_token_semantics, total_cost_usd, avg_latency_ms)
             VALUES (?1, ?2, 'p', 'm', ?3, ?4, 50, 7, 5, 0, 2, '0.5', 30)",
            params![day, app, requests, success],
        )
        .unwrap();
    }

    fn fixture() -> Fixture {
        let fixture = Fixture::new();
        let db = fixture.db();
        // A priced proxy row.
        insert_detail(
            &db,
            DAY,
            Detail {
                id: "session:aaa",
                app: "claude",
                model: "claude-sonnet-4-5",
                input: 100,
                output: 20,
                read: 30,
                write: 10,
                semantics: 2,
                cost: "0.25",
                status: 200,
                offset: 3600,
                data_source: "proxy",
            },
        );
        // A codex proxy row whose input already includes its cached reads.
        insert_detail(
            &db,
            DAY,
            Detail {
                id: "session:codex:1",
                app: "codex",
                model: "gpt-5",
                input: 100,
                output: 10,
                read: 30,
                write: 0,
                semantics: 1,
                cost: "0.10",
                status: 200,
                offset: 7200,
                data_source: "proxy",
            },
        );
        // The same codex call seen again by the session importer a minute later,
        // under its own id and a legacy (0) token meaning.
        insert_detail(
            &db,
            DAY,
            Detail {
                id: "codex_session:t:1",
                app: "codex",
                model: "gpt-5",
                input: 100,
                output: 10,
                read: 30,
                write: 0,
                semantics: 0,
                cost: "0.10",
                status: 200,
                offset: 7260,
                data_source: "codex_session",
            },
        );
        // A priced-looking zero: tokens were used, the source applied no price.
        insert_detail(
            &db,
            DAY,
            Detail {
                id: "session:bbb",
                app: "claude",
                model: "claude-opus-4-1",
                input: 7,
                output: 3,
                read: 0,
                write: 0,
                semantics: 2,
                cost: "0",
                status: 200,
                offset: 9000,
                data_source: "proxy",
            },
        );
        // A failed call with no usage.
        insert_detail(
            &db,
            DAY,
            Detail {
                id: "session:ccc",
                app: "claude",
                model: "claude-haiku-4-5",
                input: 0,
                output: 0,
                read: 0,
                write: 0,
                semantics: 2,
                cost: "0",
                status: 500,
                offset: 9900,
                data_source: "proxy",
            },
        );
        // An older day that only survives as a materialised total.
        insert_rollup(&db, OLDER, "claude", 5, 4);
        // A total for a day whose detail is still present: it must not be added.
        insert_rollup(&db, DAY, "claude", 9, 9);
        fixture
    }

    fn range() -> ImportRange {
        ImportRange::default()
    }

    #[test]
    fn preview_counts_the_rows_it_would_store_without_writing() {
        let fixture = fixture();
        let source = CcSwitchSource::open(&fixture.path(), "laptop").unwrap();
        assert_eq!(source.version(), 21);
        let preview = source.preview(&range(), 10).unwrap();
        assert_eq!(preview.requests, 5);
        assert_eq!(preview.aggregates, 1, "one uncovered day total");
        assert_eq!(preview.covered_totals, 1, "the day with detail is skipped");
        assert_eq!(preview.suspected, 1);
        assert_eq!(preview.failures, 2, "the 500 and the rollup's one failure");
        assert_eq!(preview.unpriced, 2, "the zero-cost call and the failed one");
        assert!(preview.days.contains(&DAY.to_string()));
        assert!(preview.days.contains(&OLDER.to_string()));
        assert!(!preview.samples.is_empty());
        // A preview writes nothing: the store this adapter would target does not
        // exist yet, which the API enforces by taking it as an argument.
        let store = RuntimeStore::open(&fixture.dir.path().join("runtime.db")).unwrap();
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
    }

    #[test]
    fn importing_twice_stores_each_fact_once_and_updates_a_changed_row() {
        let fixture = fixture();
        let store = RuntimeStore::open(&fixture.dir.path().join("runtime.db")).unwrap();
        let source = CcSwitchSource::open(&fixture.path(), "laptop").unwrap();

        let first = source
            .import(&store, &range(), 2, &ImportCancel::new())
            .unwrap();
        assert_eq!(first.inserted, 6);
        assert_eq!(first.unchanged, 0);
        assert!(first.revision > 0);

        let second = source
            .import(&store, &range(), 2, &ImportCancel::new())
            .unwrap();
        assert_eq!(second.inserted, 0);
        assert_eq!(second.updated, 0, "an unchanged source writes nothing");
        assert_eq!(second.unchanged, 6);
        assert_eq!(second.revision, first.revision);

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 9, "4 canonical requests + a 5-request day");
        assert_eq!(totals.failures, 2);
        assert_eq!(totals.suspected_requests, 1);
        assert_eq!(totals.aggregate_requests, 5);
        assert_eq!(totals.unpriced_requests, 2);
        // 100 fresh + 30 read + 10 write, then codex's already-inclusive 100,
        // then 7 from the unpriced call, then the day total's 50 + 5 cached
        // reads. The failed call used no tokens at all.
        assert_eq!(totals.input_tokens, 140 + 100 + 7 + 55);
        assert_eq!(totals.output_tokens, 20 + 10 + 3 + 7);
        assert_eq!(totals.cost.get("USD"), 0.25 + 0.10 + 0.5);

        let days = store
            .imported_days(OLDER.parse().unwrap(), DAY.parse().unwrap())
            .unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].day, OLDER);
        assert_eq!(days[0].requests, 5);
        assert_eq!(days[1].day, DAY);
        assert_eq!(days[1].requests, 4);
        assert_eq!(days[1].cost.get("USD"), 0.25 + 0.10);

        // A late correction to the source row is picked up as an update.
        let db = fixture.db();
        db.execute(
            "UPDATE proxy_request_logs SET output_tokens = 99 WHERE request_id = 'session:bbb'",
            [],
        )
        .unwrap();
        drop(db);
        let third = source
            .import(&store, &range(), 2, &ImportCancel::new())
            .unwrap();
        assert_eq!(third.updated, 1);
        assert_eq!(third.unchanged, 5);
        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.output_tokens, 20 + 10 + 99 + 7);
    }

    #[test]
    fn a_window_selects_requests_and_the_days_it_fully_covers() {
        let fixture = fixture();
        let source = CcSwitchSource::open(&fixture.path(), "laptop").unwrap();
        let from = day_start_utc(DAY).unwrap() + chrono::Duration::seconds(8000);
        let until = day_start_utc(DAY).unwrap() + chrono::Duration::seconds(10_000);
        let preview = source
            .preview(&ImportRange::new(Some(from), Some(until)).unwrap(), 0)
            .unwrap();
        assert_eq!(preview.requests, 2, "the zero-cost and the failed call");
        assert_eq!(
            preview.aggregates, 0,
            "the day total starts before the window, and the older day is outside it"
        );
    }

    #[test]
    fn a_rollup_average_stored_as_real_is_read_not_refused() {
        let fixture = Fixture::new();
        let db = fixture.db();
        insert_detail(
            &db,
            DAY,
            Detail {
                id: "session:real",
                app: "claude",
                model: "m",
                input: 10,
                output: 4,
                read: 0,
                write: 0,
                semantics: 2,
                cost: "0.25",
                status: 200,
                offset: 60,
                data_source: "proxy",
            },
        );
        // AVG() is REAL in cc-switch's own schema: whole latencies still produce
        // a fractional average, and SQLite keeps it REAL in an INTEGER column.
        db.execute(
            "UPDATE proxy_request_logs SET latency_ms = 42.75 WHERE request_id = 'session:real'",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO usage_daily_rollups
                (date, app_type, provider_id, model, request_model, pricing_model,
                 request_count, success_count, input_tokens, output_tokens,
                 cache_read_tokens, cache_creation_tokens, input_token_semantics,
                 total_cost_usd, avg_latency_ms)
             VALUES (?1, 'claude', 'p', 'm', '', '', 4, 4, 10, 2, 0, 0, 2, '0.5', 31.7)",
            params![OLDER],
        )
        .unwrap();
        drop(db);

        let source = CcSwitchSource::open(&fixture.path(), "laptop").unwrap();
        let preview = source.preview(&range(), 10).unwrap();
        assert_eq!(preview.requests, 1, "the request row is still read");
        assert_eq!(preview.aggregates, 1, "the day total is still read");
        let request = preview
            .samples
            .iter()
            .find(|fact| fact.granularity == Granularity::Request)
            .expect("the request sample");
        assert_eq!(
            request.latency_ms,
            Some(43),
            "a fractional latency is rounded to its own millisecond"
        );
        let day = preview
            .samples
            .iter()
            .find(|fact| fact.granularity == Granularity::Day)
            .expect("the day sample");
        assert_eq!(day.latency_ms, Some(32));
        assert!(day.priced, "the day total still carries its price");
    }

    #[test]
    fn a_source_that_cannot_say_what_its_counts_mean_is_refused() {
        let fixture = Fixture::new();
        let db = Connection::open(fixture.path()).unwrap();
        db.execute_batch(
            "CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL,
                app_type TEXT NOT NULL, model TEXT NOT NULL, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_creation_tokens INTEGER, input_token_semantics INTEGER,
                total_cost_usd TEXT, latency_ms INTEGER, first_token_ms INTEGER, status_code INTEGER,
                error_message TEXT, is_streaming INTEGER, cost_multiplier TEXT, created_at INTEGER,
                data_source TEXT);",
        )
        .unwrap();
        db.pragma_update(None, "user_version", 9i64).unwrap();
        drop(db);
        let error = CcSwitchSource::open(&fixture.path(), "laptop")
            .err()
            .expect("an old schema is refused");
        assert!(error.contains("predates"), "{error}");

        // A current schema with no usage table at all: the version check passes,
        // so this exercises the second refusal.
        let empty = Fixture::new();
        let db = Connection::open(empty.path()).unwrap();
        db.execute_batch("CREATE TABLE other (id INTEGER);")
            .unwrap();
        db.pragma_update(None, "user_version", 21i64).unwrap();
        drop(db);
        let error = CcSwitchSource::open(&empty.path(), "laptop")
            .err()
            .expect("a database without the usage table is refused");
        assert!(error.contains("proxy_request_logs"), "{error}");
    }

    #[test]
    fn reading_a_source_never_changes_it() {
        let fixture = fixture();
        let before = std::fs::read(fixture.path()).unwrap();
        let store = RuntimeStore::open(&fixture.dir.path().join("runtime.db")).unwrap();
        let source = CcSwitchSource::open(&fixture.path(), "laptop").unwrap();
        source
            .import(&store, &range(), 8, &ImportCancel::new())
            .unwrap();
        drop(source);
        let after = std::fs::read(fixture.path()).unwrap();
        assert_eq!(before, after, "the source database is untouched");
    }

    #[test]
    fn cancellation_and_late_bad_rows_publish_no_partial_batch() {
        let fixture = fixture();
        let store = RuntimeStore::open(&fixture.dir.path().join("runtime.db")).unwrap();
        let source = CcSwitchSource::open(&fixture.path(), "copy").unwrap();
        let cancel = ImportCancel::new();
        cancel.cancel();
        let outcome = source.import(&store, &range(), 1, &cancel).unwrap();
        assert!(outcome.cancelled);
        assert_eq!(store.imported_revision().unwrap(), 0);

        let db = fixture.db();
        db.execute(
            "UPDATE proxy_request_logs SET input_tokens = -1 WHERE request_id = 'session:ccc'",
            [],
        )
        .unwrap();
        assert!(source
            .import(&store, &range(), 1, &ImportCancel::new())
            .is_err());
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
        assert_eq!(store.imported_revision().unwrap(), 0);
        assert!(store.active_import_run(SOURCE, "copy").unwrap().is_none());
    }

    #[test]
    fn detail_window_does_not_include_the_duplicate_search_margin() {
        let fixture = fixture();
        let source = CcSwitchSource::open(&fixture.path(), "copy").unwrap();
        let from = day_start_utc(DAY).unwrap() + chrono::Duration::seconds(7_300);
        let until = from + chrono::Duration::seconds(300);
        let preview = source
            .preview(&ImportRange::new(Some(from), Some(until)).unwrap(), 0)
            .unwrap();
        assert_eq!(
            preview.requests, 0,
            "7,200/7,260 rows are match evidence only, not in the import window"
        );
    }
}
