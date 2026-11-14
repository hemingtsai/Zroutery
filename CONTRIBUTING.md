# Contributing

> Authority: guide · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/commit_contract_test.py --self-test`

Human onboarding. The rules live in [AGENTS.md](AGENTS.md),
[verification.md](docs/development/verification.md) and [WORKFLOW.md](docs/development/WORKFLOW.md).

## Read first

1. [AGENTS.md](AGENTS.md) — agent and contributor boundaries.
2. [docs/development/verification.md](docs/development/verification.md) — toolchain and gates.
3. [docs/development/architecture.md](docs/development/architecture.md) — layers and invariants.

## Toolchain

Resolve the compiler CI uses, then use it for lint and tests:

```text
python3 -B scripts/toolchain_gate.py
cargo +<reported-version> fmt --all -- --check
cargo +<reported-version> clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +<reported-version> test --workspace --all-features --locked
```

The declared minimum (`Cargo.toml::rust-version`) is a separate check:

```text
cargo +1.90.0 check --workspace --all-targets --locked
```

The frontend is Node 22 with the pinned pnpm (`package.json::packageManager`),
and the workspace also carries the plugin crates (`Cargo.toml::workspace`).

## Commit format

```text
<type>(<scope>): <imperative summary>
```

The validator is authoritative: `scripts/commit_contract_test.py`. It owns the
type/scope/verb allowlists; do not copy them into prose. Validate the range:

```text
python3 -B scripts/commit_contract_test.py --base <start> --head HEAD
```

Commit when the change is verified, in one-file or one-module commits. Keep the
subject about the change, not the task prompt, roadmap or test transcript.

## Before you call it done

- Tier 1 in [verification.md](docs/development/verification.md) is green; `pnpm install` runs the `prepare` hook install, or run `git config core.hooksPath .githooks` once.
- Affected documents changed in the same commit:
  `python3 -B scripts/docs_impact.py --base <start> --head HEAD`.
- Every factual sentence you added cites a source you actually read
  ([docs/DOCUMENTATION.md](docs/DOCUMENTATION.md)).
- No secret, request body or upstream credential is written into the repository.
- Platform checks you did not run are reported as unrun, not as passed.
