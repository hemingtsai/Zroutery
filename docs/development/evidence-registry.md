# Evidence Registry

## Provenance and exact revisions

This registry separates evidence inherited from the five-agent audit from
checks performed while creating the orchestration records. Evidence is
labelled `local`, `external-ci`, `documentation`, or `unrun`; labels are not
interchangeable.

| Field | Value |
|---|---|
| Audit record | `docs/development/repository-audit-2026-09-25.md` |
| Audit execution HEAD | `c66072f341664f67c1cd1c761e5f5adf1980ce16` |
| Orchestration starting HEAD | `c8bed1baf7d1f80138ecbdd3f25812835b0ea29f` |
| Orchestration branch | `node/orch-docs` |
| Upstream recorded by audit | `origin/main` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` |
| Historical tag policy | Tags are provenance and ancestry evidence only; they never establish a current `DONE` state. |

The audit's command results below are not presented as fresh results from the
`c8bed1b` documentation commit. The documentation checks use the starting
`c8bed1b` worktree and are identified separately.

## Evidence inventory

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-001 | documentation | `git status --porcelain=v1` at `c8bed1b` | PASS; clean worktree before documentation edits | A clean start does not validate implementation gates. |
| E-002 | documentation | `git rev-parse HEAD` and `git log --oneline --decorate -12` at the starting worktree | PASS; exact starting HEAD is `c8bed1baf7d1f80138ecbdd3f25812835b0ea29f` | The audit execution SHA is the parent `c66072f...`; both are recorded. |
| E-003 | documentation | `git show-ref --tags`; `git merge-base --is-ancestor stage-7e0-v1 HEAD` | PASS; tag is reachable from HEAD | Reachability is not current acceptance and does not repair a failed node. |
| E-004 | local, inherited | `cargo check --workspace` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS; exit 0, one unused-variable warning in `src-tauri/src/store.rs` | The warning is tracked by `REG-001`; this was not rerun by ORCH-DOCS. |
| E-005 | local, inherited | `cargo test --workspace` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS; exit 0 | Passing tests do not resolve the audit's production wiring defects. |
| E-006 | local, inherited | `cargo test --workspace --features ml` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS; ML, shadow, replay, and determinism suites passed | Feature-gated library evidence is not evidence of default desktop ML. |
| E-007 | local, inherited | `cargo test --workspace --all-features` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS; Core, Account, and NewAPI feature suites passed | All-feature tests do not establish a real provider lifecycle or packaging gate. |
| E-008 | local, inherited | `cargo clippy --workspace --all-targets --all-features -- -D warnings` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | FAIL; `src-tauri/src/store.rs:106` unused `existed` | External Linux core CI is narrower and cannot replace this full workspace result. |
| E-009 | local, inherited | `pnpm --dir ui build` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS; TypeScript and Vite production build passed | Build success does not prove browser layout assertions ran. |
| E-010 | local, inherited | `pnpm smoke` on Windows at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | FAIL; the script searches an extensionless Unix binary path while Windows produced `zroutery-headless.exe` | No mock-provider lifecycle result can be claimed from this run. |
| E-011 | local, inherited | `python scripts/ui_layout_test.py --self-test` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS; 7/7 pure self-tests | This does not run a browser or measure the real UI. |
| E-012 | local, inherited | `python scripts/ui_layout_test.py` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | FAIL as an acceptance gate; no Chromium was found and the script printed `skipping` while returning success | The false-green behavior is `REG-003`; this is not a browser PASS. |
| E-013 | local, inherited | `cargo fmt --all -- --check` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | FAIL; pre-existing formatting drift | ORCH-DOCS does not reformat source or claim this gate. |
| E-014 | local, inherited | `git diff --check` at `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS | This checks whitespace only; it is not a test or implementation gate. |
| E-015 | external-ci | GitHub Actions run `36014406507` for exact SHA `c66072f341664f67c1cd1c761e5f5adf1980ce16` | PASS for Linux core all-feature clippy/tests and macOS workspace check | It does not run full-workspace Tauri all-target/all-feature clippy, Windows smoke, UI layout, formatting, packaging, or real-provider E2E. |
| E-016 | documentation | `Test-Path` inventory for audit-referenced source, manifest, script, and CI paths before editing | PASS; referenced paths inspected before writing records | Path existence is not semantic proof; source findings remain those in the audit. |
| E-017 | documentation | `Get-ChildItem docs/development/node-status/*.status.json | ForEach-Object { Get-Content -Raw $_ | ConvertFrom-Json | Out-Null }; 'JSON parse: PASS'` | PASS after record generation | JSON parsing does not validate graph semantics. |
| E-018 | documentation | `python C:\Users\InfinityNeko\AppData\Local\Temp\opencode\validate_orch_docs.py` (inventory, schema, and status checks) | PASS; 63 expected node IDs present and every status is legal | Inventory completeness is checked against the complete table in `dependency-dag.md`. |
| E-019 | documentation | `python C:\Users\InfinityNeko\AppData\Local\Temp\opencode\validate_orch_docs.py` (dependency symmetry and cycle checks) | PASS; no unknown nodes, asymmetric edges, self edges, or cycles | The DAG records intended sequencing, not implementation completion. |
| E-020 | documentation | `python C:\Users\InfinityNeko\AppData\Local\Temp\opencode\validate_orch_docs.py` (source-reference and Markdown-link path checks) | PASS; every referenced path exists | The validator checks paths, not line-level semantic correctness. |
| E-021 | documentation | `git add docs/development/architecture.md docs/development/roadmap.md docs/development/dependency-dag.md docs/development/evidence-registry.md docs/development/regression-ledger.md docs/development/decisions docs/development/node-status; git diff --cached --check; git diff --cached --name-only` | PASS; no whitespace errors and only allowed documentation paths are staged | A documentation diff cannot repair the implementation regressions it records. |
| E-022 | documentation | `Get-ChildItem docs/development/decisions/*.md` plus the ADR checks in `validate_orch_docs.py` | PASS; ADR-0001 and ADR-0002 are present and listed as unresolved | An ADR is unresolved by design; implementation must not treat it as accepted policy. |
| E-023 | documentation | Final `git status --short`, `git show --format=fuller --stat HEAD`, and commit trailer inspection after the documentation batch | PASS; only allowed files are in the commit and the requested trailers are present | The final commit hash is reported by the worker, not inferred from a historical tag. |

## Command inventory by scope

### Local commands recorded by the audit

The complete inherited local inventory is:

```text
cargo check --workspace
cargo test --workspace
cargo test --workspace --features ml
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
pnpm --dir ui build
pnpm smoke
python scripts/ui_layout_test.py --self-test
python scripts/ui_layout_test.py
cargo fmt --all -- --check
git diff --check
```

These results belong to the audit execution SHA, not automatically to the
later orchestration documentation commit.

### External CI

External CI is recorded only as run `36014406507` at the exact audited SHA.
Its successful jobs are narrower than the local gate set. A CI success is not
permission to mark Windows smoke, layout, formatting, packaging, or external
E2E as passed.

### Unrun or blocked gates

No evidence is recorded for a real provider E2E, NewAPI lifecycle, packaging,
full browser layout assertion, or automatic ML activation. Those gates remain
`QUEUED` or `BLOCKED` in the node records and must not be inferred from tags,
unit tests, or local mocks.

## Evidence interpretation rules

- `PASS` means the stated command exited successfully under the stated scope.
- `FAIL` is a real reproduced failure, even if another environment passed.
- A command that prints a skip and exits zero is recorded as a failed
  acceptance gate when the required browser/process did not run.
- `external-ci` never overrides a local result without an explicit ADR and a
  new, exact-revision evidence record.
- Source paths in node records are anchors for audit recovery, not claims that
  a path has been changed by ORCH-DOCS.

## Post-first-batch evidence

