# Runbook

> Authority: guide · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

How to run, release and recover Zroutery. Rules live in
[verification.md](verification.md); this page is the operator's short path.

## Start and stop

Desktop development:

```text
pnpm install
pnpm dev
```

The app lives in the menu bar / tray. Closing the window does not stop the
gateway; quit from the tray menu to stop it.

Headless:

```text
cargo run -p zroutery-headless
```

The headless process prints the listening address and, when authentication is
on, the token. `Ctrl-C` or `SIGTERM` stops it and flushes the ledger.

## Configuration and secrets

- Config directory: macOS
  `~/Library/Application Support/app.zroutery.desktop/`, Windows
  `%APPDATA%\app.zroutery.desktop\`; override with `ZROUTERY_CONFIG_DIR`.
- API keys live in the platform credential store. The config carries `key_ref`,
  not the key. `extra_headers` is the exception: it is stored in the document,
  so do not put a credential there.
- The gateway token is in the config document with owner-only permissions.
  Regenerate it in Settings → Gateway if it leaks.
- Headless keys come from `ZROUTERY_KEY_<REF>`; the desktop does not read those
  variables.

## Where to look

| Symptom | Look at |
|---|---|
| a request failed | Activity → the request row → attempts and error class |
| a model is cooling down | Routing → the tier pool; health reset is per model |
| a provider is gated | Provider row → wallet hold; release only after the cause is fixed |
| a key is missing | Provider/account row → the Authentication field (masked value = key stored; otherwise it says which credential is missing); the value itself is never shown |
| a panel rejects a token | the panel root and the account user id |
| the gateway will not start | the banner and the bottom log; check the port and the token |
| the document is corrupt | a timestamped `config.json.broken-*` backup is left beside it |

The Activity drawer and the logs page are metadata only; prompts and responses
are not persisted.

## Release

```text
pnpm build
```

This runs `pnpm ui:build` and the Tauri bundler. macOS produces `.app`/`.dmg`
with ad-hoc signing and no notarization; Windows produces `.msi` and NSIS
installers. The installer gate is nightly, Windows-only, and not a substitute for
a macOS run.

Before publishing:

```text
python3 -B scripts/docs_check.py
python3 -B scripts/docs_impact.py --base <start> --head HEAD
python3 -B scripts/commit_contract_test.py --base <start> --head HEAD
```

Run the Tier 1 gates in [verification.md](verification.md); report any Tier 2
gate you did not run. A development build is not an authorized deployment.

## Rollback

| What | How |
|---|---|
| a learned model | Routing → rollback; the pointer returns to deterministic routing |
| a configuration change | restore the previous `config.json` from its backup |
| a client takeover | Takeover → release, choosing keep/overwrite/skip per field |
| a migration | Cutover → rollback; it restores the captured file |
| the gateway | stop it from the tray; a stale process holding the port must be stopped before a restart |

## Common failures

| Failure | Cause | Action |
|---|---|---|
| `address is already in use` | another gateway instance | stop the other instance; do not start a second one |
| every request is rejected with 401 | auth is on and the token is empty or wrong | copy the token from the Gateway menu; regenerate in Settings → Gateway if unknown |
| keychain access denied | the credential store is locked or the app lost access | unlock the store; re-enter the key |
| a panel balance is unknown | the panel is unreachable or the token is for another user | check the panel root, user id and token |
| a UI page stays on "loading" | a lazy chunk did not load (browser-dependent) | reload; report the browser and the console error |
| docs gate fails | a cited source changed without the document | follow the printed document, re-read the source, update the hash |

## Pointers

- [Verification](verification.md)
- [Current state](current-state.md)
- [Architecture](architecture.md)
- [Documentation standard](../DOCUMENTATION.md)

## Fact sources

- Start commands: `package.json::scripts`; `prepare` installs the local hook.
- Config directory: `src-tauri/src/platform.rs::default_config_dir`.
- Gateway token generation: `src-tauri/src/store.rs::generate_token`.
- Bundle targets and signing: `src-tauri/tauri.conf.json::bundle`.
- Learned-model rollback: `crates/zroutery-core/src/server/ml_state.rs::rollback_active_model`.
- Migration state and rollback: `crates/zroutery-core/src/migration/state.rs::MigrationState`.
- Plugin and theme surfaces: `crates/zroutery-core/src/config/plugin.rs::PluginInstanceConfig`, `ui/src/themes/registry.ts`, `scripts/plugin_theme_test.py`.
- Takeover release state: `crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipState`.
