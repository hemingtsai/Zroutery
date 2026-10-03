//! OpenAI Chat Completions dialect, also used for every "OpenAI compatible"
//! provider such as DeepSeek, Groq, Ollama, vLLM and OpenRouter.
//!
//! The tricky direction is OpenAI -> canonical: chunks carry no block
//! structure, so [`OpenAiStreamParser`] synthesises Anthropic style block
//! indices with a small state machine.

use serde_json::{json, Map, Value};

use super::apply_content_policy;
use super::reasoning_bridge;
use super::{explicit_placeholder, unsupported_content, ProviderQuirks};
use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, Message, Role, StopReason,
    StreamEvent, SystemPart, ThinkingConfig, ToolChoice, ToolDef, ToolResultPart,
    UnsupportedContentPolicy, Usage,
};

use super::{SseFrame, StreamEncoder, StreamParser};

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
    // Anthropic upstreams get something meaningful.
    if let Some(effort) = obj.get("reasoning_effort") {
        let effort = effort
            .as_str()
            .ok_or_else(|| Error::invalid("`reasoning_effort` must be a string"))?;
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

    req.metadata_user = obj.get("user").and_then(Value::as_str).map(str::to_string);

    for (k, v) in obj {
        if !KNOWN_KEYS.contains(&k.as_str()) {
            req.passthrough.insert(k.clone(), v.clone());
        }
    }

    req.refresh_required_capabilities();
    Ok(req)
}

/// True when the client asked for a usage chunk at the end of the stream.
pub fn wants_stream_usage(body: &Value) -> bool {
    body.get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn flatten_content(v: Option<&Value>) -> Result<Option<String>> {
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

fn decode_reasoning_value(
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

fn decode_tool_call(call: &Value) -> Result<ContentBlock> {
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
                            let data = file_obj.get("data").and_then(Value::as_str);
                            let url = file_obj.get("url").and_then(Value::as_str);
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
                            } else if let Some(url) = url {
                                MediaSource::Url {
                                    url: url.to_string(),
                                }
                            } else {
                                return Err(Error::invalid(
                                    "file content is missing `data`, `url`, or `file_id`",
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
fn arguments_json(input: &Value) -> String {
    match input {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// --------------------------------------------------------------- request out

/// The OpenAI dialect has no block structure on the system prompt, so the
/// parts are flattened back to text. A `cache_control` marker on a part is an
/// Anthropic concept and has no wire representation here; it simply does not
/// survive this direction.
fn system_text(system: &[SystemPart]) -> String {
    system
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    encode_request_with(req, upstream_model, &ProviderQuirks::default())
}

pub fn encode_request_with(
    req: &ChatRequest,
    upstream_model: &str,
    quirks: &ProviderQuirks,
) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));

    let mut messages: Vec<Value> = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({
            "role": if quirks.system_as_developer { "developer" } else { "system" },
            "content": system_text(&req.system),
        }));
    }
    for m in &req.messages {
        encode_message_into(
            m,
            &mut messages,
            req.source_dialect == Dialect::OpenAI,
            req.unsupported_content_policy,
        )?;
    }
    body.insert("messages".into(), Value::Array(messages));

    if let Some(mt) = req.max_tokens {
        let field = if quirks.use_max_completion_tokens {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(field.into(), json!(mt));
    }
    if let Some(t) = req.temperature {
        if !quirks.drop_temperature {
            body.insert("temperature".into(), json!(t));
        }
    }
    if let Some(p) = req.top_p {
        if !quirks.drop_top_p {
            body.insert("top_p".into(), json!(p));
        }
    }
    if !req.stop_sequences.is_empty() && !quirks.drop_stop {
        body.insert("stop".into(), json!(req.stop_sequences));
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
        if quirks.stream_usage {
            body.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        let mut function = Map::new();
                        function.insert("name".into(), json!(t.name));
                        function.insert(
                            "description".into(),
                            json!(t.description.clone().unwrap_or_default()),
                        );
                        function.insert("parameters".into(), t.input_schema.clone());
                        // `strict` is part of the function definition, not a
                        // vendor extension: dropping it would turn "obey this
                        // schema exactly" into an ordinary tool.
                        if let Some(strict) = req.tool_strict.get(&t.name) {
                            function.insert("strict".into(), json!(strict));
                        }
                        json!({"type": "function", "function": Value::Object(function)})
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
                ToolChoice::Specific { name } => {
                    json!({"type": "function", "function": {"name": name}})
                }
            },
        );
    }
    if let Some(allow) = req.parallel_tool_use {
        body.insert("parallel_tool_calls".into(), json!(allow));
    }
    if let Some(th) = &req.thinking {
        if quirks.send_reasoning_effort {
            let effort = match (th.enabled, th.budget_tokens.unwrap_or(4096)) {
                (false, _) => "none",
                (true, b) if b <= 1024 => "low",
                (true, b) if b >= 16384 => "high",
                _ => "medium",
            };
            body.insert("reasoning_effort".into(), json!(effort));
        }
    }
    if let Some(u) = &req.metadata_user {
        body.insert("user".into(), json!(u));
    }

    if req.source_dialect == Dialect::OpenAI {
        for (k, v) in &req.passthrough {
            body.entry(k.clone()).or_insert(v.clone());
        }
    }

    Ok(Value::Object(body))
}

/// Anthropic keeps tool results inside user messages; OpenAI needs separate
/// `role: "tool"` messages, and they must come before any plain user text.
fn encode_message_into(
    m: &Message,
    out: &mut Vec<Value>,
    echo_reasoning: bool,
    policy: UnsupportedContentPolicy,
) -> Result<()> {
    match m.role {
        Role::Assistant => {
            let mut text = String::new();
            let mut reasoning = String::new();
            let mut reasoning_items: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t, .. } => text.push_str(t),
                    ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments_json(input)},
                    })),
                    // Reasoning history is echoed only when the source dialect
                    // supports it.  Otherwise it follows the same explicit
                    // unsupported-content policy as every other block; it is
                    // never silently discarded.
                    ContentBlock::Thinking { text: t, signature } => {
                        if echo_reasoning {
                            if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                                reasoning_items.push(item);
                            } else {
                                reasoning.push_str(t);
                            }
                            let _ = signature;
                        } else if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(replacement_text) = replacement.as_text() {
                                text.push_str(replacement_text);
                            }
                        }
                    }
                    ContentBlock::RedactedThinking { .. } => {
                        if echo_reasoning {
                            if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                                reasoning_items.push(item);
                            }
                        } else if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(replacement_text) = replacement.as_text() {
                                text.push_str(replacement_text);
                            }
                        }
                    }
                    // Media and structured content that cannot be represented in
                    // an assistant turn follows the explicit policy.
                    ContentBlock::Audio { .. }
                    | ContentBlock::Document { .. }
                    | ContentBlock::File { .. }
                    | ContentBlock::Video { .. }
                    | ContentBlock::Image { .. }
                    | ContentBlock::ToolResult { .. }
                    | ContentBlock::Citation { .. }
                    | ContentBlock::Annotation { .. } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                text.push_str(t);
                            }
                        }
                    }
                }
            }
            let mut msg = Map::new();
            msg.insert("role".into(), json!("assistant"));
            if text.is_empty() && !tool_calls.is_empty() {
                msg.insert("content".into(), Value::Null);
            } else {
                msg.insert("content".into(), json!(text));
            }
            if !reasoning.is_empty() {
                msg.insert("reasoning_content".into(), json!(reasoning));
            }
            if !reasoning_items.is_empty() {
                msg.insert("reasoning".into(), Value::Array(reasoning_items));
            }
            if !tool_calls.is_empty() {
                msg.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            out.push(Value::Object(msg));
        }
        Role::User => {
            let mut parts: Vec<Value> = Vec::new();
            for b in &m.content {
                match b {
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
                                    if let Some(replacement) = apply_content_policy(policy, &image)?
                                    {
                                        if let Some(text) = replacement.as_text() {
                                            text_parts.push(text.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": text_parts.join("\n"),
                        }));
                    }
                    ContentBlock::Text { text, .. } => {
                        parts.push(json!({"type": "text", "text": text}))
                    }
                    ContentBlock::Image {
                        source: MediaSource::Reference { .. },
                    } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"type": "text", "text": t}));
                            }
                        }
                    }
                    ContentBlock::Image { source } => parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": source.to_data_url()},
                    })),
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
                                // URL audio cannot be represented as input_audio;
                                // apply the policy.
                                if let Some(replacement) = apply_content_policy(policy, b)? {
                                    if let Some(t) = replacement.as_text() {
                                        parts.push(json!({"type": "text", "text": t}));
                                    }
                                }
                            }
                        }
                    }
                    ContentBlock::Document { .. }
                    | ContentBlock::File { .. }
                    | ContentBlock::Video { .. }
                    | ContentBlock::Citation { .. }
                    | ContentBlock::Annotation { .. }
                    | ContentBlock::ToolUse { .. }
                    | ContentBlock::Thinking { .. }
                    | ContentBlock::RedactedThinking { .. } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"type": "text", "text": t}));
                            }
                        }
                    }
                }
            }
            if !parts.is_empty() {
                // Keep the simple string form when there is only text.
                let all_text = parts.iter().all(|p| p["type"] == "text");
                let content = if all_text {
                    json!(parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(""))
                } else {
                    Value::Array(parts)
                };
                out.push(json!({"role": "user", "content": content}));
            }
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- responses

