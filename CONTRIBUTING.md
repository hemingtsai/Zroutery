# Contributing

## Commit convention

Every commit must follow the conventional format:

    <type>(<scope>): <summary>

- type: `feat` | `fix` | `refactor` | `test` | `perf` | `docs` | `build` | `ci` | `chore`
- scope: `core` | `router` | `protocol` | `policy` | `runtime` | `ml` | `account` | `provider` | `integration` | `tauri` | `workflow` | `repo`
- summary: lowercase start, imperative, no trailing period, keep it under 72 characters

A commit message must describe only the changes the commit introduces.
Do not copy task prompts, roadmaps, gate reports, test transcripts, or
future-stage plans into commit messages — that information belongs in
`docs/development/`.

CI enforces this: the `commit-lint` workflow checks every commit in a
pull request. Full rules and rationale:
[docs/development/WORKFLOW.md](docs/development/WORKFLOW.md)

## Toolchain

The declared minimum is Rust 1.90, which is what `Cargo.toml`'s `rust-version`
says and what CI's `msrv` job pins. It is not a policy number: the locked
Tauri 2.12 family declares `rust-version = "1.90"`, while the source itself
needs 1.89, so the higher of the two is the declaration. Install it rather than
following whatever `stable` happens to be:

    rustup toolchain install 1.90.0

The frontend is Node 22 with pnpm 11, the pair CI's `reachability` job uses.
`packageManager` in the root `package.json` pins the pnpm major, so `corepack
enable` gets the version `pnpm-lock.yaml` was written by rather than whatever is
on `PATH`:

    corepack enable
    pnpm install

Then `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings` and `pnpm --dir ui build` are the checks a change
is expected to leave green. See [README.md](README.md#工具链).

### Why the frontend is ahead of the curve

`ui/` pins **Vite 8.2** and **TypeScript 7.0** while the rest of the ecosystem is
still largely on Vite 6 / TS 5.x. This is deliberate, not drift:

- Vite 8 builds with **Rolldown**. The production bundle is split by
  `vite.config.ts`'s `codeSplitting` into a React vendor chunk and a shared
  vendor chunk, which is what keeps the dashboard's first paint independent of
  the Tauri webview's cache state. The Vite 6 line cannot express that split, so
  "downgrade for stability" means giving up the chunking the UI was measured
  against.
- TS 7 is what `tsc --noEmit` in `pnpm --dir ui build` runs. The type layer is
  the only gate on the ~40 Tauri command signatures the dashboard calls, so it
  moves with the compiler the build actually uses.

The cost of being early is accepted knowingly: a plugin that has not caught up
shows up as a build failure rather than a runtime surprise, and CI runs the same
`pnpm --dir ui build` a developer does. A move back to Vite 6 + TS 5.6 is a
project-level decision that also requires re-measuring the bundle split; it is
not a version bump to make in passing.
