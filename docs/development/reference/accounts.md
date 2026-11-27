# Accounts Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-27 · Verified by: `cargo test --workspace --all-features --locked`

This page describes account declarations, panel-token binding, inference-key
pools, browser check-in and token capture. It does not claim real-panel
acceptance; current status is in [current-state.md](../current-state.md).

## Declaration

An account is a declaration on a provider
(`crates/zroutery-core/src/config/registry.rs::AccountConfig`); plugin
instances host their own account declarations, but the scoped account
transport gate keeps them out of native reconciliation until the adapter
surface is wired
(`src-tauri/src/plugins/bindings.rs`):

- `account_id` names the panel account;
- `key_ref` is the **panel token** used to probe balances and sign in;
- `inference_keys` is a separate ordered pool
  (`crates/zroutery-core/src/config/registry.rs::InferenceKeyConfig`);
- `maintenance` carries the check-in and panel options
  (`crates/zroutery-core/src/config/registry.rs::MaintenanceConfig`).
- a plugin instance can declare its own `accounts`, but those are forced to
  `probe_kind: None` with cleared key and maintenance, so the native probe and
  check-in path never sees them
  (`crates/zroutery-core/src/config/plugin.rs::PluginInstanceConfig`,
  `src-tauri/src/plugins/bindings.rs`).

The document stores references, not secrets. A panel token and an inference key
are different credentials with different lifetimes; replacing one must not
silently replace the other.

## Binding a panel token

```text
stage token
  -> verify against the panel
  -> preview identity (and whether it changed)
  -> publish, or discard
```

- `stage_account_binding` stores the token under a staged reference, journals the
  publication and returns a `BindingPreview` with the remote identity and a
  warning when it differs from the previous one
  (`src-tauri/src/account_binding.rs::stage_account_binding`,
  `src-tauri/src/account_binding.rs::BindingPreview`).
- `publish_account_binding` re-checks the version, points the declaration at the
  staged reference and saves the document; a stale version is refused
  (`src-tauri/src/account_binding.rs::publish_account_binding`).
- The identity change is explicit: a token for another account is not accepted
  just because it is valid.
- A secret never crosses a view or a journal; the preview carries identity and
  verification, not the token.

## Inference keys

- A provider with an inference-key pool serves only from declared keys; it does
  not silently fall back to the panel token or the provider key
  (`crates/zroutery-core/src/config/registry.rs::AccountConfig`).
- Each entry has its own `key_ref` and priority. Reordering the UI list rewrites
  each entry's priority; execution sorts by priority, account id and key id, so
  array position alone changes nothing, and the secret is never rewritten.
- Replacing or removing a key does not rewrite the account's panel token.
- The desktop commands for the pool are `set_account_inference_key` and
  `remove_account_inference_key`
  (`src-tauri/src/inference_commands.rs::set_account_inference_key`,
  `src-tauri/src/inference_commands.rs::remove_account_inference_key`).

## Check-in

Two paths exist, and they are not the same claim:

- **Browser check-in** drives a headed browser over the DevTools protocol, with
  an isolated profile. The operator starts, resumes and cancels it; the status is
  polled (`src-tauri/src/checkin_commands.rs::start_checkin`,
  `src-tauri/src/checkin_commands.rs::resume_checkin`,
  `src-tauri/src/checkin_commands.rs::cancel_checkin`,
  `src-tauri/src/checkin_commands.rs::get_checkin_status`).
- The browser engine owns a CDP session, launches the configured browser with a
  per-account profile and records the DevTools port
  (`src-tauri/src/checkin/cdp.rs::BrowserSession`,
  `src-tauri/src/checkin/cdp.rs::launch`,
  `src-tauri/src/checkin/cdp.rs::build_args`,
  `src-tauri/src/checkin/profile.rs::profile_dir`,
  `src-tauri/src/checkin/profile.rs::devtools_port_file`). Reattaching to a
  parked browser goes back to the held panel page or the attempt's own OAuth
  popup, never to whatever tab happens to be first in the target list
  (`src-tauri/src/checkin/cdp.rs::resume_page`).
