# Architecture

> Authority: normative · Owner: backend · Last verified: 2026-11-21 · Verified by: `scripts/docs_check.py`

This document describes the current boundaries and the invariants a change must
preserve. It does not track pass/fail status (`current-state.md`), command
definitions (`verification.md`), or history (`archive/`); findings follow the
[canonical policy](../DOCUMENTATION.md#findings-and-decisions).

## 1. Layers

| Layer | Location | Owns | Does not own |
|---|---|---|---|
| Core | `crates/zroutery-core/` | protocol translation, registry, routing, accounting, storage, optional ML | the desktop shell, native credentials, windows or menus |
| Desktop shell | `src-tauri/` | lifecycle, native integrations, credential store, IPC commands | protocol translation or routing decisions |
| Headless | `crates/zroutery-headless/` | the same Core and desktop state without a GUI | a second implementation of the pipeline |
| UI | `ui/` | presentation and the typed IPC contract | configuration authority or accounting |
| Plugin runtime | `crates/zroutery-plugin-api/`, `crates/zroutery-plugin-host/`, `crates/zroutery-plugin-worker/` | declarative contracts, host grants and lifecycle, the restricted worker process | the request pipeline, credentials, or a second Core |

Core is GUI-free: its manifest has no Tauri dependency
(`crates/zroutery-core/Cargo.toml::dependencies`). The desktop crate composes it
and enables the product's capability set through named features
(`src-tauri/Cargo.toml::default`). The headless binary wires the same desktop
state and then serves (`crates/zroutery-headless/src/main.rs::main`).

Dependency majors stay with the crate that calls them: Core's weighted attempt
shuffle uses the `rand` API (`crates/zroutery-core/src/router/selection.rs::weighted_shuffle`,
`crates/zroutery-core/Cargo.toml::dependencies`) and the plugin host's archive
reader uses the `zip` API (`crates/zroutery-plugin-host/Cargo.toml::dependencies`).

The module boundary is declared once in
`crates/zroutery-core/src/lib.rs::pub mod`; a new top-level module is an
architecture change, not a private refactor.

The plugin runtime stays outside the request path. `plugin-api` is declarative
only (`crates/zroutery-plugin-api/src/lib.rs`); `plugin-host` owns grants and
lifecycle (`crates/zroutery-plugin-host/src/lib.rs`); the worker is a separate
restricted process (`crates/zroutery-plugin-worker/src/main.rs::main`). The Core
bridge maps verified declarations onto the existing codecs, `Upstream` and
settlement (`crates/zroutery-core/src/plugin/mod.rs`), and the shell is the
broker (`src-tauri/src/plugins/mod.rs`).

## 2. Request path

```text
ingress handler
  -> protocol::decode_request        (wire dialect -> ChatRequest)
  -> server::pipeline::handle_chat   (identity, policy, budget, plan)
  -> Router::plan                    (eligibility, health, failover order)
  -> Upstream::send / stream         (one provider attempt)
  -> protocol encoder                (IR -> client dialect)
  -> RequestLifecycle                (one terminal settlement)
```

- The route table is built in `crates/zroutery-core/src/server/handlers.rs::build_app`;
  each dialect has its own entry point but shares the pipeline.
- Decoding is dialect-specific and returns the canonical request
  (`crates/zroutery-core/src/protocol/mod.rs::decode_request`,
  `crates/zroutery-core/src/ir/mod.rs::ChatRequest`).
- Orchestration lives in
  `crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`.
- Candidate selection is the router's responsibility
  (`crates/zroutery-core/src/router/mod.rs::Router`); policy can narrow the plan
  before the router orders it (`crates/zroutery-core/src/policy/policy.rs::RoutingPolicy`).
- Transport is one `Upstream` per provider policy
  (`crates/zroutery-core/src/upstream/mod.rs::Upstream`).
- Client identity is resolved once per attempt, from the operator's configured
  profile rather than an already-resolved one
  (`crates/zroutery-core/src/server/pipeline/client.rs::client_for`). An `Auto`
  provider presents a caller it has a fingerprint for as that profile, and a
  caller it has none for as itself: its own `user-agent` and `x-opencode-*`
  headers travel instead of the transport's `zroutery/<version>`
  (`crates/zroutery-core/src/config/provider.rs::AUTO_IDENTITY_HEADERS`,
  `crates/zroutery-core/src/config/provider.rs::resolve_client_identity`), which
  the resolver and wire tests assert
  (`crates/zroutery-core/tests/client_profile_resolution_test.rs::auto_forwards_the_opencode_headers_a_custom_provider_still_carries`,
  `crates/zroutery-core/src/upstream/client_tests.rs::auto_forwards_the_callers_own_ua_to_the_actual_wire`).
  A pinned profile never forwards the caller's user agent, so one request cannot
  carry two identities
  (`crates/zroutery-core/src/config/provider.rs::ProviderClientProfile::passthrough_headers`).

## 3. Invariants

These are the properties a change must not break. Each is enforced by code or a
test; the evidence table below marks the two that are source-only.

1. **Eligibility is authoritative.** A candidate rejected by a capability or
   policy requirement is not reintroduced by a direct model id. The pipeline
   derives required capabilities before planning
   (`crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`).
2. **Planned and served identity are separate.** A request records the candidate
   it planned and the candidate that actually answered after failover; one is
   never substituted for the other
   (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`).
3. **One terminal settlement.** A request reaches exactly one terminal state and
   is charged once. The lifecycle owns the transition
   (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`);
   the accounting fact is durable
   (`crates/zroutery-core/src/accounting.rs::SettlementFact`).
4. **Cancellation is not success.** A client disconnect or a cancel is
   `Interrupted`/`Cancelled`, never `Served`
   (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::TerminalKind`).
5. **Unknown is not zero.** Unknown usage, price or balance stays unknown; it is
   not recorded as a measured zero
   (`crates/zroutery-core/src/accounting.rs::RequestAccounting`).
6. **Token subsets are respected.** Cache read/write tokens are subsets of input;
   reasoning tokens are inside output. Costing reads the subsets rather than
   adding them again (`crates/zroutery-core/src/billing.rs::Pricing::cost_of`,
   `crates/zroutery-core/src/ir/mod.rs::Usage`).
7. **Currencies do not mix.** Totals keep currencies separate; there is no
   implicit conversion (`crates/zroutery-core/src/billing.rs::CostTotals`).
8. **Budgets are admission plus a ledger.** Admission serialises the requests
   that could cross a limit, and the ledger is the durable record
   (`crates/zroutery-core/src/server/state.rs::AdmissionGuard`,
   `crates/zroutery-core/src/budget.rs::Ledger`,
   `crates/zroutery-core/src/server/ledger.rs::charge`).
9. **Failure has one authority.** Runtime classification comes from
   `FailureClass`/`FailureImpact`, and the wire error is derived from the same
   canonical failure (`crates/zroutery-core/src/failure.rs::ClassifiedFailure`,
   `crates/zroutery-core/src/error.rs::safe_message`).
10. **Persisted errors are metadata, not upstream bodies.** Ordinary wire errors
    carry the safe provider/status message. Native Messages errors may be
    relayed request-locally for client capability recovery, but are never added
    to history or training (`crates/zroutery-core/src/error.rs::safe_message`,
    `crates/zroutery-core/src/ir/headers.rs::ReplyHeaders::capture_native_error`,
    `crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`).
11. **Redirects are off.** An upstream redirect cannot carry the provider key to
    another host (`crates/zroutery-core/src/upstream/http.rs::configure`).
12. **Auth is constant-time and local.** The gateway token is compared without
    early exit; `/health` is the only unauthenticated route
    (`crates/zroutery-core/src/server/handlers.rs::auth_layer`,
    `crates/zroutery-core/src/server/handlers.rs::constant_time_eq`).
13. **Secrets live in the credential store.** Configuration carries a `key_ref`,
    not a key (`crates/zroutery-core/src/config/registry.rs::ProviderConfig`,
    `src-tauri/src/secrets.rs::KeychainSecrets`).
14. **A local persistence failure does not replay an upstream action.** A failed
    local write is a terminal failure for that request, not a reason to re-send a
    request the provider already executed
    (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::RequestLifecycle`).
15. **Runtime health is process-local; spend is durable.** Health, latency rings
    and breaker counters start empty on restart; the spend ledger is adopted
    from disk, and the usage projection adopts the durable summary row rather
    than re-reading the whole usage table
    (`crates/zroutery-core/src/budget.rs::Ledger`,
    `crates/zroutery-core/src/runtime_store.rs::RuntimeStore`,
    `crates/zroutery-core/src/runtime_store.rs::RuntimeStore::usage_snapshot`).

### Invariant evidence

A source anchor proves the boundary exists; a test anchor is the executable
evidence. `untested` is an explicit gap, not a pass.

| # | Invariant | Test evidence |
|---|---|---|
| 1 | Eligibility is authoritative | `crates/zroutery-core/tests/eligibility_trace_test.rs::direct_and_tier_resolution_have_request_capability_parity` |
| 2 | Planned and served identity are separate | `crates/zroutery-core/tests/decision_contract_test.rs::planned_attempted_and_served_stay_three_separate_facts` |
| 3 | One terminal settlement | `crates/zroutery-core/tests/pipeline_lifecycle_test/identity_and_classification.rs::every_request_reaches_exactly_one_terminal_transition` |
| 4 | Cancellation is not success | `crates/zroutery-core/tests/failure_authority_test.rs::cancellation_and_interruption_are_terminal_non_successes` |
| 5 | Unknown is not zero | `crates/zroutery-core/tests/attempt_observation_test.rs::an_all_zero_usage_is_never_a_measured_zero` |
| 6 | Token subsets are respected | `crates/zroutery-core/tests/gemini_test.rs::cached_content_is_a_subset_of_the_gemini_prompt_total` |
| 7 | Currencies do not mix | `crates/zroutery-core/src/budget.rs::currencies_are_counted_separately_and_never_added` |
| 8 | Budgets are admission plus a ledger | `crates/zroutery-core/tests/e2e/budget.rs::concurrent_requests_cannot_all_pass_the_same_budget` |
| 9 | Failure has one authority | `crates/zroutery-core/tests/failure_authority_test.rs::one_impact_table_covers_routing_health_stats_and_fault_effects` |
| 10 | Persisted errors are metadata, not upstream bodies | `crates/zroutery-core/tests/durable_accounting_test.rs::failed_attempt_known_cost_survives_and_late_finalization_cannot_rebind_it` |
| 11 | Redirects are off | `crates/zroutery-core/src/upstream/client_tests.rs::a_redirect_is_answered_as_a_refusal_not_followed` |
| 12 | Auth is constant-time and local | `crates/zroutery-core/src/server/tests.rs::token_comparison_is_length_safe_and_exact` |
| 13 | Secrets live in the credential store | untested — config shape is enforced; no test asserts no plaintext secret |
| 14 | A local persistence failure does not replay an upstream action | `crates/zroutery-core/tests/durable_accounting_test.rs::required_storage_failure_never_silently_starts_volatile_serving` |
| 15 | Runtime health is process-local; spend is durable | `crates/zroutery-core/tests/e2e/budget.rs::spend_survives_a_restart` |

## 4. Desktop surface

Claude Code server review is a request-local pipeline layer, not a generation
candidate or a separate execution engine. It preserves native results and uses
the configured purpose pool only for missing results, with actual per-tool
verdicts and separate accounting
(`crates/zroutery-core/src/server/pipeline/safeguards/mod.rs::ReviewContext`,
`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::send_review`).

The desktop shell is not a second Core. It owns five things:

- **Lifecycle.** The proxy runs while the window may be hidden; closing the
  window does not stop it (`src-tauri/src/lib.rs::run`).
- **Credentials.** API keys go to the platform credential store; the
  configuration holds only the reference
  (`src-tauri/src/secrets.rs::KeychainSecrets`).
- **Configuration publication.** A save is validated, written atomically, and
  applied under a serialized mutation path; a failed rebind restores the previous
  document (`src-tauri/src/state/server.rs::apply_config_locked`).
- **IPC.** Commands are registered in the handler and declared in the app
  manifest; the webview may call only what the capabilities grant
  (`src-tauri/src/lib.rs::generate_handler`,
  `src-tauri/build.rs::APP_COMMANDS`,
  `src-tauri/capabilities/default.json::permissions`).
- **Plugin broker.** The shell installs, activates, disables and rolls back
  plugin packages, keeps their credentials scoped, and exposes safe appearance
  as a recovery path (`src-tauri/src/plugins/mod.rs`).

The webview is treated as a local administrator, not as a public client: the
content security policy is restrictive
(`src-tauri/tauri.conf.json::security`), and the command surface is compared
against the frontend contract by `scripts/command_surface_test.py`.

## 5. Optional subsystems

Feature gates are the switchboard, not an afterthought. A capability that is
compiled but not requested stays inert.

- **ML routing** (`ml`). The learned model ranks an already-eligible plan; it
  cannot widen eligibility. A promoted model is attached in-process and can be
  rolled back (`crates/zroutery-core/src/ml/serving/router.rs::MlRouter`). Durable
  traces are an evidence log, not a second source of truth
  (`crates/zroutery-core/src/ml/traces.rs::TraceLog`).
- **Accounts** (`account`, `newapi`). Panel declarations and runtime observations
  are separate; an account's own credential is a `key_ref`, not a stored secret
  (`crates/zroutery-core/src/config/registry.rs::AccountConfig`,
  `crates/zroutery-core/src/account/store.rs::AccountStore`).
- **Migration** (`migration`). The state machine and file mechanics are separate
  from the desktop surface that drives them
  (`crates/zroutery-core/src/migration/state.rs::MigrationState`).
- **Client takeover** (`agent_takeover`). Ownership is durable, and a release
  applies the computed conflict resolution
  (`crates/zroutery-core/src/agent_takeover/ownership.rs::OwnershipManifest`).
- **Response retention** (`ir`). The Responses lifecycle store is bounded and
  in-memory (`crates/zroutery-core/src/ir/response.rs::ResponseStore`).
- **Plugins** (`plugin`). Declarations are data: a verified binding maps onto the
  existing provider and transport paths, a package is content-digest addressed,
  and the worker runs sandboxed and separate. Capability grants, the OS sandbox
  and safe mode are the boundary (`crates/zroutery-plugin-host/src/lib.rs`,
  `src-tauri/src/plugins/mod.rs`).

## 6. What this document is not

- It is not a status report: see `current-state.md`.
- It is not a gate list: see `verification.md`.
- It is not a history of decisions: see `decisions/` and `docs/archive/`.
- It is not a place for audits or plans: findings are reported to the user under
  the [findings policy](../DOCUMENTATION.md#findings-and-decisions), and accepted
  decisions become ADRs.

## 7. See also

- [Verification](verification.md)
- [Current state](current-state.md)
- [Documentation standard](../DOCUMENTATION.md)
- [Decision records](decisions/)
