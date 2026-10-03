//! OpenAI Responses API (`/v1/responses`) dialect.
//!
//! This is the newer OpenAI format built around items and parts. The decoder
//! lifts Responses items into the canonical IR, the encoder renders IR content
//! back into a Responses response, and the stream parser/encoder handle the SSE
//! lifecycle.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use super::apply_content_policy;
use super::reasoning_bridge;
use super::{
    explicit_placeholder, unsupported_content, unsupported_upstream_content, SseFrame,
    StreamEncoder, StreamParser,
};
use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, Message, Role, StopReason,
    StreamEvent, StructuredOutput, SystemPart, ThinkingConfig, ToolChoice, ToolDef, ToolResultPart,
    Usage, DEFAULT_STRUCTURED_OUTPUT_SCHEMA_NAME,
};

// ---------------------------------------------------------------- request in

/// What the client asked the proxy to retain for a completed response.
///
/// The Responses API `store` field is a retention instruction, not a
/// passthrough: `false` means the proxy must not keep the request's input or
/// the model's output anywhere a later `GET /v1/responses/{id}` could reach.
/// Absent or `true` keeps the API default, which is to retain the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoragePolicy {
    /// The default: a completed response is kept for retrieval and
    /// continuation.
    #[default]
    Store,
    /// `store: false`: nothing about this request or its answer is retained.
    Discard,
}

impl StoragePolicy {
    /// Read the policy from the decoded request.
    ///
    /// Every dialect may call this; only the Responses dialect carries `store`.
    /// Anything other than an explicit `false` is the API default, so a
    /// malformed value never silently turns retention off.
    pub fn from_request(req: &ChatRequest) -> Self {
        match req.passthrough.get("store") {
            Some(Value::Bool(false)) => StoragePolicy::Discard,
            _ => StoragePolicy::Store,
        }
    }

    /// Whether a completed response may be written to the response store.
    pub fn retains(self) -> bool {
        matches!(self, StoragePolicy::Store)
    }
}

pub fn decode_request(body: Value) -> Result<ChatRequest> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::invalid("Responses request body must be a JSON object"))?;

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("`model` is required"))?;
    let mut req = ChatRequest::new(model, Dialect::OpenAIResponses);

    match obj.get("instructions") {
        Some(Value::String(s)) => req.system.push(SystemPart::new(s.clone())),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(text) = part.as_str() {
                    req.system.push(SystemPart::new(text));
                    continue;
                }
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("input_text");
                if kind != "input_text" && kind != "text" {
                    return Err(unsupported_content("instructions", Some(kind)));
                }
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("instruction part is missing `text`"))?;
                req.system.push(SystemPart::new(text));
            }
        }
        None | Some(Value::Null) => {}
        Some(_) => {
            return Err(Error::invalid(
                "`instructions` must be a string or an array",
            ))
        }
    }

    match obj.get("input") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => req.messages.push(Message::user_text(text)),
        Some(Value::Array(items)) => {
            for item in items {
                decode_input_item(item, &mut req)?;
            }
        }
        Some(_) => return Err(Error::invalid("`input` must be a string or an array")),
    }

    req.max_tokens = obj
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);

    if let Some(temp) = obj.get("temperature").and_then(Value::as_f64) {
        req.temperature = Some(temp);
    }
    if let Some(top_p) = obj.get("top_p").and_then(Value::as_f64) {
        req.top_p = Some(top_p);
    }
    if let Some(user) = obj
        .get("metadata")
        .and_then(|m| m.get("user"))
        .and_then(Value::as_str)
    {
        req.metadata_user = Some(user.to_string());
    }
    // Structured output travels in `text.format`; it used to be dropped here,
    // so a caller that asked for a schema silently got unconstrained text.
    if let Some(text) = obj.get("text").filter(|value| !value.is_null()) {
        req.structured_output = decode_text_format(text)?;
    }

    // Store fields we pass through but don't interpret yet
    for field in ["store", "previous_response_id", "truncation", "include"] {
        if let Some(val) = obj.get(field) {
            req.passthrough.insert(field.to_string(), val.clone());
        }
    }

    match obj.get("tools") {
        None | Some(Value::Null) => {}
        Some(Value::Array(tools)) => {
            for t in tools {
                let name = t
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("tool definition is missing `name`"))?;
                req.tools.push(ToolDef {
                    name: name.to_string(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_schema: t
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                    cache_control: None,
                });
            }
        }
        Some(_) => return Err(Error::invalid("`tools` must be an array")),
    }

    req.tool_choice = match obj.get("tool_choice") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" | "any" => Some(ToolChoice::Any),
            other => return Err(unsupported_content("tool_choice", Some(other))),
        },
        Some(Value::Object(o)) => {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_choice is missing `name`"))?;
            Some(ToolChoice::Specific {
                name: name.to_string(),
            })
        }
        Some(_) => {
            return Err(Error::invalid(
                "`tool_choice` must be a string or an object",
            ))
        }
    };

    if let Some(reasoning) = obj.get("reasoning") {
        let effort = reasoning
            .get("effort")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("reasoning is missing `effort`"))?;
        req.thinking = Some(match effort {
            "none" | "minimal" => ThinkingConfig {
                enabled: false,
                budget_tokens: None,
            },
            "low" => ThinkingConfig {
                enabled: true,
                budget_tokens: Some(1024),
            },
            "medium" => ThinkingConfig {
                enabled: true,
                budget_tokens: Some(4096),
            },
            "high" => ThinkingConfig {
                enabled: true,
                budget_tokens: Some(16384),
            },
            other => return Err(unsupported_content("reasoning effort", Some(other))),
        });
    }

    req.refresh_required_capabilities();
    Ok(req)
}

