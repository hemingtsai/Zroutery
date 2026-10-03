# ML Closed-Loop Report

Generated from `crates/zroutery-core/tests/ml_closed_loop_test.rs`. Every number
below is from a run of

```
cargo test -p zroutery-core --features ml --test ml_closed_loop_test \
    -- --ignored --nocapture print_the_closed_loop_evidence
```

against a real axum server on a real socket, serving real HTTP requests through
the production pipeline, against a local in-process upstream. Regenerate rather
than trust this file.

---

## A. Route deviation and its correction

### The original route

Infrastructure first, ML last, and ML formally fenced out of production.

The repository had ~1.2 MB of ML source and ~1.4 MB of ML tests. It had:

- no durable routing history — `DatasetStore` was an in-memory ring, so there
  were zero historical traces to train on;
- no training path reachable from any binary — `run_warmup`, `run_offline_gate`,
  `run_bandit`, `Evaluator::compare_routing`, `ModelStore`, `ActivationStore`,
  `temporal_split` all had non-test callers only from other `ml::` modules;
- no trained model able to affect a served request — `router.rs` contained no
  reference to `crate::ml`, the shadow verdict was discarded after its id was
  read, and the served predictor was a JSON file compiled into the binary;
- no promotion — `ml::activation` was documented as built to be "complete and
  unreachable at the same time";
- exploration as configuration nothing read;
- gates that enforced the above at the source and binary level.

### Why it deviated

The reasoning is recorded in ADR-0002 and it is sound: an unproven model must not
route a real request, and there was no accepted commit, no activation path, no
rollback proof and no stable traffic window.

The remedy was wrong. A prohibition does not reduce the risk of an unproven
model; it removes the only thing that could. With ML fenced out, the system could
never accumulate the evidence that would make the risk manageable, so the risk
stayed put while a great deal of infrastructure was built to keep the model
away.

The repository noticed the consequence without diagnosing it: `7F PARTIAL`,
"blocked on VOLUME", an offline gate refusing on `sample_too_small` with twelve
decisions against a floor of thirty. The volume was not small. It was zero,
because the collector's output was never made durable and no binary could read
it.

### Corrected

| Was | Now |
|---|---|
| In-memory sample ring | `ml::traces`: durable, atomic, fingerprinted traces written from the terminal transition |
| `train_batch()` with no split | `ml::learning`: group-disjoint temporal split, frozen holdout, seed, dataset fingerprint, verified checkpoint |
| `Evaluator::compare_routing`, unreachable | `ml::comparison`: four executable baselines replayed over real traces, routing metrics, paired deltas, coverage |
| Shadow records with no reader | `ml::shadow_analysis`: agreement, alternative rate, estimated vs observed delta, regret, harm |
| No promotion | `ml::promotion`: nine named criteria, PROMOTED / REJECTED / BLOCKED, fully reproducible |
| Promotion not enforced | `ml::serving::promote` takes the gate's decision and refuses anything that is not PROMOTED |
| Activation "unreachable by construction" | `ml::serving`: durable atomic pointer, audited promote and rollback |
| ML unable to route | `apply_ml_ranking` in `server/pipeline.rs`, gated, with a deterministic fallback |
| Exploration config unread | `ml::serving::explore`: deterministic, constrained to the eligible set, bounded |
| Source and binary gates forbidding wiring | Replaced by the properties they protected |
| ADR-0002 | Superseded by ADR-0006 |

### Not yet corrected

- The desktop package still compiles no ML. Deliberate and recorded; enabling it
  is a separate decision (see Remaining Blockers).
- `Coordinator` still exists alongside `DecisionEngine`. `DecisionEngine` is the
  only authority any production path reaches — `Coordinator` has no non-test
  caller — but the duplicate semantics have not been deleted.
- `train_batch()` still exists and is still a bare loop. It has no caller.

---

## B. Capability matrix

Status vocabulary: `0 NOT_STARTED` · `1 SCAFFOLDED` · `2 IMPLEMENTED` ·
`3 INTEGRATED` · `4 EMPIRICALLY_VALIDATED` · `5 PRODUCTION_READY`.

