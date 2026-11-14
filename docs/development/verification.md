# Verification

> Authority: normative · Owner: backend · Last verified: 2026-11-14 · Verified by: `scripts/docs_check.py`

This is the only document that defines the toolchain and the gates. Other
documents point here; they do not restate a command. Current pass/fail status
lives in `current-state.md`, not here.

## Scope

- Applies to every Rust, frontend, contract and documentation change.
- A gate that cannot run is reported as **unrun**, never as passed.
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
| documentation | `python3 -B scripts/docs_sources.py --check`, `python3 -B scripts/docs_check.py`, `python3 -B scripts/docs_parity.py --check` | anchors, links, sizes, freshness, three-language shape |
| docs impact | `python3 -B scripts/docs_impact.py --base <base> --head HEAD` | every affected document changed |
| whitespace | `git diff --check` | trailing damage |

The `core` CI job runs clippy and tests for `zroutery-core` only
(`.github/workflows/ci.yml::core`); the workspace-wide clippy above is the local
contract and is stricter. A CI pass on the narrower command does not replace it.

## Tier 2 — surface-specific, not optional

Some Tier 2 rows block CI; others are local-only. The "When" column says which
surface they cover; the "CI" column says whether the pipeline runs them.

| Gate | Command | When | CI |
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
| UI layout | — | **no** | local only |
| UI interaction | — | **no** | local only |
| MSRV check | `msrv` | yes | 1.90.0, locked, all targets |
| commit contract | `contracts` | yes | range check |
| orchestration docs | — | no | retired; node and evidence records are archived |
| command surface | `contracts` | yes | |
| docs checks | `contracts` | yes | anchors, index, impact |
| Rust advisories | `audit` | nightly; warning on PR | `continue-on-error` on pull requests |
| frontend advisories | `audit` | nightly; warning on PR | production dependencies only |
| installers | `packaging` | nightly | Windows NSIS |

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

- `docs_sources.py --check` resolves every `path::symbol` and `path#Lx` anchor in
  active documents.
- `docs_check.py` enforces manifest coverage, links, banners, size budgets and
  generated-file freshness. A zero-pending migration renders as complete rather
  than as an empty table. A path-like inline code span is resolved or
  redirected, and every archived file must be reachable through `redirects.json`.
- `docs_parity.py` checks that the three READMEs have matching headings, fences,
  images and source anchors; prose may differ, shape may not.
- `docs_check.py` compares `docs/verified.json` source content hashes with
  the current tree. A moved source means the document must be re-read.
- `docs_impact.py` fails when a changed source file is cited by a document that
  did not change. There is no bypass flag.

## See also

- [Architecture](architecture.md)
- [Workflow](WORKFLOW.md)
- [Current state](current-state.md)
- [Documentation standard](../DOCUMENTATION.md)
