# Regression Ledger

This ledger is the canonical owner/recovery view for `REG-001` through
`REG-010`. It preserves the audit's severity and planned owner. `baseline_state`
is the state at the audited repository; `current_state` is the state justified
by the evidence available to the orchestration batch. Both use the legal node
state vocabulary. ORCH-DOCS records defects; it does not repair them.

| ID | Regression | Baseline state | Current state | Severity | Owner | Evidence | Impacted nodes / unblock condition |
|---|---|---|---|---|---|---|---|
| `REG-001` | Full workspace all-target/all-feature clippy failed on the unused `existed` binding in the Tauri store. | `FAILED` | `DONE` | P2 | `BASELINE-GATE` | E-008; E-024; E-028; `src-tauri/src/store.rs` | Closed by 902db2c and revalidated at c782a49. |
| `REG-002` | Windows smoke searched an extensionless Unix binary while the Windows build emitted `zroutery-headless.exe`. | `FAILED` | `DONE` | P2 | `BASELINE-GATE` | E-010; E-030; `scripts/smoke_test.py` | Closed by f1797d2; native binary and local mock lifecycle pass. |
| `REG-003` | UI layout validation returned success after printing `skipping` when no Chromium browser was found. | `FAILED` | `DONE` | P1 | `BASELINE-GATE` | E-012; E-031; `scripts/ui_layout_test.py` | Closed by 4e36fdb; missing browser fails closed and real Chrome assertions pass. |
| `REG-004` | Commit lint encoded a stale subject/body contract and did not accept the current Node/Gate/Status trailer contract. | `FAILED` | `DONE` | P1 | `COMMIT-CONTRACT` | E-016; E-032; commit-lint workflow and WORKFLOW.md | Closed by efd8092, cf50328, 8ea7696, afba42c, and 0d3367b. |
| `REG-005` | Main policy and direct model-ID paths do not consistently enforce request-derived capabilities, and the initial planned candidate is not the final served identity. | `FAILED` | `PARTIAL` | P1 | `CORE-P1-MEDIA-REQ` / `CORE-P1-ELIGIBILITY-TRACE` | E-016; E-043; E-050; E-051; E-053; E-054; E-056; `crates/zroutery-core/src/policy.rs`; `crates/zroutery-core/src/router.rs`; `crates/zroutery-core/src/server/pipeline.rs` | `STAGE-1`, `STAGE-3`, `7E-1B`, `7E-2A`, `7E-3`; capability/media derivation and request eligibility on the direct, tier, and policy paths are accepted, but only the sole pipeline owner can prove that the final served identity is the candidate that actually served the response. |
| `REG-006` | `FailureImpact`/`FailureClass` and `Error` are competing runtime failure authorities. | `FAILED` | `PARTIAL` | P1 | `CORE-P1-FAILURE-AUTHORITY` | E-016; E-043; E-046; E-047; E-049; E-051; E-053; E-056; `crates/zroutery-core/src/failure.rs`; `crates/zroutery-core/src/error.rs` | `STAGE-4`, `7E-1B`, `7E-2E`, `7E-3`; canonical domain mapping, generic 402/412 semantics, and the router retry/fallback/health adapters are accepted, but the pipeline's own reclassification still has to be removed by the sole integration node. |
| `REG-007` | A client stream disconnect can be finalized as a successful request through the stream drop path. | `FAILED` | `FAILED` | P1 | `CORE-P1-PIPELINE-LIFECYCLE` | E-016; E-043; `crates/zroutery-core/src/server/pipeline.rs:1508-1516` | `STAGE-4`, `7E-1B`, `7E-2E`, `7E-3`; the sole pipeline owner must distinguish cancellation/interruption from success and test actual drop behavior. |
| `REG-008` | A valid model checkpoint could be paired with an unrelated commit ID. | `FAILED` | `DONE` | P1 | `7E-1A` | E-016; E-026; E-027; E-028; E-033; `ml/model_identity.rs`; `ml/shadow.rs` | Closed by 1c49109 and c782a49 with complete lineage and schema-envelope gates. |
| `REG-009` | Decision-time feature vectors and exact shadow input were not retained through production evaluation/training. | `FAILED` | `PARTIAL` | P1 | `7E-1B-CORE` / `7E-1B` | E-016; E-035; E-037; E-041; `crates/zroutery-core/src/ml/shadow.rs`; `crates/zroutery-core/tests/shadow_observation_test.rs` | `7E-1`, `7E-1B`, `7E-2A`; pure replayable input/meaningful counterfactual is accepted, but production final served identity and runtime Outcome/session integration remain blocked by Core P1 repairs. |
| `REG-010` | Node state, DAG, evidence, and regression recovery information was not present in development records. | `FAILED` | `DONE` | P1 | `ORCH-DOCS` | E-017 through E-023; `docs/development/node-status/`; `docs/development/dependency-dag.md`; `docs/development/evidence-registry.md` | `ORCH-INFRA`, `ORCH-STATUS`, `ORCH-EVIDENCE`, `ORCH-REGRESSION`, `ORCH-VALIDATION`; reopen if JSON, inventory, edge, enum, path, or diff validation fails. |

## Ownership and closure rules

- Severity is a risk classification, not a completion percentage.
- `REG-001`–`REG-004` are closed by the baseline and commit-contract batches;
  reopen them if the exact commands, toolchain, or validators change.
- `REG-005`–`REG-007` remain open Core P1 repairs, now assigned to the bounded
  nodes in ADR-0005. `REG-005` and `REG-006` are `PARTIAL` because the domain
  contracts, request eligibility, and the router failure adapters are accepted;
  only the sole pipeline owner can close final served identity and remove the
  pipeline's own reclassification. `REG-007` is untouched by Batch B.
- `REG-008` is closed by 7E-1A; `REG-009` is PARTIAL after 7E-1B-CORE and
  remains open until full 7E-1B production integration is proven.
- `REG-010` is closed only for the documentation artifact itself. It does not
  imply that the implementation, test, or online-learning gates are closed.

## Reopen triggers

Reopen a ledger row when the exact implementation revision changes without a
new evidence record, when a previously passing local command changes scope,
when a new consumer is added to a contract boundary, or when a node status
changes without a corresponding evidence ID. A new audit SHA creates a new
provenance record; it does not silently rewrite this ledger.
