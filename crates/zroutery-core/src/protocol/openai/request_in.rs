//! Inbound half of the Chat Completions dialect: messages in, canonical IR out.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, Message, Role, StructuredOutput, SystemPart,
    ToolChoice, ToolDef, ToolResultPart,
};
use crate::protocol::reasoning_bridge;
use crate::protocol::unsupported_content;

const KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "stop",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "functions",
    "function_call",
    "reasoning_effort",
    "response_format",
    "user",
    "n",
];

// ---------------------------------------------------------------- request in

pub fn decode_request(body: Value) -> Result<ChatRequest> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::invalid("request body must be a JSON object"))?;

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("`model` is required"))?;
    let mut req = ChatRequest::new(model, Dialect::OpenAI);

    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("`messages` is required"))?;

    for m in messages {
        let role = m
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("message is missing `role`"))?;
        match role {
            "system" | "developer" => {
                if let Some(text) = flatten_content(m.get("content"))? {
                    req.system.push(SystemPart::new(text));
                }
            }
            "tool" | "function" => {
                let tool_use_id = m
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("tool message is missing `tool_call_id`"))?
                    .to_string();
                let block = ContentBlock::ToolResult {
                    tool_use_id,
                    name: String::new(),
                    content: decode_tool_result_content(m.get("content"))?,
                    is_error: false,
                };
                // Tool results belong to the following user turn.
                match req.messages.last_mut() {
                    Some(last)
                        if last.role == Role::User
                            && last
                                .content
                                .iter()
                                .all(|b| matches!(b, ContentBlock::ToolResult { .. })) =>
                    {
                        last.content.push(block)
                    }
                    _ => req.messages.push(Message {
                        role: Role::User,
                        content: vec![block],
                    }),
                }
            }
            "assistant" => {
                let mut content = Vec::new();
                // Reasoning models (DeepSeek et al) put their chain of thought in
                // `reasoning_content`; some relays require it to be passed back on
                // history turns, so it is preserved through the IR as a Thinking block.
                if let Some(reasoning) = m.get("reasoning_content").filter(|value| !value.is_null())
                {
                    decode_reasoning_value(reasoning, &mut content, "reasoning_content")?;
                } else if let Some(reasoning) = m.get("reasoning") {
                    decode_reasoning_value(reasoning, &mut content, "reasoning")?;
                }
                if let Some(text) = flatten_content(m.get("content"))? {
                    if !text.is_empty() {
                        content.push(ContentBlock::text(text));
                    }
                }
                if let Some(calls_value) = m.get("tool_calls") {
                    let calls = calls_value
                        .as_array()
                        .ok_or_else(|| Error::invalid("assistant tool_calls must be an array"))?;
                    for call in calls {
                        content.push(decode_tool_call(call)?);
                    }
                }
                req.messages.push(Message {
                    role: Role::Assistant,
                    content,
                });
            }
            "user" => {
                let content = decode_user_content(m.get("content"))?;
                req.messages.push(Message {
                    role: Role::User,
                    content,
                });
            }
            other => return Err(unsupported_content("message role", Some(other))),
        }
    }

    if obj.get("n").and_then(Value::as_u64).is_some_and(|n| n > 1) {
        return Err(Error::invalid("`n` greater than one is not representable"));
    }

    req.max_tokens = obj
        .get("max_completion_tokens")
        .or_else(|| obj.get("max_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.temperature = obj.get("temperature").and_then(Value::as_f64);
    req.top_p = obj.get("top_p").and_then(Value::as_f64);
    req.stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    req.stop_sequences = match obj.get("stop") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::invalid("stop sequence entries must be strings"))
            })
            .collect::<Result<Vec<_>>>()?,
        Some(_) => return Err(Error::invalid("`stop` must be a string or an array")),
    };

    let tool_source = obj.get("tools").or_else(|| obj.get("functions"));
    match tool_source {
        None | Some(Value::Null) => {}
        Some(Value::Array(tools)) => {
            for t in tools {
                let f = t.get("function").unwrap_or(t);
                let name = f
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("tool definition is missing `name`"))?;
                let strict = match f.get("strict") {
                    None | Some(Value::Null) => None,
                    Some(Value::Bool(strict)) => Some(*strict),
                    Some(_) => {
                        return Err(Error::invalid("tool `strict` must be a boolean"));
                    }
                };
                if let Some(strict) = strict {
                    req.tool_strict.insert(name.to_string(), strict);
                }
                req.tools.push(ToolDef {
                    name: name.to_string(),
                    description: f
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_schema: f
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
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_choice is missing function `name`"))?;
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

    // Whether the model may run tool calls concurrently sits next to the tool
    // choice in OpenAI; in Anthropic it is a member of the tool choice itself.
    req.parallel_tool_use = match obj.get("parallel_tool_calls") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(allow)) => Some(*allow),
        Some(_) => {
            return Err(Error::invalid("`parallel_tool_calls` must be a boolean"));
        }
    };

    // Reasoning effort is the OpenAI knob; translate to a thinking budget so
    // Anthropic upstreams get something meaningful. `xhigh` and `max` are part
    // of the current OpenAI vocabulary and must not be rejected here.
    if let Some(effort) = obj.get("reasoning_effort") {
        let effort = effort
            .as_str()
            .ok_or_else(|| Error::invalid("`reasoning_effort` must be a string"))?;
        req.thinking = Some(
            reasoning_bridge::decode_effort(effort)
                .ok_or_else(|| unsupported_content("reasoning effort", Some(effort)))?,
        );
    }

    req.metadata_user = obj.get("user").and_then(Value::as_str).map(str::to_string);

    // Structured output is a request property, not a vendor extension: it is
    // decoded here so it survives translation to another dialect and so the
    // request requires `Capability::StructuredOutput`.
    req.structured_output = decode_response_format(obj.get("response_format"))?;

    for (k, v) in obj {
        if !KNOWN_KEYS.contains(&k.as_str()) {
            req.passthrough.insert(k.clone(), v.clone());
        }
    }

    req.refresh_required_capabilities();
    Ok(req)
}

/// Decode OpenAI `response_format` into the IR structured-output model.
///
/// `{"type": "text"}` is the default and states no constraint. Unknown types
/// are refused rather than dropped, because a caller that asked for a
/// constraint must not silently receive free-form text.
fn decode_response_format(value: Option<&Value>) -> Result<Option<StructuredOutput>> {
    let value = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value,
    };
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("`response_format` is missing `type`"))?;
    match kind {
        "text" => Ok(None),
        "json_object" => Ok(Some(StructuredOutput::JsonObject)),
        "json_schema" => {
            let json_schema = value
                .get("json_schema")
                .ok_or_else(|| Error::invalid("`response_format.json_schema` is required"))?;
            let schema = json_schema.get("schema").cloned().ok_or_else(|| {
                Error::invalid("`response_format.json_schema.schema` is required")
            })?;
            Ok(Some(StructuredOutput::JsonSchema {
                name: json_schema
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                schema,
                strict: json_schema
                    .get("strict")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }))
        }
        other => Err(unsupported_content("response_format", Some(other))),
    }
}

