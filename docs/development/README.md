# Development Record

> Authority: index · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

This directory holds the current development documentation. The generated map is
[../INDEX.md](../INDEX.md); the rules for writing it are
[../DOCUMENTATION.md](../DOCUMENTATION.md).

## Reading order

1. [state.json](state.json) — the machine truth, validated by
   [state.schema.json](state.schema.json); [current-state.md](current-state.md)
   is its rendered view.
2. [../../AGENTS.md](../../AGENTS.md) — what an agent may do.
3. [verification.md](verification.md) — the only toolchain and gate definitions.
4. [architecture.md](architecture.md) — layers and invariants.
5. [WORKFLOW.md](WORKFLOW.md) — commit and evidence rules.

## What lives here

| Path | Holds |
|---|---|
| `decisions/` | accepted ADRs; superseded records stay in place |
| `reference/` | one page per subsystem, source-verified |
| `state.json` | machine facts; the only editable status source |
| `state.schema.json` | the schema state.json is validated against |
| `current-state.md` | generated render of state.json; never hand-edited |
| `verification.md` | canonical gates and CI coverage |
| `architecture.md` | boundaries and invariants |

Historical audits, plans, execution records, node status and the evidence
registry live under `docs/archive/`. They are not current fact; audits and
findings are reported to the user; Issues are opened only on request.

## Adding a document

- Pick one type from `docs/DOCUMENTATION.md`; do not add an audit or a plan.
- Every factual sentence cites a source you read; run
  `python3 -B scripts/docs_sources.py --check`.
- Regenerate the index with `python3 -B scripts/docs_check.py --write-index`.
- The source hash ledger is `docs/verified.json`; a changed source forces a
  re-read.
