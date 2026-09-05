//! OpenAI Chat Completions dialect, also used for every "OpenAI compatible"
//! provider such as DeepSeek, Groq, Ollama, vLLM and OpenRouter.
//!
//! The tricky direction is OpenAI -> canonical: chunks carry no block
//! structure, so [`OpenAiStreamParser`] synthesises Anthropic style block
//! indices with a small state machine.

use serde_json::{json, Map, Value};

use super::apply_content_policy;
use super::reasoning_bridge;
use super::ProviderQuirks;
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
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                if let Some(text) = flatten_content(m.get("content")) {
                    req.system.push(SystemPart::new(text));
                }
            }
            "tool" | "function" => {
                let tool_use_id = m
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let block = ContentBlock::ToolResult {
                    tool_use_id,
                    name: String::new(),
                    content: vec![ToolResultPart::Text {
                        text: flatten_content(m.get("content")).unwrap_or_default(),
                    }],
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
                if let Some(reasoning) = m
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    content.push(ContentBlock::Thinking {
                        text: reasoning.to_string(),
                        signature: None,
                    });
                } else if let Some(reasoning) = m
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    content.push(ContentBlock::Thinking {
                        text: reasoning.to_string(),
                        signature: None,
                    });
                }
                // Responses-style reasoning items can carry an encrypted
                // Anthropic thinking block through the reasoning bridge.
                if let Some(items) = m.get("reasoning").and_then(Value::as_array) {
                    for item in items {
                        if let Some(block) = reasoning_bridge::decode_reasoning_item(item) {
                            content.push(block);
                        }
                    }
                }
                if let Some(text) = flatten_content(m.get("content")) {
                    if !text.is_empty() {
                        content.push(ContentBlock::text(text));
                    }
                }
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for c in calls {
                        let f = c.get("function");
                        content.push(ContentBlock::ToolUse {
                            id: c
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: f
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            input: parse_arguments(
                                f.and_then(|f| f.get("arguments")).and_then(Value::as_str),
                            ),
                        });
                    }
                }
                req.messages.push(Message {
                    role: Role::Assistant,
                    content,
                });
            }
            _ => {
                let content = decode_user_content(m.get("content"))?;
                req.messages.push(Message {
                    role: Role::User,
                    content,
                });
            }
        }
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
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };

    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        for t in tools {
            let f = t.get("function").unwrap_or(t);
            let Some(name) = f.get("name").and_then(Value::as_str) else {
                tracing::warn!("skipping tool definition without a `name` field");
                continue;
            };
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

    req.tool_choice = match obj.get("tool_choice") {
        Some(Value::String(s)) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" | "any" => Some(ToolChoice::Any),
            _ => None,
        },
        Some(Value::Object(o)) => o
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .map(|n| ToolChoice::Specific {
                name: n.to_string(),
            }),
        _ => None,
    };

    // Reasoning effort is the OpenAI knob; translate to a thinking budget so
    // Anthropic upstreams get something meaningful.
    if let Some(effort) = obj.get("reasoning_effort").and_then(Value::as_str) {
        req.thinking = Some(match effort {
            "none" | "minimal" => ThinkingConfig {
                enabled: false,
                budget_tokens: None,
            },
            "low" => ThinkingConfig {
                enabled: true,
                budget_tokens: Some(1024),
            },
            "high" => ThinkingConfig {
                enabled: true,
                budget_tokens: Some(16384),
            },
            _ => ThinkingConfig {
                enabled: true,
                budget_tokens: Some(4096),
            },
        });
    }

    req.metadata_user = obj.get("user").and_then(Value::as_str).map(str::to_string);

    for (k, v) in obj {
        if !KNOWN_KEYS.contains(&k.as_str()) {
            req.passthrough.insert(k.clone(), v.clone());
        }
    }

    Ok(req)
}

/// True when the client asked for a usage chunk at the end of the stream.
pub fn wants_stream_usage(body: &Value) -> bool {
    body.get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn flatten_content(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(parts)) => {
            let joined: String = parts
                .iter()
                .filter_map(|p| match p {
                    Value::String(s) => Some(s.clone()),
                    _ => p.get("text").and_then(Value::as_str).map(str::to_string),
                })
                .collect::<Vec<_>>()
                .join("");
            Some(joined)
        }
        _ => None,
    }
}

