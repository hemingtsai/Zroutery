# Client Takeover Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-21 · Verified by: `cargo test --workspace --all-features --locked`

This page describes how Zroutery takes over an external client's configuration,
previews the change, and gives it back. It does not claim a production takeover;
current status is in [current-state.md](../current-state.md).

## What it does

```text
read client config
  -> propose managed fields + per-field diff + fingerprint
  -> operator confirms
  -> adopt: write managed values, capture originals, persist ownership
  -> release: resolve conflicts, restore originals, persist state
```

The four clients are declared once in
`crates/zroutery-core/src/agent_takeover/clients.rs::AgentType::ALL`. The ownership
state machine and the manifest live in
`crates/zroutery-core/src/agent_takeover/ownership.rs`.

## Invariants

1. **Preview is read-only and fingerprinted.** A proposal carries the managed
   fields, the per-field before/after and a digest of the config it was computed
   against (`crates/zroutery-core/src/agent_takeover/ownership.rs::TakeoverProposal`).
   Adopt refuses a proposal whose fingerprint no longer matches, so a file edited
   after the preview is not overwritten
   (`src-tauri/src/takeover_commands.rs::takeover_adopt`).
2. **Only managed fields are touched.** Everything else in the client config is
   preserved; the diff is the unit of review.
3. **The ownership state is explicit.** `Verified`, `Adopted`, `Releasing` and
   `Released`; a release is allowed only from `Adopted`, and `Releasing` excludes
   a second concurrent release
   (`crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipState`).
4. **Release resolves conflicts per field, not per file.** Each conflict is
   `KeepExternal`, `OverwriteWithManaged` or `Skip`, and the report names which
   fields were kept, overwritten and skipped
   (`crates/zroutery-core/src/agent_takeover/ownership.rs::ConflictResolution`).
5. **Absent and null are different.** A managed field that did not exist before
   adoption is recorded in `absent_fields` and is deleted on release; a field
   that existed with a null value is restored as null
   (`crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipManifest`).
6. **The manifest survives the process.** It records the state, managed fields,
   adoption snapshots, absent fields, generation and last-applied values. Without
   `last_applied`, a release after a crash would treat every legitimate proxy
   rewrite as a user edit (`crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipManifest`).
7. **File-writing tests run against temporary roots.** A takeover test never
   writes a real client configuration.

## Operations

| Operation | Entry point | Safety |
|---|---|---|
| status | `src-tauri/src/takeover_commands.rs::takeover_status` | read-only |
| preview | `src-tauri/src/takeover_commands.rs::client_setup_preview` | read-only, returns a fingerprint |
| apply setup | `src-tauri/src/takeover_commands.rs::client_setup_apply` | requires the expected fingerprint |
| adopt | `src-tauri/src/takeover_commands.rs::takeover_adopt` | refuses a moved config |
| release | `src-tauri/src/takeover_commands.rs::takeover_release` | requires a conflict strategy; returns a field-level report |

Adoption and release are explicit operator actions. There is no background
takeover and no automatic retry after a failed write.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| adopt refuses | config changed since preview | re-preview; do not force |
| release finds conflicts | a managed field was edited externally | choose keep/overwrite/skip per the report |
| release reports a field missing | the client removed it | `absent_fields` governs delete vs restore |
| a second release is refused | state is `Releasing` | wait for the in-flight release |
| a config field is recreated on release | it was absent before adoption and got restored as a value | check `absent_fields` |

## Not implemented

- No production takeover is claimed; the controlled-takeover node is blocked in
  `current-state.md`.
- No automatic conflict resolution; the strategy is the operator's choice.
- No support for a client format outside the four declared agent types.

## Sources

- `crates/zroutery-core/src/agent_takeover/clients.rs::AgentType`
- `crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipState`
- `crates/zroutery-core/src/agent_takeover/ownership.rs::ConflictResolution`
- `crates/zroutery-core/src/agent_takeover/ownership.rs::TakeoverProposal`
- `crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipManifest`
- `crates/zroutery-core/src/agent_takeover/ownership.rs::extract_field_values`
- `src-tauri/src/takeover_commands.rs::takeover_status`
- `src-tauri/src/takeover_commands.rs::client_setup_preview`
- `src-tauri/src/takeover_commands.rs::client_setup_apply`
- `src-tauri/src/takeover_commands.rs::takeover_adopt`
- `src-tauri/src/takeover_commands.rs::takeover_release`
