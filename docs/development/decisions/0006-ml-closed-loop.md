# ADR-0006: The ML Closed Loop

Supersedes ADR-0002.

## Context

The repository had a large, adversarially-specified ML infrastructure and no
learning. Concretely:

- `DatasetStore` is a bounded in-memory ring, so every sample the router ever
  collected died with the process. There were no historical routing traces, so
  "train on real traffic" had nothing to name.
- Every training entry point was reachable only from a test. `run_warmup`,
  `run_offline_gate`, `run_bandit`, `Evaluator::compare_routing`,
  `ModelStore`, `ActivationStore` and `temporal_split` had no non-test caller in
  any binary.
- No trained model could affect a served request. `Router` contained no
  reference to `crate::ml`; the shadow record was discarded after its id was
  taken; the served predictor was a JSON file compiled into the binary.
- Promotion did not exist. `ml::activation` was built to be "complete and
  unreachable at the same time".
- Exploration was configuration that nothing read.

Each of these was a deliberate decision with a recorded rationale. Together they
meant the system could never produce the evidence that would justify changing
any of them.

## Decision

ML reaches the routing path, behind a gate, with a fallback and a rollback.

The loop is: real request → terminal outcome → canonical sample + durable trace
→ group-disjoint temporal split → training → frozen-holdout evaluation →
baseline comparison → shadow analysis → promotion gate → active model → provider
selection → new outcome.

Five properties are load-bearing:

1. **A trace is durable.** Without one there is no history to learn from, and
   "we do not have enough data yet" is indistinguishable from "we threw it
   away". `ml::traces` writes from the same terminal transition as the dataset,
   on the same validated outcome and the same retained decision-time input, so a
   trace and the samples it accompanies cannot disagree.

2. **A split is group-disjoint by request.** A per-sample split leaks: the model
   sees attempt 0 of a request in training and is then judged on attempt 1 of
   the same request.

3. **Counterfactuals are reported with their coverage.** A trace records what
   happened, not what would have. Only some (candidate, outcome) pairs are
   measurable, so `ml::comparison` reports coverage, unmeasured counts, and
   paired deltas restricted to the intersection where both arms have a
   measurement. A policy is not rewarded for choosing what the logging policy
   happened to try.

4. **Predicted utility and observed utility never mix.** They are on different
   scales — predicted success lives in `[0, w]`, observed is `±w` — so
   subtracting one from the other would manufacture an improvement out of
   arithmetic. Every delta in a paired comparison is observed-minus-observed.

5. **The fallback is not an error path.** A missing model, a prediction that is
   not finite, a selection outside the executable plan: each costs a request its
   ML ranking and nothing else. The deterministic plan was computed before ML
   was consulted and is still there.

## What this does not claim

- `ml_routing.enabled` defaults to **off**. An installation that has never
  promoted a model routes exactly as it did before. (ADR-0007 builds the learning
  stack into the desktop app; this default is what keeps that from changing
  product behaviour, and it is asserted behaviourally in `config.rs`.)
- ~~The desktop application still compiles no ML. Enabling it is a separate
  decision with a separate trade-off.~~ **Resolved by ADR-0007**, which enables it
  behind a named, still-refusable package feature.
- A model in the active slot has passed a gate. It has not been shown to beat
  every baseline on any particular upstream; the evidence is per-dataset and
  the gate says so.
- Exploration defaults to a probability of **zero**. **ADR-0008** measured the
  cost of that default rather than assuming it: with exploration off, a provider
  added to the configuration is never tried and never learned about. It remains a
  product decision.

## Consequences

- The desktop byte-gate (`scripts/desktop_artifact_test.py`) is retired, and
  ADR-0007 removes its CI step outright: once the desktop package gained the
  feature, the binary was *supposed* to contain the ML serving path and a live
  scan would have asserted the opposite of the truth.
- The anti-wiring assertions in `tests/dataset_production_test.rs`,
  `tests/dataset_ingestion_test.rs` and `tests/shadow_integration_test.rs` are
  replaced by the properties they were protecting, not merely deleted.
- A model can now influence a response, so a bug in ranking can now affect a
  user. That risk is managed by `AppliedRanking::apply` restricting the
  reordering to the executable plan, by the `DecisionEngine`'s own eligibility
  filter remaining authoritative, and by the fallback. It is not eliminated.
- An operator needs somewhere to see what is active. `RouteDecision::ml_ranking`
  makes the influence visible per request; the active model, its commit and its
  promotion history are in `ml::serving`. **ADR-0007** adds the read-only
  surface for this: `ml::MlStatus`, `/v1/ml/status`, the on-demand replay, and a
  rollback that is verified to reach the router rather than only the pointer.

## Related records

- `docs/development/decisions/0002-production-ml-boundary.md` (superseded)
- `docs/development/decisions/0007-ml-operator-surface.md`
- `docs/development/decisions/0008-nothing-random-decides.md`
- `docs/development/ml-closed-loop-report.md`
- `crates/zroutery-core/src/ml/{traces,learning,comparison,shadow_analysis,promotion,serving,status}.rs`
- `crates/zroutery-core/tests/ml_closed_loop_test.rs`