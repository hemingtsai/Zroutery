//! Stop reasons, usage, and the response body in both directions.

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::ir::{ChatResponse, StopReason, Usage};

use super::request_in::decode_block;
use super::request_out::encode_block;

// -------------------------------------------------------------------- responses

pub fn stop_reason_from_str(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "refusal" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

pub fn stop_reason_to_str(r: StopReason) -> Option<&'static str> {
    match r {
        StopReason::EndTurn => Some("end_turn"),
        StopReason::MaxTokens => Some("max_tokens"),
        StopReason::StopSequence => Some("stop_sequence"),
        StopReason::ToolUse => Some("tool_use"),
        StopReason::Refusal => Some("refusal"),
        StopReason::Unknown => None,
    }
}

/// Read one Anthropic counter, distinguishing "absent" from "zero".
///
/// `message_delta` restates cumulative counters one field at a time, so the
/// parser has to know which fields the frame actually carried.
pub(super) fn usage_counter(u: &Value, key: &str) -> Option<u32> {
    u.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
}

/// Decode an Anthropic `usage` object into the IR contract.
///
/// Anthropic reports `input_tokens` as the cache-*exclusive* prompt size and the
/// two cache counters alongside it. The IR total is their sum, so a cache hit is
/// not lost and `cache_read_tokens` can never exceed `input_tokens`.
pub(crate) fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(u) = v else { return Usage::default() };
    let cache_read_tokens = usage_counter(u, "cache_read_input_tokens").unwrap_or(0);
    let cache_write_tokens = usage_counter(u, "cache_creation_input_tokens").unwrap_or(0);
    let total = usage_counter(u, "input_tokens").unwrap_or(0) as u64
        + cache_read_tokens as u64
        + cache_write_tokens as u64;
    Usage {
        input_tokens: total.min(u32::MAX as u64) as u32,
        output_tokens: usage_counter(u, "output_tokens").unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: 0,
    }
}

/// Encode the IR contract back to Anthropic's cache-exclusive shape.
///
/// The total is split back into fresh input plus the two cache counters, so a
/// usage produced by any decoder round-trips to the same Anthropic numbers.
pub(crate) fn encode_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.fresh_input_tokens(),
        "output_tokens": u.output_tokens,
        "cache_read_input_tokens": u.cache_read_tokens,
        "cache_creation_input_tokens": u.cache_write_tokens,
    })
}

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if body.get("type").and_then(Value::as_str) == Some("error") {
        let msg = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown upstream error");
        return Err(Error::BadUpstreamPayload(msg.to_string()));
    }
    let content = match body.get("content") {
        Some(Value::Array(blocks)) => {
            let mut out = Vec::new();
            for b in blocks {
                if let Some(block) = decode_block(b)? {
                    out.push(block);
                }
            }
            out
        }
        None | Some(Value::Null) => Vec::new(),
        Some(_) => {
            return Err(Error::BadUpstreamPayload(
                "Anthropic response content must be an array".into(),
            ));
        }
    };
    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("msg_unknown")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content,
        stop_reason: body
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(stop_reason_from_str)
            .unwrap_or(StopReason::Unknown),
        stop_sequence: body
            .get("stop_sequence")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: decode_usage(body.get("usage")),
        passthrough: Map::new(),
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": resp.model,
        "content": Value::Array(resp.content.iter().map(encode_block).collect()),
        "stop_reason": stop_reason_to_str(resp.stop_reason),
        "stop_sequence": resp.stop_sequence,
        "usage": encode_usage(&resp.usage),
    })
}
