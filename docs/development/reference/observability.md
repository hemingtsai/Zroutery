# Observability Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-15 · Verified by: `cargo test --workspace --all-features --locked`

This page describes the request log, the health view, the activity projection and
the sessions kept in memory. It does not describe accounting; spend is durable
and covered by [accounting.md](accounting.md).

## Request records

A request record is metadata, not content:

- `RequestRecord` carries the request id, client, resolved model/provider, the
  tier a request was escalated away from, attempts, latency, usage, cost and
  terminal status (`crates/zroutery-core/src/stats.rs::RequestRecord`);
- `RecordBuilder` builds exactly one record per request
  (`crates/zroutery-core/src/stats.rs::RecordBuilder`);
- The stats store inserts it, `recent` returns the newest slice, and `summary`
  aggregates by model, request kind and client, keeping currencies separate
  (`crates/zroutery-core/src/stats.rs::Stats`,
  `crates/zroutery-core/src/stats.rs::Stats::record`,
  `crates/zroutery-core/src/stats.rs::Stats::summary`,
  `crates/zroutery-core/src/stats.rs::StatsSummary`);
- the in-memory ring is bounded by the configured `log_limit` and is not
  restored on restart
  (`crates/zroutery-core/src/stats.rs::Stats::clear_history`).

A prompt or response body is never stored in a record.

The ring is not the only copy of a request: when a runtime store is attached,
`Stats` reloads committed usage rows into its running totals, and the store
serves paged history to the UI, so a restart loses the recent view but not the
usage history (`crates/zroutery-core/src/stats.rs::Stats::attach_store`,
`crates/zroutery-core/src/runtime_store/history.rs::query_usage`).

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

The dashboard's Activity view is assembled from the live health snapshot, the
stats summary and the newest 200 records; it is a read, not a second store
(`src-tauri/src/state/budget.rs::Desktop::activity`). The accounting projection
log is a separate mechanism and is not the Activity feed
(`crates/zroutery-core/src/server/projection_log.rs::ProjectionLog`).

## Sessions and response retention

- `SessionStore` keeps routing sessions and continuation bindings
  (`crates/zroutery-core/src/session.rs::SessionStore`).
- The Responses API store is in memory and bounded by count
  (`crates/zroutery-core/src/ir/response.rs::ResponseStore`); a stored response
  can be retrieved or continued until it is evicted or the process exits.

## Request projection

`observability.rs` is a read-only correlation view over accepted Core types:

- `project_request(outcome, decision)` returns a `Projection`: a
  `RequestProjection` or a `ProjectionRefusal`
  (`crates/zroutery-core/src/observability.rs::project_request`,
  `crates/zroutery-core/src/observability.rs::Projection`);
- `project_batch(outcomes, decisions)` does the same for a batch
  (`crates/zroutery-core/src/observability.rs::project_batch`);
- `RequestProjection` carries identifiers, attempts in routing order, optional
  decision summary, status, failure class, planned/last-attempted/served labels,
  usage, costs and the inert recorded clock
  (`crates/zroutery-core/src/observability.rs::RequestProjection`);
- `CorrelationKeys` names the three request-scoped ids (`out_`, `req_`, `dec_`)
  (`crates/zroutery-core/src/observability.rs::CorrelationKeys`); the attempt id
  (`att_`) lives on the attempt projection
  (`crates/zroutery-core/src/observability.rs::AttemptProjection`).

The projection is pure: no I/O, clock read, interior state or thread. Ordering
is by identifiers through the projection's `content_order` method, never by a
timestamp (`crates/zroutery-core/src/observability.rs::RequestProjection::content_order`).
An absent decision id is a typed `ProjectionRefusal`, not a filled-in id
(`crates/zroutery-core/src/observability.rs::ProjectionRefusal`).

Failure messages and operator free text are deliberately not projected; only the
closed failure class, impact bits and HTTP status cross the boundary. The
`REQUEST_PROJECTION_FIELDS` list and its test pin the serialized field set
(`crates/zroutery-core/src/observability.rs::REQUEST_PROJECTION_FIELDS`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| health resets on restart | it is process-local | expected; not a data loss (`crates/zroutery-core/src/observation.rs::ObservationStore`) |
| a projection is refused | missing decision id | keep the refusal, do not synthesize an id (`crates/zroutery-core/src/observability.rs::ProjectionRefusal`) |
| old records disappear | bounded log | export or raise the limit (`crates/zroutery-core/src/stats.rs::Stats`) |

## Not implemented

- The recent-request ring is not durable; usage history is read from the runtime
  store through the history query and export paths.
- No health carried across restarts.
- No prompt/response content in any observation record.

## Sources

- `crates/zroutery-core/src/stats.rs::RequestRecord`
- `crates/zroutery-core/src/stats.rs::RecordBuilder`
- `crates/zroutery-core/src/stats.rs::Stats`
- `crates/zroutery-core/src/stats.rs::Stats::record`
- `crates/zroutery-core/src/stats.rs::Stats::summary`
- `crates/zroutery-core/src/stats.rs::StatsSummary`
- `crates/zroutery-core/src/stats.rs::Stats::clear_history`
- `crates/zroutery-core/src/stats.rs::Stats::attach_store`
- `crates/zroutery-core/src/runtime_store/history.rs::query_usage`
- `crates/zroutery-core/src/stats_ext.rs::StatsStore`
- `crates/zroutery-core/src/stats_ext.rs::Ewma`
- `crates/zroutery-core/src/stats_ext.rs::PercentileEstimator`
- `crates/zroutery-core/src/observation.rs::ObservationStore`
- `crates/zroutery-core/src/observation.rs::RuntimeObservation`
- `crates/zroutery-core/src/observation.rs::HealthState`
- `crates/zroutery-core/src/server/projection_log.rs::ProjectionLog`
- `src-tauri/src/state/budget.rs::Desktop::activity`
- `crates/zroutery-core/src/observability.rs::CorrelationKeys`
- `crates/zroutery-core/src/observability.rs::AttemptProjection`
- `crates/zroutery-core/src/session.rs::SessionStore`
- `crates/zroutery-core/src/ir/response.rs::ResponseStore`