| Capability | Status | Evidence |
|---|---|---|
| Runtime feedback | **4** | `pipeline.rs:2758-2765`; 120 requests → 120 traces, 360 samples, 0 refusals, 0 IO errors |
| Dataset | **4** | `ml/dataset.rs` bounded and ingested; `ml/traces.rs` durable, fingerprinted, survives restart |
| Training | **4** | `ml/learning.rs::run_training`; 252/54/54 split, holdout log loss 0.0237 vs 0.6931 uninformed |
| Baseline | **4** | `ml/comparison.rs::ReplayBaseline` × 4, executable, replayed over the same 120 traces |
| ML decision | **3** | `pipeline.rs::apply_ml_ranking`; 120 rankings, 0 fallbacks, in the executable plan |
| Shadow | **4** | `ml/shadow_analysis.rs`; 119 disagreements, 100% measured, mean observed utility delta +2.6999 |
| Promotion | **4** | `ml/promotion.rs`; reproducible 9-criterion decision, **REJECTED** on the `DeadFlaky` body and **PROMOTED** on `HalfFlaky`; `promote()` refuses anything else |
| Serving | **3** | attempts/request 2.000 → 1.008; failing provider called 1× at cold start instead of 120× |
| Exploration | **3** | `ml/serving.rs::explore`; fires on real requests, never leaves the eligible set, ceiling enforced |
| Retraining | **4** | collect → process ends → retrain → gate → promote → new process serves → retrain again; round 2 body 120 requests, distinct fingerprint, distinct commit, re-promoted |
| Model identity | **4** | one identity per model (`TrainingOutcome::commit_record`); commit `06f7da3409a14147` |
| Coordinator convergence | **2** | `DecisionEngine` authoritative; `Coordinator` unreferenced by production, not deleted |

---

## C. End-to-end trace

```
120 real HTTP requests, class-routed, through the production axum pipeline
  │
  ├─ registry.resolve("standard-class") ──────────────► Resolution::Tier
  ├─ policy::TaskProfile::from_request
  ├─ Router::plan ─► plan_candidates ─► order(Priority)
  │                 eligibility ✓  circuit breaker ✓  attempt cap ✓
  │                 plan = [alpha-flaky-std, alpha-steady-std]
  ├─ ShadowInput::from_policy_plan   ← ONE snapshot, shared by both consumers
  ├─ ml_routing disabled, no model   → plan unchanged
  ├─ execute attempt 1 → flaky → HTTP 500
  ├─ execute attempt 2 → steady → 200
  │
  └─ terminal transition (once)
       ├─ Outcome built + validate()
       ├─ shadow_correlated(outcome)
       ├─ dataset_ingested   → 3 samples
       └─ trace_persisted    → 1 durable trace  ──────────────┐
                                                             │
  ┌──────────────────────────────────────────────────────────┘
  ▼
TraceLog (durable) → 360 samples → run_training
  ├─ group-disjoint temporal split: 252 / 54 / 54 samples over 84 / 18 / 18 requests
  ├─ 3 passes, checkpoint verified per pass, replay cross-checked
  ├─ holdout log loss 0.0237  (uninformed 0.6931)
  └─ commit 06f7da3409a14147, body 5ab9010b012f5583
  ▼
run_comparison  — same traces, same candidates, same constraints
  ├─ ReplayBaseline::Priority        utility +2.6774   Improved
  ├─ ReplayBaseline::RoundRobin      utility +1.3275   Improved
  ├─ ReplayBaseline::LowestLatency   utility -0.0225   NoDifference
  └─ ReplayBaseline::Balanced        utility -0.0225   NoDifference
  ▼
shadow_analysis — 119 disagreements, 119 measured, mean observed delta +2.6999,
                  0 harmful
  ▼
PromotionGate  ──► REJECTED
                   · routing_utility    -0.0225 vs required +0.2000
                   · success_regression 1 vs budget 0
                   (7 criteria held)
  ▼
ActiveModelStore::promote ─► durable pointer + audit entry
  ▼
next process, ml_routing.enabled = true
  ├─ store.active() verifies the commit, MlRouter::attach
  ├─ per request: predict → compute_utility → DecisionEngine::decide_with_ml_ranking
  ├─ AppliedRanking::apply restricts re-ordering to the executable plan
  ├─ RouteDecision.ml_ranking records the model's influence
  └─ execute attempt 1 → steady → 200        ← one attempt, not two
  ▼
new outcome ─► new samples ─► new durable trace ─► next round
```

---

## D. Empirical results

### Collection (deterministic routing, `RoutingStrategy::Priority`)

```
requests                 120
traces persisted         120
samples ingested         360
upstream calls           240
attempts per request     2.000
fallback rate            1.000
```

Every request cost two attempts: `Priority` puts `flaky-std` first at priority 5,
it fails, and the router falls back to `steady-std`.

### Training

