# Accounting Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-21 · Verified by: `cargo test --workspace --all-features --locked`

This page describes usage, cost, settlement and budgets. It is the reference for
the invariants in [architecture.md](../architecture.md); it does not repeat the
budget UI.

## Usage

`Usage` is one accounting tuple per attempt
(`crates/zroutery-core/src/ir/mod.rs::Usage`):

| Field | Meaning |
|---|---|
| `input_tokens` | total prompt tokens: fresh + cache reads + cache writes |
| `output_tokens` | completion tokens, including reasoning tokens |
| `cache_read_tokens` | prompt tokens served from cache; a subset of input |
| `cache_write_tokens` | prompt tokens written to cache; a subset of input |
| `reasoning_tokens` | reasoning tokens, already inside output |

Fresh prompt tokens are `input_tokens - cache_read_tokens -
cache_write_tokens`, saturating at zero
(`crates/zroutery-core/src/ir/mod.rs::Usage::fresh_input_tokens`). A relay that
reports a cache count larger than its prompt total therefore cannot produce a
negative count.

## Cost

A price is per million tokens, in the provider's own currency, with an optional
per-request tariff (`crates/zroutery-core/src/billing.rs::Pricing`):

- fresh input is billed at `input_per_mtok`;
- cache reads and writes are billed at their own prices when set, otherwise at
  the input price (the conservative fallback);
- output is billed at `output_per_mtok`;
- `per_request` replaces the token arithmetic for the whole call
  (`crates/zroutery-core/src/billing.rs::Pricing::cost_of`).

Messages multi-inference `usage.iterations` is strictly parsed and replaces,
not adds to, the overlapping top-level tuple; the replacement itself happens
in the attempt's observation capture, and a later shorter or weaker iteration
set degrades the reading to conflicting evidence rather than rescuing the
total; cache TTL parts remain subsets of input
(`crates/zroutery-core/src/protocol/anthropic/usage.rs::decode_iterations`,
`crates/zroutery-core/src/ir/anthropic_usage.rs::UsageIteration`,
`crates/zroutery-core/src/ir/observation.rs::AttemptObservation::observe_iterations`,
`crates/zroutery-core/src/upstream/observation.rs::capture`). Per-model
quotes are frozen before sending, with optional 5-minute/1-hour cache-write
tariffs. Unknown model rates and uncertain refusal billing stay unknown;
known currency subtotals remain distinct even when the legacy single cost is
absent (`crates/zroutery-core/src/accounting.rs::AttemptFact::bind_iteration_pricing`,
`crates/zroutery-core/src/accounting.rs::AttemptFact::observe_response`,
`crates/zroutery-core/src/accounting.rs::AttemptFact::known_costs`,
`crates/zroutery-core/src/billing.rs::Pricing::cost_of_iteration`).

Costs are never converted between currencies. `CostTotals` keeps one total per
currency (`crates/zroutery-core/src/billing.rs::CostTotals`), which is why a
budget in a currency no model bills in can never be reached.

An unknown price is not a zero price: the attempt is counted as unknown
(`crates/zroutery-core/src/accounting.rs::RequestAccounting`,
`unknown_cost_attempts`). The same applies to usage that never arrived.

## Settlement

A request reaches exactly one terminal state and is charged once
(`crates/zroutery-core/src/server/pipeline/lifecycle.rs::TerminalKind`).

- `RequestAccounting` aggregates the attempts, separating known cost from
  unknown-cost attempts and observed usage from unknown-usage attempts
  (`crates/zroutery-core/src/accounting.rs::RequestAccounting`).
- `SettlementFact` is the durable record; its `safe` projection removes the error
  and routing detail before persistence
  (`crates/zroutery-core/src/accounting.rs::SettlementFact`).
