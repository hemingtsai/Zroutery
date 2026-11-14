# Configuration Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page describes the configuration document: defaults, validation,
normalisation, persistence and the secret boundary. It is not a settings guide;
the UI is the editing surface.

## Shape

`AppConfig` is the document (`crates/zroutery-core/src/config/app.rs::AppConfig`):

- `schema_version`, `server`, `routing`, `classifier`, `window`,
  `notifications`, `vision`, `balances`, `providers`, `models`, `budgets`,
  `plugin_instances`;
- optional `shadow` and `ml_routing` (feature `ml`); provider `accounts` is
  declared unconditionally, while the account subsystem that reads it is gated
  by the `account` feature
  (`crates/zroutery-core/src/config/registry.rs::ProviderConfig`,
  `crates/zroutery-core/src/lib.rs::account`).

Provider entries carry a `key_ref`, not a key
(`crates/zroutery-core/src/config/registry.rs::ProviderConfig`). A model entry
carries pricing, tier, capabilities and aliases
(`crates/zroutery-core/src/config/registry.rs::ModelEntry`).

## Defaults

`ServerConfig` is the local gateway's declaration
(`crates/zroutery-core/src/config/server.rs::ServerConfig`):

| Setting | Default |
|---|---|
| host | `127.0.0.1` (loopback) |
| port | `8787` |
| require auth | true |
| body limit | 32 MiB, clamped to 1-512 MiB |
| request log limit | 500 records |
| CORS | off |
| autostart | on |

`max_body_bytes` applies the clamp (`crates/zroutery-core/src/config/server.rs::ServerConfig::max_body_bytes`);
`is_exposed` and `cors_is_wide_open` drive the warnings
(`crates/zroutery-core/src/config/server.rs::ServerConfig::is_exposed`,
`crates/zroutery-core/src/config/server.rs::ServerConfig::cors_is_wide_open`).

## Validation

The document's `validate` method returns `ConfigIssue`s with a severity
(`crates/zroutery-core/src/config/validate.rs::AppConfig::validate`,
`crates/zroutery-core/src/config/validate.rs::ConfigIssue`,
`crates/zroutery-core/src/config/validate.rs::IssueSeverity`).

- An **error** blocks the save: an empty token with auth on, an impossible price
  or budget, a duplicate or orphan id, a zero timeout.
- A **warning** is surfaced but does not block: a network-exposed bind, auth off,
  a wide-open CORS policy, a budget in a currency nothing bills, an
  `extra_headers` key that overrides a managed header.
- `normalize` folds legacy ids, aliases and deprecated fields before validation
  (`crates/zroutery-core/src/config/validate.rs::AppConfig::normalize`).

An `extra_headers` entry can override `authorization` or `x-api-key`; the warning
exists because that value is written into the document and replaces the managed
auth header.

## Persistence and apply

- The save helper writes through a temporary file and a rename, so a crash cannot
  truncate the document; the temporary file is created owner-only
  (`src-tauri/src/store.rs::save`, `src-tauri/src/store.rs::create_private`).
- A corrupt file is copied aside before defaults are loaded
  (`src-tauri/src/store.rs::startup_load`); a failed backup leaves the original
  untouched.
- `apply_config_locked` normalises and validates, refuses a blocking issue,
  probes a changed address, saves, and restores the previous document if the
  listener cannot move (`src-tauri/src/state/server.rs::apply_config_locked`).
- The dashboard save is version-checked so a stale editor cannot roll back a
  binding-owned field.

The document holds the gateway token and any `extra_headers`; it never holds a
provider key. `SecretStore` is the credential boundary
(`crates/zroutery-core/src/config/secrets.rs::SecretStore`).

## Feature gates

- `zroutery-core` ships no default feature (`crates/zroutery-core/Cargo.toml::default`).
- The desktop crate enables `ml`, `account`, `newapi`, `account-maint`,
  `opencode` and `plugin` by default (`src-tauri/Cargo.toml::default`).
- A capability compiled by a feature is inert until configured; a capability not
  compiled is absent from the command surface, not merely hidden.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| save is refused | a blocking `ConfigIssue` | fix the issue; validation is authoritative (`crates/zroutery-core/src/config/validate.rs::AppConfig::validate`) |
| the listener does not move | rebind failed | the previous document is restored (`src-tauri/src/state/server.rs::apply_config_locked`) |
| config is corrupt | unreadable JSON | a backup is left before defaults load (`src-tauri/src/store.rs::startup_load`) |

## Not implemented

- No remote or cloud configuration sync; the document is local.
- No plaintext provider key in the document; the only exceptions are the local
  token and an operator-supplied `extra_headers` value.
- No schema migration beyond the normalisation above.

## Sources

- `crates/zroutery-core/src/config/app.rs::AppConfig`
- `crates/zroutery-core/src/config/registry.rs::ProviderConfig`
- `crates/zroutery-core/src/config/registry.rs::ModelEntry`
- `crates/zroutery-core/src/config/server.rs::ServerConfig`
- `crates/zroutery-core/src/config/server.rs::ServerConfig::max_body_bytes`
- `crates/zroutery-core/src/config/server.rs::ServerConfig::is_exposed`
- `crates/zroutery-core/src/config/server.rs::ServerConfig::cors_is_wide_open`
- `crates/zroutery-core/src/config/validate.rs::AppConfig::validate`
- `crates/zroutery-core/src/config/validate.rs::AppConfig::normalize`
- `crates/zroutery-core/src/config/validate.rs::ConfigIssue`
- `crates/zroutery-core/src/config/validate.rs::IssueSeverity`
- `crates/zroutery-core/src/config/secrets.rs::SecretStore`
- `src-tauri/src/store.rs::save`
- `src-tauri/src/store.rs::create_private`
- `src-tauri/src/store.rs::startup_load`
- `src-tauri/src/state/server.rs::apply_config_locked`
- `crates/zroutery-core/Cargo.toml::default`
- `src-tauri/Cargo.toml::default`
