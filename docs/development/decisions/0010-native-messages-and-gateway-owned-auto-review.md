# ADR-0010: native messages and gateway owned auto review

Status: accepted
Date: 2026-11-14
Supersedes: none

## Context

Claude Code embeds dangerous-tool-use checks in its main Messages request.
Routing that request into the standalone classifier pool loses generation;
omitting results causes the client to use standalone checks instead
(`crates/zroutery-core/src/classifier.rs::detect`,
`crates/zroutery-core/tests/claude_server_review_test.rs`). Compatible upstreams
need not implement the native review service, and a gate cannot treat missing
or malformed answers as permission
(`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::parse_response`).

## Decision

Preserve the native Messages envelope and all native review results, including
extension members. Only fields with explicit portable semantics may bridge;
other content uses request-local native routing
(`crates/zroutery-core/src/protocol/anthropic/native.rs::validate_bridge`).

When explicitly enabled through the existing classifier master switch and
server-review setting, use that pool for missing embedded results. Review the
exact pending inputs with the client policy and the unshaped transcript, return
per-tool results at the native terminal slot, and continue text/pings while
waiting. A binary decision model judges one action, not a whole parallel batch
(`crates/zroutery-core/src/server/pipeline/safeguards/mod.rs::ReviewContext`,
`crates/zroutery-core/src/server/pipeline/safeguards/stream.rs::ReviewFrames`,
`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::classify`).

Each send owns a separate lifecycle, quote and parent ID. Acquire the union of
generation and review budget scopes once and include pending known generation
spend in review's check. Never replay generation on a local settlement or
classifier failure (`crates/zroutery-core/src/server/pipeline/budget.rs::apply_budgets_with_scopes`,
`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::send_review`).

## Consequences

The client protocol can retain server review across heterogeneous upstreams,
but reviewer latency and actual spend are additional. Unknown context versions,
partial arguments, missing/uncertain verdicts and an incomplete upstream-only
thread history cannot acquire approval
(`crates/zroutery-core/src/server/pipeline/safeguards/mod.rs::ReviewContext::evaluate`).
Fail-closed holds for the evidence shapes the bridge could otherwise repair:
a Chat-bridged tool call with missing, empty or non-string arguments stays
invalid, a Responses reviewer without wire status `completed` cannot approve,
and a SystemOne answer with an explicit non-binary type is refused even when
it carries a low probability
(`crates/zroutery-core/tests/gateway_safeguards_test.rs::chat_bridge_never_approves_missing_empty_or_null_arguments`,
`crates/zroutery-core/tests/gateway_safeguards_test.rs::responses_reviewer_requires_a_completed_wire_status`,
`crates/zroutery-core/src/upstream/decision.rs::parse_answer`).
The configured reviewer sees transient conversation/tool evidence; the gateway
does not persist it. Gateway policy does not reproduce Anthropic's proprietary
classifier calibration, entitlement checks or free-review billing
(`crates/zroutery-core/src/server/pipeline/safeguards/context.rs::review_payload`,
`crates/zroutery-core/src/accounting.rs::AttemptFact`).

## Sources

- `crates/zroutery-core/src/server/pipeline/safeguards/mod.rs::ReviewContext`
- `crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs`
