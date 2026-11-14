# M0 Audit — Real-traffic closure, temporal resources, ML routing evaluation

Baseline commit: `5566915` (`dev`). Audited before any change, against the taskbook.

## 0. Verdict up front

The taskbook describes a system that is *less* built than it actually is. The ML closed
loop (traces, shadow, replay comparison, statistics, promotion gate) is mature and
carefully defended. The genuinely missing pieces are narrower and sharper:

| Workflow | Audit verdict |
|---|---|
| A — trace contract | **Partly there.** Decision-time snapshot is complete. Execution identity, account snapshot and data-quality fields are absent. |
| B — temporal resources | **Absent.** No bucket type, no expiry, no reset semantics, no charge order, no planner. |
| C — task evidence | **Absent.** No task-outcome evidence; API success is the only label. |
| D — evaluation | **Libraries exist, no entry point.** No CLI over local traces; promotion gate ignores statistics. |
| E — shadow/promotion | **Shadow exists. Gate is threshold-only**, with two dead knobs. |

## 1. Baseline state

```
cargo check --workspace --all-targets   -> exit 0
cargo test  --workspace --no-fail-fast  -> exit 101
   149 test binaries, 2316 passed, 1 failed
```

The single failure is **pre-existing and environment-dependent**, not introduced here:

- `migration::tests::verify_endpoint_unreachable_errors` (`crates/zroutery-core/src/migration.rs:2071`)
- It dials `http://192.0.2.1:1/health` (TEST-NET-1, deliberately non-routable) and asserts
  `result.errors[0].contains("unreachable")` (`:2088`). This sandbox's socket layer returns
  different error text. The file is untouched by this work.

`cargo test --workspace` **stops** at that failure, so the integration suites never run
without `--no-fail-fast`. All later runs must use `--no-fail-fast` to see the true picture.

### Uncommitted work that must be preserved

The workspace is **not clean**. Not ours, not touched:

- `crates/zroutery-core/src/agent_takeover.rs` (+1400 lines) — client takeover status surface
- `crates/zroutery-core/src/lib.rs`, `crates/zroutery-core/tests/e2e.rs` (modified)
- `crates/zroutery-core/tests/agent_takeover_status_test.rs`, `docs/development/client-attribution-takeover-audit.md` (untracked)
- A stash exists: `stash@{0} cleanup-wip: takeover + opencode in-progress`

### Feature gating (constrains placement)

`zroutery-headless/Cargo.toml` enables **`ml` only**. The shipped binary therefore has
`account` **off**, so `ml::traces::TraceLog` is live but no account code is linked. Any
resource model the planner needs must sit behind `ml` (not `account`) to be reachable at all.

## 2. The data chain, request by request

```
plan (router.rs) -> pipeline.rs attempts -> ir/response decode -> encode -> Outcome
   -> observation/stats | dataset (contained_ingest) | trace (contained_append)
   -> projections
```

Terminal transition, `server/pipeline.rs`:
- `:2717` `dataset_ingested` — in-memory ring `DatasetStore`, dies with the process.
- `:2772` `trace_persisted` — durable `traces.jsonl` via `contained_append` (`:2804`).
  Exactly once per request, only when `decision_time` survived, contained (no panic escapes).
- `:2834` `observed` — projection.

`ml/traces.rs:867` `contained_append` returns `TraceIngestion::{Appended,Nothing,Refused}`;
counters at `:339` `TraceCounters{appended,nothing,refused,io_errors}`.

## 3. Where evidence is actually lost

### 3.1 Execution identity is overwritten (A2 — real bug)

Non-streaming, `server/pipeline.rs`:
```rust
:1630   resp.model = candidate.exposed_id.clone();   // buffered path
:1715   resp.model = candidate.exposed_id.clone();   // rectified-retry path
```
`resp.model` at that moment holds the **upstream-declared** model, parsed by the protocol
decoder. It is replaced by the client-facing `exposed_id` and the original is discarded.
Streaming is worse: `pipeline.rs:2078` builds the encoder with `exposed_id` directly, so the
declared model is never captured at all.

