# Development Workflow

> Authority: normative · Owner: docs · Last verified: 2026-11-14 · Verified by: `scripts/commit_contract_test.py --self-test`

This document defines how a commit and its evidence are formed. The toolchain and
every gate live in [verification.md](verification.md); current status lives in
[current-state.md](current-state.md); audits and findings live in GitHub Issues.

## Freshness

A document's `reviewed_commit` is the commit at which its sources were last
read. When a source changes, commit the source first, then commit the document
with `reviewed_commit` pointing at that source commit. `freshness_report.py
--check` fails until the point is advanced; do not update only the hash.

## Commit content

- One commit is one intention, independently understandable and buildable.
- A commit describes only the change it introduces. Task prompts, roadmaps, gate
  reports, test transcripts and future plans belong in an Issue or a record, not
  in a commit subject or a long body.
- Commit when the change is verified. One file or one module per commit; do not
  batch unrelated files.
- A commit subject is not a status claim. A green lint or a commit hash does not
  make a node `DONE`.

## Subject

```text
<type>(<scope>): <imperative summary>
```

- The complete subject is at most 72 characters, lowercase, one line, no trailing
  `.`, `!` or `?`.
- The summary starts with an imperative verb.
- The type, scope and verb allowlists are finite and owned by
  `scripts/commit_contract_test.py`; do not copy them into prose. Run its
  self-test when the contract changes.
- Vague summaries (`misc fixes`, `update project`, `final implementation`,
  `make tests pass`) are rejected.

The validator (`scripts/commit_contract_test.py::TRAILER_KEYS`,
`scripts/commit_contract_test.py::IMPERATIVE_VERBS`) is authoritative. A subject
it rejects is not a judgement about the change; correct the grammar and keep the
change.

## Body

A useful body answers only the change:

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

`Verification:` names a command or a short result; it must not contain a long
transcript. Plans, stage reports and delivery diaries stay in Issues or
`docs/` records.

## Evidence

- New evidence is a PR, an Issue, a test name, or a `docs/verified.json` source
  hash. It is not a narrative pasted into a commit.
- The legacy `Node` / `Gate` / `Status` trailers are accepted only as a complete
  final block, in that order, in historical commits. They are not required for
  new work and must not be added to new commits.
- If any one of the three legacy trailers appears, all three must appear and be
  well formed; the validator rejects a partial block.
- A commit-level state cannot upgrade a node without that node's gates. Current
  node state is historical; use `current-state.md` and Issues.

## Documentation synchronization

Every change that touches a cited source, a gate, the toolchain, a configuration
key, a wire field or a release step updates the affected documents in the same
change.

```text
python3 -B scripts/docs_impact.py --base <start> --head HEAD
```

- The affected set is computed from the documents' own source anchors and the
  manifest triggers (`scripts/docs_impact.py::main`).
- There is no `none` escape. If a trigger is wrong, fix the manifest or the
  citation in the same change.
- Every factual sentence added to a normative or reference document cites a
  source read in the same change; see [../DOCUMENTATION.md](../DOCUMENTATION.md).

## Branch, staging and merge

- Check the branch, working tree and staged changes before editing or committing.
  Existing changes may belong to the user or another agent.
- Stage explicit paths. A backend commit must not capture staged frontend work.
  Do not reset, stash or reformat another worker's files.
- One owner publishes commits at a time. Re-read a shared file immediately before
  patching it.
- Validate the range before publishing:

```text
python3 -B scripts/commit_contract_test.py --base <start> --head HEAD
```

## Enforcement

CI runs the commit contract over the event range
(`.github/workflows/ci.yml::contracts`). The range is the PR base or the push's
previous revision; a first push with no prior revision has no range to check.
The validator's self-test proves it can still reject a bad subject.

## History

The 2026 commit-contract rewrite, its failed attempts and its tree-equality
evidence are part of git history and the archived execution records. They are not
rules: this document states the current contract, the validator enforces it, and
`docs/archive/` preserves how it was reached.

## See also

- [Verification](verification.md)
- [Current state](current-state.md)
- [Documentation standard](../DOCUMENTATION.md)