pub fn finish_reason_from_str(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

pub fn finish_reason_to_str(r: StopReason) -> Option<&'static str> {
    match r {
        StopReason::EndTurn | StopReason::StopSequence => Some("stop"),
        StopReason::MaxTokens => Some("length"),
        StopReason::ToolUse => Some("tool_calls"),
        StopReason::Refusal => Some("content_filter"),
        StopReason::Unknown => None,
    }
}

pub(crate) fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(u) = v.filter(|u| !u.is_null()) else {
        return Usage::default();
    };
    Usage {
        input_tokens: u
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        output_tokens: u
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_read_tokens: u
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| u.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
    }
}

pub(crate) fn encode_usage(u: &Usage) -> Value {
    json!({
        "prompt_tokens": u.input_tokens,
        "completion_tokens": u.output_tokens,
        "total_tokens": u.total(),
        "prompt_tokens_details": {"cached_tokens": u.cache_read_tokens},
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
        passthrough: Map::new(),
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

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
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

// ---------------------------------------------------------------- stream in

/// The kinds of block that stream as a single running block. Tool blocks are
/// not part of this enum: several of them can be open at the same time.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Block {
    Text,
    Thinking,
}

/// Rebuilds Anthropic style block structure from OpenAI chunks.
pub struct OpenAiStreamParser {
    model: String,
    id: Option<String>,
    started: bool,
    next_index: u32,
    /// The open text or thinking block, if any.  Tool calls are tracked apart
    /// from it: OpenAI delivers every parallel call under its own
    /// `tool_calls[].index` with no per-call end marker, so opening a second
    /// call says nothing about whether the first one is still streaming.
    current: Option<(u32, Block)>,
    /// OpenAI `tool_calls[].index` -> canonical block index.
    tool_slots: Vec<(u64, u32)>,
    /// Canonical indices of tool blocks that were started and not yet stopped.
    open_tools: Vec<u32>,
    /// Text/thinking deltas that arrived while a tool block was open.  An
    /// Anthropic stream carries one block at a time, so these are replayed
    /// after every tool block has stopped instead of being interleaved.
    deferred: Vec<(Block, String)>,
    usage: Usage,
    pending_stop: Option<(StopReason, Option<String>)>,
    emitted_stop: bool,
}

impl OpenAiStreamParser {
    pub fn new(model: &str) -> Self {
        OpenAiStreamParser {
            model: model.to_string(),
            id: None,
            started: false,
            next_index: 0,
            current: None,
            tool_slots: Vec::new(),
            open_tools: Vec::new(),
            deferred: Vec::new(),
            usage: Usage::default(),
            pending_stop: None,
            emitted_stop: false,
        }
    }

    fn close_current(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((index, _)) = self.current.take() {
            out.push(StreamEvent::BlockStop { index });
        }
    }

    /// Return the index of an open block of `kind`, opening a new one if needed.
    fn block_for(&mut self, kind: Block, out: &mut Vec<StreamEvent>) -> u32 {
        if let Some((index, current)) = self.current {
            if current == kind {
                return index;
            }
        }
        self.open_block(kind, out)
    }

    /// Start a fresh text/thinking block, closing whatever was open before.
    fn open_block(&mut self, kind: Block, out: &mut Vec<StreamEvent>) -> u32 {
        self.close_current(out);
        let index = self.next_index;
        self.next_index += 1;
        self.current = Some((index, kind));
        index
    }

    /// Start a fresh tool block.  It closes the open text/thinking block (an
    /// Anthropic stream never interleaves them) but deliberately leaves every
    /// other tool block open until the stream ends.
    fn open_tool_block(&mut self, out: &mut Vec<StreamEvent>) -> u32 {
        self.close_current(out);
        let index = self.next_index;
        self.next_index += 1;
        self.open_tools.push(index);
        index
    }

    /// Emit a text or thinking delta.  While a tool block is open the content
    /// is buffered so the emitted lifecycle stays one-block-at-a-time.
    fn push_text_delta(&mut self, kind: Block, text: String, out: &mut Vec<StreamEvent>) {
        if self.open_tools.is_empty() {
            let index = self.block_for(kind, out);
            out.push(match kind {
                Block::Thinking => StreamEvent::ThinkingDelta { index, text },
                _ => StreamEvent::TextDelta { index, text },
            });
        } else {
            match self.deferred.last_mut() {
                Some((last, buffered)) if *last == kind => buffered.push_str(&text),
                _ => self.deferred.push((kind, text)),
            }
        }
    }

    /// Replay the buffered text/thinking as complete blocks.  Called once every
    /// tool block has stopped.
    fn flush_deferred(&mut self, out: &mut Vec<StreamEvent>) {
        for (kind, text) in std::mem::take(&mut self.deferred) {
            let index = self.open_block(kind, out);
            out.push(match kind {
                Block::Thinking => StreamEvent::ThinkingDelta { index, text },
                _ => StreamEvent::TextDelta { index, text },
            });
            self.close_current(out);
        }
    }

    fn ensure_started(&mut self, chunk: &Value, out: &mut Vec<StreamEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let id = chunk
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl_stream")
            .to_string();
        if let Some(m) = chunk.get("model").and_then(Value::as_str) {
            if !m.is_empty() {
                self.model = m.to_string();
            }
        }
        self.id = Some(id.clone());
        out.push(StreamEvent::Start {
            id,
            model: self.model.clone(),
            usage: self.usage,
        });
    }

    fn emit_stop(&mut self, out: &mut Vec<StreamEvent>) {
        if self.emitted_stop {
            return;
        }
        let (stop_reason, stop_sequence) = self
            .pending_stop
            .take()
            .unwrap_or((StopReason::Unknown, None));
        self.close_current(out);
        // Parallel calls stay open until here: their argument deltas may
        // arrive in any order, so no earlier point proves a call is finished.
        for index in std::mem::take(&mut self.open_tools) {
            out.push(StreamEvent::BlockStop { index });
        }
        self.flush_deferred(out);
        out.push(StreamEvent::Stop {
            stop_reason,
            stop_sequence,
            usage: self.usage,
        });
        self.emitted_stop = true;
    }
}

impl StreamParser for OpenAiStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        let data = frame.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            let mut out = Vec::new();
            if self.started {
                self.emit_stop(&mut out);
            }
            return Ok(out);
        }

        let chunk = frame.json()?;
        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream stream error");
            return Err(Error::BadUpstreamPayload(msg.to_string()));
        }

        let mut out = Vec::new();
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            let parsed = decode_usage(Some(u));
            if parsed.total() > 0 {
                self.usage = parsed;
            }
        }

        let choices = chunk
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if choices.is_empty() {
            // Usage-only trailer: flush the stop we deferred.
            if self.pending_stop.is_some() {
                self.ensure_started(&chunk, &mut out);
                self.emit_stop(&mut out);
            }
            return Ok(out);
        }

        self.ensure_started(&chunk, &mut out);

        for choice in choices {
            // Only the first choice is supported; `n > 1` is not representable
            // in the Anthropic dialect.
            if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
                return Err(Error::BadUpstreamPayload(
                    "multiple response choices are not representable".into(),
                ));
            }
            let delta = choice.get("delta").or_else(|| choice.get("message"));

            if let Some(reasoning) =
                delta.and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
            {
                let text = match reasoning {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    _ => {
                        return Err(Error::BadUpstreamPayload(
                            "stream reasoning must be a string".into(),
                        ));
                    }
                };
                if !text.is_empty() {
                    self.push_text_delta(Block::Thinking, text, &mut out);
                }
            }

            if let Some(content) = delta.and_then(|d| d.get("content")) {
                // `content` is optional in the OpenAI delta shape, so an
                // explicit null is a legal chunk that merely carries no text
                // (role-only, tool-only and reasoning-only deltas all look
                // like this).  Only a genuinely wrong JSON type is an error,
                // and it is the upstream that produced it.
                let text = match content {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    Value::Array(_) => flatten_content(Some(content))
                        .map_err(|err| {
                            Error::BadUpstreamPayload(format!(
                                "stream content array is malformed: {err}"
                            ))
                        })?
                        .unwrap_or_default(),
                    _ => {
                        return Err(Error::BadUpstreamPayload(
                            "stream content must be a string, an array, or null".into(),
                        ))
                    }
                };
                if !text.is_empty() {
                    self.push_text_delta(Block::Text, text, &mut out);
                }
            }

            if let Some(calls) = delta
                .and_then(|d| d.get("tool_calls"))
                .and_then(Value::as_array)
            {
                for call in calls {
                    let slot = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let known = self
                        .tool_slots
                        .iter()
                        .find(|(s, _)| *s == slot)
                        .map(|(_, i)| *i);
                    let index = match known {
                        Some(i) => i,
                        None => {
                            let id = call.get("id").and_then(Value::as_str).ok_or_else(|| {
                                Error::BadUpstreamPayload("stream tool call is missing `id`".into())
                            })?;
                            let name = call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    Error::BadUpstreamPayload(
                                        "stream tool call is missing function `name`".into(),
                                    )
                                })?;
                            let i = self.open_tool_block(&mut out);
                            self.tool_slots.push((slot, i));
                            out.push(StreamEvent::ToolUseStart {
                                index: i,
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                            i
                        }
                    };
                    if let Some(args) = call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        out.push(StreamEvent::ToolUseDelta {
                            index,
                            partial_json: args.to_string(),
                        });
                    }
                }
            }

            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
            {
                // Defer the Stop event: a trailing usage chunk may still arrive.
                self.pending_stop = Some((finish_reason_from_str(reason), None));
            }
        }

        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if self.started {
            self.emit_stop(&mut out);
        }
        out
    }
}

