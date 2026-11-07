//! Read-only OpenCode V1/V2 accounting metadata; migrated V2 message IDs win.
use std::collections::BTreeSet;
use std::path::Path;

use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{
    finish_import, local_day, normalise_input, stage_chunk, Duplicate, Granularity, ImportCancel,
    ImportOutcome, ImportPreview, ImportRange, ImportedFact, InputSemantics,
};
use crate::billing::Cost;
use crate::ir::Usage;
use crate::runtime_store::RuntimeStore;

pub const SOURCE: &str = "opencode";

pub struct OpenCodeSource {
    db: Connection,
    instance: String,
    v1: bool,
    v2: bool,
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    db.pragma_update(None, "query_only", "ON")
        .map_err(|error| error.to_string())?;
    Ok(db)
}

fn layout(db: &Connection, table: &str, v2: bool) -> Result<bool, String> {
    let mut statement = db
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .map_err(|error| error.to_string())?;
    let names = statement
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(["id", "session_id", "time_created", "time_updated", "data"]
        .iter()
        .all(|name| names.contains(*name))
        && (!v2 || names.contains("type")))
}

impl OpenCodeSource {
    pub fn recognizes(path: &Path) -> Result<bool, String> {
        let db = open_read_only(path)?;
        Ok(layout(&db, "message", false)? || layout(&db, "session_message", true)?)
    }

    pub fn open(path: &Path, instance: impl Into<String>) -> Result<Self, String> {
        let db = open_read_only(path)?;
        let v1 = layout(&db, "message", false)?;
        let v2 = layout(&db, "session_message", true)?;
        let instance = instance.into();
        if !v1 && !v2 {
            return Err("no supported OpenCode message layout".into());
        }
        if instance.trim().is_empty() {
            return Err("an import needs a source instance name".into());
        }
        Ok(Self {
            db,
            instance,
            v1,
            v2,
        })
    }

    pub fn preview(&self, range: &ImportRange, samples: usize) -> Result<ImportPreview, String> {
        self.scan(range, samples.min(20), &ImportCancel::new(), |_| Ok(()))
    }

    pub fn import(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        _chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportOutcome, String> {
        let preview = self.stage(store, range, &uuid::Uuid::new_v4().to_string(), cancel)?;
        finish_import(store, &preview, cancel)
    }

    pub fn stage(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        id: &str,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        range.validate()?;
        if cancel.is_cancelled() {
            return Ok(ImportPreview {
                cancelled: true,
                ..Default::default()
            });
        }
        let version = if self.v2 { 2 } else { 1 };
        let run = store.begin_import_run(id, SOURCE, &self.instance, version, range)?;
        let result = (|| {
            let mut pending = Vec::with_capacity(500);
            let preview = self.scan(range, 0, cancel, |fact| {
                pending.push(fact);
                if pending.len() == 500 {
                    stage_chunk(store, &run, &mut pending, cancel)?;
                }
                Ok(())
            })?;
            stage_chunk(store, &run, &mut pending, cancel)?;
            store.ready_import(id, preview)
        })();
        if result.is_err() {
            store.discard_import_run(id)?;
            if cancel.is_cancelled() {
                return Ok(ImportPreview {
                    cancelled: true,
                    ..Default::default()
                });
            }
        }
        result
    }

    fn scan(
        &self,
        range: &ImportRange,
        samples: usize,
        cancel: &ImportCancel,
        mut visit: impl FnMut(ImportedFact) -> Result<(), String>,
    ) -> Result<ImportPreview, String> {
        range.validate()?;
        cancel.check()?;
        let _snapshot = self
            .db
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        let mut preview = ImportPreview {
            source: SOURCE.into(),
            instance: self.instance.clone(),
            source_version: if self.v2 { 2 } else { 1 },
            ..Default::default()
        };
        let mut days = BTreeSet::new();
        let mut incomplete = 0u64;
        for (table, v2) in [("message", false), ("session_message", true)] {
            if if v2 { !self.v2 } else { !self.v1 } {
                continue;
            }
            // Project metadata only: content, summary, native bodies and error
            // strings never cross the source query or enter the destination.
            let kind = if v2 {
                "m.type"
            } else {
                "json_extract(m.data, '$.role')"
            };
            let migrated = if !v2 && self.v2 {
                "AND NOT EXISTS (SELECT 1 FROM session_message n WHERE n.id = m.id)"
            } else {
                ""
            };
            let sql = format!(
                "SELECT m.id, m.session_id, {kind}, json_object(
                    'time', json_extract(m.data, '$.time'),
                    'tokens', json_extract(m.data, '$.tokens'),
                    'cost', json_extract(m.data, '$.cost'),
                    'modelID', json_extract(m.data, '$.modelID'),
                    'model', json_extract(m.data, '$.model'),
                    'providerID', json_extract(m.data, '$.providerID'),
                    'status', json_extract(m.data, '$.status'),
                    'finish', json_extract(m.data, '$.finish'),
                    'failed', json_type(m.data, '$.error') NOT IN ('null')
                 ) FROM {table} m WHERE {kind} IN ('assistant','compaction') {migrated}
                 ORDER BY m.id"
            );
            let mut statement = self.db.prepare(&sql).map_err(|error| error.to_string())?;
            let mut rows = statement.query([]).map_err(|error| error.to_string())?;
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                cancel.check()?;
                let metadata: Value = serde_json::from_str(
                    &row.get::<_, String>(3).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let kind: String = row.get(2).map_err(|error| error.to_string())?;
                let completed = if kind == "compaction" {
                    matches!(metadata["status"].as_str(), Some("completed" | "failed"))
                } else {
                    metadata["time"]["completed"].as_i64().is_some()
                };
                if !completed {
                    incomplete += 1;
                    continue;
                }
                if metadata["tokens"].is_null() {
                    continue;
                }
                let id: String = row.get(0).map_err(|error| error.to_string())?;
                let session: String = row.get(1).map_err(|error| error.to_string())?;
                let Some(fact) =
                    fact_from_metadata(&self.instance, &session, &id, &kind, &metadata)?
                else {
                    continue;
                };
                if !range.contains(fact.at) {
                    continue;
                }
                fact.validate()?;
                preview.requests += 1;
                preview.failures += fact.failures;
                if fact.priced {
                    preview.priced += 1;
                } else {
                    preview.unpriced += 1;
                }
                days.insert(fact.day.clone());
                if preview.samples.len() < samples {
                    preview.samples.push(fact.clone());
                }
                visit(fact)?;
            }
        }
        preview.days = days.into_iter().collect();
        if incomplete > 0 {
            preview.warnings.push(format!(
                "{incomplete} unfinished message(s) deferred until the next import"
            ));
        }
        Ok(preview)
    }
}