The following evidence was executed by the parent orchestrator after merging
`7E-1A`, `BASELINE-GATE`, `COMMIT-CONTRACT`, and `ORCH-DOCS` at main revision
`c782a4946b314a8db6d7d92bddf11f77d2b4ee16`.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-024 | local, integration | `cargo check --workspace` at `c782a49` | PASS; workspace compiled | Default feature set; not ML reachability |
| E-025 | local, integration | `cargo test --workspace` at `c782a49` | PASS; default workspace tests passed | Does not replace ML feature gates |
| E-026 | local, integration | `cargo test --workspace --features ml` at `c782a49` | PASS; ML/shadow/identity tests passed | Tauri still does not enable ML by default |
| E-027 | local, integration | `cargo test --workspace --all-features` at `c782a49` | PASS; Account/NewAPI/ML feature tests passed | No real provider lifecycle |
| E-028 | local, integration | `cargo clippy --workspace --all-targets --all-features -- -D warnings` at `c782a49` | PASS; no warnings/errors | Rustfmt remains a separate known debt gate |
| E-029 | local, integration | `pnpm --dir ui build` at `c782a49` | PASS; TypeScript and Vite build passed | Build is not browser E2E |
| E-030 | local, integration | `pnpm smoke` at `c782a49` | PASS; native Windows binary and local mock lifecycle passed | Expected Windows connection-reset diagnostics occur for aborted oversized request |
| E-031 | local, integration | `pnpm test:layout` at `c782a49` | PASS; UI build, 7 self-tests, real browser layout assertions passed | Harness is not Tauri IPC E2E |
| E-032 | local, integration | `python -B scripts/commit_contract_test.py --base c66072f..HEAD` at `c782a49` | PASS; all 13 non-merge commits valid | Commit validator does not validate code behavior |
| E-033 | local, performance | `cargo test -p zroutery-core --all-features --test shadow_test shadow_overhead_p95_under_1ms_p99_under_3ms -- --nocapture` at `c782a49` | PASS; 8-candidate P95 `109.4µs`, P99 `131.7µs`, max `161.4µs` | Hardware-dependent benchmark, not a production SLO |
| E-034 | documentation, integration | Parent JSON/DAG/path/status validation after the first-batch state update | PASS; 64 node records and 95 dependency edges are valid | Re-run after every future status-only change |

The old `E-004` through `E-015` records remain historical evidence for the
pre-merge audit revision. `E-024` through `E-034` are the current post-merge
baseline and do not retroactively convert blocked Core/ML nodes to `DONE`.

## 7E-1B-CORE acceptance evidence

The parent orchestrator independently reviewed and re-ran the pure shadow
subtask after the follow-up semantic gate. The accepted main commits are
`b9a5d60` and `de4e55f`; the final tree is main revision
`de4e55f02609a4cdcc2f6845ad105f8f2bd68ec7`.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-035 | review, integration | `git diff --name-status 18b00dc..de4e55f`; `git diff --check 18b00dc..de4e55f` | PASS; only the five authorized ML source/test files plus the follow-up shadow source/test files changed; no Core/server/router/policy/session/account/provider files | Pure ML seam only; production integration is deferred |
| E-036 | local, integration | `cargo check --workspace` at `de4e55f` | PASS | Default feature set; ML is also gated separately |
| E-037 | local, integration | `cargo test -p zroutery-core --all-features` at `de4e55f` | PASS; 937 Core unit tests and all Core integration suites passed, including 6 observation tests and 5 semantic store-gate tests | No real provider lifecycle |
| E-038 | local, integration | `cargo test --workspace --features ml` at `de4e55f` | PASS; workspace ML/shadow/identity and existing runtime-mutation/provider-call gates passed | Production ML activation remains disabled |
| E-039 | local, integration | `cargo test --workspace --all-features` at `de4e55f` | PASS; all workspace feature combinations passed | No real provider lifecycle |
| E-040 | local, integration | `cargo clippy --workspace --all-targets --all-features -- -D warnings` at `de4e55f` | PASS; no warnings or errors | Rustfmt remains a separate known debt gate |
| E-041 | local, performance | `cargo test -p zroutery-core --all-features --test shadow_test shadow_overhead_p95_under_1ms_p99_under_3ms -- --nocapture` at `de4e55f` | PASS; 8-candidate P95 `417.2µs`, P99 `667µs`, max `1.6085ms` | Hardware-dependent benchmark, not a production SLO |
| E-042 | documentation, integration | Parent JSON/DAG/path/status validation, `git diff --check`, and `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` after the 7E-1B-CORE state update | PASS; 64 nodes, 95 edges, and all 18 non-merge commits valid | Re-run after every future status-only change |

These records accept only `7E-1B-CORE`. They do not close full `7E-1B`,
`REG-009`, or any Core P1 repair: final served identity capture, production
pipeline/session/outcome integration, and trustworthy runtime evidence remain
separate gates.

## Core P1 split audit and dispatch contract

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-043 | audit, architecture | Read-only Agent audit of current main `5a7b295` in isolated worktree `audit/core-p1-split` | PASS; confirmed Stage 2/3/4/6 and REG-005/006/007 gaps, five non-overlapping bounded nodes, and sole ownership of `server/pipeline.rs` | Audit only; no implementation files or status nodes were changed by the audit worktree |
| E-044 | documentation, architecture | ADR-0003, ADR-0004, ADR-0005 plus five new Core P1 node records and DAG/roadmap/regression updates | PASS; capability/media fail-closed, canonical failure/outcome authority, and dispatch order are now explicit | ADRs bind the current Core repair batch only; they do not authorize ML activation or full 7E-1B |

| E-045 | documentation, integration | Parent JSON/DAG/path/status validation, `git diff --check`, and `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` after the Core P1 split commit `87291f4` | PASS; 69 nodes, 107 edges, and all 20 non-merge commits valid | Re-run after every future status/DAG/ADR change |
| E-046 | review, integration | Parent diff/API review of Agent commit `ac3842e` | FAIL; the public `ClassifiedFailure::from_error(String)` API was replaced by a structural-only generic signature; commit rejected pending compatibility repair | Failure classification logic itself was not rejected |
| E-047 | local, integration | Follow-up `534fb45` cherry-picked to main as `4048a61`/`97efde6`; `cargo check --workspace`; `cargo test -p zroutery-core --all-features`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `git diff --check` | PASS; 7 authority tests including API compatibility, Core all-feature, workspace ML/all-feature, and clippy gates passed | Runtime adapters in router/pipeline remain deferred to later nodes |
| E-048 | documentation, integration | Parent JSON/DAG/path/status validation, `git diff --check`, and `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` after the failure-authority acceptance state update | PASS; 69 nodes, 107 edges, and all 25 non-merge commits valid | Re-run after every future status/DAG/ADR change |
| E-049 | review, integration | Parent review of Agent follow-up `9e979e1`; 8 focused failure-authority tests, `cargo test -p zroutery-core --all-features`, Core clippy, workspace check, and allowed-file/diff checks | PASS; generic upstream 402/412 are `ProviderRejected`, while explicit local markers and structural `Error` variants retain local semantics | Runtime adapters remain deferred to later Core P1 nodes |
| E-050 | local, integration | Parent review of Agent commit `d1a3416`; allowed-file/diff audit, focused media/protocol tests, Core all-feature tests, Core clippy, and workspace check | PASS; capability derivation, fail-closed protocol/media gates, explicit policy outcomes, and conservative virtual registry behavior validated | Router/policy eligibility and route evidence remain deferred to CORE-P1-ELIGIBILITY-TRACE |
| E-051 | local, integration | Main integration at `74de4d0` after `e048480` and `74de4d0`; `cargo check --workspace`, Core all-feature tests, workspace ML/all-feature tests, workspace all-target/all-feature clippy, `pnpm smoke`, `pnpm test:layout`, `git diff --check`, and commit-contract range validation | PASS; Batch A code and existing runtime/UI gates remained green | Batch B and the sole pipeline integration are not yet accepted |
| E-052 | documentation, integration | Parent JSON/DAG/path/status validation, `git diff --check`, and `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` after Batch A state commit `b95b109` | PASS; 69 nodes, 107 edges, all 29 non-merge commits valid, and main working tree clean | Re-run after every future status/DAG/ADR change |

