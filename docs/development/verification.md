# Verification

> Authority: normative · Owner: backend · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

This is the only document that defines the toolchain and the gates. Other
documents point here; they do not restate a command. Current pass/fail status
lives in `current-state.md`, not here.

## Scope

- Applies to every Rust, frontend, contract and documentation change.
- **pass** completed green; **fail** completed red.
- A gate that cannot run, or starts but cannot complete (a timeout), is
  **unrun**, never a pass; record the reason.
- Running a gate on a different compiler than CI resolves is **unverified**,
  even when it exits zero.

## Toolchain

The lint and test rows run on the compiler CI resolves, not on the checkout's
default toolchain. Resolve it first (`scripts/toolchain_gate.py::main`), then use
the version it prints:

```text
python3 -B scripts/toolchain_gate.py
cargo +<reported-version> fmt --all -- --check
cargo +<reported-version> clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +<reported-version> test --workspace --all-features --locked
```

The declared minimum is separate. It is the `rust-version` field
(`Cargo.toml::rust-version`), checked by the `msrv` job
(`.github/workflows/ci.yml::msrv`):

```text
cargo +1.90.0 check --workspace --all-targets --locked
```

Frontend uses Node 22 and the pnpm pin (`package.json::packageManager`,
`.github/workflows/ci.yml::setup-node`).

## Tier 1 — blocking, every change

These are fast, deterministic and offline after the first build. A change that
does not leave them green is not ready.

| Gate | Command | Checks |
|---|---|---|
| format | `cargo +<v> fmt --all -- --check` | layout only |
| clippy | `cargo +<v> clippy --workspace --all-targets --all-features --locked -- -D warnings` | every target, including tests, including the desktop crate |
| tests | `cargo +<v> test --workspace --all-features --locked` | behaviour |
| frontend build | `pnpm --dir ui build` | `tsc --noEmit` plus the production bundle |
| frontend tests | `pnpm --dir ui test` | component and helper behaviour |
| command surface | `python3 -B scripts/command_surface_test.py` | `api.ts` ↔ Rust ↔ capabilities (`scripts/command_surface_test.py::main`) |
| documentation | `python3 -B scripts/docs_contracts.py --base <base> --head HEAD` | every documentation contract, including freshness and state |
| whitespace | `git diff --check` | trailing damage |

The `core` CI job runs clippy and tests for `zroutery-core` only
(`.github/workflows/ci.yml::core`); the workspace-wide clippy above is the local
contract and is stricter. A CI pass on the narrower command does not replace it.

## Tier 2 — surface-specific, not optional

Some Tier 2 rows block CI; others are local-only. The "When" column says which
surface they cover; the "CI" column says whether the pipeline runs them.

| Gate | Command | When | CI |
|---|---|---|---|
| overflow | `python3 -B scripts/ui_overflow_test.py` | UI structure or styles (`scripts/ui_overflow_test.py::main`) | blocking (reachability) |
| layout | `python3 -B scripts/ui_layout_test.py` | UI controls or form structure (`scripts/ui_layout_test.py::main`) | local only |
| interaction | `python3 -B scripts/ui_interaction_test.py` | keyboard, drawers, dialogs, mock IPC | local only |
| budget | `python3 -B scripts/ui_budget_test.py` | shipped bundle (`scripts/ui_budget_test.py::main`) | blocking (reachability) |
| feature matrix | `python3 -B scripts/feature_matrix_gate.py` | manifest or feature boundaries | blocking (desktop) |
| smoke | `python3 -B scripts/smoke_test.py` | proxy, routing or upstream transport | blocking (desktop) |
| packaging | `.github/workflows/packaging.yml::installer` | installer or release changes | nightly |
| advisories | `cargo audit --file Cargo.lock`, `pnpm audit --prod` | dependency changes and nightly | PR warning / nightly |

## What CI actually runs

This table is the truth about coverage, including what is missing. It is
deliberately not flattering.