/// True when the client asked for a usage chunk at the end of the stream.
pub fn wants_stream_usage(body: &Value) -> bool {
    body.get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn flatten_content(v: Option<&Value>) -> Result<Option<String>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Array(parts)) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    Value::String(s) => text.push_str(s),
                    Value::Object(_) => {
                        let kind = part.get("type").and_then(Value::as_str);
                        match kind {
                            Some("text") | None => {
                                let value =
                                    part.get("text").and_then(Value::as_str).ok_or_else(|| {
                                        Error::invalid("text content part is missing `text`")
                                    })?;
                                text.push_str(value);
                            }
                            Some(other) => {
                                return Err(unsupported_content("message", Some(other)));
                            }
                        }
                    }
                    _ => return Err(Error::invalid("content parts must be strings or objects")),
                }
            }
            Ok(Some(text))
        }
        Some(_) => Err(Error::invalid(
            "message content must be a string or an array",
        )),
    }
}

pub(super) fn decode_reasoning_value(
    value: &Value,
    content: &mut Vec<ContentBlock>,
    context: &str,
) -> Result<()> {
    match value {
        Value::Null => Ok(()),
        Value::String(text) => {
            if !text.is_empty() {
                content.push(ContentBlock::Thinking {
                    text: text.clone(),
                    signature: None,
                });
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                let block = reasoning_bridge::decode_reasoning_item(item).ok_or_else(|| {
                    unsupported_content(context, item.get("type").and_then(Value::as_str))
                })?;
                content.push(block);
            }
            Ok(())
        }
        _ => Err(Error::invalid(format!(
            "`{context}` must be a string or an array"
        ))),
    }
}

pub(super) fn decode_tool_call(call: &Value) -> Result<ContentBlock> {
    let function = call
        .get("function")
        .ok_or_else(|| Error::invalid("tool call is missing `function`"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("tool call is missing function `name`"))?;
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("tool call is missing `id`"))?;
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    Ok(ContentBlock::ToolUse {
        id: id.to_string(),
        name: name.to_string(),
        input: parse_arguments(Some(arguments)),
    })
}

fn decode_tool_result_content(value: Option<&Value>) -> Result<Vec<ToolResultPart>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![ToolResultPart::Text { text: text.clone() }]),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("tool result part is missing `type`"))?;
                match kind {
                    "text" => Ok(ToolResultPart::Text {
                        text: part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::invalid("tool result text part is missing `text`")
                            })?
                            .to_string(),
                    }),
                    "image_url" | "input_image" | "image" => {
                        let url = part
                            .get("image_url")
                            .and_then(|image| image.get("url"))
                            .and_then(Value::as_str)
                            .or_else(|| part.get("url").and_then(Value::as_str))
                            .or_else(|| part.get("file_id").and_then(Value::as_str));
                        let source = match url {
                            Some(url) if part.get("file_id").is_some() => MediaSource::Reference {
                                id: url.to_string(),
                            },
                            Some(url) => MediaSource::from_url(url),
                            None => {
                                return Err(Error::invalid(
                                    "image tool result part is missing `image_url.url`",
                                ));
                            }
                        };
                        Ok(ToolResultPart::Image { source })
                    }
                    other => Err(unsupported_content("tool result", Some(other))),
                }
            })
            .collect(),
        Some(_) => Err(Error::invalid(
            "tool message content must be a string or an array",
        )),
    }
}

