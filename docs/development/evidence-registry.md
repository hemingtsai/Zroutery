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
