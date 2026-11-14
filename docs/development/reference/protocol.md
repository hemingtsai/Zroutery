# Protocol Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

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
| OpenAI Chat/Responses | `crates/zroutery-core/src/protocol/openai/request_in.rs::decode_request` | `crates/zroutery-core/src/protocol/openai/request_out.rs::encode_request` |
| Gemini | `crates/zroutery-core/src/protocol/gemini/request_in.rs::decode_request` | `crates/zroutery-core/src/protocol/gemini/request_out.rs::encode_request` |

Only Anthropic and OpenAI-compatible are **upstream** dialects; Gemini and
Responses are client entry protocols (`crates/zroutery-core/src/config/provider.rs::ProviderKind`).

## Streaming

- `StreamEncoder` turns canonical `StreamEvent`s into the client's SSE frames
  (`crates/zroutery-core/src/protocol/mod.rs::StreamEncoder`).
- A normal terminal is what makes a stream complete. A body that ends without it
  is an interrupted stream, and the output already sent is kept
  (`crates/zroutery-core/src/ir/mod.rs::StreamEvent`).
- An explicit upstream error envelope is an upstream refusal, not malformed
  JSON; the parser keeps bounded `code`/`type`/`message` for classification
  (`crates/zroutery-core/src/error.rs::stream_payload`).

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
- A thinking block is encoded only for a dialect that accepts it
  (`crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_thinking_block`).
- An explicit client disable clears thinking; the pipeline does not re-enable it
  for a candidate (`crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`).

## Media and vision fallback

- `crates/zroutery-core/src/media/collect.rs::collect` enumerates image slots in a request
  (`crates/zroutery-core/src/media/collect.rs::collect`).
- When the selected model cannot accept an image, the configured vision target
  describes it and the description replaces the slot
  (`crates/zroutery-core/src/media/vision.rs::resolve`,
  `crates/zroutery-core/src/media/transform.rs::replace`).
- Without a reachable vision target, the image is not silently dropped; the
  capability boundary fails closed.

## Not implemented

- No upstream Gemini or Responses dialect; those are client-facing only.
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
- `crates/zroutery-core/src/protocol/gemini/response.rs::decode_response`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::decode_effort`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_effort`
- `crates/zroutery-core/src/protocol/reasoning_bridge.rs::encode_thinking_block`
- `crates/zroutery-core/src/media/collect.rs::collect`
- `crates/zroutery-core/src/media/vision.rs::resolve`
- `crates/zroutery-core/src/media/transform.rs::replace`
- `crates/zroutery-core/src/config/provider.rs::ProviderKind`
