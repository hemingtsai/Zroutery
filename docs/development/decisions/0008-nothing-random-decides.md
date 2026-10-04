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

- **The comparator is part of the measurement**, which is sharper than the point
  above and was measured afterwards. Across five bodies at the exploration
  ceiling:

  ```
    run  providers  paired: priority  round_robin  lowest_latency  balanced
      1          2               4           15              38        40
      2          2               6           11              14        31
      3          1              40           30               8         1
      4          2               2            9              17        34
      5          2               2            7               2        29
  ```

  Run 3 is the clearest case: with no discovery the candidate pairs *perfectly*
  with `baseline.priority` (40) and not at all with `baseline.balanced` (1). With
  discovery, the reverse. Exploration reached the new provider by displacing
  priority's first pick, so the candidate's measured requests and priority's are
  nearly disjoint, while a spreading baseline — which considers the same
  candidates — overlaps them.

  So `baseline.priority` is a **same-provider** choice, and naming it for a
  candidate that has discovered a provider tends to produce
  `BLOCKED (paired_evidence: N)` for a candidate that may be perfectly good. The
  blocker reads as "not enough data" rather than "you compared against the wrong
  arm", which is the failure mode this project exists to avoid.

  A sixth run broke even the anti-correlation — discovery *and*
  `paired(priority)` at 31, one over the floor — so this is a strong tendency and
  not an invariant. It is a diagnostic, not an assertion. A test that fails one
  run in three teaches its reader to re-run it.

- `pair_against_baseline` itself is verified to join on the right key, including
  when the two arms choose different candidates on every request
  (`two_arms_that_choose_different_candidates_still_pair_on_the_same_requests`).
  The collapse above is a property of the evidence, not a pairing bug — which is
  worth having checked, because the symptom is indistinguishable from one.

- `exploration_probability` still defaults to **0.0**, and that default now has a
  measured cost rather than an assumed one. It is a product decision and is left
  to the operator.

### The frozen holdout was a content cluster, not a slice of time

Found while landing the cost-axis fix, and the third defect in this family.

`project_cohorts` sorted cohorts by `CohortOrderKey` — a **content** key — and
`run_calibration` then took `cohorts.split_at(len - holdout)` as the frozen
holdout.

The sort's reasoning was sound and is documented at length in the module: two
cohorts that tie on the key are indistinguishable to every sum it computes, so
exchanging them cannot move a gradient, a log-loss, a reliability bin, a drift
bin, the base rate or the arity. That is true, and it was applied in the wrong
place. **A partition is not a sum.** Which cohorts land in the holdout is decided
by position, and position in a content-sorted vector is a function of tie-breaking
rather than of history.

Because `sort_by` is stable, tied cohorts kept the order `group_attempt_rows`
produced, so the degenerate case appeared only when the ties fell differently.
Measured on a window whose recorded decisions alternated two served identities
perfectly, across thirty decisions:

```
run  providers  paired: priority  balanced
  1          2               4        40
  2          2               6        31
  3          1              40         1
  4          2               2        34
  5          2               2        29
```

and on the calibration side, the holdout's served-identity composition varied
between one and six out of twelve from run to run. When it landed on one, the gate
correctly refused: `DegenerateHoldout { reason: SingleServedCandidate }`.

**The fix**: `project_cohorts` returns arrival order — which
`group_attempt_rows` already produces, since it walks the snapshot's rows by
index — and `run_calibration` splits there, then orders *each partition* by
content. Both properties are then true and neither is traded for the other: the
holdout is a later slice of history, and the computation within it is invariant
to tie permutation.

**The contract changed, and a test encoded the old one.**
`uuid_invariance_the_partition_and_every_number_survive_a_uuid_change` reversed
the second rendering's rows and asserted the report was unchanged, on the
reasoning that "row arrival order is not the content order either". But `render`
stamps `BASE_TIMESTAMP + index`, so reversing the rows reverses the *history*:
it is not one dataset reordered, it is the same measurements running backwards. A
frozen holdout is by definition a later slice, so a time-reversed dataset must
produce a different one. The reversal is gone and the test now asserts what its
name says — identifiers do not matter — with row-order dependence asserted
explicitly alongside it.

### Open, and stated rather than assumed

- The **cost head is verified, and verified not to matter.** It learns its targets
  (features fixed, cost varied 10x, predictions track at 10:1) and the learned
  figure reaches the ranking utility exactly — both pinned by mutation in
  `cost_head_test.rs`. But `compute_utility` scores cost as
  `-cost_weight * min(cost_dollars, 1.0)`, and a per-request LLM bill is cents, so
  nine times the price moves utility by `0.0009` against `0.75` for a success
  difference. The head is wired, working, and has never changed a routing decision.

  That is left as it is and pinned rather than changed: making cost outvote
  reliability is a policy decision nobody has made, and
  `cost_cannot_outvote_success_at_the_shipped_weights` fails until someone argues
  for it in the open.

  Two incidental findings, both asserted so neither changes silently: no head ever
  constructs a `Prediction::cold` (cold-ness reaches the decision through
  `confidence`, which is what `compute_utility` reads), and an untrained cost head
  predicts `0.01` rather than zero, so it looks like a *cheap* candidate rather
  than an absent one.

- A **billed failure** is unattributed. A 5xx carries no usage, so nothing in the
  error says what that failed call cost, and the all-failed buffered path settles
  with `Settlement::default()`. If a provider bills errors, that spend is invisible
  to the ledger and to the cost axis alike. Fixing it means carrying usage out of
  the failure path, which is a protocol change rather than a routing one.

  What is *not* a defect: a stream that ends mid-answer after reporting usage.
  `Error::InterruptedStream` carries it, the streaming path keeps it
  deliberately, and it reaches the same settlement, so the attempt is priced.
  Verified by `a_truncated_stream_that_reported_usage_is_charged_to_its_attempt`,
  which fails with its own message when the per-attempt pricing is removed.

- Whether the engine's 0.1 switch threshold is crossed for an unobserved
  candidate still depends on measured latency, which is wall clock. The *split* is
  now deterministic; this decision is not, and conflating the two would be its own
  kind of dishonesty.