// --------------------------------------------------------------- stream out

/// Renders canonical events as OpenAI chat completion chunks.
pub struct OpenAiStreamEncoder {
    model: String,
    id: String,
    created: i64,
    include_usage: bool,
    role_sent: bool,
    tool_slots: Vec<(u32, u64)>,
    usage: Usage,
    done: bool,
}

impl OpenAiStreamEncoder {
    pub fn new(model: &str) -> Self {
        OpenAiStreamEncoder {
            model: model.to_string(),
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            created: chrono::Utc::now().timestamp(),
            include_usage: false,
            role_sent: false,
            tool_slots: Vec::new(),
            usage: Usage::default(),
            done: false,
        }
    }

    pub fn with_usage(mut self, include: bool) -> Self {
        self.include_usage = include;
        self
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>) -> SseFrame {
        let mut choice = Map::new();
        choice.insert("index".into(), json!(0));
        choice.insert("delta".into(), delta);
        choice.insert("logprobs".into(), Value::Null);
        choice.insert(
            "finish_reason".into(),
            match finish_reason {
                Some(r) => json!(r),
                None => Value::Null,
            },
        );
        SseFrame {
            event: None,
            data: json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [Value::Object(choice)],
            })
            .to_string(),
        }
    }

    fn ensure_role(&mut self, out: &mut Vec<SseFrame>) {
        if self.role_sent {
            return;
        }
        self.role_sent = true;
        out.push(self.chunk(json!({"role": "assistant", "content": ""}), None));
    }

    fn slot_for(&mut self, index: u32) -> u64 {
        if let Some((_, slot)) = self.tool_slots.iter().find(|(i, _)| *i == index) {
            return *slot;
        }
        let slot = self.tool_slots.len() as u64;
        self.tool_slots.push((index, slot));
        slot
    }
}