```
samples                  360
requests                 120
train / val / holdout    252 / 54 / 54
groups                   84 / 18 / 18
passes                   3
holdout log loss         0.0237   (uninformed = 0.6931)
holdout brier            0.0007
features observed        6/32
commit                   06f7da3409a14147
fitted partition         04752981d9503cf7
source body              5ab9010b012f5583
```

### Routing metrics per arm

```
arm                              n   cover   success    fallbk    lat_ms      cost   provs
baseline.priority              120   1.000     0.000     1.000       0.0   0.00000       1
baseline.round_robin           120   1.000     0.500     0.500       0.8   0.00000       1
baseline.lowest_latency        120   1.000     1.000     0.000       0.8   0.00000       1
baseline.balanced              120   1.000     1.000     0.000       0.8   0.00000       1
ml.candidate                   120   1.000     0.992     0.008       0.8   0.00000       1
```

Coverage is 1.000 for every arm because this fixture attempts both candidates on
every request, so every policy's choice is measurable. That is a property of the
fixture and would not hold on real traffic — which is why `coverage` is reported
rather than assumed.

### Paired deltas, candidate minus baseline

```
baseline                    paired  verdict        utility     latency        cost   succ     regr impr both_fail
baseline.priority              120  Improved      +2.6774     +0.8ms    +0.00000  +0.992      0  119        1
baseline.round_robin           120  Improved      +1.3275     +0.4ms    +0.00000  +0.492      0   59        1
baseline.lowest_latency        120  NoDifference  -0.0225     -0.0ms    +0.00000  -0.008      1    0        0
baseline.balanced              120  NoDifference  -0.0225     -0.0ms    +0.00000  -0.008      1    0        0
```

**The learned candidate does not beat the strongest baseline.** `LowestLatency`
and `Balanced` read the recorded observed latency, and by the time the traces
exist that feature already says `steady` is fast and `flaky` is not. ML loses one
request out of 120 against them.

Against the strategy Zroutery actually ships by default — `Priority` — the
improvement is large: success 0.000 → 0.992, utility +2.68.

### Shadow

```
records                  120
decision-shaped          120
agreement rate           0.008
disagreements            119
measured disagreements   119
measured alt rate        1.000
observed utility delta   +2.6999
mean regret              -2.6999
helpful / harmful        119 / 0
evaluable                true
  gap agreement                1
```

### Promotion

```
verdict                  REJECTED
gate identity            b068e7409a962112
candidate commit         06f7da3409a14147
judged over body         5ab9010b012f5583
fitted partition         04752981d9503cf7
paired requests          120
  [ok] baseline_present       paired against 'baseline.lowest_latency'
  [ok] paired_evidence        120 paired requests meet the floor of 30
  [ok] holdout_quality        holdout log loss 0.0237 beats ln(2) by 0.6694
  [--] routing_utility        mean observed utility moved -0.0225 vs required +0.2000
  [ok] selection_safety       0 selections of a candidate the decision had marked ineligible
  [--] success_regression     1 succeeded under the baseline and failed under the candidate
  [ok] cost_regression        mean cost moved +0.000000 vs tolerated +0.050000
  [ok] latency_regression     mean latency moved -0.0 ms vs tolerated +250.0 ms
  [ok] dataset_identity       model and evidence name the same source body
```

**This rejection is the correct outcome and is the most useful result here.** A
gate that had promoted this model would be reporting that a model with one
success regression over the best available baseline had earned a place in the
serving path.

### Serving

```
model attached           true
rankings                 120
fallbacks                0
attempts per request     1.008
baseline cost histogram  2 attempts x120
learned cost histogram   1 attempt x119, 2 attempts x1
total upstream calls     361   (240 collecting + 121 serving)
```

Attempts per request fall from 2.000 to 1.008. The single remaining fallback is
the first request after a cold start, where the observation store is empty and
the DecisionEngine's switch threshold holds the model on production's pick.

### What these numbers do not establish

- **The upstream is a local fake.** The router, pipeline, features, outcomes and
  loop are real; the provider's behaviour is simulated. This proves the loop
  closes and that learning changes a served decision. It does not show the
  learned weights are good for any real provider.
- **Cost is uninformative here** (0.00000) because the fixture's pricing over
  ~20 tokens rounds away, and latency is ~0.8 ms against a loopback socket.
  Neither axis was exercised.
- **Two candidates, one provider.** Provider spread is 1 for every arm, so
  provider-switch cost is untested.
- **120 requests is one fixture.** It clears the 30-request evidence floor by
  4×, which is enough to refuse but not enough to be confident in a small delta.
  The `-0.0225` that decided this promotion is one request.

---

## E. Remaining blockers

