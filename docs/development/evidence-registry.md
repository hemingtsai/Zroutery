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
| E-089 | review, local | Parent diagnostic against the accepted 7E-2E journal, run at `b234e15` and then deleted | FAIL, reproduced, and the node was reverted to `REVALIDATE`. `serde_json` is used without `float_roundtrip`, so its float parsing is not correctly rounded: 5 of 11 arbitrary finite `f64` values came back one ULP different, while round-trip-safe decimals such as 0.1, 1.3 and one third did not. Because `same_content` compares the record re-parsed from disk with the caller's in-memory record, including exact equality over 32 `f32` features and every `f64` target, a byte-identical retry returned `Err(IdempotencyConflict{ .. "already records this event id with different content" })` after a first write of `Appended { sequence: 1 }`. This is precisely the retry scenario idempotency exists for, and the original suite missed it because its fixtures use safe decimals | The journal's own integrity is unaffected: its frame checksum is computed over the serialized bytes on disk, so tampering is still detected. The defect is confined to the duplicate check, which is a false refusal rather than silent corruption. The minimal fix is to retain the raw body bytes on the frame and compare the incoming serialization against them; enabling `float_roundtrip` in the workspace manifest would also fix it but is a global change outside any node's ownership |
| E-090 | local, integration | `cargo check --workspace`; `cargo test --workspace`; `cargo test -p zroutery-core --all-features`; `cargo test --workspace --features ml`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `pnpm smoke`; `pnpm test:layout`; `python -B scripts/orch_docs_test.py`; commit-contract range validation; the 31-test activation suite at `b234e15` | PASS; all four feature combinations, both UI gates, the whitespace check, all 62 non-merge commits, and 31 activation tests | The unreproduced 7E-2C bandit failure in E-085 did not recur in this matrix, and `TEST-FORMAT` remains a separate known debt gate |
| E-093 | review, local, integration | Parent repair of the accepted 7E-2E journal, then `cargo check --workspace`; `cargo test -p zroutery-core --all-features`; `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings`; `cargo test --workspace`; `cargo test --workspace --features ml`; `git diff --check`; `python -B scripts/orch_docs_test.py`; plus a deliberate pre-fix run | PASS; 35 suites and 1596 tests all-features, 39 suites and 966 tests on default features, 39 suites and 1594 tests with `ml`, clippy clean under `-D warnings`. E-089 is closed. Each frame retains its body as it appears on disk and the duplicate check compares the bytes the record WOULD be written as against them, with the two volatile wall-clock fields substituted from the stored record so a later-instant retry stays a duplicate | The regression test was verified to be real rather than merely green: with only the source fix stashed and the test kept, it failed with `IdempotencyConflict` on sequence 1, the recorded E-089 signature. Two costs are accepted knowingly rather than discovered later: a scan holds each raw body alongside the parsed one, and the comparison now depends on a stable serializer byte form, which the pinned `serde_json` gives inside this repository but a log written by another serializer version could disagree with. Making the comparison exact over the whole body changed no legitimate caller, because both record constructors hardcode the current schema version and the scan already refuses a foreign one |
| E-094 | local, integration | `cargo test -p zroutery-core --all-features` observed once during the E-093 matrix, then `cargo test -p zroutery-core --all-features --test shadow_test shadow_overhead -- --nocapture`, then a second full `cargo test -p zroutery-core --all-features`, and a process listing taken at the time | OPEN, and NOT attributed to any code change. `shadow_overhead_p95_under_1ms_p99_under_3ms` failed once on its p99 wall-clock budget. Run alone it measures p95 325.9µs, p99 385.6µs, max 717.8µs against a 3ms budget, which is 7.8x headroom, and the immediate re-run of the whole suite passed 35 suites and 1596 tests | The mechanism is load sensitivity, not a known code defect: a `cargo` build for the concurrent 7E-3 worker was running on a 20-core machine at the time, and the assertion is a percentile of wall-clock durations rather than a property of the code. That said this is the second unreproduced intermittent in the project after E-085, and both are timing-sensitive, so the honest reading is that these gates are load-fragile rather than that the code is sound by luck. A percentile of wall-clock time has no business in a suite that runs beside other builds; it needs either a quiet-machine precondition, a much looser budget with the observed numbers recorded, or removal in favour of a deterministic proxy. Left open rather than closed on one green run |

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

E-094 is open and is the second unreproduced intermittent in this project after
E-085. Both are timing assertions rather than properties of the code, which
makes the gate design, not a single unlucky run, the thing worth fixing.
