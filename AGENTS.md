# Repository guidance

> Authority: normative · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

Read [state.json](docs/development/state.json) first: it is the machine truth,
validated by `state.schema.json`. [current-state.md](docs/development/current-state.md)
is its rendered view. If this file and the state disagree, the state wins for
facts; this file wins for permission and process.

## How to use this documentation

- Start with [state.json](docs/development/state.json) and
  [current-state.md](docs/development/current-state.md); they are the current facts.
- This file owns permission and process; [DOCUMENTATION.md](docs/DOCUMENTATION.md)
  owns document rules; [verification.md](docs/development/verification.md) owns
  gates; [INDEX.md](docs/INDEX.md) owns reading paths.
- Before changing code, inspect citations with `python3 -B scripts/docs_sources.py
  --index` and the affected set with `python3 -B scripts/docs_impact.py --base
  <start> --head HEAD`.
- Before committing, follow Definition of done below.

## Definition of done

Before each commit run `docs_contracts.py`, `current_state.py --check` and
`commit_contract_test.py --base origin/dev --head HEAD`. A red gate is fixed in
the document or trigger; if the state differs, run `record_state.py --no-range`
and commit it. Never weaken a gate or use `--no-verify`.

## Layout

- `crates/zroutery-core/`: protocol codecs, routing, accounts, accounting, storage, optional ML.
- `src-tauri/`: lifecycle, native integrations, credential store, IPC commands.
- `crates/zroutery-headless/`: the same Core and desktop state without a GUI.
- `ui/`: React/TypeScript frontend and typed mock bridge.
- `docs/`: current documentation; `docs/archive/` is history, never current fact.

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

## Other agents

- Respect ownership: `crates/` and `src-tauri/` are backend; `ui/` is frontend.
- Shared files need coordination: manifests, `Cargo.lock`, `ui/src/api.ts`,
  `ui/mock-bridge.ts`, the three i18n dictionaries, fixtures and
  `docs/docs.manifest.json`.
- Re-read a shared file immediately before patching it; stage explicit paths; one
  owner publishes commits at a time. Delegate only when authorized.

## Implementation contracts

[reference/implementation-contracts.md](docs/development/reference/implementation-contracts.md)
owns the IPC, lifecycle, takeover, platform and UI boundaries. This entry does
not restate them.

## Documentation is part of the change

- Every change updates every affected document in the same change. The affected
  set is computed: `python3 -B scripts/docs_impact.py --base <start> --head HEAD`.
  There is no `none` escape; fix the manifest or citation if a trigger is wrong.
- Every factual sentence in a normative or reference document cites a source
  read in the same change (`path::symbol` or `path#Lx`). Old documents are leads,
  never sources. See [docs/DOCUMENTATION.md](docs/DOCUMENTATION.md).
- Do not open GitHub Issues proactively. Report findings to the user; open an
  Issue only when explicitly asked. An audit that yields a decision writes an
  ADR; one that yields a durable fact updates the relevant document.
- `state.json` may carry a one-line status projection; `current_state.py`
  renders it. A finding is reported to the user, not filed unless asked.
- Node status, the evidence registry and execution records are archived legacy
  history. Do not add records there.

## Verification

Gate definitions and the toolchain live in
[verification.md](docs/development/verification.md). `state.json` records the
current status and the command used for that snapshot; it does not redefine the
gate.

- Resolve the CI compiler first; `verification.md` names the resolver and version.
  `1.90.0` is the separate MSRV check.
- Tier 1 blocks every change. Tier 2 is surface-specific; `verification.md` says
  which rows block a CI job and which are local-only.
- A gate that cannot run, or times out, is **unrun**, never a pass. Record it.
- A known-red gate is in `current-state.md`; report it, do not silently fix an
  unrelated red gate.

## Commits

- Commit when the change is verified; do not wait to be asked.
- One file or one module per commit; independently understandable and green.
- Format: `<type>(<scope>): <imperative summary>`, lowercase, at most 72
  characters. `scripts/commit_contract_test.py` is authoritative.
- Validate the range: `python3 -B scripts/commit_contract_test.py --base <start> --head HEAD`.
- Plans, test transcripts and delivery status belong in `docs/development/` or
  the conversation, never in commit subjects or long bodies.

## Stop and report

Stop instead of guessing when a gate fails for an unrelated reason, the required
toolchain/browser/network is unavailable, a citation cannot be resolved, a
document contradicts the code, or a dependency/release/ownership decision is
needed. Report what ran, what did not, and the exact failure. A green local unit
run is not a full green.
