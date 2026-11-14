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
- `CorrelationKeys` names the four UUID-derived ids (`out_`, `req_`, `dec-`,
  `att_`) (`crates/zroutery-core/src/observability.rs::CorrelationKeys`).

The projection is pure: no I/O, clock read, interior state or thread. Ordering
is by identifiers through the projection's `content_order` method, never by a
timestamp (`crates/zroutery-core/src/observability.rs::RequestProjection::content_order`).
An absent decision id is a typed `ProjectionRefusal`, not a filled-in id
(`crates/zroutery-core/src/observability.rs::ProjectionRefusal`).

Failure messages and operator free text are deliberately not projected; only the
closed failure class, impact bits and HTTP status cross the boundary. The
`REQUEST_PROJECTION_FIELDS` list and its test pin the serialized field set
(`crates/zroutery-core/src/observability.rs::REQUEST_PROJECTION_FIELDS`).

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
