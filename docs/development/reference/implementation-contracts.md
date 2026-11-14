# Implementation Contracts Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

These are the boundaries a change must preserve. `AGENTS.md` routes here; it does
not restate them.

## Module and wire boundaries

- Keep module boundaries and feature gates; a capability compiled by a feature is
  inert until requested (`crates/zroutery-core/Cargo.toml::default`,
  `src-tauri/Cargo.toml::default`).
- Backend wire fields are `snake_case`; the Tauri argument mapping lives in the
  typed frontend contract (`ui/src/api.ts`).
- Configuration is declaration; runtime observations, credentials and historical
  accounting have separate owners (`crates/zroutery-core/src/config/app.rs::AppConfig`,
  `crates/zroutery-core/src/observation.rs::ObservationStore`,
  `src-tauri/src/secrets.rs::KeychainSecrets`).

## IPC

- Define the `api.ts` contract, implement Rust, then synchronize
  `ui/mock-bridge.ts`.
- Register commands in both the handler and the app manifest
  (`src-tauri/src/lib.rs::generate_handler`,
  `src-tauri/build.rs::APP_COMMANDS`).
- Grant only the required capabilities
  (`src-tauri/capabilities/default.json::permissions`).

## Request lifecycle

- Preserve request/attempt identity and exactly one terminal settlement
  (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`,
  `crates/zroutery-core/src/outcome/mod.rs::Outcome`).
- A failed attempt's known cost remains recorded; unknown usage, price or balance
  is not a measured zero (`crates/zroutery-core/src/accounting.rs::RequestAccounting`,
  `crates/zroutery-core/src/outcome/attempt.rs::Attempt`).
- Cache read/write tokens are subsets of input; reasoning tokens are inside
  output. Monetary totals keep currencies and non-equivalent units separate
  (`crates/zroutery-core/src/ir/mod.rs::Usage`,
  `crates/zroutery-core/src/billing.rs::CostTotals`).
- Retain native protocol semantics, capability evidence and explicit reasoning
  controls (`crates/zroutery-core/src/ir/mod.rs::ChatRequest`,
  `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_effort`).
- A local persistence failure is terminal; it must not replay an upstream or tool
  action that already executed
  (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`).

## Client takeover

- Preserve unrelated edits, support preview and stale-file detection, and make a
  release apply the computed conflict resolution
  (`crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipManifest`).
- File-writing tests run against isolated temporary roots; multi-file writes use
  the recoverable transaction (`crates/zroutery-core/src/file_transaction.rs::commit`).

## Platform and UI

- Use platform-specific dependencies for native features; preserve Windows,
  macOS and Linux target boundaries (`src-tauri/Cargo.toml`).
- UI changes follow the design system
  (`docs/development/reference/ui-design-system.md`); keep English, Simplified
  Chinese and Traditional Chinese keys synchronized and update layout/interaction
  fixtures when structure changes (`ui/src/i18n.tsx`,
  `ui/src/i18n/en.ts`, `ui/src/i18n/zh.ts`, `ui/src/i18n.zhTW.ts`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| a command is registered but not granted | capability drift | `scripts/command_surface_test.py` fails the surface |
| the frontend contract drifts from Rust | `api.ts` not synchronized | fix the contract and the mock bridge |
| a local write fails after an upstream action | persistence failure | terminal failure; never replay (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`) |

## Not implemented

- No per-command behaviour page; the IPC pattern and command-surface gate cover registration.
- No automatic extraction of contracts from code.

## Sources

- `crates/zroutery-core/Cargo.toml::default`
- `src-tauri/Cargo.toml::default`
- `ui/src/api.ts`
- `src-tauri/src/lib.rs::generate_handler`
- `src-tauri/build.rs::APP_COMMANDS`
- `src-tauri/capabilities/default.json::permissions`
- `crates/zroutery-core/src/config/app.rs::AppConfig`
- `crates/zroutery-core/src/observation.rs::ObservationStore`
- `src-tauri/src/secrets.rs::KeychainSecrets`
- `crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`
- `crates/zroutery-core/src/outcome/mod.rs::Outcome`
- `crates/zroutery-core/src/accounting.rs::RequestAccounting`
- `crates/zroutery-core/src/outcome/attempt.rs::Attempt`
- `crates/zroutery-core/src/ir/mod.rs::Usage`
- `crates/zroutery-core/src/billing.rs::CostTotals`
- `crates/zroutery-core/src/ir/mod.rs::ChatRequest`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_effort`
- `crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipManifest`
- `crates/zroutery-core/src/file_transaction.rs::commit`
- `ui/src/i18n.tsx`
- `ui/src/i18n/en.ts`
- `ui/src/i18n/zh.ts`
- `ui/src/i18n.zhTW.ts`
