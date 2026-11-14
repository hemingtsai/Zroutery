# Archive

> Authority: index · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

This directory holds documents that cannot be made current by editing: past
audits, superseded plans, execution records, historical snapshots and the
evidence registry. They are kept for provenance only.

**They are not current fact.** The current truth is
[`docs/development/current-state.md`](../development/current-state.md); the rules
are in [`docs/DOCUMENTATION.md`](../DOCUMENTATION.md). Audits and findings are
discussed in GitHub Issues, not here.

## Criteria

| Code | Meaning |
|---|---|
| A | an event that already happened; editing it would falsify history |
| B | a plan executed, abandoned or superseded |
| C | its premise no longer exists |
| D | a snapshot of one moment; updating it destroys its meaning |

## Moved documents

| Original path | Archived path | Criterion | Current replacement |
|---|---|---|---|
| `docs/development/repository-audit-2026-09-25.md` | `audits/repository-audit-2026-09-25.md` | A | Issues; `architecture.md` |
| `docs/development/routing-learning-closure-audit.md` | `audits/routing-learning-closure-audit.md` | A | `reference/ml-routing.md` |
| `docs/development/ui-reference-experience-review-2026-11-06.md` | `audits/ui-reference-experience-review-2026-11-06.md` | A | Issues |
| `docs/development/client-attribution-takeover-audit.md` | `audits/client-attribution-takeover-audit.md` | A | `reference/client-takeover.md` |
| `docs/development/account-surface-plan.md` | `plans/account-surface-plan.md` | B | Issues |
| `docs/development/backend-optimization-plan.md` | `plans/backend-optimization-plan.md` | B | Issues |
| `docs/development/ui-optimization-plan.md` | `plans/ui-optimization-plan.md` | B | Issues |
| `docs/development/upgrade-master-plan.md` | `plans/upgrade-master-plan.md` | B | `architecture.md` |
| `docs/development/roadmap.md` | `plans/roadmap.md` | B | `current-state.md`, Issues |
| `docs/development/godfile-split-plan.md` | `plans/godfile-split-plan.md` | B | Issues |
| `docs/development/backend-optimization-execution.md` | `records/backend-optimization-execution.md` | A | git history, PRs |
| `docs/development/upgrade-execution.md` | `records/upgrade-execution.md` | A | git history, PRs |
| `docs/development/ml-closed-loop-report.md` | `records/ml/ml-closed-loop-report.md` | A/D | `reference/ml-routing.md` |
| `docs/development/perf-baseline.md` | `records/perf-baseline.md` | D | `verification.md`, `ui_budget_test.py` |
| `docs/development/regression-ledger.md` | `records/regression-ledger.md` | A | Issues |
| `docs/development/upstream-sse-errors-2026-11-07.md` | `records/incidents/upstream-sse-errors-2026-11-07.md` | A | code, tests, Issue |
| `scripts/desktop_packaging_record.md` | `records/packaging-record.md` | A | `verification.md`, `packaging.yml` |
| `docs/development/history-treatment-map.md` | `history/history-treatment-map.md` | C | git history |
| `docs/development/tag-remap.md` | `history/tag-remap.md` | C | git history |
| `docs/development/dependency-dag.md` | `history/dependency-dag.md` | C | `current-state.md` |
| `docs/development/evidence-registry.md` | `history/evidence-registry.md` | D | PRs, tests, Issues |
| `docs/development/upgrade-contracts.md` | `history/upgrade-contracts.md` | B | `architecture.md`, `verification.md` |
| `docs/development/cc-switch-design-notes.md` | `records/cc-switch-design-notes.md` | B | `reference/providers.md` |
| `docs/development/newapi-adapter.md` | `records/newapi-adapter.md` | B | `reference/providers.md` |
| `docs/development/opencode-zen.md` | `records/opencode-zen.md` | B | `reference/providers.md` |
| `docs/development/codex-auto-rectifier.md` | `records/codex-auto-rectifier.md` | B | `reference/providers.md` |
| `docs/development/routing-data-collection-guide.md` | `records/routing-data-collection-guide.md` | B | `reference/routing-data.md` |
| `docs/development/skin-spec-md3-fluent.md` | `records/skin-spec-md3-fluent.md` | B | `reference/ui-design-system.md` |
| `docs/development/node-status/` (84 JSON) | `records/node-status/` | D | `current-state.md`, Issues |

## Rules

- A moved document keeps its original content and gains no edits beyond this
  index entry. It is frozen.
- Old paths resolve through [`redirects.json`](redirects.json); active documents
  and archived records keep their original references.
- The legacy node/evidence validator is retired with this migration. The JSON
  snapshots remain readable, but they are not maintained and no new records are
  added.
