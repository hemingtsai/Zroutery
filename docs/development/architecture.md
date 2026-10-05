# Zroutery Architecture Memory

## Scope and baseline

This document is the architecture memory for the audited repository. It is a
record of current boundaries and verified seams, not a claim that a roadmap item
is complete. The immutable audit input is
[`repository-audit-2026-09-25.md`](repository-audit-2026-09-25.md). The
orchestration baseline used for this record is
`c8bed1baf7d1f80138ecbdd3f25812835b0ea29f`; the commands cited as audit
execution evidence were run at the audited SHA
`c66072f341664f67c1cd1c761e5f5adf1980ce16`.

The current state vocabulary is intentionally small and is shared by every
record under `docs/development/node-status/`:

`QUEUED`, `READY`, `RUNNING`, `VALIDATING`, `DONE`, `PARTIAL`, `BLOCKED`,
`FAILED`, and `REVALIDATE`.

A tag, commit subject, or historical roadmap statement is provenance only. It
cannot move a node to `DONE` without the node's required gates and current
implementation evidence.

## Architectural layers

### Production Core

`zroutery-core` with its default feature set is the production authority. The
Tauri crate composes the Core server, configuration, secret storage, and the
operational UI. The Core boundary includes:

- ingress and canonical IR (`crates/zroutery-core/src/ir/`,
  `crates/zroutery-core/src/protocol/`, `crates/zroutery-core/src/query.rs`);
- provider/model registry, policy, candidate planning, and failover
  (`crates/zroutery-core/src/registry.rs`,
  `crates/zroutery-core/src/policy.rs`, `crates/zroutery-core/src/router.rs`);
- runtime observation, failure classification, statistics, budget, session,
  and response lifecycle (`crates/zroutery-core/src/observation.rs`,
  `crates/zroutery-core/src/failure.rs`, `crates/zroutery-core/src/stats_ext.rs`,
  `crates/zroutery-core/src/budget.rs`, `crates/zroutery-core/src/session.rs`,
  `crates/zroutery-core/src/ir/response.rs`);
- protocol-facing error and request/response recording
  (`crates/zroutery-core/src/error.rs`,
  `crates/zroutery-core/src/server/pipeline.rs`).

The default `zroutery-core` feature set is empty in
`crates/zroutery-core/Cargo.toml`. The desktop dependency in
`src-tauri/Cargo.toml` does not enable `ml` or `account`. Therefore the
production desktop path cannot be described as an online-learning or
account-lifecycle implementation.

### Optional ML libraries

The `ml` feature exposes libraries and test seams for feature extraction,
models, datasets, evaluation, reward, identity/replay, shadow evaluation, and
coordination under `crates/zroutery-core/src/ml/`. These modules are useful
offline and in all-feature test runs, but they are not evidence of a shipped
activation path. The shadow verdict is evidence-only; it cannot change the
production route. The current closure gaps recorded by the audit remain
authoritative:

- `7E-0` is `FAILED` until commit/checkpoint binding and replay lineage are
  repaired and revalidated;
- `7E-1` is `PARTIAL` because a meaningful counterfactual, retained
  decision-time input, and separate initial/final served identities are not
  closed;
- `7E-2A` is `BLOCKED` behind the identity/replay repairs.

The dependency direction is Core-authoritative:

```text
Core policy/router/observation/stats/session/account/protocol
                         ↑
                         └── ML features/models/reward/identity/shadow
                                      ↑
                               server composition
```

ML may consume Core-owned facts. Core must not depend on an ML model to make
a production routing decision. The optional feature boundary is therefore a
safety property, not merely a Cargo organization detail.

### Parallel sidecars and product surfaces

The following tracks are useful foundations but are not part of a verified
production learning loop:

- **Account / NewAPI:** account types, store, pure calculations, and an
  adapter exist behind optional features. The adapter currently treats a
  non-empty credential as authentication, synthesizes an active runtime, and
  leaves usage and quota unimplemented. Its next safe gate is a local mock
  protocol contract for auth, expiry, refresh, usage, and quota.
- **I2 Migration:** the state machine and local file mechanics are present.
  Process stop/start actions do not own the intended process lifecycle, and
  endpoint verification accepts any HTTP response. A typed action runner and
  local child-process fixtures are required.
- **I3 Takeover:** ownership state and temporary-adapter tests exist, but
  release does not apply the computed conflict resolution and no durable
  ownership manifest is available across process boundaries.
- **I4 Restore / Rollback:** conflict resolution is disconnected from release,
  and migration rollback can report `RolledBack` even when restoration emits
  warnings. This track is `FAILED` until restore and rollback are
  conflict-safe and observable.
