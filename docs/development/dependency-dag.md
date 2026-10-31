# Dependency DAG

## Purpose and validation contract

This graph turns the audit matrix into a recoverable dependency graph. The
JSON records under `docs/development/node-status/` are the machine-readable
source for node state. This document explains the edges and the critical path;
it does not override a node's evidence.

For every node:

- `dependencies` lists upstream node IDs;
- `dependents` is the exact reverse edge set;
- `blocked_by` is a subset of dependencies naming an active unresolved gate;
- `unblock_condition` is a concrete acceptance condition.

The graph is acyclic by construction. A failed or blocked node can have a
repair successor, but a repair successor must not be made a prerequisite of the
failed node. Historical tags are not graph edges.

## Complete node inventory

| Node | State | Dependencies |
|---|---|---|
| `STAGE-1` | `DONE` | `CORE-P1-REPAIR` |
| `STAGE-2` | `DONE` | `CORE-P1-REPAIR` |
| `STAGE-3` | `DONE` | `CORE-P1-REPAIR` |
| `STAGE-4` | `DONE` | `CORE-P1-REPAIR` |
| `STAGE-5` | `DONE` | `STAGE-1` |
| `STAGE-6` | `DONE` | `CORE-P1-REPAIR` |
| `STAGE-7` | `PARTIAL` | `STAGE-6`, `7A`, `7B`, `7C`, `7D`, `7E-0`, `7E-1` |
| `STAGE-8` | `BLOCKED` | `STAGE-7`, `7H` |
| `7A` | `PARTIAL` | `STAGE-4`, `STAGE-5` |
| `7B` | `DONE` | `STAGE-6`, `7A` |
| `7C` | `PARTIAL` | `7A` |
| `7D` | `DONE` | `7B`, `7C` |
| `7E-0` | `PARTIAL` | `7C` |
| `7E-1` | `DONE` | `7E-0`, `CORE-P1-REPAIR`, `STAGE-3`, `STAGE-4`, `STAGE-6` |
| `7E-1A` | `DONE` | `7E-0`, `7A`, `7C` |
| `7E-1B-CORE` | `DONE` | `7E-1A`, `7A`, `7C` |
| `7E-1B` | `DONE` | `7E-1A`, `7E-1B-CORE`, `CORE-P1-REPAIR`, `STAGE-3`, `STAGE-4`, `STAGE-6` |
| `7E-2A` | `DONE` | `7E-1B` |
| `7E-2B` | `DONE` | `7E-2A`, `STAGE-6` |
| `7E-2C` | `DONE` | `7E-2B` |
| `7E-2D` | `DONE` | `7E-2C` |
| `7E-2E` | `DONE` | `7E-2D`, `7E-0` |
| `7E-2F` | `DONE` | `7E-2E` |
| `7E-3` | `DONE` | `7E-2F`, `STAGE-3`, `STAGE-4`, `STAGE-6` |
| `7F` | `PARTIAL` | `7E-3`, `TEST-PACKAGING`, `OBSERVABILITY` |
| `7G` | `BLOCKED` | `7F` |
| `7H` | `BLOCKED` | `7G` |
| `ACCOUNT` | `DONE` | `STAGE-5` |
| `NEWAPI` | `PARTIAL` | `ACCOUNT` |
| `I2` | `DONE` | none |
| `I3` | `DONE` | none |
| `I4` | `DONE` | none |
| `UI` | `DONE` | `STAGE-1` |
| `UI-LAYOUT` | `DONE` | `UI` |
| `UI-NEW-TRACKS` | `DONE` | `UI`, `ACCOUNT`, `NEWAPI`, `I2`, `I3`, `I4` |
| `OBSERVABILITY` | `DONE` | none |
| `CLIENT-IDENTITY` | `DONE` | none |
| `OPENCODE-ZEN-FREE` | `DONE` | `CLIENT-IDENTITY` |
| `OPENCODE-CONSOLE` | `DONE` | `CLIENT-IDENTITY` |
| `CORE-P1-REPAIR` | `DONE` | `CORE-P1-MEDIA-REQ`, `CORE-P1-FAILURE-AUTHORITY`, `CORE-P1-ELIGIBILITY-TRACE`, `CORE-P1-OUTCOME-FEEDBACK`, `CORE-P1-PIPELINE-LIFECYCLE` |
| `BASELINE-GATE` | `DONE` | `TEST-CLIPPY`, `TEST-SMOKE`, `TEST-LAYOUT-BROWSER` |
| `CORE-P1-MEDIA-REQ` | `DONE` | none |
| `CORE-P1-FAILURE-AUTHORITY` | `DONE` | none |
| `CORE-P1-ELIGIBILITY-TRACE` | `DONE` | `CORE-P1-MEDIA-REQ`, `CORE-P1-FAILURE-AUTHORITY` |
| `CORE-P1-OUTCOME-FEEDBACK` | `DONE` | `CORE-P1-FAILURE-AUTHORITY` |
| `CORE-P1-PIPELINE-LIFECYCLE` | `DONE` | `CORE-P1-MEDIA-REQ`, `CORE-P1-FAILURE-AUTHORITY`, `CORE-P1-ELIGIBILITY-TRACE`, `CORE-P1-OUTCOME-FEEDBACK` |
| `COMMIT-CONTRACT` | `DONE` | none |
| `TEST-CHECK` | `DONE` | none |
| `TEST-WORKSPACE` | `DONE` | `TEST-CHECK` |
| `TEST-ML` | `DONE` | `TEST-WORKSPACE` |
| `TEST-ALL-FEATURES` | `DONE` | `TEST-ML` |
| `TEST-CLIPPY` | `DONE` | `TEST-CHECK` |
| `TEST-UI-BUILD` | `DONE` | none |
| `TEST-SMOKE` | `DONE` | `TEST-CHECK` |
| `TEST-LAYOUT-SELF` | `DONE` | `TEST-UI-BUILD` |
| `TEST-LAYOUT-BROWSER` | `DONE` | `TEST-LAYOUT-SELF` |
| `TEST-FORMAT` | `DONE` | none |
| `TEST-DIFF-CHECK` | `DONE` | none |
| `CI-CORE` | `DONE` | none |
| `CI-DESKTOP` | `DONE` | none |
| `TEST-PACKAGING` | `DONE` | `CI-DESKTOP` |
| `TEST-REAL-E2E` | `BLOCKED` | `NEWAPI` |
| `TEST-COMMIT-CONTRACT` | `DONE` | `COMMIT-CONTRACT` |
| `ORCH-INFRA` | `DONE` | none |
| `ORCH-STATUS` | `DONE` | `ORCH-INFRA` |
| `ORCH-EVIDENCE` | `DONE` | `ORCH-INFRA` |
| `ORCH-REGRESSION` | `DONE` | `ORCH-INFRA` |
| `ORCH-ARCHITECTURE` | `DONE` | `ORCH-INFRA` |
| `ORCH-DAG` | `DONE` | `ORCH-INFRA` |
| `ORCH-ROADMAP` | `DONE` | `ORCH-INFRA` |
| `ORCH-VALIDATION` | `DONE` | `ORCH-STATUS`, `ORCH-EVIDENCE`, `ORCH-REGRESSION`, `ORCH-ARCHITECTURE`, `ORCH-DAG`, `ORCH-ROADMAP` |
| `ORCH-DOCS` | `DONE` | `ORCH-VALIDATION` |
| `UI-UX-U10` | `DONE` | `UI` |
| `UI-UX-U3` | `DONE` | `UI-NEW-TRACKS` |
| `UI-UX-U6` | `QUEUED` | `UI-NEW-TRACKS` |
| `UI-UX-U7` | `DONE` | `UI-NEW-TRACKS` |
| `UI-UX-U12` | `DONE` | `UI-NEW-TRACKS` |
| `UI-UX-U14` | `DONE` | `UI-NEW-TRACKS` |
| `UI-UX-U13` | `DONE` | `UI-NEW-TRACKS` |
| `UI-UX-M1` | `DONE` | `UI-LAYOUT` |
| `UI-UX-M2` | `DONE` | `UI-UX-M1`, `UI-UX-U12` |
| `UI-UX-M3` | `DONE` | `UI-UX-M1` |
| `UI-UX-P2` | `DONE` | `UI-NEW-TRACKS`, `TEST-UI-BUILD` |
| `BACKEND-OPTIMIZATION` | `PARTIAL` | none |

