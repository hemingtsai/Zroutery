# Migration Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page describes a client cutover: how a plan is previewed, executed and
rolled back. It is not a claim that a particular client is compatible; detection
and planning are separate from execution.

## What it does

```text
detect client state
  -> build MigrationPlan (ordered steps + snapshot target)
  -> preview with a fingerprint
  -> snapshot the target file (desktop), then execute: apply steps
  -> rollback: restore the snapshot
```

The state machine is `MigrationState`; a plan is a sequence of `MigrationStep`s,
each carrying a `MigrationAction`
(`crates/zroutery-core/src/migration/state.rs::MigrationState`,
`crates/zroutery-core/src/migration/state.rs::MigrationPlan`,
`crates/zroutery-core/src/migration/state.rs::MigrationAction`).

## Invariants

1. **Preview carries a fingerprint.** `migration_apply` refuses a plan whose
   fingerprint no longer matches the file it was computed against
   (`src-tauri/src/migration_commands.rs::migration_apply`,
   `src-tauri/src/migration.rs::fingerprint`).
2. **Snapshot before write.** The desktop takes the snapshot before it calls the
   executor, and refuses to run when no bytes could be captured; a rollback
   restores those bytes, not a regenerated guess
   (`src-tauri/src/state/migration.rs::migration_apply`,
   `crates/zroutery-core/src/migration/executor.rs::create_snapshot`,
   `crates/zroutery-core/src/migration/executor.rs::snapshot_from_disk`).
3. **Rollback needs a snapshot.** A record without a snapshot offers no undo; the
   status says so rather than pretending a rollback is available
   (`crates/zroutery-core/src/migration/result.rs::MigrationResult`).
4. **Owned processes only.** A migration stops only processes it started; an
   external process named by a step is reported as running and is not killed
   (`crates/zroutery-core/src/migration/state.rs::MigrationAction`).
5. **External validation is typed.** A JSON file is parsed; an unsupported format
   is refused rather than accepted without reading
   (`crates/zroutery-core/src/migration/state.rs::MigrationAction`).
6. **The record survives the process.** `MigrationStore` persists the state and
   the completed results; the rollback snapshot is a separate sidecar file
   beside the manifest, not part of it
   (`crates/zroutery-core/src/migration/store.rs::MigrationStore`,
   `crates/zroutery-core/src/migration/store.rs::save_snapshot`).

## Operations

| Operation | Entry point | Safety |
|---|---|---|
| status | `src-tauri/src/migration_commands.rs::migration_status` | read-only |
| previews | `src-tauri/src/migration_commands.rs::migration_previews` | builds plans and fingerprints |
| apply | `src-tauri/src/migration_commands.rs::migration_apply` | requires the expected fingerprint |
| rollback | `src-tauri/src/migration_commands.rs::migration_rollback` | restores the snapshot |

Execution is async because a step may probe an endpoint; file writes and process
start/stop happen in the executor.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| apply refuses | the file changed after preview | preview again; do not force |
| rollback is unavailable | no snapshot was captured | fix forward or restore manually |
| an external process is still running | the migration does not own it | stop it yourself, then retry |
| a step fails midway | a file or process action failed | the result names the step; rollback restores the snapshot |
| a config file is rejected as invalid | the typed validator does not read that format | use a supported format |

## Not implemented

- No automatic compatibility decision; a detected client can still produce a plan
  that needs review.
- No kill of an external process the migration did not start.
- No cross-platform installer orchestration.

## Sources

- `crates/zroutery-core/src/migration/state.rs::MigrationState`
- `crates/zroutery-core/src/migration/state.rs::MigrationPlan`
- `crates/zroutery-core/src/migration/state.rs::MigrationAction`
- `crates/zroutery-core/src/migration/executor.rs::MigrationExecutor`
- `crates/zroutery-core/src/migration/executor.rs::execute`
- `crates/zroutery-core/src/migration/executor.rs::create_snapshot`
- `crates/zroutery-core/src/migration/executor.rs::rollback`
- `crates/zroutery-core/src/migration/executor.rs::snapshot_from_disk`
- `crates/zroutery-core/src/migration/result.rs::MigrationResult`
- `crates/zroutery-core/src/migration/result.rs::MigrationSnapshot`
- `crates/zroutery-core/src/migration/store.rs::MigrationStore`
- `crates/zroutery-core/src/migration/store.rs::save_snapshot`
- `src-tauri/src/state/migration.rs::migration_apply`
- `src-tauri/src/migration.rs::fingerprint`
- `src-tauri/src/migration_commands.rs::migration_apply`
- `src-tauri/src/migration_commands.rs::migration_rollback`
- `src-tauri/src/migration_commands.rs::migration_status`
- `src-tauri/src/migration_commands.rs::migration_previews`
