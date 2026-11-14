# IPC Reference

> Authority: reference · Owner: frontend · Last verified: 2026-11-14 · Verified by: `scripts/command_surface_test.py`

The webview talks to Rust through Tauri commands. This page describes that
boundary; it does not list every command signature, because the contract is the
pair of files below and the command-surface gate.

## What it does

- The frontend calls a command by name through the typed wrapper
  (`ui/src/api.ts::invoke`).
- Rust registers command handlers in one `generate_handler!` list
  (`src-tauri/src/lib.rs::generate_handler`) and declares the same names in the
  build-time app manifest (`src-tauri/build.rs::APP_COMMANDS`).
- Tauri turns each declared command into an `allow-<command>` permission; the
  capability files grant them per window
  (`src-tauri/capabilities/default.json::permissions`,
  `src-tauri/capabilities/tray-panel.json::permissions`).
- `scripts/command_surface_test.py` compares the three lists in both directions.

## Invariants

1. **The handler list and the app manifest agree.** A command registered but not
   declared is refused at the IPC boundary; a declaration with no handler is a
   surface wider than the tree. The gate fails on either difference
   (`scripts/command_surface_test.py::main`).
2. **Capabilities grant exactly what is registered.** Every app command gets a
   grant in `default.json`; the tray panel gets only its read/action subset. A
   registered command with no grant is unreachable; a grant with no command is
   stale (`src-tauri/capabilities/default.json::permissions`).
3. **Feature-gated commands are declared unconditionally.** `APP_COMMANDS` is the
   union over all features, not the current build; feature gates live in
   `lib.rs`, not in the permission list (`src-tauri/build.rs::APP_COMMANDS`).
4. **The frontend guards a feature-gated call by an availability flag.** Snapshot
   fields such as `ml_available`, `account_available` and
   `token_capture_available` say whether this build serves the command
   (`src-tauri/src/state/snapshot.rs::snapshot`).
5. **Argument names keep their side.** JavaScript object keys are camelCase;
   Rust command parameters stay snake_case and Tauri maps between them. Do not
   rename one side without the other
   (`ui/src/api.ts::startCheckin`, `src-tauri/src/commands.rs::get_snapshot`).
6. **Secrets do not cross the bridge.** The snapshot strips the gateway token and
   reports only a hint; explicit reveal/copy commands are the only token path
   (`src-tauri/src/state/snapshot.rs::snapshot`).
7. **Errors are values, not provider bodies.** A command returns a typed error
   string; upstream bodies stay in the activity record
   (`src-tauri/src/commands.rs::get_snapshot`).

## Command groups

| Group | Examples | Notes |
|---|---|---|
| Snapshot and activity | `get_snapshot`, `get_activity` | dashboard polling |
| Usage and imported history | `query_usage_history`, `query_imported_history`, `export_imported_history` | paginated; imported history is separate from local usage |
| Credentials and configuration | `save_config`, `set_provider_key`, `clear_provider_key` | keys go to the credential store, not the document |
| Learning stack | `get_ml_status`, `get_ml_shadow`, `run_ml_promotion_round` | feature `ml`; gated by `ml_available` |
| Accounts | `reconcile_accounts`, `set_account_key`, `set_account_inference_key` | feature `account`; panel token and inference keys are separate |
| Check-in and token capture | `start_checkin`, `start_token_capture`, `complete_token_capture` | feature `account-maint`; browser-driven |
| Migration and takeover | `migration_previews`, `migration_apply`, `takeover_adopt` | file-writing; preview and fingerprint required |
| Tray panel | `get_tray_snapshot`, `tray_set_gateway_running` | second capability file, minimal surface |

## Failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `unknown command` at invoke | handler not registered, or name mismatch | register in `lib.rs` and `build.rs` |
| command denied by ACL | no `allow-<command>` grant | add the grant to the correct capability |
| frontend call fails only in some builds | feature-gated command called without an availability guard | read the snapshot flag first |
| mock preview rejects a command | no fixture in `ui/mock-bridge.ts` | add the fixture, or state that the mock does not model it |
| argument is missing or null | camelCase/snake_case mismatch | follow the `api.ts` mapping |

## Not implemented

- Mock-bridge coverage is not enforced: a fixture may be missing while the real
  command exists, and the preview rejects at invoke time. `command_surface_test`
  checks the contract against Rust, not against the mock.
- The capability files are hand-maintained; the gate compares them, but a new
  window still needs its own capability file.

## Sources

- `src-tauri/build.rs::APP_COMMANDS`
- `src-tauri/src/lib.rs::generate_handler`
- `src-tauri/capabilities/default.json::permissions`
- `src-tauri/capabilities/tray-panel.json::permissions`
- `ui/src/api.ts::invoke`
- `ui/mock-bridge.ts::handlers`
- `scripts/command_surface_test.py::main`
- `src-tauri/src/state/snapshot.rs::snapshot`