Consequence: `Outcome` can say *which Zroutery model was served*, never *which backend the
provider actually ran*. A hidden upstream pool behind one `exposed_id` is invisible — exactly
what the taskbook forbids guessing at.

`OutcomeIdentity` (`outcome.rs:69`) is strong but purely **routing** identity:
`planned` / `last_attempted` / `served` `CandidateIdentity`. `Attempt` (`outcome.rs:191`)
carries `http_status`, `failure_class`, `latency_ms`, `ttft_ms`, `cost: Option<f64>` — but
no declared model, no fingerprint, no response id.

There is **no** `system_fingerprint` anywhere in the tree, and no upstream response header
is captured.

### 3.2 Trace schema (A1)

`RequestTrace` (`traces.rs:125`), `TRACE_SCHEMA_VERSION = 1`:
```rust
schema_version, request_id, decision_id, recorded_at, input: ShadowInput, samples
```
Absent: `outcome_id` (derivable), execution identity, account/resource snapshot,
per-trace HTTP status, data-quality / missing-field reasons, trace-write outcome.

`ShadowInput` (`shadow.rs:407`) is **already rich** and does not need redoing:
`policy_id`, `policy_revision`, `client_id`, `task: TaskProfileSummary`,
`production_selected`, `candidates: Vec<ShadowCandidateInput>`, `session_mode`,
`session_switch_count`, `is_fallback`.
`ShadowCandidateInput` (`shadow.rs:390`) carries `eligible`, `rejection_reason`, and the
exact `RoutingFeatures`. So "decision scene" is covered.

### 3.3 Feedback attribution (C3)

`Feedback` (`feedback.rs:86`) keys on **`outcome_id` only** — no attempt id, no candidate.
It is attributable to the served candidate only by joining `Outcome.served_*`. Weak signals
(`RetryRequested`, `ConversationAbandoned`, ...) are modelled and `Feedback::rating()`
deliberately refuses to synthesize a score when signals are absent. The *semantics* are right;
the *attribution key* is missing.

### 3.4 Coverage of terminal states (A3)

Not yet established for: stream interrupted, upstream-unparseable, eligibility-refused
with no attempt. `TraceLog::clear` (`:782`) and restart-read are already handled and tested.

## 4. Account / resource support (B — audit of `account/`)

Verified against source:

- **One flat quota, not buckets.** `AccountQuota` (`account/types.rs:45`) = `total`, `used`,
  `remaining`, `unit: String`, `resets_at: Option<i64>`. `AccountRuntime` holds
  `quota: Option<AccountQuota>` (`types.rs:122`) — one, not a list.
- **Expiry: absent.** No expiry field on any account type. `AuthenticatedSession.expires_at`
  is *access-token* expiry, unrelated.
- **Reset time: field exists, never populated.** `quota_from_self` hardcodes
  `resets_at: None` (`adapters/newapi.rs:1507`). `Some(..)` appears only in tests.
- **Reset behaviour: absent.** No clear/refill/carry-over/rolling/unknown enum.
- **Charge order: absent.** Nothing decides which bucket is debited. `has_active_subscription`
  (`newapi.rs:1201`) only suppresses a `QuotaExhausted` status.
- **User-declared expiry/reset rules: absent.** `AccountConfig` (`config.rs:620`) has
  `account_id`, `key_ref`, `enabled`, `maintenance`. `MaintenanceConfig` (`config.rs:665`) is
  a **check-in schedule**, not an entitlement rule; its own docs admit the gap.
- **Concurrency reservation: absent.** `remaining` is a snapshot
  (`reconcile.rs:245-296`); nothing subtracts in-flight spend. The only admission control is
  `AdmissionGates` (`server/mod.rs:187`) keyed on `BudgetScope` = Global/Provider/Tier — no
  account id, no quota kind.
