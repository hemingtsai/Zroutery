# Zroutery Repository Audit — 2026-09-25

## Purpose

This record consolidates five independent read-only audits into a recoverable
orchestrator state. It records evidence, not intentions. Historical tags and
commit messages are provenance; they do not override current implementation,
tests, or gates.

## Repository Baseline

- HEAD: `c66072f341664f67c1cd1c761e5f5adf1980ce16`
- Branch: `main`
- Upstream: `origin/main`
- Merge shape: UI/main line `cd43f1b` merged with shadow/dev line `763d87b`
- Working tree at audit start: clean
- Node status files found: none
- Existing development records: `WORKFLOW.md`, `history-treatment-map.md`,
  `tag-remap.md`

## Audit Inputs

1. `AUDIT-7E-1` — shadow implementation, production wiring, identity/replay,
   determinism, purity, and closure gaps.
2. `AUDIT-HISTORY-DAG` — Git ancestry, stage tags, actual implementation graph,
   sidecars, and stale historical claims.
3. `AUDIT-CONTRACTS` — frozen Core contracts, ML/Core dependency direction,
   high-risk shared contracts, and production integration.
4. `AUDIT-GATES` — manifests, CI, feature combinations, deterministic tests,
   external/provider-dependent checks, and documentation drift.
5. `AUDIT-PARALLEL-TRACKS` — Account/NewAPI, I2 Migration, I3 Agent Takeover,
   I4 restore, UI, and observability.

All five agents were read-only. No agent modified the repository.

## Consolidated Findings

### 7E-1

Status: **PARTIAL**

Validated capabilities:

- Shadow evaluation is feature-gated and disabled by default.
- Policy-routed buffered and streaming requests can record shadow decisions.
- Existing tests cover panic and non-finite isolation, checksums, no duplicate
  provider calls, no runtime mutation, no fallback/retry delta, and bounded
  overhead.
- The production shadow verdict is discarded and cannot alter routing.

Blocking closure gaps:

- A normal production plan puts the production selection first. The shadow
  engine uses that same first candidate as `current_candidate`, so
  `ActionGuard` commonly returns `Keep` before a meaningful comparison.
- `ShadowInput` and exact per-candidate feature vectors are discarded after
  evaluation.
- Initial planned selection and final served candidate after fallback are not
  represented as separate authoritative identities.
- `ModelEnsemblePredictor::from_commit` accepts a valid checkpoint paired with
  an unrelated commit ID.
- Shadow training retains a commit ID but discards the parent commit,
  checkpoint, ordered events, and historical model state.
- The shadow input builder iterates the executable plan, so policy-rejected
  candidates present only in `RouteDecision` are not observed.
- `ShadowConfig.max_decisions` and `max_age_secs` are not wired to
  `ShadowStore`; hot configuration replacement does not update the engine.

### Shared contracts

No current contract exists for:

- `ModelInput`
- `DecisionModel`
- `DecisionState`
- `DecisionCandidate`
- `DecisionDistribution`

The current `RoutingModel` predicts from a fixed feature vector and is not a
candidate-aware K-way decision model. Existing shadow, coordinator, and reward
types are foundations, not the 7E-2A contract.

The observed dependency direction remains Core-authoritative:

```text
Core policy/router/observation/stats/session/account/protocol
                         ↑
                         └── ML features/models/reward/identity/shadow
                                      ↑
                               server composition
```

P1 contract contradictions found outside the shadow-only boundary:

- Request-derived capabilities are not enforced on the main policy path, and
  direct model IDs bypass capability checks.
- `RouteDecision.selected` is the initial planned candidate, not necessarily
  the candidate that ultimately served after runtime failover.
- `FailureImpact` and `Error` are competing runtime failure authorities.
- Client stream disconnect can be finalized as a successful request.
- No production path constructs the Stage 6 `Outcome`/`Feedback` objects.
- Stage 6 and Stage 7B expose incompatible `TrainingSample` definitions.
- `LearningEvent` has no durable ordered journal, idempotency, or durable
  sequence semantics.
- ML/account features are not enabled by the default Tauri dependency.

### History and roadmap reality

- Stage tags through `stage-7e0-v1` are ancestors of current HEAD, but tags do
  not prove current acceptance gates.
