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

Required P1 repairs fed the path from the side and are now accepted:

```text
Stage 2 media/capability repair ─┐
Stage 3 eligibility/identity ────┼─> 7E-1B / 7E-2B / 7E-2E / 7E-3
Stage 4 failure/stream repair ────┤
Stage 6 Outcome/Feedback bridge ─┘
```

`7E-0` remains `PARTIAL`; that decision is not erased by anything below. `7E-1A`,
`7E-1B-CORE`, and `7E-1B` are all `DONE`, so `7E-1` is `DONE` as well, and the
Core P1 split (ADR-0003 through ADR-0005) is fully accepted with the audited
stage records revalidated. `7E-2A` is `READY`: the replayable production input it
must be typed against now exists. Development continues on `dev`.

## Stage and ML inventory

| Node | State | Current decision and next safe work |
|---|---|---|
| `STAGE-1` | `DONE` | Naming/tier contracts and request-derived capability enforcement on every resolution path revalidated. |
| `STAGE-2` | `DONE` | Canonical, deduplicated capability derivation with fail-closed protocol/media handling. |
| `STAGE-3` | `DONE` | Eligibility parity on tier/direct/policy paths; planned identity is separate from the served identity on one Outcome. |
| `STAGE-4` | `DONE` | One failure authority end to end; a client disconnect or cancellation is never recorded as success. |
| `STAGE-5` | `PARTIAL` | Account contracts and unit tests exist; persistence and production ownership/execution do not. |
| `STAGE-6` | `DONE` | One validated Outcome per production request and one canonical lossless TrainingSample schema. |
| `STAGE-7` | `PARTIAL` | This aggregate contains partial foundations and blocked learning subnodes; it is not a DONE umbrella. |
| `STAGE-8` | `BLOCKED` | Requires the complete learning loop through `7H`. |
| `7A` | `PARTIAL` | Deterministic feature extraction exists; identity/schema architecture memory is incomplete. |
| `7B` | `DONE` | The store holds the canonical sample; production ingestion is once per request, validated, bounded by count and age, and refuses explicitly. |
| `7C` | `PARTIAL` | Specialist baseline models work; the typed contract now exists in `ml/decision_contract.rs` but nothing produces a decision through it. |
| `7D` | `PARTIAL` | The framework exists and its blocker is closed; holdout, calibration, and statistical gates are still unbuilt. |
| `7E-0` | `PARTIAL` | 7E-1A repaired identity/replay; durable model operations and journal remain for later nodes. |
| `7E-1` | `DONE` | All three children accepted: identity/lineage, the pure shadow seam, and the production integration that retains the decision-time input and correlates the served identity. |
| `7E-1A` | `DONE` | Identity, lineage, schema-envelope, predictor-swap, and replay substrate accepted on main. |
| `7E-1B-CORE` | `DONE` | Pure replayable observation, non-degenerate counterfactual, rejected-candidate evidence, and fail-closed store semantics accepted on main. |
| `7E-1B` | `DONE` | The production path records one counterfactual per request over the retained decision-time input and attaches the final served identity from the single validated Outcome. |
| `7E-2A` | `DONE` | The typed candidate-aware contract exists, is validated, and sits beside the accepted engine. |
| `7E-2B` | `DONE` | Offline supervised warmup: deterministic, verifiable lineage, disjoint holdout, honest verdict, and no activation path. |
| `7E-2C` | `DONE` | The reward weights are fitted from data and a UCB1 rule is evaluated offline behind a safety gate that can refuse on tail grounds. |
| `7E-2D` | `DONE` | A fitted calibrator emits a K-way distribution measured on the final vector; nothing consumes it. |
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
| `CORE-P1-MEDIA-REQ` | `DONE` | Canonical/deduplicated capability derivation and fail-closed protocol/media handling accepted |
| `CORE-P1-FAILURE-AUTHORITY` | `DONE` | `failure.rs`/`error.rs` canonical classification, impact table, and API compatibility accepted |
| `CORE-P1-ELIGIBILITY-TRACE` | `DONE` | Request eligibility on the direct/tier/policy paths, planned identity, rejection trace, and router classified-attempt adapter accepted |
| `CORE-P1-OUTCOME-FEEDBACK` | `DONE` | Outcome/Feedback schema, terminal classification, and pure lossless dataset conversion accepted |
| `CORE-P1-PIPELINE-LIFECYCLE` | `DONE` | Sole `pipeline.rs`/`server` lifecycle, final served identity, stream terminal state, and fan-out owner accepted |

