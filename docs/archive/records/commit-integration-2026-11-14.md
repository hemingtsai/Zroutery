# Commit integration and CI verification — 2026-11-14

## Scope and commit boundaries

The user requested all existing changes be split into commits, pushed to `dev`,
then synchronized to `main` without re-merging another author's work. The local
starting point was `083bf01`; the remote `dev` starting point was `26d887f`.
`main`/`origin/main` at `3428895` is an ancestor of the integration tip.

Slices separate thinking conversion, Codex Guardian contracts, plugin API/broker/
worker foundations, native Core bindings, theme sources, exact-model takeover,
desktop management, native gradient validation, Radix behavior/dependencies,
theme regression baselines, CI lint/source-reference repairs, icon assets and
the separate AgentRouter check-in research record.

Concurrent UI writes appeared during the initial staging. Work stopped until
the user confirmed the author had finished. The initial theme/compiler commit
contained a truncated compiler tail; a separate repair completed it and restored
keyboard recipe metadata. No unrelated changes were discarded. Three subject
verbs in this task's unpublished range were corrected without changing trees,
message bodies, authors, committers or dates. Earlier remote history is unchanged.

## Toolchain and completed local checks

The Python toolchain gate could not resolve static.rust-lang.org through its
network path. A separate fetch of the exact official channel manifest resolved
Rust **1.100.0 (0ef1bc386)** and Cargo **3acc79c0c**, matching the installed tools.
Frontend checks used Node **22.23.5**, pinned pnpm **11.22.0**, Vite 8.2.1 and
TypeScript 7.0.2.

- Workspace formatting and strict Clippy, all targets/all features, passed.
  Platform-conditional binary discovery and three ML test loops were repaired
  without suppressing lints. Two plugin credential needless borrows were fixed.
- Full workspace all-feature tests passed. The initial run recorded 3505 passed,
  zero failed and 22 ignored; later native gradient tests also passed.
- Workspace tests excluding headless passed on the final retry. An earlier
  run failed in `upstream_stream_error_test` without a captured failing test name;
  all six tests in that suite passed on retry. The initial failure is not
  retroactively diagnosed.
- The plain workspace test command passed as a distinct earlier run.
- Rust 1.90.0 locked/all-target workspace check passed, including the final
  native gradient change.
- Feature matrix passed all **76** manifest-derived combinations. Core and
  desktop no-default checks passed separately; desktop no-default still reports
  pre-existing/gated unused-import/helper/dependency warnings.
- Frozen-lockfile frontend install, build and **93** tests passed. A staged
  desktop/plugin slice was also checked from an isolated archive with its own
  locked dependencies; its TypeScript check passed before Radix migration.
- Command surface and self-test passed: 106 interface commands, 281 registered
  handlers, exact capability correspondence. Orchestration docs/self-tests and
  the commit validator self-tests passed.
- All **31** UI interaction cases passed. Industrial, MD3 and Fluent each passed
  all 12 layout surfaces/79 controls; all 15 layout self-tests passed.
- Narrow-window overflow passed all nine pages plus a forced-width table probe.
  Its initial failure was caused by a newly introduced `__SKIN__` placeholder
  and retired Tab IDs in the shared harness; the script now fills the placeholder
  and uses the Radix data attributes.
- Official-source theme checks passed. The v3.2 regression baseline passed
  **1620** component/state samples with zero differences. This is not legacy
  visual parity: the initial old-CSS comparison reported **822 differences**.
  `--legacy-baseline` preserves that comparison; no old result is erased.
- The headless binary built and all isolated mock-upstream smoke checks passed.
- Tauri `build --no-bundle` built the self-contained Windows application at
  `target/release/zroutery.exe`, using `custom-protocol`. It was not deployed or
  launched against real client/runtime configuration. The build reports a
  STATIC_VCRUNTIME deprecation warning.
- `pnpm audit --prod --registry=https://registry.npmjs.org` reported no known
  vulnerabilities.

## Dependency and historical contract failures

RustSec's latest fetched database at `4ef2df8` reports **9 vulnerabilities** in the
locked tree: libsqlite3-sys (RUSTSEC-2026-0459), zip (0367, 0368, 0369, 0429),
zstd (0391), zstd-safe (0398, 0403), and zstd-sys (0410), plus 18 allowed warnings.
The fetch initially failed through cargo-audit's HTTP path; system Git fetched
the official database and the audit ran against an isolated copy of that exact
revision with `--no-fetch`, not against a stale database. No advisory was ignored.
Plugin archive loading does not use ZIP extraction, but this is not an exemption
from the dependency gate. Dependency upgrades require their own verified slice.

The pre-existing `origin/main..dev` range contains five rejected historical
subjects (`458d982`, `1f31a88`, `72f5b1e`, `31b2107`, `7010894`). This task does not
rewrite published history or weaken the validator. Its new `origin/dev..HEAD`
range is validated separately. Synchronizing `main` may expose those existing
historical commit-contract failures to push CI.

## Verification limits and delivery

Local Windows checks do not establish Linux/macOS runtime acceptance, native
client model-menu behavior, real credential flows or six-state WebView2 accent
acceptance. Plugin control/UI worker dispatch remains deliberately unavailable;
successful sandbox probing is not proof of a complete extension execution path.
No release, installer, tag or running gateway/client takeover was requested.

Dependency installation during isolated slice verification initially reused a
directory link to the live frontend, producing duplicate dependency resolution
and an 875147-byte JavaScript bundle. The affected generated dependency folders
were moved intact to a task-specific backup under `target/`; a clean locked
install and final bundle measurement are recorded below. No source was reset.

The clean install restored single dependency resolution (188 modules instead of
259); build and 93 frontend tests passed. Final JavaScript is **799803 bytes**
of the Radix feature allowance **848000**, CSS **177539** of **180000**, largest
chunk **203089** of **320000**. The allowance change belongs to the Radix commit
and is explained there, not to an unexplained budget exemption. The earlier
875147-byte result remains a failed contaminated-dependency measurement.

The new push range contains the existing `083bf01` commit plus this task's
independently described slices; all 15 commits checked before this report commit
pass the unchanged commit validator. Working/index changes were inspected at
each staging boundary. Final synchronization and remote CI outcome are reported
separately from local implementation/build acceptance.
