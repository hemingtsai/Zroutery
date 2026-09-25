# ADR-0004: Canonical Failure and Outcome Authority

## Decision state

Accepted for the bounded Core P1 repair batch. This is a Core contract, not an
ML training or activation decision.

## Context

`Error`, `FailureClass`, and `FailureImpact` currently make overlapping runtime
decisions. Stream drops can finalize with a success-defaulted record. Outcome
and Feedback types exist but have no trusted production construction path, and
planned, last-attempted, and actually served identities are not separated.

## Decision

1. `Error` remains the structural/wire error type. One exhaustive
   `ClassifiedFailure` constructor is the sole source of retry, fallback,
   circuit, observation, stats, and provider-fault decisions.
2. Legacy `Error::is_retryable` and `Error::counts_against_health` may remain
   compatibility helpers, but runtime code must consume the classified result
   rather than independently reclassifying errors.
3. Local/configuration failures (missing key, budget denial, no candidate,
   capability rejection, invalid request) cannot poison provider health. A
   missing key may permit a different-provider attempt, but is not an upstream
   provider failure.
4. Client cancellation, stream interruption, and premature stream destruction
   are explicit terminal states. They are never finalized as success merely
   because usage or cost accounting completed.
5. Outcome identity distinguishes:
   - planned identity: selected before attempts;
   - last attempted identity: final attempt made, if any;
   - final served identity: provider/model that produced a successful terminal
     response, otherwise `None`.
6. Outcome is Core-owned and emitted at most once per request. Feedback is
   optional; absence of a user signal must not fabricate a positive/negative
   rating. Dataset conversion is pure and remains optional-ML-consumable.
7. Rectifier retries, if retained, must be represented explicitly in attempt
   evidence before they can become training samples.

## Consequences

- Failure and router adapters can be implemented and tested independently of
  the production pipeline.
- The pipeline integration node becomes the sole owner of terminal lifecycle
  wiring and final served identity.
- No ML shadow contract is reopened; accepted `7E-1A`/`7E-1B-CORE` records
  remain unchanged.
- No provider execution, session-affinity implementation, or automatic model
  activation is authorized by this ADR.

## Required gates

- Exhaustive Error-to-ClassifiedFailure mapping tests.
- One-impact-table tests for retry, fallback, circuit, observation, and stats.
- Client stream drop and explicit cancellation regression tests.
- Planned/last-attempted/served identity correlation tests.
- Exactly-once Outcome/Feedback fan-out tests.
- Full Core/workspace tests, clippy, and `git diff --check`.

## Related records

- `docs/development/repository-audit-2026-09-25.md`
- `docs/development/node-status/core-p1-failure-authority.status.json`
- `docs/development/node-status/core-p1-outcome-feedback.status.json`
- `docs/development/node-status/core-p1-pipeline-lifecycle.status.json`