- **Reconciliation:** `ObservedProviderAccounting` (`account/resource.rs:241`) reconstructs
  from log ratios and carries `agrees_with_recorded: Option<bool>` (`:267`). There is **no**
  Matched/Mismatched/Insufficient/Unattributable verdict, and no "unattributable balance
  change" concept. `RewardSource::BalanceDelta` (`account/checkin.rs:212`) is explicitly
  "confounded by concurrent traffic… never sufficient on its own".
- **Units are kept separate on purpose.** `budget.rs:30-32` and `billing.rs:4-6`: currencies
  are never converted. This must be preserved.

## 5. What evaluation can and cannot measure today

**Baselines** (`ml/comparison.rs:182`): `Priority`, `RoundRobin`, `LowestLatency`, `Balanced`.
`Balanced` is honestly documented as **not** production Balanced (`:199-203`), which
contradicts the enclosing doc comment at `:180-181` ("each mirrors the production strategy").
Production Balanced is a per-tier latency/price election (`router.rs:1146`); **no price is
recorded in the trace**, so faithful replay is currently impossible.

**Predicted vs measured** is structurally separated and correct: `observed_utility`
(`comparison.rs:457`) scores ±w; `compute_utility` (`reward.rs:246`) scores [0,w];
`PairedDeltas` has only `mean_observed_utility_delta`. Predicted utility never leaves the policy.

**Counterfactual coverage** is tracked: `ArmRecord.measured` (`:480`), `ArmMetrics.coverage`
(`:509`), `PairedDeltas.paired_requests` (`:647`), `ShadowGap::AlternativeUnmeasured`.

**Gaps, precisely:**
- `PairedDeltas::fallback_rate_delta` is **hardcoded `0.0`** (`comparison.rs:771`).
- `PromotionConfig::max_fallback_rate_delta` (`promotion.rs:152`) is **read by no criterion** —
  declared, defaulted, hashed, never compared.
- `ArmMetrics::coverage` / `requests_measured` **never gate anything**; `promotion.rs` does
  not read them.
- `promotion.rs` imports **no** `statistics` type: no interval, no p-value, no effect size.
  Its only floors are `MIN_PAIRED_REQUESTS = 30` and `UTILITY_DELTA_THRESHOLD = 0.2`.
- The full statistics module (`wilson_interval`, `mcnemar_exact_p`, `holm_adjust`,
  `required_decisions`, MDE via `minimum_effect`) is wired **only** into `offline_gate.rs`,
  which ends at `Considerable`/`NotConsiderable` and does **not** gate promotion.
- No task-quality and no resource/quota metric exists in comparison, statistics or promotion.
  `Feedback` is collected and **never consumed**.
- `temporal_split` (`evaluation.rs:773`) is row-level, ungrouped, unguarded, and has **no
  non-test caller**. `learning.rs` and `warmup.rs` use whole-request-group splits instead,
  each documenting that `temporal_split` would leak an attempt row across the boundary.
- **Features**: `FEATURE_DIMENSION = 32`, `FEATURE_SCHEMA_VERSION = 1`, slot 31 unused. No
  provider identity, no model identity, no expiry/reset feature. Slots 28-30 (quota, rate
  pressure, account health) are `#[cfg(feature = "account")]` and therefore **permanently
  `UNKNOWN` in the shipped build**.

## 6. Plan

New modules under `ml` (reachable with the `ml` feature the binary actually enables):

| Module | Workflow | Purpose |
|---|---|---|
| `ml/task_evidence.rs` | C | `TaskOutcomeEvidence`, verifier kinds, `unknown` preserved |
| `ml/resource_buckets.rs` | B1/B2 | `ResourceBucket`, `ResetBehavior`, windows, sources, charge order |
| `ml/execution_identity.rs` | A2 | layered declared/observed identity + provenance |
| `ml/temporal_planner.rs` | B3/B4 | deterministic planner, injectable clock, shadow prices |
| `ml/data_quality.rs` | A5/D1 | trace audit + `BLOCKED` reasons |
| `ml/eval_report.rs` | D | offline runner, JSON + Markdown report |

