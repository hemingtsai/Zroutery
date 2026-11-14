# Documentation Rules Reference

> Authority: reference · Owner: docs · Last verified: 2026-11-14 · Verified by: `python3 -B scripts/docs_rules.py`

This page registers every documentation rule and the check that enforces it.
`docs/rules.json` is the machine source; `scripts/docs_rules.py` checks that the
registered set and the executed set are the same. The executed list is
`scripts/docs_contracts.py::STEPS`.

## Rules

| ID | Rule | Enforced by | Type |
|---|---|---|---|
| DOC-01 | Source anchors resolve | `documentation sources` | machine |
| DOC-02 | Generated files and links are current | `documentation check` | machine |
| DOC-03 | ADR header, date, sections and sources | `documentation check` | machine |
| DOC-04 | Canonical homes resolve | `documentation check` | machine |
| DOC-05 | Markdown tables and fences are well formed | `markdown structure` | machine |
| DOC-06 | Three READMEs have the same shape | `documentation parity` | machine |
| DOC-07 | State matches its schema | `state schema` | machine |
| DOC-08 | State render matches state | `rendered state` | machine |
| DOC-09 | Invariants have test evidence | `invariant evidence` | machine |
| DOC-10 | Top-level modules have coverage | `module coverage` | machine |
| DOC-11 | Sources did not change after review | `freshness` | machine |
| DOC-12 | Changed sources force affected docs | `documentation impact (range)` | machine |
| DOC-13 | Every executed gate is registered | `rules registry` | machine |
| DOC-14 | Commit is blocked locally and in CI | pre-commit hook + required CI check | manual |
| DOC-15 | Reference prose matches code | second-reader sample of three sentences | manual |

## Failure modes

A rule with no step, or a step with no rule, fails `docs_rules.py`. A manual
rule is listed explicitly rather than silently omitted.

## Not implemented

Manual rules are not automated. DOC-15 is a review protocol, not a gate.

## Sources

- `docs/rules.json`
- `scripts/docs_rules.py`
- `scripts/docs_contracts.py`
