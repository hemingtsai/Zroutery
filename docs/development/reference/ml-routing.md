# ML Routing Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

This page describes how a learned model can change a route. It is a mechanism
reference, not a claim that a model has earned promotion; current status is in
[current-state.md](../current-state.md).

## What it does

```text
decision-time snapshot
  -> shadow record / durable trace
  -> offline training round
  -> promotion gate (named baseline, evidence floors)
  -> operator promotion (install=true)
  -> pointer file / in-process attach
  -> MlRouter::rank on the eligible plan
  -> fallback to the production plan when no model can rank
```

The model re-orders a plan the router already produced. It never derives
eligibility, adds a candidate, or bypasses the circuit breaker
(`crates/zroutery-core/src/server/pipeline/ml_rank.rs::apply_ml_ranking`,
`crates/zroutery-core/src/ml/serving.rs::MlRouter::rank`).

## Invariants

1. **ML cannot widen eligibility.** The ranking input carries the eligible flag;
   a rejected candidate stays rejected. `rank` predicts for every candidate so
   the engine's own filter excludes it, rather than this layer deciding
   eligibility on the side (`crates/zroutery-core/src/ml/serving.rs::MlRouter::rank`).
2. **Promotion is explicit.** A round returns a verdict; installing is a
   separate argument and defaults to false. There is no timer and no path that
   promotes because a score looked good
   (`crates/zroutery-core/src/ml/round.rs::run_promotion_round`,
   `crates/zroutery-core/src/server/ml_state.rs::ml_run_promotion_round`).
3. **The gate names its baseline.** "Beats the baseline" is a claim about a
   named baseline, not whichever was convenient. The evidence floors are not
   caller-controlled (`crates/zroutery-core/src/ml/promotion.rs::PromotionConfig`).
4. **Rollback is the same path in reverse.** Reloading the previous pointer
   detaches the model from the live router; no restart is required
   (`crates/zroutery-core/src/server/ml_state.rs::reload_active_model`,
   `crates/zroutery-core/src/server/ml_state.rs::rollback_active_model`).
5. **Shadow is non-authoritative.** A shadow record reports an alternative; it
   does not mutate routing, session, account or budget state
   (`crates/zroutery-core/src/ml/shadow.rs::ShadowEngine::evaluate`).
6. **No model means the production plan.** No attached model, no candidates, or a
   non-finite prediction falls back to the router's order; the fallback is
   counted (`crates/zroutery-core/src/ml/serving.rs::MlRouter::rank`).
7. **Traces are evidence, not a second truth.** The durable log is appended after
   settlement and can be read or cleared by the operator; clearing is explicit
   because the next round trains from the whole log
   (`crates/zroutery-core/src/ml/traces.rs::TraceLog`).

## Promotion gate

The shipped defaults are a policy, not a suggestion
(`crates/zroutery-core/src/ml/promotion.rs::PromotionConfig`):

| Criterion | Default |
|---|---|
| required baseline | `baseline.lowest_latency`, or the caller's named baseline |
| minimum paired requests | comparison floor (`MIN_PAIRED_REQUESTS`) |
| minimum utility delta | comparison threshold (`UTILITY_DELTA_THRESHOLD`) |
| success regressions | 0 |
| fallback-rate increase | at most 0.02 |
| mean-cost increase | at most 0.05 |
| mean-latency increase | at most 250 ms |
| ineligible selections | never allowed |
| holdout loss improvement | at least 0.05 nats over an uninformed predictor |

The gate returns a verdict with the candidate commit, the evidence body and the
configuration identity, so a decision can be re-derived rather than trusted
(`crates/zroutery-core/src/ml/promotion.rs::PromotionGate::evaluate`).

## Data path

- A trace is built from the decision-time snapshot, not from a later
  reconstruction (`crates/zroutery-core/src/ml/traces.rs::RequestTrace`).
- `run_promotion_round` reads the durable traces, deduplicates samples, trains,
  runs the comparison and evaluates the gate
  (`crates/zroutery-core/src/ml/round.rs::run_promotion_round`).
- The operator surface is the authenticated `/v1/ml/*` endpoints; installation
  and rollback are explicit calls, not a background loop
  (`crates/zroutery-core/src/server/handlers.rs::ml_promote`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| no model is attached | nothing promoted | deterministic routing (`crates/zroutery-core/src/ml/serving.rs::MlRouter`) |
| no candidate is eligible | the plan is empty | refuse; never invent a fallback |
| rollback is needed | a promoted model misbehaves | rollback the pointer (`crates/zroutery-core/src/server/ml_state.rs::rollback_active_model`) |

## Not implemented

- No automatic promotion, no scheduled training, no online update between rounds.
- No production exploration without passing the controlled-takeover gates; the
  current state records what is blocked.
- The benefit of a learned model on **real** provider traffic is not established
  by the offline fixtures; a promoted model is evidence of a closed loop, not of
  a production cost saving.

## Sources

- `crates/zroutery-core/src/ml/serving.rs::MlRouter`
- `crates/zroutery-core/src/ml/serving.rs::MlRouter::rank`
- `crates/zroutery-core/src/ml/shadow.rs::ShadowEngine::evaluate`
- `crates/zroutery-core/src/ml/traces.rs::TraceLog`
- `crates/zroutery-core/src/ml/promotion.rs::PromotionConfig`
- `crates/zroutery-core/src/ml/promotion.rs::PromotionGate::evaluate`
- `crates/zroutery-core/src/ml/round.rs::run_promotion_round`
- `crates/zroutery-core/src/server/ml_state.rs::reload_active_model`
- `crates/zroutery-core/src/server/ml_state.rs::rollback_active_model`
- `crates/zroutery-core/src/server/handlers.rs::ml_promote`
- `crates/zroutery-core/src/server/pipeline/ml_rank.rs::apply_ml_ranking`