## Core P1 Batch B acceptance evidence

Batch B was executed on `dev` after `dev` was fast-forwarded to the accepted
main state. The reviewed commits are `2633092`, `e5f3874`, `9bf2911`
(eligibility/trace) and `b99baa4` (Outcome/Feedback); the integration revision
is `b99baa4`.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-053 | review, integration | `git diff --name-status a5c55b1..9bf2911` and `9bf2911..b99baa4`; read of the eligibility, policy, Outcome, and dataset boundaries | PASS; eligibility touched only `policy.rs`, `router.rs`, and its focused tests; Outcome/Feedback touched only `outcome.rs`, `feedback.rs`, the ML dataset adapter, `lib.rs`, and its focused tests; no `server/pipeline.rs`, shadow, identity, decision-engine, reward, `src-tauri`, or `ui/` file | Boundary and invariant review; the sole pipeline lifecycle, `STAGE-2`/`STAGE-3`/`STAGE-4`/`STAGE-6` revalidation, and `CORE-P1-REPAIR` remain open |
| E-054 | local, integration | `cargo test -p zroutery-core --all-features --test eligibility_trace_test` and `--test outcome_feedback_test` at `9bf2911` and `b99baa4` | PASS; 15 eligibility/trace tests and 15 Outcome/Feedback tests, including request/direct/tier parity, planned identity, `IgnoreRequirements` limits, terminal classification, identity correlation, malformed-record rejection, and determinism | Focused tests do not prove production emission; the pipeline owner must construct exactly one Outcome |
| E-055 | local, integration | `cargo test -p zroutery-core --all-features` at `b99baa4` | PASS; all Core unit and integration suites passed, including the accepted 7E-1B-CORE shadow, identity, and media/failure suites | No real provider lifecycle and no packaging gate |
| E-056 | local, integration | `cargo check --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `git diff --check` at `b99baa4` on `dev` | PASS; all Rust gates, the Windows native-binary and local mock-provider lifecycle, the 7 layout self-tests with real browser assertions, and the whitespace check passed | A green regression suite is not evidence that the pipeline records a client drop correctly; `TEST-FORMAT` remains a separate known debt gate |
| E-057 | documentation, integration | `python -B scripts/orch_docs_test.py --self-test` and `python -B scripts/orch_docs_test.py` at the Batch B acceptance state update, plus `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` | PASS; 17 self-test fixtures reject as designed, 69 nodes, 107 edges, 52 evidence rows, 10 regressions, 5 ADRs, and 13 development links are valid | The documentation gate validates records and paths, not code behavior |

Batch B is accepted: `CORE-P1-ELIGIBILITY-TRACE` and
`CORE-P1-OUTCOME-FEEDBACK` are `DONE` on `dev`. `CORE-P1-PIPELINE-LIFECYCLE` is
the only remaining Core P1 work at that point, and `CORE-P1-REPAIR` stays
`QUEUED` until that integration and the audited stage revalidation pass.
Development continues on `dev`, not `main`; a historical tag, a green gate, or a
commit hash is still not a `DONE` claim for a node whose required gates have not
run.

## Core P1 Batch C acceptance evidence

Batch C is the serial production lifecycle integration, the sole owner of
`server/pipeline.rs`. The reviewed commit is `cbe2240` on
`node/core-p1-pipeline-lifecycle`, integrated into `dev` as `c4188be`.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-058 | review, integration | `git diff --name-status d6f6e83..cbe2240`; read of the lifecycle, stream loop, drop path, and Outcome construction | PASS; only `server/pipeline.rs`, `server/mod.rs`, `stats.rs`, and a new focused `tests/pipeline_lifecycle_test.rs` changed; no ML, outcome, feedback, failure, error, policy, router, protocol, Tauri, UI, or documentation file | Mock upstream and real HTTP client only; the Outcome has no production ML consumer yet |
| E-059 | local, integration | `cargo test -p zroutery-core --all-features --test pipeline_lifecycle_test` at `c4188be` | PASS; 12 tests, including a real mid-answer client drop that is `Interrupted` and never served, a drop before output that is `Cancelled`, a drop that does not count against provider health, explicit Responses-API cancellation, exactly one terminal transition across served/failed/dropped paths with record-to-outcome id correlation, planned/last-attempted/served correlation, a budget denial with no candidate, a rate limit classified once without opening the circuit, and structural tripwires for the absent second classifier and the single record/charge sites | No real provider lifecycle; the drop tests use a held-open mock stream rather than a real upstream abort |
| E-060 | local, integration | `cargo check --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `git diff --check`; `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` at `c4188be` on `dev` | PASS; every Rust gate, the Windows native-binary and local mock-provider smoke lifecycle, the 7 layout self-tests with real browser assertions, the whitespace check, and all 38 non-merge commits valid; worktree clean | A client-closed request now shows activity status 499 and counts as a failure, which is a deliberate user-visible change; an interrupted stream still reports the usage it consumed so the charge and the Outcome agree |
| E-061 | documentation, integration | `python -B scripts/orch_docs_test.py` after the Batch C acceptance state update | PASS; 69 nodes, 107 edges, and every evidence reference, ADR, regression, and development link valid | The documentation gate validates records and paths, not code behavior |

Batch C is accepted: `CORE-P1-PIPELINE-LIFECYCLE` is `DONE`, so all five bounded
Core P1 nodes are accepted and `REG-006`/`REG-007` are closed. `CORE-P1-REPAIR`
remains `QUEUED` until the aggregate gates and the audited stage revalidation
are recorded; `7E-1B` remains `BLOCKED` until then.

## Core P1 revalidation evidence

Revalidation ran after the integration node was accepted, as ADR-0005 requires.
It re-ran the per-stage gate suites by name at the integration revision
`c4188be` on `dev`, so each stage decision cites a specific suite rather than a
summary of a full-workspace pass.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-062 | local, revalidation | `cargo test -p zroutery-core --all-features --no-fail-fast` over the 15 stage gate targets `tier_contract_test`, `media_capability_test`, `protocol_media_gate_test`, `protocol_golden_test`, `vision_test`, `eligibility_trace_test`, `routing_policy_test`, `failure_authority_test`, `circuit_breaker_test`, `outcome_test`, `outcome_feedback_test`, `pipeline_lifecycle_test`, `shadow_observation_test`, `shadow_test` at `c4188be` | PASS; 28, 3, 9, 48, 6, 15, 86, 8, 11, 15, 10, 12, 6, and 19 tests respectively, 0 failed, which covers every listed gate of the audited stages 1, 2, 3, 4, and 6 | One earlier sweep of the same targets reported a single failure in the load-dependent `shadow_overhead_p95_under_1ms_p99_under_3ms` benchmark. It passed inside the full gate matrix and in five subsequent runs, so it is recorded as an intermittent performance assertion, not a functional regression. The stage gates cited above are deterministic. |
| E-063 | documentation, integration | `python -B scripts/orch_docs_test.py --self-test` and `python -B scripts/orch_docs_test.py`, plus `git diff --check` and `python -B scripts/commit_contract_test.py --base c66072f341664f67c1cd1c761e5f5adf1980ce16 --head HEAD` after the revalidation state update | PASS; 17 self-test fixtures reject as designed, 69 nodes, 107 edges, 63 evidence rows, 10 regressions, 5 ADRs, and 13 development links are valid, and the range has no invalid commit | A record that says DONE is still only as strong as this evidence; a later change to any cited suite must produce a new record |

The Core P1 aggregate and the five audited stage records are `DONE` on `dev`.
Their blockers were removed, not their edges: `7E-1B` and `7B` became dispatchable,
`7E-1` is blocked only by `7E-0`, and every later ML node keeps its own
predecessor as the blocker. No DecisionModel, DecisionDistribution, warmup, RL,
calibration, activation, or durable journal work is authorized, and the final
served identity is not consumed by the shadow seam until `7E-1B` proves it.

## 7E-1B production integration dispatch

