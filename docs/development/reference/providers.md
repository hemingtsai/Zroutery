# Provider Surfaces Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page covers the provider kinds that are not the plain Anthropic/OpenAI
upstream: NewAPI account panels, OpenCode Zen, and the CC Switch import. It does
not claim real-panel acceptance; current status is in
[current-state.md](../current-state.md).

## Provider kinds

`ProviderKind` is the one declaration of what an upstream speaks
(`crates/zroutery-core/src/config/provider.rs::ProviderKind`):

| Kind | Speaks | Used for |
|---|---|---|
| `Anthropic` | Anthropic Messages | Anthropic or a compatible relay |
| `OpenAICompatible` | OpenAI Chat/Responses | OpenAI or a compatible relay |
| `OpenCodeZenFree` | OpenCode Zen free tier | anonymous keyless tier |
| `OpenCodeConsole` | OpenCode Console | OAuth-backed inference |
| `Decision` | the decision protocol | Auto Mode verdicts, not chat traffic |

A decision provider is not a tier member and is not offered for main traffic
(`crates/zroutery-core/src/config/provider.rs::ProviderKind::is_decision`). It
answers typed questions; a relay carries neither a decision path nor a
confidence threshold.

## NewAPI account panels

A NewAPI relay publishes no balance of its own; the figure belongs to a panel
account.

- `NewApiAdapter` is the panel client
  (`crates/zroutery-core/src/account/adapters/newapi/mod.rs::NewApiAdapter`).
- The panel origin is derived from the provider's base URL, with an explicit
  per-account override (`crates/zroutery-core/src/config/binding.rs::panel_root`).
  A relay endpoint ending in `/v1` is not the panel root.
- An account's `key_ref` is the **panel token** used to probe balances and sign
  in. It never travels upstream as chat traffic. Inference credentials are a
  separate pool on the account, each with its own reference and priority
  (`crates/zroutery-core/src/config/registry.rs::AccountConfig`).
- A provider with an inference-key pool serves only from declared keys; it does
  not silently fall back to the panel token or the provider's default key.

Panel response shapes are parsed into typed values
(`crates/zroutery-core/src/account/adapters/newapi/types.rs::NewApiStatus`); a
missing user-id header can make a valid personal token look expired on some
panels, which is why the panel binding carries the user id.

## OpenCode Zen

OpenCode has two upstream shapes, both under the `opencode` feature
(`crates/zroutery-core/src/config/provider.rs::ProviderKind`):

- **Free tier.** The gateway checks a fixed client fingerprint. The request is
  shaped with the OpenCode identity and a request id generated locally
  (`crates/zroutery-core/src/opencode/mod.rs::request_id`). The session is owned
  here, never forwarded from the client.
- **Console.** Device-code OAuth tokens are stored in the credential store and
  resolved by reference
  (`crates/zroutery-core/src/opencode/console.rs::resolve`). The Console catalog
  is discovered from the Console config endpoint
  (`crates/zroutery-core/src/opencode/console.rs::discover`); the inference
  endpoint requires the billing org id.

Neither kind is a generic OpenAI relay: without the shaping, the gateway refuses
the request.

## CC Switch import

CC Switch is a client provider database, imported read-only.

- `CcSwitchSource` opens the SQLite file read-only and sets `query_only`;
  importing never writes to the source
  (`crates/zroutery-core/src/history_import/ccswitch.rs::CcSwitchSource::open`).
- The source identity is the constant `ccswitch`
  (`crates/zroutery-core/src/history_import/ccswitch.rs::SOURCE`), so a re-import
  updates facts rather than duplicating them.
- The desktop finds the database at the tool's conventional location
  (`src-tauri/src/ccswitch/read.rs::read_providers`); the import itself is
  preview-then-commit, with a fingerprint and a duplicate/suspected count.
- Imported facts are accounting metadata only: no prompt, response, credential or
  request body is stored.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| panel token rejected as expired | wrong panel root or missing user id | check `panel_root` and the account user id |
| balance reports zero | the panel answered a different account | compare the account/user binding |
| OpenCode request refused 403 | missing or wrong shaping | check the provider kind and profile |
| Console token refresh fails | expired refresh token | re-run the device-code flow |
| CC Switch import finds nothing | wrong database or no usage rows | check the source path and range |
| re-import duplicates rows | source identity missing | use the source constant and entity id |

## Not implemented

- No claim that a real NewAPI panel accepts the adapter; loopback fixtures are
  not a production acceptance.
- No generic upstream support for a provider kind outside the table.
- No write path to the CC Switch database.

## Sources

- `crates/zroutery-core/src/config/provider.rs::ProviderKind`
- `crates/zroutery-core/src/config/provider.rs::ProviderKind::is_decision`
- `crates/zroutery-core/src/config/binding.rs::panel_root`
- `crates/zroutery-core/src/config/registry.rs::AccountConfig`
- `crates/zroutery-core/src/account/adapters/newapi/mod.rs::NewApiAdapter`
- `crates/zroutery-core/src/account/adapters/newapi/types.rs::NewApiStatus`
- `crates/zroutery-core/src/opencode/mod.rs::request_id`
- `crates/zroutery-core/src/opencode/console.rs::resolve`
- `crates/zroutery-core/src/opencode/console.rs::discover`
- `crates/zroutery-core/src/history_import/ccswitch.rs::CcSwitchSource::open`
- `crates/zroutery-core/src/history_import/ccswitch.rs::SOURCE`
- `src-tauri/src/ccswitch/read.rs::read_providers`