Plus targeted extensions: `RequestTrace` v2 (additive, `#[serde(default)]`), pipeline
identity capture before the overwrite, promotion gate coverage/statistics criteria,
CLI entries following the existing `--flag` subcommand style.

Compatibility: additive only. `TRACE_SCHEMA_VERSION` moves to 2 with `#[serde(default)]`
on every new field, so v1 lines still load. No 32-dim feature change, so
`FEATURE_SCHEMA_VERSION` stays 1.

## 7. What was implemented, and how to run it

### Commands

```sh
# The audit (this document).
#   docs/development/routing-learning-closure-audit.md

# Offline evaluation over collected traces. Sends no upstream request.
zroutery-headless --evaluate-routing
zroutery-headless --evaluate-routing --state-dir <dir> --out <report-dir>
zroutery-headless --evaluate-routing --min-traces 200 --min-coverage 0.5

# A starting strategy for getting real data (workflow F, "起步策略").
# 1. Serve real traffic normally. Traces accumulate automatically; nothing else is needed.
# 2. Run --evaluate-routing. A fresh install reports BLOCKED and names what is missing.
# 3. Treat BLOCKED as the expected state until three conditions hold:
#      - traces >= --min-traces (default 50)
#      - execution-identity coverage is reported per request
#      - task verdicts exist at all
# 4. Only a `Supported` verdict with an available task-quality dimension is promotable.
```

`--evaluate-routing` exits **0 for a BLOCKED verdict**. A blocked verdict is a correct
answer, not a failure; only a corrupt trace file (exit 2) or a bad argument is an error.

### Modules added

| File | Lines | Purpose |
|---|---|---|
| `ml/execution_identity.rs` | 396 | layered declared/observed identity, unknown-with-reason |
| `ml/task_evidence.rs` | 550 | `TaskOutcomeEvidence`, verifier authority, `EvidenceCoverage` |
| `ml/resource_buckets.rs` | 699 | multi-bucket resources, reset semantics, reconciliation |
| `ml/temporal_planner.rs` | 750 | deterministic planner, shadow prices, injectable clock |
| `ml/eval_report.rs` | 908 | evaluation runner, `BLOCKED` semantics, JSON + Markdown |
| `zroutery-headless/src/evaluate.rs` | 263 | the CLI entry point |
| `tests/trace_execution_identity_test.rs` | — | 15 tests: schema v1/v2 compat, attribution, containment |
| `tests/evaluation_report_test.rs` | — | 20 tests: verdict semantics, denominators, determinism |

### Verification (all run on the final tree)

```
cargo fmt --check                                     exit 0
cargo check --workspace --all-targets                 exit 0
cargo test  --workspace --no-fail-fast                exit 0   78 suites, 2496 passed, 1 pre-existing env failure
cargo test  -p zroutery-core --all-features \
             --no-fail-fast                           exit 0   72 suites, 2694 passed, 1 pre-existing env failure
cargo clippy -p zroutery-core --all-features         1 pre-existing warning
```

The one clippy warning (`too_many_arguments` on `buffered_chat`) is present at `5566915`
and is not from this work — verified against `git show HEAD:...pipeline.rs`.

Baseline was 2316 passed / 1 failed; the failure was
`migration::tests::verify_endpoint_unreachable_errors`, an environment flake (it dials
TEST-NET `192.0.2.1:1` and asserts the error text contains "unreachable"). It passed on
later runs and is unrelated to routing.

## 9. Second round: three defects closed

Found by auditing the code I had just written against the rest of the tree, not by
adding features.

### 9.1 A constant zero published as a measurement

`PairedDeltas::fallback_rate_delta` was hardcoded to `0.0` at `comparison.rs:771`.
Every report therefore stated that a policy did not change how often a retry was
needed, on every dataset, regardless of the traffic. Two consequences:

- The number was a **measurement that was not taken**, presented with the same
  typography as the ones that were. That is the specific failure the taskbook forbids
  ("must not output apparently precise zero error / zero cost / 100% success").
- `PromotionConfig::max_fallback_rate_delta` had nothing to compare against, so it was
  declared, defaulted, folded into `identity()`, and read by no criterion — a
  tolerance the operator believed they had and did not.

Both are fixed. `pair_against_baseline` now counts each arm's `fell_back` flags over
exactly the paired set, and `PromotionGate` emits a `fallback_regression` criterion.

Pinned by `tests/fallback_delta_test.rs` (9 tests), including one that asserts the
gate's reported measurement **equals** the pairing's independently computed delta — if
it ever reads a constant again, that equality breaks.

### 9.2 The identity capture ran after the trace was already written

`lifecycle.finalize(...)` **is** the terminal transition, and the durable trace is
written inside it. Capturing the declared model immediately before the
`resp.model = exposed_id` overwrite was too late: the record had already been
persisted with the constructor default, and the overwrite merely destroyed the value
that had not been saved.

The streaming path *passed* while the buffered path failed, because a stream's terminal
transition happens later (when the body ends). Two paths disagreeing is exactly how
this would have survived review — the working one is not evidence for the broken one.
The capture now happens before `finalize` on both paths.

### 9.3 Streaming declared the model only in its opening frame

`pipeline.rs:2078` builds the stream encoder with `exposed_id`, so the streaming path
captured nothing at all. `StreamEvent::Start { model, .. }` carries the upstream's
declaration, so the capture now happens there.

Proven end-to-end by `tests/execution_identity_end_to_end_test.rs` (4 tests), which
drives a real server against a mock upstream that declares a model name no Zroutery
config contains, for both buffered and streaming, and asserts:

- the **client** still sees `alpha-zr-public-name` (the rewrite is unchanged);
- the **trace** records `backend-from-a-hidden-pool-0007` (the evidence survived).

Three separate harness bugs had to be fixed to get this test running, each of which
would have made it pass for the wrong reason: the `ServerHandle` was dropped (server
stopped), the model id needed provider namespacing (`exposed_id()` is
`{provider}-{model}`), and concurrent tests shared a trace directory.

### 9.4 A task verifier, and why it runs on the serving path

Task quality was `BLOCKED` because nothing was wired to produce a verdict. The first
attempt was wrong and was deleted rather than shipped: it derived verdicts from
`RequestTrace`, assuming the response body was available there. It is not — traces
deliberately carry no prompt and no answer — so every function in it would have
returned a placeholder and every rate built on it would have been fabricated.

The honest design verifies **on the serving path**, in `pipeline.rs`, at the one point
where the request's demands (`tools`, `tool_choice`, `structured_output`) and the
response's content are both in hand. Only the verdict is persisted.

`ml/task_verifier.rs` implements two checks:

- **Tool execution.** A required call (`tool_choice: any` / a named tool) that never
  arrives is `Failed`; an *optional* call that never arrives is `Unknown`. That
  distinction is load-bearing — conflating them would charge every conversational
  answer carrying a tool schema with a task failure. A truncated tool call (null
  input, no id) is `Failed`, because the client cannot execute it.
- **Structured output.** JSON parseability when the client actually stated a
  constraint. With no constraint, nothing is checked and nothing is claimed.

A request with no applicable check produces **no evidence at all**, which is what keeps
label coverage honest: `Unavailable` would have implied a check ran.

Every check is a **form** check, not a correctness check. A model can emit valid JSON
that answers the wrong question and pass. Each summary says so.

Proven end-to-end in `tests/execution_identity_end_to_end_test.rs`: a tier-routed
request with `tool_choice: any` produces a `Succeeded` `ToolExecution` verdict
attributed to the served candidate, while a plain chat turn produces **no** verdict at
all.

**Done in section 9.7.** Streaming is reassembled and verified on the same terms as a
buffered answer.

### 9.5 Statistical evidence, and clustered independence

