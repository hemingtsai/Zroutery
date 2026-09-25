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
| `Status` | exactly one of `DONE`, `PARTIAL`, `BLOCKED`, or `FAILED` |

If any one of `Node`, `Gate`, or `Status` appears, all three must appear as
one final block in the order above. Duplicate, missing, reordered, malformed,
or differently-cased evidence trailers are rejected. A non-Node repository
commit may omit the block; a Node commit must not use a partial block.

`Status` records the worker's legal final state, but it cannot upgrade a Node
without passing that Node's required gates. In particular, a successful lint
or a commit hash is not a `DONE` claim for the implementation being described.

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
