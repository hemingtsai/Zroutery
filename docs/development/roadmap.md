# Audited Roadmap and Node State

## How to read this roadmap

This roadmap is a recoverable view of the repository audit, not a forecast
that silently upgrades work. The canonical machine-readable records are in
`docs/development/node-status/`; the graph is in
[`dependency-dag.md`](dependency-dag.md), and gate provenance is in
[`evidence-registry.md`](evidence-registry.md). The audit input remains
[`repository-audit-2026-09-25.md`](repository-audit-2026-09-25.md).

Only these states are used: `QUEUED`, `READY`, `RUNNING`, `VALIDATING`, `DONE`,
`PARTIAL`, `BLOCKED`, `FAILED`, and `REVALIDATE`. A historical tag or commit
is not current acceptance.

## Current critical path

The safe online-learning path is:

```text
7E-1A  model identity / lineage / replay repair
  ↓
7E-1B-CORE  pure replayable shadow observation and counterfactual seam
  ↓
7E-1B  Core-integrated shadow observation closure
  ↓
7E-2A  typed candidate-aware decision contract
  ↓
7E-2B  supervised warmup with trustworthy Outcome
  ↓
7E-2C  bandit/reward learning
  ↓
7E-2D  calibration and K-way distribution
  ↓
7E-2E  durable ordered learning events and verified commits
  ↓
7E-2F  immutable snapshot and atomic activation
  ↓
7E-3   offline replay and calibrated evaluation
  ↓
7F     real-traffic shadow with packaging and observability
  ↓
7G     controlled takeover
  ↓
7H     bounded exploration
  ↓
8      continual policy learning
```

Required P1 repairs feed the path from the side:

```text
Stage 2 media/capability repair ─┐
Stage 3 eligibility/identity ────┼─> 7E-1B / 7E-2B / 7E-2E / 7E-3
Stage 4 failure/stream repair ────┤
Stage 6 Outcome/Feedback bridge ─┘
```

`7E-0` is `PARTIAL`, `7E-1` is `PARTIAL`, and `7E-2A` is `BLOCKED`; the repair
nodes do not erase those decisions. `7E-1A` and `7E-1B-CORE` are now `DONE`;
full `7E-1B` remains blocked by the recorded Core P1 repair dependencies. The
read-only Core P1 split is accepted as ADR-0003 through ADR-0005; Batch A is
accepted and Batch B is the only currently authorized parallel implementation
batch.

## Stage and ML inventory

| Node | State | Current decision and next safe work |
|---|---|---|
| `STAGE-1` | `REVALIDATE` | Naming/tier tests pass, but capability enforcement needs P1 repair and revalidation. |
| `STAGE-2` | `FAILED` | Unknown media and capability derivation can lose or bypass requirements; repair before relying on derived policy input. |
| `STAGE-3` | `FAILED` | Main policy/direct-ID capability checks and final-served identity are not trustworthy. |
| `STAGE-4` | `FAILED` | Failure authority conflicts and client stream disconnect can be recorded as success. |
| `STAGE-5` | `PARTIAL` | Account contracts and unit tests exist; persistence and production ownership/execution do not. |
| `STAGE-6` | `FAILED` | No production `Outcome`/`Feedback` bridge and incompatible training schemas remain. |
| `STAGE-7` | `PARTIAL` | This aggregate contains partial foundations and blocked learning subnodes; it is not a DONE umbrella. |
| `STAGE-8` | `BLOCKED` | Requires the complete learning loop through `7H`. |
| `7A` | `PARTIAL` | Deterministic feature extraction exists; identity/schema architecture memory is incomplete. |
| `7B` | `BLOCKED` | Dataset library exists but cannot be treated as trustworthy without production Outcome. |
| `7C` | `PARTIAL` | Specialist baseline models work; typed candidate-aware decision contracts do not. |
| `7D` | `PARTIAL` | Evaluation framework exists; holdout, calibration, and statistical gates are incomplete. |
| `7E-0` | `PARTIAL` | 7E-1A repaired identity/replay; durable model operations and journal remain for later nodes. |
| `7E-1` | `PARTIAL` | Purity/determinism foundations pass; production counterfactual and replay closure fail. |
| `7E-1A` | `DONE` | Identity, lineage, schema-envelope, predictor-swap, and replay substrate accepted on main. |
| `7E-1B-CORE` | `DONE` | Pure replayable observation, non-degenerate counterfactual, rejected-candidate evidence, and fail-closed store semantics accepted on main. |
| `7E-1B` | `BLOCKED` | Pure seam accepted; full integration still requires Core eligibility, failure, stream, and Outcome repairs. |
| `7E-2A` | `BLOCKED` | Requires replayable input and separate final-served identity. |
| `7E-2B` | `BLOCKED` | Requires 7E-2A and the Stage 6 Outcome bridge. |
| `7E-2C` | `BLOCKED` | Requires supervised warmup, Dataset, and RewardPolicy. |
| `7E-2D` | `BLOCKED` | Requires a trustworthy K-way distribution. |
| `7E-2E` | `BLOCKED` | Requires durable ordered `LearningEvent` and verified `ModelCommit`. |
| `7E-2F` | `BLOCKED` | Requires verified immutable snapshots and atomic activation. |
| `7E-3` | `BLOCKED` | Requires calibrated offline replay/evaluation gates. |
| `7F` | `BLOCKED` | Requires an accepted commit, shipping reachability, and observability. |
| `7G` | `BLOCKED` | Requires stable real-traffic shadow evidence and rollback. |
| `7H` | `BLOCKED` | Requires stable takeover, budget, monitoring, and rollback. |

