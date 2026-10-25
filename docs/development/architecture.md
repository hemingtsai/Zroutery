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
`crates/zroutery-core/Cargo.toml`: every capability in that crate is declared
there and named nowhere else, which is what keeps the core's switches
independently declared and the activation guard untouched.

The desktop package is the other half of that picture, and the two are easy to
confuse. `src-tauri/Cargo.toml` declares
`default = ["ml", "account", "newapi", "account-maint", "opencode"]`, so the
shipped application contains the learning stack, the account subsystem, the
NewAPI panel adapter and the OpenCode provider kinds — while the dependency
line on `zroutery-core` still names no features at all. The capability is in
the binary because the dashboard offers it; a build that compiles an interface
it cannot serve tells the operator the thing works and then does not do it.

Carrying a capability is not the same as exercising it, and that distinction is
the one the desktop's defaults actually rest on. `ml_routing.enabled` still
defaults to `false`, so an installation that has never promoted a model routes
exactly as it did before the learning stack was compiled in. Nothing probes an
account until an account is declared; nothing asks a panel until one exists and
the button is pressed; no browser opens until a check-in is started; the
OpenCode kinds only answer when one is configured. `--no-default-features`
still builds a desktop app with none of it, so "ship without the account
subsystem" and "ship without the learning stack" remain builds rather than
deletions.

### Desktop security surface

Two things decide what the dashboard's webview may reach, and neither of them is
visible from the page that calls it.

**Capabilities.** Tauri v2's ACL gates *plugin* commands by default. An
application's own `#[tauri::command]`s are not gated at all until the
application declares an app manifest — without one, every registered command is
served from any local origin. `src-tauri/build.rs` now declares one with the
full `generate_handler!` list, so each command becomes an `allow-<command>`
permission and `src-tauri/capabilities/default.json` grants them one at a time.
The grant list and the handler list are two files that nothing in Cargo
connects, so `scripts/command_surface_test.py` compares them in both directions
and fails on any difference: a registered command with no grant is a control
that quietly does nothing, and a grant for a command nobody registers is a
surface wider than the tree it claims to describe.

The plugin side of the same file was reduced to what the webview actually
reaches: `core:event:allow-listen` and `core:event:allow-unlisten` for
`@tauri-apps/api/event`, which the dashboard uses for the gateway-state and
deep-link subscriptions. Clipboard, notification, deep link and global shortcut
are all driven from Rust (`commands.rs`, `shell.rs`, `tray.rs`), where the ACL
does not apply, so granting them to the webview would only widen it, and the
window drag surface went with them because no element in the dashboard carries
a drag region. `core:default` is gone: it granted nine core plugin defaults,
including menu and tray commands the dashboard never calls.

**Content Security Policy.** `tauri.conf.json`'s `csp` names `script-src 'self'`
explicitly. It is a clarity change rather than a tightening, and saying so is
the point: `default-src 'self'` already applied to scripts, so the effective
policy is unchanged. What changes is that the script rule is now visible where a
reader looks for it, and a later edit to `default-src` cannot silently relax it.

`style-src 'self' 'unsafe-inline'` keeps `'unsafe-inline'`. The dashboard passes
inline styles (`style={{ ... }}` in `Activity.tsx`, `MlRouting.tsx`,
`Overview.tsx` and `Routing.tsx`), and whether React's CSSOM writes would pass a
strict `style-src` is a browser detail that differs by engine and by how the
write is made. The cost of being wrong here is a dashboard that renders
unstyled on a platform nobody re-tests, against a benefit that is theoretical —
`'unsafe-inline'` in `style-src` cannot execute script, because the script rule
is separate and stays `'self'`. The relaxation is therefore scoped to
declarations and stated rather than assumed. The rationale lives here rather
than beside the directive because `tauri.conf.json` is JSON, which has no
comments.