fn fact_from_metadata(
    instance: &str,
    session: &str,
    id: &str,
    kind: &str,
    value: &Value,
) -> Result<Option<ImportedFact>, String> {
    if id.is_empty() || session.is_empty() {
        return Err("OpenCode message has no stable identity".into());
    }
    let tokens = &value["tokens"];
    let count = |value: &Value, optional: bool| -> Result<u32, String> {
        if value.is_null() && optional {
            return Ok(0);
        }
        value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| "invalid OpenCode token count".into())
    };
    let input = count(&tokens["input"], false)?;
    let output = count(&tokens["output"], false)?;
    let reasoning = count(&tokens["reasoning"], true)?;
    let read = count(&tokens["cache"]["read"], true)?;
    let write = count(&tokens["cache"]["write"], true)?;
    let input_tokens = normalise_input(input, read, write, InputSemantics::Fresh, false)?;
    // OpenCode stores visible output separately from reasoning. The destination
    // stores reasoning inside output, exactly once.
    let output_tokens = output
        .checked_add(reasoning)
        .ok_or("OpenCode output count overflow")?;
    if input_tokens == 0 && output_tokens == 0 {
        return Ok(None);
    }
    let created = value["time"]["created"]
        .as_i64()
        .ok_or("OpenCode usage has no timestamp")?;
    let at = Utc
        .timestamp_millis_opt(created)
        .single()
        .ok_or("invalid OpenCode timestamp")?;
    let amount = value["cost"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0);
    let reported_cost = amount.map(|amount| Cost {
        currency: "USD".into(),
        amount,
    });
    let cost = reported_cost.clone().filter(|cost| cost.amount > 0.0);
    let model = value["modelID"]
        .as_str()
        .or_else(|| value["model"].as_str())
        .or_else(|| value["model"]["id"].as_str())
        .map(str::to_string);
    let failed = value["failed"].as_i64() == Some(1)
        || value["status"] == "failed"
        || value["finish"] == "error";
    Ok(Some(ImportedFact {
        source: SOURCE.into(),
        instance: instance.into(),
        entity_id: format!("opencode_session:{session}:{id}"),
        revision: String::new(),
        granularity: Granularity::Request,
        at,
        day: local_day(at),
        client: Some("opencode".into()),
        provider_id: value["providerID"]
            .as_str()
            .or_else(|| value["model"]["providerID"].as_str())
            .map(str::to_string),
        model,
        request_model: None,
        pricing_model: None,
        tokens: Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens: read,
            cache_write_tokens: write,
            reasoning_tokens: reasoning,
        },
        input_semantics: Some(InputSemantics::Fresh),
        priced: cost.is_some(),
        cost,
        reported_cost,
        latency_ms: None,
        ttft_ms: None,
        status: None,
        ok: if failed { Some(false) } else { None },
        streaming: false,
        requests: 1,
        failures: u64::from(failed),
        data_source: if kind == "compaction" {
            "compaction"
        } else {
            "opencode_session"
        }
        .into(),
        duplicate: Duplicate::None,
        duplicate_of: None,
        multiplier: None,
        error: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mixed_layout_prefers_migrated_messages_and_updates_late_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL;
          CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
          CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);").unwrap();
        let mut metadata = json!({"role":"assistant","modelID":"m","time":{"created":1767348000000i64,"completed":1767348001000i64},
            "tokens":{"input":10,"output":3,"reasoning":2,"cache":{"read":4,"write":1}},"cost":0.2,
            "content":"private source content"});
        db.execute(
            "INSERT INTO message VALUES ('m1','s',1,1,?1)",
            [metadata.to_string()],
        )
        .unwrap();
        metadata["tokens"]["output"] = json!(7);
        metadata["model"] = json!({"id":"m-v2","providerID":"p"});
        metadata.as_object_mut().unwrap().remove("modelID");
        db.execute(
            "INSERT INTO session_message VALUES ('m1','s','assistant',1,2,?1)",
            [metadata.to_string()],
        )
        .unwrap();
        let source = OpenCodeSource::open(&path, "laptop").unwrap();
        let store = RuntimeStore::open(&dir.path().join("runtime.db")).unwrap();
        let preview = source
            .stage(
                &store,
                &ImportRange::default(),
                &uuid::Uuid::new_v4().to_string(),
                &ImportCancel::new(),
            )
            .unwrap();
        assert_eq!(preview.requests, 1);
        assert_eq!(store.imported_totals(None, None).unwrap().requests, 0);
        finish_import(&store, &preview, &ImportCancel::new()).unwrap();
        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.input_tokens, 15);
        assert_eq!(totals.output_tokens, 9);
        metadata["tokens"]["output"] = json!(11);
        db.execute(
            "UPDATE session_message SET data = ?1",
            [metadata.to_string()],
        )
        .unwrap();
        assert_eq!(
            source
                .import(&store, &ImportRange::default(), 500, &ImportCancel::new())
                .unwrap()
                .updated,
            1
        );
        assert_eq!(store.imported_totals(None, None).unwrap().output_tokens, 13);
        let destination = Connection::open(dir.path().join("runtime.db")).unwrap();
        let record: String = destination
            .query_row("SELECT record FROM imported_fact", [], |row| row.get(0))
            .unwrap();
        assert!(!record.contains("private source content"));
        assert_eq!(
            serde_json::from_str::<Value>(&record).unwrap()["model"],
            "m-v2"
        );
    }

    #[test]
    fn zero_cost_and_compaction_preserve_missing_fields() {
        let value = json!({"time":{"created":1767348000000i64},"status":"failed","cost":0,
            "tokens":{"input":5,"output":0,"reasoning":2,"cache":{"read":1,"write":0}}});
        let fact = fact_from_metadata("copy", "s", "c", "compaction", &value)
            .unwrap()
            .unwrap();
        assert!(!fact.priced);
        assert_eq!(fact.reported_cost.unwrap().amount, 0.0);
        assert_eq!(fact.tokens.output_tokens, 2);
        assert_eq!(fact.failures, 1);
        assert_eq!(fact.status, None);
        assert_eq!(fact.latency_ms, None);
        assert_eq!(fact.error, None);
    }

    #[test]
    fn a_confirmed_snapshot_does_not_reopen_the_changed_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);").unwrap();
        let metadata = json!({"role":"assistant","time":{"created":1767348000000i64,"completed":1767348001000i64},
            "tokens":{"input":10,"output":3},"cost":0});
        db.execute(
            "INSERT INTO message VALUES ('m1','s',1,1,?1)",
            [metadata.to_string()],
        )
        .unwrap();
        let source = OpenCodeSource::open(&path, "copy").unwrap();
        let store = RuntimeStore::open(&dir.path().join("runtime.db")).unwrap();
        let preview = source
            .stage(
                &store,
                &ImportRange::default(),
                &uuid::Uuid::new_v4().to_string(),
                &ImportCancel::new(),
            )
            .unwrap();
        db.execute("DELETE FROM message", []).unwrap();
        assert_eq!(
            finish_import(&store, &preview, &ImportCancel::new())
                .unwrap()
                .inserted,
            1
        );
        assert_eq!(store.imported_totals(None, None).unwrap().input_tokens, 10);
    }
}
