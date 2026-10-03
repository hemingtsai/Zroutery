# Zroutery Development Workflow

This directory holds the development record: workflow rules, history
treatment records, and stage documentation. Git commit history carries code
changes and concise, auditable commit evidence; process and planning
artifacts live here.

## Commit Content Rule

Git commit messages describe only the changes introduced by the commit. They
must not be used as a copy of the task prompt, roadmap, workflow, gate report,
test transcript, future-stage plan, or implementation diary.

A commit is an auditable engineering milestone only when its subject states
one concrete intent. A commit's existence is not proof that its Node is
`DONE`; the Node status still comes from its required gates and evidence.

## Conventional Commit Contract

Every non-merge commit uses this form:

```text
<type>(<scope>): <imperative summary>
```

The subject must start with a lowercase summary character, use a finite
imperative verb allowlist, avoid a trailing period, and be no more than **72
characters for the complete subject**, including `type(scope): `. Whitespace
is trimmed and the subject is one line.

### Type allowlist

Only these types are accepted:

| Type | Meaning |
|---|---|
| `feat` | add intended capability |
| `fix` | correct a defect |
| `test` | add or correct tests |
| `refactor` | change structure without changing intended behavior |
| `perf` | improve a measured performance property |
| `docs` | change documentation |
| `build` | change build or packaging behavior |
| `ci` | change CI or automation |
| `chore` | perform a bounded maintenance change |

`misc`, `update`, `change`, `stuff`, `work`, `tmp`, `final`, and `done` are
not types. A vague word cannot be made acceptable by pairing it with a
conventional type.

### Scope allowlist

Scopes are finite and describe the affected boundary. The module allowlist is
the union of established repository boundaries and the current Node modules:

```text
core
router
protocol
policy
runtime
ml
account
provider
integration
tauri
workflow
repo
routing
observation
stats
newapi
migration
takeover
server
decision
shadow
reward
replay
dataset
model
ui
docs
tests
```

The established `router`, `runtime`, `provider`, `integration`, `tauri`, and
`repo` scopes remain valid compatibility boundaries. `workflow` is the
explicit orchestration/documentation boundary used by Node-contract commits
such as `ci(workflow): ...`; it is not a wildcard.

The current finite stage scopes are:

```text
7e1
7e2a
7e2b
7e2c
7e2d
7e2e
7e2f
7e3
7f
7g
7h
```

Examples such as `7e9`, `7e2z`, `stage-1`, `misc`, and `unknown` are not
valid scopes. Do not replace this finite list with a pattern such as
`stage-[0-9]+` or an arbitrary identifier; an unrecognized boundary must be
reviewed before it is added.

### Subject regex

The workflow's structural expression is:

```text
^(feat|fix|refactor|test|perf|docs|build|ci|chore)\((core|router|protocol|policy|runtime|ml|account|provider|integration|tauri|workflow|repo|routing|observation|stats|newapi|migration|takeover|server|decision|shadow|reward|replay|dataset|model|ui|docs|tests|7e1|7e2a|7e2b|7e2c|7e2d|7e2e|7e2f|7e3|7f|7g|7h)\): [a-z][^\r\n]*$
```

The expression is followed by deterministic checks in
[`scripts/commit_contract_test.py`](../../scripts/commit_contract_test.py):

- complete subject length `<= 72` and no leading/trailing summary whitespace;
- first summary word belongs to the finite imperative verb set;
- no trailing `.`, `!`, or `?`;
- no vague summary such as `misc fixes`, `update project`, `final
  implementation`, `make tests pass`, or `finish everything`; and
- no short report/transcript marker such as `test transcript`, `test output`,
  `gate report`, `roadmap`, `next stage`, `Stage N`, `Gate ... PASS`, or a
  `PASS`/`FAILED` result pasted into the subject.

The old expression was materially different. It rejected valid current
subjects such as `feat(7e2a): add candidate-masked decision distribution` and
`fix(routing): prevent duplicate routing planning`, while accepting vague
`feat(ml): added some new model stuff.`. The established compatibility scopes
remain accepted; the new fixtures preserve both the newly authorized scopes
and the vague-subject regression case.

## Commit Body

A useful body is concise and answers only the change:

```text
Why:
<why the change is needed>

What:
<what changed>

Verification:
<commands or concise results>

Architecture:
<compatibility or authority note, when relevant>
```

`Verification:` may name a command or a short result; it must not contain a
long transcript. Roadmap, stage, implementation, risk, and gate-report fields
belong in `docs/development/`, not in the subject. The exact legal evidence
trailers below are the exception to the old gate-report filter.

## Node Evidence Trailers

A Node milestone commit ends with a blank line followed by all three trailers
in this exact order:

```text
Node: COMMIT-CONTRACT
Gate: commit-contract
Status: DONE
```

The value rules are deterministic:

| Trailer | Accepted value |
|---|---|
| `Node` | uppercase hyphenated identifier, such as `COMMIT-CONTRACT` or `7E-1A` |
| `Gate` | lowercase hyphenated identifier, such as `commit-contract` or `model-identity-replay` |
| `Status` | exactly one of `QUEUED`, `READY`, `RUNNING`, `VALIDATING`, `DONE`, `PARTIAL`, `BLOCKED`, `FAILED`, or `REVALIDATE` |