- **HTTP check-in** performs the panel's own API call without a browser
  (`src-tauri/src/checkin_commands.rs::api_checkin`).
- **Relay panels that grant at sign-in** are driven by one executor for the
  manual command and the scheduler alike: the driver detects the relay's panel
  root, walks the panel's own sign-in flow in the account's browser session,
  and reads the settlement back from the page probe. The reward is granted
  while signing in, not by posting a separate check-in endpoint
  (`src-tauri/src/checkin_commands.rs::launch_attempt`,
  `src-tauri/src/checkin/agentrouter.rs::is_panel`).
- The relay page probe re-derives the state from the panel's own storage and
  API inside the page and returns only an amount-bearing result: the account's
  password, the panel token and every other response field stay in the page,
  and a login older than the attempt's start is never accepted as a new reward
  (`src-tauri/src/checkin/agentrouter.rs::observe_expression`,
  `src-tauri/src/checkin/agentrouter.rs::parse`).

A browser check-in reports its phase and a readable failure; it never logs the
panel credential.

## Token capture

Token capture is for a panel token the operator cannot copy by hand:

- `start_token_capture` opens a headed browser and waits for sign-in
  (`src-tauri/src/token_capture/mod.rs::start_token_capture`).
- The report has no token field; the only destination is the credential store
  (`src-tauri/src/token_capture/mod.rs::TokenCaptureView`).
- `complete_token_capture` points the account declaration at the stored token
  (`src-tauri/src/token_capture/mod.rs::complete_token_capture`).
- Phases: `Idle`, `Opening`, `Waiting`, `NeedsVerification`, `Stored`,
  `Captured`, `Failed`, `Cancelled`, `Unsupported`
  (`src-tauri/src/token_capture/mod.rs::CapturePhase`).

The browser's own access token and HttpOnly refresh cookie never leave the page;
only the minted durable token is stored.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| binding refused | the declaration changed since staging | start again; do not force |
| identity warning | the token belongs to another account | confirm the identity change explicitly |
| panel token expired | the panel rotated the token | capture or paste a new one |
| check-in phase `NeedsVerification` | the panel asked for a step in the browser | finish it in the open browser |
| capture phase `Unsupported` | the panel offers no durable token | use another credential path |
| inference keys not used | the account has no pool, or priorities are unset | inspect the account's key list |

## Not implemented

- No claim that a real NewAPI panel accepts this binding; loopback fixtures are
  not production acceptance.
- No automatic identity-change confirmation; it is an operator decision.
- No token value in any view, diagnostic or report.

## Sources

- `crates/zroutery-core/src/config/registry.rs::AccountConfig`
- `crates/zroutery-core/src/config/registry.rs::InferenceKeyConfig`
- `crates/zroutery-core/src/config/registry.rs::MaintenanceConfig`
- `crates/zroutery-core/src/account/store.rs::AccountStore`
- `crates/zroutery-core/src/account/types.rs::AccountStatus`
- `src-tauri/src/account_binding.rs::BindingPreview`
- `src-tauri/src/account_binding.rs::stage_account_binding`
- `src-tauri/src/account_binding.rs::publish_account_binding`
- `src-tauri/src/checkin_commands.rs::start_checkin`
- `src-tauri/src/checkin_commands.rs::resume_checkin`
- `src-tauri/src/checkin_commands.rs::cancel_checkin`
- `src-tauri/src/checkin_commands.rs::get_checkin_status`
- `src-tauri/src/checkin_commands.rs::api_checkin`
- `src-tauri/src/checkin/agentrouter.rs::is_panel`
- `src-tauri/src/checkin/agentrouter.rs::parse`
- `src-tauri/src/checkin/cdp.rs::resume_page`
- `src-tauri/src/token_capture/mod.rs::CapturePhase`
- `src-tauri/src/token_capture/mod.rs::start_token_capture`
- `src-tauri/src/token_capture/mod.rs::complete_token_capture`
- `src-tauri/src/token_capture/mod.rs::TokenCaptureView`
- `crates/zroutery-core/src/config/plugin.rs::PluginInstanceConfig`
- `src-tauri/src/plugins/bindings.rs`
