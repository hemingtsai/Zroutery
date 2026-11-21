# Archive

> Authority: index · Owner: docs · Last verified: 2026-11-21 · Verified by: `scripts/docs_check.py`

This directory holds documents that cannot be made current by editing: past
audits, superseded plans, execution records, historical snapshots and the
evidence registry. They are kept for provenance only.

**They are not current fact.** The current truth is
[`docs/development/current-state.md`](../development/current-state.md); the rules
are in [`docs/DOCUMENTATION.md`](../DOCUMENTATION.md). Audits and findings are
reported to the user; Issues are opened only on request.

## Links inside archived documents

Archived prose is not edited. When a link target also moved, only the link target
is retargeted to the archived copy; the evidence and wording stay as they were.
A code span that names a document removed from the repository (not archived)
stays as history and is not rewritten. `docs_check.py` resolves every Markdown
link in this directory, so a link that is neither present nor redirected fails
CI.

## Criteria

| Code | Meaning |
|---|---|
| A | an event that already happened; editing it would falsify history |
| B | a plan executed, abandoned or superseded |
| C | its premise no longer exists |
| D | a snapshot of one moment; updating it destroys its meaning |

## No successor

Some archived documents describe a module that was deleted outright. They have no
active replacement, and inventing one would be false. The identifier is
`no-successor:<slug>`; the slug is registered in `successors.json` together with
the archived path and the reason. A missing successor is allowed only through
that registry.

| Identifier | Archived document | Why |
|---|---|---|
| `no-successor:evidence-registry` | `docs/development/evidence-registry.md` | the evidence registry process was deleted |
| `no-successor:legacy-node-orchestration` | `docs/development/node-status/` | the legacy node orchestration was deleted |

## Moved documents

| Original path | Archived path | Criterion | Current replacement |
|---|---|---|---|
| `docs/development/repository-audit-2026-09-25.md` | `audits/repository-audit-2026-09-25.md` | A | report to user; `architecture.md` |
| `docs/development/routing-learning-closure-audit.md` | `audits/routing-learning-closure-audit.md` | A | `reference/ml-routing.md` |
| `docs/development/ui-reference-experience-review-2026-11-06.md` | `audits/ui-reference-experience-review-2026-11-06.md` | A | report to user |
| `docs/development/client-attribution-takeover-audit.md` | `audits/client-attribution-takeover-audit.md` | A | `reference/client-takeover.md` |
| `docs/development/account-surface-plan.md` | `plans/account-surface-plan.md` | B | report to user |
| `docs/development/backend-optimization-plan.md` | `plans/backend-optimization-plan.md` | B | report to user |
| `docs/development/ui-optimization-plan.md` | `plans/ui-optimization-plan.md` | B | report to user |
| `docs/development/upgrade-master-plan.md` | `plans/upgrade-master-plan.md` | B | `architecture.md` |
| `docs/development/roadmap.md` | `plans/roadmap.md` | B | `current-state.md`, report to user |
| `docs/development/godfile-split-plan.md` | `plans/godfile-split-plan.md` | B | report to user |
| `docs/development/backend-optimization-execution.md` | `records/backend-optimization-execution.md` | A | git history, PRs |
| `docs/development/upgrade-execution.md` | `records/upgrade-execution.md` | A | git history, PRs |
| `docs/development/ml-closed-loop-report.md` | `records/ml/ml-closed-loop-report.md` | A/D | `reference/ml-routing.md` |
| `docs/development/perf-baseline.md` | `records/perf-baseline.md` | D | `verification.md`, `ui_budget_test.py` |
| `docs/development/regression-ledger.md` | `records/regression-ledger.md` | A | report to user |
| `docs/development/upstream-sse-errors-2026-11-07.md` | `records/incidents/upstream-sse-errors-2026-11-07.md` | A | code, tests, Issue |
| `scripts/desktop_packaging_record.md` | `records/packaging-record.md` | A | `verification.md`, `packaging.yml` |
| `docs/development/history-treatment-map.md` | `history/history-treatment-map.md` | C | git history |
| `docs/development/tag-remap.md` | `history/tag-remap.md` | C | git history |
| `docs/development/dependency-dag.md` | `history/dependency-dag.md` | C | `current-state.md` |
| `docs/development/evidence-registry.md` | `history/evidence-registry.md` | D | PRs, tests, report to user |
| `docs/development/upgrade-contracts.md` | `history/upgrade-contracts.md` | B | `architecture.md`, `verification.md` |
| `docs/development/cc-switch-design-notes.md` | `records/cc-switch-design-notes.md` | B | `reference/providers.md` |
| `docs/development/newapi-adapter.md` | `records/newapi-adapter.md` | B | `reference/providers.md` |
| `docs/development/opencode-zen.md` | `records/opencode-zen.md` | B | `reference/providers.md` |
| `docs/development/codex-auto-rectifier.md` | `records/codex-auto-rectifier.md` | B | `reference/providers.md` |
| `docs/development/routing-data-collection-guide.md` | `records/routing-data-collection-guide.md` | B | `reference/routing-data.md` |
| `docs/development/skin-spec-md3-fluent.md` | `records/skin-spec-md3-fluent.md` | B | `reference/ui-design-system.md` |
| `docs/development/agentrouter-checkin-cdp-flow.md` | `records/agentrouter-checkin-cdp-flow.md` | A | report to user |
| `docs/development/commit-integration-2026-11-14.md` | `records/commit-integration-2026-11-14.md` | A | report to user |
| `docs/development/plugin-api-design.md` | `plans/plugin-api-design.md` | B | report to user |
| `docs/development/takeover-conversion-fix-2026-11-14.md` | `records/takeover-conversion-fix-2026-11-14.md` | A | report to user |
| `docs/development/node-status/` (84 JSON) | `records/node-status/` | D | `no-successor:legacy-node-orchestration` |

## Rules

- A moved document keeps its original content and gains no edits beyond this
  index entry. It is frozen.
- Old file paths resolve through [`redirects.json`](redirects.json); the
  `docs/development/node-status/*` directory resolves by prefix. Active
  documents and archived records keep their original references.
- The legacy node/evidence validator is retired with this migration. The JSON
  snapshots remain readable, but they are not maintained and no new records are
  added.