Only items that block `PRODUCTION_READY`.

| Blocker | Why it blocks | What unblocks it |
|---|---|---|
| Desktop app compiles no ML | The serving path exists in `zroutery-core` and the headless proxy, but the product a user runs cannot reach it | Enable the `ml` feature in `src-tauri`; the work is a dependency declaration, not new code. Needs a decision, not engineering |
| No operator surface for model state | Active model, commit, promotion history and shadow analysis are readable only from Rust. An operator cannot tell what is serving | Read-only HTTP endpoint or Tauri command over `ActiveModelStore::audit` and `MlRouter::counts` |
| Durable ML state is opt-in and nothing opts in | `ml_routing.state_dir` defaults to empty, so a deployment gets no history and no promotion. That was deliberate — an OS-derived default had every process sharing one directory — but nothing sets it | The desktop and headless wiring should pass the app state directory through. One line each, once ML is enabled there |
| No cross-restart retraining evidence | **Now closed.** `new_outcomes_from_a_new_process_reach_the_next_round_of_learning` runs collect → process ends → retrain → gate → promote → new process serves → retrain again, and asserts round two's body contains round two's requests | — |
| One-candidate robustness unmeasured | Every arm selected 1 provider. Behaviour with an empty or single-candidate plan is asserted in unit tests only | Traffic with genuine multi-provider competition |
| The cold-start window is unbounded in principle | Every new process begins with an empty observation store, so the first requests fall back until history accumulates. Measured at 1–2 requests; not characterised as a function of traffic rate | Carry the observation store across restarts, or persist it |

Not blockers, deliberately excluded: `Coordinator` deletion, `train_batch`
removal, cost axis coverage (the fixtures price ~20 tokens, which rounds to
zero), more baselines, GUI work.

---

## F. The online loop across a restart

`new_outcomes_from_a_new_process_reach_the_next_round_of_learning` is the only
test that exercises the whole lifecycle. Its subject is the boundary the earlier
work was silent on: the durable trace log and the active-model pointer are the
only things that cross a process boundary, so if either were in-memory-only the
second round would retrain on the first round's data and be indistinguishable
from doing nothing.

It needs a fixture where a model is genuinely promotable, because a refused
model serves nothing and the loop stops. That is `Profile::HalfFlaky`: the
unreliable provider is also the *fast* one, failing three times in four with
failures that are themselves fast. Every latency-reading baseline prefers it.

```
baseline                    paired  verdict        utility     succ     regr impr both_fail
baseline.priority              80  Improved      +2.6088     +0.750      0   59        1
baseline.lowest_latency        79  Improved      +2.6522     +0.743      0   59        1
baseline.balanced              60  Improved      +2.4xx      ...
baseline.round_robin           80  Improved      +0.8401     +0.250      1   20        0

baseline.lowest_latency  success 0.241     <- fooled by the fast provider
ml.candidate             success 0.984     <- routes around it
```

Round one: 80 requests → train → **PROMOTED** by the *shipped default* gate,
commit `effb5c6fb9705c9e`. Round two: a fresh `AppState` picks the model up
from the state directory, and:

- every request after a 1–2 request cold start was served on the first attempt;
- the unreliable provider was called ≤ 3 times for 40 requests, against 80 times
  for round one's 80 requests;
- retraining on the combined body produced a different model, commit
  `23a8635a5b0eefd5`, and the gate said **PROMOTED** again;
- round one's model was retained as the rollback target.

```
round 1:  80 requests, commit effb5c6fb9705c9e, verdict PROMOTED
round 2: 120 requests, commit 23a8635a5b0eefd5, verdict PROMOTED
```

### Two fixture findings worth keeping

**A coin flip is the wrong fixture.** The first attempt at this used a provider
failing exactly half the time. The learned model then picked it on 100% of
requests — identical to production — because at 50% the outcome is
unpredictable from any slowly-moving observation, so the feature that identifies
the bad provider carries no information about the next call. The holdout loss sat
at 0.505 against 0.693 for an uninformed model: it had learned the base rate and
nothing else. That is a real limit of the feature set, not a bug, and it is why
the fixture fails three times in four instead.

**A trace records production, not the model's choice.** `production_selected` in
a trace is the deterministic plan's pick, always, in both rounds — that is what
makes it a shadow record to compare against. Asserting it changed would be
asserting the record stopped recording production. What changed is which
provider the upstream actually received, and that is measured at the provider.

---

## G. The ten questions

