# Rectifier Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-27 · Verified by: `cargo test --workspace --all-features --locked`

A rectifier repairs a request the upstream refused, before the gateway gives up
on that provider. It is not a retry of the same bytes: it changes the request
shape, then sends again.

## Contract

- `Rectifier` is stateless: `should_apply(error, body)` decides, `rectify(body)`
  mutates, and `name()` is the tried-set key
  (`crates/zroutery-core/src/rectifier/mod.rs::Rectifier`).
- `RectifyResult` reports whether the body changed and why
  (`crates/zroutery-core/src/rectifier/mod.rs::RectifyResult`).
- `from_config` builds the enabled list in a fixed order: thinking signature,
  media fallback, thinking budget, Codex metadata
  (`crates/zroutery-core/src/rectifier/mod.rs::from_config`).
- A provider can override the global `RectifierConfig`; the resolved config is
  what the repair loop uses
  (`crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`).

## Repair loop

- Each enabled rectifier that applies is retried against the same candidate for
  up to three failed sends; a rectifier that keeps failing is abandoned and the
  next one is tried (`crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`).
- A rectifier retry is **not** reported to the circuit breaker. It is the same
  provider and model, and the failure was a request-shape problem, not provider
  health.
- Every actual upstream send opens and settles its own attempt; a successful
  send is left open for the caller to settle with the answer
  (`crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`).
- A decision candidate (`provider.kind.is_decision()`) is not repaired here; it
  has its own path.
- A native-only Messages request skips media/signature rewrites; its inspection
  projection does not own the original signed conversation
  (`crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`,
  `crates/zroutery-core/src/server/pipeline/mod.rs::shape_for_candidate`).
- An explicit pre-output `safeguards` rejection can recover once by removing
  that review field and its beta pair, retaining system/tools/history and all
  other beta values; the gateway then supplies real review results
  (`crates/zroutery-core/src/server/pipeline/safeguards/mod.rs::recover_rejected_review`,
  `crates/zroutery-core/src/upstream/headers.rs::is_server_review_beta`).
- The OpenCode shaping profile's client UA and request id are fabricated by the
  host, never forwarded from the inbound client; the UA version must track
  OpenCode's own updater, since the free tier rejects stale versions with a
  quota-flavored 429
  (`crates/zroutery-core/src/upstream/headers.rs::OPENCODE_USER_AGENT`). The
  session id is the one exception: an inbound id that already has the canonical
  shape is forwarded verbatim for its upstream prompt-cache affinity, and only
  a foreign conversation signal is derived into a fresh session
  (`crates/zroutery-core/src/server/pipeline/client.rs::client_for`,
  `crates/zroutery-core/src/opencode/mod.rs::session_from_signal`).

## The repairs

| Rectifier | Trigger | Change | Risk |
|---|---|---|---|
| thinking signature | invalid/extra/missing thinking or signature blocks | strips the offending thinking/redacted-thinking blocks and signature fields | broad keyword match; could fire on a message that merely mentions the words |
| media fallback | 400/415/422/501 with "image"/"vision"/"multimodal" + unsupported | replaces image blocks with a text placeholder, preserving message structure | loses image detail unless a vision target describes it |
| thinking budget | "budget" + thinking/reasoning, or an explicit maximum | halves `thinking.budget_tokens`, floor 1024 | may still exceed a provider's allowance |
| Codex metadata | 400/422 + unknown parameter/field naming `internal_chat_message_metadata_passthrough` | removes that one transport field from Responses items | must not touch ordinary metadata or nested tool arguments |

Sources: `crates/zroutery-core/src/rectifier/thinking_signature.rs::ThinkingSignatureRectifier`,
`crates/zroutery-core/src/rectifier/media_fallback.rs::MediaFallbackRectifier`,
`crates/zroutery-core/src/rectifier/thinking_budget.rs::ThinkingBudgetRectifier`,
`crates/zroutery-core/src/rectifier/codex_metadata.rs::CodexMetadataRectifier`.

## Media upgrade

When the upstream rejects media, the repair path first tries the configured
vision model: it describes the images and replaces them with those descriptions.
If that is unavailable, it uses an honest text placeholder rather than deleting
the blocks. The same provider is retried, because the upstream rejected the
image, not the question (`crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`).

## Codex Auto

Codex Auto is a streaming policy, not a `Rectifier` implementor:

- it stages Responses SSE and conditionally repairs history;
- `MAX_RETRIES` is 20, the deadline is 1800 s, idle timeout 120 s;
- buffer limit is 64 MiB, error limit 64 KiB
  (`crates/zroutery-core/src/rectifier/codex_auto.rs::MAX_RETRIES`,
  `crates/zroutery-core/src/rectifier/codex_auto.rs::DEADLINE`,
  `crates/zroutery-core/src/rectifier/codex_auto.rs::IDLE_TIMEOUT`,
  `crates/zroutery-core/src/rectifier/codex_auto.rs::BUFFER_LIMIT`);
- it keeps process-local counters only: requests, retries, removed reasoning
  items, cleanup refusals, cancellations. No prompt, response or credential
  (`crates/zroutery-core/src/rectifier/codex_auto.rs::FilterStats`).

It is enabled by the config's `codex_auto` switch, separate from the four repair
cases (`crates/zroutery-core/src/config/routing.rs::RectifierConfig`).

## Configuration

`RectifierConfig` defaults every switch to true: `enabled`,
`thinking_signature`, `media_fallback`, `thinking_budget`, `thinking_fallback`,
`codex_metadata`, `codex_auto`
(`crates/zroutery-core/src/config/routing.rs::RectifierConfig`).

`thinking_fallback` is not a rectifier: it lets a reasoning request run on a
model whose thinking support is unknown, with the controls removed and the
attempt marked degraded. It is visible in the route decision, never silent.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| the repair is refused | no rectifier matched | ordinary failover (`crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`) |
| the wrong repair fires | broad keyword match | narrow the signature; a rectifier that still fails is abandoned after three sends |
| vision is unavailable | no target configured | placeholders, not silent deletion (`crates/zroutery-core/src/media/vision.rs::resolve`) |

## Not implemented

- No arbitrary request rewrite; each repair is keyed to a provider signature.
- No guarantee a repair fixes the request; a second rejection falls through to
  ordinary failover.
- No rectifier retry counted against provider health.

## Sources

- `crates/zroutery-core/src/rectifier/mod.rs::Rectifier`
- `crates/zroutery-core/src/rectifier/mod.rs::RectifyResult`
- `crates/zroutery-core/src/rectifier/mod.rs::from_config`
- `crates/zroutery-core/src/rectifier/thinking_signature.rs::ThinkingSignatureRectifier`
- `crates/zroutery-core/src/rectifier/media_fallback.rs::MediaFallbackRectifier`
- `crates/zroutery-core/src/rectifier/thinking_budget.rs::ThinkingBudgetRectifier`
- `crates/zroutery-core/src/rectifier/codex_metadata.rs::CodexMetadataRectifier`
- `crates/zroutery-core/src/rectifier/codex_auto.rs::MAX_RETRIES`
- `crates/zroutery-core/src/rectifier/codex_auto.rs::DEADLINE`
- `crates/zroutery-core/src/rectifier/codex_auto.rs::IDLE_TIMEOUT`
- `crates/zroutery-core/src/rectifier/codex_auto.rs::BUFFER_LIMIT`
- `crates/zroutery-core/src/rectifier/codex_auto.rs::FilterStats`
- `crates/zroutery-core/src/config/routing.rs::RectifierConfig`
- `crates/zroutery-core/src/server/pipeline/repair.rs::try_rectify_buffered`