- The 7E-1 commits `ec4236e`, `c207069`, `a52fd3f`, and `763d87b` are in
  current HEAD ancestry.
- There is no repository evidence for 7F, 7G, 7H, or Stage 8 implementation.
- The historical statement that Stage 6 is pipeline-wired is contradicted by
  current call sites.
- Historical claims that NewAPI, I2, and I3 are "real" are limited to local
  library/temp-file behavior and are not real product lifecycle evidence.

### Parallel tracks

- Account types, store, and pure calculations have useful unit tests.
- NewAPI currently treats non-empty credentials as authentication, synthesizes
  an active runtime, and leaves usage/quota unimplemented.
- Migration has a valid state machine and local file mechanics, but does not
  actually stop/start the intended processes and accepts any HTTP response as
  endpoint verification.
- Takeover has substantial ownership and temp-adapter tests, but release does
  not apply the computed conflict resolution and no durable ownership manifest
  exists.
- The current operational UI builds, but the layout fixture is stale and the
  layout script can return success when no browser ran.
- Account, migration, takeover, shadow, and ML have no Tauri/UI product surface.

## Executed Gate Evidence

### Local commands run by the orchestrator

| Command | Result | Evidence summary |
|---|---|---|
| `cargo check --workspace` | PASS | Exit 0; one unused-variable warning in `src-tauri/src/store.rs` |
| `cargo test --workspace` | PASS | Exit 0 |
| `cargo test --workspace --features ml` | PASS | ML, shadow, replay, and determinism suites passed |
| `cargo test --workspace --all-features` | PASS | Core Account/ML/NewAPI feature suites passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | FAIL | `src-tauri/src/store.rs:106`, unused `existed` |
| `pnpm --dir ui build` | PASS | TypeScript and Vite production build passed |
| `pnpm smoke` | FAIL | Windows binary is `zroutery-headless.exe`, but the script searches the extensionless Unix path |
| `python scripts/ui_layout_test.py --self-test` | PASS | 7/7 self-tests passed |
| `python scripts/ui_layout_test.py` | FAIL | No Chromium found, but the script exits successfully after printing `skipping` |
| `cargo fmt --all -- --check` | FAIL | Substantial pre-existing formatting drift |
| `git diff --check` | PASS | No whitespace errors |

### External CI

GitHub Actions run `36014406507` succeeded for the exact audited SHA:

- Linux core all-feature clippy: success
- Linux core all-feature tests: success
- macOS workspace check: success

This does not supersede the failed local full-workspace clippy because CI does
not run the Tauri crate through all-target/all-feature clippy and does not run
Windows smoke, UI layout, formatting, or packaging gates.

## Node Status Matrix

### Frozen Core and ML

| Node | Status | Current evidence / unblock condition |
|---|---|---|
| Stage 1 | REVALIDATE | Naming/tier tests pass; capability enforcement defects require a P1 repair and revalidation |
| Stage 2 | FAILED | Unknown media and capability derivation can silently lose or bypass requirements |
| Stage 3 | FAILED | Main policy/direct-ID capability enforcement and final-served identity are incorrect |
| Stage 4 | FAILED | Failure authority conflicts and stream disconnect can be recorded as success |
| Stage 5 | PARTIAL | Account unit contracts exist; persistence and production ownership/execution do not |
| Stage 6 | FAILED | No production Outcome/Feedback bridge; incompatible training schemas |
| 7A | PARTIAL | Deterministic feature extraction exists; identity/schema architecture memory is incomplete |
| 7B | BLOCKED | Dataset library exists but depends on a trustworthy production Outcome |
| 7C | PARTIAL | Specialist models work as baselines but do not implement a typed candidate decision interface |
| 7D | PARTIAL | Evaluation framework exists; required holdout/calibration/statistical gates are incomplete |
| 7E-0 | FAILED | Commit/checkpoint binding, lineage, replay validation, and content-addressed identity are incomplete |
| 7E-1 | PARTIAL | Purity/determinism foundations pass; production counterfactual and replay closure fail |
| 7E-2A | BLOCKED | Requires 7E-1A and 7E-1B |
| 7E-2B | BLOCKED | Requires 7E-2A and the Stage 6 Outcome repair |
| 7E-2C | BLOCKED | Requires supervised warmup, DecisionDistribution, Dataset, and RewardPolicy |
| 7E-2D | BLOCKED | Requires a trustworthy K-way distribution |
| 7E-2E | BLOCKED | Requires durable ordered LearningEvent and verified ModelCommit |
| 7E-2F | BLOCKED | Requires verified immutable snapshots and atomic activation |
| 7E-3 | BLOCKED | Requires calibrated offline replay/evaluation gates |
| 7F | BLOCKED | Requires accepted ModelCommit, shipping reachability, and observability |
| 7G | BLOCKED | Requires stable real-traffic shadow evidence and rollback |
| 7H | BLOCKED | Requires stable takeover, budget, monitoring, and rollback |
| Stage 8 | BLOCKED | Requires the complete learning loop |

