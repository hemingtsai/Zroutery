# Repository guidance

## Project layout

- `crates/zroutery-core/`: protocol codecs, routing, accounts, accounting, storage and optional ML.
- `src-tauri/`: desktop lifecycle, native integrations, credential storage and IPC commands.
- `crates/zroutery-headless/`: headless gateway using the same Core and desktop state.
- `ui/`: React/TypeScript frontend and typed mock bridge.
- `scripts/`: build, contract, layout, interaction and release checks.
- `docs/development/`: implementation contracts, execution evidence and node status.

Read `CONTRIBUTING.md` and `docs/development/WORKFLOW.md` before changing repository
workflow or creating commits. For upgrade work, follow `upgrade-contracts.md` and
the accepted plan it references; `upgrade-master-plan.md` is a historical draft.
Implementation, local verification, commit and release are distinct states.

## Working with other agents

- Check branch, working changes and staged changes before editing or committing.
  Existing changes may belong to the user or another agent.
- Respect the task's assigned directories. The upgrade split assigns `crates/`
  and `src-tauri/` to the backend owner and `ui/` to the frontend owner.
- Coordinate changes to shared manifests, `Cargo.lock`, API types, fixtures and
  evidence records. Re-read shared files immediately before applying a patch.
- Use explicit paths when staging. A backend commit must not capture pre-staged
  frontend work. Do not reset, stash, reformat or overwrite another worker's files.
- Delegate only when the user or task instructions authorize it. Parallel workers
  need non-overlapping write scopes; one owner publishes Git commits at a time.

## Implementation contracts

- Keep existing module boundaries and feature gates. Backend wire fields use
  `snake_case`; follow the existing Tauri argument mapping in `ui/src/api.ts`.
- IPC changes: define the `api.ts` contract, implement Rust, then synchronize
  `ui/mock-bridge.ts`. Coordinate the contract edit when frontend ownership is separate.
- Register commands in both `src-tauri/src/lib.rs` and `src-tauri/build.rs`, and
  grant only the required window capabilities under `src-tauri/capabilities/`.
- Configuration is declaration; runtime observations, credentials and historical
  accounting have separate owners. Never persist upstream secrets or request bodies
  in configuration, history, diagnostics, fixtures or reports.
- Preserve request/attempt identity and unique terminal settlement. Failed attempts'
  known costs remain recorded; unknown usage, price or balance is not measured zero.
- Cache read/write tokens are subsets of input; reasoning tokens are inside output.
  Monetary totals keep currencies and non-equivalent units separate.
- Retain native protocol semantics, capability evidence and explicit reasoning
  controls. A local persistence failure must not replay an executed upstream/tool action.
- Client takeover must preserve unrelated edits and support preview, stale-file
  detection and recovery. Run file-writing tests against isolated temporary roots.
- Use platform-specific dependencies and implementations for native features;
  preserve Windows, macOS and Linux target boundaries.
- UI changes follow the current design system and user-approved theme specs.
  Keep English, Simplified Chinese and Traditional Chinese keys synchronized;
  update relevant layout/interaction fixtures when changing UI structure.

## Verification

Use focused behavior tests for changed contracts, then the required repository gates.
Do not infer Linux/macOS runtime acceptance from a Windows build or native client
menu acceptance from the browser mock.

On Windows, Python's Store alias may not be an interpreter. Run Python gates with:

```text
uv run --python 3.12 --no-project python -B scripts/toolchain_gate.py
```

The toolchain gate resolves current CI Rust. Use the version it reports, rather
than a version copied from an old execution record. The manifest's minimum Rust
is a separate compatibility check. Frontend uses Node 22 and pinned pnpm 11.

```text
cargo +<verified-ci-version> fmt --all -- --check
cargo +<verified-ci-version> clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +<verified-ci-version> test --workspace --all-features --locked
cargo +1.90.0 check --workspace --all-targets --locked
pnpm --dir ui build
pnpm --dir ui test
uv run --python 3.12 --no-project python -B scripts/command_surface_test.py --self-test
uv run --python 3.12 --no-project python -B scripts/orch_docs_test.py --self-test
git diff --check
```

Run Core/desktop `--no-default-features` and `scripts/feature_matrix_gate.py` when
feature boundaries change. Workspace feature unification is not proof of the
minimal build. Run UI budget/layout/interaction and local mock-upstream smoke for
their relevant surfaces. Record failures and unrun platform checks accurately.
Avoid whole-tree formatting while another worker is editing; format owned files.

## Commits and evidence

- Commit only when requested, using independently understandable, buildable slices.
- Use `<type>(<scope>): <imperative summary>` with a lowercase summary and at most
  72 characters for the whole subject. The validator's scope/verb allowlists are
  authoritative: `scripts/commit_contract_test.py`.
- Keep commit messages about their actual changes. Put plans, detailed test output
  and delivery status in `docs/development/`, not in commit subjects or long bodies.
- Validate the resulting range with `commit_contract_test.py --base <start> --head HEAD`.
  Inspect each committed diff and confirm unrelated staged work is preserved.
- Update node status, DAG reverse edges and evidence references together. Re-read
  the evidence registry before choosing an ID; it is shared by concurrent sessions.
- Leave local runtime/client configuration and running gateways alone unless the
  user asks for those operations. A development build is not an authorized deployment.