- **Operational UI:** provider, model, routing, activity, and settings views
  build and are integrated. This is a `DONE` node for the current operational
  surface, not evidence that account, migration, takeover, shadow, or ML has
  a product surface.
- **New-track UI / Tauri surfaces:** these remain `BLOCKED` until the
  corresponding backend lifecycles are real and owned.
- **Runtime observability projection:** a read-only projection is `READY`;
  it must not mutate ML schemas or make an unverified activation decision.

## Contract boundaries and invariants

1. **Eligibility is authoritative.** A candidate rejected by policy
   requirements must not be reintroduced by a direct model-ID path. The
   current audit records this as a P1 repair (`REG-005`).
2. **Served identity is authoritative and explicit.** The initial planned
   candidate and the candidate that ultimately served after failover are
   separate identities (`REG-005`/`REG-007` territory). A route trace cannot
   silently substitute one for the other.
3. **Failure authority is singular.** `FailureClass`/`FailureImpact` and the
   public `Error` mapping must be reconciled before Outcome-based learning is
   trusted (`REG-006`).
4. **Cancellation is not success.** A client disconnect or interrupted stream
   cannot be finalized as a successful request (`REG-007`).
5. **Outcome is produced by production.** A production request must construct
   and fan out an `Outcome`/`Feedback` record before a dataset can be treated
   as representative. A type or local builder is not a production bridge.
6. **Model identity is content-bound.** A checkpoint, commit, parent, schema,
   and ordered event lineage must agree. A valid checkpoint paired with an
   unrelated commit is invalid evidence.
7. **Shadow is pure and non-authoritative.** Shadow evaluation may snapshot
   inputs and record a verdict, but it may not mutate routing/session/account
   state, retry a provider, or become a production action.
8. **External evidence is scoped.** A narrower CI job is not equivalent to a
   local all-target, all-feature, packaging, Windows, or browser gate. The
   evidence registry labels each result.

## Orchestration record model

Each file in `docs/development/node-status/` is one node record with the same
fields and types:

| Field | Contract |
|---|---|
| `schema_version` | Integer `1`. |
| `node_id` | Stable uppercase node identifier matching the file stem. |
| `display_name` | Human-readable name. |
| `status` | One of the nine legal state values above. |
| `implementation_state` | Evidence-based prose; never a percentage. |
| `dependencies` | Array of node IDs that are upstream inputs. |
| `dependents` | Array of node IDs whose upstream input is this node. |
| `required_gates` | Array of explicit acceptance gates. |
| `blocked_by` | Array of upstream node IDs currently preventing progress. |
| `unblock_condition` | Non-empty, testable condition for leaving a blocked or failed state. |
| `evidence` | Array of IDs defined in the evidence registry. |
| `source_references` | Array of repository-relative paths that exist. |
| `notes` | Array of concise caveats or provenance notes. |

Dependencies and dependents are kept symmetric. `blocked_by` is a subset of
`dependencies`; it names an active unresolved gate, while ordinary historical
inputs may remain dependencies without preventing a bounded repair from
starting. The graph must be acyclic.

## Current architecture decision posture

The following decisions are deliberately unresolved and are tracked as ADRs:

- [ADR-0001: recoverable node record contract](decisions/0001-recoverable-node-records.md)
  — review whether the JSON schema and evidence ownership rules need a
  versioned migration before the next implementation batch.

Until that decision is accepted, the safe architecture is the one recorded
here: production Core authority, optional ML libraries, and parallel sidecars
with no automatic activation or takeover.

## Settled decisions that changed this posture

Listed separately so the unresolved list above stays honest about what is
actually unresolved. ADR-0002 was previously in that list; ADR-0006 answered
its question, and leaving it there would have meant this document kept asking
something the repository had already decided.

- [ADR-0002: production ML boundary](decisions/0002-production-ml-boundary.md)
  — **superseded by ADR-0006**. It asked whether future online learning remains
  an optional in-process library, becomes a separately owned sidecar, or is
  exposed only through a read-only evidence service. ADR-0006 took the first
  option and answered it: ML reaches the routing path in-process, behind a gate,
  with a fallback and a rollback.
- [ADR-0006: the ML closed loop](decisions/0006-ml-closed-loop.md) — accepted;
  the loop is real and its five load-bearing properties are enumerated there.
- [ADR-0007: the loop is legible and reversible](decisions/0007-ml-operator-surface.md)
  — accepted; builds on ADR-0006 without superseding it. An operator can read
  what is routing their requests, and reverse it.
- [ADR-0008: nothing random decides](decisions/0008-nothing-random-decides.md)
  — accepted; builds on ADR-0006 and ADR-0007. No coin, clock, or hash decides a
  split, a verdict, or a served model.
