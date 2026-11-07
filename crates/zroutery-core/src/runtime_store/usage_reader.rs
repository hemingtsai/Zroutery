//! Read-only access to another Zroutery instance's runtime usage database.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::stats::{RequestRecord, Stats, StatsSummary, UsageDay};

pub struct UsageReader {
    path: PathBuf,
    store: Arc<super::RuntimeStore>,
    stats: Stats,
    cached: Mutex<Option<(i64, Instant, u32, UsageSourceSnapshot)>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageSourceSnapshot {
    pub source_name: String,
    pub revision: i64,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
    pub days: Vec<UsageDay>,
}

impl std::fmt::Debug for UsageReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageReader")
            .field("path", &self.path)
            .finish()
    }
}

impl UsageReader {
    /// Open an existing runtime database without running migrations or enabling writes.
    pub fn open(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Err("the selected file is not a runtime database".into());
        }
        let path = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve the selected runtime database: {error}"))?;
        let store =
            Arc::new(super::RuntimeStore::open_read_only(&path).map_err(|error| {
                format!("cannot open standby usage database read-only: {error}")
            })?);
        let stats = Stats::new(200);
        stats.attach_store(Arc::clone(&store))?;
        Ok(Self {
            path,
            store,
            stats,
            cached: Mutex::new(None),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_same_database(&self, other: &Path) -> bool {
        other.canonicalize().is_ok_and(|path| path == self.path)
    }

    pub fn snapshot(&self, days: u32) -> Result<UsageSourceSnapshot, String> {
        let days = days.clamp(1, 400);
        for _ in 0..3 {
            let observed_revision = self.store.usage_revision()?;
            if let Some((revision, cached_at, cached_days, snapshot)) =
                crate::sync::lock(&self.cached).as_ref()
            {
                if *revision == observed_revision
                    && *cached_days == days
                    && cached_at.elapsed() < Duration::from_secs(10)
                {
                    return Ok(snapshot.clone());
                }
            }
            self.stats.refresh_history()?;
            let today = chrono::Local::now().date_naive();
            let first = today - chrono::Duration::days(i64::from(days) - 1);
            let summary = self.stats.summary();
            let recent = self.stats.recent(200);
            let day_rows = self.store.usage_days(first, today)?;
            let revision = self.store.usage_revision()?;
            if revision != observed_revision {
                continue;
            }
            let snapshot = UsageSourceSnapshot {
                source_name: self
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("runtime.sqlite3")
                    .to_string(),
                revision,
                summary,
                recent,
                days: day_rows,
            };
            *crate::sync::lock(&self.cached) =
                Some((revision, Instant::now(), days, snapshot.clone()));
            return Ok(snapshot);
        }
        Err("standby usage changed while it was being read; retry".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::RecordBuilder;

    #[test]
    fn a_schema_four_database_is_read_without_migrating_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        {
            let store = super::super::RuntimeStore::open(&path).unwrap();
            let mut record = RecordBuilder::new(crate::ir::Dialect::OpenAI, "gpt", false).finish(4);
            record.id = "v4-1".into();
            record.usage.input_tokens = 11;
            store.append_usage(&[record]).unwrap();
        }
        // A file written before imported history existed has no imported
        // tables. The usage side is unchanged, so the reader must read it
        // rather than demand a migration it is forbidden to perform.
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(
                "DROP TABLE imported_fact; DROP TABLE imported_revision; PRAGMA user_version = 4;",
            )
            .unwrap();
        }

        let reader = UsageReader::open(&path).expect("schema 4 is readable read-only");
        let snapshot = reader.snapshot(30).unwrap();
        assert_eq!(snapshot.summary.requests, 1);
        assert_eq!(snapshot.summary.input_tokens, 11);
        let version: i64 = reader
            .store
            .db
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4, "reading did not migrate the file");
    }

    #[test]
    fn reads_live_database_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let store = super::super::RuntimeStore::open(&path).unwrap();
        let mut record = RecordBuilder::new(crate::ir::Dialect::OpenAI, "gpt", false).finish(4);
        record.id = "standby-1".into();
        record.usage.input_tokens = 17;
        store.append_usage(&[record]).unwrap();

        let reader = UsageReader::open(&path).unwrap();
        let snapshot = reader.snapshot(30).unwrap();
        assert_eq!(snapshot.summary.input_tokens, 17);
        assert_eq!(snapshot.recent[0].id, "standby-1");
        assert!(reader.store.clear_usage(None).is_err());

        let mut live = RecordBuilder::new(crate::ir::Dialect::OpenAI, "gpt", false).finish(4);
        live.id = "standby-2".into();
        live.usage.input_tokens = 23;
        store.append_usage(&[live]).unwrap();
        assert_eq!(reader.snapshot(30).unwrap().summary.input_tokens, 40);
    }
}