/// Decode the Responses `text.format` structured-output constraint.
///
/// Unlike Chat Completions, the schema fields sit directly on the format
/// object (`{"type": "json_schema", "name": ..., "schema": ..., "strict": ...}`)
/// rather than inside a nested `json_schema` member. A `text` object with no
/// `format`, or `{"type": "text"}`, states no constraint.
fn decode_text_format(text: &Value) -> Result<Option<StructuredOutput>> {
    let format = match text.get("format") {
        None | Some(Value::Null) => return Ok(None),
        Some(format) => format,
    };
    let kind = format
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("`text.format` is missing `type`"))?;
    match kind {
        "text" => Ok(None),
        "json_object" => Ok(Some(StructuredOutput::JsonObject)),
        "json_schema" => {
            let schema = format
                .get("schema")
                .cloned()
                .ok_or_else(|| Error::invalid("`text.format.schema` is required"))?;
            Ok(Some(StructuredOutput::JsonSchema {
                name: format
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                schema,
                strict: format
                    .get("strict")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }))
        }
        other => Err(unsupported_content("text.format", Some(other))),
    }
}

/// Render an IR structured-output constraint as Responses `text.format`.
fn encode_text_format(structured: &StructuredOutput) -> Value {
    match structured {
        StructuredOutput::JsonObject => json!({"type": "json_object"}),
        StructuredOutput::JsonSchema {
            name,
            schema,
            strict,
        } => json!({
            "type": "json_schema",
            "name": name
                .as_deref()
                .unwrap_or(DEFAULT_STRUCTURED_OUTPUT_SCHEMA_NAME),
            "schema": schema,
            "strict": strict,
        }),
    }
}

/// Decode a Responses `input_image` style object into a media source.
///
/// The Responses schema stores the URL or data URL directly in the
/// `image_url` string field. The nested `{"image_url": {"url": ...}}` shape
/// belongs to Chat Completions and is only kept as a compatibility branch.
fn decode_image_source(value: &Value, context: &str) -> Result<MediaSource> {
    if let Some(url) = value.get("image_url").and_then(Value::as_str) {
        return Ok(MediaSource::from_url(url));
    }
    if let Some(url) = value.pointer("/image_url/url").and_then(Value::as_str) {
        return Ok(MediaSource::from_url(url));
    }
    if let Some(id) = value.get("file_id").and_then(Value::as_str) {
        return Ok(MediaSource::Reference { id: id.to_string() });
    }
    Err(Error::invalid(format!(
        "{context} is missing a string `image_url` or `file_id`"
    )))
}

fn decode_tool_result_output(value: &Value) -> Result<Vec<ToolResultPart>> {
    match value {
        Value::String(text) => Ok(vec![ToolResultPart::Text { text: text.clone() }]),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("tool result part is missing `type`"))?;
                match kind {
                    "output_text" | "input_text" | "text" => Ok(ToolResultPart::Text {
                        text: part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::invalid("tool result text part is missing `text`")
                            })?
                            .to_string(),
                    }),
                    "input_image" | "image" | "image_url" => {
                        let source = decode_image_source(part, "tool result image")?;
                        Ok(ToolResultPart::Image { source })
                    }
                    other => Err(unsupported_content("tool result", Some(other))),
                }
            })
            .collect(),
        _ => Err(Error::invalid(
            "tool result output must be a string or an array",
        )),
    }
}

/// Decode a Responses reasoning item into IR content blocks.
///
/// Our own bridge envelope unpacks to a single thinking block. A native
/// reasoning item is kept as faithfully as the IR allows: every summary entry
/// survives instead of only the first, and the opaque `encrypted_content` is
/// carried through unchanged so a later turn can echo it back.
fn decode_reasoning_blocks(item: &Value) -> Vec<ContentBlock> {
    if let Some(block) = reasoning_bridge::decode_reasoning_item(item) {
        return vec![block];
    }
    let mut blocks = Vec::new();
    let summary = item
        .get("summary")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    if !summary.is_empty() {
        blocks.push(ContentBlock::Thinking {
            text: summary,
            signature: None,
        });
    }
    if let Some(encrypted) = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        blocks.push(ContentBlock::RedactedThinking {
            data: encrypted.to_string(),
        });
    }
    blocks
}