If any one of `Node`, `Gate`, or `Status` appears, all three must appear as
one final block in the order above. Duplicate, missing, reordered, malformed,
or differently-cased evidence trailers are rejected. A non-Node repository
commit may omit the block; a Node commit must not use a partial block.

`Status` is a commit-level global Node state, not the worker's final report
state. It accepts the complete canonical vocabulary documented by the node
state architecture: `QUEUED`, `READY`, `RUNNING`, `VALIDATING`, `DONE`,
`PARTIAL`, `BLOCKED`, `FAILED`, and `REVALIDATE`. Intermediate commits may
therefore record `VALIDATING` or `REVALIDATE` accurately.

The worker's final report remains restricted to `DONE`, `PARTIAL`, `BLOCKED`,
or `FAILED`; that narrower completion vocabulary does not narrow commit-level
trailers. A commit-level state cannot upgrade a Node without passing that
Node's required gates. In particular, a successful lint or a commit hash is
not a `DONE` claim for the implementation being described.

## The Verification Matrix

These are the gates a change is accepted against. They are written down here
because they used to live only in dispatch briefs, and a contract that lives
only in prose can change meaning without anyone noticing.

**That already happened once.** `cargo test --workspace` was the default-features
gate for the whole project, and it stopped being one when the headless proxy
became its own package: that package depends on `zroutery-core` with
`features = ["ml"]` unconditionally, so its presence in the workspace graph turns
`ml` on for the entire resolution. The command kept passing, so nothing
complained, while the gate it named was no longer being run.

| Gate | Command | What it actually checks |
|---|---|---|
| check | `cargo check --workspace` | everything compiles |
| clippy | `cargo clippy -p zroutery-core --all-targets --all-features -- -D warnings` | no lint debt |
| **default features** | `cargo test --workspace --exclude zroutery-headless` | the build the desktop app ships, with `ml` genuinely off |
| whole workspace | `cargo test --workspace` | everything, with `ml` on because the headless package forces it |
| all features | `cargo test -p zroutery-core --all-features` | the `ml` surface on its own |
| boundary | `cargo test -p zroutery-core --all-features --test activation_test` | the shipped product cannot name the installer |
| docs | `python -B scripts/orch_docs_test.py` | node records, DAG and evidence registry agree |
| contract | `python -B scripts/commit_contract_test.py --base <rev> --head <rev>` | commit subjects and trailers over the range |
| toolchain | `python -B scripts/toolchain_gate.py` | the local toolchain is the build CI resolves, so a green matrix speaks to CI |
| minimum toolchain | `cargo +1.89.0 check --workspace --locked --all-targets`, the version `Cargo.toml` declares | the declared minimum really builds the locked tree, test targets included |
| whitespace | `git diff --check` | no trailing damage |

The default-features row is spelled the way it is on purpose. `--workspace`
without the exclusion runs 1733 tests; with the exclusion it runs 999, and the
ml-gated `statistics_test` file compiles to zero instead of running 22. If the
default-features row ever stops being a default-features run, the matrix itself
is wrong and must be corrected in the same commit that changed it.

CI runs the same set (`.github/workflows/ci.yml`, plus `packaging.yml` for the
installer gate on a schedule), and no step in it may be made non-blocking. If a
gate is too expensive for every push, limit **when** it runs, never **whether
its result counts**.

### The toolchain the matrix is run under

Every row above is a claim about a **toolchain version**, and the version is not
in the table. CI resolves the floating `dtolnay/rust-toolchain@stable`, which was
`1.99.0` when run 6 executed, while this checkout's default `stable` was still
`1.97.1` from July. That gap is not hypothetical: the first push of the shadow
branch passed all eleven local gates and failed CI's clippy job, because 1.99
added two lints 1.97.1 does not have (`eacbc55`; E-111). A green local matrix
therefore implies nothing about CI until the local toolchain is the one CI
resolves.

Two consequences worth keeping:

- To find out what CI actually resolved, read
  `https://static.rust-lang.org/dist/channel-rust-stable.toml` and take
  `[pkg.rust] version`, not the first `version` in the file, which is Cargo's.
- `rustup update stable` can fail with `os error 32` when another project's
  build holds a file inside the shared toolchain directory. Do not kill that
  build. `rustup toolchain install <version> --profile minimal --component
  clippy` writes a separate directory and leaves the running build alone. Note
  that a minimal profile has **no `rustfmt`**, so `cargo fmt --check` under it
  fails as a missing component rather than as a formatting violation; add
  `--component rustfmt` before believing that result.

Both `rustfmt` versions currently agree on every file in the workspace, so there
is no formatting drift to reconcile.

One honest asymmetry in the table: the clippy row uses `--all-targets` and CI's
clippy step does not, so CI checks strictly less than this matrix does. The
stricter local form is kept deliberately, and the gap is recorded rather than
quietly narrowed in either direction.

### The toolchain row is deliberately not a CI step