fn decode_user_content(v: Option<&Value>) -> Result<Vec<ContentBlock>> {
    match v {
        Some(Value::String(s)) => Ok(vec![ContentBlock::text(s.clone())]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("content part is missing `type`"))?;
                match kind {
                    "text" => {
                        let text = part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("text content part is missing `text`"))?;
                        out.push(ContentBlock::text(text));
                    }
                    "image_url" => {
                        let url = part
                            .get("image_url")
                            .and_then(|image| image.get("url"))
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::invalid("image_url content part is missing `url`")
                            })?;
                        out.push(ContentBlock::Image {
                            source: MediaSource::from_url(url),
                        });
                    }
                    "input_audio" => {
                        let audio = part.get("input_audio").ok_or_else(|| {
                            Error::invalid("input_audio content part is missing `input_audio`")
                        })?;
                        let format = audio.get("format").and_then(Value::as_str).unwrap_or("wav");
                        let data = audio
                            .get("data")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("input_audio is missing `data`"))?;
                        let media_type = format!("audio/{format}");
                        out.push(ContentBlock::Audio {
                            source: MediaSource::Base64 {
                                media_type: media_type.clone(),
                                data: data.to_string(),
                            },
                            media_type,
                        });
                    }
                    "file" => {
                        // Support both legacy {file: {data, url, media_type}} and
                        // Responses-style {file_data, file_url, file_id, filename, media_type}.
                        let (source, media_type, name) = if let Some(file_obj) = part.get("file") {
                            // The official Chat Completions shape is
                            // `{"type":"file","file":{"file_id"|"file_data",...}}`;
                            // the flat `data`/`url` members are older relay
                            // spellings and stay supported as a fallback.
                            let data = file_obj.get("data").and_then(Value::as_str);
                            let file_data = file_obj.get("file_data").and_then(Value::as_str);
                            let url = file_obj.get("url").and_then(Value::as_str);
                            let file_id = file_obj.get("file_id").and_then(Value::as_str);
                            let media_type = file_obj
                                .get("media_type")
                                .or_else(|| file_obj.get("mime_type"))
                                .and_then(Value::as_str)
                                .unwrap_or("application/octet-stream")
                                .to_string();
                            let name = file_obj
                                .get("filename")
                                .or_else(|| file_obj.get("name"))
                                .and_then(Value::as_str)
                                .map(String::from);
                            let source = if let Some(data) = data {
                                MediaSource::Base64 {
                                    media_type: media_type.clone(),
                                    data: data.to_string(),
                                }
                            } else if let Some(file_data) = file_data {
                                // `file_data` is a base64 data URL in the
                                // official shape, so the media type it carries
                                // wins over the declared one.
                                MediaSource::from_url(file_data)
                            } else if let Some(url) = url {
                                MediaSource::Url {
                                    url: url.to_string(),
                                }
                            } else if let Some(id) = file_id {
                                MediaSource::Reference { id: id.to_string() }
                            } else {
                                return Err(Error::invalid(
                                    "file content is missing `data`, `url`, `file_data`, or `file_id`",
                                ));
                            };
                            (source, media_type, name)
                        } else {
                            let media_type = part
                                .get("media_type")
                                .and_then(Value::as_str)
                                .unwrap_or("application/octet-stream")
                                .to_string();
                            let name = part
                                .get("filename")
                                .and_then(Value::as_str)
                                .map(String::from);
                            let source = if let Some(data) =
                                part.get("file_data").and_then(Value::as_str)
                            {
                                MediaSource::Base64 {
                                    media_type: media_type.clone(),
                                    data: data.to_string(),
                                }
                            } else if let Some(url) = part.get("file_url").and_then(Value::as_str) {
                                MediaSource::Url {
                                    url: url.to_string(),
                                }
                            } else if let Some(id) = part.get("file_id").and_then(Value::as_str) {
                                MediaSource::Reference { id: id.to_string() }
                            } else {
                                return Err(Error::invalid(
                                    "file content is missing `file_data`, `file_url`, or `file_id`",
                                ));
                            };
                            (source, media_type, name)
                        };
                        out.push(ContentBlock::File {
                            source,
                            media_type,
                            name,
                        });
                    }
                    other => return Err(unsupported_content("message", Some(other))),
                }
            }
            Ok(out)
        }
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(Error::invalid("`content` must be a string or an array")),
    }
}

fn parse_arguments(args: Option<&str>) -> Value {
    match args {
        Some(s) if !s.trim().is_empty() => {
            serde_json::from_str(s).unwrap_or(Value::String(s.to_string()))
        }
        _ => json!({}),
    }
}

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