## Core P1 repair split

The read-only audit produced ADR-0003 through ADR-0005 and five bounded nodes.
`server/pipeline.rs` has one owner only.

| Node | State | Boundary |
|---|---|---|
| `CORE-P1-MEDIA-REQ` | `DONE` | Canonical/deduplicated capability derivation and fail-closed protocol/media handling accepted on main |
| `CORE-P1-FAILURE-AUTHORITY` | `DONE` | `failure.rs`/`error.rs` canonical classification, impact table, and API compatibility accepted |
| `CORE-P1-ELIGIBILITY-TRACE` | `READY` | `policy.rs`/`router.rs` request eligibility, planned identity, and router adapter |
| `CORE-P1-OUTCOME-FEEDBACK` | `READY` | Outcome/Feedback schema and pure dataset conversion; no training/activation |
| `CORE-P1-PIPELINE-LIFECYCLE` | `QUEUED` | Sole `pipeline.rs`/`server` lifecycle, final served identity, stream terminal state, and fan-out owner |

Batch A (`CORE-P1-MEDIA-REQ` and `CORE-P1-FAILURE-AUTHORITY`) is accepted on
main. Batch B is the next parallel implementation batch: the two `READY`
domain nodes have disjoint file ownership. Batch C is serial and owns the
production pipeline seam. The aggregate `CORE-P1-REPAIR` remains `QUEUED`
until Batch B, Batch C, and Stage 2/3/4/6 revalidation pass.

## Parallel engineering inventory

| Node | State | Boundary and next safe work |
|---|---|---|
| `ACCOUNT` | `PARTIAL` | Generic types/store/calculations are useful; add persistence and runtime ownership without changing ML feature order. |
| `NEWAPI` | `PARTIAL` | Build a local mock-server auth, expiry, refresh, usage, and quota contract before any real protocol E2E. |
| `I2` | `PARTIAL` | Introduce a typed action runner, owned process/config/auth contract, and local child-process fixtures. |
| `I3` | `PARTIAL` | Add a durable ownership manifest, conflict application, confirmation flow, and CAS restore fixtures. |
| `I4` | `FAILED` | Make restore/rollback apply conflict resolution and report restoration failures accurately. |
| `UI` | `DONE` | Existing provider/model/routing/activity/settings UI builds and is integrated. |
| `UI-LAYOUT` | `DONE` | Current fixture, real browser assertions, and fail-closed missing-browser behavior pass. |
| `UI-NEW-TRACKS` | `BLOCKED` | Backend lifecycles must be real and owned before Account, migration, takeover, shadow, or ML surfaces are exposed. |
| `OBSERVABILITY` | `READY` | A read-only runtime projection can be built without changing ML schemas. |

## Gate inventory

