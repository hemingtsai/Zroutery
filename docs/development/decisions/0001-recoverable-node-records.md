# ADR-0001: Recoverable Node Record Contract

## Decision state

Unresolved. The provisional record format is implemented by ORCH-DOCS so the
repository can recover the audit, but the orchestrator must review the
schema before it becomes a compatibility promise.

## Context

The repository previously had a prose audit and workflow documents but no
machine-readable node state, dependency graph, evidence registry, or regression
ledger. A commit or stage tag could therefore be mistaken for current
acceptance. The current audit explicitly requires legal state values,
dependencies, dependents, gates, blockers, unblock conditions, and exact
evidence.

## Provisional decision

Use one JSON object per node under `docs/development/node-status/`, with the
following stable fields:

- `schema_version: 1`;
- `node_id`, `display_name`, and one legal `status`;
- `implementation_state`, `dependencies`, `dependents`, and `required_gates`;
- `blocked_by`, `unblock_condition`, `evidence`, `source_references`, and
  `notes`.

The evidence registry owns command provenance and caveats. The DAG document
owns the human-readable graph. The regression ledger owns REG-001 through
REG-010. Historical tags remain provenance only.

## Open questions

1. Should status records be append-only revisions, or may a worker replace the
   record when the evidence changes?
2. Which orchestrator role is authoritative for moving a node from `VALIDATING`
   to `DONE` after a merge?
3. Should evidence IDs be immutable per command run, and what retention policy
   applies to superseded local results?
4. How should a node represent an external dependency that is not a repository
   node without weakening the dependency/dependent symmetry rule?

## Consequences until resolved

- A status is recoverable without reading implementation intent.
- A failed or blocked node cannot be hidden by a tag or an aggregate stage.
- Documentation validation can fail independently of implementation tests.
- The provisional schema may receive a versioned migration before external
  automation consumes it.

## Related records

- `docs/development/architecture.md`
- `docs/development/dependency-dag.md`
- `docs/development/evidence-registry.md`
- `docs/development/roadmap.md`
