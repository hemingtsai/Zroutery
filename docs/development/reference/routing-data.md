# Routing Data Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page describes the data that routing learns from, where it lives, and what
it deliberately does not contain. It is not a retention policy; the operator
clears the durable trace log explicitly.

## Data families

| Family | Lifetime | Contains | Owner |
|---|---|---|---|
| request log (recent ring) | process memory, bounded; durable usage facts live in the runtime store, with a summary row folded forward in the same transaction as every write | metadata for the Activity page | `Stats`, runtime store |
| spend ledger | durable (`spend.json` / runtime store) | settled cost per scope, period and currency | accounting |
| routing traces | durable JSONL, operator-cleared | decision-time features, attempt outcomes, execution identity | ML |
| imported history | durable SQLite facts | a source's accounting rows, normalised | history import |

These are not interchangeable. A request record is not a training sample; an
imported day total is not a local request.

## Routing traces

- A trace is built from the decision-time input, not from a later
  reconstruction, and records the canonical samples the request produced
  (`crates/zroutery-core/src/ml/traces.rs::RequestTrace`).
- The execution identity records what the upstream declared it ran, separately
  from the configured candidate. A relay fronting several backends is why the two
  are separate.
- Task evidence is optional: absent means no verifier ran, not that the task
  failed (`crates/zroutery-core/src/ml/traces.rs::RequestTrace`).
- The log is append-only while the process runs; `TraceLog` owns the append
  handle and can be counted, tailed or cleared
  (`crates/zroutery-core/src/ml/traces.rs::TraceLog`).
- There is no automatic retention: the promotion round trains from the whole log,
  so a policy that fired on its own would change what the next model learns.

## Imported history

- A fact is keyed by source, instance and the source's own entity id; a
  re-import updates what changed and recognises what did not
  (`crates/zroutery-core/src/history_import/mod.rs::ImportedFact`).
- A revision detects a changed row without trusting source timestamps.
- Duplicates are stored with a `duplicate` marker and are excluded from totals by
  their own column; the evidence is kept without inflating the number.
- The import is preview/commit; `ImportPreview` carries the counts and samples
  the operator confirms (`crates/zroutery-core/src/history_import/mod.rs::ImportPreview`).
- The runtime store writes facts in one transaction
  (`crates/zroutery-core/src/runtime_store/imports.rs::commit_import`).

## Privacy boundary

- Neither family stores a prompt, a response body, a credential or a request
  body. Imported facts are accounting metadata; traces are features and outcomes.
- A source's reported zero is kept apart from a price actually applied: a zero is
  not evidence that the request was free
  (`crates/zroutery-core/src/history_import/mod.rs::ImportedFact`).
- An imported instance is part of the key, so importing another machine's
  database cannot overwrite this machine's facts.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| a re-import duplicates a day | source/instance/entity id not stable | fix the adapter identity |
| imported total is lower than the source | suspected duplicates are excluded | inspect the duplicate detail, do not sum both |
| traces are empty after a restart | no durable state directory, or the log was cleared | check the configured state directory |
| a trace says "unknown execution" | the upstream never declared a model | not a fabricated identity; read it as absent |
| a promotion round says nothing to learn | no traces, or no decision-time input | check that shadow/attachment records decisions |

## Not implemented

- No automatic retention or rotation for the trace log.
- No cross-source duplicate identity: the same call present in two sources can
  still be counted once per source.
- No prompt/response content in either family; that is a boundary, not a gap.

## Sources

- `crates/zroutery-core/src/ml/traces.rs::RequestTrace`
- `crates/zroutery-core/src/ml/traces.rs::TraceLog`
- `crates/zroutery-core/src/history_import/mod.rs::ImportedFact`
- `crates/zroutery-core/src/history_import/mod.rs::ImportPreview`
- `crates/zroutery-core/src/runtime_store/imports.rs::commit_import`
- `crates/zroutery-core/src/stats.rs::Stats`
- `crates/zroutery-core/src/runtime_store.rs::RuntimeStore::usage_snapshot`
- `crates/zroutery-core/src/runtime_store/history.rs::query_usage`
