//! Stop reasons, usage, and the response body in both directions.

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::ir::{ChatResponse, ContentBlock, StopReason, Usage};
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
                    content.push(ContentBlock::text(text));
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
        passthrough: Map::new(),
    })
}

/// Decode a Gemini `usageMetadata` object into the IR contract.
///
/// Gemini's `promptTokenCount` is the total effective prompt size and already
/// includes `cachedContentTokenCount`, which is the cached (read) subset. The
/// cached count is clamped to the total so the IR subset invariant holds even
/// for a relay that reports the two inconsistently.
pub(super) fn decode_usage_metadata(u: &Value) -> Usage {
    let counter = |key: &str| {
        u.get(key)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32
    };
    let input_tokens = counter("promptTokenCount");
    Usage {
        input_tokens,
        output_tokens: counter("candidatesTokenCount"),
        cache_read_tokens: counter("cachedContentTokenCount").min(input_tokens),
        cache_write_tokens: 0,
        reasoning_tokens: counter("thoughtsTokenCount"),
    }
}

/// Encode the IR contract to Gemini's cache-inclusive `usageMetadata`.
pub(super) fn encode_usage(u: &Usage) -> Value {
    json!({
        "promptTokenCount": u.input_tokens,
        "candidatesTokenCount": u.output_tokens,
        "totalTokenCount": u.total(),
        "cachedContentTokenCount": u.cache_read_tokens.min(u.input_tokens),
        "thoughtsTokenCount": u.reasoning_tokens,
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    let parts: Vec<Value> = resp
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text, .. } => json!({"text": text}),
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
                StopReason::MaxTokens => "MAX_TOKENS",
                StopReason::Refusal => "SAFETY",
                _ => "STOP",
            },
        }],
        "usageMetadata": encode_usage(&resp.usage),
    })
}