fn decode_input_item(item: &Value, req: &mut ChatRequest) -> Result<()> {
    // `type` is optional on the official EasyInputMessage: a bare object with
    // `role` and `content` is a normal chat message.
    let item_type = match item.get("type").and_then(Value::as_str) {
        Some(kind) => kind,
        None if item.get("role").is_some() || item.get("content").is_some() => "message",
        None => return Err(Error::invalid("input item is missing `type`")),
    };
    match item_type {
        "function_call" => {
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("function_call is missing `arguments`"))?;
            let input =
                serde_json::from_str(arguments).unwrap_or(Value::String(arguments.to_string()));
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("function_call is missing `call_id`"))?;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("function_call is missing `name`"))?;
            req.messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input,
                }],
            });
        }
        "function_call_output" => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("function_call_output is missing `call_id`"))?;
            let output = item
                .get("output")
                .ok_or_else(|| Error::invalid("function_call_output is missing `output`"))?;
            req.messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: call_id.to_string(),
                    name: String::new(),
                    content: decode_tool_result_output(output)?,
                    is_error: false,
                }],
            });
        }
        "input_text" => {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("input_text is missing `text`"))?;
            req.messages.push(Message::user_text(text));
        }
        "input_image" => {
            let source = decode_image_source(item, "input_image")?;
            req.messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Image { source }],
            });
        }
        "input_audio" => {
            let audio = item
                .get("input_audio")
                .ok_or_else(|| Error::invalid("input_audio is missing `input_audio`"))?;
            let data = audio
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("input_audio is missing `data`"))?
                .to_string();
            let format = audio.get("format").and_then(Value::as_str).unwrap_or("wav");
            let media_type = format!("audio/{}", normalize_audio_format_to_mime(format));
            req.messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Audio {
                    source: MediaSource::Base64 {
                        media_type: media_type.clone(),
                        data,
                    },
                    media_type,
                }],
            });
        }
        "input_file" => {
            let name = item
                .get("filename")
                .and_then(Value::as_str)
                .map(String::from);
            if let Some(data) = item.get("file_data").and_then(Value::as_str) {
                // Base64-encoded file data
                let media_type = item
                    .get("media_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string();
                req.messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::File {
                        source: MediaSource::Base64 {
                            media_type: media_type.clone(),
                            data: data.to_string(),
                        },
                        media_type,
                        name,
                    }],
                });
            } else if let Some(url) = item.get("file_url").and_then(Value::as_str) {
                let media_type = item
                    .get("media_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string();
                req.messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::File {
                        source: MediaSource::Url {
                            url: url.to_string(),
                        },
                        media_type,
                        name,
                    }],
                });
            } else if let Some(file_id) = item.get("file_id").and_then(Value::as_str) {
                let media_type = item
                    .get("media_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string();
                req.messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::File {
                        source: MediaSource::Reference {
                            id: file_id.to_string(),
                        },
                        media_type,
                        name,
                    }],
                });
            } else {
                return Err(Error::invalid(
                    "input_file is missing `file_data`, `file_url`, or `file_id`",
                ));
            }
        }
        "reasoning" => {
            let blocks = decode_reasoning_blocks(item);
            if blocks.is_empty() {
                return Err(unsupported_content(
                    "reasoning",
                    Some("unrecognized reasoning item"),
                ));
            }
            req.messages.push(Message {
                role: Role::Assistant,
                content: blocks,
            });
        }
        "message" => {
            let role = item.get("role").and_then(Value::as_str);
            let mut content = Vec::new();
            match item.get("content") {
                Some(Value::Array(parts)) => {
                    for part in parts {
                        let kind = part
                            .get("type")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("message part is missing `type`"))?;
                        match kind {
                            "output_text" | "input_text" => {
                                let text = part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| Error::invalid("text part is missing `text`"))?;
                                content.push(ContentBlock::text(text));
                            }
                            "input_image" => {
                                let source = decode_image_source(part, "input_image part")?;
                                content.push(ContentBlock::Image { source });
                            }
                            "input_audio" => {
                                let audio = part.get("input_audio").ok_or_else(|| {
                                    Error::invalid("input_audio part is missing `input_audio`")
                                })?;
                                let data = audio
                                    .get("data")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| Error::invalid("input_audio is missing `data`"))?
                                    .to_string();
                                let format =
                                    audio.get("format").and_then(Value::as_str).unwrap_or("wav");
                                let media_type =
                                    format!("audio/{}", normalize_audio_format_to_mime(format));
                                content.push(ContentBlock::Audio {
                                    source: MediaSource::Base64 {
                                        media_type: media_type.clone(),
                                        data,
                                    },
                                    media_type,
                                });
                            }
                            "input_file" => {
                                let name = part
                                    .get("filename")
                                    .and_then(Value::as_str)
                                    .map(String::from);
                                let media_type = part
                                    .get("media_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or("application/octet-stream")
                                    .to_string();
                                let source = if let Some(data) =
                                    part.get("file_data").and_then(Value::as_str)
                                {
                                    MediaSource::Base64 {
                                        media_type: media_type.clone(),
                                        data: data.to_string(),
                                    }
                                } else if let Some(url) =
                                    part.get("file_url").and_then(Value::as_str)
                                {
                                    MediaSource::Url {
                                        url: url.to_string(),
                                    }
                                } else if let Some(file_id) =
                                    part.get("file_id").and_then(Value::as_str)
                                {
                                    MediaSource::Reference {
                                        id: file_id.to_string(),
                                    }
                                } else {
                                    return Err(Error::invalid(
                                        "input_file part is missing `file_data`, `file_url`, or `file_id`",
                                    ));
                                };
                                content.push(ContentBlock::File {
                                    source,
                                    media_type,
                                    name,
                                });
                            }
                            other => return Err(unsupported_content("message", Some(other))),
                        }
                    }
                }
                Some(Value::String(text)) => content.push(ContentBlock::text(text)),
                None | Some(Value::Null) => {}
                Some(_) => {
                    return Err(Error::invalid(
                        "message content must be a string, an array, or null",
                    ));
                }
            }
            match role {
                Some("assistant") => req.messages.push(Message {
                    role: Role::Assistant,
                    content,
                }),
                Some("user") => req.messages.push(Message {
                    role: Role::User,
                    content,
                }),
                // system/developer messages are instructions, not turns.
                Some("system") | Some("developer") => {
                    let mut parts = Vec::new();
                    for block in &content {
                        match block {
                            ContentBlock::Text { text, .. } => parts.push(text.clone()),
                            _ => {
                                return Err(Error::invalid(
                                    "system and developer messages only support text content",
                                ))
                            }
                        }
                    }
                    if !parts.is_empty() {
                        req.system.push(SystemPart::new(parts.join("\n\n")));
                    }
                }
                Some(other) => return Err(unsupported_content("message role", Some(other))),
                None => return Err(Error::invalid("message item is missing `role`")),
            }
        }
        other => return Err(unsupported_content("input item", Some(other))),
    }
    Ok(())
}

