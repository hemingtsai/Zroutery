# ADR-0007: The Loop Is Legible and Reversible

Builds on ADR-0006. Does not supersede it.

## Context

ADR-0006 made the closed loop real: real requests produce durable traces, traces
train an identified model, a gate decides whether that model may serve, it
serves, and it is rolled back. Every part of that was reachable from Rust and
from the headless binary.

Two things were still missing, and both of them are things a person needs rather
than things a test needs.

**Nothing could see it.** An operator could not answer the only question that
matters in production — *what is routing my requests right now, and who
authorised it?* — without a debugger. There was no status document, no
counterfactual view, and no history. A mechanism that cannot be inspected is a
mechanism nobody can operate, and a mechanism nobody can operate gets turned off
the first time it does something surprising.

**The desktop app was outside the loop.** `src-tauri` depended on
`zroutery-core` with no features, so the shipping desktop product contained no
trace log, no model store and no gate. "Zroutery is a router that learns" was
true of a CLI and false of the thing most people install. Worse, the loop stopped
even earlier in practice: nothing in the product ever set
`ml_routing.state_dir`, so there was no durable state to learn from at all.

## Decision

The loop ships in the desktop app, and it can be read and undone from it.

### The state directory is claimed by the app that owns it

Durable ML state stays **opt-in** — that part of ADR-0006 stands, because an
implicit directory meant every process on the machine shared one. But the desktop
app is the one place that legitimately owns a directory, so `Desktop::new` now
points `ml_routing.state_dir` at `<config_dir>/ml` unless the document already
names one.

The document's value always wins. An operator who wants the history elsewhere
gets that; the default is a floor, not an override.

Claiming a directory changes nothing an operator can observe. `ml_routing.enabled`
still defaults to false, `state_dir` still defaults to empty, and no default
configuration has ever promoted a model. An install that has never promoted a
model routes exactly as it did before. Both defaults are now asserted
behaviourally, against a default-constructed *and* a document-deserialised
configuration, because "opt-in" is a claim about what those configurations do.

### `ml` is a named desktop feature, on by default, still refusable

```toml
default = ["ml"]
ml = ["zroutery-core/ml"]
```

A package feature rather than a bare `features = [...]` on the dependency,
because a bare one cannot be turned off without editing the manifest. `cargo build
-p zroutery --no-default-features` still produces a desktop app with no learning
stack, and CI now builds it, because "ship without learning" is a legitimate
choice for someone who does not want their routing history on disk and a promise
nothing else keeps honest.

### Capability is declared, not discovered

`Snapshot` carries `ml_available`. The dashboard checks it before calling the ML
commands rather than calling them and reading the error.

To a webview, a command that was never registered and a command that failed are
the same event. Without the flag, a broken IPC bridge renders as "this build has
no ML stack" — which is the one conclusion an operator must not be led to by
accident, because it looks like a deliberate configuration rather than a fault.

### The status document reports state, and says which state

`ml::MlStatus` is read live from the router, the stores and the gate. It has one
job: describe the process. The distinctions it keeps are the ones an operator
would otherwise have to guess:

- no model attached vs. `enabled` with nothing attached — the misconfiguration,
  which reads as "no model is attached" and never as "working";
- promoted but `enabled: false` — installed and deliberately inert;
- "could not tell whether there is a model" vs. "there is no model" — a store
  that fails to read reports itself as unreadable;
- counters as counters. A count of collected samples says collection is
  happening. It says nothing about whether the model is good, and the document
  does not let the two be read as one.

The one quality claim on it is the gate's, carried whole: which configuration was
in force, which baseline had to be beaten, and every criterion with the number it
was decided on.

### The replay answers "what would this model have done with my traffic?"

`AppState::ml_shadow_analysis(limit)` replays the **attached** model over the
durable trace tail. Same `analyse` the offline gate uses, the operator's own
traffic instead of a fixture, and the weights actually ranking requests rather
than a re-trained copy.

Bounded, because it is callable from a button: the tail is read by walking
backwards to the *n*-th line from the end, and the limit is capped again inside.
`TraceLog::load` reads everything, which is right for training and wrong for a UI
click on a year of traffic.

It reports *why* it could not run. "No history" and "no model attached" are
different answers and only the first is fixed by serving traffic.

### A rollback has to reach the router

`AppState::rollback_active_model` moves the durable pointer **and** reloads the
router from it.

Doing only the first is a cosmetic rollback: the pointer says one thing, the
router keeps ranking with the withdrawn model, and every status document then
reports a state the process is not in. This is not hypothetical — a rollback that
only rewrote the pointer was the defect class ADR-0006 already found once, and
this test exists because of it. `a_rollback_takes_effect_in_the_live_process_and_not_only_on_disk`
promotes two models into a *running* process, requires the router's attached
commit to follow the pointer forward and back, and drives real traffic through
the restored model to prove it ranks. It was verified to fail against a
pointer-only rollback.

A pointer that cannot be read is **not** treated as "no model": whatever is
attached stays attached and the fault is reported. The one thing an operator must
not lose to a transient read error is a model they chose.

### The status surface is behind the auth layer

Which model is serving, and on whose authority, is not public information. It says
what an installation's routing is doing right now. `/v1/ml/status`,
`/v1/ml/shadow` and `/v1/ml/rollback` sit behind the same middleware as
everything else, and the tests assert the 401.

## Consequences

- The desktop app is where the loop lives, not a CLI that can.
- An operator can see the model, its gate, its evidence, its history and its
  counterfactuals, and can remove it in one action without stopping the proxy.
- `ModelEnsemble` and the four models derive `Clone`, so an analysis can snapshot
  the serving weights rather than hold the router's lock for a whole replay.
- The retired desktop byte-scan gate is gone from CI, not merely parked. Its
  premise inverted when the desktop package gained the feature: the binary is now
  *supposed* to contain the ML serving path, so a live scan would assert the
  opposite of the truth.
- `Coordinator` and `train_batch` remain unreferenced by production. Still out of
  scope, still harmless, still a thing to delete.

### Known gaps, stated rather than assumed

- **Cold start is uncharacterised.** A fresh process has an empty observation
  store, so the first one or two requests fall back to the deterministic plan.
  That is bounded and measured per-request in the tests; it is not yet measured as
  a function of traffic rate.
- **The desktop assertions stop at the core.** The status document, replay and
  rollback are exercised through `AppState` and over HTTP. Tauri command dispatch
  through a webview is not covered by a test, because the harness would have to be
  a webview. `ml_available` exists so that a failure there is reported as a
  failure rather than read as an absence.
- **Multi-provider robustness is still unmeasured.** Every fixture is
  single-provider, so cross-provider interaction remains unexercised.
