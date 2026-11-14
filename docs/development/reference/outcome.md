# Outcome Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

An outcome is the complete result of one client request: every routing attempt,
the terminal status, timing, usage, cost and the identity evidence. Stats,
observation, accounting and ML read it; none of them may invent a missing fact.

## The unit

One outcome per client request (`crates/zroutery-core/src/outcome/mod.rs::Outcome`):

- `outcome_id`, `request_id`, optional `decision_id` and `response_id`;
- `success` and the terminal status;
- planned / last-attempted / served identity, plus legacy `initial_*` and
  `final_*` projections;
- `attempts`, timing, usage, costs and terminal error facts.

The terminal status is `Success`, `Failed`, `Cancelled` or `Interrupted`
(`crates/zroutery-core/src/outcome/status.rs::FinalStatus`). `success` must agree
with the `Success` variant (`crates/zroutery-core/src/outcome/validation.rs::Outcome::validate`).

## Attempts

An attempt is one send to one candidate
(`crates/zroutery-core/src/outcome/attempt.rs::Attempt`):

- candidate model/provider, start/end, latency, TTFT;
- success, failure class/message and HTTP status;
- `rectified`: a rectifier retry of the same candidate is a separate attempt;
- `cost`: per-attempt cost, because a failover chain can spend more than the
  attempt that answered.

The attempt's `is_terminal_success` method is the only definition of a successful terminal
attempt: success, no failure facts, and no non-2xx status
(`crates/zroutery-core/src/outcome/attempt.rs::Attempt::is_terminal_success`).

`cost: None` means the price or usage was unavailable. It is not zero; a zero
would claim the call was free (`crates/zroutery-core/src/outcome/attempt.rs::Attempt`).

## Building and classifying

- `Outcome::builder(request_id)` is the only constructor for a new record
  (`crates/zroutery-core/src/outcome/mod.rs::Outcome::builder`).
- `Outcome::classify_final(attempts, cancelled)` derives the terminal status
  from the attempt evidence (`crates/zroutery-core/src/outcome/mod.rs::Outcome::classify_final`).
- The builder is consuming; `served_candidate` records the identity, `build()`
  clears it on a non-success terminal, and the accessors refuse to surface it
  (`crates/zroutery-core/src/outcome/builder.rs::OutcomeBuilder`).

## Validation

The outcome's `validate` method is fallible and side-effect free
(`crates/zroutery-core/src/outcome/validation.rs::Outcome::validate`):

- ids are non-empty; attempt ids are unique;
- `success` agrees with the `Success` variant;
- latency, TTFT and costs are non-negative and finite;
- identity pairs are complete (a model without a provider is invalid);
- a failed outcome with no attempts needs terminal error facts;
- a last-attempted identity needs an attempt;
- when present, the explicit planned/last-attempted/served identity must agree
  with the attempt evidence.

`canonicalized` returns a validated copy; `is_valid` is the boolean form
(`crates/zroutery-core/src/outcome/validation.rs::Outcome::canonicalized`).

The legacy `initial_*` / `final_*` fields are compatibility projections. They
must not be used to infer a served identity on failure; use `served_*`.

## Fan-out

`record_to_observation` writes each attempt to the observation and stats stores
(`crates/zroutery-core/src/outcome/feedback.rs::Outcome::record_to_observation`):

- a terminal-success attempt records success with latency and TTFT;
- a failed attempt records its classified failure; an unset class becomes
  `Unknown`, not success.

`to_feedback` converts an outcome plus optional signals into feedback
(`crates/zroutery-core/src/outcome/feedback.rs::Outcome::to_feedback`):

- `None` signals means no rating was supplied;
- success or failure alone never creates a rating;
- `try_to_feedback` is the checked variant.

## Boundaries

- This module does not persist anything; it is a value model plus fan-out.
- It does not route or retry; the pipeline records attempts into it.
- It does not turn unknown cost/usage into zero.

## Sources

- `crates/zroutery-core/src/outcome/mod.rs::Outcome`
- `crates/zroutery-core/src/outcome/mod.rs::Outcome::builder`
- `crates/zroutery-core/src/outcome/mod.rs::Outcome::classify_final`
- `crates/zroutery-core/src/outcome/status.rs::FinalStatus`
- `crates/zroutery-core/src/outcome/attempt.rs::Attempt`
- `crates/zroutery-core/src/outcome/attempt.rs::Attempt::is_terminal_success`
- `crates/zroutery-core/src/outcome/builder.rs::OutcomeBuilder`
- `crates/zroutery-core/src/outcome/validation.rs::Outcome::validate`
- `crates/zroutery-core/src/outcome/validation.rs::Outcome::canonicalized`
- `crates/zroutery-core/src/outcome/feedback.rs::Outcome::record_to_observation`
- `crates/zroutery-core/src/outcome/feedback.rs::Outcome::to_feedback`
- `crates/zroutery-core/src/feedback.rs::Feedback`
- `crates/zroutery-core/src/feedback.rs::FeedbackSignal`