### Parallel engineering

| Node / track | Status | Current evidence / next safe work |
|---|---|---|
| Account generic contract | PARTIAL | Add persistence/runtime contract without changing ML feature order |
| NewAPI adapter | PARTIAL | Build a local mock-server auth/expiry/refresh/usage/quota contract |
| I2 Migration | PARTIAL | Introduce a typed action runner and local child-process fixtures |
| I3 Takeover | PARTIAL | Add durable manifest, conflict application, and CAS restore fixtures |
| I4 Restore/Rollback | FAILED | Conflict resolution is disconnected from release; migration rollback can report false success |
| Operational UI | DONE | Existing provider/model/routing/activity UI builds and is integrated |
| UI layout harness | FAILED | Stale fixture and false-green missing-browser behavior |
| New-track UI/Tauri surfaces | BLOCKED | Backend lifecycles must become real before exposure |
| Runtime observability projection | READY | Read-only projection can be built without changing ML schemas |
| Commit contract alignment | READY | CI and WORKFLOW must adopt the current Node commit/trailer contract |
| Orchestrator status artifacts | READY | Status/DAG/evidence/regression records can be created from this audit |

## Critical Path

```text
7E-1A  Model identity / lineage / replay repair
  ↓
7E-1B  Replayable shadow observation + meaningful counterfactual
  ↓
7E-2A  DecisionModel / DecisionState / Candidate / Distribution
  ↓
7E-2B  Supervised warmup
  ↓
7E-2C  RLCD-style bandit learning
  ↓
7E-2D  Calibration
  ↓
7E-2E  LearningEvent / verified ModelCommit
  ↓
7E-2F  Atomic activation
  ↓
7E-3  Offline replay/evaluation
  ↓
7F    Real traffic shadow
  ↓
7G    Controlled takeover
  ↓
7H    Exploration
  ↓
8     Continual policy learning
```

Required P1 side repairs before online learning/evaluation:

```text
Stage 3 eligibility/decision-trace repair ─┐
Stage 4 failure/stream-state repair ───────┼─> 7E-2B / 7E-2E / 7E-3
Stage 6 Outcome/Feedback bridge repair ────┘
```

## Blocking Register

| Node | BlockedBy | Why | Required artifact | Unblock condition |
|---|---|---|---|---|
| 7E-1B | 7E-1A | Shadow predictor still depends on unverified commit/checkpoint lineage | Verified commit, checkpoint, ordered replay | 7E-1A gates pass |
| 7E-2A | 7E-1B | Replayable input and meaningful production counterfactual are incomplete | Serialized decision-time input, commit, final served identity | 7E-1B gates pass |
| 7E-2B | 7E-2A, Stage 6 | No typed decision interface or production Outcome | DecisionModel contract and Outcome bridge | Both nodes pass |
| 7E-2C | 7E-2B | No supervised warmup or trusted Dataset/RewardPolicy | Warmup model, Dataset, RewardPolicy | 7E-2B passes |
| 7E-2D | 7E-2C | No trustworthy K-way probability | Calibrated distribution seam | 7E-2C passes |
| 7F | 7E-3 and packaging | ML is not in the default desktop path | Accepted commit, replay evidence, observability | 7E-3 and shipping gates pass |
| NewAPI E2E | External protocol/credential artifacts | No protocol fixture, token, or real lifecycle | Documented protocol and test credential | Mock gate passes, then real E2E |
| I2 E2E | Process/config/auth contract | Current executor actions are placeholders | Owned runner, config format, auth policy | Local child-process gate passes |
| I3 E2E | Ownership/confirmation contract | No durable manifest or conflict-safe restore | ADR, manifest schema, confirmation flow | Persistence/conflict gate passes |

