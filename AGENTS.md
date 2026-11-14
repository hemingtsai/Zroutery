# Repository guidance

> Authority: normative · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

This is the only agent entry. Read
[docs/development/current-state.md](docs/development/current-state.md) first: it
is the generated truth about what is red and what is blocked. If this file and a
generated file disagree, the generated file wins for facts; this file wins for
permission and process.

## Project layout

- `crates/zroutery-core/`: protocol codecs, routing, accounts, accounting, storage and optional ML.
- `src-tauri/`: desktop lifecycle, native integrations, credential storage and IPC commands.
- `crates/zroutery-headless/`: headless gateway using the same Core and desktop state.
- `ui/`: React/TypeScript frontend and typed mock bridge.
- `scripts/`: build, contract, documentation, layout and release checks.
- `docs/`: current documentation; `docs/archive/` is history and is not read by default.

## Non-negotiables

1. Never persist upstream secrets or request bodies in configuration, history,
   diagnostics, fixtures or reports.
2. Never weaken, skip, delete or re-threshold a test, gate, mutation or budget to
   make a result green. Fix the code or report the gate.
3. Do not reset, stash, reformat or overwrite another worker's files. Check the
   branch, working tree and staged changes before editing.
4. Commit when the change is verified and scoped to one file or one module.
   Leave local runtime configuration and running gateways alone. A development
   build is not an authorized deployment.
5. Dependency changes and release/signing decisions need human approval.

## Working with other agents

- Respect the assigned directories: `crates/` and `src-tauri/` belong to the
  backend owner; `ui/` belongs to the frontend owner.
- Shared files need coordination: manifests, `Cargo.lock`, `ui/src/api.ts`,
  `ui/mock-bridge.ts`, the three i18n dictionaries, fixtures and
  `docs/docs.manifest.json`.
- Re-read a shared file immediately before patching it. Use explicit paths when
  staging. One owner publishes Git commits at a time.
- Delegate only when the user or task authorizes it; parallel workers need
  non-overlapping write scopes.

## Implementation contracts

- Keep module boundaries and feature gates. Backend wire fields are `snake_case`;
  follow the existing Tauri argument mapping in `ui/src/api.ts`.
- IPC changes: define the `api.ts` contract, implement Rust, then synchronize
  `ui/mock-bridge.ts`. Register commands in both `src-tauri/src/lib.rs` and
  `src-tauri/build.rs` (`src-tauri/build.rs::APP_COMMANDS`), and grant only the
  required capabilities in `src-tauri/capabilities/`
  (`src-tauri/capabilities/default.json::permissions`).
- Configuration is declaration; runtime observations, credentials and historical
  accounting have separate owners.
- Preserve request/attempt identity and unique terminal settlement. Failed
  attempts' known costs remain recorded; unknown usage, price or balance is not
  measured zero.
- Cache read/write tokens are subsets of input; reasoning tokens are inside
  output. Monetary totals keep currencies and non-equivalent units separate.
- Retain native protocol semantics, capability evidence and explicit reasoning
  controls. A local persistence failure must not replay an executed upstream or
  tool action.
- Client takeover must preserve unrelated edits and support preview, stale-file
  detection and recovery. Run file-writing tests against isolated temporary roots.
- Use platform-specific dependencies for native features; preserve Windows,
  macOS and Linux target boundaries.
- UI changes follow the design system; keep English, Simplified Chinese and
  Traditional Chinese keys synchronized and update layout/interaction fixtures
  when structure changes.

## Documentation is part of the change

- Every change to code, contracts, gates, toolchain, configuration keys, wire
  fields, release steps or process rules updates every affected document in the
  same change.
- The affected set is computed, not remembered:
  `python3 -B scripts/docs_impact.py --base <start> --head HEAD`.
  If it fails, update the listed documents. There is no `none` escape; if a
  trigger is wrong, fix the manifest or the citation in the same change.
- Every factual sentence in a normative or reference document must cite a source
  you read in this change (`path::symbol` or `path#Lx`). Old documents are leads,
  never sources. See [docs/DOCUMENTATION.md](docs/DOCUMENTATION.md).
- Reports of audits, findings and triage are GitHub Issues, not files under
  `docs/`. An audit that yields a decision writes an ADR; one that yields a
  durable fact updates the relevant document. If you cannot read the issue
  tracker, ask the user for the issue body; do not invent it.
- `docs/development/node-status/`, the evidence registry and the execution
  records are legacy history. Do not add new records there; they are being
  archived. The current facts live in `current-state.md`.

## Verification

The toolchain and every gate are defined once in
[docs/development/verification.md](docs/development/verification.md). Do not
copy the commands here.

- Resolve the CI compiler first: `python3 -B scripts/toolchain_gate.py`; use the
  version it reports for clippy and tests. `1.90.0` is the separate MSRV check.
- Tier 1 is blocking on every change. Tier 2 runs for its relevant surface.
- A gate that could not run is **unrun**, not passed. Record the reason.
- A known-red gate is in `current-state.md`; report it, do not silently fix an
  unrelated red gate.

## Commits

- Commit when the change is verified; do not wait to be asked.
- One file or one module per commit; independently understandable and green for
  its scope. Do not batch unrelated files into one commit.
- Format: `<type>(<scope>): <imperative summary>`, lowercase, at most 72
  characters. The validator is authoritative:
  `scripts/commit_contract_test.py`.
- Validate the range: `python3 -B scripts/commit_contract_test.py --base <start> --head HEAD`.
- Put plans, test transcripts and delivery status in Issues or
  `docs/development/`, never in commit subjects or long bodies.
- Inspect each committed diff and confirm unrelated staged work is preserved.

## Stop and report

Stop instead of guessing when:

- a gate fails for a reason unrelated to your change;
- the required toolchain, browser or network is unavailable;
- a citation cannot be resolved or a document contradicts the code;
- a dependency, release or ownership decision is needed.

Report what ran, what did not, and the exact failure. A green local unit-test run
is not a full green: Tier 2 and unrun platforms are part of the truth.
