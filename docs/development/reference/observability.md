# Observability Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page describes the request log, the health view, the activity projection and
the sessions kept in memory. It does not describe accounting; spend is durable
and covered by [accounting.md](accounting.md).

## Request records

A request record is metadata, not content:

- `RequestRecord` carries the request id, client, resolved model/provider, tier,
  attempts, latency, usage, cost and terminal status
  (`crates/zroutery-core/src/stats.rs::RequestRecord`);
- `RecordBuilder` builds exactly one record per request
  (`crates/zroutery-core/src/stats.rs::RecordBuilder`);
- The stats store inserts it, `recent` returns the newest slice, `summary`
  aggregates by model/provider/tier/client and keeps currencies separate
  (`crates/zroutery-core/src/stats.rs::Stats`,
  `crates/zroutery-core/src/stats.rs::Stats::record`,
  `crates/zroutery-core/src/stats.rs::Stats::summary`);
- the log is bounded by the configured `log_limit`; it is in memory and cleared
  on restart (`crates/zroutery-core/src/stats.rs::Stats::clear_history`).

A prompt or response body is never stored in a record.

## Health and latency

- `StatsStore` holds the EWMA, percentile rings and per-provider/model failure
  counts (`crates/zroutery-core/src/stats_ext.rs::StatsStore`).
- `Ewma` smooths latency; `PercentileEstimator` keeps a bounded distribution
  (`crates/zroutery-core/src/stats_ext.rs::Ewma`,
  `crates/zroutery-core/src/stats_ext.rs::PercentileEstimator`).
- `ObservationStore` holds the runtime observations the router reads
  (`crates/zroutery-core/src/observation.rs::ObservationStore`,
  `crates/zroutery-core/src/observation.rs::RuntimeObservation`,
  `crates/zroutery-core/src/observation.rs::HealthState`).

This state is process-local. A restart starts with an empty health view: scores
return to their no-sample default and a cooling model is eligible again. That is
not the same as "every model healthy"; it means there are no samples yet.

## Activity projection

The dashboard's Activity view is a bounded projection, not a second store
(`crates/zroutery-core/src/server/projection_log.rs::ProjectionLog`). It holds
the recent records the UI reads; clearing it is an explicit operation.

## Sessions and response retention

- `SessionStore` keeps routing sessions and continuation bindings
  (`crates/zroutery-core/src/session.rs::SessionStore`).
- The Responses API store is in memory and bounded by count
  (`crates/zroutery-core/src/ir/response.rs::ResponseStore`); a stored response
  can be retrieved or continued until it is evicted or the process exits.

## Not implemented

- No durable request log; use the runtime store or an export for history.
- No health carried across restarts.
- No prompt/response content in any observation record.

## Sources

- `crates/zroutery-core/src/stats.rs::RequestRecord`
- `crates/zroutery-core/src/stats.rs::RecordBuilder`
- `crates/zroutery-core/src/stats.rs::Stats`
- `crates/zroutery-core/src/stats.rs::Stats::record`
- `crates/zroutery-core/src/stats.rs::Stats::summary`
- `crates/zroutery-core/src/stats.rs::Stats::clear_history`
- `crates/zroutery-core/src/stats_ext.rs::StatsStore`
- `crates/zroutery-core/src/stats_ext.rs::Ewma`
- `crates/zroutery-core/src/stats_ext.rs::PercentileEstimator`
- `crates/zroutery-core/src/observation.rs::ObservationStore`
- `crates/zroutery-core/src/observation.rs::RuntimeObservation`
- `crates/zroutery-core/src/observation.rs::HealthState`
- `crates/zroutery-core/src/server/projection_log.rs::ProjectionLog`
- `crates/zroutery-core/src/session.rs::SessionStore`
- `crates/zroutery-core/src/ir/response.rs::ResponseStore`