**Local persistence.** `spend.json` sits beside `config.json` and is written
through the same `create_private` path, so on unix it is created `0o600` rather
than tightened afterwards. Windows has no mode bit and `std` cannot read an ACL,
so the assertion there is the property the inherited ACL depends on: the ledger
is written inside the user's own profile (`%APPDATA%`) instead of a shared
location whose ACL every local user already passes. `store.rs` states that
asymmetry in the test that checks it.

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
- **I2 Migration:** the state machine and local file mechanics are present, and
  so is the desktop surface that drives them. Typed actions own the process
  lifecycle they start, and endpoint verification judges the answer rather than
  the socket (E-116). The cutover page builds a plan from real detected state,
  runs it behind a confirmation, and keeps the original file as a byte snapshot.
  What remains unexercised is the `Completed` path, which needs a live gateway
  to answer `/health`; everything asserted about it is asserted against
  fixtures.
- **I3 Takeover:** ownership state and temporary-adapter tests exist, release
  applies the computed conflict resolution, and the ownership manifest is
  durable across process boundaries. The takeover page previews an adoption
  with a field-level diff and refuses a stale one by fingerprint.
- **I4 Restore / Rollback:** conflict resolution is applied during release and
  restore failures are reported as errors rather than warnings, so a rollback
  that restored nothing reports `false` (E-118). The cutover page renders
  `rolled_back` and `state` as two separate claims for exactly this reason.
- **Operational UI:** provider, model, routing, activity, and settings views
  build and are integrated. This is a `DONE` node for the current operational
  surface, not evidence that account, migration, takeover, shadow, or ML has
  a product surface.
- **New-track UI / Tauri surfaces:** every engine named here now has a command
  and a page — account and check-in under Accounts, takeover under Takeover,
  the migration engine under Cutover, and the learned model under Routing. The
  node is `PARTIAL` because the cutover's completed path is still only asserted
  against fixtures, not because a surface is missing.
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

### Runtime compatibility fixes (2026-10-25)

- **Payment failures:** upstream HTTP 402 and explicit insufficient-balance
  markers use `BalanceExhausted`, distinct from local budget denial and a
  scheduled upstream quota pool. One failure gates every model of that provider
  for at least 300 seconds (a longer configured cooldown wins); the current
  request may fail over. Saving config preserves the gate. Removing the provider
  drops it, and the operator's model-health reset also clears its provider gate.
  Gates are process-local; expiry admits traffic again, not a provider-wide
  single-probe guarantee.
- **Upstream 400:** repairable errors go through the existing rectifiers first.
  Unrepaired provider rejections are not replayed unchanged on the same model,
  but may fail over within the configured plan. Repeated rejections contribute
  to the model circuit. Locally invalid input remains a local failure.
- **Reasoning history:** Anthropic/Responses thinking text has a Chat
  Completions representation (`reasoning_content`). It no longer hits the
  unsupported-content rejection solely because its source dialect differs.
  Foreign signatures are not asserted to validate on the target provider;
  unrepresentable opaque-only blocks still follow the explicit content policy.
- **Client verification:** the gateway notes recognised, authenticated chat
  arrivals, separately from completed activity records. Desktop verification
  requires an arrival after adoption plus matching managed fields and the
  current token. Discovery/health probes, unknown callers and pre-adoption
  traffic do not confirm ownership. Header-based attribution remains probable,
  not cryptographic identity proof. Evidence is bounded and process-local;
  clearing activity does not erase it, but a restart needs a new request.
- **Panel probing:** the Core reconciler passes each account's `panel_url`
  and `new_api_user` to its NewAPI adapter, alongside that account's credential.
  The desktop's browser/check-in adapter and its balance/reconcile path must
  address the same dashboard, not the inference host. A missing user-id header
  can make a valid personal token look expired on AgentRouter's panel.
- **Stream completion:** an explicit canonical `Stop` settles the lifecycle
  before the terminal egress frame is handed out. A client that closes on
  `response.completed` is not misreported as 499 while the relay keeps TCP open.
  Actual mid-stream disconnection remains interrupted, charged only for
  observed usage, and never poisons provider health.
  Responses reasoning emits the official summary-part events and
  `summary_index`, rather than treating reasoning as a message content part.
  This keeps strict Responses consumers from rejecting the reasoning frames.

For a self-contained desktop run, build through `pnpm build` or Tauri CLI.
The production switch is Tauri's `custom-protocol` feature, **not** Cargo's
release optimization profile: a plain `cargo build --release` can still load
the Vite dev URL. Keep the running fallback binary outside Cargo's output path
while rebuilding on Windows, where a running executable cannot be replaced.

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
