# Regression Ledger

This ledger is the canonical owner/recovery view for `REG-001` through
`REG-010`. It preserves the audit's severity and planned owner. `baseline_state`
is the state at the audited repository; `current_state` is the state justified
by the evidence available to the orchestration batch. Both use the legal node
state vocabulary. ORCH-DOCS records defects; it does not repair them.

| ID | Regression | Baseline state | Current state | Severity | Owner | Evidence | Impacted nodes / unblock condition |
|---|---|---|---|---|---|---|---|
| `REG-001` | Full workspace all-target/all-feature clippy fails on the unused `existed` binding in the Tauri store. | `FAILED` | `FAILED` | P2 | `BASELINE-GATE` | E-008; `src-tauri/src/store.rs:106` | `TEST-CLIPPY`, `BASELINE-GATE`; repair the binding and rerun the full workspace clippy gate. |
| `REG-002` | Windows smoke searches an extensionless Unix binary while the Windows build emits `zroutery-headless.exe`. | `FAILED` | `FAILED` | P2 | `BASELINE-GATE` | E-010; `scripts/smoke_test.py:357-361` | `TEST-SMOKE`, `BASELINE-GATE`; resolve the platform-specific binary and run the local mock-provider lifecycle. |
| `REG-003` | UI layout validation returns success after printing `skipping` when no Chromium browser is found. | `FAILED` | `FAILED` | P1 | `BASELINE-GATE` | E-012; `scripts/ui_layout_test.py:496-501` | `UI-LAYOUT`, `TEST-LAYOUT-BROWSER`, `BASELINE-GATE`; missing browser must be an explicit non-success and a present browser must execute assertions. |
| `REG-004` | Commit lint encodes a stale subject/body contract and does not accept the current Node/Gate/Status trailer contract. | `FAILED` | `READY` | P1 | `COMMIT-CONTRACT` | E-016; `.github/workflows/commit-lint.yml:25-33`; `docs/development/WORKFLOW.md:21-36` | `COMMIT-CONTRACT`, `TEST-COMMIT-CONTRACT`; align deterministic fixtures and workflow rules without rewriting history. |
| `REG-005` | Main policy and direct model-ID paths do not consistently enforce request-derived capabilities, and the initial planned candidate is not the final served identity. | `FAILED` | `FAILED` | P1 | Future Core repair node (`CORE-P1-REPAIR`) | E-016; `crates/zroutery-core/src/policy.rs`; `crates/zroutery-core/src/router.rs`; `crates/zroutery-core/src/server/pipeline.rs` | `STAGE-1`, `STAGE-3`, `7E-1B`, `7E-2A`, `7E-3`; enforce eligibility on every resolution path and record initial/final identities separately. |
| `REG-006` | `FailureImpact`/`FailureClass` and `Error` are competing runtime failure authorities. | `FAILED` | `FAILED` | P1 | Future Core repair node (`CORE-P1-REPAIR`) | E-016; `crates/zroutery-core/src/failure.rs`; `crates/zroutery-core/src/error.rs` | `STAGE-4`, `7E-1B`, `7E-2E`, `7E-3`; choose one classified authority and prove consistent observation/circuit effects. |
| `REG-007` | A client stream disconnect can be finalized as a successful request through the stream drop path. | `FAILED` | `FAILED` | P1 | Future Stage 4 repair node (`CORE-P1-REPAIR`) | E-016; `crates/zroutery-core/src/server/pipeline.rs:1508-1516` | `STAGE-4`, `7E-1B`, `7E-2E`, `7E-3`; distinguish cancellation/interruption from success and test the drop path. |
| `REG-008` | A valid model checkpoint can be paired with an unrelated commit ID. | `FAILED` | `READY` | P1 | `7E-1A` | E-016; `crates/zroutery-core/src/ml/shadow.rs:361-372`; `crates/zroutery-core/src/ml/model_identity.rs` | `7E-0`, `7E-1A`; reject mismatches, retain verified parent/checkpoint lineage, and rerun identity/replay gates. |
| `REG-009` | Decision-time feature vectors and exact shadow input are not retained through production evaluation/training. | `FAILED` | `READY` | P1 | `7E-1B` | E-016; `crates/zroutery-core/src/ml/shadow.rs:220-325`; `crates/zroutery-core/src/ml/shadow.rs:598-813` | `7E-1`, `7E-1B`, `7E-2A`; retain immutable inputs, meaningful counterfactuals, and separate final served identity. |
| `REG-010` | Node state, DAG, evidence, and regression recovery information was not present in development records. | `FAILED` | `DONE` | P1 | `ORCH-DOCS` | E-017 through E-023; `docs/development/node-status/`; `docs/development/dependency-dag.md`; `docs/development/evidence-registry.md` | `ORCH-INFRA`, `ORCH-STATUS`, `ORCH-EVIDENCE`, `ORCH-REGRESSION`, `ORCH-VALIDATION`; reopen if JSON, inventory, edge, enum, path, or diff validation fails. |

## Ownership and closure rules

- Severity is a risk classification, not a completion percentage.
- `REG-001`–`REG-003` are executable baseline-gate repairs and remain open
  until their local commands pass.
- `REG-004` is ready for the separate commit-contract node; ORCH-DOCS does
  not edit the workflow or commit-lint files.
- `REG-005`–`REG-007` are Core P1 repairs and block trustworthy learning
  evidence. They are not fixed by adding status records.
- `REG-008` and `REG-009` are assigned to the bounded ML repair path and must
  not be closed by historical tags.
- `REG-010` is closed only for the documentation artifact itself. It does not
  imply that the implementation, test, or online-learning gates are closed.

## Reopen triggers

Reopen a ledger row when the exact implementation revision changes without a
new evidence record, when a previously passing local command changes scope,
when a new consumer is added to a contract boundary, or when a node status
changes without a corresponding evidence ID. A new audit SHA creates a new
provenance record; it does not silently rewrite this ledger.