The promotion gate is threshold-only: its floors are a paired-request count and a raw
effect delta. A candidate with 30 pairs and a large delta passes the same gate as one
with 3000 — the count is a *minimum*, never a claim that the evidence supports the
conclusion. `ml/statistics.rs` ships everything needed to say more but is wired into
`offline_gate.rs`, which ends at `Considerable` and does not gate anything.

`ml/eval_stats.rs` computes the same quantities for a paired routing comparison,
leaving the mature gate untouched:

- A clustered Wald interval on the mean paired effect, with the minimum detectable
  effect from `comparison::UTILITY_DELTA_THRESHOLD` so the two modules cannot disagree
  about what "large" means.
- A power calculation via `statistics::required_decisions`, which reports how many pairs
  the effect actually needed and blocks when the body is thinner than that.
- Insufficient evidence produces `BLOCKED`, not `NotImproved`: "cannot tell" and "does
  not help" stay distinct.

**Clustering is the part `statistics.rs` deliberately does not do.** It states that the
unit of independence is the decision, and is right for an offline holdout whose rows are
independent draws. Routing traces are not: twenty requests in one conversation are twenty
observations of one user. Counting them as independent shrinks the interval by roughly
`sqrt(20)`.

Traces carry no session id, so clusters are **time blocks** — conservative, because
merging independent observations widens the interval rather than narrowing it. A
one-block body is reported as degenerate, since a clustered variance with a single
degree of freedom is not an interval.

Two bugs in this module were caught by its own tests rather than by review:

- `clusters.len()` returned the number of observations, not the number of distinct
  clusters, so 200 requests in one hour reported as 200 clusters and the
  single-block degeneracy check could never fire.
- Zero dispersion produced an **infinite** standard error. That is wrong in the
  direction that matters: perfectly separated data is what a strong result looks like,
  and an unusable interval would have thrown away the best evidence. It now reports the
  (correct) zero-width interval and sets `is_zero_dispersion`, because a zero-width
  interval reads as infinite certainty and no finite sample earns that.

`comparison.rs` gained one additive function, `run_comparison_with_records`, because a
variance cannot be estimated from `PairedDeltas`' sums and counts. `run_comparison` is
now a thin wrapper with an unchanged signature, so no existing consumer or serialised
report moves.

Pinned by `tests/routing_evidence_test.rs` (9 tests), including one asserting that 400
requests bunched into 8 hours yield a strictly wider interval than the same 400 spread
over 400 hours.

### 9.6 Closing the gate itself

Section 9.5 left the statistical layer in the evaluation path only. That was not
enough: `round.rs:199` — **the production promotion round** — calls
`PromotionGate::evaluate` directly, so a thin body still promoted through the one path
that actually installs models.

`PromotionGate` now emits a `statistical_evidence` criterion. Evidence is attached to
`RoutingComparison`, so the gate cannot be reached without it. Two rules that took
correction to get right, each caught by an existing test rather than by review:

**Evidence must be keyed per baseline.** The first version measured against
`PromotionConfig::default().required_baseline` and stored one figure. `ml_closed_loop_test`
configures `baseline.priority`, so the gate read a measurement of a *different*
comparison — its CI centre (−0.067) did not even agree with the `routing_utility`
criterion's own mean. Evidence is now a map keyed by baseline name and the gate reads
whichever its configuration names.

**Required-n is a planning number, not a second gate.** It was initially a blocker,
which produced a clear failure: *"40 pairs is below the 192 needed to detect an effect
of +0.2000 at 80% power."* That reasoning is wrong for a measurement that already
happened. `required_decisions` answers "how much data would you need to *plan* for the
minimum effect worth caring about", and it is computed against the minimum effect, so a
large real effect "needs" the same n. The interval has already accounted for how many
pairs there were; gating on both double-counts the sample size. It is now a reported
caveat.

With those two fixed, the existing closed-loop fixtures pass **on their own merits** —
their evidence genuinely supports promotion — rather than by weakening the criterion.

