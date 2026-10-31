//! The chart, headline and paged durable history share accounting semantics.

use std::sync::Arc;

use zroutery_core::accounting::{PersistenceState, RequestAccounting, UsageTotals};
use zroutery_core::billing::{Cost, CostTotals};
use zroutery_core::client::ClientId;
use zroutery_core::ir::{Dialect, Usage};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::stats::{RecordBuilder, Stats, UsageHistoryQuery};

#[test]
fn cache_cost_and_partial_failures_survive_ring_eviction_reopen_and_clear() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.db");
    let stats = Stats::new(1);
    stats
        .attach_store(Arc::new(RuntimeStore::open(&path).unwrap()))
        .unwrap();
    let mut builder = RecordBuilder::new(Dialect::OpenAI, "standard-class", false);
    builder.resolved("p-model", "Provider").usage(Usage {
        input_tokens: 100,
        output_tokens: 10,
        cache_read_tokens: 20,
        cache_write_tokens: 5,
        ..Default::default()
    });
    let mut legacy = builder.finish(10);
    legacy.client.client_id = ClientId::trusted("codex");
    legacy.cost = Some(Cost {
        currency: "CNY".into(),
        amount: 0.125,
    });
    stats.record(legacy);

    let mut partial = builder.finish(20);
    partial.id = "multi-attempt".into();
    partial.client.client_id = ClientId::trusted("claude_code");
    partial.ok = false;
    // Final-attempt-only values must not overwrite the durable total or count
    // alongside it. Known failed-attempt spend remains visible in both views.
    partial.cost = Some(Cost {
        currency: "USD".into(),
        amount: 999.0,
    });
    let mut known_cost = CostTotals::default();
    known_cost.add(&Cost {
        currency: "USD".into(),
        amount: 0.000_000_012_5,
    });
    partial.accounting = Some(RequestAccounting {
        schema_version: 1,
        persistence: PersistenceState::Durable,
        attempt_ids: vec!["failed".into(), "unknown".into()],
        known_cost,
        unknown_cost_attempts: 1,
        observed_usage: UsageTotals {
            input_tokens: 200,
            output_tokens: 30,
            cache_read_tokens: 80,
            cache_write_tokens: 20,
            reasoning_tokens: 10,
        },
        unknown_usage_attempts: 1,
    });
    stats.record(partial);
    stats.flush().unwrap();
    let reopened = Stats::new(1);
    reopened
        .attach_store(Arc::new(RuntimeStore::open(&path).unwrap()))
        .unwrap();
    for stats in [&stats, &reopened] {
        let summary = stats.summary();
        assert_eq!(summary.requests, 2);
        assert_eq!(summary.input_tokens, 300);
        assert_eq!(summary.output_tokens, 40);
        assert_eq!(summary.cache_read_tokens, 100);
        assert_eq!(summary.cache_write_tokens, 25);
        assert_eq!(summary.unpriced_requests, 1);
        assert_eq!(summary.cost.get("CNY"), 0.125);
        assert_eq!(summary.cost.get("USD"), 0.000_000_012_5);
        assert_eq!(
            summary
                .per_client
                .iter()
                .map(|client| client.cache_read_tokens)
                .sum::<u64>(),
            100
        );
        let day = stats.usage_days(1).unwrap().remove(0);
        assert_eq!(day.requests, summary.requests);
        assert_eq!(day.input_tokens, summary.input_tokens);
        assert_eq!(day.cache_read_tokens, summary.cache_read_tokens);
        assert_eq!(day.cache_write_tokens, summary.cache_write_tokens);
        assert_eq!(day.cost, summary.cost);
        assert_eq!(day.unpriced_requests, summary.unpriced_requests);
        let page = stats.query_history(&UsageHistoryQuery::default()).unwrap();
        assert_eq!(page.total, 2, "history is not the one-row recent ring");
        assert_eq!(page.records.len(), 2);
    }
    reopened.clear_history(Some("claude_code")).unwrap();
    let remaining = reopened.summary();
    assert_eq!(remaining.requests, 1);
    assert_eq!(remaining.unpriced_requests, 0);
    assert_eq!(remaining.cache_read_tokens, 20);
    assert!(!remaining.cost.0.contains_key("USD"));
    assert_eq!(reopened.usage_days(1).unwrap()[0].cost, remaining.cost);
    let mut archive = Vec::new();
    reopened.write_history_archive(&mut archive).unwrap();
    let archive: serde_json::Value = serde_json::from_slice(&archive).unwrap();
    assert_eq!(archive["schema_version"], 1);
    assert_eq!(archive["records"].as_array().unwrap().len(), 1);
    assert_eq!(archive["records"][0]["client"]["client_id"], "codex");
}

#[test]
fn memory_pages_reject_a_stale_revision_and_keep_historical_ids() {
    let stats = Stats::new(2);
    let mut record = RecordBuilder::new(Dialect::OpenAI, "m", false).finish(1);
    record.client.client_id = ClientId::trusted("codex_desktop_legacy_unknow");
    stats.record(record);
    let page = stats.query_history(&Default::default()).unwrap();
    assert_eq!(
        page.records[0].client.client_id.as_str(),
        "codex_desktop_legacy_unknow"
    );
    stats.clear_history(None).unwrap();
    let query = UsageHistoryQuery {
        expected_revision: Some(page.revision),
        ..Default::default()
    };
    assert!(stats.query_history(&query).unwrap_err().contains("changed"));
    assert_eq!(stats.summary().cache_read_tokens, 0);
    assert_eq!(stats.summary().cache_write_tokens, 0);
}

#[test]
fn invalid_imported_attempt_totals_are_rejected_before_any_row_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Stats::new(1);
    stats
        .attach_store(Arc::new(
            RuntimeStore::open(&dir.path().join("history.db")).unwrap(),
        ))
        .unwrap();
    let mut record = RecordBuilder::new(Dialect::OpenAI, "m", false).finish(1);
    record.accounting = Some(RequestAccounting {
        schema_version: 1,
        persistence: PersistenceState::Durable,
        attempt_ids: vec!["attempt".into()],
        known_cost: CostTotals::default(),
        unknown_cost_attempts: 0,
        observed_usage: UsageTotals {
            input_tokens: 10,
            cache_read_tokens: 11,
            ..Default::default()
        },
        unknown_usage_attempts: 0,
    });
    assert!(stats.import_history(vec![record.clone()]).is_err());
    let accounting = record.accounting.as_mut().unwrap();
    accounting.observed_usage.cache_read_tokens = 0;
    accounting.known_cost.add(&Cost {
        currency: "USD".into(),
        amount: -1.0,
    });
    assert!(stats.import_history(vec![record.clone()]).is_err());
    record.accounting.as_mut().unwrap().known_cost = CostTotals::default();
    record.accounting.as_mut().unwrap().known_cost.add(&Cost {
        currency: "usd ".into(),
        amount: 0.1,
    });
    assert!(
        stats.import_history(vec![record]).is_err(),
        "imports cannot split a currency into spelling variants"
    );
    assert_eq!(stats.query_history(&Default::default()).unwrap().total, 0);
}