The JSON `dependents` arrays are generated as the exact reverse of this
inventory. The validator checks that symmetry, known IDs, legal statuses,
blocked-by membership, and topological acyclicity.

## Critical-path edges

```text
CORE-P1-MEDIA-REQ ───────────────┐
                                 ├─> CORE-P1-ELIGIBILITY-TRACE ─┐
CORE-P1-FAILURE-AUTHORITY ───────┤                              │
                                 └─> CORE-P1-OUTCOME-FEEDBACK ──┼─> CORE-P1-PIPELINE-LIFECYCLE
                                                                    │
                                                                    └─> CORE-P1-REPAIR ─┬─> STAGE-1 ─> STAGE-5 ─> ACCOUNT ─> NEWAPI
                                                                                       ├─> STAGE-2
                                                                                       ├─> STAGE-3 ───────────────┐
                                                                                       ├─> STAGE-4 ───────────────┼─> 7E-1B
                                                                                       └─> STAGE-6 ───────────────┘

7C ─> 7E-0 ─> 7E-1A ─> 7E-1B-CORE ─> 7E-1B ─> 7E-2A ─> 7E-2B
                                      │
                                      └─> 7E-2C ─> 7E-2D ─> 7E-2E ─> 7E-2F
                                                                       │
                                                                       └─> 7E-3
                                                                            │
                                                            TEST-PACKAGING ─┘
                                                                            │
                                                                         7F ─> 7G ─> 7H ─> STAGE-8
```

