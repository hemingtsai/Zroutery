# Upstream SSE error envelopes — 2026-11-07

## Report and diagnosis

The reported warning named provider `xiatianai` and ended in
`upstream returned malformed data: openai_error`. The Chat Completions and
Anthropic stream parsers decoded an explicit JSON error envelope but converted
it to `BadUpstreamPayload`, retaining only `error.message`. A generic message
therefore erased the code/type needed to distinguish a wallet refusal, a rate
window, authentication and an upstream availability failure.

That log alone does not establish the provider's underlying refusal. The
regression fixtures below are synthetic; they are not a recovered live response.

## Implementation

- `Error::stream_payload` retains bounded `code`, `type` and `message` fields
  from explicit errors, including wrapped errors. Unrelated relay fields and
  request echoes are omitted; client/activity messages use the existing safe
  error representation.
- Chat Completions, Anthropic and Responses SSE use this helper. Genuine
  malformed JSON remains a protocol failure. Null error fields and text that
  merely quotes an error stay normal output.
- `failure::upstream_payload_status` is the shared effective-status mapping
  for live parsers and staged HTTP-200 SSE failures. It recognizes NewAPI's
  `insufficient_user_quota`, falls back to a recognized type when the code is
  null or unknown, and preserves authentication precedence over history codes.
- The transport attaches the actual provider name and handshake `Retry-After`
  to parser errors. Its warning distinguishes an explicit upstream failure
  from parser rejection, with bounded type/code tokens, effective error status
  and the canonical failure class instead of the raw event/message.
- The existing output boundary remains authoritative: a refusal before output
  may fail over; a refusal after output keeps that output and ends with an
  error. Known usage on a failed event remains captured before parsing and
  charged to its attempt.

## Verification

- Reproduced before the fix: `upstream_stream_error_test` failed with
  `BadUpstreamPayload("openai_error")` and a wrapped wallet error classified
  as `Protocol` (two failures, one malformed-JSON negative passing).
- Toolchain gate: default stable initially drifted at Rust 1.99.0; an explicit
  `RUSTUP_TOOLCHAIN=1.99.1` run matched CI by commit hash. Rust commands use
  `+1.99.1`; minimum-version checks remain separate.
- Focused no-default-feature suites: `codex_auto_test` 6,
  `failure_authority_test` 10, `native_client_session_test` 4,
  `pipeline_lifecycle_test` 51, `rate_limit_cooldown_test` 5,
  `responses_interop_test` 25, `upstream_session_binding_test` 6 and the first
  three SSE-error regressions all passed (110 tests).
- After adding flat/unnamed error-envelope and quoted-output negatives:
  no-default-feature Core library 976, `attempt_observation_test` 12,
  `claude_server_review_test` 1 and `upstream_stream_error_test` 5 passed.
- Node 22.23.4 / pnpm 11.22.0: UI build and 12 files / 86 tests passed.
  Concurrent UI work continued after that run; this is a result for the tree
  tested, not certification of subsequent UI edits.
- Command-surface self-test, workspace formatting check and whitespace check
  passed.
- Orchestration self-test fixtures passed, but its subsequent repository check
  failed on existing stale source references: `UI` names
  `ui/src/pages/MlRouting.tsx`, and `UI-UX-U10` names `ui/src/components.tsx`.
  Those files have moved into the routing/components directories.

Workspace clippy, all-feature tests and the Rust 1.90.0 check are pending.
The workspace run also covers the final additional lifecycle regression for
provider attribution, `Retry-After` forwarding and subsequent local cooldown,
plus a native-review unnamed-error regression (six SSE-error tests in total).
The fresh headless build and isolated local mock-upstream smoke are pending.

## Delivery state

Local source changes only. No commit or deployment was requested. Verification
uses loopback fixtures; the historical upstream cause and live recovery are not
claimed. Concurrent UI commits and existing documentation changes in this
checkout are outside this fix.
