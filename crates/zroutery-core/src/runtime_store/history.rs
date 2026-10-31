//! Indexed, snapshot-consistent history queries. Only one page crosses IPC;
//! day charts stream the requested time window instead of loading all history.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::{params, params_from_iter, types::Value};

use crate::stats::{RequestRecord, UsageDay, UsageHistoryPage, UsageHistoryQuery};

use super::{usage_timestamp, RuntimeStore};

impl RuntimeStore {
    pub(crate) fn visit_usage(
        &self,
        visit: &mut impl FnMut(RequestRecord) -> Result<(), String>,
    ) -> Result<(), String> {
        let db = crate::sync::lock(&self.db);
        let mut statement = db
            .prepare("SELECT record FROM usage ORDER BY at, id")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        for row in rows {
            let json = row.map_err(|error| error.to_string())?;
            visit(serde_json::from_str(&json).map_err(|error| error.to_string())?)?;
        }
        Ok(())
    }

    pub fn query_usage(&self, query: &UsageHistoryQuery) -> Result<UsageHistoryPage, String> {
        query.validate()?;
        let mut db = crate::sync::lock(&self.db);
        let tx = db.transaction().map_err(|error| error.to_string())?;
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM usage_revision WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        query.check_revision(revision)?;
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        for (column, value) in [
            ("client", query.client_id.as_ref()),
            (
                "json_extract(record, '$.provider_id')",
                query.provider_id.as_ref(),
            ),
            (
                "json_extract(record, '$.resolved_model')",
                query.model_id.as_ref(),
            ),
        ] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                values.push(value.clone().into());
            }
        }
        if let Some(from) = &query.from {
            clauses.push("at >= ?".into());
            values.push(usage_timestamp(from).into());
        }
        if let Some(until) = &query.until {
            clauses.push("at < ?".into());
            values.push(usage_timestamp(until).into());
        }
        if query.failures_only {
            clauses.push("json_extract(record, '$.ok') = 0".into());
        }
        let condition = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let total: i64 = tx
            .query_row(
                &format!("SELECT COUNT(*) FROM usage{condition}"),
                params_from_iter(&values),
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let total = u64::try_from(total).map_err(|_| "invalid negative history count")?;
        let (page, page_size, offset) = query.pagination(total);
        values.push(i64::from(page_size).into());
        values.push(
            i64::try_from(offset)
                .map_err(|_| "history page offset exceeds SQLite's range")?
                .into(),
        );
        let mut statement = tx
            .prepare(&format!(
                "SELECT record FROM usage{condition} ORDER BY at DESC, id DESC LIMIT ? OFFSET ?"
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
        Ok(UsageHistoryPage {
            records,
            page,
            page_size,
            total,
            revision,
        })
    }

    pub(crate) fn usage_days(
        &self,
        first: NaiveDate,
        today: NaiveDate,
    ) -> Result<Vec<UsageDay>, String> {
        let mut days = BTreeMap::new();
        let mut date = first;
        while date <= today {
            days.insert(
                date,
                UsageDay {
                    day: date.format("%Y-%m-%d").to_string(),
                    ..Default::default()
                },
            );
            date += chrono::Duration::days(1);
        }
        // Widen UTC bounds by a day before doing the exact Local conversion.
        // This handles DST and zones whose midnight is ambiguous/nonexistent.
        let from = (first - chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let until = (today + chrono::Duration::days(2))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let db = crate::sync::lock(&self.db);
        let mut statement = db
            .prepare("SELECT record FROM usage WHERE at >= ?1 AND at < ?2 ORDER BY at, id")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![usage_timestamp(&from), usage_timestamp(&until)],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| error.to_string())?;
        for row in rows {
            let json = row.map_err(|error| error.to_string())?;
            let record: RequestRecord =
                serde_json::from_str(&json).map_err(|error| error.to_string())?;
            if let Some(day) = days.get_mut(&record.at.with_timezone(&chrono::Local).date_naive()) {
                day.observe(&record);
            }
        }
        Ok(days.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::RecordBuilder;
    use chrono::Utc;

    fn record(id: &str, at: &str) -> RequestRecord {
        let mut record =
            RecordBuilder::new(crate::ir::Dialect::OpenAI, "standard-class", false).finish(20);
        record.id = id.into();
        record.at = at.parse::<chrono::DateTime<Utc>>().unwrap();
        record
    }

    #[test]
    fn pages_share_a_revision_and_nanosecond_order_with_filters() {
        let dir = tempfile::tempdir().unwrap();
        let store = RuntimeStore::open(&dir.path().join("history.db")).unwrap();
        let mut a = record("a", "2026-10-31T00:00:00Z");
        a.provider_id = Some("p".into());
        a.resolved_model = Some("m".into());
        a.client.client_id = crate::client::ClientId::trusted("codex");
        a.ok = false;
        let mut b = a.clone();
        b.id = "b".into();
        b.at = "2026-10-31T00:00:00.000000001Z".parse().unwrap();
        let mut c = b.clone();
        c.id = "c".into();
        store.append_usage(&[a.clone(), b, c]).unwrap();
        let mut query = UsageHistoryQuery {
            page_size: 1,
            client_id: Some("codex".into()),
            provider_id: Some("p".into()),
            model_id: Some("m".into()),
            from: Some(a.at),
            until: Some("2026-10-31T00:00:01Z".parse().unwrap()),
            failures_only: true,
            ..Default::default()
        };
        let page = store.query_usage(&query).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.records[0].id, "c");
        query.expected_revision = Some(page.revision);
        query.page = 2;
        assert_eq!(store.query_usage(&query).unwrap().records[0].id, "b");
        query.page = 3;
        assert_eq!(store.query_usage(&query).unwrap().records[0].id, "a");
        query.page_size = u32::MAX;
        query.until = Some(a.at);
        assert!(
            store.query_usage(&query).is_err(),
            "empty/inverted windows are invalid"
        );
        query.from = None;
        let empty = store.query_usage(&query).unwrap();
        assert_eq!(empty.total, 0, "exclusive bound excludes a");
        assert_eq!(empty.page_size, 200);
        assert_eq!(empty.page, 1);
        store.clear_usage(None).unwrap();
        assert!(store.query_usage(&query).unwrap_err().contains("changed"));
    }

    #[test]
    fn migrating_keys_preserves_records_and_queries_use_the_time_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        drop(RuntimeStore::open(&path).unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        let legacy = record("kept", "2026-10-30T20:00:00.123456789Z");
        let json = serde_json::to_string(&legacy).unwrap();
        db.execute("INSERT INTO usage VALUES ('kept', 'unknown', '2026-10-31T04:00:00.123456789+08:00', ?1)", [&json]).unwrap();
        db.execute_batch("DROP INDEX usage_client_time; DROP INDEX usage_provider_time; DROP INDEX usage_model_time; PRAGMA user_version=3;").unwrap();
        drop(db);
        let store = RuntimeStore::open(&path).unwrap();
        let db = crate::sync::lock(&store.db);
        assert_eq!(
            db.query_row("SELECT record FROM usage", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            json
        );
        assert_eq!(
            db.query_row("SELECT at FROM usage", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "2026-10-30T20:00:00.123456789Z"
        );
        let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT record FROM usage WHERE at >= '2026' AND at < '2027' ORDER BY at, id", [], |row| row.get(3)).unwrap();
        assert!(plan.contains("usage_time"), "{plan}");
    }
}
