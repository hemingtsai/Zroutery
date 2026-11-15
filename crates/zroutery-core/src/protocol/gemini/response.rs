//! Stop reasons, usage, and the response body in both directions.

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::ir::{
    capture_declaration_in_passthrough, declaration_from_wire, ChatResponse, ContentBlock,
    StopReason, Usage,
};
use crate::protocol::explicit_placeholder;
// ---------------------------------------------------------------- responses

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if let Some(err) = body.get("error") {
        return Err(Error::BadUpstreamPayload(
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown Gemini error")
                .to_string(),
        ));
    }
    let candidate = match body.get("candidates") {
        None | Some(Value::Null) => None,
        Some(Value::Array(candidates)) => candidates.first(),
        Some(_) => {
            return Err(Error::BadUpstreamPayload(
                "Gemini candidates must be an array".into(),
            ));
        }
    };
    let mut content = Vec::new();
    let mut stop_reason = StopReason::Unknown;
    if let Some(candidate) = candidate {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        content.push(ContentBlock::Thinking {
                            text: text.to_string(),
                            signature: part
                                .get("thoughtSignature")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        });
                    } else {
                        content.push(ContentBlock::text(text));
                    }
                } else if let Some(call) = part.get("functionCall") {
                    let id = call.get("id").and_then(Value::as_str).ok_or_else(|| {
                        Error::BadUpstreamPayload("functionCall is missing `id`".into())
                    })?;
                    let name = call.get("name").and_then(Value::as_str).ok_or_else(|| {
                        Error::BadUpstreamPayload("functionCall is missing `name`".into())
                    })?;
                    content.push(ContentBlock::ToolUse {
                        id: id.to_string(),
                        name: name.to_string(),
                        input: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    });
                } else {
                    return Err(Error::BadUpstreamPayload(
                        "unsupported Gemini response part".into(),
                    ));
                }
            }
        }
        stop_reason = match candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "STOP" => StopReason::EndTurn,
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" => StopReason::Refusal,
            _ => StopReason::Unknown,
        };
    }
    let usage = body
        .get("usageMetadata")
        .map(decode_usage_metadata)
        .unwrap_or_default();

    // Keep the upstream's own structured declaration on the response, before
    // anything rewrites `model` to the client-facing exposed id. The complete
    // classified structure travels — both axes and any disagreement between
    // the camel and snake version spellings — and the lifecycle reads it off
    // the internal key; the client never sees it.
    let mut passthrough = Map::new();
    capture_declaration_in_passthrough(&mut passthrough, &declaration_from_wire(&body));

    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("gemini_unknown")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content,
        stop_reason,
        stop_sequence: None,
        usage,
        passthrough,
    })
}

/// Decode a Gemini `usageMetadata` object into the IR contract.
///
/// Gemini's `promptTokenCount` is the total effective prompt size and already
/// includes `cachedContentTokenCount`, which is the cached (read) subset. The
/// cached count is clamped to the total so the IR subset invariant holds even
/// for a relay that reports the two inconsistently.
///
/// Candidate text and thinking are separate output counters:
/// `candidatesTokenCount` does not include `thoughtsTokenCount`, and the
/// grand total of a thinking reply is prompt + candidates + thoughts. Both
/// counters are billed output, and the IR keeps reasoning tokens inside
/// `output_tokens`, so the decoded output total is their sum. The raw usage
/// capture reports the same number; settlement must not see two different
/// totals for one wire object.
pub(super) fn decode_usage_metadata(u: &Value) -> Usage {
    let counter = |key: &str| {
        u.get(key)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32
    };
    let input_tokens = counter("promptTokenCount");
    let candidates = counter("candidatesTokenCount");
    let thoughts = counter("thoughtsTokenCount");
    let output_tokens = candidates.saturating_add(thoughts);
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: counter("cachedContentTokenCount").min(input_tokens),
        cache_write_tokens: 0,
        reasoning_tokens: thoughts.min(output_tokens),
    }
}

/// Encode the IR contract to Gemini's cache-inclusive `usageMetadata`.
///
/// The inverse of the decode above: the IR output total already contains the
/// reasoning subset, so the wire carries the two counters Gemini bills from —
/// candidate text plus thoughts — and no thought is counted twice.
pub(super) fn encode_usage(u: &Usage) -> Value {
    let reasoning = u.reasoning_tokens.min(u.output_tokens);
    json!({
        "promptTokenCount": u.input_tokens,
        "candidatesTokenCount": u.output_tokens - reasoning,
        "totalTokenCount": u.total(),
        "cachedContentTokenCount": u.cache_read_tokens.min(u.input_tokens),
        "thoughtsTokenCount": reasoning,
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    let parts: Vec<Value> = resp
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text, .. } => json!({"text": text}),
            ContentBlock::Thinking { text, .. } => json!({"text": text, "thought": true}),
            ContentBlock::ToolUse { id, name, input } => json!({
                "functionCall": {"id": id, "name": name, "args": input}
            }),
            _ => explicit_placeholder(b)
                .as_text()
                .map(|text| json!({"text": text}))
                .unwrap_or_else(|| json!({"text": "[Unsupported content]"})),
        })
        .collect();
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": match resp.stop_reason {
                StopReason::MaxTokens | StopReason::ModelContextWindowExceeded => "MAX_TOKENS",
                StopReason::Refusal => "SAFETY",
                _ => "STOP",
            },
        }],
        "usageMetadata": encode_usage(&resp.usage),
    })
}