The Stage 3, Stage 4, and Stage 6 repair edges are intentionally repeated in
`7E-1B`, `7E-2B`/`7E-2E`, and `7E-3`: an offline ML gate is not valid if its
Outcome, cancellation, or candidate identity inputs are untrustworthy. Those
edges stay in the graph after revalidation because the dependency is real, not
because it is still blocking; only the `blocked_by` sets changed. The
`CORE-P1-PIPELINE-LIFECYCLE` node was the sole owner of `server/pipeline.rs`,
since split into `server/pipeline/`, and has completed its work. All five
bounded Core P1 nodes and the aggregate are accepted. The ML chain has since
closed `7E-2A` through `7E-3`; its frontier is `7F`, which is `PARTIAL` on one
of its four gates — the accepted model's statistical volume — with `blocked_by`
empty, and `7G`, `7H` and `STAGE-8` remain blocked behind it.

## Stage and aggregate edges

- `STAGE-1` is the capability/naming contract input to Account and the
  operational UI.
- `STAGE-7` aggregates the ML foundation nodes and the two shadow/identity
  nodes. It is `PARTIAL` because its children are mixed; it is not an
  acceptance shortcut.
- `STAGE-8` depends on both the aggregate learning surface and `7H`, so a
  library test cannot close continual policy learning.
- `UI` is independent of new backend sidecars. `UI-NEW-TRACKS` is no longer
  blocked on upstream — `ACCOUNT`, `NEWAPI`, `I2`, `I3` and `I4` all pass their
  gates and its `blocked_by` is empty — but it is `PARTIAL` on its own three
  gates. Takeover, account, check-in and ML all have commands and surfaces; the
  migration engine did not, and now does through the cutover page. What remains
  open is narrower than the sentence this replaced claimed: the cutover page is
  declared coverage with fixtures, and the one path it cannot exercise is a run
  that reaches `Completed`, which needs a live gateway to answer `/health`. The
  `rolled_back`-reports-false decision, which this record had listed as an open
  confirmation question, was taken in code (E-118) and is now a rendering
  decision the panel makes by branching on the field rather than on the state.
  `UI` is `DONE` on the three gates it owns, and since the integration
  recorded in E-143 it does mount the learning panel, the shadow replay and the
  per-account maintenance row — so `DONE` there means the surface builds and is
  integrated, not that the new tracks exist.

## Parallel-track edges

```text
STAGE-5 ─> ACCOUNT ─> NEWAPI ─┐
I2 ───────────────────────────┼─> UI-NEW-TRACKS
I3 ───────────────────────────┤
I4 (depends on I2 + I3) ──────┘
UI ─> UI-LAYOUT
```

## Frontend queue (UI-UX)

