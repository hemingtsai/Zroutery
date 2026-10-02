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
| `STAGE-5` | `PARTIAL` | `STAGE-1` |
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
| `ACCOUNT` | `PARTIAL` | `STAGE-5` |
| `NEWAPI` | `PARTIAL` | `ACCOUNT` |
| `I2` | `PARTIAL` | none |
| `I3` | `PARTIAL` | none |
| `I4` | `FAILED` | `I2`, `I3` |
| `UI` | `DONE` | `STAGE-1` |
| `UI-LAYOUT` | `DONE` | `UI` |
| `UI-NEW-TRACKS` | `BLOCKED` | `UI`, `ACCOUNT`, `NEWAPI`, `I2`, `I3`, `I4` |
| `OBSERVABILITY` | `DONE` | none |
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
`CORE-P1-PIPELINE-LIFECYCLE` node was the sole owner of `server/pipeline.rs` and
has completed its work. All five bounded Core P1 nodes and the aggregate are
accepted, so `7E-1B` and `7B` are the next dispatchable nodes while `7E-2A`
stays blocked on `7E-1B`.

## Stage and aggregate edges

- `STAGE-1` is the capability/naming contract input to Account and the
  operational UI.
- `STAGE-7` aggregates the ML foundation nodes and the two shadow/identity
  nodes. It is `PARTIAL` because its children are mixed; it is not an
  acceptance shortcut.
- `STAGE-8` depends on both the aggregate learning surface and `7H`, so a
  library test cannot close continual policy learning.
- `UI` is independent of new backend sidecars. `UI-NEW-TRACKS` is blocked
  until those lifecycles are real; the current operational UI can remain
  `DONE` without implying that the new surfaces exist.

## Parallel-track edges

```text
STAGE-5 ─> ACCOUNT ─> NEWAPI ─┐
I2 ───────────────────────────┼─> UI-NEW-TRACKS
I3 ───────────────────────────┤
I4 (depends on I2 + I3) ──────┘
UI ─> UI-LAYOUT
```

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
- `TEST-REAL-E2E` is `BLOCKED` behind the NewAPI contract and external
  protocol/credential evidence.
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