The bar itself is the interval excluding zero. Clearing the minimum detectable effect is
reported in the criterion's reason but is deliberately *not* a promotion requirement: a
real improvement that is merely small is still an improvement, and rejecting a better
candidate is its own kind of failure.

Also corrected here: the single-time-block case moved from `blockers` to `caveats`. The
clustered estimator already falls back to the per-observation standard error there,
which is valid under an independence assumption the body cannot test — so the weakened
assumption is stated rather than turned into a refusal, and it no longer duplicates the
pair floor.

### 9.7 Streaming task verification

The gap left in section 9.4: a streamed answer never exists as a `ChatResponse`, so the
verifier had nothing to read and streamed requests recorded execution identity but no
task verdict. Since streaming is most of real interactive traffic, that was most of the
coverage the milestone needed.

`StreamResponseAccumulator` reassembles a response from the canonical stream events:
`ToolUseStart` supplies the call's identity, `ToolUseDelta` accumulates its arguments
across fragments, `TextDelta` accumulates the answer. The accumulator is fed the
**published** event rather than the raw upstream one, so what is judged is what the
client actually received, not what the provider said before rewrites.

Three details that are correctness, not polish:

- **Arguments that never parse become `Null`**, which the verifier rejects. That is the
  right verdict for a truncated arguments blob — the client could not execute it — and
  it falls out of the reassembly rather than needing a special case.
- **Only a clean terminal is judged.** A truncated, failed or cancelled stream is
  content the client never received in full, so verification runs only on
  `StreamTerminal::Completed`. Judging a cut-off stream would score a model for a
  connection problem.
- **Classifier streams are excluded.** Their text is already accumulated for the
  classifier verdict, and judging that would score Zroutery's classifier rather than a
  model.

Reasoning deltas are dropped rather than buffered: they are not an output a client
consumes and no check here reads them, so holding them in memory would serve no reader.

Proven end-to-end in `tests/execution_identity_end_to_end_test.rs`: a streaming
`tool_choice: any` request produces a `Succeeded` `ToolExecution` verdict attributed to
the served candidate, with the tool call visible in what the client received.

### 9.8 Connecting the account subsystem to the planner

`ml/resource_buckets.rs` and `ml/temporal_planner.rs` were a complete system nothing
fed. `account/` describes the world that actually exists — one flat
`AccountQuota`, a `resets_at` the NewAPI adapter always leaves `None`, no expiry — and
nothing joined the two, so the planner had nothing to plan over.

`ml/account_projection.rs` is the join, behind `cfg(feature = "account")` because the
shipped binary enables `ml` and not `account`. It is deliberately **lossy in the honest
direction**:

- A reset rule is applied only when a reset time was actually reported. The NewAPI
  adapter never reports one, so in practice every projected bucket is
  `ResetBehavior::Unknown` — and an unstated rule is the common case, not an edge case.
- **No bucket claims to expire.** Nothing in `account/` reports expiry, so inventing one
  would let the planner treat an ordinary balance as though it were about to be
  destroyed, which is the exact behaviour the temporal planner exists to exploit.
- Credits are never priced as money. `budget.rs` already refuses to add currencies, and
  misclassifying someone's free credit as real cash is the expensive direction.
- An account that is suspended, or that has never synced, is **not trustworthy** — a
  hard gate, because spending against a stale balance is how a quota goes negative.

Two defects surfaced from writing the integration tests:

- **Both projected buckets shared a `resource_id`.** An account may legitimately hold a
  credit balance and a money balance at once; two buckets sharing an id would silently
  shadow each other in a charge order. The kind is now part of the identity.
- **Real money was priced as unquantified.** `shadow_price` demanded a known reset
  behaviour before pricing at all, so a pay-as-you-go wallet — whose future value is
  perfectly known, because money never "refills", it is simply spent — was returned at
  the unknown-rule price. Real money is now exempt, which is the one case where the
  missing reset rule genuinely carries no information.

14 tests, including two that drive a projected bucket through the planner end to end.

