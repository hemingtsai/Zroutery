//! In-memory request log and token accounting for the GUI.
//!
//! Nothing is written to disk: the log is a bounded ring buffer, so prompts and
//! completions never outlive the process.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ir::{Dialect, Usage};

/// One completed (or failed) client request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub id: String,
    pub at: DateTime<Utc>,
    /// Which API the client used.
    pub ingress: Dialect,
    /// What the client asked for, e.g. `sonnet-class`.
    pub requested_model: String,
    /// Which concrete model answered, if any.
    pub resolved_model: Option<String>,
    pub provider_name: Option<String>,
    pub stream: bool,
    pub status: u16,
    pub ok: bool,
    pub error: Option<String>,
    pub latency_ms: u64,
    /// Time to first streamed token.
    pub ttft_ms: Option<u64>,
    pub usage: Usage,
    /// How many upstream attempts it took (>1 means failover happened).
    pub attempts: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelTotals {
    pub model_id: String,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_tokens: u64,
    /// Average end to end latency in milliseconds.
    pub avg_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummary {
    pub since: DateTime<Utc>,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub per_model: Vec<ModelTotals>,
}

#[derive(Debug)]
pub struct Stats {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    limit: usize,
    since: DateTime<Utc>,
    records: VecDeque<RequestRecord>,
    per_model: BTreeMap<String, ModelTotals>,
    requests: u64,
    failures: u64,
    input_tokens: u64,
    output_tokens: u64,
}

impl Stats {
    pub fn new(limit: usize) -> Self {
        Stats {
            inner: Mutex::new(Inner {
                limit: limit.max(1),
                since: Utc::now(),
                records: VecDeque::new(),
                per_model: BTreeMap::new(),
                requests: 0,
                failures: 0,
                input_tokens: 0,
                output_tokens: 0,
            }),
        }
    }

    pub fn set_limit(&self, limit: usize) {
        let mut inner = self.inner.lock().expect("stats poisoned");
        inner.limit = limit.max(1);
        while inner.records.len() > inner.limit {
            inner.records.pop_front();
        }
    }

    pub fn record(&self, record: RequestRecord) {
        let mut inner = self.inner.lock().expect("stats poisoned");
        inner.requests += 1;
        if !record.ok {
            inner.failures += 1;
        }
        inner.input_tokens += record.usage.input_tokens as u64;
        inner.output_tokens += record.usage.output_tokens as u64;

        let key = record
            .resolved_model
            .clone()
            .unwrap_or_else(|| record.requested_model.clone());
        let totals = inner
            .per_model
            .entry(key.clone())
            .or_insert_with(|| ModelTotals {
                model_id: key,
                ..ModelTotals::default()
            });
        totals.requests += 1;
        if !record.ok {
            totals.failures += 1;
        }
        totals.input_tokens += record.usage.input_tokens as u64;
        totals.output_tokens += record.usage.output_tokens as u64;
        totals.reasoning_tokens += record.usage.reasoning_tokens as u64;
        totals.cached_tokens += record.usage.cache_read_tokens as u64;
        let n = totals.requests as f64;
        totals.avg_latency_ms += (record.latency_ms as f64 - totals.avg_latency_ms) / n;

        let limit = inner.limit;
        inner.records.push_back(record);
        while inner.records.len() > limit {
            inner.records.pop_front();
        }
    }

    /// Most recent requests first.
    pub fn recent(&self, limit: usize) -> Vec<RequestRecord> {
        let inner = self.inner.lock().expect("stats poisoned");
        inner.records.iter().rev().take(limit).cloned().collect()
    }

    pub fn summary(&self) -> StatsSummary {
        let inner = self.inner.lock().expect("stats poisoned");
        StatsSummary {
            since: inner.since,
            requests: inner.requests,
            failures: inner.failures,
            input_tokens: inner.input_tokens,
            output_tokens: inner.output_tokens,
            per_model: inner.per_model.values().cloned().collect(),
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().expect("stats poisoned");
        inner.records.clear();
        inner.per_model.clear();
        inner.requests = 0;
        inner.failures = 0;
        inner.input_tokens = 0;
        inner.output_tokens = 0;
        inner.since = Utc::now();
    }
}

impl Default for Stats {
    fn default() -> Self {
        Stats::new(500)
    }
}

/// Accumulates the facts about one in-flight request.
pub struct RecordBuilder {
    record: RequestRecord,
}

impl RecordBuilder {
    pub fn new(ingress: Dialect, requested_model: &str, stream: bool) -> Self {
        RecordBuilder {
            record: RequestRecord {
                id: format!("req_{}", uuid::Uuid::new_v4().simple()),
                at: Utc::now(),
                ingress,
                requested_model: requested_model.to_string(),
                resolved_model: None,
                provider_name: None,
                stream,
                status: 200,
                ok: true,
                error: None,
                latency_ms: 0,
                ttft_ms: None,
                usage: Usage::default(),
                attempts: 0,
            },
        }
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn resolved(&mut self, model_id: &str, provider_name: &str) -> &mut Self {
        self.record.resolved_model = Some(model_id.to_string());
        self.record.provider_name = Some(provider_name.to_string());
        self
    }

    pub fn attempt(&mut self) -> &mut Self {
        self.record.attempts += 1;
        self
    }

    pub fn usage(&mut self, usage: Usage) -> &mut Self {
        self.record.usage = usage;
        self
    }

    pub fn ttft(&mut self, ms: u64) -> &mut Self {
        if self.record.ttft_ms.is_none() {
            self.record.ttft_ms = Some(ms);
        }
        self
    }

    pub fn ok(&mut self) -> &mut Self {
        self.record.ok = true;
        self.record.status = 200;
        self.record.error = None;
        self
    }

    pub fn fail(&mut self, status: u16, error: String) -> &mut Self {
        self.record.ok = false;
        self.record.status = status;
        self.record.error = Some(error);
        self
    }

    pub fn finish(mut self, latency_ms: u64) -> RequestRecord {
        self.record.latency_ms = latency_ms;
        self.record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(model: &str, ok: bool, latency: u64, usage: Usage) -> RequestRecord {
        let mut b = RecordBuilder::new(Dialect::Anthropic, "sonnet-class", false);
        b.resolved(model, "DeepSeek").usage(usage).attempt();
        if !ok {
            b.fail(502, "boom".into());
        }
        b.finish(latency)
    }

    #[test]
    fn ring_buffer_is_bounded_and_newest_first() {
        let stats = Stats::new(2);
        for i in 0..5 {
            stats.record(rec(&format!("m{i}"), true, 10, Usage::default()));
        }
        let recent = stats.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].resolved_model.as_deref(), Some("m4"));
        assert_eq!(recent[1].resolved_model.as_deref(), Some("m3"));
        // aggregates still count every request
        assert_eq!(stats.summary().requests, 5);
    }

    #[test]
    fn aggregates_tokens_failures_and_latency() {
        let stats = Stats::new(10);
        stats.record(rec(
            "deepseek-v4-pro",
            true,
            100,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                reasoning_tokens: 2,
                ..Usage::default()
            },
        ));
        stats.record(rec(
            "deepseek-v4-pro",
            false,
            300,
            Usage {
                input_tokens: 1,
                output_tokens: 0,
                ..Usage::default()
            },
        ));
        let s = stats.summary();
        assert_eq!(s.requests, 2);
        assert_eq!(s.failures, 1);
        assert_eq!(s.input_tokens, 11);
        assert_eq!(s.output_tokens, 5);
        let m = &s.per_model[0];
        assert_eq!(m.model_id, "deepseek-v4-pro");
        assert_eq!(m.requests, 2);
        assert_eq!(m.failures, 1);
        assert_eq!(m.reasoning_tokens, 2);
        assert!((m.avg_latency_ms - 200.0).abs() < 0.001);
    }

    #[test]
    fn clear_resets_everything() {
        let stats = Stats::new(4);
        stats.record(rec("m", true, 1, Usage::default()));
        stats.clear();
        assert!(stats.recent(4).is_empty());
        assert_eq!(stats.summary().requests, 0);
        assert!(stats.summary().per_model.is_empty());
    }

    #[test]
    fn builder_marks_failures_and_ttft() {
        let mut b = RecordBuilder::new(Dialect::OpenAI, "gpt-5.3-sol", true);
        b.attempt();
        b.attempt();
        b.ttft(42);
        b.ttft(99);
        b.fail(429, "rate limited".into());
        let r = b.finish(1234);
        assert_eq!(r.attempts, 2);
        assert_eq!(r.ttft_ms, Some(42));
        assert!(!r.ok);
        assert_eq!(r.status, 429);
        assert_eq!(r.latency_ms, 1234);
        assert!(r.id.starts_with("req_"));
    }
}