- A failed attempt's known cost remains recorded. A cancellation or client
  disconnect is not a success and is settled through the same path
  (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::TerminalKind`).
- An auxiliary call made on a request's behalf (a vision description) is charged
  to the provider and tier that answered it; it is not folded into the main
  request's settlement. It settles through the same durable attempt path as any
  other attempt
  (`crates/zroutery-core/src/server/pipeline/auxiliary.rs::AuxiliaryAttempt`,
  `crates/zroutery-core/src/server/accounting.rs::persist_attempt`).
- An attempt's `ExecutionSnapshot` carries `plugin_origin` (installation,
  contribution, digest, grant epoch, transport profile); it is durable and
  survives the `safe` projection, and plugin-served traffic settles on the same
  path (`crates/zroutery-core/src/accounting.rs::ExecutionSnapshot`).

Gateway server review is a separate `auto_mode` request lifecycle. Every real
classifier send has its own attempt and parent request ID; its measured usage
and price never overwrite generation usage or native rate-limit headers
(`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::send_review`,
`crates/zroutery-core/src/accounting.rs::AttemptFact`). Multi-currency iteration
costs enter SQLite and memory ledgers atomically under one attempt ID, so local
settlement retries do not charge them twice
(`crates/zroutery-core/src/runtime_store/accounting.rs::write_attempt`,
`crates/zroutery-core/src/server/accounting.rs::AppState::persist_attempt`).

## Budgets

A budget is a limit plus a period and a scope
(`crates/zroutery-core/src/budget.rs::Budget`):

- scopes: global, one provider, one tier;
- periods: local day or local month
  (`crates/zroutery-core/src/budget.rs::BudgetPeriod`);
- over-limit action: reject or degrade to a cheaper tier
  (`crates/zroutery-core/src/budget.rs::OnExceeded`).

Admission is a line-crossing detector, not a pre-authorisation. The request takes
one permit for every budgeted scope it could spend against, in canonical order,
before the ledger check; the permit is held until the terminal settlement
(`crates/zroutery-core/src/server/state.rs::AdmissionGuard`). At most one request
per scope is therefore in flight across the line, and the next request reads a
ledger that already includes the crossing spend.

The check itself is `crates/zroutery-core/src/budget.rs::check`;
`crates/zroutery-core/src/budget.rs::Ledger` is the value type the check reads.
The durable accumulator is the runtime store's `accounting_ledger` table:
attempts and settlements commit there
(`crates/zroutery-core/src/runtime_store/accounting.rs::record_attempt`,
`crates/zroutery-core/src/runtime_store/accounting.rs::settle`), and the server
publishes the committed ledger to the in-process one
(`crates/zroutery-core/src/server/accounting.rs::publish_accounting_ledger`).
The settlement transaction also folds the settled usage row into the runtime
store's durable usage summary — replacing the contribution of any provisional
row it superseded — so the aggregate a dashboard reads is updated by the same
commit that made it
(`crates/zroutery-core/src/runtime_store.rs::RuntimeStore::usage_snapshot`).
The desktop writes the spend projection through `flush_ledger` and `save_ledger`
(`src-tauri/src/state/budget.rs::flush_ledger`,
`src-tauri/src/store.rs::save_ledger`).

The classifier pool is the one scope exception: a tier budget neither rejects nor
degrades a side request, because the pool is chosen by classifier configuration
rather than tier policy. Global and provider limits still apply
(`crates/zroutery-core/src/server/ledger.rs::classifier_budget_verdict`).

Embedded review acquires the union of generator and reviewer scopes once in
canonical order, avoiding recursive acquisition of the same budget permit. Its
budget check includes the generator's pending known cost without charging that
cost twice (`crates/zroutery-core/src/server/pipeline/budget.rs::apply_budgets_with_scopes`,
`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::budget_allows`).

## Failure modes

| Symptom | Meaning | Where |
|---|---|---|
| `unknown_cost_attempts > 0` | the price or the final usage was missing | `RequestAccounting` |
| `unpriced_requests > 0` | the model has no pricing | activity record |
| a currency total is missing | no cost was recorded in that currency | `CostTotals` |
| a budget never triggers | its currency is not billed, or its scope has no traffic | `crates/zroutery-core/src/budget.rs::Budget::problems` |
| overshoot larger than one request | admission was bypassed | `AdmissionGuard` |

## Not implemented

- No pre-authorisation of a request's maximum cost; the limit is checked against
  spend already settled.
- No currency conversion and no cross-currency sum.
- A failed provider call that bills without returning usage has its spend
  unattributed; only the known cost of an attempt is recorded.

## Sources

- `crates/zroutery-core/src/ir/mod.rs::Usage`
- `crates/zroutery-core/src/billing.rs::Pricing::cost_of`
- `crates/zroutery-core/src/billing.rs::CostTotals`
- `crates/zroutery-core/src/accounting.rs::RequestAccounting`
- `crates/zroutery-core/src/accounting.rs::SettlementFact`
- `crates/zroutery-core/src/server/pipeline/lifecycle.rs::TerminalKind`
- `crates/zroutery-core/src/budget.rs::Budget`
- `crates/zroutery-core/src/budget.rs::check`
- `crates/zroutery-core/src/budget.rs::Ledger`
- `crates/zroutery-core/src/runtime_store/accounting.rs::record_attempt`
- `crates/zroutery-core/src/runtime_store/accounting.rs::settle`
- `crates/zroutery-core/src/runtime_store.rs::RuntimeStore::usage_snapshot`
- `crates/zroutery-core/src/server/accounting.rs::publish_accounting_ledger`
- `crates/zroutery-core/src/server/pipeline/auxiliary.rs`
- `crates/zroutery-core/src/server/state.rs::AdmissionGuard`
- `crates/zroutery-core/src/server/ledger.rs::classifier_budget_verdict`
- `src-tauri/src/state/budget.rs::flush_ledger`
- `src-tauri/src/store.rs::save_ledger`
- `crates/zroutery-core/src/runtime_store.rs::RuntimeStore`
- `crates/zroutery-core/src/accounting.rs::ExecutionSnapshot`
