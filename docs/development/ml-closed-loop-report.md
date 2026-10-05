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

> **§E9 was written when promotion was not product-wired, and has since been
> fixed.** `POST /v1/ml/promote` now runs a round in-process and, with
> `?install=true`, puts the model on the live router. What this report has been
> measuring throughout was *test-reachable* capability rather than product
> capability — that distinction was the most important thing in the file, and it is
> the reason §E9 exists. Read it for the shape of the failure, not for the current
> state.

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
| `train_batch()` with no split | **Removed.** It was a bare loop reachable from outside the crate and called by nothing; `ml::learning` is the real training path, with a group-disjoint temporal split, frozen holdout, seed, dataset fingerprint and a verified checkpoint |
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
- `Coordinator` still exists alongside `DecisionEngine`, and now that the deletion
  has been argued properly the right answer is that it should. `DecisionEngine` is
  the only authority any production path reaches; `Coordinator` has no non-test
  caller. But it is not redundant: `decision_engine.rs`'s test module runs the
  frozen `Coordinator::decide` over the same bundles and asserts the engine
  reproduces it — action, reason, selection, and every utility term. The duplicate
  semantics are the *specification* the authority is measured against.
- `train_batch()` **has been removed.** It was `pub` inside a `pub mod`, so it was
  reachable from outside the crate, and nothing called it but its own tests.
  That combination is worse than dead code: a caller could have used it and shipped
  a model with no holdout. `ml::learning`'s module doc names the shape it had, so
  the contrast survives the removal.
- Removing it took four `*_learning_direction` tests with it — they had used
  `train_batch` as their training loop, so they stopped compiling and looked like
  collateral. They carried a real claim (each head moves its prediction toward the
  target) and nothing else covered it, so they are rewritten against `update`
  directly and strengthened: the success head is now tested in *both* directions, a
  negative cost target is pinned as refused, and `a_head_learns_the_input_and_not_only_its_bias`
  closes a gap the originals had. **A deletion commit has to diff the test
  inventory, not just the compiler output** — four tests stopping compiling reads
  exactly like collateral until you check what they asserted.
