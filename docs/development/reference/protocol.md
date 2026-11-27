# Protocol Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-27 · Verified by: `cargo test --workspace --all-features --locked`

This page describes the canonical IR, the four client dialects, SSE and the
media/reasoning bridges. It does not define routing; see the routing reference.

## Canonical IR

Every dialect decodes into one request/response model:

- `ChatRequest`, `ChatResponse`, `StreamEvent`, `Usage` and `Dialect`
  (`crates/zroutery-core/src/ir/mod.rs::ChatRequest`,
  `crates/zroutery-core/src/ir/mod.rs::ChatResponse`,
  `crates/zroutery-core/src/ir/mod.rs::StreamEvent`,
  `crates/zroutery-core/src/ir/mod.rs::Usage`,
  `crates/zroutery-core/src/ir/mod.rs::Dialect`);
- content is a list of `ContentBlock`s, including text, images, tool calls and
  thinking (`crates/zroutery-core/src/ir/mod.rs::ContentBlock`);
- tool choice and thinking controls are explicit
  (`crates/zroutery-core/src/ir/mod.rs::ToolChoice`,
  `crates/zroutery-core/src/ir/mod.rs::ThinkingConfig`).

Vendor fields the IR does not understand are carried in `passthrough` rather
than silently dropped (`crates/zroutery-core/src/ir/mod.rs::ChatRequest`).

## Ingress and egress

```text
client body -> decode_request(dialect) -> ChatRequest
ChatRequest -> encode_request(upstream dialect) -> provider body
provider body -> decode_response(upstream dialect) -> ChatResponse
ChatResponse -> encode_response(client dialect) -> client body
```

The dispatch points are `decode_request`, `encode_request`, `decode_response` and
`encode_response` (`crates/zroutery-core/src/protocol/mod.rs::decode_request`,
`crates/zroutery-core/src/protocol/mod.rs::encode_request`,
`crates/zroutery-core/src/protocol/mod.rs::decode_response`,
`crates/zroutery-core/src/protocol/mod.rs::encode_response`).

Each dialect owns its decoder and encoder:

| Dialect | Ingress | Egress |
|---|---|---|
| Anthropic Messages | `crates/zroutery-core/src/protocol/anthropic/request_in.rs::decode_request` | `crates/zroutery-core/src/protocol/anthropic/request_out.rs::encode_request` |
| OpenAI Chat | `crates/zroutery-core/src/protocol/openai/request_in.rs::decode_request` | `crates/zroutery-core/src/protocol/openai/request_out.rs::encode_request` |
| OpenAI Responses | `crates/zroutery-core/src/protocol/responses/request_in.rs::decode_request` | `crates/zroutery-core/src/protocol/responses/request_out.rs::encode_request` |
| Gemini | `crates/zroutery-core/src/protocol/gemini/request_in.rs::decode_request` | `crates/zroutery-core/src/protocol/gemini/request_out.rs::encode_request` |

