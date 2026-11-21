# Failure Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-21 · Verified by: `cargo test --workspace --all-features --locked`

This page describes how an error becomes a classification, a retry decision and
a terminal state. It does not describe accounting; a failed attempt's known cost
is covered by [accounting.md](accounting.md).

## One classification

The error's `classified` method is the single mapping from an error to a
`ClassifiedFailure` (`crates/zroutery-core/src/error.rs::classified`).

- `FailureClass` names the cause in one canonical vocabulary: transport, timeout,
  rate limit, authentication, provider unavailable, provider rejected, protocol,
  capability, invalid request, client cancelled, missing key, over budget, no
  candidate, configuration, quota exhausted, interrupted, unknown and balance
  exhausted (`crates/zroutery-core/src/failure.rs::FailureClass`,
  `crates/zroutery-core/src/failure.rs::FailureClass::ALL`).
- `FailureImpact` carries five boolean decisions: affects observation, affects
  the circuit breaker, retryable on the same candidate, fallbackable to another
  candidate, and whether the failure is a provider fault
  (`crates/zroutery-core/src/failure.rs::FailureImpact`).
- A caller derives retry and failover behaviour from the impact
  (`crates/zroutery-core/src/failure.rs::ClassifiedFailure`); `retry_advice`
  may still inspect status or body to pick the delay class or admit the
  history-resource 400
  (`crates/zroutery-core/src/upstream/errors.rs::retry_advice`).

## Retry and failover

`retry_advice` decides whether the same candidate is retried, the next candidate
is tried, or the failure is returned
(`crates/zroutery-core/src/upstream/errors.rs::retry_advice`):

- a session-binding invalidation is never retried;
- the total retry count is capped by `MAX_TOTAL_RETRIES`
  (`crates/zroutery-core/src/upstream/errors.rs::MAX_TOTAL_RETRIES`);
- when another candidate is available, failover wins over waiting: one account's
  rate limit says nothing about the next account;
- a rate-limit error is not slept on by the gateway; the upstream's
  `Retry-After` is returned to the caller as a floor, never violated;
- authentication, quota/budget exhaustion, a malformed request and a
  deterministic content rejection are returned immediately;
- a history/resource error can be a legitimate retry despite a 400
  (`crates/zroutery-core/src/failure.rs::upstream_payload_status`).

A provider that rejects the gateway's translated body is `ProviderRejected`: it
is fallbackable (another candidate may accept the shape) but not retryable
against the same provider.

## Wire versus log

- Ordinary client-facing messages use `safe_message`: provider name and status,
  without the upstream body
  (`crates/zroutery-core/src/error.rs::safe_message`).
- An internal error is flattened to "internal error"; a malformed payload to
  "upstream returned malformed data".
- An explicit SSE error envelope retains bounded `code`/`type`/`message` for
  classification. Native Messages egress preserves the original in-band error
  only after native content output has started; a pre-output error takes the
  canonical recovery/failure path instead
  (`crates/zroutery-core/src/error.rs::stream_payload`,
  `crates/zroutery-core/src/upstream/stream.rs::Upstream::stream_with_identity_policy`,
  `crates/zroutery-core/tests/upstream_stream_error_test.rs::native_review_does_not_forward_an_unnamed_error_as_an_extension`).
- Native Messages HTTP error JSON can be returned through request-local reply
  metadata so the client recognizes capability-rejection wording; it is not
  persisted (`crates/zroutery-core/src/ir/headers.rs::ReplyHeaders`,
  `crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`).
- A failed gateway-owned safety review does not replay generation; its per-tool
  result is unavailable and its own failed attempt retains known spend
  (`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::send_review`).

## Terminal states

A failed request is not a success, and a client that walks away is not a served
request (`crates/zroutery-core/src/server/pipeline/lifecycle.rs::TerminalKind`):

| Terminal | Meaning |
|---|---|
| `Served` | the upstream answer was delivered |
| `Failed` | a canonical failure class ended the request; it carries the class, the retry-after floor and the safe log message |
| `ClientDisconnected { emitted }` | the client left before the answer finished; `emitted: true` is an interrupted stream whose already-sent output is kept, `emitted: false` is a cancel before the first byte |
| `ClientCancelled` | the client explicitly cancelled |

An upstream stream that ends before its terminal event is
`crates/zroutery-core/src/error.rs::Error`'s `InterruptedStream`; it is
classified as the interrupted class by
`crates/zroutery-core/src/failure.rs::FailureClass`, not turned into a
`ClientDisconnected` — the client still receives what was already sent. A
`ClientDisconnected` terminal is the client leaving, not the upstream ending.

## Session binding

Some relays invalidate a session's model binding (code `30013`). The gateway maps
that to a client error with a new-session instruction, rather than retrying the
same binding (`crates/zroutery-core/src/failure.rs::is_session_binding_invalidated_value`,
`crates/zroutery-core/src/error.rs::is_session_binding_invalidated`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| the class is missing | unknown upstream shape | `Unknown`, never success (`crates/zroutery-core/src/failure.rs::FailureClass`) |
| one candidate is rate-limited | provider 429 | fail over to a sibling; do not sleep (`crates/zroutery-core/src/upstream/errors.rs::retry_advice`) |
| a session binding is invalid | relay code `30013` | return a new-session error; never retry (`crates/zroutery-core/src/error.rs::is_session_binding_invalidated`) |

## Not implemented

- No retry of an upstream or tool action that already executed; a local
  persistence failure is terminal, not a replay.
- No retry that ignores the upstream's `Retry-After` floor.
- No second classifier in a caller; the error's `classified` method is the authority.

## Sources

- `crates/zroutery-core/src/failure.rs::FailureClass`
- `crates/zroutery-core/src/failure.rs::FailureImpact`
- `crates/zroutery-core/src/failure.rs::ClassifiedFailure`
- `crates/zroutery-core/src/failure.rs::is_session_binding_invalidated_value`
- `crates/zroutery-core/src/failure.rs::upstream_payload_status`
- `crates/zroutery-core/src/error.rs::classified`
- `crates/zroutery-core/src/error.rs::safe_message`
- `crates/zroutery-core/src/error.rs::is_session_binding_invalidated`
- `crates/zroutery-core/src/error.rs::stream_payload`
- `crates/zroutery-core/src/upstream/errors.rs::retry_advice`
- `crates/zroutery-core/src/upstream/errors.rs::MAX_TOTAL_RETRIES`
- `crates/zroutery-core/src/server/pipeline/lifecycle.rs::TerminalKind`
