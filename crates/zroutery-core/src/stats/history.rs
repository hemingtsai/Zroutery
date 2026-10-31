//! Bounded history reads. A revision ties offset pages to one unchanged set;
//! inserts and deletes require a fresh first page instead of silently skipping.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{RequestRecord, Stats};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageHistoryQuery {
    pub page: u32,
    pub page_size: u32,
    pub client_id: Option<String>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    /// Inclusive UTC instant.
    pub from: Option<DateTime<Utc>>,
    /// Exclusive UTC instant.
    pub until: Option<DateTime<Utc>>,
    pub failures_only: bool,
    pub expected_revision: Option<i64>,
}

impl Default for UsageHistoryQuery {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: 50,
            client_id: None,
            provider_id: None,
            model_id: None,
            from: None,
            until: None,
            failures_only: false,
            expected_revision: None,
        }
    }
}

impl UsageHistoryQuery {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self
            .from
            .zip(self.until)
            .is_some_and(|(from, until)| from >= until)
        {
            return Err("history start must precede its exclusive end".into());
        }
        if [&self.client_id, &self.provider_id, &self.model_id]
            .into_iter()
            .filter_map(|value| value.as_ref())
            .any(|value| value.len() > 512)
        {
            return Err("history filter exceeds 512 bytes".into());
        }
        if self.expected_revision.is_some_and(|revision| revision < 0) {
            return Err("history revision cannot be negative".into());
        }
        Ok(())
    }

    pub(crate) fn check_revision(&self, revision: i64) -> Result<(), String> {
        if self
            .expected_revision
            .is_some_and(|expected| expected != revision)
        {
            return Err("usage history changed; refresh from the first page".into());
        }
        Ok(())
    }

    pub(crate) fn pagination(&self, total: u64) -> (u32, u32, u64) {
        let size = self.page_size.clamp(1, 200);
        let last = total.div_ceil(u64::from(size)).max(1);
        let page = u64::from(self.page.max(1)).min(last) as u32;
        (page, size, u64::from(page - 1) * u64::from(size))
    }

    fn matches(&self, record: &RequestRecord) -> bool {
        self.client_id
            .as_deref()
            .is_none_or(|client| record.client.client_id.as_str() == client)
            && self
                .provider_id
                .as_deref()
                .is_none_or(|provider| record.provider_id.as_deref() == Some(provider))
            && self
                .model_id
                .as_deref()
                .is_none_or(|model| record.resolved_model.as_deref() == Some(model))
            && self.from.is_none_or(|from| record.at >= from)
            && self.until.is_none_or(|until| record.at < until)
            && (!self.failures_only || !record.ok)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageHistoryPage {
    pub records: Vec<RequestRecord>,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub revision: i64,
}

impl Stats {
    /// Stream the portable archive without materializing all historical rows.
    /// SQLite holds one read snapshot; live diagnostic text never enters it.
    pub fn write_history_archive(&self, mut writer: impl std::io::Write) -> Result<(), String> {
        let mut inner = crate::sync::lock(&self.inner);
        Self::flush_pending(&mut inner)?;
        if !inner.settlement_pending.is_empty() {
            return Err("usage settlement is pending; retry after accounting recovery".into());
        }
        let store = inner.store.clone();
        let recent = if store.is_none() {
            inner.records.iter().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        drop(inner);
        writer
            .write_all(b"{\"schema_version\":1,\"records\":[")
            .map_err(|error| error.to_string())?;
        let mut first = true;
        let mut write = |mut record: RequestRecord| {
            record.error = None;
            record.routing_decision = None;
            if !first {
                writer.write_all(b",").map_err(|error| error.to_string())?;
            }
            first = false;
            serde_json::to_writer(&mut writer, &record).map_err(|error| error.to_string())
        };
        if let Some(store) = store {
            store.visit_usage(&mut write)?;
        } else {
            for record in recent {
                write(record)?;
            }
        }
        writer.write_all(b"]}\n").map_err(|error| error.to_string())
    }

    pub fn query_history(&self, query: &UsageHistoryQuery) -> Result<UsageHistoryPage, String> {
        query.validate()?;
        let mut inner = crate::sync::lock(&self.inner);
        Self::flush_pending(&mut inner)?;
        if inner.store.is_some() && !inner.settlement_pending.is_empty() {
            return Err("usage settlement is pending; retry after accounting recovery".into());
        }
        if let Some(store) = inner.store.clone() {
            drop(inner);
            return store.query_usage(query);
        }
        let revision = inner.local_revision;
        query.check_revision(revision)?;
        let mut records: Vec<_> = inner
            .records
            .iter()
            .filter(|record| query.matches(record))
            .cloned()
            .collect();
        records.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
        let total = records.len() as u64;
        let (page, page_size, offset) = query.pagination(total);
        Ok(UsageHistoryPage {
            records: records
                .into_iter()
                .skip(offset as usize)
                .take(page_size as usize)
                .collect(),
            page,
            page_size,
            total,
            revision,
        })
    }
}