- `Coordinator` **stays**, and listing it as "a thing to delete" was wrong. It is
  the differential oracle the `DecisionEngine` is verified against.

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
| Reproducibility | **4** | ADR-0008: the split's tiebreak was a random UUID, so the verdict alternated 3-of-6 on identical traffic; now 6-of-6 identical |
| Cross-provider | **4** | `ml_multi_provider_test.rs`: three providers on three upstreams; unobserved provider received **0 of 60** requests with exploration off, and the learned ranking used **1 provider** against round-robin's 2 |
| Exploration reach | **4** | `MlRouter::explore_plan` reachable with no model; `blind_explorations` counted apart from `explorations`; stays inside the eligible set and the plan. **But reaching a candidate and being reliably promotable are in conflict** — at exploration 0.25 the gate refuses at 120 *and* 240 requests for want of paired evidence (§E6), and over 4 repeats per setting the first-round verdict decays monotonically: 0 promotes 4/4, 0.02 promotes 2/4, 0.10 promotes 0/4. A blind provider is also still reachable as a *fallback*, which is narrower than "unreachable". See §E6, §E11 |
| Regime change | **4** | `zroutery-headless --experiment`: collect → promote → exploit → degrade → relearn → adapt over 480 requests on the production router. The degraded provider must be one **actually in the rotation** — degrading an unreachable one produces a phase that looks like an improvement and measures nothing. Adaptation observed: the promoted model moved 120/120 requests onto the provider that survived, `rankings` 0→120, two distinct commit ids — §E10 |
| Promotion verdict reproducibility | **3** | Degrades **monotonically** with exploration over 4 repeats per setting: exploration 0 promotes 4/4 at paired 91 every time (and `balanced`'s replay utility has zero spread), 0.02 promotes 2/4, 0.10 promotes **0/4**. Round-2 promotes everywhere, so the coupling is specific to the smallest body. A single run at 0.02 or 0.10 can report either verdict. Marked 3 because the defect is characterised and bounded, not because it is fixed — §E11 |
| Learned vs replay baselines | **4** | 12 runs (3 exploration settings × 4 repeats × 480 requests). `ml.candidate` beats `priority` +2.00, `round_robin` +0.41..+1.08 and `lowest_latency` +0.69..+1.66 in **12/12** each; it ties `balanced` (−0.003..−0.007, winning 5/12 on sign) while being **cheaper in every configuration**. Qualified: arms are scored on their own measured subsets, not one body; `ReplayBaseline::Balanced` is not the production policy — §E12 |
| Cost axis | **4** | `Attempt` grew a cost field, populated at the terminal transition. 120 of 171 samples now carry a real cost; `mean_cost` separates across arms (`baseline.priority` 0.00300, `ml.candidate` 0.00702). `RewardPolicy::cost_weight` and the gate's cost budget read a real number — §E4. Separately, the experiment *fixture* had its own inert axis from a per-token/per-million unit error, invisible because a constant across arms looks like an unmeasured one; prices now come from CC Switch's real table and `ml.candidate` is consistently **cheaper** than `baseline.balanced` — §E10 |
| Holdout temporality | **4** | ADR-0008: the frozen holdout was a contiguous tail of a *content*-sorted vector, so it was a content cluster and its composition varied run to run. Now split in arrival order, ordered per partition — §E5 |
| Model identity | **4** | one identity per model (`TrainingOutcome::commit_record`); commit `06f7da3409a14147` |
| **Promotion is not product-wired** | **Closed.** `POST /v1/ml/promote` behind the same auth layer as `status`, `shadow` and `rollback`, backed by `AppState::ml_run_promotion_round`, runs the round in-process and — with `?install=true` — puts the model on the **live** router before responding. `install` defaults to **false**: judging and installing are separate acts, and a caller polling for a dashboard cannot promote by accident. Verified both ways by mutation — installing when not asked fails `asking_for_a_verdict_does_not_install_anything`, and skipping the live reload fails `asking_for_the_install_attaches_the_model_to_the_live_router` — §E9 | Two decisions remain and are **deliberately not made in code**: *when* a round runs (nothing here schedules; the caller's scheduler decides by choosing when to call) and *what an operator may hold a model to* (the caller names the **baseline**; the evidence floors — minimum paired requests, required utility delta, permitted regressions — are **not** caller-controlled, because relaxing those is not a claim about what to compare against, it is a decision to stop requiring evidence) |
| Product wiring | **3** | Everything below the promotion source is wired and correct: `ml` is a named default-on desktop feature, `<config_dir>/ml` is claimed, the durable pointer is read and a verified predictor attached at startup, `reload_active_model` reaches the live router. Marked down from 4 because the top of the loop is not — see the row above |; `ml_routing.enabled` asserted off by default, so no install's behaviour changed |
| Operator surface | **3** | `ml::MlStatus` read live and asserted to agree with `MlRouter`; `/v1/ml/{status,shadow,rollback}` behind the auth layer (401 asserted); the replay runs the *attached* model over real traffic, bounded; unreachable candidates are derived from config against the observation store and named, with the consequence stated rather than left to derive. Marked down from 4: correct, and reporting a **permanently empty** state — no attached model to describe, no shadow model to replay, no previous pointer to roll back to |
| Reversibility | **4** | rollback reaches the router in a live process, verified to fail against a pointer-only implementation; refused rollback changes nothing |
| Coordinator convergence | **4** | `DecisionEngine` is the sole authority; `Coordinator` is retained deliberately as the differential oracle it is verified against, not as a second decision path. Verified by mutation: changing the engine's switch rate limit from `>=` to `>` fails `cross_check_full_matrix` and `cross_check_switch_rate_limit_reached`, which run the frozen `Coordinator::decide` over the same bundles and compare action, reason, selection and every utility term |

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
| ~~Desktop app compiles no ML~~ | **Now closed (ADR-0007).** `src-tauri` depends on `zroutery-core/ml` through a named `default = ["ml"]` package feature, so the shipping desktop product contains the trace log, the model store and the gate. `--no-default-features` still yields a desktop app with no learning stack, and CI builds it | — |
| ~~No operator surface for model state~~ | **Now closed (ADR-0007).** `ml::MlStatus` reads live; `GET /v1/ml/status`, `POST /v1/ml/rollback`, `GET /v1/ml/shadow` behind the auth layer; `get_ml_status` / `get_ml_shadow` / `rollback_ml_model` Tauri commands; a panel on the Routing page with the gate's criteria, the promotion history and the replay's measured numbers | — |
| ~~Durable ML state is opt-in and nothing opts in~~ | **Now closed for the desktop app (ADR-0007).** `Desktop::new` claims `<config_dir>/ml` unless the document names one. Still open for the headless proxy, which has no app directory to claim | A state directory for the headless binary, which is a product decision about where a CLI keeps history |
| ~~No cross-restart retraining evidence~~ | **Now closed.** `new_outcomes_from_a_new_process_reach_the_next_round_of_learning` runs collect → process ends → retrain → gate → promote → new process serves → retrain again, and asserts round two's body contains round two's requests | — |
| One-candidate robustness unmeasured | **Partly closed (ADR-0008).** The first fixture configured two providers but gave models to only one. `ml_multi_provider_test.rs` runs three providers on three separate upstreams and measures candidate-set shape directly: `mean_eligible_candidates` 3.00, `distinct_providers` per arm. What remains open is cross-provider *promotion*, and the reason is measured: the comparator a candidate is named against can move its paired set from 2 to 40. See §E3 |
| The cold-start window is unbounded in principle | Every new process begins with an empty observation store, so the first requests fall back until history accumulates. Measured at 1–2 requests; not characterised as a function of traffic rate | Carry the observation store across restarts, or persist it |
| The cost axis is inert | **Closed.** `Attempt` had no cost field and `Targets::from_attempt` read `None` unconditionally, so per-attempt spend was never recorded, every attempt-scoped sample was cost-free, and `mean_cost` was `0.0000000000` for every arm — which made `RewardPolicy::cost_weight` and the gate's cost criterion both read a constant. Now recorded and discriminating — §E4 | — |
| A billed failure is unattributed | A 5xx carries no usage, so nothing in the error says what the failed call cost. If a provider bills errors, that spend is invisible to the ledger and to the cost axis | Carry usage out of the failure path — an upstream protocol change, not a routing one |
| The cost head is never checked | **Closed.** The head learns its targets at 10:1; the learned figure reaches the ranking utility; and both survive to a *served* decision through a real promotion, all verified by mutation. Separately measured: at the shipped weights a 9x price difference moves utility `0.0009` against `0.75` for a success difference, so the cost head **cannot outvote reliability** — pinned so the weights cannot be changed silently — §E4 |
| The frozen holdout was a content cluster | **Closed (ADR-0008).** `project_cohorts` sorted by `CohortOrderKey` and the holdout was a contiguous tail of that, so which decisions were held out depended on tie-breaking rather than on time; the gate refused a fifth of runs with `SingleServedCandidate` — §E5 | — |
| Exploration defaults to zero, which has a measured cost | **Closed as a report, and the cost turned out to be two-sided.** The unreachable candidate is named in `MlStatus::blind_candidates` and warned about in the panel. But raising `exploration_probability` to fix it **blocks promotion of any learned model**, so the advice would have been self-defeating; the warning now says so and recommends top priority instead — §E6 | Nothing, unless the promotion gate learns to compare on something other than request-identical pairing. That is a design change with a real cost: paired evidence is what makes the improvement claim honest |
| The shadow overhead gate was a wall-clock assertion | **Closed by reshaping, not widening.** `shadow_marginal_cost_per_candidate_does_not_grow` measures the *marginal* cost of a candidate at 4–8 and at 32–64 candidates, interleaved under one scheduler, and asserts it does not grow. Scale-free, so machine speed and load cancel. Verified by mutation: a quadratic regression scores 42.3us against 239.0us and fails; the old endpoint ratio scored that same mutation 3.6x against a bound of 12 and **passed**. `shadow_evaluate_absolute_cost` (`#[ignore]`) reads the magnitude for a human — §E7 | The shape is gated; a constant-factor slowdown is not, and cannot be without an absolute ceiling. That limitation is stated in the test rather than papered over |
| `state::tests::a_successful_rebind_commits_the_document_and_moves_the_listener` was intermittent | **Closed.** `advertised_port()` binds `:0`, reads the number, drops the socket — so the port is free when probed and can be gone by the time the gateway binds it. Four tests start on an explicitly probed port and run concurrently, so the window was taken about 1 run in 6. `with_ports_that_survive` retries on a bind that says `cannot bind`, which is the one string `ServerHandle::start` produces and both callers pass through verbatim — §E8 | — |
| Whether the port race is *eliminated* rather than tolerated | Retry, honestly. 8 consecutive clean full-suite runs after the change against a ~1-in-6 baseline is consistent with a large reduction and is **not** proof of elimination: the race depends on what else on the machine binds ports at the same moment | Reserving the port instead of probing it, which needs `ServerHandle` to accept a pre-bound listener. That is a real capability (port 0 in production, socket activation) rather than test scaffolding, so it is worth doing on its own merits — but it is production surface, not a test fix |
| The status field's meaning is only asserted, not measured | **Closed.** `blind_spot_warning_matches_what_exploration_actually_does` runs `explore` over 2000 ids at each probability and requires the document's verdict to match. Mutating `explore` to explore at probability 0 fails this test and **nothing else in the workspace** — the other 2128 pass with exploration silently running | — |
| Tauri command dispatch is not covered by a test | The status document, replay and rollback are exercised through `AppState` and over HTTP. Nothing drives them through a real webview | An integration harness around a webview, or accepting the seam and keeping `ml_available` so a failure there is reported as a failure rather than read as an absence |

Not blockers, deliberately excluded: cost axis coverage (the fixtures price ~20
tokens, which rounds to zero — closed in §E4 by recording per-attempt cost instead),
more baselines, and `ServerHandle` accepting a pre-bound listener (§E8's real fix).
`Coordinator` deletion and `train_batch` removal were on this list and are now
resolved — one deleted, one deliberately kept, for opposite reasons.

---

## E2. The operator surface (ADR-0007)

The loop was real and invisible. Four capabilities close that, and each replaces
a guess an operator would otherwise be making.

### Reading the state

`ml::MlStatus` is read live from the router, the stores and the gate. The
distinctions it keeps are the ones that would otherwise be guesses:

| State | What it reads as |
|---|---|
| No model, `enabled: false` | "ML routing off; routing is deterministic" |
| `enabled: true`, nothing attached | "no model is attached" — the misconfiguration, never "working" |
| Promoted, `enabled: false` | installed and deliberately inert |
| Attached and enabled | names the commit, the gate identity and the body it was judged on |

The gate decision travels whole, so "why is this serving" has an answer rather
than a verdict: every criterion, whether it held, the number it was decided on,
and the baseline that had to be beaten. Collection counters are counters and are
labelled as such — a count of collected samples says collection is happening and
nothing about quality.

`the_status_document_describes_the_process_that_is_actually_serving` asserts each
row, asserts that `attached.commit_id` equals `MlRouter::attached_commit()` (the
document and the router must not disagree), and round-trips the whole thing
through JSON, because a status that cannot be serialised is a status nobody reads.

### Replaying the serving model over real traffic

`AppState::ml_shadow_analysis(limit)` runs the same `analyse` the offline gate
runs, over the operator's own traces, against the weights actually ranking
requests. Bounded: the tail is read by walking backwards to the *n*-th line from
the end, and the limit is capped at 50 000 inside, because this is callable from
a button.

It reports why it could not run. "No history" and "no model attached" are
different answers and only the first is fixed by serving traffic:

```
no state dir      → traces_read 0,  reason: no durable state directory
history, no model → traces_read N,  reason: no model is attached to the router
model attached    → commit_id = attached commit, records = traces_read
```

### Removing a model that turned out to be wrong

`AppState::rollback_active_model` moves the durable pointer **and** reloads the
router from it.

`a_rollback_takes_effect_in_the_live_process_and_not_only_on_disk` promotes two
models into a *running* process and requires the router's attached commit to
follow the pointer forward and back, then drives 20 real requests through the
restored model to prove it ranks. It was verified to fail against a pointer-only
rollback, with the withdrawn model's commit still attached.

A pointer that cannot be read leaves whatever is attached alone and reports the
fault. A second rollback with no prior model is refused, says why, and changes
nothing.

### Being explicit about what the dashboard cannot see

`Snapshot.ml_available` declares build capability. To a webview a command that
was never registered and a command that failed are the same event, so without
the flag a broken IPC bridge renders as "this build has no ML stack" — which
looks like deliberate configuration rather than a fault.

`ml_routing.enabled` and `ml_routing.state_dir` are asserted to default to off
and empty, behaviourally, against a default-constructed **and** a
document-deserialised configuration. That is what makes building ML into the
desktop app a packaging change rather than a behaviour change.

---

## E3. Cross-provider, and what it cost to find out (ADR-0008)

Every earlier fixture routed between two models of **one** provider. Two providers
were configured; the second existed so the secret store had a key. The product's
actual claim — *aggregate several providers* — had no evidence behind it.

`ml_multi_provider_test.rs` runs three providers on three separate upstreams:
`alpha` serves a fast-unreliable and a slow-reliable model, `gamma` serves a fast
reliable one at nine times the price. `Priority` puts `gamma` last, so no cheap
deterministic signal ever prefers it.

### The deadlock

With exploration off and no model promoted:

```
mean attempts 1.759   alpha 105 (flaky 60 / steady 45)   gamma 0
ml.candidate   distinct_providers 1   mean_eligible_candidates 3.00
round_robin    distinct_providers 2
```

Three eligible candidates on every request. The learned ranking used one
provider. Round-robin — defined as spreading — used two. The learned ranking was
**worse than round-robin at discovering a provider nobody thought to try**, and
the body contained no outcome for `gamma` at all: no feature, no sample, no
fingerprint contribution.

The cause was structural. Exploration lived inside `MlRouter::rank`, which is
unreachable when no model is attached. So exploring required a model, and a model
requires evidence, and the evidence requires exploring.

`MlRouter::explore_plan` breaks it, and the same fixture then records
`blind_explorations 59` and `gamma 24 of 270 calls`, with the ranking crossing
providers.

### The coin

Chasing an unstable verdict found something worse. Six runs, identical fixture,
identical traffic:

```
  run  gamma calls  ml providers  ml measured  paired(priority)  verdict
    1            0             1           46                46  PROMOTED
    2            0             2            2                 2  BLOCKED
    3            0             1           46                46  PROMOTED
    4            0             2            2                 2  BLOCKED
    5            0             1           46                46  PROMOTED
    6            0             2            2                 2  BLOCKED
```

Alternating, so systematic. `split_samples` ordered groups by
`(timestamp, request_id)`; timestamps are second-resolution and request ids are
fresh UUIDs, so every group tied and **a random UUID decided which requests
landed in the frozen holdout**.

`dataset_identity` and `holdout_loss` are two of the nine gate criteria. A gate
that alternates on identical evidence cannot be a fact about the model.

The fix is the input's own position, which is durable because the trace log is
append-only. After it: **6 of 6 identical.**

Two tests pin it, and both were verified to fail against the old tiebreak:
`a_body_collected_within_one_second_splits_the_same_way_regardless_of_its_ids`
and `the_split_follows_the_supplied_order_rather_than_the_identifiers`.

### The problem that is characterised, not fixed

An unobserved candidate is predicted as its prior. Average beats a quarter, so on
this fixture the model reaches for the candidate it knows nothing about. Those
choices have no recorded outcome, so the request cannot be paired.

Which means **a model that genuinely prefers a different provider is harder to
promote than one that agrees with the incumbent** — and the requests that would
prove the model right are the requests nobody collected. Exploration breaks this,
and it has to be on *before* the promotion attempt.

The gate's behaviour here is correct: `BLOCKED (paired_evidence: 2 against a
floor of 30)`. The system is honestly reporting that it cannot measure what it
needs to.

### And the comparator is part of the measurement

Sharper than the point above, and measured afterwards. Five bodies at the
exploration ceiling:

```
  run  providers  paired: priority  round_robin  lowest_latency  balanced
    1          2               4           15              38        40
    2          2               6           11              14        31
    3          1              40           30               8         1
    4          2               2            9              17        34
    5          2               2            7               2        29
```

Run 3 is the clearest case: with **no** discovery the candidate pairs perfectly
with `baseline.priority` (40) and not at all with `baseline.balanced` (1). With
discovery, the reverse. Exploration reached the new provider by displacing
priority's first pick, so the candidate's measured requests and priority's are
nearly disjoint; a spreading baseline considers the same candidates and overlaps
them.

So `baseline.priority` is a **same-provider** choice. Naming it for a candidate
that discovered a provider tends to produce `BLOCKED (paired_evidence: N)` for a
candidate that may be perfectly good — and the blocker reads as "not enough data"
rather than "you compared against the wrong arm".

A sixth run broke even the anti-correlation (discovery *and* `paired(priority)`
at 31, one over the floor), so this is a strong tendency and not an invariant.
It is a diagnostic, not an assertion: a test that fails one run in three teaches
its reader to re-run it.

`pair_against_baseline` itself is verified to join on the right key, including
when two arms choose different candidates on every request. The collapse is a
property of the evidence, not a pairing bug — worth having checked, because the
symptom is indistinguishable from one.

---

## E4. The cost axis was inert

Every arm in every fixture has reported `mean_cost` as `0.0000000000`. That is not
a reporting-precision artefact.

`Targets::from_attempt` sets `cost: None` unconditionally, and it is not reading a
missing field: **`Attempt` has no cost field at all.** Per-attempt spend is never
recorded anywhere in a request's evidence.

```
171 samples, 0 carry a cost target, 171 do not
```

Three consequences, each a capability loss rather than a reporting nit:

1. Every attempt-scoped training sample is cost-free.
2. The routing comparison reads **only** attempt samples, so `mean_cost` is a
   structural constant for every arm.
3. So `RewardPolicy::cost_weight` contributes nothing to the observed utility the
   promotion gate reads, and the gate's cost-budget criterion is measured against
   zero.

The request-scoped sample beside it carries a perfectly good
`outcome.actual_cost`, and nothing consumes it. No test noticed, because every
test with a non-zero `actual_cost` builds the `Outcome` by hand, and the one
pipeline test comparing spend against the activity record passes just as happily
with both sides at zero — a ledger and a record agreeing about nothing.

### The fix

Adding `cost: Option<f64>` to `Attempt`, populating it in the terminal transition
from the settled usage, and reading it in `from_attempt` makes all three correct.
Measured:

```
alpha-flaky-std         n=17   0.00300000
alpha-steady-std        n=77   0.00088000
gamma-gamma-std         n=26   0.00792000

baseline.priority        mean_cost 0.0030000000
baseline.round_robin     mean_cost 0.0029502041
baseline.lowest_latency  mean_cost 0.0069360000
baseline.balanced        mean_cost 0.0071657143
ml.candidate             mean_cost 0.0070220690
```

Which immediately shows something that was invisible before: **the learned router
spends 2.3× what `baseline.priority` spends.** It buys its latency improvement
with money, and the gate's cost budget was structurally unable to see it.

> **Read this next to §E10, which measures the opposite sign.** The 2.3× here is a
> property of *this fixture's price structure* — priority points at an expensive,
> reasonably healthy provider here, and at the cheapest, most broken one there.
> The direction of the ML-versus-priority cost comparison is set by the provider
> layout, not by the model.

### What it broke, and what that turned out to be

It broke `the_candidate_reaches_a_named_verdict_over_real_request_evidence` with:

```
DegenerateHoldout { partition: Holdout, reason: SingleServedCandidate,
                    detail: "only alpha/alpha-std-one ever served" }
```

That test passes on the unmodified tree four runs out of four, and its recorded
window alternates two served identities perfectly across all thirty decisions.

Instrumenting the split showed the projection arriving as **14 fast, then 15 std,
then 1 fast** — clustered by content — and `split_at(18)` producing a holdout of
**11 std and 1 fast**. So the "temporal" holdout was a content cluster.

Then the interesting part: the holdout's composition **varied between runs**,
holding one identity in five runs out of six and six in the other. So the
tie-breaking was random, which means `sort_by`'s stability was *preserving* the
randomness rather than removing it. The content key makes the **statistics** of a
partition invariant to tie permutation; it says nothing about **which** cohorts are
in the partition, and that is decided by position.

So the fix was not to the cost axis at all — it is §E5. With it, the cost fix
lands and the gate test passes ten runs out of ten.

---

## E5. The frozen holdout was a content cluster

`project_cohorts` sorted cohorts by `CohortOrderKey`, and `run_calibration` took
`cohorts.split_at(len - holdout_cohorts)` as the frozen holdout.

The sort is not a mistake. The module documents at length why it exists: two
cohorts that tie on the key are indistinguishable to every sum it computes, so
exchanging them cannot move a gradient, a log-loss, a reliability bin, a drift
bin, the base rate or the arity. All of that is true.

It was applied in the wrong place. **A partition is not a sum.** Which cohorts are
held out is decided by position, and position in a content-sorted vector is a
function of tie-breaking rather than of history. So the frozen holdout was a
content cluster, and `DegenerateHoldout { SingleServedCandidate }` was the gate
correctly refusing a partition it had been handed.

The fix splits in **arrival** order and orders **each partition** on content
afterwards. Both properties then hold and neither is traded away:

- the holdout is a later slice of history;
- the computation within a partition is invariant to tie permutation.

`group_attempt_rows` already walked the snapshot's rows by index, so arrival order
was available and deterministic all along.

### The contract changed, and a test encoded the old one

`uuid_invariance_the_partition_and_every_number_survive_a_uuid_change` reversed
the second rendering's rows and asserted nothing changed, reasoning that "row
arrival order is not the content order either".

But `render` stamps `BASE_TIMESTAMP + index`, so reversing the rows reverses the
*history*. It is not one dataset reordered; it is the same measurements running
backwards, and a later slice of that is a different slice. The reversal is gone,
the test asserts what its name says — identifiers do not matter — and row-order
dependence is asserted explicitly beside it by
`projection_is_in_arrival_order_not_content_order` and
`the_holdout_is_the_later_slice_rather_than_a_content_cluster`.

**Not claimed:** that this ever produced a bad promotion decision. It produced a
gate that refused roughly a fifth of the time for a reason that had nothing to do
with the model. That is a false negative rather than a false positive — the safe
direction — but it would have blocked a release on unrelated grounds.

### What "cost" means here, and one suspicion that was wrong

`Targets::from_attempt` prices an attempt from what that attempt's own settlement
carries. Two consequences worth stating precisely:

- **A failed attempt with no reported usage is unattributed, not free.** A 5xx
  carries no usage, so there is nothing to price. `None` is the honest answer; zero
  would claim the call was free.
- **A failed attempt that _did_ report usage is priced.** A stream that ends
  mid-answer after the upstream reported tokens produces
  `Error::InterruptedStream { usage }`, the streaming path keeps that usage
  explicitly, and it reaches the same settlement — so the attempt carries the cost
  it incurred. `a_truncated_stream_that_reported_usage_is_charged_to_its_attempt`
  asserts the attempt figure and the request total agree, and was verified to fail
  with exactly that message when the per-attempt pricing call is removed.

I suspected the all-failed buffered path, which passes `Settlement::default()`,
was dropping spend, and went looking. It is not a defect: a buffered HTTP failure
carries no usage to drop, and the stream path — the only place usage can exist on a
failure — already preserves it. Three probes before reporting something turned out
to be wrong is cheaper than one wrong commit.

### Whether the cost head learns anything

**Yes — and it cannot outvote reliability. Both are pinned by
`crates/zroutery-core/tests/cost_head_test.rs` and two tests in `ml/serving.rs`.**

Six tests, because three questions were conflated by "the cost axis is inert": does
the head learn its targets, does what it learned reach the utility, and does it
survive all the way to a *served* decision.

| Test | Claim | Fails if |
|---|---|---|
| `the_cost_head_learns_the_targets_it_is_given` | Features fixed, cost varied 10x; predictions track it at 10:1 and the head reports its sample count | The cost branch of `update_all` is removed |
| `a_learned_cost_reaches_the_ranking_utility` | The utility gap between two bundles equals `cost_weight ×` the predicted-cost gap, with the other three terms identical | The cost branch is removed, **or** the cost term is dropped from `compute_utility` |
| `samples_without_a_cost_leave_the_cost_head_untouched` | A cost-free body trains the cost head zero times while the success head trains normally | The per-head gate in `update_all` is removed |
| `cost_cannot_outvote_success_at_the_shipped_weights` | Nine times the price moves utility orders of magnitude less than a success difference | The normalisation or the weight changes |

Each was verified by mutation. Disabling `update_all`'s cost branch fails the first
two; zeroing the cost term in `compute_utility` fails the second and fourth. The
second test needed an explicit "the two predictions differ" assertion placed *first*
— without it the algebraic identity holds as `0 == 0` and the test passes with the
cost axis disconnected, which is exactly the failure mode it exists to prevent.

### The serving seam had no coverage at all

The four tests above are component-level, and that turned out to matter. Zeroing
`bundle.cost` inside `ActivePredictor::predict` — between the trained ensemble and
the ranking — left **all 2129 tests in the workspace green**. The cost head is
trained, checkpointed, loaded and bundled correctly; the cost term is three
orders of magnitude below the success term; and `RankedPlan` reports only *total*
utility. A serving path that discards cost entirely therefore produces
byte-identical rankings, and nothing in the repository could see it.

Two tests in `ml/serving.rs` close it:

| Test | Claim | Fails if |
|---|---|---|
| `a_promoted_model_delivers_the_cost_its_head_learned` | A model promoted through the real gate and `ActivePredictor::load` reports the trained cost, and reports it *exactly* — not a plausible reconstruction | Cost is dropped or replaced between the ensemble and the ranking |
| `the_reported_utility_carries_the_cost_the_model_predicted` | Each candidate's reported total equals `compute_utility` over the bundle the ensemble produces for it | Any link in the chain breaks, including the reported number itself |

Both fail, and are the only failures, under the mutation that previously passed
everything. The second is deliberately a **per-candidate** fidelity check rather
than a comparison between two candidates: the other three heads converge towards a
constant but not to exactly one, and checking each candidate against its own
expectation makes that drift cancel instead of needing a tolerance wide enough to
hide a real cost regression.

Persistence needed no test of its own: `ModelCheckpoint::cost` is a non-`Option`
field with no `Default`, so losing the cost head at checkpoint time does not
compile. The gate-and-load route in the first test covers it anyway.

**The confound these tests are shaped around.** The obvious end-to-end check is
worthless: in the routing fixtures a candidate's cost is a deterministic function
of *which model it is*, so a head that learned nothing about cost but correlated it
with whatever features happen to separate those two models — priority, tier,
observation statistics — would score identically. These tests hold features fixed,
vary only the cost target, and never go through model identity.

**The answer, as a number.** `compute_utility` scores cost as
`-cost_weight * min(cost_dollars, 1.0)`. A per-request LLM bill is cents, so the
clamp never bites and the term is `0.1 × dollars`. The dearest provider on the
three-provider fixture costs 9× the cheapest — `0.00792` against `0.00088` — and the
whole utility consequence is `0.0009`, against `0.75` for a candidate that succeeds
where another fails.

So the cost head works, is wired in, and **has never once changed a routing
decision**. Not because it is broken, but because at the shipped weights a price
difference is three orders of magnitude less consequential than a reliability
difference. That may be a sensible default, but it is a policy nobody has
explicitly made, so it is pinned: anyone who wants cost to matter has to change
`RewardPolicy::cost_weight` or the normalisation, and
`cost_cannot_outvote_success_at_the_shipped_weights` fails until they argue for it
in the open.

**Two incidental findings, both asserted.** `Prediction::cold` is constructed
nowhere except its own test — every head reports `trained`, so the flag is `false`
even on a wholly untrained head. Cold-ness does reach the decision, but through
`confidence` (below 20 samples it drops to 0.1), which `compute_utility`'s
uncertainty term reads. And an untrained cost head predicts `0.01`, not zero, so it
looks like a *cheap* candidate rather than an absent one.

## E6. Exploration and promotion are in direct conflict

Found by running a diagnostic the repository already contained. It is the opposite
of what §E3 recommends, and it made advice shipped one commit earlier wrong.

**The measurement.** Sweeping traffic volume against exploration probability on the
three-provider fixture, then asking the gate for its own reason rather than
inferring one:

| requests | exploration | ml measured | priority measured | paired | verdict |
|---|---|---|---|---|---|
| 120 | 0.00 | 91 | 120 | 91 | **PROMOTED** |
| 120 | 0.25 | 10–40 | 81–87 | 2–15 | BLOCKED |
| 240 | 0.00 | 181 | 240 | 181 | **PROMOTED** |
| 240 | 0.25 | 32–41 | 181–192 | 6–10 | BLOCKED |

and the gate names its own blocker:

```text
BLOCKED paired_evidence: 2 paired requests against a floor of 30
```

At zero exploration the learned policy picks the provider the plan always tries
second, which was attempted on 90 of 120 traces, so 91 requests pair 1:1 against
`baseline.priority`. At 0.25 the paired count collapses by at least 6×.

**Doubling the traffic does not recover it.** At 240 requests with exploration 0.25
the paired count was 6, 8 and 10 across three runs — under the floor of 30, and not
trending toward it.

**The mechanism, from the code rather than from inference.** `ArmRecord::measured`
is false exactly when the policy's choice was not among the candidates that trace
attempted, and `pair_against_baseline` skips any request where either arm is
unmeasured. Exploration routes a fraction of requests to a provider the
baseline's pick never displaced, so on those requests the baseline has no outcome
to pair against. Once the body has trained, the policy prefers the explored
provider, whose attempt count is set by the *exploration probability* rather than
by traffic — which is why more requests do not help.

What is **not** measured: which candidate the learned policy actually chooses.
`RoutingComparison` reports aggregates only, and adding per-request choice records to
it would be production surface bought for a test, so that link in the chain is
argued from the code rather than observed.

**Why this is not a bug in the gate.** Paired, request-identical evidence is
precisely what makes an improvement claim honest: you cannot know how a model would
have done on a request whose outcome nobody recorded. A gate that accepted unpaired
evidence would be accepting a guess. The tension is real and the gate is right about
it.

**What it cost us.** The blind-spot warning added in `1933853` told an operator with
an unreachable provider to "raise `ml_routing.exploration_probability` above 0".
Acting on that trades an unreachable provider for a permanently unpromotable model:
routing stays deterministic either way, and now the operator has spent real money
on exploration to get there. The warning now states the cost and recommends top
priority instead, which reaches the provider *and* keeps promotion possible.
`blind_spot_test.rs` asserts the warning says "blocks promotion" — a warning that
recommends the option that does not work is worse than no warning, because it gets
acted on.

**What is pinned.** `exploration_starves_the_evidence_a_promotion_needs` asserts the
direction (paired collapses by ≥4×) and the blocker's name. It deliberately asserts
no count: the counts vary 4× and 7× run to run, so an exact figure would be a flaky
test asserting noise. The *verdict* was PROMOTED at 0 and BLOCKED at 0.25 in every
run observed, and the test passed 8 of 8.

## E7. A performance gate that could not tell a slow machine from a slow regression

`shadow_overhead_stays_an_order_of_magnitude_under_budget` asserted p95 ≤ 10ms and
p99 ≤ 30ms against a path that measures tens of microseconds. It failed roughly one
run in eight, always under concurrent load, never in isolation — a preemption
reported as a regression.

The previous author diagnosed it correctly and declined to fix it, saying that
loosening would not help and naming two real options. Both of those turn out to be
the wrong frame. The problem is not the threshold; it is that **a fixed ceiling
cannot distinguish a slow machine from a slow regression.** Any ceiling loose enough
not to fire on a loaded machine is too loose to catch a 10× regression, and any
ceiling tight enough to catch one fires on a busy one. No value of that number works,
so tuning it was never going to.

**Measured shape of the path.** Cost is linear at about 20µs per candidate plus a
~15µs fixed cost:

```text
candidates      1      2      4      8     16     32     64
p50          34.8   53.1   95.3  169.4  324.1  639.9 1298.9   us
per cand.    34.8   26.6   23.8   21.2   20.3   20.0   20.3   us
```

The per-candidate cost converges to ~20µs and stays flat to n=64. Nothing is wrong
today — but that flatness is the thing worth guarding, and a ceiling cannot guard it.

**The tempting fix that does not work.** An endpoint ratio (8 candidates against 1)
looks scale-free and therefore load-independent. It is not sufficient: because the
curve is linear with a large fixed term, a *quadratic* term small enough to be
invisible between n=1 and n=8 fits that data exactly. Verified by mutation — putting
a pairwise comparison where the sort belongs gives:

```text
  4c p50=  137us    8c p50=  306us    32c p50= 2818us    64c p50=10016us
```

As an endpoint ratio that is `306/84 = 3.6×`, which passes any bound loose enough to
survive load. **The old gate would not have caught a 5× regression at 64 candidates,
and neither would a wider one.**

**What replaced it.** `shadow_marginal_cost_per_candidate_does_not_grow` measures the
*marginal* cost of a candidate — differentiating the curve divides out the fixed
per-request term — at 4–8 and at 32–64 candidates, and asserts it does not grow. The
same mutation reports 42.3µs against 239.0µs and fails; the real code reports 17.7µs
against 19.9µs and passes. The four arms are measured **interleaved in one loop** so
all see the same scheduler, and compared on **medians**, which an occasional
preemption cannot move.

**What it does not catch, stated plainly.** A *constant-factor* slowdown. Measured, by
mutation: a duplicated prediction per candidate moved the 1-to-8 ratio from 4.69 to
4.83, which is nothing, because doubling per-candidate work doubles the whole curve
and leaves its shape alone. Catching that requires an absolute ceiling — the flaky
thing this replaced. So the shape is gated and the magnitude is *measured*:
`shadow_evaluate_absolute_cost` is `#[ignore]`d and prints p50/p95/p99 at 1, 4, 16 and
64 candidates for a human to read.

## E8. A port that was free when probed, and gone when bound

`a_successful_rebind_commits_the_document_and_moves_the_listener` failed about one
full-suite run in six, always under concurrent load, never in isolation.

**It is not the same defect as §E7, and my first guess that it was was wrong.** That
one was a wall-clock ceiling; this one is a resource collision.

`advertised_port()` probes by binding `127.0.0.1:0`, reading the number the OS
assigned, and **dropping the socket**:

```rust
let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
let port = listener.local_addr().unwrap().port();
drop(listener);          // <- the port is released here
if port >= 30_000 { return port; }
```

The port is free when probed. Between that and the gateway's `bind` there are two
`await`s — `desktop.start()`, and for the rebind test a full migrate — and in that
window another test in the same binary can be handed the same port by the OS and take
it. Four tests here start on an explicitly probed port, and the test harness runs
them concurrently, so the window is taken often enough to be visible and rarely
enough to look like noise.

**Scope, which is smaller than it looks.** Only four of the six users of
`advertised_port` are affected: the default configuration port is `0`, so
`a_refused_address_is_not_persisted_while_stopped` never binds, and
`a_save_racing_a_stop_leaves_one_consistent_state` already tolerates either outcome
of its target bind.

**The fix is retry, and the reason it is retry rather than something cleverer is
worth stating.** The port genuinely is free at probe time, so a lost race is exactly
that — a race. The alternative framings were both rejected on their merits:

- *A fixed port.* Worse. It collides with anything else on the machine and cannot be
  reasoned about.
- *Configure port 0 and read the resolved port back from `bound_addr()`.* This does
  remove the race entirely, and `bound_addr`'s own documentation says port 0 resolves
  to a real port. But these tests exist to check that **the committed document names a
  port the listener really serves** — `store::save` persists the configuration as
  given, so a port-0 document stays 0 and the assertion becomes vacuous. That is the
  property this file is for.

`with_ports_that_survive` therefore retries while the failure is a bind failure. The
discriminator is one string from one place: `ServerHandle::start` is the only code
that binds and it reports `cannot bind {addr}: {os error}`, which reaches
`Desktop::start` directly and is embedded verbatim by `migrate_listener` in its
"previous gateway was restored" note.

**The retry is proven, not assumed.** `the_port_race_retry_retries_only_port_races`
covers three cases: a port lost twice is retried and then succeeds with the body
having run exactly three times; a failure that is *not* a port race is reported
immediately with the caller's message intact; and — the case that matters most — a
refusal that two of the sibling tests assert on never enters the retry loop. Verified
by mutation: making the helper not retry fails the first case.

**What is not claimed.** 8 consecutive clean full-suite runs, against a ~1-in-6
baseline, is consistent with a large reduction and is not proof of elimination — the
race depends on what else on the machine binds ports at the same moment. Eliminating
it means reserving the port rather than probing it, which needs `ServerHandle` to
accept a pre-bound listener. That is a genuine capability (port 0 in production,
socket activation) rather than test scaffolding, so it is worth doing on its own
merits; it is just not a test fix, and it is not taken here.


## E9. The loop was closed in code and open in the product

**This gap is closed.** The section is kept because the *shape* of the failure is the
most useful thing this report found, and because the fix's design decisions are only
intelligible against it. The title is past tense deliberately.

Found while chasing a much smaller question. I went to add a per-candidate choice
histogram to `ArmMetrics` to close §E6's unmeasured link, checked who reads
`ArmMetrics`, and found that `run_comparison` has **no production caller** — only its
own tests and the test harnesses. Following that thread is the whole finding.

**Every mechanism on the path exists, is exercised, and is mutation-verified.**
Outcomes are recorded per attempt with their cost. The trace log is durable,
atomic and fingerprinted. `run_training` fits a model on a group-disjoint temporal
split with a frozen holdout. `run_comparison` replays it against four executable
baselines. `PromotionGate::evaluate` judges it on nine named criteria.
`ActiveModelStore::promote` installs it behind a verified commit id.
`AppState::new` reads the durable pointer on the next start and attaches a verified
predictor. The closed loop is real.

**No running build can reach any of it.** Every call to `ActiveModelStore::promote`,
every `PromotionGate::new`, every `run_comparison` and every `run_training` in the
repository sat inside a `#[cfg(test)]` module or a `tests/` binary:

| File | test module starts at | promotion calls |
|---|---|---|
| `ml/promotion.rs` | 573 | 667+ |
| `ml/serving.rs` | 1159 | 1404–2011 |
| `ml/comparison.rs` | — | tests only |

`src-tauri` contains no reference to `promote`, `PromotionGate`, `run_comparison` or
`run_training` — the only hits are doc comments saying "promoted". `zroutery-headless`
has none either. The HTTP surface offers `/v1/ml/status`, `/v1/ml/shadow` and
`/v1/ml/rollback`, and nothing that promotes. The only caller of
`AppState::active_models()` is a test.

So in a shipped build:

- the durable pointer is never written, so **`MlStatus::active` is always `None`**;
- **`MlRouter::is_attached()` is always `false`**;
- `apply_ml_ranking` always takes its no-model branch, so the model-ranking path is
  unreachable and only blind exploration can move a plan;
- `ml_shadow_analysis` has no attached model to replay and `rollback` has no previous
  pointer to return to;
- **`ml_routing.enabled` is inert.**

The sink is wired and the source is missing.

### The mechanism was missing too, and now is not

There was a second gap underneath the first, and it explains the first. The
*sequence* had no single home, so **every caller re-assembled it by hand** — and two
hand-rolled copies survived, both in test files:

| | `loop_over` (`ml_closed_loop_test.rs`) | `learn` (`ml_multi_provider_test.rs`) |
|---|---|---|
| load → dedupe → train → compare | yes | yes |
| shadow analysis | yes | **no** |
| gate | **no** | yes |
| install | **no** | yes |

Neither was obliged to do what the other did. Neither was the copy that runs. Two
copies is also two places for the spine to drift.

`ml/round.rs` is that spine, once: `run_promotion_round` reads the durable log, dedupes,
fits, replays against every baseline, analyses the counterfactual and puts the result
to the gate. Installing is a **separate call**, `PromotionRound::install`, because a
round you can run without mutating the store is how you find out *why* a model was
refused. It is mechanism, not policy — it does not decide when to run or on whose
authority.

Both hand-rolled copies now call it, and **all 16 tests across the two files pass
unchanged** against it, which is the evidence that it does what they did.

**What this does and does not change.** The gap in §E9's headline table is still open:
nothing runs a round. But it is now a *narrower* and more precise gap — one missing
call rather than a missing mechanism — and the tripwire says so by name:

```
no_running_module_starts_a_promotion_round
```

It fires on a call to `run_promotion_round` outside a test module, and its failure
message asks the question that actually matters next: *who decided to run it?*

**One honest note on the new tests.** `running_a_round_that_cannot_run_touches_nothing`
asserts the error path — no pointer, no audit entry — and its doc comment originally
claimed it verified that a *successful* round does not install. It does not: an empty
log returns before there is a verdict. The property is caught by
`new_outcomes_from_a_new_process_reach_the_next_round_of_learning`, verified by giving
`run_promotion_round` an install side effect and watching that test fail while this
one stayed green. The comment now says which half is which.

### What this says about the rest of this report

Two matrix rows were over-reporting and are corrected: **Product wiring** 4 → 3 and
**Operator surface** 4 → 3. The second is the more uncomfortable one — the operator
surface is *correct* and reports a permanently empty state, which is the worst
combination: it looks like a working feature.

The rows that remain at 4 are component claims, and they are still true. What this
report has been measuring, without saying so, is **test-reachable capability**. That
is precisely the failure the original brief named — treating struct, trait and
unit-test as ML capability complete — and I committed it for several rounds while
adding tests to the components. The tests were worth adding; the claim that they
meant the loop was live was not.

### Why this was invisible

Every gate in the exercise was closed by a test, and every one of those tests
builds its own harness: `learn()` in `ml_multi_provider_test.rs` trains, compares,
gates and promotes in-process. So the loop is closed *end to end* in the test suite
and open *at both ends* in the product, and no test that existed could tell the
difference — because each was exercising the same harness rather than the product.

The tripwire added with this finding is deliberately a **characterisation**, not a
wish. `promotion_reachability_test.rs` asserts both halves of today's truth — no
running module names the promotion sink, and a maximally-configured product still
has no attached model — so the gap is a fact in the suite rather than an impression
in a document. It is written to **fail when the gap closes**, with a message saying
what to update, so that wiring promotion cannot leave a test that quietly stopped
describing the product. Verified by mutation: adding a gate construction to
`server/mod.rs` fails it with the offending line quoted.

### What closed it

**An HTTP endpoint on the running proxy, not a CLI.** That was not a preference. Only
in-process code can attach a model to the *live* router: `reload_active_model` acts
on this `AppState`, and nothing watches the pointer file, so a model installed by a
separate process would sit unread until a restart. The entry point is where the
mechanism forces it to be.

So the three product decisions resolve as follows, and only the first is genuinely
free:

1. **Where** — `POST /v1/ml/promote`, in-process. Forced by the fact above.
2. **On whose authority** — the caller, made explicit. `install` defaults to
   **false**, so `POST /v1/ml/promote` on its own reports what the gate would decide
   and changes nothing. That makes the endpoint safe to poll and safe to point a
   dashboard at, and it is the reason the default is not `true`: a promotion changes
   what every later request is served by and should be something an operator asked
   for. With `?install=true` the model is on the live router *before the response is
   written*.
3. **When** — **deliberately not decided in code.** Nothing here schedules, retries
   or holds a cadence. An operator's own scheduler decides, and it decides by
   choosing when to call.

The one product surface that *was* added is the baseline. "Beats a baseline" is a
claim about a specific baseline, so `?baseline=baseline.priority` names one rather
than letting a promotion quietly pick whichever it happened to win against. **Only
the baseline is caller-controlled.** The evidence floors — minimum paired requests,
required utility delta, permitted regressions — stay at their shipped values,
because relaxing those is not a statement about *what* to hold a model to; it is a
decision to stop requiring evidence at all.

And the report of what the learned policy actually *chose* now rides along:
`PromotionRoundStatus::policy_choices` is a per-candidate histogram, so an operator
reading a promotion can see a policy that named one candidate on every request — a
model that has not learned a ranking — which `distinct_providers: 1` cannot
distinguish from sensible traffic.

**Both directions are mutation-verified**, which is the part worth noting. Installing
when the caller did not ask fails `asking_for_a_verdict_does_not_install_anything`;
skipping the live reload fails `asking_for_the_install_attaches_the_model_to_the_
live_router`. A one-sided test would have left the flag decorative.

**One test that was vacuous until it wasn't.** The first version of the safety test
passed under a mutation that installed anyway — because the *default* gate refuses
this fixture, so the install branch was never reached. It now asserts
`gate_authorised()` first, which failed with `BLOCKED` and pointed at the real
cause: at 60 requests this fixture cannot produce a promotable model, which is §E6's
traffic floor behaving correctly. Two pre-existing tests and one diagnostic had their
traffic silently doubled by an unscoped string replace in the same edit; all three are
restored, and the lesson is that a scoped change made unscoped reads exactly like
correct work until the diff is read.

### What closing it actually requires

Not more machinery — every mechanism is built and proven. It requires deciding:

1. **When** does a retrain happen? On an interval, on a sample-count threshold, on
   operator request, on idle?
2. **Who** authorises the promotion? The gate already judges it; something has to
   decide to *run* the gate without a human pressing a button.
3. **What is the entry point** — a scheduled task inside the desktop app, a CLI
   subcommand, an HTTP endpoint, or all three? `zroutery-headless` exists and is the
   natural home for the first two.

Those are product decisions, and they are the reason this is recorded rather than
implemented. The one thing that should not happen is what would happen by default:
leaving the loop closed in tests and open in the product while the matrix says 4.

### Still not measured

- **A failover chain's billed failures.** If a provider bills a 5xx, that spend is
  invisible to the ledger *and* to the cost axis, because nothing in the error
  carries the tokens. Fixing it means getting usage out of the failure path, which
  is an upstream protocol change rather than a routing one.
- ~~**Which candidate a learned policy converges on.**~~ **Closed.** `ArmMetrics`
  gained `selections`, a per-candidate histogram, and `PromotionRoundStatus::
  policy_choices` surfaces it on the promotion endpoint. `distinct_providers` counted
  the identities and threw them away, which cannot distinguish a policy that sensibly
  split traffic from one that named the same candidate every request — and the second
  is a learned model that has not learned a ranking. Declined twice before it was
  added, both times correctly: there was no product surface to read it on until
  `/v1/ml/promote` existed.

---

## E10. The fixture's own cost axis was inert, and it was a unit error

§E4 made the cost axis real in the product. The headless experiment harness then
reported `mean_cost = 0.000000` on **all five arms**, and the cause was in the
harness, not the product.

`Pricing::cost_of` divides by `1_000_000`. The fixture's prices were
`0.000_000_5` and `0.000_001_5` — written as if per *single token*. A 400-token
prompt therefore cost `2e-10`, and six decimal places printed it as zero.

Nothing failed. No test caught it. The axis was simply absent from every
comparison, which is the same failure shape as §E4 one level up: a measurement
that is structurally constant across every arm looks exactly like a measurement
that has not been taken yet.

### Prices now come from real traffic

CC Switch's own `model_pricing` table, for models that actually served its
requests — chosen so the cheapest and the best are different providers, which is
what gives the axis something to decide:

| provider | model it copies | input | output | cache read |
|---|---|---|---|---|
| `alpha` | deepseek-v4-flash | 0.15 | 0.6 | 0.003 |
| `bravo` | glm-5.3 | 1.4 | 4.4 | 0.26 |
| `charlie` | claude-opus-4-8 | 5.0 | 25 | 0.5 |

Cache reads are modelled because they dominate real cost: observed traffic ran
**360×–595×** more cache reads than fresh input. The cache read price is also set
explicitly, since `Pricing::new` leaves it unset and bills every cache read at
the fresh input rate — which would have overcharged by more than an order of
magnitude, and *unevenly across providers*, inventing a cost difference the price
table does not contain.

Price also moved off `Behaviour` onto the provider, where it belongs: a provider
that degrades does not get cheaper. `Behaviour::FastReliableExpensive` existed
only to carry a price and is gone.

### What the cost axis changed

| exploration | arm | `mean_cost` | utility |
|---|---|---|---|
| 0.0 | `ml.candidate` | **0.029029** | 0.9766 |
| 0.0 | `baseline.balanced` | 0.029862 | 0.9834 |
| 0.02 | `ml.candidate` | **0.033544** | 0.9736 |
| 0.02 | `baseline.balanced` | 0.040763 | 0.9876 |
| 0.10 | `ml.candidate` | 0.043990 | **0.9818** |
| 0.10 | `baseline.balanced` | 0.044198 | 0.9817 |

With cost at zero, `ml.candidate` was consistently a little *behind* `balanced`.
With cost live it is consistently **cheaper**, at a utility gap of 0.0001–0.0140
— 17.7% cheaper at exploration 0.02. So part of the earlier shortfall was the
missing dimension rather than the model.

And one result worth stating on its own: **`baseline.priority` is 26–40× cheaper
than every other arm and has the worst utility in the set** (−1.0252), because it
routes every request to the provider that fails three times in four. A pure
cost-minimising objective picks the worst router available.

### This reverses §E4's direction, and the reason matters

§E4 measured the opposite sign in the multi-provider fixture: the learned router
spent **2.3×** what `baseline.priority` spent. Here it spends **26× less**.

Both are correct, and the difference is not about the model. It is about which
provider a static priority order happens to point at:

- In §E4's fixture, priority's first choice was expensive and reasonably healthy,
  so beating it on latency meant spending more.
- Here, priority's first choice is the **cheapest and most broken** provider, so
  beating it means refusing to use the cheap one.

So "the learned router costs more than priority" is **not a property of the
learned router**. It is a statement about the price/quality layout of the provider
set, and it flips sign with the layout. Any claim of the form *ML routing is more
expensive* needs the price structure stated alongside it, or it is not a finding.

### Verified by a cross-check, not an assertion

`baseline.priority` routes only `alpha`, so it prices at exactly `0.001113` —
which is `0.15 × 4000 + 0.003 × 11000 + 0.6 × 800`, per million, computed by
hand. That one number exercises the pricing table, the cache-read path, the
subset invariant and the OpenAI usage decoder simultaneously.

### The two tools disagree about what `input_tokens` means

`ir::Usage` documents `cache_read_tokens` as a **subset** of `input_tokens`, and
`protocol::openai::decode_usage` clamps to enforce it.

CC Switch's `input_token_semantics` has three values across its rows — 17,836 /
4,297 / 74. In the 81% group, cache reads run **595×** the input count, i.e.
`input_tokens` **excludes** cache reads. That is the opposite convention.

Feeding those rows to `fresh_input_tokens()` — a saturating subtraction — drives
fresh input to zero, so **the fresh-input term is silently dropped entirely**.
Any cross-tool cost comparison has to reconcile this first, and "reconcile" cannot
mean "assume the Zroutery convention", because the majority group uses the other
one. Recorded in the data-collection guide as well, since anyone importing CC
Switch data will hit it.

---

## E11. The blind spot is narrower than "unreachable", and exploration is not the fix

Two claims this report previously leaned on did not survive measurement.

**The blind spot is not "never reached".** With exploration at 0, `charlie` was
never a *first* choice — `blind_candidates = 1` — but it was still reachable as a
**fallback**. Once `bravo` degraded, the deterministic chain walked to it and it
served 120 of 120 requests. So the accurate statement is: a provider can be
permanently excluded from *preferred* traffic while remaining available as
fallback. Every earlier attempt to test a regime change failed because the
degraded provider was neither preferred nor a fallback target.

**Exploration does not fix it without breaking something else.** Exploration is
the only mechanism that can route to a candidate the plan does not already pick,
and it does produce discovery — `blind_candidates` goes to 0, and the learned
policy then prefers the newly-reachable provider. But the promotion gate's
verdict degrades **monotonically** with exploration probability. Four repeats per
setting, same configuration each time:

| exploration | round-1 verdict, 4 repeats | round-1 paired | round-2 |
|---|---|---|---|
| 0.00 | PROMOTED **4/4** | 91, 91, 91, 91 | PROMOTED 4/4 |
| 0.02 | **2/4** PROMOTED | 28, 3, 89, 42 | PROMOTED 4/4 |
| 0.10 | **0/4** PROMOTED | 7, 2, 11, 15 | PROMOTED 4/4 |

Two things follow, and the first is a retraction.

**It is not a coin flip above 0 — it is a monotone decay.** Exploration 0 is
deterministic to the digit: paired 91 four times out of four, and the replay
utility of `baseline.balanced` has *zero* spread across repeats (0.9834 every
time). Exploration 0.02 is genuinely unstable. Exploration 0.10 fails round-1
**every time**, which contradicts the earlier claim in this section that 0.10
"produces discovery and still promotes". That claim came from a single run and
was wrong; 0.10 reliably discovers and reliably cannot promote.

Round-2 promotes at every setting, so the coupling is specific to the first
round, where the body is smallest.

The mechanism is the same one §E6 named: pairing depends on whether the candidate
a learned policy nominates happens to have been attempted on a request where the
baseline was also attempted, and exploration disperses exactly that overlap.

So this is a real trade-off, not a misconfiguration: **the only setting that
reliably produces a promotable model is also the only setting that cannot reach a
newly added provider.** Neither is wrong; they cannot both be had from the
configuration surface as it stands. And because a single run at 0.02 or 0.10 can
report either verdict, **one run is not evidence** — that is why the numbers above
are four repeats and not four one-off observations.

---

## E12. ML against the baselines, with cost live, over repeated runs

§E10 made the cost axis real. This asks the question that motivates the whole
subsystem: **is the learned router better than not learning?** Three exploration
settings × four repeats of an identical configuration, 480 requests each — 12
runs, because §E11 established that a single run cannot be trusted.

Utility difference, `ml.candidate` minus baseline, per run:

| baseline | exploration | mean | range across 4 runs | ML wins |
|---|---|---|---|---|
| `baseline.priority` | 0.00 | **+2.0024** | +2.0020 .. +2.0026 | **4/4** |
| `baseline.priority` | 0.02 | +1.9948 | +1.9819 .. +2.0021 | **4/4** |
| `baseline.priority` | 0.10 | +2.0050 | +2.0005 .. +2.0073 | **4/4** |
| `baseline.round_robin` | 0.00 | +0.4072 | +0.4068 .. +0.4074 | **4/4** |
| `baseline.round_robin` | 0.02 | +0.7488 | +0.4043 .. +1.1131 | **4/4** |
| `baseline.round_robin` | 0.10 | +1.0784 | +1.0559 .. +1.1062 | **4/4** |
| `baseline.lowest_latency` | 0.00 | +0.6863 | +0.6729 .. +0.7078 | **4/4** |
| `baseline.lowest_latency` | 0.02 | +1.2047 | +0.5583 .. +1.9254 | **4/4** |
| `baseline.lowest_latency` | 0.10 | +1.6595 | +1.4992 .. +1.8243 | **4/4** |
| `baseline.balanced` | 0.00 | −0.0062 | −0.0066 .. −0.0060 | **0/4** |
| `baseline.balanced` | 0.02 | −0.0071 | −0.0149 .. +0.0002 | 2/4 |
| `baseline.balanced` | 0.10 | −0.0032 | −0.0133 .. +0.0001 | 3/4 |

Against three of the four baselines the answer is not close: **12 runs out of 12,
with margins 15× to 400× the run-to-run spread.** Against `baseline.balanced` it
is a tie that `balanced` wins on the sign and ML wins on cost.

On cost, `ml.candidate` is cheaper than `baseline.balanced` in every
configuration — by 0.9%, 8.0% and 0.5%. So the honest summary is: **the learned
router matches the strongest replay baseline on utility and is marginally cheaper,
while beating the other three by margins no amount of noise explains.**

### Three qualifications that belong with that number

**1. The arms are not evaluated over the same requests.** `mean_cost`/`utility`
are means over the traces where *that arm's* choice was actually attempted, and
those sets differ substantially — at exploration 0, `ml.candidate` has n=444 and
`baseline.priority` n=120. So this is not five policies scored on one identical
body; it is five policies scored on their own measured subsets. The defensible
comparison remains the **paired** one, which is what the gate uses (91 paired at
exploration 0). The arm means are reported because they are the only per-arm
figures available, and the ±0.003 gap to `balanced` is well inside the subset
noise that implies.

**2. `ReplayBaseline::Balanced` is not the production `Balanced` policy.** It is
a replay heuristic sharing only the name. So "ML ties balanced" is a statement
about the learned model and a heuristic of similar spread, **not** about the
learned model and the shipped product's balanced strategy.

**3. The environment is three synthetic providers with known prices.** One is
cheap and broken, one is mid-priced and dependable, one is fast and dearest. That
is a real trade-off shape, but it is a three-point world chosen by the same
process that wrote the model. The 2.00 margin over `baseline.priority` is
substantially the statement that a static order pointing at a 75%-failure provider
is a bad order — which is true, and is not the same as the learned ranking being
good.

### What this does and does not license

It licenses: the learned router **does** change real routing behaviour after
promotion, **does** beat three of four baselines reproducibly, and **does** reach
the quality of the fourth without paying more for it. Cost is no longer a
dimension the comparisons ignore.

It does not license "the ML router is proven better". That would need real
providers, a cost structure the operator did not design, and the denominator
problem in (1) resolved so all arms are scored on one body.

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

The re-split is a pure function of the body **and the order it is supplied in**.
That qualifier is load-bearing and was added by ADR-0008: the tiebreak used to be
the request id, which is a fresh UUID, so on any body collected faster than one
request per second the frozen holdout was re-drawn from a coin every run. The
promotion verdict alternated on identical traffic until it was fixed. See §E3.

This is demonstrated end to end rather than argued:
`new_outcomes_from_a_new_process_reach_the_next_round_of_learning` collects 80
requests, ends the process, retrains and promotes; a **new** `AppState` picks the
model up from the state directory and serves 40 more; the second retraining sees
a body of 120 requests whose fingerprint differs from round one's, contains 40
request ids round one never saw, produces a different commit, and is promoted
again. Round one's model is retained as the rollback target.

That target is not decorative.
`a_rollback_takes_effect_in_the_live_process_and_not_only_on_disk` requires the
rollback to reach the *router* in a running process, not only the durable pointer,
and was verified to fail when it does not. See §E2.