```text
UI ─────────────────> UI-UX-U10 (toast dedup)              DONE
UI-NEW-TRACKS ──────┬─> UI-UX-U3 (usage KPI row)           DONE
                    ├─> UI-UX-U6 (copy and empty states)
                    ├─> UI-UX-U7 (chart hover cards)
                    ├─> UI-UX-U12 (usage split: monitor vs ledger)          DONE
                    ├─> UI-UX-U14 (stream facet filters)
                    ├─> UI-UX-U13 (copy slim-down)
                    └─> UI-UX-P2 (perf baseline, also behind TEST-UI-BUILD)
UI-LAYOUT ─> UI-UX-M1 (simple and geek modes)
                 └─> UI-UX-M2 (simple home workbench)
                 └─> UI-UX-M3 (skins: md3 / fluent)
```

These seven nodes carry the frontend queue from
[`ui-optimization-plan.md`](ui-optimization-plan.md) (batches U, M, P) and its
design inputs from [`cc-switch-design-notes.md`](cc-switch-design-notes.md).
Their edges are data dependencies only — the **execution order** (U10 → U3 →
U7 → U12 → U6 → P2 baseline, M1 last because it restyles every surface) is
maintained by the frontend owner and is deliberately not encoded as graph
edges, so a reordering never has to rewrite the graph. `UI-UX-M1` depends on `UI-LAYOUT` for a real
reason: mode switching changes the sidebar inventory and folds forms, and the
layout harness must describe those surfaces before the feature can pass.
Backend-gated items (batch P1: the P0 fixes' UI faces) are not nodes here yet —
they join when the backend P0 work has DAG nodes to depend on.

`I2` and `I3` have no Core implementation dependency because their local
state-machine and adapter seams are independently testable. `I4` cannot be
claimed until both ownership and migration inputs are real and conflict-safe.

## Gate edges

- Local test results form a provenance chain only; a failed local gate is not
  hidden by a later successful test command.
- `BASELINE-GATE` consumes the three reproduced executable failures:
  `TEST-CLIPPY`, `TEST-SMOKE`, and `TEST-LAYOUT-BROWSER`.
- `CI-CORE` and `CI-DESKTOP` are external evidence nodes with no local
  dependency. Their scope is recorded in the evidence registry.
- `TEST-PACKAGING` and `OBSERVABILITY` are both `DONE`, so every recorded
  blocker on `7F` is cleared.
- `TEST-REAL-E2E` is `BLOCKED` behind external protocol and credential
  evidence. The NewAPI half of that blocker is satisfied: the contract passes
  its gates. What is missing is a real credential and a real panel, so the
  adapter's integration is unproven rather than unimplemented, and those are
  different sentences.
- `TEST-COMMIT-CONTRACT` is `QUEUED` behind the separate
  `COMMIT-CONTRACT` repair node.

## Orchestrator edges

```text
ORCH-INFRA
  ├─> ORCH-STATUS
  ├─> ORCH-EVIDENCE
  ├─> ORCH-REGRESSION
  ├─> ORCH-ARCHITECTURE
  ├─> ORCH-DAG
  └─> ORCH-ROADMAP
          └─> ORCH-VALIDATION ─> ORCH-DOCS
```

The documentation nodes are downstream of the record contract, and
`ORCH-DOCS` is downstream of validation. They do not create implementation
dependencies or claim that any audited node passed a missing gate.

## Edge validation procedure

The checks are owned by a repository script so they are reproducible from any
checkout instead of a scratch path:

```bash
python -B scripts/orch_docs_test.py --self-test
python -B scripts/orch_docs_test.py
```

The first command proves that every rejection rule actually rejects a synthetic
tree; the second validates the real records. Together they:

1. parse every `*.status.json` file against the exact record schema;
2. verify the complete inventory in this document and the status directory, and
   that each DAG state matches its record;
3. reject unknown IDs, self edges, asymmetric dependency/dependent sets,
   blocked-by values outside dependencies, illegal states, and cycles;
4. verify every `source_references` path exists;
5. verify the ADR inventory, ADR references, the regression ledger sequence,
   the evidence rows referenced by records, and the development Markdown links;
6. run `git diff --check` and confirm the diff is limited to allowed files.

The historical command/result records are E-017 through E-023 and E-052 in
[`evidence-registry.md`](evidence-registry.md).