`7E-1B` is dispatched on `node/7e-1b-integration` from revision `1e62e33`. It
owns the production lifecycle seam again for the duration of the dispatch, so
`7B` is held back rather than run in parallel: both would need the same
terminal transition point, and that file has one owner at a time.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-064 | review, architecture | Read of the accepted seam before dispatch: `ProductionDecisionRef.served` is documented as `None` until an integration node supplies it, `ShadowEngine::evaluate` is already panic-isolated and returns `None` when disabled, and `shadow_decision_checksum` covers the decision-time input and verdict but not `actual.served` | PASS; the served identity can be correlated after evaluation without invalidating the decision identity, so the integration does not need to re-derive or recompute anything | Reading the seam proves the contract is satisfiable, not that the wiring is correct; only the integration tests and gates can do that |

The worker may claim `DONE` for this node only when the served identity is
consumed from the single validated Outcome, the exact decision-time input is
retained rather than reconstructed, a dropped or cancelled stream is correlated
as a non-success observation, shadow faults never affect the request, and
determinism plus the accepted 7E-1B-CORE suites still pass. The worker reported
`PARTIAL` for exactly that reason, which was correct.

## 7E-1B acceptance and a corrected gate failure

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-065 | review, local | `cargo test --workspace` at the Batch C acceptance revision; `Select-String` on the compiler output; the same failure reproduced with the integration diff stashed at `1e62e33` | FAIL then PASS; `pipeline_lifecycle_test.rs` called the ml-gated `Router::observations()` without a feature gate, so the default-feature test target did not compile from `c4188be` until `ccdbe15` feature-gated that one assertion | This was a parent acceptance failure, not a worker failure: the Batch C gate matrix (E-060) omitted `cargo test --workspace`, so a broken default-feature gate was accepted and recorded as green. The default-feature gate is now in the matrix for every Core and ML acceptance. |
| E-066 | review, integration | `git diff --name-status 1e62e33..48018af` and a read of the seam, lifecycle, and correlation changes | PASS; only `ml/shadow.rs`, `server/pipeline.rs`, and a new focused `tests/shadow_integration_test.rs` changed; the seam change is purely additive (`ShadowStore::correlate_served`, `ShadowEngine::correlate_served`, a private `count_fault` helper) with no existing signature, checksum, or behavior changed | The seam gained one primitive because a stored record had no way to receive a terminal fact; 7B will build on that surface |
| E-067 | local, review | Worker mutation checks on its own wiring, repeated by the parent reading the same assertions | PASS; deleting the correlation call fails 5 tests, and substituting `last_attempted_identity` for `served_identity` fails exactly the failed, abandoned-stream, and cancelled-stream tests | Mutation evidence shows the gates bite; it is not a substitute for the behavioral tests themselves |
| E-068 | local, integration | `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `git diff --check`; `python -B scripts/orch_docs_test.py`; commit-contract range validation at `0f5c935` on `dev` | PASS; all four test feature combinations, 937 Core unit tests, the 14 new integration tests, both UI gates, the whitespace check, and all 43 non-merge commits valid; worktree clean | Tauri does not enable the `ml` feature, so the shipped desktop app does not exercise the shadow path; that is a packaging decision, not a code gap |

`7E-1B` is `DONE`, which makes `7E-1` DONE as well because all three of its
children are accepted. The served identity is now consumed from the single
validated Outcome, and the record retains the exact decision-time input, so
REG-009 is closed on the evaluation side only. `7E-2A` is `READY` and `7B` is
`READY`; neither may be claimed as started work, and no training, activation,
takeover, exploration, or durable journal work is authorized by this acceptance.

## 7E-2A and 7B parallel dispatch

Both nodes are dispatched together from revision `2580850`. Parallelism is
allowed only because their file ownership is disjoint, which was checked rather
than assumed: `7E-2A` owns `ml/decision_engine.rs`, `ml/model.rs`, a new contract
module, and its `ml/mod.rs` registration; `7B` owns `ml/dataset.rs` and the
production ingestion hook in `server/pipeline.rs`, where it is the sole owner for
the duration. Neither may edit the other's files.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-069 | review, architecture | Read of the current module surfaces before dispatch: `ml/mod.rs` registration list, the existing `EngineInput`/`EngineCandidate`/`EngineOutput` surface in `decision_engine.rs`, the `ModelState`/`Prediction` types in `model.rs`, the `DatasetStore` and `OutcomeTrainingSample` in `dataset.rs`, and the correlation key shared by the Outcome and the shadow record | PASS; the two nodes touch disjoint files, and both can be correlated on the request id that `RequestLifecycle` already uses | Deciding the file split is not the same as proving the two designs compose; that is what the parent review and the gate matrix are for |

`7E-2A` may define `DecisionDistribution` as a type only. Calibrated K-way
distributions belong to `7E-2D`, and no warmup, RL, activation, or journal work
is authorized by this dispatch. `7B` may ingest, validate, and retain canonical
samples; it may not train on them, and it may not make dataset collection
silently depend on an undeclared configuration state.

## 7E-2A and 7B acceptance evidence

Both nodes were reviewed against their own required gates and then integrated
together, so the matrix below covers the combined tree.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-070 | review, integration | `git diff --name-status` for both dispatches; read of the contract types, the `DecisionPhase` successor table, `DecisionState::advance`, `DecisionDistribution::try_new`, the ingestion entry point, `store_all`, and the pipeline hook | PASS; 7E-2A added one module and 7 additive `ml/mod.rs` lines and left `decision_engine.rs` and `model.rs` byte-for-byte unchanged; 7B touched only `ml/dataset.rs`, `server/mod.rs`, `server/pipeline.rs`, and two new test files, and satisfied the accepted 7E-1B tripwires instead of loosening them | Reviewing the two designs separately does not prove they compose; the combined matrix and the cross-node tests do |
| E-071 | local, review | Search of the new contract for a training surface and for unguarded deserialization | PASS; no `&mut self`, `update`, `reset`, `train`, or `fit` on `DecisionModel`, and no contract type derives `Deserialize` | The privacy that keeps training unreachable is a property of the current shape, not a language guarantee; a later node must add a seam deliberately |
| E-072 | local, integration | `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `git diff --check`; `python -B scripts/orch_docs_test.py`; commit-contract range validation at `5e3f49b` on `dev` | PASS; all four test feature combinations, the 26 contract tests, the 21 ingestion-boundary and 10 production ingestion tests, both UI gates, the whitespace check, and all 47 non-merge commits valid; worktree clean | Tauri does not enable `ml`, so neither the shadow path nor dataset collection runs in the shipped desktop app; `config.shadow.enabled` is the de facto dataset switch |

`7E-2A` and `7B` are both `DONE`. Two consequences are recorded rather than
smoothed over: a sample's features currently come from a snapshot cloned at
decision time on the path where the shadow record was accepted, so the store
needs a read accessor to make that correlation authoritative; and dataset
collection follows the shadow configuration because no dataset-specific
configuration exists. `7E-2B` is the next critical-path node, and it is where the
first consumer of this dataset and of the typed contract may be built — training,
warmup, and calibration are still unauthorized until that node is dispatched.

## 7E-2B supervised warmup dispatch

`7E-2B` is dispatched on `node/7e-2b-warmup` from revision `f5fca90`. It is the
first node that consumes the collected dataset and the first that would train
anything, so the dispatch named the seam explicitly rather than letting the
worker discover it.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-073 | review, architecture | Read of the training and evaluation surface before dispatch: `ModelEnsemblePredictor::try_train` and `try_train_with_history` take `&self` and return the ordered lineage instead of storing it, the infallible `train` wrapper `.expect()`s, `FrozenHoldout` and `Evaluator` are built on the legacy `TrainingSample`, and `OutcomeTrainingSample::into_legacy` copies `targets` verbatim while dropping the evidence fields | PASS; training is a pure function whose installation is a separate `ShadowEngine::swap` or `try_train_and_swap` call, so an offline warmup can produce a verified commit with no reachable activation path | The accepted training surface is the legacy shape while production collects the canonical one, so warmup must project between them and prove the projection cannot turn a failure into a positive label. Reading the seam proves the split is possible, not that warmup is correct |