fn decode_user_content(v: Option<&Value>) -> Result<Vec<ContentBlock>> {
    match v {
        Some(Value::String(s)) => Ok(vec![ContentBlock::text(s.clone())]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") | None => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            out.push(ContentBlock::text(t));
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = p
                            .get("image_url")
                            .and_then(|i| i.get("url"))
                            .and_then(Value::as_str)
                        {
                            out.push(ContentBlock::Image {
                                source: MediaSource::from_url(url),
                            });
                        }
                    }
                    Some("input_audio") => {
                        if let Some(audio) = p.get("input_audio") {
                            let format = audio
                                .get("format")
                                .and_then(Value::as_str)
                                .unwrap_or("wav");
                            let data = audio
                                .get("data")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            out.push(ContentBlock::Audio {
                                source: MediaSource::Base64 {
                                    media_type: format!("audio/{format}"),
                                    data: data.to_string(),
                                },
                                media_type: format!("audio/{format}"),
                            });
                        }
                    }
                    Some("file") => {
                        // OpenAI `file` type is not yet standard; drop it.
                    }
                    _ => {}
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
        encode_message_into(m, &mut messages, req.source_dialect == Dialect::OpenAI, req.unsupported_content_policy)?;
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
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description.clone().unwrap_or_default(),
                                "parameters": t.input_schema,
                            }
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
                ToolChoice::Specific { name } => {
                    json!({"type": "function", "function": {"name": name}})
                }
            },
        );
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
                    // Thinking blocks are echoed only when the client itself sent
                    // reasoning (OpenAI source): some gateways require history
                    // turns to carry `reasoning_content` back. For other sources
                    // (e.g. Anthropic) it is dropped — most providers reject it.
                    ContentBlock::Thinking { text: t, signature } if echo_reasoning => {
                        if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                            reasoning_items.push(item);
                        } else {
                            reasoning.push_str(t);
                        }
                        let _ = signature;
                    }
                    ContentBlock::RedactedThinking { .. } if echo_reasoning => {
                        if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                            reasoning_items.push(item);
                        }
                    }
                    // Audio is not representable in assistant messages.
                    ContentBlock::Audio { .. }
                    | ContentBlock::File { .. }
                    | ContentBlock::Video { .. }
                    | ContentBlock::Citation { .. }
                    | ContentBlock::Annotation { .. } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                text.push_str(t);
                            }
                        }
                    }
                    _ => {}
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
                        let text: String = content
                            .iter()
                            .map(|p| match p {
                                ToolResultPart::Text { text } => text.clone(),
                                ToolResultPart::Image { .. } => "[image]".to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": text,
                        }));
                    }
                    ContentBlock::Text { text, .. } => {
                        parts.push(json!({"type": "text", "text": text}))
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
                            MediaSource::Url { .. } => {
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
                    ContentBlock::File { .. }
                    | ContentBlock::Video { .. }
                    | ContentBlock::Citation { .. }
                    | ContentBlock::Annotation { .. } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"type": "text", "text": t}));
                            }
                        }
                    }
                    _ => {}
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
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| Error::BadUpstreamPayload("response has no choices".into()))?;
    let msg = choice
        .get("message")
        .ok_or_else(|| Error::BadUpstreamPayload("choice has no message".into()))?;

    let mut content = Vec::new();
    if let Some(reasoning) = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(ContentBlock::Thinking {
            text: reasoning.to_string(),
            signature: None,
        });
    }
    if let Some(text) = flatten_content(msg.get("content")).filter(|t| !t.is_empty()) {
        content.push(ContentBlock::text(text));
    }
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for c in calls {
            let f = c.get("function");
            content.push(ContentBlock::ToolUse {
                id: c
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple())),
                name: f
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: parse_arguments(f.and_then(|f| f.get("arguments")).and_then(Value::as_str)),
            });
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
            _ => {}
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum Block {
    Text,
    Thinking,
    Tool,
}

/// Rebuilds Anthropic style block structure from OpenAI chunks.
pub struct OpenAiStreamParser {
    model: String,
    id: Option<String>,
    started: bool,
    next_index: u32,
    current: Option<(u32, Block)>,
    /// OpenAI `tool_calls[].index` -> canonical block index.
    tool_slots: Vec<(u64, u32)>,
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

    /// Always start a fresh block. Each tool call is its own block, even though
    /// consecutive tool calls share the same `kind`.
    fn open_block(&mut self, kind: Block, out: &mut Vec<StreamEvent>) -> u32 {
        self.close_current(out);
        let index = self.next_index;
        self.next_index += 1;
        self.current = Some((index, kind));
        index
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
                continue;
            }
            let delta = choice.get("delta").or_else(|| choice.get("message"));

            if let Some(reasoning) = delta
                .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let index = self.block_for(Block::Thinking, &mut out);
                out.push(StreamEvent::ThinkingDelta {
                    index,
                    text: reasoning.to_string(),
                });
            }

            if let Some(text) = delta
                .and_then(|d| d.get("content"))
                .and_then(|c| match c {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(_) => flatten_content(Some(c)),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
            {
                let index = self.block_for(Block::Text, &mut out);
                out.push(StreamEvent::TextDelta { index, text });
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
                            let i = self.open_block(Block::Tool, &mut out);
                            self.tool_slots.push((slot, i));
                            out.push(StreamEvent::ToolUseStart {
                                index: i,
                                id: call
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .unwrap_or_else(|| {
                                        format!("call_{}", uuid::Uuid::new_v4().simple())
                                    }),
                                name: call
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
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
            StreamEvent::Start { id, model, usage } => {
                self.id = if id.starts_with("chatcmpl") {
                    id.clone()
                } else {
                    format!("chatcmpl-{id}")
                };
                self.model = model.clone();
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
        assert_eq!(events[4], StreamEvent::BlockStop { index: 0 });
        assert_eq!(
            events[5],
            StreamEvent::ToolUseStart {
                index: 1,
                id: "call_2".into(),
                name: "other".into()
            }
        );
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
        let body = encode_request(&req, "m").unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert!(msgs[1].get("reasoning_content").is_none());
        assert_eq!(msgs[1]["content"], "answer");
    }
}