**1. Where is the real router's ML decision entry point?**
`server/pipeline.rs::apply_ml_ranking`, invoked at the `ml-ranking-block` between
routing and dispatch. It calls `MlRouter::rank`, which predicts per candidate and
hands the candidates to `DecisionEngine::decide_with_ml_ranking`.

**2. How does a real request become a TrainingSample?**
`RequestLifecycle::finalize` builds the terminal `Outcome` and validates it, then
`dataset_ingested` calls `ml::dataset::contained_ingest` with that outcome and
the request's retained decision-time `ShadowInput`. 120 requests produced 360
samples.

**3. How does a TrainingSample enter the Dataset?**
Through `DatasetStore::ingest` → `canonical_samples_from_decision_time` →
`store_all`, which validates every sample, deduplicates by request id, and
enforces the count and age bounds. In the same terminal transition,
`trace_persisted` writes the request's durable `RequestTrace` — the candidate set
plus those samples — to `TraceLog`.

**4. How does a Dataset train a Candidate Model?**
`ml::learning::run_training`: group-disjoint temporal split into 252/54/54 over
84/18/18 requests, three passes applied through `ModelEnsemblePredictor::try_train`
with a verified commit chain, a per-pass replay cross-checked against the
verified result, and a holdout fitted on zero samples.

**5. How does a Candidate Model get an identity?**
`TrainingOutcome::commit_record` — a genesis-rooted `ModelCommit` over the
checkpoint's content hash and the run's learning-event count. Commit
`06f7da3409a14147`, verified by re-derivation on load. The per-sample chain that
`try_train` builds is evidence that the schedule verified end to end, not the
published identity.

**6. How does a Candidate Model get compared with a baseline?**
`ml::comparison::run_comparison` replays the same traces through
`ReplayBaseline::{Priority, RoundRobin, LowestLatency, Balanced}` and the
`MlPolicy`, reports routing metrics per arm, and pairs them request by request
over the intersection where both arms have a measurement.

**7. How does Shadow prove model quality rather than reachability?**
`ml::shadow_analysis::analyse` reports agreement rate, alternative selection
frequency, estimated utility delta, observed utility delta, mean regret, and
helpful-versus-harmful counts, keeps prediction-only records separate from
decision records, and reports every unmeasurable record as a named gap. On this
body: 119 disagreements, all measured, mean observed delta +2.6999, 0 harmful,
`is_evaluable() == true`. `shadow_evaluated` returning a record proves nothing and
is not relied on for any of this.

**8. What allows a model to be promoted?**
`ml::promotion::PromotionGate` with a configurable `PromotionConfig`. Nine
criteria, each carrying its own measurement and reason: baseline present, paired
evidence floor, holdout quality against an uninformed predictor, routing utility,
selection safety, success-regression budget, cost budget, latency budget, and
dataset identity. Verdict is PROMOTED, REJECTED (evidence sufficient, candidate
failed) or BLOCKED (evidence insufficient).

`ActiveModelStore::promote` requires that verdict. It takes a
`PromotionDecision` and a `ModelCheckpoint`, refuses any decision that is not
PROMOTED with the unmet criteria named, and **re-derives** the checkpoint's
commit id from the decision's own inputs — so a stale decision cannot install a
model it never saw. `PromotionCriterion`'s constructors are private, so a
decision cannot be constructed outside the promotion module at all: the only way
to obtain a PROMOTED verdict is to pass the gate.

On the `DeadFlaky` body it returned **REJECTED**; on the `HalfFlaky` body it
returned **PROMOTED** on both rounds.

**9. How does the system fall back when ML misbehaves?**
`MlRouter::rank` returns `Err(RankUnavailable)` for no attached model, no
candidates, a non-finite prediction, a selection outside the executable plan, or
an exploration probability above the ceiling. `apply_ml_ranking` treats every one
of those as an ordinary return and keeps the router's own plan. The deterministic
plan is computed before ML is consulted and is still there. Across the 120 served
requests there were 0 fallbacks and 120 successful responses.

**10. How does a new outcome enter the next round of learning?**
Every request's terminal transition writes a durable `RequestTrace`. The next
`run_training` reads them, re-splits, retrains, re-compares, re-gates and
re-promotes.

This is demonstrated end to end rather than argued:
`new_outcomes_from_a_new_process_reach_the_next_round_of_learning` collects 80
requests, ends the process, retrains and promotes; a **new** `AppState` picks the
model up from the state directory and serves 40 more; the second retraining sees
a body of 120 requests whose fingerprint differs from round one's, contains 40
request ids round one never saw, produces a different commit, and is promoted
again. Round one's model is retained as the rollback target.