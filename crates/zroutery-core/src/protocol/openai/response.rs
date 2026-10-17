//! Non-streaming chat completions, the stop-reason mapping, and the wire usage shape.

use serde_json::{json, Map, Value};

use super::request_in::{
    arguments_json, decode_reasoning_value, decode_tool_call, flatten_content,
};
use crate::error::{Error, Result};
use crate::ir::{ChatResponse, ContentBlock, StopReason, Usage};
use crate::protocol::explicit_placeholder;

// -------------------------------------------------------------------- responses

pub(super) fn finish_reason_from_str(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

pub(super) fn finish_reason_to_str(r: StopReason) -> Option<&'static str> {
    match r {
        StopReason::EndTurn | StopReason::StopSequence => Some("stop"),
        StopReason::MaxTokens => Some("length"),
        StopReason::ToolUse => Some("tool_calls"),
        StopReason::Refusal => Some("content_filter"),
        StopReason::Unknown => None,
    }
}

/// Decode an OpenAI `usage` object into the IR contract.
///
/// `prompt_tokens` is already a cache-*inclusive* total and
/// `prompt_tokens_details.cached_tokens` (or DeepSeek's
/// `prompt_cache_hit_tokens`) is the read part, so the total passes through and
/// only the read is clamped to it. A relay that reports a cache count larger
/// than its own prompt total cannot break the IR subset invariant.
pub(super) fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(u) = v.filter(|u| !u.is_null()) else {
        return Usage::default();
    };
    let input_tokens = u
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let cache_read_tokens = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| u.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    Usage {
        input_tokens,
        output_tokens: u
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_read_tokens: cache_read_tokens.min(input_tokens),
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
    }
}

/// Encode the IR contract to OpenAI's cache-inclusive total.
///
/// `prompt_tokens` is the full IR prompt total (fresh + cache read + cache
/// write) and `cached_tokens` is only the read part, so the value a client sees
/// can never exceed the total it is nested in.
pub(super) fn encode_usage(u: &Usage) -> Value {
    json!({
        "prompt_tokens": u.input_tokens,
        "completion_tokens": u.output_tokens,
        "total_tokens": u.total(),
        "prompt_tokens_details": {"cached_tokens": u.cache_read_tokens.min(u.input_tokens)},
        "completion_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
    })
}

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if let Some(err) = body.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown upstream error");
        return Err(Error::BadUpstreamPayload(msg.to_string()));
    }
    let choices = body
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::BadUpstreamPayload("response has no choices".into()))?;
    if choices.len() > 1 {
        return Err(Error::BadUpstreamPayload(
            "multiple response choices are not representable".into(),
        ));
    }
    let choice = choices
        .first()
        .ok_or_else(|| Error::BadUpstreamPayload("response has no choices".into()))?;
    let msg = choice
        .get("message")
        .ok_or_else(|| Error::BadUpstreamPayload("choice has no message".into()))?;

    let mut content = Vec::new();
    if let Some(reasoning) = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
    {
        decode_reasoning_value(reasoning, &mut content, "reasoning_content")?;
    }
    if let Some(text) = flatten_content(msg.get("content"))?.filter(|t| !t.is_empty()) {
        content.push(ContentBlock::text(text));
    }
    if let Some(calls_value) = msg.get("tool_calls") {
        let calls = calls_value
            .as_array()
            .ok_or_else(|| Error::BadUpstreamPayload("tool_calls must be an array".into()))?;
        for call in calls {
            content.push(decode_tool_call(call)?);
        }
    }
    let mut passthrough = Map::new();
    if let Some(refusal) = msg
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|refusal| !refusal.is_empty())
    {
        // A refusal is a legal answer whose `content` is null. The text joins
        // the content so every dialect can show it, and the field is kept so a
        // same-dialect re-encode can restore OpenAI's own shape instead of
        // turning the answer into an empty success.
        content.push(ContentBlock::text(refusal));
        passthrough.insert("refusal".into(), json!(refusal));
    }

    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl_unknown")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content,
        stop_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(finish_reason_from_str)
            .unwrap_or(StopReason::Unknown),
        stop_sequence: None,
        usage: decode_usage(body.get("usage")),
        passthrough,
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for b in &resp.content {
        match b {
            ContentBlock::Text { text: t, .. } => text.push_str(t),
            ContentBlock::Thinking { text: t, .. } => reasoning.push_str(t),
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "index": tool_calls.len(),
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments_json(input)},
            })),
            ContentBlock::Image { .. }
            | ContentBlock::Document { .. }
            | ContentBlock::File { .. }
            | ContentBlock::Audio { .. }
            | ContentBlock::Video { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::RedactedThinking { .. }
            | ContentBlock::Citation { .. }
            | ContentBlock::Annotation { .. } => {
                if let Some(placeholder_text) = explicit_placeholder(b).as_text() {
                    text.push_str(placeholder_text);
                }
            }
        }
    }

    // OpenAI reports a refusal as `content: null` plus a `refusal` string.
    // Keep that shape when the refusal text is the whole answer, and keep the
    // text as content otherwise so a mixed answer loses nothing.
    let refusal = resp
        .passthrough
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|refusal| !refusal.is_empty());
    let refusal_is_the_answer = refusal.is_some_and(|refusal| refusal == text);

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if refusal_is_the_answer || (text.is_empty() && !tool_calls.is_empty()) {
            Value::Null
        } else {
            json!(text)
        },
    );
    if let Some(refusal) = refusal.filter(|_| refusal_is_the_answer) {
        message.insert("refusal".into(), json!(refusal));
    }
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    json!({
        "id": resp.id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": resp.model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "logprobs": Value::Null,
            "finish_reason": finish_reason_to_str(resp.stop_reason).unwrap_or("stop"),
        }],
        "usage": encode_usage(&resp.usage),
    })
}