### Deliberately not done

Stated rather than left implicit, because each is a real gap:

- ~~No task verifier wired~~ — **closed in round 2** (section 9.4). The serving path now derives
  tool-call and structured-output verdicts from real traffic. What remains is *coverage*,
  not plumbing: streaming is unverified, and there is no compile/test verifier, so
  code-task quality — arguably the most valuable signal — remains unmeasured.
- ~~Streaming execution identity~~ — **closed in round 2** (section 9.3). Both paths are now captured and covered end-to-end.
- ~~Resource buckets not connected~~ — **closed in section 9.8**. `ml/account_projection.rs`
  bridges `AccountQuota`, `ObservedResourceEvent` and `ObservedProviderAccounting` into
  the planner's vocabulary, conservatively. What remains is depth: one account still
  projects to exactly one bucket, because `account/` still holds exactly one quota.
- **No provider-model pool is connected to the planner.** `MlPolicy` is evaluated through
  the existing decision engine; the temporal plan is computed and reportable but does not
  yet alter candidate ranking.
- ~~Promotion gate threshold-only~~ — **closed in section 9.6**. The gate now emits a
  `statistical_evidence` criterion, evidence rides on the comparison keyed by baseline,
  and the production promotion round cannot reach the gate without it. The bar is the
  confidence interval excluding zero; clearing the minimum detectable effect is reported
  but not required.
- **No Shadow→Canary traffic split was implemented.** Shadow recording already exists and
  is reused; controlled rollout is not started.

### Concurrent work in this tree

`agent_takeover.rs`, `stats.rs`, `client.rs`, `lib.rs`, `tests/e2e.rs`,
`tests/agent_takeover_status_test.rs`, `tests/client_attribution_test.rs` and
`docs/development/client-attribution-takeover-audit.md` are **not** part of this task and
were not modified by it. A stash exists: `cleanup-wip: takeover + opencode in-progress`.
Those files were actively changing during this work; the verification numbers above were
produced with them present.

## 8. Stage conclusions

| Milestone | Status | Why |
|---|---|---|
| M0 Baseline | **PASS** | Audit, HEAD, test baseline, gap list, DAG all recorded above. |
| M1 Trace usable | **PARTIAL** | Execution identity captured and durable on both buffered and streaming paths (schema v2, backwards compatible, 19 tests incl. 4 end-to-end). Account snapshot at decision time still missing. |
| M2 Temporal resource contract | **PARTIAL** | Bucket model, dual time semantics, reset behaviour, reconciliation and a deterministic planner, now fed by a conservative projection of the account subsystem (section 9.8). `account/` still holds one flat quota per account, so multi-bucket reality — a plan *and* a wallet at once — needs the account model itself changed, and nothing reports expiry, so no expiring-benefit scenario can yet be exercised. |
| M3 Task quality evidence | **PARTIAL** | A runtime verifier on the serving path produces tool-call and structured-output verdicts for **both buffered and streamed** traffic, attributed to the served candidate and covered end-to-end (9 unit + 7 end-to-end tests). All checks are form-only — there is no compile/test verifier, so code-task quality, arguably the most valuable signal, remains unmeasured. |
| M4 Offline evaluation | **PASS** | Single CLI entry, JSON + Markdown, dataset fingerprint, explicit `BLOCKED`. |
| M5 Shadow & promotion | **BLOCKED** | Statistical evidence now exists and gates the evaluation: clustered 95% intervals, minimum detectable effect, required-n power, and BLOCKED when the effect cannot be supported. Still blocked in practice for want of task-quality coverage. `PromotionGate` itself remains threshold-only. |
| M6 Controlled rollout | **NOT STARTED** | Nothing reached M5. |

Overall: the implementation is real and tested, and the honest current answer for any real dataset remains **BLOCKED** — not because a report is missing, but because the verified fraction of task outcomes is small and concentrated in tool and structured-output traffic. That is the correct verdict, and collecting more real traffic with the verifier attached is what moves it.