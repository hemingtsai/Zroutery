# ADR-0005: Core P1 Repair Node Boundaries

## Decision state

Accepted for dispatch sequencing. The node records remain the canonical status
source; this ADR defines ownership and merge order.

## Decision

Split the former broad `CORE-P1-REPAIR` package into five bounded nodes:

1. `CORE-P1-MEDIA-REQ` — canonical capability/media derivation and fail-closed
   protocol handling. Owns IR, config, registry, protocol, and media files.
2. `CORE-P1-FAILURE-AUTHORITY` — one Error-to-ClassifiedFailure mapping and
   impact table. Owns `failure.rs` and `error.rs` only.
3. `CORE-P1-ELIGIBILITY-TRACE` — request-capability eligibility, planned
   identity, and router-side classified-attempt adapter. Owns `policy.rs` and
   `router.rs` only.
4. `CORE-P1-OUTCOME-FEEDBACK` — Outcome/Feedback schema and pure dataset
   conversion. Owns Outcome/Feedback and narrowly the ML dataset adapter;
   never wires training or activation.
5. `CORE-P1-PIPELINE-LIFECYCLE` — sole owner of `server/pipeline.rs`,
   `server/mod.rs`, `stats.rs`, `ir/response.rs`, and the minimal accepted
   session seam. It consumes the four domain contracts and owns production
   terminal lifecycle wiring.

`server/pipeline.rs` has exactly one implementation owner. No two workers may
edit it concurrently. Existing broad test files and all accepted
`ml/shadow.rs`/identity files are read-only to Core repair workers; focused new
test files are preferred.

## Dispatch order

- Batch A, parallel: `CORE-P1-MEDIA-REQ` and `CORE-P1-FAILURE-AUTHORITY`.
- Batch B, parallel after Batch A acceptance: `CORE-P1-ELIGIBILITY-TRACE` and
  `CORE-P1-OUTCOME-FEEDBACK`.
- Batch C, serial after Batch B acceptance: `CORE-P1-PIPELINE-LIFECYCLE`.
- Revalidate `CORE-P1-REPAIR` and the Stage 2/3/4/6 records only after the
  integration node is accepted.
- Dispatch full `7E-1B` only after the Core P1 aggregate is accepted.
- Keep `7E-2A` blocked; no DecisionModel, DecisionDistribution, warmup, RL,
  calibration, activation, or durable journal work is authorized here.

## Worker rules

Every implementation worker must first report implementation state, actual
gap, files, dependency risks, and plan. Workers must not edit status/DAG/
evidence files, push, merge, rebase, or touch the main worktree. A worker may
return `DONE`, `PARTIAL`, `BLOCKED`, or `FAILED`; only the parent may accept a
node after independent diff and gate review.

## Related records

- `docs/development/node-status/core-p1-repair.status.json`
- `docs/development/node-status/core-p1-media-req.status.json`
- `docs/development/node-status/core-p1-failure-authority.status.json`
- `docs/development/node-status/core-p1-eligibility-trace.status.json`
- `docs/development/node-status/core-p1-outcome-feedback.status.json`
- `docs/development/node-status/core-p1-pipeline-lifecycle.status.json`
- `docs/development/dependency-dag.md`
- `docs/development/roadmap.md`
