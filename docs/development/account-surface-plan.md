# Account surface: the ordered slices

## Why this is a decomposition and not a task

The request was to put the Account component in the GUI. The repository already
answers when that can happen, in its own records rather than in prose:

| node | status | blocked by |
|---|---|---|
| `STAGE-5` optional account component | `PARTIAL` | — |
| `ACCOUNT` generic contract | `PARTIAL` | `STAGE-5` |
| `NEWAPI` adapter | `PARTIAL` | `ACCOUNT` |
| `I2` migration execution | `PARTIAL` | — |
| `I3` agent takeover ownership | `PARTIAL` | — |
| `I4` restore and rollback | `FAILED` | `I2`, `I3` |
| `UI-NEW-TRACKS` new-track UI and Tauri surfaces | `BLOCKED` | `ACCOUNT`, `NEWAPI`, `I2`, `I3`, `I4` |

`UI-NEW-TRACKS` carries the note **"No placeholder control is treated as a product
lifecycle"** and an unblock condition of *"Close the Account, NewAPI, I2, I3 and I4
lifecycle contracts before adding user-facing controls."*

So the GUI is the **last** of seven slices, not the first, and it is blocked by five
things rather than one. An `AccountPanel` built today would render an empty list
against a store nothing owns — which is the placeholder the record forbids.

Two constraints repeat in both `account.status.json` and `stage-5.status.json` and
are load-bearing rather than incidental:

- **"without changing ML feature order"**
- **"Account remains optional and is not enabled by the default Tauri dependency"**

So Account ships as an opt-in feature. It must not join `default = ["ml"]` in
`src-tauri/Cargo.toml`, and it must not perturb the `ml` feature's position.

## What is *not* part of this

The ML panel work already on `dev` belongs to node `UI` (operational UI, `DONE`),
not to `UI-NEW-TRACKS`. It extends an existing panel with backend capabilities that
already exist and are already owned. Closing `UI-NEW-TRACKS` is a different and much
larger piece of work.

---

## Slice 0 — Revalidate the `STAGE-1` precondition

`STAGE-5`'s unblock condition is *"Revalidate the Stage 1 contract and add an owned
persistence/runtime boundary"*. `STAGE-1` is already `DONE` and its accepted text
names three things: the naming and tier contract tests, capability enforcement on
the direct/tier/policy paths, and the Core P1 revalidation.

This is a verification slice, not an implementation one. It exists because the
record says *revalidate*, and because a record that is trusted without
revalidation is the same class of problem as a gate that is skipped.

**Unlocks:** slice 1. **Effort:** one run of the named tests.
**Do not** flip any node status here.

## Slice 1 — `STAGE-5`: give the store an owner and a persistence boundary

Today `AccountStore` exists, is tested, and is constructed nowhere in the product.
That is the whole of the gap: `new()` returns a value and nobody holds it.

The slice is a runtime-ownership contract, not a feature. What it has to settle,
and what it must not do:

- Who constructs the store, and for how long. `AppState` is the obvious owner,
  which makes it a `Desktop` delegate for Tauri exactly as `Snapshot` is.
- Where it lives between runs. `AccountStore` is in-memory; the record's required
  gate is *"Persistence/runtime ownership"*, so an in-memory-only owner does not
  satisfy it. That means a decision about what is persisted and where.
- Feature-gate it as `account`, non-default, alongside `ml` — never inside it.

**Unlocks:** slice 2. **Gate:** the record's own three, of which
*"Persistence/runtime ownership"* is the one that fails today.

## Slice 2 — `ACCOUNT`: decide where an account lives, and keep its credential out of config

`newapi-adapter.md` states the remaining application work as four items; this is
items 1–2:

1. Enable `zroutery-core/newapi` for `src-tauri`, and decide whether the desktop
   build ships the account subsystem at all.
2. Store the credential in the existing keychain path (`SecretStore` / keyring),
   **not** in `AppConfig` plaintext.

The open decision is `AppConfig` shape: what identifies an account, and which
fields are config versus runtime state. The types already distinguish them —
`AccountRuntime` carries `quota`/`usage`/`rate_limit` as `Option`, and those are
observations, not configuration. Writing them into `AppConfig` would persist a
number that expires.

`AppConfig` is `Deserialize` with a `config_path` the user can edit, so a
credential placed there is a credential in a file the app will also write back.

**Unlocks:** slice 3. **Product decision:** the `AppConfig` shape.
**Gate:** *"Default-build compatibility"* — the `--no-default-features` build must
still compile and still ship no account code.

## Slice 3 — `NEWAPI`: pass a local contract before any real protocol