## Regression Ledger

| ID | Regression | Evidence | Severity | Planned owner |
|---|---|---|---|---|
| REG-001 | Full workspace clippy failure | `4991d21`, `src-tauri/src/store.rs:106` | P2 | BASELINE-GATE |
| REG-002 | Windows smoke cannot find headless binary | `156e3193`, `scripts/smoke_test.py:357-361` | P2 | BASELINE-GATE |
| REG-003 | Layout test succeeds without running a browser | `scripts/ui_layout_test.py` | P1 | BASELINE-GATE |
| REG-004 | CI commit contract conflicts with current Node contract | `.github/workflows/commit-lint.yml:25-33` | P1 | COMMIT-CONTRACT |
| REG-005 | Stage 3 capability enforcement bypass | `policy.rs` / `router.rs` consumers | P1 | Future Core repair node |
| REG-006 | Failure/circuit authority conflict | `failure.rs` vs `error.rs` | P1 | Future Core repair node |
| REG-007 | Stream disconnect can be recorded as success | `server/pipeline.rs:1508-1516` | P1 | Future Stage 4 repair node |
| REG-008 | Model checkpoint can be paired with unrelated commit | `ml/shadow.rs:361-372` | P1 | 7E-1A |
| REG-009 | Decision-time features are not retained | `ml/shadow.rs:220-232,598-821` | P1 | 7E-1B |
| REG-010 | Node state/evidence is not recoverable from docs | Missing status/evidence registry | P1 | ORCH-DOCS |

## First Implementation Dispatch Batch

The first batch has four non-overlapping worktrees.

### Package 7E-1A — Model Identity / Replay Substrate Repair

```text
NODE_ID: 7E-1A
PARENT_PHASE: 7E-1 Shadow Decision Closure
OBJECTIVE: Repair the 7E-0/7E-1 shared model identity, checkpoint binding,
commit lineage, and deterministic replay substrate. Do not implement 7E-2.
CURRENT_REPOSITORY: main @ c66072f341664f67c1cd1c761e5f5adf1980ce16,
clean worktree
DEPENDENCIES: Existing Stage 7A-7C model/dataset contracts; existing 7E-0
ModelCommit/LearningEvent/ReplayEngine; Core remains authoritative
INPUT_ARTIFACTS:
- crates/zroutery-core/src/ml/model_identity.rs
- crates/zroutery-core/src/ml/shadow.rs
- crates/zroutery-core/tests/model_identity_test.rs
- crates/zroutery-core/tests/shadow_test.rs
ALLOWED_FILES:
- crates/zroutery-core/src/ml/model_identity.rs
- crates/zroutery-core/src/ml/shadow.rs, limited to predictor
  identity/checkpoint/lineage retention
- crates/zroutery-core/tests/model_identity_test.rs
- crates/zroutery-core/tests/shadow_test.rs
OPTIONAL_FILES:
- crates/zroutery-core/src/ml/mod.rs
- crates/zroutery-core/src/lib.rs only if a public export is required
FORBIDDEN:
- DecisionModel or DecisionDistribution implementation
- warmup, RL, calibration, activation
- server/pipeline, router, policy, session, or account changes
- Tauri ML feature enablement or provider execution
- unrelated refactors
- durable LearningEvent journal, which belongs to 7E-2E
REQUIRED_GATES:
- complete the first audit report before editing
- bind checkpoint and commit ID; reject mismatches
- distinguish model/schema/parent/lineage in canonical commit identity
- remove unrelated sequential commit identity from ModelStore
- validate model, parent, result, and logical order during replay
- prove Replay(A+B) == Replay(Replay(A), B)
- prove identical ordered events produce identical commit/checksum
- reject corrupt checkpoint, event, and lineage
- make schema/version behavior explicit
- retain verified commit, parent, and checkpoint lineage through train/swap
- preserve zero routing/session/account/provider side effects
- git diff --check
- cargo test -p zroutery-core --all-features
- cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings
- cargo test --workspace --features ml
EXPECTED_EVIDENCE:
- wrong checkpoint/commit pairing test
- lineage tamper and ordering tests
- deterministic replay/checksum tests
- corrupt artifact fail-closed tests
- consumer/schema audit
- exact command exits
COMMIT_EXPECTATION: fix(ml): verify model commit lineage
Use Why/What/Verification/Architecture and trailers:
Node: 7E-1A
Gate: model-identity-replay
Status: <final legal state>
REPORT_FORMAT:
NODE / STATUS / BASE / FINAL / IMPLEMENTATION_STATE / ACTUAL_GAP /
FILES / CHANGES / TESTS / INVARIANTS / REGRESSIONS / BLOCKED_BY /
UNBLOCK_CONDITION / DEFERRED / DOCUMENTATION / COMMIT / NEXT
```