| Gate | CI job | Blocking | Note |
|---|---|---|---|
| fmt | `core` | yes | |
| clippy (core, not `--all-targets`) | `core` | yes | narrower than Tier 1 |
| core tests | `core` | yes | `zroutery-core` only |
| workspace check | `desktop` | yes | |
| default-feature tests | `desktop` | yes | headless package excluded |
| whole-workspace tests | `desktop` | yes | `ml` forced by the headless package |
| feature matrix | `desktop` | yes | every manifest combination |
| smoke | `desktop` | yes | local fake upstream |
| UI build and unit tests | `reachability` | yes | |
| UI budget | `reachability` | yes | |
| UI overflow | `reachability` | yes | |
| shipped desktop binary | `reachability` | yes | `tauri build --no-bundle` proves the shipped target builds with the custom protocol, not the dev server |
| desktop without the learning stack | `reachability` | yes | `cargo check -p zroutery --no-default-features` |
| UI layout | — | **no** | local only |
| UI interaction | — | **no** | local only |
| MSRV check | `msrv` | yes | 1.90.0, locked, all targets |
| commit contract | `contracts` | yes | range check |
| validator self-tests (mutation, feature matrix) | `contracts` | yes | each tool's `--self-test` runs before its real gate |
| orchestration docs | `contracts` | self-test only | the gate is retired; `orch_docs_test.py --self-test` still runs as a validator self-test |
| command surface | `contracts` | yes | |
| docs checks | `contracts` | yes | anchors, index, impact |
| Rust advisories | `audit` | nightly; warning on PR | `continue-on-error` on pull requests |
| frontend advisories | `audit` | nightly; warning on PR | production dependencies only |
| installers | `packaging` / `installer` | nightly | Windows NSIS and MSI; install, reinstall and uninstall |

Consequences, stated rather than discovered:

- `ui_layout_test.py` and `ui_interaction_test.py` are local gates. A green CI
  run says nothing about them.
- The CI clippy row is not `--all-targets` and not workspace-wide. Tier 1 is the
  stricter contract.
- The advisory job is allowed to be red on a pull request by design; a red
  nightly is actionable, a red PR warning is information.

## Failure reporting

- Report the exact command, the exact failure name and whether the gate ran.
- A gate blocked by a missing toolchain, a browser that cannot start, or a
  network mirror is **unrun**, with the reason.
- Never fix a flaky gate by loosening its threshold, skipping a test or deleting
  an assertion. Fix the gate or quarantine it with an owner and a date in
  `docs/development/state.json`; `current_state.py` renders it into
  `current-state.md`.
- A known-red gate is listed in `current-state.md`; do not silently "fix" an
  unrelated red gate in a change that did not cause it.

## Documentation gates

- `docs_contracts.py` is the single entry point: it runs every documentation
  contract below, including `freshness_report.py --check`. Branch protection
  must require the `contracts` job (its documentation step is named
  `Documentation contracts`); a required status check keys on the job, not on a
  step name. `--no-range` is for local diagnosis only, never for CI.
- `markdown_check.py` enforces fenced-code closure and table structure without
  a third-party parser.
- `docs_sources.py --check` resolves every `path::symbol` and `path#Lx` anchor in
  active documents.
- `docs_check.py` enforces manifest coverage, links, banners, size budgets and
  generated-file freshness. A zero-pending migration renders as complete rather
  than as an empty table. A path-like inline code span is resolved or
  redirected, every archived file must be reachable through `redirects.json`,
  and every Markdown link inside `docs/archive/` must resolve. A deleted module
  with no successor is allowed only through a `no-successor:<slug>` entry in
  `docs/archive/successors.json`.
- `coverage_check.py` requires every top-level Core module (public or private) to be owned, covered
  elsewhere, or explicitly unowned with a reason.
