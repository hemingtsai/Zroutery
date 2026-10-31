//! Non-streaming responses, and the one place the wire usage shape lives.

use serde_json::{json, Map, Value};

use super::request_in::decode_reasoning_blocks;
use super::request_out::arguments_json;
use crate::error::{Error, Result};
use crate::ir::{ChatResponse, ContentBlock, StopReason, Usage};
use crate::protocol::reasoning_bridge;
use crate::protocol::{explicit_placeholder, unsupported_upstream_content};

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if let Some(error) = Error::session_binding_payload(
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or("upstream"),
        &body,
    ) {
        return Err(error);
    }
    // `error` is nullable on a normal Response: only a non-null error object
    // means the upstream failed.
    if let Some(err) = body.get("error").filter(|err| !err.is_null()) {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown upstream error");
        return Err(Error::BadUpstreamPayload(msg.to_string()));
    }
    let mut content = Vec::new();
    let output = match body.get("output") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(items)) => items.as_slice(),
        Some(_) => {
            return Err(Error::BadUpstreamPayload(
                "Responses output must be an array".into(),
            ));
        }
    };
    for item in output {
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadUpstreamPayload("response item is missing `type`".into()))?;
        match item_type {
            "message" => {
                let parts = item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        Error::BadUpstreamPayload("response message is missing `content`".into())
                    })?;
                for part in parts {
                    let part_type = part.get("type").and_then(Value::as_str).ok_or_else(|| {
                        Error::BadUpstreamPayload("response message part is missing `type`".into())
                    })?;
                    match part_type {
                        "output_text" => {
                            let text =
                                part.get("text").and_then(Value::as_str).ok_or_else(|| {
                                    Error::BadUpstreamPayload(
                                        "output_text is missing `text`".into(),
                                    )
                                })?;
                            content.push(ContentBlock::text(text));
                        }
                        _ => {
                            return Err(unsupported_upstream_content("Responses response message"));
                        }
                    }
                }
            }
            "function_call" => {
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::BadUpstreamPayload("function_call is missing `arguments`".into())
                    })?;
                let id = item.get("call_id").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("function_call is missing `call_id`".into())
                })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("function_call is missing `name`".into())
                })?;
                content.push(ContentBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: serde_json::from_str(arguments)
                        .unwrap_or(Value::String(arguments.to_string())),
                });
            }
            "reasoning" => {
                let blocks = decode_reasoning_blocks(item);
                if blocks.is_empty() {
                    return Err(Error::BadUpstreamPayload(
                        "unsupported or empty reasoning item".into(),
                    ));
                }
                content.extend(blocks);
            }
            _ => {
                return Err(unsupported_upstream_content("Responses response item"));
            }
        }
    }

    let mut passthrough = Map::new();
    if let Some(prev_id) = body.get("previous_response_id").and_then(Value::as_str) {
        passthrough.insert("previous_response_id".to_string(), json!(prev_id));
    }

    if body.get("status").and_then(Value::as_str) == Some("incomplete") {
        if let Some(reason) = body
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            passthrough.insert("incomplete_details".to_string(), json!({"reason": reason}));
        }
    }

    let has_tool_call = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("resp_unknown")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content,
        stop_reason: if body.get("status").and_then(Value::as_str) == Some("incomplete") {
            StopReason::MaxTokens
        } else if has_tool_call {
            // A response that carries function calls ended for a tool round.
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        },
        stop_sequence: None,
        usage: decode_usage(body.get("usage")),
        passthrough,
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    let mut output = Vec::new();
    for block in &resp.content {
        match block {
            ContentBlock::Text { text, .. } => output.push(json!({
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": text, "annotations": []}],
            })),
            ContentBlock::ToolUse { id, name, input } => output.push(json!({
                "type": "function_call",
                "id": format!("fc_{id}"),
                "call_id": id,
                "name": name,
                "arguments": arguments_json(input),
                "status": "completed",
            })),
            ContentBlock::Thinking { text, signature } => {
                let mut item = json!({
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": text}],
                });
                if let Some(block) = reasoning_bridge::encode_thinking_block(block) {
                    if let Some(encrypted) = block.get("encrypted_content") {
                        item["encrypted_content"] = encrypted.clone();
                    }
                }
                let _ = signature;
                output.push(item);
            }
            ContentBlock::RedactedThinking { .. } => {
                if let Some(item) = reasoning_bridge::encode_thinking_block(block) {
                    output.push(item);
                }
            }
            ContentBlock::Image { .. }
            | ContentBlock::Document { .. }
            | ContentBlock::File { .. }
            | ContentBlock::Audio { .. }
            | ContentBlock::Video { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Citation { .. }
            | ContentBlock::Annotation { .. } => {
                if let Some(text) = explicit_placeholder(block).as_text() {
                    output.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{"type": "output_text", "text": text, "annotations": []}],
                    }));
                }
            }
        }
    }
    let text = resp.text();
    let status = if resp.stop_reason == StopReason::MaxTokens {
        "incomplete"
    } else {
        "completed"
    };
    let mut resp_json = json!({
        "id": resp.id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": status,
        "model": resp.model,
        "output": output,
        "output_text": text,
        "usage": encode_usage(&resp.usage),
    });
    if let Some(prev_id) = resp.passthrough.get("previous_response_id") {
        resp_json["previous_response_id"] = prev_id.clone();
    }
    if let Some(metadata) = resp.passthrough.get("metadata") {
        resp_json["metadata"] = metadata.clone();
    }
    if status == "incomplete" {
        if let Some(details) = resp.passthrough.get("incomplete_details") {
            resp_json["incomplete_details"] = details.clone();
        } else {
            resp_json["incomplete_details"] = json!({"reason": "max_output_tokens"});
        }
    }
    resp_json
}

/// Decode an OpenAI Responses `usage` object into the IR contract.
///
/// `input_tokens` is already the cache-*inclusive* prompt total and
/// `input_tokens_details.cached_tokens` is a subset of it, so the total passes
/// through and the read part is clamped to it.
pub(super) fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(u) = v.filter(|u| !u.is_null()) else {
        return Usage::default();
    };
    let input_tokens = u
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let cache_read_tokens = u
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    Usage {
        input_tokens,
        output_tokens: u
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_read_tokens: cache_read_tokens.min(input_tokens),
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
    }
}

/// Encode the IR contract to the Responses cache-inclusive total.
pub(super) fn encode_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
        "total_tokens": u.total(),
        "input_tokens_details": {"cached_tokens": u.cache_read_tokens.min(u.input_tokens)},
        "output_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
    })
}
