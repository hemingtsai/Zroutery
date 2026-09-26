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
| `REG-005` | Main policy and direct model-ID paths do not consistently enforce request-derived capabilities, and the initial planned candidate is not the final served identity. | `FAILED` | `DONE` | P1 | `CORE-P1-MEDIA-REQ` / `CORE-P1-ELIGIBILITY-TRACE` | E-016; E-043; E-050; E-051; E-053; E-054; E-056; E-059; E-062; `crates/zroutery-core/src/policy.rs`; `crates/zroutery-core/src/router.rs`; `crates/zroutery-core/src/server/pipeline.rs` | Closed by the Batch A/B domain contracts and c4188be: the derived capability vector is enforced on the direct, tier, and policy paths, and the served identity is recorded on exactly one validated Outcome per request. |
| `REG-006` | `FailureImpact`/`FailureClass` and `Error` are competing runtime failure authorities. | `FAILED` | `DONE` | P1 | `CORE-P1-FAILURE-AUTHORITY` | E-016; E-043; E-046; E-047; E-049; E-051; E-053; E-056; E-058; E-059; E-060; `crates/zroutery-core/src/failure.rs`; `crates/zroutery-core/src/error.rs` | Closed by 4048a61, the Batch B router adapters, and c4188be: one canonical mapping, no local classifier in the pipeline, and a structural tripwire that fails if a second one returns. |
| `REG-007` | A client stream disconnect can be finalized as a successful request through the stream drop path. | `FAILED` | `DONE` | P1 | `CORE-P1-PIPELINE-LIFECYCLE` | E-016; E-043; E-058; E-059; E-060; `crates/zroutery-core/src/server/pipeline.rs:1508-1516`; `crates/zroutery-core/tests/pipeline_lifecycle_test.rs` | Closed by c4188be: a drop is Interrupted or Cancelled, never served, and a behavioral test drops a real response mid-answer to prove it. |
| `REG-008` | A valid model checkpoint could be paired with an unrelated commit ID. | `FAILED` | `DONE` | P1 | `7E-1A` | E-016; E-026; E-027; E-028; E-033; `ml/model_identity.rs`; `ml/shadow.rs` | Closed by 1c49109 and c782a49 with complete lineage and schema-envelope gates. |
| `REG-009` | Decision-time feature vectors and exact shadow input were not retained through production evaluation/training. | `FAILED` | `PARTIAL` | P1 | `7E-1B-CORE` / `7E-1B` | E-016; E-035; E-037; E-041; E-062; E-067; E-068; `crates/zroutery-core/src/ml/shadow.rs`; `crates/zroutery-core/tests/shadow_integration_test.rs` | `7E-2A`, `7E-2B`; retention through production evaluation is closed — the stored record holds the exact decision-time input and a test replays it through a fresh engine to reproduce both checksums — but the training-side consumer does not exist yet, so reopen this row when 7E-2B adds one. |
| `REG-010` | Node state, DAG, evidence, and regression recovery information was not present in development records. | `FAILED` | `DONE` | P1 | `ORCH-DOCS` | E-017 through E-023; `docs/development/node-status/`; `docs/development/dependency-dag.md`; `docs/development/evidence-registry.md` | `ORCH-INFRA`, `ORCH-STATUS`, `ORCH-EVIDENCE`, `ORCH-REGRESSION`, `ORCH-VALIDATION`; reopen if JSON, inventory, edge, enum, path, or diff validation fails. |

## Ownership and closure rules

- Severity is a risk classification, not a completion percentage.
- `REG-001`–`REG-004` are closed by the baseline and commit-contract batches;
  reopen them if the exact commands, toolchain, or validators change.
- `REG-005`–`REG-007` were the Core P1 repairs assigned by ADR-0005 and are all
  closed by c4188be and the revalidation in E-062. Reopen any of them if a
  later change reintroduces a second failure classifier, loses the planned versus
  served distinction, or records a client drop as success.
- `REG-008` is closed by 7E-1A; `REG-009` is `PARTIAL` after 7E-1B because the
  evaluation side is proven and the training side does not exist yet.
- `REG-010` is closed only for the documentation artifact itself. It does not
  imply that the implementation, test, or online-learning gates are closed.

## Reopen triggers

Reopen a ledger row when the exact implementation revision changes without a
new evidence record, when a previously passing local command changes scope,
when a new consumer is added to a contract boundary, or when a node status
changes without a corresponding evidence ID. A new audit SHA creates a new
provenance record; it does not silently rewrite this ledger.