Anthropic, OpenAI Chat and OpenAI Responses are **upstream** dialects: a
provider's `ApiSurface` selects which OpenAI surface it answers
(`crates/zroutery-core/src/config/provider.rs::ApiSurface`,
`crates/zroutery-core/src/config/registry.rs::ProviderConfig`), and
`encode_request` dispatches on the resulting `Dialect`
(`crates/zroutery-core/src/protocol/mod.rs::encode_request`). Gemini is a client
entry protocol only (`crates/zroutery-core/src/config/provider.rs::ProviderKind`).
For the chat, models and plugin paths, a provider base URL that already ends
in a version segment (`/v1`, or a relay's own `/v2`) is kept as declared and
never grows a second one
(`crates/zroutery-core/src/config/registry.rs::versioned_suffix`). A request
shaped as the OpenCode CLI also carries the environment block in its system
prompt, completed there when the client's own prompt lacks it
(`crates/zroutery-core/src/opencode/mod.rs::ensure_environment_block`), and
a fresh session registers through the ungated models catalog before its first
chat call (`crates/zroutery-core/src/upstream/mod.rs::register_opencode_session`).

The dialect and the client identity are separate choices. Which surface the
request is encoded for is `ApiSurface`; which name the upstream sees is the
provider's client profile, and an `Auto` provider that has no fingerprint for the
caller forwards the caller's own instead of the gateway's
(`crates/zroutery-core/src/config/provider.rs::AUTO_IDENTITY_HEADERS`,
`crates/zroutery-core/src/config/provider.rs::resolve_client_identity`).

## Streaming

Native Messages envelopes preserve nested content and unknown extensions,
signed thinking, model/tool IDs and SSE events; in ordinary encoding only model
routing, the output cap and the explicitly selected API-version migration
reshape the request. Two documented exceptions deliberately change the body:
an upstream that rejects the review extension recovers once by removing that
field and its beta pair, and a thread continuation may replace its
upstream-only carrier id with a renewed binding
(`crates/zroutery-core/src/protocol/anthropic/request_out.rs::encode_request_for`,
`crates/zroutery-core/src/protocol/anthropic/response.rs::decode_native_response`,
`crates/zroutery-core/src/protocol/anthropic/stream_out.rs::AnthropicStreamEncoder`,
`crates/zroutery-core/src/server/pipeline/safeguards/mod.rs::recover_rejected_review`,
`crates/zroutery-core/src/server/pipeline/session_binding.rs::UpstreamSessions`).
Requests with no portable representation remain on Messages upstreams and
return capability-recovery wording instead of silently dropping context,
tool-search or advisor semantics
(`crates/zroutery-core/src/protocol/anthropic/native.rs::validate_bridge`,
`crates/zroutery-core/src/server/pipeline/anthropic_native.rs::registry_for`).

Strict tools and JSON Schema formats have explicit Messages/OpenAI conversions;
Gemini refuses strict tool enforcement rather than claiming an equivalent
(`crates/zroutery-core/src/protocol/anthropic/request_in.rs::decode_request`,
`crates/zroutery-core/src/protocol/anthropic/request_out.rs::encode_request_for`,
`crates/zroutery-core/src/protocol/responses/request_out.rs::encode_request_with`,
`crates/zroutery-core/src/protocol/gemini/request_out.rs::encode_request`).
The effective per-message effort is used for eligibility without changing the
cached top-level config (`crates/zroutery-core/src/protocol/anthropic/request_in.rs::decode_request`).

- `StreamEncoder` turns canonical `StreamEvent`s into the client's SSE frames
  (`crates/zroutery-core/src/protocol/mod.rs::StreamEncoder`).
- A normal terminal is what makes a stream complete. A body that ends without it
  is an interrupted stream, and the output already sent is kept
  (`crates/zroutery-core/src/ir/mod.rs::StreamEvent`).
- An explicit upstream error envelope is an upstream refusal, not malformed
  JSON; the parser keeps bounded `code`/`type`/`message` for classification
  (`crates/zroutery-core/src/error.rs::stream_payload`).

Native completion requires `message_stop`, not only a stop-reason delta; an
error or EOF before it preserves partial output without a synthetic success
(`crates/zroutery-core/src/protocol/anthropic/stream_in.rs::AnthropicStreamParser`,
`crates/zroutery-core/src/upstream/stream.rs::Upstream::stream_with_identity_policy`).
Gateway-owned auto-review results use the same final Messages delta slot;
native results stay unmodified (see [request-purpose.md](request-purpose.md))
(`crates/zroutery-core/src/server/pipeline/safeguards/stream.rs::ReviewFrames::release`).

## Messages versions, threads and counting

Encoding chooses the version actually forwarded: inbound native version,
provider extra header, then provider default. The `2026-10-21` migration drops
retired sampling controls and rejects assistant documents/user citations rather
than rewriting signed history
(`crates/zroutery-core/src/upstream/encode.rs::anthropic_version_for`,
`crates/zroutery-core/src/protocol/anthropic/request_out.rs::apply_version`).
Message-thread continuation is scoped to its original provider, endpoint,
model, account/key, binding version and profile. Changed/expired/conflicting
bindings require full-history reconstruction; bindings are process-local
(`crates/zroutery-core/src/server/pipeline/session_binding.rs::Scope`,
`crates/zroutery-core/src/server/pipeline/session_binding.rs::UpstreamSessions`).
Native token counting forwards the original shape with the routed model and
version, performs no inference settlement, and uses estimation only when no
native route or a 404/405 optional endpoint is available
(`crates/zroutery-core/src/server/pipeline/mod.rs::native_count_tokens`,
`crates/zroutery-core/src/upstream/json.rs::Upstream::count_messages_tokens`).

## Usage normalisation

The IR invariant is that `input_tokens` is the total prompt and the cache
counters are subsets of it; `output_tokens` includes reasoning.

- Anthropic reports `input_tokens` cache-exclusive, so the decoder adds cache
  reads and writes back into the total
  (`crates/zroutery-core/src/protocol/anthropic/response.rs::decode_response`).
- OpenAI reports prompt tokens with cached tokens as a detail; the decoder keeps
  the subset relation (`crates/zroutery-core/src/protocol/openai/response.rs::decode_response`).
- Gemini's counters are mapped into the same shape
  (`crates/zroutery-core/src/protocol/gemini/response.rs::decode_response`).

A relay that reports a cache count larger than its prompt total is saturated at
zero by `crates/zroutery-core/src/ir/mod.rs::Usage::fresh_input_tokens`, never wrapped
(`crates/zroutery-core/src/ir/mod.rs::Usage::fresh_input_tokens`).

## Reasoning bridge

- Effort is decoded and encoded through the bridge, not copied as free text
  (`crates/zroutery-core/src/protocol/reasoning_bridge.rs::decode_effort`,
  `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_effort`).
- `encode_thinking_block` filters by block shape (a signed thinking block or a
  redacted one) and builds the Responses reasoning item; deciding whether the
  target dialect accepts it is the caller's job
  (`crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_thinking_block`).
- An explicit client disable clears thinking; the pipeline does not re-enable it
  for a candidate (`crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`).
- Gemini thinking round-trips: `thought` parts decode into thinking blocks and
  encode back, and `thoughtSignature` is forwarded only for Gemini-origin
  history; streaming emits thinking deltas with their signature
  (`crates/zroutery-core/src/protocol/gemini/request_in.rs::decode_request`,
  `crates/zroutery-core/src/protocol/gemini/request_out.rs::encode_request`,
  `crates/zroutery-core/src/protocol/gemini/stream_in.rs`).
- An unsigned or empty-signature thinking block is emitted to Responses as a
  plain reasoning summary item instead of being policy-replaced or rejected
  (`crates/zroutery-core/src/protocol/responses/request_out.rs::encode_request`).

## Media and vision fallback

- `crates/zroutery-core/src/media/collect.rs::collect` enumerates image slots in a request
  (`crates/zroutery-core/src/media/collect.rs::collect`).
- When the selected model cannot accept an image, the configured vision target
  describes it and the description replaces the slot
  (`crates/zroutery-core/src/media/vision.rs::resolve`,
  `crates/zroutery-core/src/media/transform.rs::replace`).
- Without a configured vision target, each image slot is replaced with the
  configured text placeholder and the request is sent: the content is not
  silently dropped, but the request is not refused either
  (`crates/zroutery-core/src/server/pipeline/mod.rs::apply_vision_fallback_inner`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| unsupported upstream dialect | Gemini selected as an upstream | refused; Gemini is a client-entry protocol only (`crates/zroutery-core/src/config/provider.rs::ProviderKind`) |
| malformed body | bad provider payload | protocol failure (`crates/zroutery-core/src/error.rs::safe_message`) |
| stream ends without a terminal | truncation | `InterruptedStream`, keep sent output (`crates/zroutery-core/src/ir/mod.rs::StreamEvent`) |

## Not implemented

- No upstream Gemini dialect; Gemini is client-facing only. Responses exists on
  both sides: as a client entry and as an upstream `ApiSurface`
  (`crates/zroutery-core/src/config/provider.rs::ApiSurface`).
- No generic unknown-field translation between dialects; passthrough is retained
  where the IR has a slot.
- No image generation.

## Sources

- `crates/zroutery-core/src/ir/mod.rs::ChatRequest`
- `crates/zroutery-core/src/ir/mod.rs::ChatResponse`
- `crates/zroutery-core/src/ir/mod.rs::StreamEvent`
- `crates/zroutery-core/src/ir/mod.rs::Usage`
- `crates/zroutery-core/src/ir/mod.rs::Usage::fresh_input_tokens`
- `crates/zroutery-core/src/ir/mod.rs::ContentBlock`
- `crates/zroutery-core/src/ir/mod.rs::Dialect`
- `crates/zroutery-core/src/protocol/mod.rs::decode_request`
- `crates/zroutery-core/src/protocol/mod.rs::encode_request`
- `crates/zroutery-core/src/protocol/mod.rs::decode_response`
- `crates/zroutery-core/src/protocol/mod.rs::encode_response`
- `crates/zroutery-core/src/protocol/mod.rs::StreamEncoder`
- `crates/zroutery-core/src/protocol/anthropic/response.rs::decode_response`
- `crates/zroutery-core/src/protocol/openai/response.rs::decode_response`
- `crates/zroutery-core/src/protocol/responses/request_in.rs::decode_request`
- `crates/zroutery-core/src/protocol/responses/request_out.rs::encode_request`
- `crates/zroutery-core/src/protocol/gemini/response.rs::decode_response`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::decode_effort`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_effort`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_thinking_block`
- `crates/zroutery-core/src/media/collect.rs::collect`
- `crates/zroutery-core/src/media/vision.rs::resolve`
- `crates/zroutery-core/src/media/transform.rs::replace`
- `crates/zroutery-core/src/server/pipeline/mod.rs::apply_vision_fallback_inner`
- `crates/zroutery-core/src/error.rs::safe_message`
- `crates/zroutery-core/src/error.rs::stream_payload`
- `crates/zroutery-core/src/config/provider.rs::ApiSurface`
- `crates/zroutery-core/src/config/registry.rs::ProviderConfig`
- `crates/zroutery-core/src/config/provider.rs::ProviderKind`
- `crates/zroutery-core/src/protocol/gemini/request_in.rs::decode_request`
- `crates/zroutery-core/src/protocol/gemini/request_out.rs::encode_request`
- `crates/zroutery-core/src/protocol/gemini/stream_in.rs`
- `crates/zroutery-core/src/protocol/responses/request_out.rs::encode_request`
- `crates/zroutery-core/tests/thinking_history_test.rs`