impl StreamEncoder for OpenAiStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        let mut out = Vec::new();
        match event {
            StreamEvent::Start { id, usage, .. } => {
                // The upstream's own model name is ignored on purpose: a client
                // that asked for an exposed id is told that id, exactly as the
                // non-streaming encoder reports it.
                self.id = if id.starts_with("chatcmpl") {
                    id.clone()
                } else {
                    format!("chatcmpl-{id}")
                };
                self.usage = *usage;
                self.ensure_role(&mut out);
            }
            StreamEvent::TextDelta { text, .. } => {
                self.ensure_role(&mut out);
                out.push(self.chunk(json!({"content": text}), None));
            }
            StreamEvent::ThinkingDelta { text, .. } => {
                self.ensure_role(&mut out);
                out.push(self.chunk(json!({"reasoning_content": text}), None));
            }
            // No Chat Completions wire equivalent; signatures are only meaningful
            // when a later request is encoded through the reasoning bridge.
            StreamEvent::ThinkingSignature { .. } => {}
            StreamEvent::RedactedThinking { index, data } => {
                if let Some(item) =
                    reasoning_bridge::encode_thinking_block(&ContentBlock::RedactedThinking {
                        data: data.clone(),
                    })
                {
                    if let Some(encoded) = item.get("encrypted_content").and_then(Value::as_str) {
                        self.ensure_role(&mut out);
                        out.push(self.chunk(json!({"reasoning_content": encoded}), None));
                    }
                }
                let _ = index;
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.ensure_role(&mut out);
                let slot = self.slot_for(*index);
                out.push(self.chunk(
                    json!({"tool_calls": [{
                        "index": slot,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    }]}),
                    None,
                ));
            }
            StreamEvent::ToolUseDelta {
                index,
                partial_json,
            } => {
                self.ensure_role(&mut out);
                let slot = self.slot_for(*index);
                out.push(self.chunk(
                    json!({"tool_calls": [{
                        "index": slot,
                        "function": {"arguments": partial_json}
                    }]}),
                    None,
                ));
            }
            StreamEvent::BlockStop { .. } => {}
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                self.ensure_role(&mut out);
                self.usage = *usage;
                out.push(self.chunk(
                    json!({}),
                    Some(finish_reason_to_str(*stop_reason).unwrap_or("stop")),
                ));
                if self.include_usage {
                    out.push(SseFrame {
                        event: None,
                        data: json!({
                            "id": self.id,
                            "object": "chat.completion.chunk",
                            "created": self.created,
                            "model": self.model,
                            "choices": [],
                            "usage": encode_usage(usage),
                        })
                        .to_string(),
                    });
                }
                out.push(SseFrame {
                    event: None,
                    data: "[DONE]".into(),
                });
                self.done = true;
            }
            StreamEvent::Ping => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.done {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_role(&mut out);
        out.push(self.chunk(json!({}), Some("stop")));
        out.push(SseFrame {
            event: None,
            data: "[DONE]".into(),
        });
        self.done = true;
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        let mut out = vec![SseFrame {
            event: None,
            data: err.to_wire(Dialect::OpenAI).to_string(),
        }];
        if !self.done {
            out.push(SseFrame {
                event: None,
                data: "[DONE]".into(),
            });
            self.done = true;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SseDecoder;

    fn parse(raw: &str) -> Vec<StreamEvent> {
        let mut dec = SseDecoder::new();
        let mut parser = OpenAiStreamParser::new("fallback");
        let mut events = Vec::new();
        for f in dec.push(raw.as_bytes()) {
            events.extend(parser.push(&f).unwrap());
        }
        events.extend(parser.finish());
        events
    }

    fn chunk(delta: Value) -> String {
        format!(
            "data: {}\n\n",
            json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"deepseek-v4-pro",
                   "choices":[{"index":0,"delta":delta,"finish_reason":null}]})
        )
    }

    #[test]
    fn decodes_system_user_and_tool_history() {
        let req = decode_request(json!({
            "model": "gpt-5.3-sol",
            "messages": [
                {"role": "system", "content": "be nice"},
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "get_weather", "arguments": "{\"city\":\"SH\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
                {"role": "user", "content": "thanks"}
            ],
            "max_completion_tokens": 256,
            "stop": "END",
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}],
            "tool_choice": "required",
            "reasoning_effort": "high",
            "user": "u9"
        }))
        .unwrap();

        assert_eq!(req.system, vec![SystemPart::new("be nice")]);
        assert_eq!(req.max_tokens, Some(256));
        assert_eq!(req.stop_sequences, vec!["END"]);
        assert_eq!(req.tool_choice, Some(ToolChoice::Any));
        assert_eq!(req.thinking.unwrap().budget_tokens, Some(16384));
        assert_eq!(req.metadata_user.as_deref(), Some("u9"));
        assert_eq!(req.messages.len(), 4);
        assert!(matches!(
            req.messages[1].content[0],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            req.messages[2].content[0],
            ContentBlock::ToolResult { .. }
        ));
        assert_eq!(req.messages[3].content[0], ContentBlock::text("thanks"));
    }

    #[test]
    fn decodes_multimodal_user_content() {
        let req = decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QQ=="}}
            ]}]
        }))
        .unwrap();
        assert_eq!(req.messages[0].content.len(), 2);
        assert_eq!(
            req.messages[0].content[1],
            ContentBlock::Image {
                source: MediaSource::Base64 {
                    media_type: "image/png".into(),
                    data: "QQ==".into()
                }
            }
        );
    }

    #[test]
    fn tool_history_round_trips_back_to_openai_shape() {
        let original = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "s"},
                {"role": "user", "content": "q"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "f", "arguments": "{\"a\":1}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
            ]
        });
        let req = decode_request(original).unwrap();
        let body = encode_request(&req, "up").unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], Value::Null);
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
        assert_eq!(msgs[3]["content"], "ok");
        // and again through the decoder
        let again = decode_request(body).unwrap();
        assert_eq!(again.messages, req.messages);
        assert_eq!(again.system, req.system);
    }

    #[test]
    fn quirks_control_parameter_names() {
        let req = decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 10,
            "temperature": 0.7,
            "stop": ["x"],
            "stream": true
        }))
        .unwrap();

        let plain = encode_request(&req, "up").unwrap();
        assert_eq!(plain["max_tokens"], 10);
        assert_eq!(plain["temperature"], 0.7);
        assert_eq!(
            plain["temperature"].to_string(),
            "0.7",
            "no float noise on the wire"
        );
        assert_eq!(plain["stream_options"]["include_usage"], true);

        let strict = ProviderQuirks {
            use_max_completion_tokens: true,
            drop_temperature: true,
            drop_stop: true,
            stream_usage: false,
            system_as_developer: true,
            ..ProviderQuirks::default()
        };
        let body = encode_request_with(&req, "up", &strict).unwrap();
        assert_eq!(body["max_completion_tokens"], 10);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("stop").is_none());
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn thinking_maps_to_reasoning_effort_only_when_supported() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.thinking = Some(ThinkingConfig {
            enabled: true,
            budget_tokens: Some(20000),
        });
        assert!(encode_request(&req, "up")
            .unwrap()
            .get("reasoning_effort")
            .is_none());
        let quirks = ProviderQuirks {
            send_reasoning_effort: true,
            ..ProviderQuirks::default()
        };
        assert_eq!(
            encode_request_with(&req, "up", &quirks).unwrap()["reasoning_effort"],
            "high"
        );
    }

    #[test]
    fn response_with_reasoning_and_tool_calls() {
        let resp = decode_response(json!({
            "id": "chatcmpl-9",
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "here you go",
                    "reasoning_content": "let me think",
                    "tool_calls": [{"id": "call_1", "type": "function",
                                    "function": {"name": "f", "arguments": "{}"}}]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 34,
                      "prompt_tokens_details": {"cached_tokens": 5},
                      "completion_tokens_details": {"reasoning_tokens": 7}}
        }))
        .unwrap();

        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert!(matches!(resp.content[0], ContentBlock::Thinking { .. }));
        assert_eq!(resp.content[1], ContentBlock::text("here you go"));
        assert!(matches!(resp.content[2], ContentBlock::ToolUse { .. }));
        assert_eq!(resp.usage.input_tokens, 12);
        assert_eq!(resp.usage.cache_read_tokens, 5);
        assert_eq!(resp.usage.reasoning_tokens, 7);

        let back = encode_response(&resp);
        assert_eq!(back["choices"][0]["message"]["content"], "here you go");
        assert_eq!(
            back["choices"][0]["message"]["reasoning_content"],
            "let me think"
        );
        assert_eq!(back["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(back["usage"]["total_tokens"], 46);
    }

    #[test]
    fn deepseek_cache_hit_field_is_understood() {
        let u = decode_usage(Some(&json!({
            "prompt_tokens": 100, "completion_tokens": 1, "prompt_cache_hit_tokens": 64
        })));
        assert_eq!(u.cache_read_tokens, 64);
    }

    #[test]
    fn parses_text_stream_with_usage_trailer() {
        let finish = json!({"id":"chatcmpl-1","model":"deepseek-v4-pro",
                            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
        let trailer = json!({"id":"chatcmpl-1","model":"deepseek-v4-pro","choices":[],
                             "usage":{"prompt_tokens":9,"completion_tokens":2}});
        let raw = format!(
            "{}{}{}data: {finish}\n\ndata: {trailer}\n\ndata: [DONE]\n\n",
            chunk(json!({"role": "assistant", "content": ""})),
            chunk(json!({"content": "Hel"})),
            chunk(json!({"content": "lo"})),
        );
        let events = parse(&raw);
        assert_eq!(
            events[0],
            StreamEvent::Start {
                id: "chatcmpl-1".into(),
                model: "deepseek-v4-pro".into(),
                usage: Usage::default()
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::TextDelta {
                index: 0,
                text: "Hel".into()
            }
        );
        assert_eq!(
            events[2],
            StreamEvent::TextDelta {
                index: 0,
                text: "lo".into()
            }
        );
        assert_eq!(events[3], StreamEvent::BlockStop { index: 0 });
        match &events[4] {
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                assert_eq!(*stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 9);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(events.len(), 5, "no duplicate stop after [DONE]");
    }

    #[test]
    fn reasoning_then_text_opens_two_blocks() {
        let raw = format!(
            "{}{}{}",
            chunk(json!({"reasoning_content": "думаю"})),
            chunk(json!({"content": "answer"})),
            "data: [DONE]\n\n"
        );
        let events = parse(&raw);
        assert_eq!(
            events[1],
            StreamEvent::ThinkingDelta {
                index: 0,
                text: "думаю".into()
            }
        );
        assert_eq!(events[2], StreamEvent::BlockStop { index: 0 });
        assert_eq!(
            events[3],
            StreamEvent::TextDelta {
                index: 1,
                text: "answer".into()
            }
        );
        assert!(matches!(events.last().unwrap(), StreamEvent::Stop { .. }));
    }

    #[test]
    fn parses_incremental_tool_calls() {
        let raw = format!(
            "{}{}{}{}{}",
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                                          "function": {"name": "get_weather", "arguments": ""}}]})
            ),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"ci"}}]})),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "ty\":1}"}}]})),
            chunk(
                json!({"tool_calls": [{"index": 1, "id": "call_2", "type": "function",
                                          "function": {"name": "other", "arguments": "{}"}}]})
            ),
            "data: [DONE]\n\n"
        );
        let events = parse(&raw);
        assert_eq!(
            events[1],
            StreamEvent::ToolUseStart {
                index: 0,
                id: "call_1".into(),
                name: "get_weather".into()
            }
        );
        assert_eq!(
            events[2],
            StreamEvent::ToolUseDelta {
                index: 0,
                partial_json: "{\"ci".into()
            }
        );
        assert_eq!(
            events[3],
            StreamEvent::ToolUseDelta {
                index: 0,
                partial_json: "ty\":1}".into()
            }
        );
        assert_eq!(
            events[4],
            StreamEvent::ToolUseStart {
                index: 1,
                id: "call_2".into(),
                name: "other".into()
            }
        );
        assert_eq!(
            events[5],
            StreamEvent::ToolUseDelta {
                index: 1,
                partial_json: "{}".into()
            }
        );
        // Both calls stay open until the stream ends, then stop in order.
        assert_eq!(events[6], StreamEvent::BlockStop { index: 0 });
        assert_eq!(events[7], StreamEvent::BlockStop { index: 1 });
        assert!(matches!(events[8], StreamEvent::Stop { .. }));
        assert_legal_lifecycle(&events);
    }

    /// Anthropic lifecycle rules the parser must honour: a block receives no
    /// delta after it stopped, and blocks stop in the order they were opened.
    fn assert_legal_lifecycle(events: &[StreamEvent]) {
        let mut stopped: Vec<u32> = Vec::new();
        for event in events {
            match event {
                StreamEvent::BlockStop { index } => {
                    assert!(
                        !stopped.contains(index),
                        "block {index} stopped twice: {events:?}"
                    );
                    if let Some(last) = stopped.last() {
                        assert!(
                            index > last,
                            "block {index} stopped before block {last}: {events:?}"
                        );
                    }
                    stopped.push(*index);
                }
                StreamEvent::TextDelta { index, .. }
                | StreamEvent::ThinkingDelta { index, .. }
                | StreamEvent::ThinkingSignature { index, .. }
                | StreamEvent::RedactedThinking { index, .. }
                | StreamEvent::ToolUseDelta { index, .. } => assert!(
                    !stopped.contains(index),
                    "delta for block {index} after its stop: {events:?}"
                ),
                _ => {}
            }
        }
    }

    fn tool_arguments(events: &[StreamEvent], wanted: u32) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolUseDelta {
                    index,
                    partial_json,
                } if *index == wanted => Some(partial_json.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn interleaved_parallel_tool_calls_keep_every_block_open() {
        // OpenAI delimits parallel calls by `tool_calls[].index`; their
        // argument deltas may arrive in any order, so a new call index proves
        // nothing about the calls already running.
        let raw = format!(
            "{}{}{}{}{}",
            chunk(json!({"tool_calls": [
                {"index": 0, "id": "call_a", "type": "function",
                 "function": {"name": "fa", "arguments": "{\"x\":"}},
                {"index": 1, "id": "call_b", "type": "function",
                 "function": {"name": "fb", "arguments": "{\"y\":"}}
            ]})),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "1"}}]})),
            chunk(json!({"tool_calls": [{"index": 1, "function": {"arguments": "2}"}}]})),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "}"}}]})),
            "data: [DONE]\n\n"
        );
        let events = parse(&raw);
        assert_legal_lifecycle(&events);

        let starts: Vec<u32> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolUseStart { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![0, 1], "both calls must open: {events:?}");
        assert_eq!(tool_arguments(&events, 0), "{\"x\":1}");
        assert_eq!(tool_arguments(&events, 1), "{\"y\":2}");

        let tail = events.len() - 3;
        assert_eq!(events[tail], StreamEvent::BlockStop { index: 0 });
        assert_eq!(events[tail + 1], StreamEvent::BlockStop { index: 1 });
        assert!(matches!(events[tail + 2], StreamEvent::Stop { .. }));
    }

    #[test]
    fn text_after_open_tools_is_replayed_in_order() {
        // Text and reasoning that arrive while tool arguments are still
        // streaming are replayed as their own blocks once the tools stopped,
        // so no block ever receives a delta after its stop.
        let raw = format!(
            "{}{}{}{}{}{}",
            chunk(json!({"reasoning_content": "plan"})),
            chunk(json!({"content": "first"})),
            chunk(json!({"tool_calls": [
                {"index": 0, "id": "call_a", "type": "function",
                 "function": {"name": "fa", "arguments": "{}"}},
                {"index": 1, "id": "call_b", "type": "function",
                 "function": {"name": "fb", "arguments": "{}"}}
            ]})),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "extra"}}]})),
            chunk(json!({"content": "second"})),
            "data: [DONE]\n\n"
        );
        let events = parse(&raw);
        assert_legal_lifecycle(&events);

        assert!(events.contains(&StreamEvent::ThinkingDelta {
            index: 0,
            text: "plan".into()
        }));
        assert!(events.contains(&StreamEvent::TextDelta {
            index: 1,
            text: "first".into()
        }));
        // The late text survives, in a block of its own after the tools.
        let late = events
            .iter()
            .position(
                |event| matches!(event, StreamEvent::TextDelta { text, .. } if text == "second"),
            )
            .expect("late text must not be dropped");
        for index in [2u32, 3] {
            let stop = events
                .iter()
                .position(
                    |event| matches!(event, StreamEvent::BlockStop { index: i } if *i == index),
                )
                .expect("tool block must stop");
            assert!(
                late > stop,
                "late text must follow the stop of block {index}: {events:?}"
            );
        }
        assert_eq!(tool_arguments(&events, 2), "{}extra");
    }

    #[test]
    fn truncated_stream_still_stops() {
        let events = parse(&chunk(json!({"content": "partial"})));
        assert_eq!(
            events[1],
            StreamEvent::TextDelta {
                index: 0,
                text: "partial".into()
            }
        );
        assert_eq!(events[2], StreamEvent::BlockStop { index: 0 });
        assert!(matches!(
            events[3],
            StreamEvent::Stop {
                stop_reason: StopReason::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn stream_error_payload_surfaces() {
        let mut p = OpenAiStreamParser::new("m");
        let f = SseFrame {
            event: None,
            data: r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#.into(),
        };
        assert!(p.push(&f).unwrap_err().to_string().contains("rate limited"));
    }

    #[test]
    fn null_stream_content_is_not_an_error() {
        // `content` is an optional member of the OpenAI delta shape, so an
        // explicit null is a legal chunk that simply carries no text.
        let role_only = parse(&chunk(json!({"role": "assistant", "content": null})));
        assert_eq!(
            role_only.len(),
            2,
            "role-only delta must only start and stop the message: {role_only:?}"
        );

        let reasoning_only = parse(&chunk(
            json!({"content": null, "reasoning_content": "думаю"}),
        ));
        assert!(reasoning_only.contains(&StreamEvent::ThinkingDelta {
            index: 0,
            text: "думаю".into()
        }));

        let tool_only = parse(&chunk(json!({
            "content": null,
            "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                            "function": {"name": "get_weather", "arguments": "{}"}}]
        })));
        assert!(tool_only.contains(&StreamEvent::ToolUseStart {
            index: 0,
            id: "call_1".into(),
            name: "get_weather".into()
        }));
        assert!(tool_only.contains(&StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "{}".into()
        }));
    }

    #[test]
    fn invalid_stream_content_is_an_upstream_payload_error() {
        // A genuinely wrong JSON type is the upstream's bug, not the client's.
        for content in [json!(42), json!({"text": "nope"})] {
            let data = json!({
                "id": "chatcmpl-1",
                "model": "m",
                "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]
            })
            .to_string();
            let err = OpenAiStreamParser::new("m")
                .push(&SseFrame { event: None, data })
                .unwrap_err();
            assert!(
                matches!(err, Error::BadUpstreamPayload(_)),
                "content {content} must be an upstream payload error, got {err:?}"
            );
        }
    }

    #[test]
    fn tool_strict_flags_round_trip_through_openai() {
        let original = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"type": "function", "function": {"name": "exact", "strict": true,
                 "parameters": {"type": "object"}}},
                {"type": "function", "function": {"name": "loose", "strict": false,
                 "parameters": {"type": "object"}}},
                {"type": "function", "function": {"name": "silent",
                 "parameters": {"type": "object"}}}
            ]
        });
        let req = decode_request(original).unwrap();
        assert_eq!(req.tool_strict.get("exact"), Some(&true));
        assert_eq!(req.tool_strict.get("loose"), Some(&false));
        assert_eq!(req.tool_strict.get("silent"), None);

        let body = encode_request(&req, "up").unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(
            tools[0]["function"]["strict"],
            json!(true),
            "an explicit strict request must survive: {body}"
        );
        assert_eq!(tools[1]["function"]["strict"], json!(false));
        assert!(
            tools[2]["function"].get("strict").is_none(),
            "an absent flag must stay absent: {body}"
        );

        let again = decode_request(body).unwrap();
        assert_eq!(again.tool_strict, req.tool_strict);

        let err = decode_request(json!({
            "model": "m",
            "messages": [],
            "tools": [{"type": "function", "function": {"name": "f", "strict": "yes"}}]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("`strict` must be a boolean"));
    }

    #[test]
    fn parallel_tool_calls_round_trips() {
        let req = decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "q"}],
            "tools": [{"type": "function", "function": {"name": "f"}}],
            "parallel_tool_calls": false
        }))
        .unwrap();
        assert_eq!(req.parallel_tool_use, Some(false));
        let body = encode_request(&req, "up").unwrap();
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(decode_request(body).unwrap().parallel_tool_use, Some(false));

        let absent = decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "q"}]
        }))
        .unwrap();
        assert_eq!(absent.parallel_tool_use, None);
        assert!(encode_request(&absent, "up")
            .unwrap()
            .get("parallel_tool_calls")
            .is_none());

        let err = decode_request(json!({
            "model": "m",
            "messages": [],
            "parallel_tool_calls": "no"
        }))
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("`parallel_tool_calls` must be a boolean"));
    }

    #[test]
    fn anthropic_parallel_restriction_maps_to_openai() {
        let req = crate::protocol::anthropic::decode_request(json!({
            "model": "m",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "q"}],
            "tools": [{"name": "f", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "any", "disable_parallel_tool_use": true}
        }))
        .unwrap();
        let body = encode_request(&req, "up").unwrap();
        assert_eq!(body["tool_choice"], json!("required"));
        assert_eq!(
            body["parallel_tool_calls"],
            json!(false),
            "the Anthropic restriction must reach OpenAI: {body}"
        );

        let allowed = crate::protocol::anthropic::decode_request(json!({
            "model": "m",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "q"}],
            "tools": [{"name": "f", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto", "disable_parallel_tool_use": false}
        }))
        .unwrap();
        assert_eq!(
            encode_request(&allowed, "up").unwrap()["parallel_tool_calls"],
            json!(true)
        );
    }

    #[test]
    fn encoder_emits_valid_chunk_sequence() {
        let mut enc = OpenAiStreamEncoder::new("sonnet-class").with_usage(true);
        let mut wire = String::new();
        for ev in [
            StreamEvent::Start {
                id: "msg_1".into(),
                model: "sonnet-class".into(),
                usage: Usage {
                    input_tokens: 3,
                    ..Usage::default()
                },
            },
            StreamEvent::ThinkingDelta {
                index: 0,
                text: "think".into(),
            },
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::TextDelta {
                index: 1,
                text: "hi".into(),
            },
            StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 2,
                    ..Usage::default()
                },
            },
        ] {
            for f in enc.encode(&ev) {
                wire.push_str(&f.to_wire());
            }
        }
        assert!(enc.finish().is_empty());
        assert!(wire.ends_with("data: [DONE]\n\n"));
        assert!(wire.contains("\"reasoning_content\":\"think\""));
        assert!(wire.contains("\"finish_reason\":\"stop\""));
        assert!(wire.contains("\"prompt_tokens\":3"));
        // role, reasoning, text, finish, usage
        assert_eq!(wire.matches("chatcmpl-msg_1").count(), 5);

        // Feeding our own output back through the parser is lossless for text.
        let events = parse(&wire);
        assert!(events.contains(&StreamEvent::ThinkingDelta {
            index: 0,
            text: "think".into()
        }));
        assert!(events.contains(&StreamEvent::TextDelta {
            index: 1,
            text: "hi".into()
        }));
    }

    #[test]
    fn streamed_chunks_report_the_client_facing_model() {
        // The upstream names what it served ("deepseek-v4-flash"); the client
        // asked for an exposed id and must see that id, exactly as the
        // non-streaming encoder reports it.
        let mut enc = OpenAiStreamEncoder::new("deepseek-deepseek-v4-flash");
        let frames = enc.encode(&StreamEvent::Start {
            id: "chatcmpl-upstream".into(),
            model: "deepseek-v4-flash".into(),
            usage: Usage::default(),
        });
        let chunk: Value = serde_json::from_str(&frames[0].data).unwrap();
        assert_eq!(chunk["model"], "deepseek-deepseek-v4-flash");
    }

    #[test]
    fn encoder_maps_block_indices_to_tool_call_slots() {
        let mut enc = OpenAiStreamEncoder::new("m");
        let mut wire = String::new();
        for ev in [
            StreamEvent::ToolUseStart {
                index: 3,
                id: "a".into(),
                name: "fa".into(),
            },
            StreamEvent::ToolUseStart {
                index: 7,
                id: "b".into(),
                name: "fb".into(),
            },
            StreamEvent::ToolUseDelta {
                index: 7,
                partial_json: "{}".into(),
            },
        ] {
            for f in enc.encode(&ev) {
                wire.push_str(&f.to_wire());
            }
        }
        assert!(wire.contains("\"index\":0,\"id\":\"a\""));
        assert!(wire.contains("\"index\":1,\"id\":\"b\""));
        // the delta for block 7 reuses slot 1
        assert_eq!(wire.matches("\"index\":1").count(), 2);
    }

    #[test]
    fn encoder_without_usage_option_omits_the_trailer() {
        let mut enc = OpenAiStreamEncoder::new("m");
        let frames = enc.encode(&StreamEvent::Stop {
            stop_reason: StopReason::MaxTokens,
            stop_sequence: None,
            usage: Usage {
                output_tokens: 5,
                ..Usage::default()
            },
        });
        let wire: String = frames.iter().map(|f| f.to_wire()).collect();
        assert!(!wire.contains("prompt_tokens"));
        assert!(wire.contains("\"finish_reason\":\"length\""));
    }

    #[test]
    fn error_frames_terminate_the_stream() {
        let mut enc = OpenAiStreamEncoder::new("m");
        let frames = enc.error(&Error::Unauthorized);
        assert!(frames[0].data.contains("authentication_error"));
        assert_eq!(frames[1].data, "[DONE]");
    }

    #[test]
    fn assistant_reasoning_content_survives_the_ir_round_trip() {
        let req = decode_request(json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello", "reasoning_content": "thinking hard"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "ls", "arguments": "{}"}}
                ], "reasoning_content": "need to list"},
                {"role": "tool", "tool_call_id": "call_1", "content": "a.txt"},
            ]
        }))
        .unwrap();
        let body = encode_request(&req, "deepseek-v4-flash").unwrap();
        let msgs = body["messages"].as_array().unwrap();

        // Plain assistant text turn keeps its reasoning.
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["reasoning_content"], "thinking hard");
        assert_eq!(msgs[1]["content"], "hello");

        // Tool-call turn keeps both reasoning and the calls.
        assert_eq!(msgs[2]["reasoning_content"], "need to list");
        assert!(msgs[2]["tool_calls"].is_array());

        // Tool result round-trips with its call id.
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
    }

    #[test]
    fn anthropic_sourced_thinking_is_not_echoed_as_reasoning_content() {
        let mut req = decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        req.source_dialect = Dialect::Anthropic;
        req.messages.push(crate::ir::Message {
            role: crate::ir::Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    text: "chain of thought".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::text("answer"),
            ],
        });
        assert!(encode_request(&req, "m").is_err());
        req.unsupported_content_policy = UnsupportedContentPolicy::Placeholder;
        let body = encode_request(&req, "m").unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert!(msgs[1].get("reasoning_content").is_none());
        assert!(msgs[1]["content"].as_str().unwrap().contains("Unsupported"));
        assert!(msgs[1]["content"].as_str().unwrap().contains("answer"));
    }
}
