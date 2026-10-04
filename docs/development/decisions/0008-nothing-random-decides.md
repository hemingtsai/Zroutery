# ADR-0008: Nothing Random Decides

Builds on ADR-0006 and ADR-0007. Does not supersede either.

## Context

ADR-0007 closed the loop's visibility and reversibility. This round went looking
for the product's actual claim — *aggregate several providers* — and every
fixture in the repository routes between two models of **one** provider. Two
providers are configured in each; `beta` exists so the secret store has a key.
No evidence had ever crossed a provider boundary.

Building a three-provider fixture to close that gap turned up two defects, and
the second one is the most serious thing found in this work.

### Defect one: exploration could not open the loop

`apply_ml_ranking` returned early on `!state.ml_routing().is_attached()`, and
exploration lived inside `MlRouter::rank`. So exploration required a promoted
model.

That is a closed loop. The dataset is built from outcomes of requests that
actually happened, so it can only contain candidates the deterministic plan
already reached. A provider at the bottom of the priority order is never tried,
therefore never measured, therefore never learned about, therefore never
preferred — and no amount of retraining breaks it.

Measured on a three-provider fixture with exploration configured at 30% and no
model promoted: the unobserved provider received **0 requests out of 60**, and
the learned ranking used **1 provider**. With the same fixture and
`baseline.round_robin`, which is *defined* as spreading, **2 providers**. The
learned ranking was worse than round-robin at discovering a provider nobody
thought to try, and nothing anywhere reported a problem.

### Defect two: the split was decided by a coin

Chasing why the promotion verdict moved produced the real finding.
`print_how_often_the_verdict_moves` runs the same fixture, the same traffic and
the same configuration six times:

```
  run  gamma calls  ml providers  ml measured  paired(priority)  verdict
    1            0             1           46                46  PROMOTED
    2            0             2            2                 2  BLOCKED
    3            0             1           46                46  PROMOTED
    4            0             2            2                 2  BLOCKED
    5            0             1           46                46  PROMOTED
    6            0             2            2                 2  BLOCKED
```

Three of six, alternating. Not noise — a deterministic alternation, which meant
something was varying systematically.

`split_samples` ordered groups by `(timestamp, request_id)`. Timestamps have
**second** resolution (`chrono::Utc::now().timestamp()`), and request ids are
fresh UUIDs (`format!("dec-{}", Uuid::new_v4().simple())`). Sixty requests driven
in a couple of seconds all tie, so the request id broke every tie, so **which
requests landed in the frozen holdout was decided by a random UUID**.

That is not a cosmetic problem. `dataset_identity` and `holdout_loss` are two of
the nine promotion criteria. A gate whose verdict alternates on identical
evidence cannot mean anything reproducible, and "the gate said PROMOTED" stops
being a fact about the model.

The doc comment on `split_samples` claimed: *"The split is a pure function of the
body: no clock, no RNG, no hash-map iteration order."* Two of those three were
true. The claim was wrong in exactly the way that mattered, and it had been
wrong since the function was written.

## Decision

### The split's tiebreak is position in the supplied body, not an identifier

`split_samples` now carries each sample's index alongside it and orders by
`(timestamp, index)` — within a group and between groups.

Input position is the right key because it is the one order that is **durable**:
`deduped_samples_from` reads an append-only log, so a sample's position is its
write order. A caller that supplies samples in some other order gets a
deterministic split *of that order*, which is at least reproducible — whereas
the previous behaviour was deterministic only within a single process lifetime.

The contract is now stated as it actually is: a pure function of the body *and
its order*. `the_split_follows_the_supplied_order_rather_than_the_identifiers`
pins that, because it is a real consequence — reordering the input moves the
partition — and the old behaviour hid it by ignoring the supplied order entirely.

Verified: 6 of 6 identical after the change, against 3 of 6 alternating before.

### Exploration is reachable with no model, and says so when it fires

`MlRouter::explore_plan` draws from the eligible set against the deterministic
plan's own pick, with or without an attached model, and reorders strictly within
the executable plan.

The result is recorded as `MlRankingTrace { modelled: false, commit_id: "" }`. The
field is a real boolean rather than an empty commit id because a field that can
hold a fake commit is a field that will, and because *"the model chose this"* and
*"the router went to find out"* are different facts that every consumer has to be
able to tell apart.

`MlRouterCounts` gains `blind_explorations`, counted apart from `explorations`.
One says the model chose to try something else; the other says the router admitted
it knows nothing and went to find out. Folding them into one number would let the
second be reported as the first, which is the exact confusion this whole module
tree exists to prevent.

`MlRankingTrace::modelled` defaults to `true` on deserialisation, because every
trace written before the field existed was a model ranking.
## Consequences

- The promotion gate is reproducible on identical evidence, and a holdout loss
  means something.
- A provider added to the configuration can be discovered, provided exploration
  is on. With it off, the router is frozen against configuration changes — which
  is a **product decision**, recorded below rather than made here.
- Two findings are now characterised rather than fixed, because fixing either
  would mean inventing a policy:

  **An unobserved candidate is predicted as its prior, and average beats a bad
  incumbent.** On a plan whose first choice succeeds a quarter of the time, the
  model reaches for the candidate it knows nothing about. Those choices have no
  recorded outcome, so the request cannot be paired.

  **Therefore a model that genuinely prefers a different provider is harder to
  promote than one that agrees with the incumbent.** Pairing is defined on the
  measured intersection, so divergence removes requests from the comparison — and
  the better the model gets, the fewer requests remain to prove it. The
  requests that would prove the model right are the requests nobody collected.
  Exploration is what breaks this, and it has to be on *before* the promotion
  attempt, not after.

- `exploration_probability` still defaults to **0.0**, and that default now has a
  measured cost rather than an assumed one. It is a product decision and is left
  to the operator.

### Open, and stated rather than assumed

- Cost is still degenerate. At 400 prompt and 120 completion tokens every arm
  reports `mean_cost` at five decimal places as `0.00000`. The cost axis carries
  no discriminative signal in these fixtures even after the token counts were
  raised.
- Whether the engine's 0.1 switch threshold is crossed for an unobserved
  candidate still depends on measured latency, which is wall clock. The *split* is
  now deterministic; this decision is not, and conflating the two would be its own
  kind of dishonesty.