- `docs_check.py` validates `docs/docs.manifest.json` keys against
  `docs/docs.manifest.schema.json`, and banner authority/owner/verified-date
  against the manifest and ledger. It requires an ADR header (`Status`, `Date`, `Supersedes`), the
  `Context`/`Decision`/`Consequences`/`Sources` sections, and a resolvable
  `manifest.canonical[].home` anchor. `docs_contracts.py` also compares the
  executed step statuses with `state.json`; a state file that disagrees with an
  otherwise green run fails with the `record_state.py --no-range` repair
  command, while a red gate reports its own failure and is not repaired by
  recording it. A local check of the staged change passes
  `--base HEAD --include-staged`; the CI range check still covers the pushed
  commits. A zero or empty CI base
  prints a GitHub warning and leaves the range unverified; the final summary then
  says `local documentation contracts passed; range UNVERIFIED`, not
  `all passed`. An ADR `Date` must equal the file's first
  commit date (`git log --diff-filter=A --format=%cs`); `## Sources` needs at
  least one existing path or `path::symbol`.
- `invariant_check.py` requires every architecture invariant to name a real
  test anchor or an explicit `untested` reason; its self-test covers a non-test
  anchor. `current_state.py --self-test` rejects a hand edit.
- `current_state.py --check` fails when `current-state.md` is not the render of
  `state.json`; hand edits are not a source.
- `new_adr.py` creates the next ADR skeleton with the enforced header and
  sections; it is a generator, not a gate.
- `docs_sources.py` resolves definition-shaped symbols (fn/struct/enum/impl/
  const/mod, TOML/YAML keys, CSS custom properties, macro invocations), not
  arbitrary substrings. A Rust anchor of the form `path::Type::item` must
  resolve inside that type's `impl`/`enum`/`mod` block, and a URL on the line no
  longer hides the anchors beside it.
- `docs_verified.py` derives the cited source set and hashes into
  `docs/verified.json`; `--check` requires exact coverage and current hashes.
- `docs_review.py` records `reviewed_by`, `reviewed_at`, `reviewed_note`,
  `reviewed_sha` and a deterministic 3-5 sentence `sample` per verified document
  (including bullet items). The reviewer is a session other than the author's;
  `--note` records the agent, model and session. Recording is per document
  (`--doc`); a document edited after its review fails until it is recorded again;
  a reviewer equal to the last author fails.
- `docs_rules.py` checks that every `docs/rules.json` machine rule names an
  executed `docs_contracts.py` step, and that every gated step is registered.
- `record_state.py` runs `docs_contracts.py --json` and writes the documentation
  gate status, `ran_at` and commit into `state.json`; it also derives `head`
  from the latest code commit and `as_of` from today. `state_check.py` rejects
  a `head` with later code commits. Hand-filled doc statuses are not the source. `documentation contracts (local)` is the state truth;
  `documentation impact (range)` is not recorded in `state.json`; it runs both in
CI and from the installed pre-commit hook, which stages the change into the
range.
- Commit messages are checked by the CI range; no local `commit-msg` hook is
  installed. `state_check.py` also validates that every gate `commit` exists and
  is an ancestor of HEAD, and that `ran_at` is an ISO timestamp.
- `state_check.py` validates `state.json` against `state.schema.json`, including
  blocker dependencies/cycles and structured flaky/quarantine owner+review
  deadlines; an expired review date fails. Each gate declares `source`:
  `recorded` gates come from `record_state.py` and must carry `ran_at` and
  `commit`; `asserted` gates are hand-entered and are labelled as such in
  `current-state.md`.
- `docs_parity.py` checks that the three READMEs have matching headings, fences,
  images and source anchors; prose may differ, shape may not.
- `docs_check.py` compares `docs/verified.json` source content hashes with
  the current tree. A moved source means the document must be re-read.
- `freshness_report.py --check` fails when a cited source changed after the
  document's `reviewed_commit` and the document did not change in the same
  worktree or commit; otherwise the review point must be advanced.
  `--self-test` covers same-commit and worktree acceptance, advanced-baseline
  acceptance, source-only rejection and late-document rejection.
- `docs_impact.py` fails when a changed source file is cited by a document that
  did not change. There is no bypass flag; `--include-staged` is used by the
  pre-commit hook so an uncommitted documentation edit counts as changed.

## See also

- [Architecture](architecture.md)
- [Workflow](WORKFLOW.md)
- [Current state](current-state.md)
- [Documentation standard](../DOCUMENTATION.md)