Batches A, B, and C are accepted. Batch B ran as two disjoint domain repairs
(`2633092`, `e5f3874`, `9bf2911`, `b99baa4` on `dev`), the serial pipeline
integration landed as `c4188be`, and the aggregate plus the audited stage records
were revalidated with the parent review and gates recorded as E-053 through
E-063. `CORE-P1-REPAIR` is `DONE`.

The current position on the critical path: the whole observation-and-collection
spine now exists on `dev`. A per-request counterfactual correlated with the
identity that actually served is recorded, the typed candidate-aware contract is
defined and validated, and canonical training samples are collected once per
request from the retained decision-time input with bounded, observable refusal.
`7E-2B` is `DONE`, so a supervised model can be trained offline from real
collected samples with a verified commit and an honest verdict — and nothing
installs it. `7E-2C` is `DONE`, so a reward is fitted from real samples and a
selection rule is evaluated offline behind a safety gate that refuses when the
mean improved while failures, cost, tail latency, or fallbacks regressed, and
nothing installs or explores with it. `7E-2D` is `DONE` too: a calibrated K-way
distribution now exists, measured on the vector actually emitted rather than on
its inputs, and the independent route's near-perfect marginals lost 0.0667 ECE to
normalization, which is the measured reason the joint parameterization is the
claim. `7E-2E` is next and owns the durable ordered learning journal. `7D` is
unblocked and may now consume the evaluation surface this node built.
Two limitations are recorded
rather than smoothed over: a sample's features currently come from a snapshot
cloned at decision time rather than a re-read of the accepted record; and dataset
collection follows `config.shadow.enabled` because no dataset-specific
configuration exists. The accepted sample validator's attempt-scope timing gap is
now closed in the validator itself. Because every production sample carries no
feedback, any learned reward is an outcome-derived proxy rather than a user
preference, and a bandit optimizing it can diverge from what a user would have
chosen; the accepted reward basis also cannot order two requests that both
exceeded its latency clamp. No production takeover, online RL, exploration,
real-provider E2E, or automatic model activation is authorized by this roadmap.

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
| `TEST-WORKSPACE` | `DONE` | Local `cargo test --workspace` passed at the audited SHA and at `0f5c935`; it regressed at `c4188be` because a new test used an ml-gated API, and the gap is recorded in E-065. |
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

## Authorized batches

The audit authorized four non-overlapping worktrees, all accepted:

1. `7E-1A` (`DONE`) — model identity, lineage, and replay repair;
2. `BASELINE-GATE` (`DONE`) — local clippy, Windows smoke, and layout-harness repair;
3. `COMMIT-CONTRACT` (`DONE`) — CI/workflow commit evidence alignment; and
4. `ORCH-DOCS` (`DONE`) — this recoverable documentation record.

Core P1 then ran as three bounded batches: Batch A (`CORE-P1-MEDIA-REQ`,
`CORE-P1-FAILURE-AUTHORITY`), Batch B (`CORE-P1-ELIGIBILITY-TRACE`,
`CORE-P1-OUTCOME-FEEDBACK`, dispatched in parallel because their ownership was
disjoint), and the serial Batch C (`CORE-P1-PIPELINE-LIFECYCLE`, the sole owner of
the production lifecycle seam). All five nodes, the aggregate, and the audited
stage records are accepted. `7E-1B` followed as the production ML integration and
is accepted.

The next authorized work is `7E-2A`, which is `READY` on the critical path, and
`7B`, which is `READY` beside it. They own different files, so they may be
dispatched together; neither may be reported as started work before dispatch. No
production takeover, online RL, exploration, real-provider E2E, or automatic
model activation is authorized by this roadmap.

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