The record's unblock condition is specific: *"Pass a local mock-server
auth/expiry/refresh/usage/quota contract before any real protocol E2E."*

Five named gates, all against a local mock: auth, expiry and refresh, usage and
quota, and a documented protocol fixture. `probe_status` exists at
`newapi.rs:518` and is the connection test the adapter's own doc points at.

This is the slice where a real account can be created and read without a network.
Until it passes, `AccountProvider` has no production caller worth naming.

**Unlocks:** part of slice 7. **Gate:** the five, all local.

## Slices 4 and 5 — `I2` and `I3`: the unblocked lane

Both are `PARTIAL` with **empty `blocked_by`**, which makes them the only Account-adjacent
work that can start today.

- **`I2` migration execution** — typed action runner, owned stop/start process
  lifecycle, config and auth policy, local child-process fixtures, endpoint
  response policy.
- **`I3` agent takeover ownership** — durable ownership manifest, conflict detection
  and application, confirmation flow, CAS restore fixtures.

They are independent and can run in parallel. They gate `I4`, which gates the GUI.

## Slice 6 — `I4` restore and rollback

Currently `FAILED`, blocked by `I2` and `I3`. Its unblock condition names the four
gates: conflict resolution applied during release, accurate restore failure
reporting, migration rollback semantics, cross-track restore fixtures.

`FAILED` is the only non-`PARTIAL`/`BLOCKED` status on this path, and *"accurate
restore failure reporting"* is the gate that matters most for a feature that
migrates a user's routing configuration.

**Unlocks:** slice 7.

## Slice 7 — `UI-NEW-TRACKS`: the GUI, last

Only once all five of `ACCOUNT`, `NEWAPI`, `I2`, `I3`, `I4` are closed. Then the
work is a real one rather than a placeholder:

- An account list per provider, from a store something owns.
- Quota, usage and rate-limit rendering where present. **Absent is not zero.**
  `AccountQuota`, `AccountUsage` and `RateLimitState` are all `Option`, and a
  `0` renders as a measurement that was taken and found to be nothing, which is a
  different claim from "not supported".
- Capability-gated actions: `supports_usage`, `supports_quota`, `supports_refresh`,
  `supports_checkin`, `supports_health_check` decide which affordances exist. This
  is the same discipline as the `available` flag the ML panel already uses.
- `reset_at` is a timestamp from the upstream, in seconds. Render it with the
  existing `ms()` formatter's sibling for durations and an absolute formatter for
  the time; do not print a raw epoch.

---

## Every slice must keep these true

| invariant | how it is checked |
|---|---|
| Account never enters the default Tauri build | `src-tauri/Cargo.toml` `default = ["ml"]` unchanged |
| ML feature order untouched | `cargo +1.99.0 check -p zroutery` and `--no-default-features` both clean |
| The shipped product cannot name the activation mechanism | `cargo +1.99.0 test -p zroutery-core --all-features --test activation_test` |
| The DAG agrees with the records | `python -B scripts/orch_docs_test.py` |

**The DAG update is a two-file lockstep and it is not optional.**
`scripts/orch_docs_test.py` compares the status column of the inventory table in
`dependency-dag.md` against each `node-status/*.status.json`, and reports
`DAG state disagrees with record` when they diverge. Flipping `ACCOUNT` to `DONE`
without editing the inventory table in the same commit turns a green gate red. The
node id set is compared as a set, so the table must keep exactly the same rows.

It also requires `blocked_by ⊆ dependencies`, so clearing `blocked_by` on a node
whose `dependencies` still lists the blocker fails differently.

## Reading order for whoever picks this up

1. `docs/development/node-status/account.status.json` and `stage-5.status.json` —
   the three `required_gates` and the unblock condition.
2. `docs/development/newapi-adapter.md` "Remaining application work" — the four
   items, which are slices 2 through 4.
3. `docs/development/dependency-dag.md` — the critical path
   `CORE-P1-REPAIR → STAGE-1 → STAGE-5 → ACCOUNT → NEWAPI`, and the parallel lane
   through `I2`/`I3`/`I4`.
4. `crates/zroutery-core/src/account/` — types, store, provider. The `Option`
   fields are the design, not an oversight.

## The honest summary

Account is **three slices deep** (`STAGE-5` → `ACCOUNT` → `NEWAPI`) before the GUI
is even reachable, and the GUI is additionally gated on `I2`, `I3` and `I4`.

If the goal is movement rather than specifically Account, **`I2` and `I3` have no
blockers and can start now**, and they gate `I4` which gates the same UI. That is
the shortest path to a closable `UI-NEW-TRACKS`, and it does not require the
`STAGE-5` revalidation that Account does.