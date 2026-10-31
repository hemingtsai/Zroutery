//! Inbound half of the Responses dialect: items and parts in, canonical IR out.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, Message, ResponsesTools, Role,
    StructuredOutput, SystemPart, ToolChoice, ToolResultPart,
    DEFAULT_STRUCTURED_OUTPUT_SCHEMA_NAME,
};
use crate::protocol::reasoning_bridge;
use crate::protocol::unsupported_content;

/// Convert an OpenAI audio format string to a MIME subtype.
fn normalize_audio_format_to_mime(format: &str) -> &str {
    match format {
        "mp3" => "mpeg",
        "wav" => "wav",
        other => other,
    }
}
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

/// The request's `input` field normalized into Responses item form.
///
/// A bare string is one user message item — exactly the turn
/// [`decode_request`] lifts into the IR. Keeping a string as an empty list when
/// storing would silently drop that turn from every later
/// `previous_response_id` continuation, so both shapes converge here before
/// either storage or history expansion reads them.
pub fn input_items(body: &Value) -> Vec<Value> {
    match body.get("input") {
        Some(Value::String(text)) => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })],
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// Decode stored Responses items — a prior turn's input and output — into IR
/// messages, for `previous_response_id` continuation.
///
/// This is the same item decoder the request path uses, so a continued
/// conversation reconstructs what the earlier request meant: text, images,
/// reasoning blocks and `function_call`/`function_call_output` pairings all
/// survive the round trip through the store.
pub fn decode_history(items: &[Value]) -> Result<Vec<Message>> {
    let mut scratch = ChatRequest::new(String::new(), Dialect::OpenAIResponses);
    for item in items {
        decode_input_item(item, &mut scratch)?;
    }
    Ok(scratch.messages)
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
    req.responses_body = Some(body.clone());
    req.responses_native_only = obj.get("conversation").is_some_and(|v| !v.is_null())
        || obj
            .get("previous_response_id")
            .is_some_and(|v| !v.is_null())
        || obj.get("context_management").is_some_and(|v| !v.is_null());
    if let Some(include) = obj.get("include") {
        if !include.is_array() {
            return Err(Error::invalid("include must be an array"));
        }
    }

    // The initial catalog precedes history declarations; later additional_tools
    // items may replace an earlier definition with the same identity.
    match obj.get("tools") {
        None | Some(Value::Null) => {}
        Some(Value::Array(tools)) => {
            req.responses_tools = Some(ResponsesTools::default());
            super::tools::decode_tools(tools, &mut req)?;
        }
        Some(_) => return Err(Error::invalid("`tools` must be an array")),
    }

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
    for field in [
        "store",
        "previous_response_id",
        "truncation",
        "include",
        // The client's own prompt-cache partition. Re-deriving one would
        // silently miss the cache the client is asking for, so it travels
        // verbatim instead of being dropped.
        "prompt_cache_key",
    ] {
        if let Some(val) = obj.get(field) {
            req.passthrough.insert(field.to_string(), val.clone());
        }
    }

    req.tool_choice = match obj.get("tool_choice") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" | "any" => Some(ToolChoice::Any),
            other => return Err(unsupported_content("tool_choice", Some(other))),
        },
        Some(Value::Object(o)) => Some(ToolChoice::Specific {
            name: super::tools::call_name(&Value::Object(o.clone()))?,
        }),
        Some(_) => {
            return Err(Error::invalid(
                "`tool_choice` must be a string or an object",
            ))
        }
    };
    req.parallel_tool_use = obj.get("parallel_tool_calls").and_then(Value::as_bool);

    if let Some(reasoning) = obj.get("reasoning") {
        let effort = reasoning
            .get("effort")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("reasoning is missing `effort`"))?;
        req.thinking = Some(reasoning_bridge::decode_effort(effort).unwrap_or(
            crate::ir::ThinkingConfig {
                enabled: true,
                budget_tokens: None,
            },
        ));
        req.reasoning = Some(if effort == "none" {
            // Codex always sends an explicit effort, including `none`. That is
            // the absence of a reasoning requirement, not a native capability
            // that every candidate must advertise. The encoders still project
            // this as `none` on same-protocol Responses upstreams, while Chat
            // and Anthropic fallbacks can legally omit their reasoning knob.
            crate::ir::ReasoningRequest::Disabled
        } else {
            crate::ir::ReasoningRequest::native(effort).map_err(Error::invalid)?
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
pub(super) fn encode_text_format(structured: &StructuredOutput) -> Value {
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
pub(super) fn decode_reasoning_blocks(item: &Value) -> Vec<ContentBlock> {
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
    if item_type != "reasoning"
        && item
            .get("encrypted_content")
            .is_some_and(|value| !value.is_null())
    {
        req.responses_native_only = true;
    }
    match item_type {
        // These are upstream-owned context, never empty substitute messages.
        "compaction"
        | "compaction_summary"
        | "context_compaction"
        | "item_reference"
        | "web_search_call"
        | "tool_search_call"
        | "tool_search_output"
        | "configuration_update"
        | "compaction_trigger"
        | "image_generation_call" => {
            req.responses_native_only = true;
        }
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
            req.messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.to_string(),
                    name: super::tools::call_name(item)?,
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
