# Reference Index

> Authority: index · Owner: docs · Last verified: 2026-11-27 · Verified by: `scripts/docs_check.py`

The pages in this directory are the source-verified subsystem references. A page
owns its facts; other documents link to it instead of restating it.

Coverage is explicit in [`docs/coverage.json`](../../coverage.json): every top-level Core
module, public or private, is `owned`, `covered_elsewhere` with a named place, or
`unowned` with a reason. `scripts/coverage_check.py` fails on silence.

## Core

| Module area | Page |
|---|---|
| account, check-in, token capture | [accounts.md](accounts.md) |
| accounting, budget, billing | [accounting.md](accounting.md) |
| agent takeover | [client-takeover.md](client-takeover.md) |
| client attribution | [client-attribution.md](client-attribution.md) |
| classifier, query purpose | [request-purpose.md](request-purpose.md) |
| config, secrets | [config.md](config.md) |
| failure, error, upstream errors | [failure.md](failure.md) |
| file transactions | [file-transactions.md](file-transactions.md) |
| history import, routing data | [routing-data.md](routing-data.md) |
| IR, protocol, media | [protocol.md](protocol.md) |
| migration, cutover | [migration.md](migration.md) |
| ML routing, outcome, feedback | [ml-routing.md](ml-routing.md), [outcome.md](outcome.md) |
| models.dev catalogue | [model-catalog.md](model-catalog.md) |
| policy, router, election, breaker | [routing-policy.md](routing-policy.md) |
| rectifiers | [rectifier.md](rectifier.md) |
| stats, observation, sessions | [observability.md](observability.md) |
| security: auth, CORS, capabilities | [security.md](security.md) |
| documentation rules and enforcement | [documentation-rules.md](documentation-rules.md) |

## Desktop and UI

| Module area | Page |
|---|---|
| IPC commands, capabilities | [ipc.md](ipc.md) |
| providers, CC Switch, OpenCode | [providers.md](providers.md) |
| UI tokens, primitives, languages | [ui-design-system.md](ui-design-system.md) |

## Covered elsewhere, not here

| Area | Where |
|---|---|
| headless entry and subcommands | [README.md](../../../README.md), [runbook.md](../runbook.md) |
| lock helpers (`sync.rs`) | internal utility; no user-facing behaviour page |
| `src-tauri` command plumbing (history imports, logs, usage export, tray/shell) | the IPC pattern in [ipc.md](ipc.md); per-command behaviour has no dedicated page |
| individual UI pages | [ui-design-system.md](ui-design-system.md), [ipc.md](ipc.md); page behaviour is not yet a dedicated reference |

## Rule

A new core module either gets a page here or is added to "covered elsewhere"
with the page that owns its behaviour. Silence is not coverage.
