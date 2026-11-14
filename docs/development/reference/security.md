# Security Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page describes the local trust boundary: who may call the gateway, where
secrets live, what the desktop exposes to the webview, and what is deliberately
kept out of logs and files.

## Threat model

The gateway is a local administrator tool, not a public service:

- it binds loopback by default (`crates/zroutery-core/src/config/server.rs::ServerConfig`);
- authentication is on by default;
- `/health` is the only unauthenticated route
  (`crates/zroutery-core/src/server/handlers.rs::build_app`);
- CORS is off by default; enabling it without origins is a warning
  (`crates/zroutery-core/src/server/cors.rs::cors_layer`).

A local process that holds the token can spend the configured provider credit.
That is the model; hiding the token from the same user is not a goal.

## Gateway authentication

- `auth_layer` accepts `x-api-key`, `x-goog-api-key` or `Authorization: Bearer`
  (`crates/zroutery-core/src/server/handlers.rs::auth_layer`).
- The comparison is constant-time and length-independent
  (`crates/zroutery-core/src/server/handlers.rs::constant_time_eq`).
- An empty configured token with auth on is a validation error, not an open
  gateway (`crates/zroutery-core/src/config/validate.rs::AppConfig::validate`).
- The token is not sent to the webview; the snapshot strips it and exposes only a
  hint (`src-tauri/src/state/snapshot.rs::snapshot`). Reveal/copy are explicit
  commands.

## CORS

- A configured origin is validated as `scheme://host[:port]`; a path, query,
  userinfo or whitespace is rejected
  (`crates/zroutery-core/src/server/cors.rs::is_valid_origin`).
- A configured list where every entry is invalid denies all rather than falling
  back to "any origin".
- An empty configured list means "any origin" and is surfaced as a warning; that
  is a deliberate, visible choice, not an accident.

## Secrets

- Provider keys live in the platform credential store; the document holds
  `key_ref` (`crates/zroutery-core/src/config/registry.rs::ProviderConfig`,
  `src-tauri/src/secrets.rs::KeychainSecrets`).
- Headless may read `ZROUTERY_KEY_*` environment variables; the desktop does not.
- The gateway token and operator-supplied `extra_headers` are in the document;
  it is written owner-only and atomically (`src-tauri/src/store.rs::save`,
  `src-tauri/src/store.rs::create_private`).
- No view, diagnostic, fixture or report contains a token value; the token
  capture view has no field for it
  (`src-tauri/src/token_capture/mod.rs::TokenCaptureView`).
- Plugin credentials are keyring-only: the native keyring refuses `plugin:` refs,
  a plugin slot is readable only after an ownership and grant-epoch check, and
  the snapshot clears plugin credential references
  (`src-tauri/src/plugins/scoped_secrets.rs::verify_slot`,
  `src-tauri/src/secrets.rs::KeychainSecrets`).

## Webview surface

- The desktop declares its own command manifest, and each command needs a
  capability grant; the tray panel has a smaller grant list
  (`src-tauri/build.rs::APP_COMMANDS`,
  `src-tauri/capabilities/default.json::permissions`,
  `src-tauri/capabilities/tray-panel.json::permissions`).
- `withGlobalTauri` is off and the content security policy is restrictive
  (`src-tauri/tauri.conf.json::security`).
- `scripts/command_surface_test.py` fails when a registered command has no grant
  or a grant names no command.
- Plugin-contributed UI is rendered only through host components, and the tray
  exposes safe appearance as a containment path
  (`crates/zroutery-plugin-api/src/ui.rs`, `src-tauri/src/tray.rs`).

## Upstream and error boundary

- Redirects are disabled and both proxy policies stay HTTP/1.1, so a redirect
  cannot carry the provider key to another host
  (`crates/zroutery-core/src/upstream/http.rs::configure`).
- An upstream error body is bounded at construction for the log and never echoed
  to the client (`crates/zroutery-core/src/upstream/errors.rs::truncate`); the
  wire error names the provider and status
  (`crates/zroutery-core/src/error.rs::safe_message`).
- Request and response bodies are not persisted; records carry metadata and
  traces carry features and outcomes.
- Plugin traffic has its own egress path: https-only, no query or userinfo,
  public-IP-only SSRF filter, DNS pinned per operation and retry, no proxy and no
  redirects; a declared TLS transport profile is refused
  (`crates/zroutery-core/src/upstream/transport.rs::request_for`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| every request is 401 | token empty or wrong | copy from the Gateway menu; regenerate if unknown |
| a website can call the proxy | CORS enabled with no origin list | add origins or turn CORS off |
| a key appears in `config.json` | it was put in `extra_headers` | move it to the credential store |
| a network peer can reach the gateway | non-loopback bind | keep auth on; restrict the interface |
| a redirect leaks a header | would require a redirect policy change | do not re-enable redirects |

## Not implemented

- No multi-user accounts or remote authentication.
- No TLS on the loopback listener; it is a local socket.
- No guarantee that a process running as the same user cannot read the token if
  it can read the config document.
- Plugin packages are content-digest addressed without a publisher signature; the
  OS sandbox is Windows-only; an unverified runtime-only package refuses
  activation; the host does not render plugin UI (`render_plugin_ui` fails
  closed) (`crates/zroutery-plugin-host/src/install.rs`,
  `crates/zroutery-plugin-host/src/sandbox.rs::probe`,
  `src-tauri/src/plugins/commands.rs::render_plugin_ui`).

## Sources

- `crates/zroutery-core/src/server/handlers.rs::build_app`
- `crates/zroutery-core/src/server/handlers.rs::auth_layer`
- `crates/zroutery-core/src/server/handlers.rs::constant_time_eq`
- `crates/zroutery-core/src/server/cors.rs::cors_layer`
- `crates/zroutery-core/src/server/cors.rs::is_valid_origin`
- `crates/zroutery-core/src/config/server.rs::ServerConfig`
- `crates/zroutery-core/src/config/registry.rs::ProviderConfig`
- `crates/zroutery-core/src/config/validate.rs::AppConfig::validate`
- `crates/zroutery-core/src/config/secrets.rs::SecretStore`
- `src-tauri/src/secrets.rs::KeychainSecrets`
- `src-tauri/src/state/snapshot.rs::snapshot`
- `src-tauri/src/store.rs::save`
- `src-tauri/src/store.rs::create_private`
- `src-tauri/src/token_capture/mod.rs::TokenCaptureView`
- `src-tauri/build.rs::APP_COMMANDS`
- `src-tauri/capabilities/default.json::permissions`
- `src-tauri/capabilities/tray-panel.json::permissions`
- `src-tauri/tauri.conf.json::security`
- `crates/zroutery-core/src/upstream/http.rs::configure`
- `crates/zroutery-core/src/error.rs::safe_message`
- `src-tauri/src/plugins/scoped_secrets.rs::verify_slot`
- `src-tauri/src/secrets.rs::KeychainSecrets::keyring_only`
- `crates/zroutery-core/src/upstream/transport.rs::request_for`
- `crates/zroutery-plugin-host/src/sandbox.rs::probe`
- `src-tauri/src/plugins/commands.rs`
- `crates/zroutery-core/src/upstream/errors.rs::truncate`