### Package BASELINE-GATE — Restore Executable Local Gates

```text
NODE_ID: BASELINE-GATE
PARENT_PHASE: Repository-wide gate repair
OBJECTIVE: Fix the reproduced workspace clippy failure, Windows smoke binary
resolution, and layout-test false-green without business refactoring.
CURRENT_REPOSITORY: main @ c66072f341664f67c1cd1c761e5f5adf1980ce16,
clean worktree
DEPENDENCIES: none
INPUT_ARTIFACTS:
- src-tauri/src/store.rs:103-115
- scripts/smoke_test.py:357-361
- scripts/ui_layout_test.py
- package.json smoke/layout commands
ALLOWED_FILES:
- src-tauri/src/store.rs
- scripts/smoke_test.py
- scripts/ui_layout_test.py
- package.json only if the command entry point must change
FORBIDDEN:
- ML/Core routing or business behavior changes
- whole-repository formatting
- dependency updates
- ignoring failures
- treating missing Chromium as success
- real provider calls or real user configuration
REQUIRED_GATES:
- complete the first audit report before editing
- cargo check has no unused-variable warning
- full workspace all-target/all-feature clippy passes
- pnpm smoke finds and runs the Windows headless binary
- smoke continues to use only its local mock provider
- layout self-test passes
- missing browser causes an explicit non-success, never false-green
- if a browser exists, layout assertions execute
- git diff --check
EXPECTED_EVIDENCE:
- before/after command output
- binary-resolution evidence
- smoke lifecycle result
- missing-browser behavior
- full clippy exit code
COMMIT_EXPECTATION: up to three coherent commits, for example:
fix(tauri): remove platform-specific unused warning
fix(workflow): resolve headless binary on Windows
fix(workflow): reject skipped layout validation
REPORT_FORMAT:
NODE / STATUS / BASE / FINAL / IMPLEMENTATION_STATE / ACTUAL_GAP /
FILES / CHANGES / TESTS / INVARIANTS / REGRESSIONS / BLOCKED_BY /
UNBLOCK_CONDITION / DEFERRED / DOCUMENTATION / COMMIT / NEXT
```

### Package COMMIT-CONTRACT — Align CI With Node Evidence

```text
NODE_ID: COMMIT-CONTRACT
PARENT_PHASE: Orchestrator infrastructure
OBJECTIVE: Align commit lint and workflow documentation with the current Node
commit/trailer contract while continuing to reject vague commits.
CURRENT_REPOSITORY: main @ c66072f341664f67c1cd1c761e5f5adf1980ce16,
clean worktree
DEPENDENCIES: Current user-provided Commit Contract; existing commit-lint and
WORKFLOW documents
INPUT_ARTIFACTS:
- .github/workflows/commit-lint.yml
- docs/development/WORKFLOW.md
- current commit subjects
ALLOWED_FILES:
- .github/workflows/commit-lint.yml
- docs/development/WORKFLOW.md
- scripts/commit_contract_test.py only if deterministic fixtures require it
FORBIDDEN:
- source-code changes
- Git history rewrite or tag movement/deletion
- misc/update/change/stuff/work/tmp/final/done as primary types
- treating a commit as proof that a Node is DONE
REQUIRED GATES:
- complete the first audit report before editing
- type and scope rules match the current contract
- Node/Gate/Status trailers are accepted
- imperative/length/subject rules are deterministic
- vague subjects fail
- valid and invalid fixtures are tested
- workflow YAML is valid
- git diff --check
EXPECTED EVIDENCE:
- valid/invalid subject fixtures
- allowed/forbidden scope matrix
- trailer accept/reject tests
- workflow lint result
COMMIT_EXPECTATION: ci(workflow): align commit lint with node evidence
REPORT_FORMAT:
NODE / STATUS / BASE / FINAL / IMPLEMENTATION_STATE / ACTUAL_GAP /
FILES / CHANGES / TESTS / INVARIANTS / REGRESSIONS / BLOCKED_BY /
UNBLOCK_CONDITION / DEFERRED / DOCUMENTATION / COMMIT / NEXT
```