| Gate | State | Scope and caveat |
|---|---|---|
| `TEST-CHECK` | `DONE` | Local `cargo check --workspace` passed at the audited SHA with one unused-variable warning. |
| `TEST-WORKSPACE` | `DONE` | Local `cargo test --workspace` passed at the audited SHA. |
| `TEST-ML` | `DONE` | Local `cargo test --workspace --features ml` passed at the audited SHA. |
| `TEST-ALL-FEATURES` | `DONE` | Local `cargo test --workspace --all-features` passed at the audited SHA. |
| `TEST-CLIPPY` | `DONE` | Full workspace all-target/all-feature clippy passed after the baseline repair. |
| `TEST-UI-BUILD` | `DONE` | Local `pnpm --dir ui build` passed at the audited SHA. |
| `TEST-SMOKE` | `DONE` | Windows native binary resolution and local mock-provider lifecycle passed. |
| `TEST-LAYOUT-SELF` | `DONE` | Seven pure layout self-tests passed. |
| `TEST-LAYOUT-BROWSER` | `DONE` | Real Chrome layout assertions passed; missing browser now fails closed. |
| `TEST-FORMAT` | `FAILED` | `cargo fmt --all -- --check` found pre-existing formatting drift. |
| `TEST-DIFF-CHECK` | `DONE` | `git diff --check` passed at the audited baseline. |
| `CI-CORE` | `DONE` | External run `36014406507` passed Linux core all-feature clippy/tests only. |
| `CI-DESKTOP` | `DONE` | External run `36014406507` passed a macOS workspace check only. |
| `TEST-PACKAGING` | `QUEUED` | No packaging gate was run in the audit; it remains unverified. |
| `TEST-REAL-E2E` | `BLOCKED` | External protocol fixtures, credentials, and a real lifecycle contract are absent. |
| `TEST-COMMIT-CONTRACT` | `DONE` | Deterministic validator, compatibility scopes, global statuses, and real Git range passed. |

External CI is not a substitute for the local failed or unrun gates. The
full command inventory and caveats are canonical in the evidence registry.

## Orchestrator infrastructure

The orchestration records are split into stable responsibilities:

- `ORCH-INFRA` — defines the record/evidence contract;
- `ORCH-STATUS` — owns one JSON record per node;
- `ORCH-EVIDENCE` — owns exact HEAD, command, result, scope, and caveat data;
- `ORCH-REGRESSION` — owns `REG-001` through `REG-010`;
- `ORCH-ARCHITECTURE` — owns the Core/ML/sidecar boundary;
- `ORCH-DAG` — owns edge validation and the critical path;
- `ORCH-ROADMAP` — owns sequencing and unblock conditions;
- `ORCH-VALIDATION` — runs JSON, inventory, DAG, enum, path, ADR, and diff
  checks;
- `ORCH-DOCS` — the completed documentation-only batch that composes those
  records.

The status files are initially written as a documentation batch and may only
move to `DONE` after their evidence and validation IDs pass. This record does
not change implementation status for any audited node.

## Authorized first batch

The audit authorizes four non-overlapping worktrees:

1. `7E-1A` (`DONE`) — model identity, lineage, and replay repair;
2. `BASELINE-GATE` (`DONE`) — local clippy, Windows smoke, and layout-harness repair;
3. `COMMIT-CONTRACT` (`DONE`) — CI/workflow commit evidence alignment; and
4. `ORCH-DOCS` (`DONE`) — this recoverable documentation record.

The next authorized work is Core P1 Batch B: `CORE-P1-ELIGIBILITY-TRACE` and
`CORE-P1-OUTCOME-FEEDBACK` are both `READY` and may run in isolated worktrees
with disjoint ownership. The pure `7E-1B-CORE` seam and both Batch A domain
contracts are accepted, but full `7E-1B` remains `BLOCKED` until Batch B, the
sole pipeline integration, and Stage 2/3/4/6 revalidation pass. No production
takeover, online RL, exploration, real-provider E2E, or automatic model
activation is authorized by this roadmap.

## Unresolved decisions

- [ADR-0001](decisions/0001-recoverable-node-records.md): approve or revise
  the versioned node-record schema and ownership rules.
- [ADR-0002](decisions/0002-production-ml-boundary.md): choose the future
  deployment boundary for online ML and activation.
- [ADR-0003](decisions/0003-capability-media-fail-closed.md): accepted for the
  Core P1 capability/media boundary; revisit only with new evidence.
- [ADR-0004](decisions/0004-failure-outcome-authority.md): accepted for the
  Core P1 failure/Outcome boundary; revisit only with new evidence.
- [ADR-0005](decisions/0005-core-p1-node-boundaries.md): accepted for the
  current dispatch split and sole pipeline ownership rule.

## Acceptance rule for future nodes

A future worker must report `IMPLEMENTATION_STATE`, `ACTUAL_GAP`,
`FILES_TO_CHANGE`, `DEPENDENCY_RISKS`, and `PLAN` before editing. A worker may
claim `DONE` only when every required gate passes, local and external evidence
are distinguished, referenced paths parse/exist, and the final diff is checked.
Otherwise the worker must use `PARTIAL`, `BLOCKED`, `FAILED`, `REVALIDATE`, or
another legal state that describes the evidence.