// --------------------------------------------------------------- request out

/// Render a tool-use `input` value back into the `arguments` JSON string.
///
/// When the original arguments could not be parsed (invalid JSON), the IR stores
/// them as `Value::String(raw)` rather than wrapping them in a synthetic object.
/// This helper returns the raw string unchanged in that case, and serialises
/// Object values as usual.
fn arguments_json(input: &Value) -> String {
    match input {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Convert an OpenAI audio format string to a MIME subtype.
fn normalize_audio_format_to_mime(format: &str) -> &str {
    match format {
        "mp3" => "mpeg",
        "wav" => "wav",
        other => other,
    }
}

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
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
                    let format = super::normalize_audio_format(media_type);
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
        let effort = match (th.enabled, th.budget_tokens.unwrap_or(4096)) {
            (false, _) => "none",
            (true, b) if b <= 1024 => "low",
            (true, b) if b >= 16384 => "high",
            _ => "medium",
        };
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
    for field in ["store", "previous_response_id", "truncation", "include"] {
        if let Some(val) = req.passthrough.get(field) {
            body.insert(field.to_string(), val.clone());
        }
    }
    Ok(Value::Object(body))
}

// ---------------------------------------------------------------- responses

pub fn decode_response(body: Value) -> Result<ChatResponse> {
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
pub(crate) fn decode_usage(v: Option<&Value>) -> Usage {
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
pub(crate) fn encode_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
        "total_tokens": u.total(),
        "input_tokens_details": {"cached_tokens": u.cache_read_tokens.min(u.input_tokens)},
        "output_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
    })
}

// ---------------------------------------------------------------- stream in

pub struct ResponsesStreamParser {
    id: String,
    model: String,
    started: bool,
    /// Whether the upstream really ended the response
    /// (`response.completed`/`response.incomplete`). A body that just stops
    /// arriving is truncated.
    normal_terminal: bool,
    next_index: u32,
    text_index: Option<u32>,
    thinking_index: Option<u32>,
    tool_indices: HashMap<String, u32>,
    /// Tool indices whose block stop has already been emitted.
    stopped_tools: HashSet<u32>,
    usage: Usage,
}

impl ResponsesStreamParser {
    pub fn new(model: &str) -> Self {
        Self {
            id: String::new(),
            model: model.to_string(),
            started: false,
            normal_terminal: false,
            next_index: 0,
            text_index: None,
            thinking_index: None,
            tool_indices: HashMap::new(),
            stopped_tools: HashSet::new(),
            usage: Usage::default(),
        }
    }

    /// Emit the block stop for a finished tool call exactly once.
    fn close_tool(&mut self, index: u32, out: &mut Vec<StreamEvent>) {
        if self.stopped_tools.insert(index) {
            out.push(StreamEvent::BlockStop { index });
        }
    }

    /// Resolve a tool index from an event that references it by item or call id.
    fn tool_index(&self, event: &Value) -> Option<u32> {
        let by_item = event
            .get("item_id")
            .or_else(|| event.get("id"))
            .and_then(Value::as_str)
            .and_then(|id| self.tool_indices.get(id).copied());
        by_item.or_else(|| {
            event
                .get("call_id")
                .and_then(Value::as_str)
                .and_then(|id| self.tool_indices.get(id).copied())
        })
    }

    fn open_index(&mut self, kind: &str) -> u32 {
        // Reuse an open block while deltas for the same part keep arriving.
        match kind {
            "text" => {
                if let Some(i) = self.text_index {
                    return i;
                }
            }
            "thinking" => {
                if let Some(i) = self.thinking_index {
                    return i;
                }
            }
            _ => {}
        }
        let index = self.next_index;
        self.next_index += 1;
        match kind {
            "text" => self.text_index = Some(index),
            "thinking" => self.thinking_index = Some(index),
            _ => {}
        }
        index
    }
}