### Package ORCH-DOCS — Recoverable Node State Architecture Memory

```text
NODE_ID: ORCH-DOCS
PARENT_PHASE: Orchestrator infrastructure
OBJECTIVE: Convert the audit into canonical DAG, node-status, evidence, and
regression records without changing code or claiming unverified nodes DONE.
CURRENT_REPOSITORY: main @ c66072f341664f67c1cd1c761e5f5adf1980ce16,
clean worktree
DEPENDENCIES: The five audit reports and orchestrator gate evidence in this record
INPUT_ARTIFACTS:
- docs/development/WORKFLOW.md
- Git refs/tags/history
- this audit's matrix, critical path, and regression ledger
ALLOWED_FILES:
- docs/development/architecture.md
- docs/development/roadmap.md
- docs/development/dependency-dag.md
- docs/development/evidence-registry.md
- docs/development/regression-ledger.md
- docs/development/decisions/**
- docs/development/node-status/*.status.json
FORBIDDEN:
- source, test, Cargo, or CI changes
- modifying WORKFLOW.md or commit-lint
- treating historical tags as current DONE
- illegal percentage/almost-done states
- fabricated tests/E2E
REQUIRED GATES:
- complete the first audit report before editing
- record Stage 1-8, 7A-7F subnodes, Account, I2, I3, UI, and Gate nodes
- use only QUEUED/READY/RUNNING/VALIDATING/DONE/PARTIAL/BLOCKED/FAILED/REVALIDATE
- record dependencies, dependents, required gates, blocked_by, and
  unblock_condition for every node
- preserve 7E-1 PARTIAL, 7E-0 FAILED, Stage 2/3/4/6 FAILED, and 7E-2A BLOCKED
- distinguish external CI from local execution
- record REG-001 through REG-010
- validate all JSON and referenced paths
- git diff --check
EXPECTED EVIDENCE:
- JSON parse validation
- complete node inventory
- DAG edge validation
- status enum validation
- evidence command list
- unresolved ADR list
COMMIT_EXPECTATION: docs(workflow): record audited node status and evidence
REPORT_FORMAT:
NODE / STATUS / BASE / FINAL / IMPLEMENTATION_STATE / ACTUAL_GAP /
FILES / CHANGES / TESTS / INVARIANTS / REGRESSIONS / BLOCKED_BY /
UNBLOCK_CONDITION / DEFERRED / DOCUMENTATION / COMMIT / NEXT
```

## Dispatch Order and Merge Rules

1. `7E-1A`, `BASELINE-GATE`, `COMMIT-CONTRACT`, and `ORCH-DOCS` may run in
   separate worktrees because their allowed file sets do not overlap.
2. Each worker must first report:
   `IMPLEMENTATION_STATE`, `ACTUAL_GAP`, `FILES_TO_CHANGE`,
   `DEPENDENCY_RISKS`, and `PLAN` before editing.
3. A worker may claim DONE only when every required gate passes.
4. Inspect each diff, commit, and report before merge.
5. After merging this batch, rerun the full workspace gates even if individual
   nodes passed narrower gates.
6. Do not dispatch 7E-1B until 7E-1A is merged and revalidated.
7. Do not dispatch 7E-2A until both 7E-1A and 7E-1B pass.

## Audit Decision

The first implementation batch is authorized as four bounded nodes:

1. `7E-1A`
2. `BASELINE-GATE`
3. `COMMIT-CONTRACT`
4. `ORCH-DOCS`

No production takeover, online RL, exploration, real-provider E2E, or automatic
model activation is authorized.
