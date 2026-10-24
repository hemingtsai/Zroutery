//! Outbound half of the Responses dialect: canonical IR in, a request body out.

use serde_json::{json, Map, Value};

use super::request_in::encode_text_format;
use super::sanitize;
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, ContentBlock, MediaSource, Role, ToolChoice, ToolResultPart};
use crate::protocol::apply_content_policy;
use crate::protocol::reasoning_bridge;

/// Render a tool-use `input` value back into the `arguments` JSON string.
///
/// When the original arguments could not be parsed (invalid JSON), the IR stores
/// them as `Value::String(raw)` rather than wrapping them in a synthetic object.
/// This helper returns the raw string unchanged in that case, and serialises
/// Object values as usual.
pub(super) fn arguments_json(input: &Value) -> String {
    match input {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
/// Encode a Responses request with the default history handling.
///
/// The history travels as the client sent it. Use [`encode_request_with`] with
/// `portable_history` for a relay that may serve one conversation through
/// several accounts or resources.
pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    encode_request_with(req, upstream_model, false)
}

/// Encode a Responses request, optionally making its history portable.
///
/// `portable_history` is the provider's [`ProviderQuirks::portable_history`]:
/// when set, history ids and reasoning items issued by an earlier upstream are
/// removed before the body leaves the proxy. See `sanitize`.
pub fn encode_request_with(
    req: &ChatRequest,
    upstream_model: &str,
    portable_history: bool,
) -> Result<Value> {
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        let mut parts: Vec<Value> = Vec::new();
        let mut separate_items: Vec<Value> = Vec::new();
        // Assistant history echoes the output_text parts a Responses response
        // carries; user turns use input_text.
        let text_part_type = if m.role == Role::Assistant {
            "output_text"
        } else {
            "input_text"
        };

        for b in &m.content {
            match b {
                ContentBlock::Text { text, .. } => parts.push(json!({
                    "type": text_part_type,
                    "text": text,
                })),
                ContentBlock::Image { source } => match source {
                    MediaSource::Base64 { media_type, data } => {
                        parts.push(json!({
                            "type": "input_image",
                            "image_url": format!("data:{media_type};base64,{data}"),
                        }));
                    }
                    MediaSource::Url { url } => {
                        parts.push(json!({"type": "input_image", "image_url": url}));
                    }
                    MediaSource::Reference { id } => {
                        parts.push(json!({"type": "input_image", "file_id": id}));
                    }
                },
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: tool_input,
                } => separate_items.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments_json(tool_input),
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let mut text_parts = Vec::new();
                    for part in content {
                        match part {
                            ToolResultPart::Text { text } => text_parts.push(text.clone()),
                            ToolResultPart::Image { source } => {
                                let image = ContentBlock::Image {
                                    source: source.clone(),
                                };
                                if let Some(replacement) =
                                    apply_content_policy(req.unsupported_content_policy, &image)?
                                {
                                    if let Some(text) = replacement.as_text() {
                                        text_parts.push(text.to_string());
                                    }
                                }
                            }
                        }
                    }
                    separate_items.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": text_parts.join("\n"),
                    }));
                }
                ContentBlock::Thinking { text, signature } => {
                    if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                        separate_items.push(item);
                    } else if signature.is_none() {
                        separate_items.push(json!({
                            "type": "reasoning",
                            "summary": [{"type": "summary_text", "text": text}],
                        }));
                    } else if let Some(replacement) =
                        apply_content_policy(req.unsupported_content_policy, b)?
                    {
                        if let Some(text) = replacement.as_text() {
                            parts.push(json!({"type": text_part_type, "text": text}));
                        }
                    } else {
                        return Err(Error::internal("thinking content could not be encoded"));
                    }
                }
                ContentBlock::RedactedThinking { data } => {
                    if data.is_empty() {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(text) = replacement.as_text() {
                                parts.push(json!({"type": text_part_type, "text": text}));
                            }
                        } else {
                            return Err(Error::internal(
                                "redacted thinking content could not be encoded",
                            ));
                        }
                    } else {
                        // Already opaque: a native reasoning payload must go
                        // back byte for byte instead of being re-wrapped in
                        // this bridge's own envelope.
                        separate_items.push(json!({
                            "type": "reasoning",
                            "encrypted_content": data,
                            "summary": [],
                        }));
                    }
                }
                ContentBlock::Audio { source, media_type } => {
                    let format = crate::protocol::normalize_audio_format(media_type);
                    match source {
                        MediaSource::Base64 { data, .. } => {
                            parts.push(json!({
                                "type": "input_audio",
                                "input_audio": {"data": data, "format": format},
                            }));
                        }
                        MediaSource::Url { .. } | MediaSource::Reference { .. } => {
                            // URL/reference audio cannot be encoded inline; apply the policy.
                            if let Some(replacement) =
                                apply_content_policy(req.unsupported_content_policy, b)?
                            {
                                if let Some(t) = replacement.as_text() {
                                    parts.push(json!({"type": text_part_type, "text": t}));
                                }
                            }
                        }
                    }
                }
                ContentBlock::File {
                    source,
                    media_type,
                    name,
                } => match source {
                    MediaSource::Base64 { data, .. } => {
                        let mut item = json!({
                            "type": "input_file",
                            "file_data": data,
                            "media_type": media_type,
                        });
                        if let Some(n) = name {
                            item["filename"] = json!(n);
                        }
                        parts.push(item);
                    }
                    MediaSource::Url { url } => {
                        let mut item = json!({
                            "type": "input_file",
                            "file_url": url,
                            "media_type": media_type,
                        });
                        if let Some(n) = name {
                            item["filename"] = json!(n);
                        }
                        parts.push(item);
                    }
                    MediaSource::Reference { id } => {
                        let mut item = json!({
                            "type": "input_file",
                            "file_id": id,
                            "media_type": media_type,
                        });
                        if let Some(n) = name {
                            item["filename"] = json!(n);
                        }
                        parts.push(item);
                    }
                },
                ContentBlock::Document { .. }
                | ContentBlock::Video { .. }
                | ContentBlock::Citation { .. }
                | ContentBlock::Annotation { .. } => {
                    if let Some(replacement) =
                        apply_content_policy(req.unsupported_content_policy, b)?
                    {
                        if let Some(t) = replacement.as_text() {
                            parts.push(json!({"type": text_part_type, "text": t}));
                        }
                    }
                }
            }
        }

        if !parts.is_empty() {
            input.push(json!({
                "type": "message",
                "role": if m.role == Role::Assistant { "assistant" } else { "user" },
                "content": parts,
            }));
        }
        input.extend(separate_items);
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));
    if !req.system.is_empty() {
        body.insert(
            "instructions".into(),
            json!(req
                .system
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")),
        );
    }
    body.insert("input".into(), Value::Array(input));
    if let Some(mt) = req.max_tokens {
        body.insert("max_output_tokens".into(), json!(mt));
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
    }
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "name": t.name,
                            "description": t.description.clone().unwrap_or_default(),
                            "parameters": t.input_schema,
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(tc) = &req.tool_choice {
        body.insert(
            "tool_choice".into(),
            match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Any => json!("required"),
                ToolChoice::Specific { name } => json!({"type": "function", "name": name}),
            },
        );
    }
    if let Some(th) = &req.thinking {
        let effort = reasoning_bridge::encode_effort(th);
        body.insert("reasoning".into(), json!({"effort": effort}));
    }
    if let Some(temp) = req.temperature {
        body.insert("temperature".into(), json!(temp));
    }
    if let Some(top_p) = req.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if let Some(user) = &req.metadata_user {
        body.insert("metadata".into(), json!({"user": user}));
    }
    if let Some(structured) = &req.structured_output {
        body.insert(
            "text".into(),
            json!({"format": encode_text_format(structured)}),
        );
    }
    // Pass through fields we stored but don't interpret
    for field in [
        "store",
        "previous_response_id",
        "truncation",
        "include",
        // See the decoder: `prompt_cache_key` belongs to the client.
        "prompt_cache_key",
    ] {
        if let Some(val) = req.passthrough.get(field) {
            body.insert(field.to_string(), val.clone());
        }
    }
    // Only when the operator says this upstream may not be the one that issued
    // the history: ids, reasoning blobs and `previous_response_id` name
    // resources on another account. See `sanitize` for the full argument.
    let mut body = Value::Object(body);
    if portable_history {
        let report = sanitize::make_portable(&mut body);
        if report != sanitize::Portability::default() {
            tracing::debug!(
                reasoning_items = report.reasoning_items_removed,
                include_entries = report.include_entries_removed,
                item_ids = report.item_ids_removed,
                previous_response_id = report.previous_response_id_removed,
                "responses request made portable for an upstream that may not have issued its history"
            );
        }
    }
    Ok(body)
}