impl StreamParser for ResponsesStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let event = frame.json()?;
        let event_type = event.get("type").and_then(Value::as_str).ok_or_else(|| {
            Error::BadUpstreamPayload("Responses stream event is missing `type`".into())
        })?;
        let mut out = Vec::new();

        match event_type {
            "response.created" => {
                let response = event.get("response").cloned().unwrap_or_default();
                self.id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("resp_stream")
                    .to_string();
                if let Some(model) = response.get("model").and_then(Value::as_str) {
                    self.model = model.to_string();
                }
                self.started = true;
                out.push(StreamEvent::Start {
                    id: self.id.clone(),
                    model: self.model.clone(),
                    usage: self.usage,
                });
            }
            "response.output_item.added" => {
                let item = event.get("item").ok_or_else(|| {
                    Error::BadUpstreamPayload("output item event is missing `item`".into())
                })?;
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let index = self.next_index;
                        self.next_index += 1;
                        let item_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::BadUpstreamPayload(
                                    "function call item is missing `id`".into(),
                                )
                            })?
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::BadUpstreamPayload(
                                    "function call item is missing `call_id`".into(),
                                )
                            })?
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::BadUpstreamPayload(
                                    "function call item is missing `name`".into(),
                                )
                            })?
                            .to_string();
                        self.tool_indices.insert(item_id, index);
                        self.tool_indices.insert(call_id.clone(), index);
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: call_id,
                            name,
                        });
                    }
                    Some("message") | Some("reasoning") => {}
                    Some(_) => {
                        return Err(unsupported_upstream_content("Responses stream output item"));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses stream output item is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.content_part.added" => {
                let part = event.get("part").ok_or_else(|| {
                    Error::BadUpstreamPayload("content part event is missing `part`".into())
                })?;
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        self.open_index("text");
                    }
                    Some("reasoning") | Some("summary_text") => {
                        self.open_index("thinking");
                    }
                    Some(_) => {
                        return Err(unsupported_upstream_content(
                            "Responses stream content part",
                        ));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses stream content part is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.content_part.done" => {
                let part = event.get("part").ok_or_else(|| {
                    Error::BadUpstreamPayload("content part event is missing `part`".into())
                })?;
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => self.text_index = None,
                    Some("reasoning") | Some("summary_text") => self.thinking_index = None,
                    Some(_) => {
                        return Err(unsupported_upstream_content(
                            "Responses stream content part",
                        ));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses stream content part is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.output_item.done" => {
                self.text_index = None;
                self.thinking_index = None;
                // A finished function_call item closes its tool block.
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        if let Some(index) = self.tool_index(item) {
                            self.close_tool(index, &mut out);
                        }
                    }
                }
            }
            "response.output_text.delta" => {
                let index = self.open_index("text");
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("output text delta is missing `delta`".into())
                })?;
                out.push(StreamEvent::TextDelta {
                    index,
                    text: delta.to_string(),
                });
            }
            "response.function_call_arguments.delta" => {
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let index = self.tool_indices.get(item_id).copied().or_else(|| {
                    event
                        .get("call_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.tool_indices.get(id).copied())
                });
                let index = index.ok_or_else(|| {
                    Error::BadUpstreamPayload(
                        "function argument delta does not reference a known tool call".into(),
                    )
                })?;
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("function argument delta is missing `delta`".into())
                })?;
                out.push(StreamEvent::ToolUseDelta {
                    index,
                    partial_json: delta.to_string(),
                });
            }
            "response.reasoning_summary_text.delta" => {
                let index = self.open_index("thinking");
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("reasoning summary delta is missing `delta`".into())
                })?;
                out.push(StreamEvent::ThinkingDelta {
                    index,
                    text: delta.to_string(),
                });
            }
            "response.completed" | "response.incomplete" => {
                if !self.started {
                    self.started = true;
                    out.push(StreamEvent::Start {
                        id: self.id.clone(),
                        model: self.model.clone(),
                        usage: self.usage,
                    });
                }
                if let Some(response) = event.get("response") {
                    if let Some(usage) = response.get("usage") {
                        self.usage = decode_usage(Some(usage));
                    }
                }
                let stop_reason = if event_type == "response.incomplete" {
                    match event
                        .get("response")
                        .and_then(|r| r.pointer("/incomplete_details/reason"))
                        .and_then(Value::as_str)
                    {
                        Some("content_filter") => StopReason::Refusal,
                        _ => StopReason::MaxTokens,
                    }
                } else if response_has_tool_call(event.get("response")) {
                    // A response that yielded function calls ended for a tool
                    // round, not a natural turn end.
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                self.normal_terminal = true;
                out.push(StreamEvent::Stop {
                    stop_reason,
                    stop_sequence: None,
                    usage: self.usage,
                });
            }
            "response.reasoning_summary_part.added" | "response.reasoning_summary_part.done" => {
                let part = event.get("part").ok_or_else(|| {
                    Error::BadUpstreamPayload(
                        "reasoning summary part event is missing `part`".into(),
                    )
                })?;
                match part.get("type").and_then(Value::as_str) {
                    Some("summary_text") | Some("reasoning") | Some("reasoning_text") => {
                        if event_type == "response.reasoning_summary_part.added" {
                            self.open_index("thinking");
                        } else {
                            self.thinking_index = None;
                        }
                    }
                    Some(_) => {
                        return Err(unsupported_upstream_content(
                            "Responses reasoning summary part",
                        ));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses reasoning summary part is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(index) = self.tool_index(&event) {
                    self.close_tool(index, &mut out);
                }
            }
            "response.in_progress"
            | "response.queued"
            | "response.output_text.done"
            | "response.reasoning_summary_text.done" => {}
            _ => {
                return Err(unsupported_upstream_content("Responses stream event"));
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        // A stream cut off after tool deltas still owes each open tool its
        // block stop, so the client's block stream is well formed — but no
        // `Stop` is synthesized: this body never named a terminal.
        let mut indices: Vec<u32> = self.tool_indices.values().copied().collect();
        indices.sort_unstable();
        indices.dedup();
        for index in indices {
            self.close_tool(index, &mut out);
        }
        out
    }

    fn saw_normal_terminal(&self) -> bool {
        self.normal_terminal
    }

    fn reported_usage(&self) -> Usage {
        self.usage
    }
}

/// Whether a terminal Responses payload contains at least one function call.
fn response_has_tool_call(response: Option<&Value>) -> bool {
    response
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        })
        .unwrap_or(false)
}

// --------------------------------------------------------------- stream out

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputItemKind {
    Text,
    Thinking,
}