`scripts/toolchain_gate.py` fails when the local toolchain is not the build the
floating `stable` channel resolves to. It is in the **local** matrix and it is
**not** wired into `ci.yml`, and the reason is worth stating rather than leaving
as an omission: inside CI the toolchain is by definition the one CI installed, so
the gate could not fail there. A step that cannot fail is not a gate, and wiring
it would be theatre that costs a network fetch on every push. Its whole job is to
stop a *local* green matrix from being read as a claim about CI.

It resolves the manifest over the network, so when that fetch fails it reports
`INCONCLUSIVE` and exits non-zero rather than quietly passing; `--accept-unresolved`
is the documented offline opt-out and CI does not pass it. That path was not
designed and then checked — a real timeout during development exercised it.

Two traps in the manifest are closed by `--self-test`, which is why that mode
exists rather than a comment:

- `[pkg.cargo]` sits at byte 44 and `[pkg.rust]` at byte 80035, so reading the
  first `version =` in the file yields **Cargo's** number and calls it the
  compiler's. The parser is anchored on the section header.
- The manifest and the binary number the same Cargo build differently —
  `0.100.0` versus `1.99.0` — so a release-number comparison reports DRIFT for
  a toolchain that is exactly CI's. The manifest also carries a
  `git_commit_hash` beside the version that, for cargo, holds the **rustc**
  commit. The comparison key is therefore the hash inside the version's
  parentheses, which both sides state identically; release numbers are printed
  for the reader and never used as the verdict.

The gate was proven in both directions before being recorded: `DRIFT` and exit 1
against the real 1.97.1-versus-1.99.0 gap, and `MATCH` and exit 0 with the
1.99.0 toolchain placed ahead on `PATH`, where the output deliberately shows two
different cargo version numbers agreeing on the hash.

### The declared minimum is verified

The workspace declares `rust-version = "1.89"`, and CI's `msrv` job pins exactly
that compiler and runs `cargo check --workspace --locked --all-targets`. The
declaration is therefore a claim a red job can contradict, instead of a sentence
in a manifest that no job reads.

The number is the union of two floors read off the tree rather than chosen:

- the locked dependencies need 1.88 — `icu_normalizer` 2.3.0, `time` 0.3.55,
  `darling` 0.23.0, `image` 0.25.10 and `plist` 1.10.0 all declare
  `rust-version = "1.88.0"` — so the old `1.80` could not resolve this
  `Cargo.lock` at all;
- the source needs 1.89 — `ml/features.rs` builds
  `Some(&ModelCapabilities { .. })` inside a `let`, and temporary lifetime
  extension only reaches through a tuple-variant constructor from 1.89
  ([rust-lang/rust#140593](https://github.com/rust-lang/rust/pull/140593)), so
  the test targets do not compile at 1.88.

`--all-targets` is not decoration: the 1.89 floor comes from test code, so a
check that skipped test targets would certify 1.88 and be wrong.

The toolchain row above is a different claim, and it stays local-only.

## Fixtures and Enforcement

The deterministic validator has no third-party dependencies:

```bash
python scripts/commit_contract_test.py --self-test
```

It checks valid established and current module scopes, finite stage scopes,
unknown scopes, vague and past-tense subjects, the length limit,
transcript/report subjects, and
accept/reject trailer cases. The pull-request workflow runs that suite first,
then validates every non-merge commit introduced by the PR:

```bash
python3 scripts/commit_contract_test.py --base "$BASE_SHA" --head HEAD
```

Merge commits are intentionally excluded because this contract governs
ordinary Node commits, not Git's merge-message format.

### Valid subject fixtures

```text
feat(ml): add calibrated decision distribution
fix(routing): prevent duplicate routing planning
fix(tauri): harden config permissions
feat(provider): add provider health checks
test(integration): cover provider failover
test(7e1): add deterministic decision replay coverage
docs(workflow): align commit lint with node evidence
ci(workflow): align commit lint with node evidence
```

### Invalid subject fixtures

```text
feat(ml): added some new model stuff.
chore(repo): update project
fix(7e9): prevent duplicate routing planning
fix(stage-1): prevent duplicate routing planning
fix(misc): repair fallback logic
fix(core): Repair fallback logic
chore(workflow): update project
test(ml): add test transcript and gate report
```

### Trailer fixtures

Accepted:

```text
fix(shadow): prevent duplicate routing planning

Why: Keep the production plan authoritative.
What: Record one decision identity.
Verification: Add a deterministic fixture.

Node: COMMIT-CONTRACT
Gate: commit-contract
Status: DONE
```

Rejected:

```text
fix(core): repair fallback logic

Node: COMMIT-CONTRACT
Gate: commit-contract
Status: PASS
```

```text
fix(core): repair fallback logic

Node: COMMIT-CONTRACT
Status: DONE
```

```text
fix(core): repair fallback logic

Gate: commit-contract
Node: COMMIT-CONTRACT
Status: DONE
```

## History

Main was rewritten once (2026-09-11) from 73 commits to 34 curated commits —
see `history-treatment-map.md` and `tag-remap.md` in this directory. Old
history remains reachable via `archive/pre-history-treatment`. Further
history rewrites are frozen.