`7E-2B` may train. It may not activate, may not be reachable from the running
product, may not run on a schedule, and may not fabricate a Feedback rating.
Bandit and RL work is `7E-2C`, calibration is `7E-2D`, the durable journal is
`7E-2E`, and installing a verified commit is `7E-2F`; none of them is authorized
by this dispatch.

## 7E-2B acceptance evidence

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-074 | review, integration | `git diff --name-status f5fca90..85dd8b8`; read of `warmup.rs` for the activation tripwire, the verdict logic, and the refusal set; read of the attempt-scope branch of `validate_outcome_sample` in `dataset.rs` | PASS with one repair; the diff is `ml/warmup.rs`, its test, and additive `ml/mod.rs` registration, and `warmup.rs` cannot name `ShadowEngine`, `swap`, or `server::` at all. Two of the three premises in the dispatch brief were wrong — `try_train_with_history` is private rather than lineage-returning, and `try_train_and_swap` does not exist while `ShadowEngine::swap` plus `ShadowEngine::try_train` is the real activation surface — and the worker verified both instead of trusting them. The worker's commit had no subject line, so it violated the commit contract; the parent rewrote the subject on integration and confirmed the three source files were already bit-identical. | The contract violation reached `dev` through the cherry-pick before it was caught, so a malformed subject does pass a cherry-pick unnoticed; only the range validator sees it |
| E-075 | review, local | Read of `validate_outcome_sample`'s request-scope and attempt-scope branches in the accepted `ml/dataset.rs` | FAIL in the accepted artifact, closed locally; the request scope rejects success timing on a non-success sample, but the attempt scope checks only index, ids, identity, and success agreement, so an attempt sample can carry `latency_ms` or `ttft_ms` with `success` false and would train the latency head on a failed attempt | The gap is in `7B`'s accepted file and is still open there. `7E-2B` closed it in the node that owns the training consequence with `WarmupError::NonSuccessTiming`; the validator itself is unchanged |
| E-076 | local, integration | `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `git diff --check`; `python -B scripts/orch_docs_test.py`; commit-contract range validation at `a332b79` on `dev`; the 29-test warmup suite | PASS; all four test feature combinations, the 29 warmup tests, both UI gates, the whitespace check, and all 51 non-merge commits valid; worktree clean | `run_warmup` has exactly one caller, the test file, so nothing in the product can trigger a warmup. Tauri does not enable `ml`, so the shipped desktop app has neither the dataset nor the warmup entry point |

`7E-2B` is `DONE`. A supervised model can now be trained offline from real
collected samples, with a verified commit, honest lineage, and a verdict that
withholds its own improvement claim when the holdout is degenerate. Nothing
installs that commit: `7E-2C` is next on the critical path, and it owns bandit
and reward learning rather than more supervised warmup.

## E-075 repair: attempt-scope sample timing

`E-075` recorded a real gap in an accepted validator rather than closing it in
the node that happened to find it. It is repaired here, in the file that owns
the rule.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-077 | local, repair | `validate_outcome_sample` attempt-scope branch in `ml/dataset.rs`; `Targets::from_attempt`; the focused `dataset_ingestion_test` plus the four-way matrix | PASS; the attempt scope now refuses success timing on a non-success attempt, exactly as the request scope already did, and `Targets::from_attempt` never emitted such a value in the first place, so nothing the producer legitimately produced is now invalid. Two tests cover both directions: a failed attempt carrying `latency_ms` or `ttft_ms` is refused by name, a failed attempt carrying `cost` is still accepted and stored, and a successful attempt keeps its own measured timing | `7E-2B`'s `WarmupError::NonSuccessTiming` is deliberately left in place as defense in depth for samples built in memory and never passed through the store; two gates on one invariant is intentional, not duplication to clean up |

The fix belongs in the validator rather than in the training node because the
validator is the single authority every stored sample passes, and the defect was
reachable by any hand-built or deserialized sample, not only by the warmup path
that stumbled on it.

## 7E-2C bandit and reward learning dispatch

`7E-2C` is dispatched on `node/7e-2c-bandit` from revision `4854b65`. The
pre-dispatch read is what makes this node's boundary precise: it is not a repair,
because the capability does not exist yet.

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-078 | review, architecture | Read of `ml/reward.rs` and a search across `ml/` before dispatch: `RewardPolicy` is a hand-set weight vector, `ActionGuard::decide` is a hand-written rule, there is no `fit`, `train`, `update`, or `learn` in the reward module, and a search for UCB, Thompson, epsilon-greedy, arm counts, and confidence accumulators finds none | PASS; no bandit, no arm statistics, and no learned reward exist anywhere, so this node builds the capability rather than repairing it. `Action::Explore` exists in the accepted coordinator, which is why the production-inaccessibility gate is explicit rather than assumed | Reading the absence establishes scope, not correctness. The binding risk here is not the fitting but the safety claim, because every production sample carries `feedback: None` and the reward is therefore an outcome proxy that can diverge from user preference |

`7E-2C` may fit a reward offline and evaluate a selection rule offline. It may
not explore in production, install a policy, calibrate a distribution, write a
durable journal, or fabricate a user preference signal. The safety evaluation
must be able to refuse on tail grounds, because a policy that improves mean
reward while regressing failures, cost, or a tail latency percentile is not an
improvement.

## 7E-2C acceptance evidence

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-079 | review, integration | `git diff --name-status 4854b65..5a53e68`; read of `bandit.rs` for the partition construction, `accepted_arm`, `OutcomeProxy`, and the safety metric sources; read of the `Action::Explore` tripwire in the test file | PASS; the diff is a new `ml/bandit.rs`, its test, and additive `ml/mod.rs` registration, with no read-only file touched. Five claims were checked rather than believed: the fit and holdout partition is built from a `BTreeMap` in sorted key order, so it no longer depends on a per-outcome UUID; `accepted_arm` derives from the verdict enum rather than a stored boolean, so a forged verdict field changes nothing; `OutcomeProxy` carries exactly four fields with no slot a rating could occupy; the safety metrics are computed from raw measurements so a reward weighting cannot grade itself; and the `Action::Explore` tripwire strips comment lines before matching | The worker also found and fixed two silent defects in its own work, which E-080 records rather than this row |
| E-080 | local, review | Worker self-review of its own diff, reported to the parent unprompted | Two defects found and fixed; the fit and holdout partition was built in first-seen order, so because `sample_id` derives from a per-outcome UUID the partition was non-deterministic across process runs, and the degenerate-holdout downgrade was applied unconditionally, so a fit could never report `Better` | The second defect is the same trap 7E-2B closed one node earlier, reached independently. Two workers hitting the same statistical trap suggests it deserves a shared regression test rather than a per-node lesson |

`7E-2C` is `DONE`. A reward can now be fitted from collected samples and a
selection rule evaluated offline, and the run can refuse to endorse a policy
whose mean reward improved while failures, cost, tail latency, or fallbacks
regressed. Nothing installs it, nothing explores in production, and no calibrated
distribution is produced. `7E-2D` is next on the critical path and owns
calibration, `7E-2E` owns the durable journal, and `7E-2F` owns activation, so
nothing here can reach a live router.

## 7E-2D calibration and distribution evidence

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-081 | review, architecture | Comparison of the declared boundaries before dispatch: `7D` and `7E-2D` both list `ml/evaluation.rs` in `source_references`, and their required gates overlap on calibration metrics and a frozen holdout | PASS with a decision recorded; `7E-2D` holds that file exclusively for its dispatch and is required to build the calibration and holdout surface as a reusable public API, including the `passes` rule on both measurement types so `7D` can later judge two routes under one rule. `7D` must not be dispatched against that file concurrently | The decision lives here and in the node notes rather than in a new ADR, because it resolves a collision between two existing records rather than changing a boundary policy. If `7D`'s remaining scope grows, this deserves an ADR |
| E-082 | review, integration | `git diff --name-status c71e561..c2240b4`; read of the cohort ordering key, the `EmittedDecision` field privacy, the `checked_add` split refusal, the UUID-invariance test, and the confidently-wrong assertions | PASS after two parent corrections; the diff is a new `ml/calibration.rs`, additive `ml/evaluation.rs`, `ml/mod.rs` registration, and two test files, with `lib.rs`, `server/`, `ui/`, `docs/**` and every other read-only `ml/*.rs` untouched. The parent verified that the ordering key is built only from cohort content, that `EmittedDecision.distribution` is private with no bare-`f64` constructor and no `Deserialize`, that the split overflow refuses instead of panicking, and that both a constant-0.98 head and a fully inverted head are judged `Miscalibrated` while a merely-miscalibrated honest head is rejected and the fitted joint over the same snapshot is accepted | Two premises in the worker's plan were false and were caught before implementation rather than after: `sample_id` is UUID-derived, and both `decision_id` and `outcome_id` are too, with second-resolution timestamps, so the planned ordering was UUID-dependent |
| E-083 | local, review | Worker self-review reported unprompted, plus its own metric probe | Four defects found and fixed; a binned gap measure averaged a real error away, with three calibrated masses landing in one bin and cancelling to an ECE of 5.55e-17 while the worst named candidate was 12.2 points out, which is why the bin-free per-candidate table and a third ceiling exist; the Platt map used `ln p` instead of log-odds; the initial and final log losses were both read from the gradient-accumulation loop and were near-duplicates; and the split addition overflowed and panicked instead of refusing | The binned-cancellation case is the reason this row exists. It is a metric that reported success while a named candidate was badly wrong, and it would have passed a mean-only gate |

The recorded holdout is 40 decisions and 120 candidate observations, disjoint
from the 160-decision fit, with a hand-computable truth of `(0.20, 0.40, 0.20)`.
The emitted vector reached ECE 0.0667, MCE 0.1032, worst named candidate 0.1032,
Brier 0.1918 and multiclass log loss 1.0397 for a `Calibrated` verdict, against
ECE 0.1243 and MCE 0.2865 for the same parameterization left unfitted. The
independent route's pre-normalization marginals were near-perfectly calibrated
and normalization destroyed that by `+0.0667` ECE, which is the measured reason
the joint parameterization is the claim rather than the independent one.

`7E-2D` is `DONE`. A calibrated K-way distribution now exists, it is measured on
the vector actually emitted, and nothing consumes it: the emitted vector is an
unconsumed offline artifact, the running product cannot construct a distribution
at all, and `7E-2E` (durable journal) and `7E-2F` (activation) remain the only
routes from here to a live router.

Batch A was accepted earlier on `main`: `CORE-P1-MEDIA-REQ` and
`CORE-P1-FAILURE-AUTHORITY` are `DONE`. The records above supersede the state
that existed when they were still `READY` for a Batch B dispatch.

## 7E-2E durable journal evidence

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-084 | review, integration | `git diff --name-status 5eac5bc..HEAD`; read of `FrameReader`, the `Anchor` fields, `record_canonical` and its projection comparison, `frame_checksum` visibility, and the refusal surface; `Cargo.toml` confirmed unmodified | PASS; the diff is a new `ml/journal.rs`, its test, and additive `ml/mod.rs` registration, with `model_identity.rs` and every other read-only file untouched and no new dependency. Verified directly: the frame reader keeps its own explicit carry buffer, the anchor carries record count, next sequence, log length, whole-log checksum, tail frame checksum and its own checksum, the canonical path compares `projected == event.samples` after validating every sample with the accepted validator, and `frame_checksum` is public so the format is independently verifiable | The worker's first frame reader used `BufReader`, and `Read::read` with a buffer at least the internal capacity bypasses that buffer, so every record after the first was unreadable to itself. It found and fixed this itself |
| E-085 | review, local | One unexplained failure during the 7E-2E integration matrix, then eleven attempts to reproduce or explain it | UNREPRODUCED and recorded rather than dismissed. `cargo test --workspace --all-features` at `83659ee` failed `a_different_seed_replays_a_different_schedule` at `tests/bandit_safety_test.rs:618` with 38 passed and 1 failed, while `cargo test -p zroutery-core --all-features` and `--features ml` passed in the same matrix. Follow-up: five isolated runs, one configuration-matched run of that test under the same `--workspace --all-features` flags, and three whole-binary runs all passed. No mechanism was identified: `ml/bandit.rs` has no mutable global state, `seeded_hash` is pure integer arithmetic that the module documents as independent of any hash-map iteration order, and the fixture pins explicit deterministic outcome ids. 7E-2E cannot affect that path, but neither can it be shown to be unrelated | The failing assertion compares two seeds' aggregate observation COUNTS while the test is named for the assignment SCHEDULE, and an aggregate can coincide without the schedules matching. That is a plausible weakness in the test rather than in the module, but it is not a diagnosis. Reopen this row if it recurs. Separately, that test file's comment claiming `OutcomeBuilder` mints a fresh UUID per build is inaccurate for its own fixture, which overrides the id with `req_<timestamp>` |
| E-086 | local, integration | `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `git diff --check` at `83659ee`, then a rerun of `cargo test --workspace --all-features` after E-085, plus `pnpm smoke`, `pnpm test:layout`, `python -B scripts/orch_docs_test.py`, commit-contract range validation, and the 30-test journal suite | PASS on the rerun; the four feature combinations, both UI gates, the whitespace check, all 60 non-merge commits, and 30 journal tests. The first matrix was not clean, which is why E-085 exists and why acceptance waited for a clean rerun instead of being written on the first result | A clean rerun is a second observation, not a proof of determinism. `TEST-FORMAT` remains a separate known debt gate and the desktop app still does not enable `ml` |

`7E-2E` is `DONE`. A durable, ordered, idempotent learning journal now exists with
no auto-repair and no background writer, and `7E-0` is one delegated item closer
to completion. `7E-2F` owns immutable snapshots and atomic activation, so it is
the only remaining route from this work to a live router.

## 7E-2F acceptance, and a defect this audit found in 7E-2E

| ID | Class | Exact command or observation | Result | Known caveat |
|---|---|---|---|---|
| E-087 | review, integration | `git diff --name-status 535881a..217e964`; read of the `FrameReader` carry, the `Anchor` fields, `record_canonical` and its projection comparison, `frame_checksum` visibility, and the refusal surface; `Cargo.toml` confirmed unmodified | PASS; a new `ml/activation.rs`, its test, and additive `ml/mod.rs` registration, with `config.rs`, `lib.rs`, `docs/**`, `server/**`, `src-tauri/**`, `ui/**` and every existing `ml/*.rs` byte-identical to base | The worker's first frame reader used `BufReader`, and `Read::read` with a buffer at least the internal capacity bypasses that buffer, so every record after the first was unreadable to itself. It found and fixed this itself |
| E-088 | review, integration | Read of the unreachability test, plus the manifest: `src-tauri/Cargo.toml` names `zroutery-core = { path = ... }` with no features at all, and the workspace `Cargo.toml` sets serde_json features to `preserve_order` only | PASS; the test enumerates 31 concrete symbols this node introduces and walks `src/server`, `src-tauri/src` and `ui/`, and it deliberately does NOT scan a bare `activation` substring because two accepted modules use that word for a mathematical activation function. The packaging consequence is therefore fact, not rhetoric: none of this code is compiled into the desktop binary | Windows rename is a replace but not a guaranteed atomic one, so the pointer is fsynced before the rename and read back afterwards, refusing `PointerNotDurable` rather than assuming the rename landed |
| E-089 | review, local | Parent diagnostic against the accepted 7E-2E journal, run at `b234e15` and then deleted | FAIL, reproduced, and the node was reverted to `REVALIDATE`. `serde_json` was used without `float_roundtrip`, so its float parsing was not correctly rounded: 5 of 11 arbitrary finite `f64` values came back one ULP different, while round-trip-safe decimals such as 0.1, 1.3 and one third did not. Because `same_content` compared the record re-parsed from disk with the caller's in-memory record, including exact equality over 32 `f32` features and every `f64` target, a byte-identical retry returned `Err(IdempotencyConflict{ .. "already records this event id with different content" })` after a first write of `Appended { sequence: 1 }`. This is precisely the retry scenario idempotency exists for, and the original suite missed it because its fixtures use safe decimals. **The root cause was later removed at `5bd20cf`**, so the value class no longer exists; the byte-comparison fix stands on its own regardless | The journal's own integrity is unaffected: its frame checksum is computed over the serialized bytes on disk, so tampering is still detected. The defect was confined to the duplicate check, which is a false refusal rather than silent corruption. The minimal fix was to retain the raw body bytes on the frame and compare the incoming serialization against them, which is exact and immune to the parse. The alternative — enabling `float_roundtrip` — is what eventually closed the whole class, three nodes later |
| E-090 | local, integration | `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `python -B scripts/orch_docs_test.py`; commit-contract range validation; the 31-test activation suite at `b234e15` | PASS; all four feature combinations, both UI gates, the whitespace check, all 62 non-merge commits, and 31 activation tests | The unreproduced 7E-2C bandit failure in E-085 did not recur in this matrix, and `TEST-FORMAT` remains a separate known debt gate |
| E-092 | review | Boundary decision taken by the parent before dispatching 7E-3, after the second collision of this kind between `7D` and a `7E` node over `ml/evaluation.rs` | RECORDED as a decision, not as a pass. `7D` claims a calibration and statistical gate, `7E-3` claims a holdout, calibration and statistical gate, and both pointed at the same file. Following the E-081 precedent, where the critical-path node took the file exclusively and built it as a reusable public surface, `7E-3` owns `ml/evaluation.rs` for its dispatch and `7D` is sequenced AFTER it | The split is: `7E-3` owns the gate wiring and the verdict, which is the part that has to produce a defensible offline answer; `7D` keeps attempt-level attribution and the statistical release methodology. The worker was required to state which parts it built and which it deferred rather than half-build the remainder, and it deferred all of it: no confidence interval, power analysis, significance test, minimum detectable effect, multiple-comparison control or bootstrap exists in the node, and `EvidenceFloors` is documented as a presence floor rather than a significance test. Colliding nodes are never assumed parallel-capable; this one is recorded so the next reader knows the file has an owner and why |
| E-093 | review, local, integration | Parent repair of the accepted 7E-2E journal, then `cargo check --workspace`; `cargo test -p zroutery-core --all-features`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `cargo test --workspace`; `cargo test --workspace --features ml`; `git diff --check`; `python -B scripts/orch_docs_test.py`; plus a deliberate pre-fix run | PASS; 35 suites and 1596 tests all-features, 39 suites and 966 tests on default features, 39 suites and 1594 tests with `ml`, clippy clean under `-D warnings`. E-089 is closed. Each frame retains its body as it appears on disk and the duplicate check compares the bytes the record WOULD be written as against them, with the two volatile wall-clock fields substituted from the stored record so a later-instant retry stays a duplicate | The regression test was verified to be real rather than merely green: with only the source fix stashed and the test kept, it failed with `IdempotencyConflict` on sequence 1, the recorded E-089 signature. Two costs are accepted knowingly rather than discovered later: a scan holds each raw body alongside the parsed one, and the comparison now depends on a stable serializer byte form, which the pinned `serde_json` gives inside this repository but a log written by another serializer version could disagree with. Making the comparison exact over the whole body changed no legitimate caller, because both record constructors hardcode the current schema version and the scan already refuses a foreign one |
| E-094 | local, integration | `cargo test -p zroutery-core --all-features` observed once during the E-093 matrix, then `cargo test -p zroutery-core --all-features --test shadow_test shadow_overhead -- --nocapture`, then a second full `cargo test -p zroutery-core --all-features`, and a process listing taken at the time | OPEN, and NOT attributed to any code change. `shadow_overhead_p95_under_1ms_p99_under_3ms` failed once on its p99 wall-clock budget. Run alone it measures p95 325.9µs, p99 385.6µs, max 717.8µs against a 3ms budget, which is 7.8x headroom, and the immediate re-run of the whole suite passed 35 suites and 1596 tests | The mechanism is load sensitivity, not a known code defect: a `cargo` build for the concurrent 7E-3 worker was running on a 20-core machine at the time, and the assertion is a percentile of wall-clock durations rather than a property of the code. A percentile of wall-clock time has no business in a suite that runs beside other builds; it needs either a quiet-machine precondition, a much looser budget with the observed numbers recorded, or removal in favour of a deterministic proxy. Left open rather than closed on one green run. It has since recurred once more, during the 7D worker matrix, and again did not reproduce for the parent. CORRECTION to an earlier statement in this row: it previously claimed E-085 was also a timing assertion, which is wrong. E-085 compares two seeds' aggregate observation COUNTS and is not a wall-clock measurement, so it remains unclassified. What the intermittents share is only that they are unreproduced; two of the three are wall-clock budgets and the third is not |
| E-100 | review, local, integration | Parent audit of the TEST-PACKAGING diff, then on the integrated state at `271cff1`: `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `git diff --check`; `python -B scripts/orch_docs_test.py`; `python -B scripts/commit_contract_test.py`; the 7E-2F boundary test; and FOUR PLANTED VIOLATIONS plus a silent install and a byte-level comparison | PASS; 43 suites and 967 tests on default features, 39 suites and 1728 tests all-features, 43 suites and 1689 tests with `ml`, clippy clean, contract valid, and the boundary test 31 of 31 unmodified. The artifact gate was NOT taken on its self-test: the parent proved it non-vacuous by planting violations and requiring each to be caught | The negative control is the point of this row. An ASCII `ActivationStore` planted at the end of a copy of the shipped executable was caught at byte offset 0x10DB402 with a non-zero exit; a UTF-16LE `SnapshotId` was caught at 0x10DB404; an empty file and a nonexistent path both failed closed rather than passing. A gate that cannot fail protects nothing, and this one demonstrably can. The symbol list is also cross-checked rather than merely followed: the parent planted a probe symbol into the accepted Rust boundary test and the gate reported the drift and failed, which is stronger than the worker's claim that the list cannot drift. The boundary ruling held and was verified by blob hash at the base and at HEAD for the 7E-2F test and for src-tauri/Cargo.toml, package.json, the root Cargo.toml and tauri.conf.json, with a three-new-files zero-deletions diff and an empty diff over every protected path. The worker's explanation for why the installed executable does not hash-equal target/release was checked rather than believed: the parent installed and compared byte by byte, and found exactly three differing bytes at exactly 0xA004EF, decoding as ASCII `NSS` against `UNK`, which confirms the bundle-type marker account precisely. TWO GATES ARE NOT FULLY MET AND ARE NOT COUNTED AS PASSES: the MSI installer returned 1603 with Error 1925 for want of privileges, so the install and reinstall gate is carried by NSIS alone, and the `app` and `dmg` bundle targets were never built because they are macOS formats. The committed tauri.conf.json was deliberately not narrowed to hide that. The scan is string-level evidence and not a disassembly proof, and no pdb was consulted under `strip = true`. Finally both new scripts are manual: nothing runs them automatically, CI only listens on main and does not build the desktop artifact, so today they protect nobody without a human running them |
| E-099 | review, local, integration | Owner decision on E-097, then `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `git diff --check`; `cargo tree -e features` to confirm the feature resolved; the shadow p99 timing gate before and after | PASS; 43 suites and 967 tests on default features, 39 suites and 1728 tests all-features, 43 suites and 1689 tests with `ml`, clippy clean. E-097 is closed. Three tests that asserted the transport hazard now assert its absence, which makes them the regression tests for the fix rather than for the bug | This was an authorised exception to the ownership boundary, taken explicitly by the owner rather than assumed by a worker: the workspace root manifest had been recorded by 7E-2E, 7E-2F, 7E-3 and 7D as outside every node's ownership, and the parent changed it only after being asked to choose. The cascade was predicted before the edit and then measured, not discovered. Three tests failed exactly as expected, one per layer: the activation test asserted plain JSON does NOT verify, the journal helper searched for a drifting float and found none so it panicked, and the gate's fidelity measurement asserted `moved_fields > 0`. Each now asserts the corrected reality with the failure message naming the cause, so removing the feature again fails loudly and points at itself. The `f32` survival claim recorded in E-096 as a measurement rather than a proof is now moot rather than confirmed: the double-rounding path it worried about is not on this transport any more. Cost measured rather than assumed, because correctly-rounded parsing is not the fast path: the shadow p99 moved from 385.6us to 406.4us against a 3ms budget while its max fell from 717.8us to 602.7us, and the 10k extraction gate is unaffected. Nothing is near a ceiling, but the p99 regression is real and is why the load-fragile gates in E-094 and E-095 matter more now. AN UNEXPLAINED OBSERVATION FROM THIS CHANGE, recorded rather than smoothed over: one `cargo test -p zroutery-core --all-features` run aborted after three suites and roughly 1054 tests, and the parent FAILED TO CAPTURE WHICH TEST FAILED before rerunning. Six subsequent lib runs of 1013 tests and four subsequent full runs of 39 suites and 1728 tests were all clean, so it did not reproduce. The parent is NOT attributing it to the timing gates and is NOT claiming it is a flake: the culprit is unknown because the name was not recorded. The suspicion that this change contributed cannot be excluded, since it made float parsing slower and the abort happened in the lib suite where the 10k extraction budget lives | |
| E-095 | local, integration | `performance_10k_extractions` at `crates/zroutery-core/src/ml/features.rs:595` failing once under full-suite parallel load during the 7E-3 worker matrix, then three isolated runs | OPEN, and NOT attributed to any code change. The assertion is `elapsed.as_millis() < 100` over 10,000 `extract_features` calls, and it passes in isolation at 0.00-0.01s, which is 10x or more of headroom. `features.rs` is not in the 7E-3 diff, and every full run since has been clean | The second wall-clock budget to fail only under load, after E-094, and it is a unit test inside `src/ml/features.rs` rather than an integration test, so it runs in parallel with its own binary's other tests. Together with E-094 this is two of the three unreproduced intermittents in the project being wall-clock assertions. The honest reading is that this suite has load-fragile gates, not that the code is sound by luck; a duration budget with no quiet-machine precondition will keep producing false alarms that cost real audit time. Not closed on green runs |
| E-096 | review, local, integration | Parent audit of the 7E-3 diff, then the full matrix on the integrated state at `5bed10c`: `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `git diff --check`; `python -B scripts/commit_contract_test.py` | PASS; 41 suites and 966 tests on default features, 37 suites and 1646 tests all-features, 41 suites and 1644 tests with `ml`, clippy clean, contract valid. The worker's claims were re-derived rather than accepted, and each held | The load-bearing claim was checked by reading the code, not the report: replay equivalence compares every float through `to_bits`, never `==`; a tolerance sweep over the new module found no epsilon, no rounding and no partial comparison anywhere; `Exactness` keeps the FIRST divergence in a documented order; and a non-finite recorded value is refused BEFORE any comparison, which is what closes the hole where two same-bit NaNs would otherwise score a match. A replayed NaN is still safe because the recorded side is guaranteed finite, so it necessarily diverges. The decisive test does what its name says: it perturbs a recorded prediction by `to_bits() + 1`, the smallest representable disagreement, and asserts both a refusal and that the refusal names the exact field and index. The worker's mid-implementation self-correction was checked against the source and was right: `project_cohorts` does skip `SampleScope::Request` rows when building the candidate axis, so a request-only holdout really would have produced a one-candidate axis. Two residuals: `component_count` is a hand-maintained parallel formula rather than derived from the comparison, so it is correct today at 32 per candidate and 107 in the fixture but nothing would fail if a future field were compared and not counted; and the claim that f32 features always survive the read path is a measurement, not a proof, though a counterexample would make the gate REFUSE rather than falsely pass, so it limits decidability and not safety |
| E-098 | review, local, integration | Parent audit of the 7D diff, then the full matrix on the integrated state at `c0d8da1` and again after the parent correction: `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `git diff --check`; `python -B scripts/commit_contract_test.py`; `python -B scripts/orch_docs_test.py`; plus a hand re-derivation of the reported arithmetic and a byte comparison of `RELEASE_SCOPE` against `d472e3a` | PASS; 43 suites and 967 tests on default features, 39 suites and 1728 tests all-features, 43 suites and 1689 tests with `ml`, clippy clean, contract valid. The worker's central claim, the sampling unit, was re-derived from the source and holds structurally: the ledger increments the decision count once per decision, a `selections_total_is_the_decision_count` check refuses if candidate totals stop reconciling, and more than one served candidate is refused, so a row-level analysis would break a type rather than quietly emit a narrow interval | The reported numbers were checked by hand and are arithmetically correct: effect `(1333-667)/4000 = 0.16650`, `se = sqrt(2000)/4000 = 0.011180`, interval `0.1665 +/- 1.96*0.011180`, reconciliation `2000+2000 = 4000`, and the row-level `sqrt(0.5/12000) = 0.006455` is narrower by exactly root 3. The additive-only boundary held: 187 insertions and 4 deletions, where the 4 are two reflowed import lines, one expanded doc comment and one rewrapped format string, and every prior import survives. The cumulative preservation test is correct in construction, because 7E-3's counts interact and a fresh clone per step would let a later mutation mask a blocker. The parent disagreed with the worker on ONE point and the worker was right: it reported `RELEASE_SCOPE` as byte-identical and the parent first measured a length difference, but that difference was an artifact of comparing equal-length context windows on either side of the const. Two worker claims the parent doubted held up, including that no test was weakened: a count-only blocker assertion was in fact upgraded to check which blocker, and editing 7E-3's fixture was mechanically necessary because it constructs `GateConfig` literally. One residual the worker correctly refused to fix alone: the scope string now understates the combined verdict, which the parent closed with a doc paragraph rather than by rewriting another node's accepted value |
| E-097 | review, local | The float suite at `5bed10c` plus the accepted `ModelCommit` verification path and the 7E-2F boundary test; re-measured after `5bd20cf` | RESOLVED, and it was resolved at the root rather than worked around. Originally OPEN and systemic: a trained checkpoint did NOT survive a plain JSON round trip, 4 of 65 success parameters moved, and the round-tripped commit's own `verify()` returned false. The only bit-exact model persistence lived in `ml/activation.rs`, which the 7E-2F boundary test forbids any other `ml` module from naming, so no node could persist a model in a form that re-verifies — which is what a node serving real traffic needs | The owner authorised the global change three separate nodes had recorded as outside their ownership: serde_json's `float_roundtrip` is now enabled workspace-wide, so parsing is correctly rounded and the shortest-round-trip decimal parses back to the original bits. After it, a plain JSON round trip of a trained commit verifies, so the persistence path exists without touching the boundary. `7F` is no longer blocked by this. What is deliberately NOT claimed: the activation wire form is now redundancy rather than necessity and is kept only because it is lossless by construction and carries the schema envelope; and the transport hazard can return, which is why the float fidelity constituent of the release verdict is still computed on every run and the journal still compares bytes |

`7E-2F` is `DONE`. The activation mechanism exists, is proven offline, and is
inert by construction rather than merely unreferenced: the only reader of the
pointer is this module's own accessor, and no predictor, ensemble, decision
engine, or serving handle is ever constructed. Nothing reaches a live router.

`7E-2E` is `DONE`. The idempotency gate it failed at revalidation is
re-established by byte comparison against the stored frame rather than by
comparing a re-parsed value, which closes E-089 in E-093. The node was
`REVALIDATE` in the interval because reverting a node whose gate is provably
false is the fail-closed behaviour the records demand; the green claim is now
backed by a regression test that was seen to fail without the fix.

E-094 is open. It is one of three unreproduced intermittents in this project,
along with E-085 and E-095, and what they share is only that they are
unreproduced: two of the three are wall-clock timing budgets and the third is
not. That makes the gate design, not a single unlucky run, the thing worth
fixing, and the recurring p99 assertion is now the second time one of them has
fired during a worker's matrix without reproducing for the parent.