/// Per-tool-call state, fully independent of the global Text/Thinking state.
struct ToolOutputState {
    output_index: u32,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

pub struct ResponsesStreamEncoder {
    id: String,
    model: String,
    created_at: i64,
    done: bool,
    next_output_index: u32,
    /// Monotonic SSE `sequence_number`, stamped on every emitted frame.
    next_sequence: Cell<u64>,
    /// True once the upstream reported a token-limit stop.
    truncated: bool,
    // --- Text/Thinking state (sequential, one at a time) ---
    current_output_index: u32,
    content_index: u32,
    current_kind: Option<OutputItemKind>,
    content_part_open: bool,
    output_item_open: bool,
    current_item_id: String,
    current_text: String,
    current_thinking: String,
    /// Upstream block index of the thinking item currently open.
    current_thinking_source: Option<u32>,
    /// Anthropic thinking signatures awaiting the end of their block.
    thinking_signatures: HashMap<u32, String>,
    // --- Tool state (parallel, per-index) ---
    tool_states: HashMap<u32, ToolOutputState>,
    // --- Output accumulator (indexed by output_index) ---
    output_items: HashMap<u32, Value>,
}

impl ResponsesStreamEncoder {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("resp-{}", uuid::Uuid::new_v4().simple()),
            model: model.to_string(),
            created_at: chrono::Utc::now().timestamp(),
            done: false,
            next_output_index: 0,
            next_sequence: Cell::new(0),
            truncated: false,
            current_output_index: 0,
            content_index: 0,
            current_kind: None,
            content_part_open: false,
            output_item_open: false,
            current_item_id: String::new(),
            current_text: String::new(),
            current_thinking: String::new(),
            current_thinking_source: None,
            thinking_signatures: HashMap::new(),
            tool_states: HashMap::new(),
            output_items: HashMap::new(),
        }
    }

    fn frame(&self, event_type: &str, data: Value) -> SseFrame {
        // Every official Responses event carries a monotonic sequence_number.
        let mut data = data;
        if let Value::Object(map) = &mut data {
            map.insert("sequence_number".into(), json!(self.next_sequence.get()));
        }
        self.next_sequence.set(self.next_sequence.get() + 1);
        SseFrame {
            event: Some(event_type.to_string()),
            data: data.to_string(),
        }
    }

    /// Allocate the next output_index.
    fn alloc_output_index(&mut self) -> u32 {
        let i = self.next_output_index;
        self.next_output_index += 1;
        i
    }

    fn close_content_part(&mut self) -> Option<SseFrame> {
        if !self.content_part_open {
            return None;
        }
        self.content_part_open = false;
        let part = match self.current_kind {
            Some(OutputItemKind::Text) => json!({
                "type": "output_text",
                "text": self.current_text,
                "annotations": [],
                "logprobs": [],
            }),
            Some(OutputItemKind::Thinking) => json!({
                "type": "reasoning",
                "text": self.current_thinking,
            }),
            _ => return None,
        };
        Some(self.frame(
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "item_id": self.current_item_id,
                "output_index": self.current_output_index,
                "content_index": self.content_index,
                "part": part,
            }),
        ))
    }

    /// Collect output_items into a sorted Vec by output_index.
    fn sorted_output(&mut self) -> Value {
        let mut items: Vec<_> = std::mem::take(&mut self.output_items).into_iter().collect();
        items.sort_by_key(|(k, _)| *k);
        Value::Array(items.into_iter().map(|(_, v)| v).collect())
    }

    /// Finalize the current Text/Thinking output item into output_items.
    fn finalize_output_item(&mut self) {
        let status = if self.truncated {
            "incomplete"
        } else {
            "completed"
        };
        match self.current_kind {
            Some(OutputItemKind::Text) => {
                if !self.current_text.is_empty() {
                    self.output_items.insert(self.current_output_index, json!({
                        "id": self.current_item_id,
                        "type": "message",
                        "role": "assistant",
                        "status": status,
                        "content": [{"type": "output_text", "text": self.current_text, "annotations": []}],
                    }));
                    self.current_text.clear();
                }
            }
            Some(OutputItemKind::Thinking) => {
                // A streamed thinking block is only replayable when its
                // signature travels with the reasoning item, so pack it into
                // the same opaque envelope the non-streaming encoder uses.
                let signature = self
                    .current_thinking_source
                    .and_then(|index| self.thinking_signatures.remove(&index));
                let block = ContentBlock::Thinking {
                    text: self.current_thinking.clone(),
                    signature,
                };
                let encrypted = reasoning_bridge::encode_thinking_block(&block)
                    .and_then(|item| item.get("encrypted_content").cloned());
                if !self.current_thinking.is_empty() || encrypted.is_some() {
                    let summary = if self.current_thinking.is_empty() {
                        json!([])
                    } else {
                        json!([{"type": "summary_text", "text": self.current_thinking}])
                    };
                    let mut item = json!({
                        "id": self.current_item_id,
                        "type": "reasoning",
                        "summary": summary,
                    });
                    if let Some(encrypted) = encrypted {
                        item["encrypted_content"] = encrypted;
                    }
                    self.output_items.insert(self.current_output_index, item);
                    self.current_thinking.clear();
                }
            }
            _ => {}
        }
    }

    /// Close the current Text/Thinking output item.
    /// Returns multiple events: terminal text/thinking event, content_part.done,
    /// output_item.done. Only operates on Text/Thinking global state.
    fn close_output_item(&mut self) -> Vec<SseFrame> {
        if !self.output_item_open {
            return Vec::new();
        }
        let mut out = Vec::new();

        // 1. Emit terminal text/thinking event (before content_part.done).
        if self.content_part_open {
            match self.current_kind {
                Some(OutputItemKind::Text) => {
                    out.push(self.frame(
                        "response.output_text.done",
                        json!({
                            "type": "response.output_text.done",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "content_index": self.content_index,
                            "text": self.current_text,
                            "logprobs": [],
                        }),
                    ));
                }
                Some(OutputItemKind::Thinking) => {
                    out.push(self.frame(
                        "response.reasoning_summary_text.done",
                        json!({
                            "type": "response.reasoning_summary_text.done",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "content_index": self.content_index,
                            "text": self.current_thinking,
                        }),
                    ));
                }
                _ => {}
            }
        }

        // 2. Emit content_part.done (with item_id).
        if let Some(f) = self.close_content_part() {
            out.push(f);
        }

        // 3. Finalize into output_items.
        self.finalize_output_item();

        // 4. Emit output_item.done with the complete item.
        self.output_item_open = false;
        let done_item = self
            .output_items
            .get(&self.current_output_index)
            .cloned()
            .unwrap_or(json!({
                "id": self.current_item_id,
                "type": "message",
                "status": "completed",
            }));
        out.push(self.frame(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": self.current_output_index,
                "item": done_item,
            }),
        ));
        self.current_kind = None;
        self.current_thinking_source = None;
        out
    }

    /// Emit response.output_item.added for a new item.
    fn emit_output_item_added(&self, output_index: u32, item: Value) -> SseFrame {
        self.frame(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": item,
            }),
        )
    }

    /// Open a reasoning output item unless one is already open.
    fn ensure_thinking_item(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        if self.current_kind != Some(OutputItemKind::Thinking) {
            out.extend(self.close_output_item());
            self.current_output_index = self.alloc_output_index();
            self.content_index = 0;
            self.current_kind = Some(OutputItemKind::Thinking);
            self.output_item_open = true;
            self.current_thinking.clear();
            self.current_item_id = format!("rs_{}", self.current_output_index);
            out.push(self.emit_output_item_added(
                self.current_output_index,
                json!({
                    "id": self.current_item_id,
                    "type": "reasoning",
                    "summary": [],
                }),
            ));
        }
        out
    }
}

impl StreamEncoder for ResponsesStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        let mut out = Vec::new();
        match event {
            StreamEvent::Start { id, .. } => {
                // The upstream model name in the event is ignored: the frames
                // carry the exposed id the client asked for.
                if !id.is_empty() {
                    self.id = id.clone();
                }
                out.push(self.frame(
                    "response.created",
                    json!({
                        "type": "response.created",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "in_progress",
                            "model": self.model,
                            "output": [],
                        }
                    }),
                ));
            }
            StreamEvent::TextDelta { text, .. } => {
                // Switch to Text kind if needed.
                if self.current_kind != Some(OutputItemKind::Text) {
                    out.extend(self.close_output_item());
                    self.current_output_index = self.alloc_output_index();
                    self.content_index = 0;
                    self.current_kind = Some(OutputItemKind::Text);
                    self.output_item_open = true;
                    self.current_text.clear();
                    self.current_item_id = format!("msg_{}", self.current_output_index);
                    // Emit output_item.added
                    out.push(self.emit_output_item_added(
                        self.current_output_index,
                        json!({
                            "id": self.current_item_id,
                            "type": "message",
                            "role": "assistant",
                            "status": "in_progress",
                            "content": [],
                        }),
                    ));
                }
                // Emit content_part.added if needed. The official
                // ResponseOutputText part starts with an empty text; the SDK's
                // stream state machine appends deltas to it and raises a
                // TypeError when it is missing.
                if !self.content_part_open {
                    self.content_part_open = true;
                    out.push(self.frame(
                        "response.content_part.added",
                        json!({
                            "type": "response.content_part.added",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "content_index": self.content_index,
                            "part": {"type": "output_text", "text": "", "annotations": [], "logprobs": []},
                        }),
                    ));
                }
                out.push(self.frame(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": self.current_item_id,
                        "output_index": self.current_output_index,
                        "content_index": self.content_index,
                        "delta": text,
                        "logprobs": [],
                    }),
                ));
                self.current_text.push_str(text);
            }
            StreamEvent::ThinkingDelta { text, index } => {
                out.extend(self.ensure_thinking_item());
                self.current_thinking_source = Some(*index);
                if !self.content_part_open {
                    self.content_part_open = true;
                    out.push(self.frame(
                        "response.content_part.added",
                        json!({
                            "type": "response.content_part.added",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "content_index": self.content_index,
                            "part": {"type": "reasoning", "text": ""},
                        }),
                    ));
                }
                out.push(self.frame(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "type": "response.reasoning_summary_text.delta",
                        "item_id": self.current_item_id,
                        "output_index": self.current_output_index,
                        "content_index": self.content_index,
                        "delta": text,
                    }),
                ));
                self.current_thinking.push_str(text);
            }
            StreamEvent::ThinkingSignature { index, signature } => {
                // The signature arrives after its block's deltas and is only
                // useful when it travels with the reasoning item.
                if !signature.is_empty() {
                    out.extend(self.ensure_thinking_item());
                    self.current_thinking_source = Some(*index);
                    self.thinking_signatures.insert(*index, signature.clone());
                }
            }
            StreamEvent::RedactedThinking { data, .. } => {
                // Redacted thinking is already opaque: echo it verbatim as its
                // own reasoning item, exactly like the non-streaming encoder.
                if let Some(item) =
                    reasoning_bridge::encode_thinking_block(&ContentBlock::RedactedThinking {
                        data: data.clone(),
                    })
                {
                    let output_index = self.alloc_output_index();
                    let item_id = format!("rs_{output_index}");
                    let mut done = item;
                    done["id"] = json!(item_id);
                    self.output_items.insert(output_index, done.clone());
                    out.push(self.emit_output_item_added(
                        output_index,
                        json!({"id": item_id, "type": "reasoning", "summary": []}),
                    ));
                    out.push(self.frame(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": done,
                        }),
                    ));
                }
            }
            StreamEvent::ToolUseStart {
                index, id, name, ..
            } => {
                // Does NOT close Text/Thinking — tools are independent.
                let oi = self.alloc_output_index();
                let item_id = format!("fc_{id}");
                self.tool_states.insert(
                    *index,
                    ToolOutputState {
                        output_index: oi,
                        item_id: item_id.clone(),
                        call_id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    },
                );
                out.push(self.emit_output_item_added(
                    oi,
                    json!({
                        "id": item_id,
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": "",
                        "status": "in_progress",
                    }),
                ));
            }
            StreamEvent::ToolUseDelta {
                partial_json,
                index,
                ..
            } => {
                if let Some(ts) = self.tool_states.get_mut(index) {
                    ts.arguments.push_str(partial_json);
                    let item_id = ts.item_id.clone();
                    let output_index = ts.output_index;
                    let delta = partial_json.clone();
                    let frame = self.frame(
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "delta": delta,
                        }),
                    );
                    out.push(frame);
                }
            }
            StreamEvent::BlockStop { index } => {
                if let Some(ts) = self.tool_states.remove(index) {
                    let oi = ts.output_index;
                    let args_display = if ts.arguments.is_empty() {
                        "{}".to_string()
                    } else {
                        ts.arguments.clone()
                    };
                    out.push(self.frame(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": ts.item_id,
                            "output_index": oi,
                            "content_index": 0,
                            "arguments": ts.arguments,
                        }),
                    ));
                    let item = json!({
                        "type": "function_call",
                        "id": ts.item_id,
                        "call_id": ts.call_id,
                        "name": ts.name,
                        "arguments": args_display,
                        "status": "completed",
                    });
                    self.output_items.insert(oi, item.clone());
                    out.push(self.frame(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": oi,
                            "item": item,
                        }),
                    ));
                    // Does NOT touch global current_* state.
                }
            }
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                self.truncated = *stop_reason == StopReason::MaxTokens;
                out.extend(self.close_output_item());
                // Flush incomplete tool calls with full terminal events.
                let incomplete_tools: Vec<_> = self.tool_states.drain().map(|(_, v)| v).collect();
                for ts in incomplete_tools {
                    out.push(self.frame(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": ts.item_id,
                            "output_index": ts.output_index,
                            "content_index": 0,
                            "arguments": if ts.arguments.is_empty() { "{}" } else { &ts.arguments },
                        }),
                    ));
                    let item = json!({
                        "type": "function_call",
                        "id": ts.item_id,
                        "call_id": ts.call_id,
                        "name": ts.name,
                        "arguments": if ts.arguments.is_empty() { "{}".to_string() } else { ts.arguments },
                        "status": "incomplete",
                    });
                    self.output_items.insert(ts.output_index, item.clone());
                    out.push(self.frame(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": ts.output_index,
                            "item": item,
                        }),
                    ));
                }
                let output = self.sorted_output();
                // A stream cut off by the output token limit is not a success:
                // report the terminal status the non-streaming encoder reports.
                let terminal = if self.truncated {
                    json!({
                        "type": "response.incomplete",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "incomplete",
                            "model": self.model,
                            "output": output,
                            "incomplete_details": {"reason": "max_output_tokens"},
                            "usage": encode_usage(usage),
                        }
                    })
                } else {
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "completed",
                            "model": self.model,
                            "output": output,
                            "usage": encode_usage(usage),
                        }
                    })
                };
                let event_type = terminal["type"]
                    .as_str()
                    .expect("terminal frame carries a type")
                    .to_string();
                out.push(self.frame(&event_type, terminal));
                self.done = true;
            }
            _ => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.done {
            return Vec::new();
        }
        self.done = true;
        let mut out = Vec::new();
        out.extend(self.close_output_item());
        let has_incomplete = !self.tool_states.is_empty();
        let incomplete_tools: Vec<_> = self.tool_states.drain().map(|(_, v)| v).collect();
        for ts in incomplete_tools {
            out.push(self.frame(
                "response.function_call_arguments.done",
                json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": ts.item_id,
                    "output_index": ts.output_index,
                    "content_index": 0,
                    "arguments": if ts.arguments.is_empty() { "{}" } else { &ts.arguments },
                }),
            ));
            let item = json!({
                "type": "function_call",
                "id": ts.item_id,
                "call_id": ts.call_id,
                "name": ts.name,
                "arguments": if ts.arguments.is_empty() { "{}".to_string() } else { ts.arguments },
                "status": "incomplete",
            });
            self.output_items.insert(ts.output_index, item.clone());
            out.push(self.frame(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": ts.output_index,
                    "item": item,
                }),
            ));
        }
        let output = self.sorted_output();
        let status = if has_incomplete {
            "incomplete"
        } else {
            "completed"
        };
        out.push(self.frame(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": self.id,
                    "object": "response",
                    "created_at": self.created_at,
                    "status": status,
                    "model": self.model,
                    "output": output,
                }
            }),
        ));
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        let mut out = Vec::new();
        if !self.done {
            self.done = true;
            out.extend(self.close_output_item());
        }
        let output = self.sorted_output();
        out.push(self.frame(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": {
                    "id": self.id,
                    "object": "response",
                    "status": "failed",
                    "model": self.model,
                    "error": err.to_wire(Dialect::OpenAIResponses),
                    "output": output,
                }
            }),
        ));
        out
    }
}
