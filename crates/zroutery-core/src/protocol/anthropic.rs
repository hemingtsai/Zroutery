//! Anthropic Messages API dialect: `/v1/messages` requests, responses and the
//! stateful SSE event stream.

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, Message, Role, StopReason,
    StreamEvent, SystemPart, ThinkingConfig, ToolChoice, ToolDef, ToolResultPart,
    UnsupportedContentPolicy, Usage,
};

use super::apply_content_policy;
use super::{SseFrame, StreamEncoder, StreamParser};

/// Anthropic requires `max_tokens`; used when the client omits it.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

const KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "system",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
    "tools",
    "tool_choice",
    "thinking",
    "metadata",
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

    let mut req = ChatRequest::new(model, Dialect::Anthropic);

    req.system = match obj.get("system") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![SystemPart::new(s.clone())],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|b| {
                Some(SystemPart {
                    text: b.get("text").and_then(Value::as_str)?.to_string(),
                    // A breakpoint on a system block is the caller's decision about
                    // money, so it travels with the text.
                    cache_control: b.get("cache_control").cloned(),
                })
            })
            .collect(),
        Some(_) => return Err(Error::invalid("`system` must be a string or an array")),
    };

    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("`messages` is required"))?;
    for m in messages {
        req.messages.push(decode_message(m)?);
    }

    req.max_tokens = obj
        .get("max_tokens")
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.temperature = obj.get("temperature").and_then(Value::as_f64);
    req.top_p = obj.get("top_p").and_then(Value::as_f64);
    req.top_k = obj
        .get("top_k")
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if let Some(stops) = obj.get("stop_sequences").and_then(Value::as_array) {
        req.stop_sequences = stops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }

    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        for t in tools {
            let Some(name) = t.get("name").and_then(Value::as_str) else {
                tracing::warn!("skipping tool definition without a `name` field");
                continue;
            };
            req.tools.push(ToolDef {
                name: name.to_string(),
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: t
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
                cache_control: t.get("cache_control").cloned(),
            });
        }
    }

    req.tool_choice =
        obj.get("tool_choice")
            .and_then(|tc| match tc.get("type").and_then(Value::as_str)? {
                "auto" => Some(ToolChoice::Auto),
                "any" => Some(ToolChoice::Any),
                "none" => Some(ToolChoice::None),
                "tool" => tc
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|n| ToolChoice::Specific {
                        name: n.to_string(),
                    }),
                _ => None,
            });

    if let Some(th) = obj.get("thinking") {
        let enabled = th.get("type").and_then(Value::as_str) == Some("enabled");
        req.thinking = Some(ThinkingConfig {
            enabled,
            budget_tokens: th
                .get("budget_tokens")
                .and_then(Value::as_u64)
                .map(|v| v.min(u32::MAX as u64) as u32),
        });
    }

    req.metadata_user = obj
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    for (k, v) in obj {
        if !KNOWN_KEYS.contains(&k.as_str()) {
            req.passthrough.insert(k.clone(), v.clone());
        }
    }

    Ok(req)
}

fn decode_message(m: &Value) -> Result<Message> {
    let role = match m.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some(other) => {
            return Err(Error::invalid(format!(
                "unsupported message role `{other}`"
            )))
        }
        None => return Err(Error::invalid("message is missing `role`")),
    };
    let content = match m.get("content") {
        Some(Value::String(s)) => vec![ContentBlock::text(s.clone())],
        Some(Value::Array(blocks)) => {
            let mut out = Vec::with_capacity(blocks.len());
            for b in blocks {
                if let Some(block) = decode_block(b)? {
                    out.push(block);
                }
            }
            out
        }
        Some(Value::Null) | None => Vec::new(),
        Some(_) => return Err(Error::invalid("`content` must be a string or an array")),
    };
    Ok(Message { role, content })
}

fn decode_block(b: &Value) -> Result<Option<ContentBlock>> {
    let kind = b
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("content block is missing `type`"))?;
    let block = match kind {
        "text" => ContentBlock::Text {
            text: b
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            cache_control: b.get("cache_control").cloned(),
        },
        "thinking" => ContentBlock::Thinking {
            text: b
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            signature: b
                .get("signature")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "redacted_thinking" => ContentBlock::RedactedThinking {
            data: b
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "image" => ContentBlock::Image {
            source: decode_source(b.get("source"))?,
        },
        "document" => ContentBlock::Document {
            source: decode_source(b.get("source"))?,
        },
        "tool_use" => ContentBlock::ToolUse {
            id: b
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            name: b
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_use block is missing `name`"))?
                .to_string(),
            input: b.get("input").cloned().unwrap_or_else(|| json!({})),
        },
        "tool_result" => {
            let tool_use_id = b
                .get("tool_use_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_result block is missing `tool_use_id`"))?
                .to_string();
            let content = match b.get("content") {
                Some(Value::String(s)) => vec![ToolResultPart::Text { text: s.clone() }],
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                        Some("text") => Some(ToolResultPart::Text {
                            text: p
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        }),
                        Some("image") => decode_source(p.get("source"))
                            .ok()
                            .map(|source| ToolResultPart::Image { source }),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            ContentBlock::ToolResult {
                tool_use_id,
                name: String::new(),
                content,
                is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }
        }
        // Unknown block types (e.g. server side tools) are dropped rather than
        // failing the whole request.
        _ => return Ok(None),
    };
    Ok(Some(block))
}

fn decode_source(source: Option<&Value>) -> Result<MediaSource> {
    let s = source.ok_or_else(|| Error::invalid("media block is missing `source`"))?;
    match s.get("type").and_then(Value::as_str) {
        Some("base64") => Ok(MediaSource::Base64 {
            media_type: s
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            data: s
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        Some("url") => Ok(MediaSource::Url {
            url: s
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        _ => Err(Error::invalid("unsupported media source type")),
    }
}

// --------------------------------------------------------------- request out

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));
    body.insert(
        "max_tokens".into(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );

    if !req.system.is_empty() {
        body.insert(
            "system".into(),
            Value::Array(
                req.system
                    .iter()
                    .map(|part| {
                        with_cache(
                            json!({"type": "text", "text": part.text}),
                            &part.cache_control,
                        )
                    })
                    .collect(),
            ),
        );
    }

    let messages: Result<Vec<Value>> = req
        .messages
        .iter()
        .map(|m| encode_message(m, req.unsupported_content_policy))
        .collect();
    body.insert("messages".into(), Value::Array(messages?));

    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.top_p {
        body.insert("top_p".into(), json!(p));
    }
    if let Some(k) = req.top_k {
        body.insert("top_k".into(), json!(k));
    }
    if !req.stop_sequences.is_empty() {
        body.insert("stop_sequences".into(), json!(req.stop_sequences));
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
                        let mut m = Map::new();
                        m.insert("name".into(), json!(t.name));
                        if let Some(d) = &t.description {
                            m.insert("description".into(), json!(d));
                        }
                        m.insert("input_schema".into(), t.input_schema.clone());
                        with_cache(Value::Object(m), &t.cache_control)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(tc) = &req.tool_choice {
        body.insert(
            "tool_choice".into(),
            match tc {
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::Any => json!({"type": "any"}),
                ToolChoice::None => json!({"type": "none"}),
                ToolChoice::Specific { name } => json!({"type": "tool", "name": name}),
            },
        );
    }
    if let Some(th) = &req.thinking {
        if th.enabled {
            let mut m = Map::new();
            m.insert("type".into(), json!("enabled"));
            m.insert(
                "budget_tokens".into(),
                json!(th.budget_tokens.unwrap_or(2048)),
            );
            body.insert("thinking".into(), Value::Object(m));
        }
    }
    if let Some(u) = &req.metadata_user {
        body.insert("metadata".into(), json!({"user_id": u}));
    }

    // Vendor extensions are only forwarded within the same dialect.
    if req.source_dialect == Dialect::Anthropic {
        for (k, v) in &req.passthrough {
            body.entry(k.clone()).or_insert(v.clone());
        }
    }

    Ok(Value::Object(body))
}

fn encode_message(m: &Message, policy: UnsupportedContentPolicy) -> Result<Value> {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut content = Vec::new();
    for b in &m.content {
        match b {
            // Types not natively supported by Anthropic: apply the policy.
            ContentBlock::File { .. }
            | ContentBlock::Audio { .. }
            | ContentBlock::Video { .. }
            | ContentBlock::Citation { .. }
            | ContentBlock::Annotation { .. } => {
                if let Some(replacement) = apply_content_policy(policy, b)? {
                    content.push(encode_block(&replacement));
                }
            }
            _ => content.push(encode_block(b)),
        }
    }
    Ok(json!({
        "role": role,
        "content": Value::Array(content),
    }))
}

pub(crate) fn encode_block(b: &ContentBlock) -> Value {
    match b {
        ContentBlock::Text {
            text,
            cache_control,
        } => with_cache(json!({"type": "text", "text": text}), cache_control),
        ContentBlock::Thinking { text, signature } => {
            let mut m = Map::new();
            m.insert("type".into(), json!("thinking"));
            m.insert("thinking".into(), json!(text));
            m.insert(
                "signature".into(),
                json!(signature.clone().unwrap_or_default()),
            );
            Value::Object(m)
        }
        ContentBlock::RedactedThinking { data } => {
            json!({"type": "redacted_thinking", "data": data})
        }
        ContentBlock::Image { source } => json!({"type": "image", "source": encode_source(source)}),
        ContentBlock::Document { source } => {
            json!({"type": "document", "source": encode_source(source)})
        }
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => {
            let parts: Vec<Value> = content
                .iter()
                .map(|p| match p {
                    ToolResultPart::Text { text } => json!({"type": "text", "text": text}),
                    ToolResultPart::Image { source } => {
                        json!({"type": "image", "source": encode_source(source)})
                    }
                })
                .collect();
            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": parts,
                "is_error": is_error,
            })
        }
        // New IR variants not yet mapped to Anthropic wire format.
        _ => json!(null),
    }
}

/// Put a `cache_control` marker back on an encoded block.
///
/// A prompt cache breakpoint is the caller's decision about money, so it travels
/// through the proxy untouched rather than being re-derived or quietly dropped.
fn with_cache(mut value: Value, cache_control: &Option<Value>) -> Value {
    if let (Some(object), Some(marker)) = (value.as_object_mut(), cache_control) {
        object.insert("cache_control".into(), marker.clone());
    }
    value
}

fn encode_source(s: &MediaSource) -> Value {
    match s {
        MediaSource::Base64 { media_type, data } => {
            json!({"type": "base64", "media_type": media_type, "data": data})
        }
        MediaSource::Url { url } => json!({"type": "url", "url": url}),
        MediaSource::Reference { id } => json!({"type": "url", "url": id}),
    }
}

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

pub(crate) fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(u) = v else { return Usage::default() };
    Usage {
        input_tokens: u
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        output_tokens: u
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_read_tokens: u
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_write_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        reasoning_tokens: 0,
    }
}

pub(crate) fn encode_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
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
        _ => Vec::new(),
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

// ---------------------------------------------------------------- stream in

/// Parses an Anthropic SSE stream into canonical events.
pub struct AnthropicStreamParser {
    model: String,
    usage: Usage,
    started: bool,
    open: Vec<u32>,
    stopped: bool,
}

impl AnthropicStreamParser {
    pub fn new(model: &str) -> Self {
        AnthropicStreamParser {
            model: model.to_string(),
            usage: Usage::default(),
            started: false,
            open: Vec::new(),
            stopped: false,
        }
    }
}

impl StreamParser for AnthropicStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        if frame.data.trim() == "[DONE]" {
            return Ok(Vec::new());
        }
        let v = frame.json()?;
        let kind = frame
            .event
            .as_deref()
            .or_else(|| v.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        let index = v
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32;

        let events = match kind {
            "message_start" => {
                let msg = v.get("message");
                self.usage = decode_usage(msg.and_then(|m| m.get("usage")));
                self.started = true;
                vec![StreamEvent::Start {
                    id: msg
                        .and_then(|m| m.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("msg_stream")
                        .to_string(),
                    model: msg
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .unwrap_or(&self.model)
                        .to_string(),
                    usage: self.usage,
                }]
            }
            "content_block_start" => {
                let block = v.get("content_block");
                self.open.push(index);
                match block.and_then(|b| b.get("type")).and_then(Value::as_str) {
                    Some("tool_use") => vec![StreamEvent::ToolUseStart {
                        index,
                        id: block
                            .and_then(|b| b.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: block
                            .and_then(|b| b.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    Some("redacted_thinking") => vec![StreamEvent::RedactedThinking {
                        index,
                        data: block
                            .and_then(|b| b.get("data"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    Some("thinking") => {
                        let t = block
                            .and_then(|b| b.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if t.is_empty() {
                            Vec::new()
                        } else {
                            vec![StreamEvent::ThinkingDelta {
                                index,
                                text: t.to_string(),
                            }]
                        }
                    }
                    _ => {
                        let t = block
                            .and_then(|b| b.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if t.is_empty() {
                            Vec::new()
                        } else {
                            vec![StreamEvent::TextDelta {
                                index,
                                text: t.to_string(),
                            }]
                        }
                    }
                }
            }
            "content_block_delta" => {
                let delta = v.get("delta");
                match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => vec![StreamEvent::TextDelta {
                        index,
                        text: delta
                            .and_then(|d| d.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    Some("thinking_delta") => vec![StreamEvent::ThinkingDelta {
                        index,
                        text: delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    Some("signature_delta") => vec![StreamEvent::ThinkingSignature {
                        index,
                        signature: delta
                            .and_then(|d| d.get("signature"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    Some("input_json_delta") => vec![StreamEvent::ToolUseDelta {
                        index,
                        partial_json: delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                self.open.retain(|i| *i != index);
                vec![StreamEvent::BlockStop { index }]
            }
            "message_delta" => {
                let delta = v.get("delta");
                if let Some(u) = v.get("usage") {
                    let partial = decode_usage(Some(u));
                    // message_delta only carries output side counters.
                    self.usage.output_tokens = partial.output_tokens;
                    if partial.input_tokens > 0 {
                        self.usage.input_tokens = partial.input_tokens;
                    }
                }
                self.stopped = true;
                vec![StreamEvent::Stop {
                    stop_reason: delta
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(Value::as_str)
                        .map(stop_reason_from_str)
                        .unwrap_or(StopReason::Unknown),
                    stop_sequence: delta
                        .and_then(|d| d.get("stop_sequence"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    usage: self.usage,
                }]
            }
            "message_stop" => Vec::new(),
            "ping" => vec![StreamEvent::Ping],
            "error" => {
                let msg = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream stream error");
                return Err(Error::BadUpstreamPayload(msg.to_string()));
            }
            _ => Vec::new(),
        };
        Ok(events)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for index in std::mem::take(&mut self.open) {
            out.push(StreamEvent::BlockStop { index });
        }
        if self.started && !self.stopped {
            out.push(StreamEvent::Stop {
                stop_reason: StopReason::Unknown,
                stop_sequence: None,
                usage: self.usage,
            });
            self.stopped = true;
        }
        out
    }
}

// --------------------------------------------------------------- stream out

#[derive(Debug, Clone, Copy, PartialEq)]
enum OpenKind {
    Text,
    Thinking,
    ToolUse,
}

/// Renders canonical events as an Anthropic SSE stream, opening and closing
/// content blocks as needed.
pub struct AnthropicStreamEncoder {
    model: String,
    open: Vec<(u32, OpenKind)>,
    started: bool,
    usage: Usage,
    id: String,
    finished: bool,
}

impl AnthropicStreamEncoder {
    pub fn new(model: &str) -> Self {
        AnthropicStreamEncoder {
            model: model.to_string(),
            open: Vec::new(),
            started: false,
            usage: Usage::default(),
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            finished: false,
        }
    }

    fn frame(event: &str, data: Value) -> SseFrame {
        SseFrame {
            event: Some(event.to_string()),
            data: data.to_string(),
        }
    }

    fn ensure_started(&mut self, out: &mut Vec<SseFrame>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(Self::frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": encode_usage(&self.usage),
                }
            }),
        ));
    }

    /// Open `index` as `kind`, closing it first if it was open as another kind.
    fn ensure_block(&mut self, out: &mut Vec<SseFrame>, index: u32, kind: OpenKind, start: Value) {
        self.ensure_started(out);
        if let Some(pos) = self.open.iter().position(|(i, _)| *i == index) {
            if self.open[pos].1 == kind {
                return;
            }
            out.push(Self::frame(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
            self.open.remove(pos);
        }
        self.open.push((index, kind));
        out.push(Self::frame("content_block_start", start));
    }
}

impl StreamEncoder for AnthropicStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        let mut out = Vec::new();
        match event {
            StreamEvent::Start { id, model, usage } => {
                self.id = id.clone();
                self.model = model.clone();
                self.usage = *usage;
                self.ensure_started(&mut out);
            }
            StreamEvent::TextDelta { index, text } => {
                self.ensure_block(
                    &mut out,
                    *index,
                    OpenKind::Text,
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "text", "text": ""}
                    }),
                );
                out.push(Self::frame(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "text_delta", "text": text}
                    }),
                ));
            }
            StreamEvent::ThinkingDelta { index, text } => {
                self.ensure_block(
                    &mut out,
                    *index,
                    OpenKind::Thinking,
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "thinking", "thinking": "", "signature": ""}
                    }),
                );
                out.push(Self::frame(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "thinking_delta", "thinking": text}
                    }),
                ));
            }
            StreamEvent::ThinkingSignature { index, signature } => {
                self.ensure_block(
                    &mut out,
                    *index,
                    OpenKind::Thinking,
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "thinking", "thinking": "", "signature": ""}
                    }),
                );
                out.push(Self::frame(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "signature_delta", "signature": signature}
                    }),
                ));
            }
            StreamEvent::RedactedThinking { index, data } => {
                self.ensure_started(&mut out);
                out.push(Self::frame(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "redacted_thinking", "data": data}
                    }),
                ));
                out.push(Self::frame(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": index}),
                ));
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.ensure_block(
                    &mut out,
                    *index,
                    OpenKind::ToolUse,
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
                    }),
                );
            }
            StreamEvent::ToolUseDelta {
                index,
                partial_json,
            } => {
                self.ensure_started(&mut out);
                out.push(Self::frame(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": partial_json}
                    }),
                ));
            }
            StreamEvent::BlockStop { index } => {
                if let Some(pos) = self.open.iter().position(|(i, _)| i == index) {
                    self.open.remove(pos);
                    out.push(Self::frame(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": index}),
                    ));
                }
            }
            StreamEvent::Stop {
                stop_reason,
                stop_sequence,
                usage,
            } => {
                self.ensure_started(&mut out);
                for (index, _) in std::mem::take(&mut self.open) {
                    out.push(Self::frame(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": index}),
                    ));
                }
                self.usage = *usage;
                out.push(Self::frame(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": stop_reason_to_str(*stop_reason),
                            "stop_sequence": stop_sequence,
                        },
                        "usage": {"output_tokens": usage.output_tokens},
                    }),
                ));
                out.push(Self::frame("message_stop", json!({"type": "message_stop"})));
                self.finished = true;
            }
            StreamEvent::Ping => {
                out.push(Self::frame("ping", json!({"type": "ping"})));
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        for (index, _) in std::mem::take(&mut self.open) {
            out.push(Self::frame(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
        }
        out.push(Self::frame(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": Value::Null},
                "usage": {"output_tokens": self.usage.output_tokens},
            }),
        ));
        out.push(Self::frame("message_stop", json!({"type": "message_stop"})));
        self.finished = true;
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        vec![Self::frame("error", err.to_wire(Dialect::Anthropic))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SseDecoder;

    fn frames_to_events(raw: &str, model: &str) -> Vec<StreamEvent> {
        let mut dec = SseDecoder::new();
        let mut parser = AnthropicStreamParser::new(model);
        let mut events = Vec::new();
        for f in dec.push(raw.as_bytes()) {
            events.extend(parser.push(&f).unwrap());
        }
        events.extend(parser.finish());
        events
    }

    #[test]
    fn decodes_string_and_block_content() {
        let req = decode_request(json!({
            "model": "sonnet-class",
            "max_tokens": 100,
            "system": "be brief",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
            ]
        }))
        .unwrap();
        assert_eq!(req.model, "sonnet-class");
        assert_eq!(req.system, vec![SystemPart::new("be brief")]);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].content[0], ContentBlock::text("hi"));
        assert_eq!(req.max_tokens, Some(100));
        assert!(!req.stream);
    }

    #[test]
    fn decodes_system_blocks_tools_and_thinking() {
        let req = decode_request(json!({
            "model": "m",
            "system": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{"name": "get_weather", "description": "d", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "get_weather"},
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "metadata": {"user_id": "u1"},
            "top_k": 40,
            "stop_sequences": ["STOP"],
            "beta_flag": true
        }))
        .unwrap();
        assert_eq!(req.system, vec![SystemPart::new("a"), SystemPart::new("b")]);
        assert_eq!(req.tools[0].name, "get_weather");
        assert_eq!(
            req.tool_choice,
            Some(ToolChoice::Specific {
                name: "get_weather".into()
            })
        );
        assert_eq!(req.thinking.unwrap().budget_tokens, Some(1024));
        assert_eq!(req.metadata_user.as_deref(), Some("u1"));
        assert_eq!(req.top_k, Some(40));
        assert_eq!(req.stop_sequences, vec!["STOP"]);
        assert_eq!(req.passthrough.get("beta_flag"), Some(&json!(true)));
    }

    #[test]
    fn decodes_tool_use_and_tool_result_round_trip() {
        let original = json!({
            "model": "m",
            "max_tokens": 50,
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                    {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SH"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": "sunny", "is_error": false}
                ]}
            ]
        });
        let req = decode_request(original).unwrap();
        let encoded = encode_request(&req, "upstream-model").unwrap();
        assert_eq!(encoded["model"], "upstream-model");
        assert_eq!(encoded["messages"][1]["content"][1]["name"], "get_weather");
        assert_eq!(encoded["messages"][1]["content"][0]["signature"], "sig");
        assert_eq!(
            encoded["messages"][2]["content"][0]["content"][0]["text"],
            "sunny"
        );
        // Re-decoding the encoded form yields the same IR.
        let again = decode_request(encoded).unwrap();
        assert_eq!(again.messages, req.messages);
    }

    #[test]
    fn encode_request_fills_required_max_tokens() {
        let req = decode_request(json!({"model": "m", "messages": []})).unwrap();
        let body = encode_request(&req, "m").unwrap();
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn passthrough_is_not_leaked_across_dialects() {
        let mut req = ChatRequest::new("m", Dialect::OpenAI);
        req.passthrough
            .insert("frequency_penalty".into(), json!(0.5));
        let body = encode_request(&req, "m").unwrap();
        assert!(body.get("frequency_penalty").is_none());
    }

    #[test]
    fn response_round_trip() {
        let body = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "stop_sequence": Value::Null,
            "usage": {"input_tokens": 10, "output_tokens": 3, "cache_read_input_tokens": 2}
        });
        let resp = decode_response(body).unwrap();
        assert_eq!(resp.text(), "hi");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.cache_read_tokens, 2);
        let back = encode_response(&resp);
        assert_eq!(back["content"][0]["text"], "hi");
        assert_eq!(back["stop_reason"], "end_turn");
        assert_eq!(back["usage"]["input_tokens"], 10);
    }

    #[test]
    fn error_response_becomes_error() {
        let err = decode_response(json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "too busy"}
        }))
        .unwrap_err();
        assert!(err.to_string().contains("too busy"));
    }

    #[test]
    fn parses_a_full_text_stream() {
        let raw = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_9\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"He\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"llo\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let events = frames_to_events(raw, "fallback");
        assert_eq!(
            events[0],
            StreamEvent::Start {
                id: "msg_9".into(),
                model: "claude-x".into(),
                usage: Usage {
                    input_tokens: 7,
                    ..Usage::default()
                }
            }
        );
        assert!(events.contains(&StreamEvent::Ping));
        assert!(events.contains(&StreamEvent::TextDelta {
            index: 0,
            text: "He".into()
        }));
        match events.last().unwrap() {
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                assert_eq!(*stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 5);
            }
            other => panic!("unexpected tail {other:?}"),
        }
    }

    #[test]
    fn parses_thinking_and_tool_use_stream() {
        let raw = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"c\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"fn\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\n",
        );
        let events = frames_to_events(raw, "c");
        assert!(events.contains(&StreamEvent::ThinkingDelta {
            index: 0,
            text: "plan".into()
        }));
        assert!(events.contains(&StreamEvent::ThinkingSignature {
            index: 0,
            signature: "sig".into()
        }));
        assert!(events.contains(&StreamEvent::ToolUseStart {
            index: 1,
            id: "tu".into(),
            name: "fn".into()
        }));
        assert!(events.contains(&StreamEvent::ToolUseDelta {
            index: 1,
            partial_json: "{\"a\":".into()
        }));
        assert!(matches!(
            events.last().unwrap(),
            StreamEvent::Stop {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[test]
    fn parser_closes_dangling_blocks_on_truncated_stream() {
        let raw = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"c\"}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
        );
        let events = frames_to_events(raw, "c");
        assert_eq!(
            events[events.len() - 2],
            StreamEvent::BlockStop { index: 0 }
        );
        assert!(matches!(
            events.last().unwrap(),
            StreamEvent::Stop {
                stop_reason: StopReason::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn stream_error_frame_surfaces_as_error() {
        let mut parser = AnthropicStreamParser::new("m");
        let frame = SseFrame {
            event: Some("error".into()),
            data: r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#.into(),
        };
        assert!(parser
            .push(&frame)
            .unwrap_err()
            .to_string()
            .contains("busy"));
    }

    #[test]
    fn encoder_emits_block_lifecycle() {
        let mut enc = AnthropicStreamEncoder::new("m");
        let mut wire = String::new();
        for f in enc.encode(&StreamEvent::Start {
            id: "msg_1".into(),
            model: "gpt".into(),
            usage: Usage {
                input_tokens: 4,
                ..Usage::default()
            },
        }) {
            wire.push_str(&f.to_wire());
        }
        for f in enc.encode(&StreamEvent::TextDelta {
            index: 0,
            text: "hi".into(),
        }) {
            wire.push_str(&f.to_wire());
        }
        for f in enc.encode(&StreamEvent::TextDelta {
            index: 0,
            text: "!".into(),
        }) {
            wire.push_str(&f.to_wire());
        }
        for f in enc.encode(&StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage {
                input_tokens: 4,
                output_tokens: 2,
                ..Usage::default()
            },
        }) {
            wire.push_str(&f.to_wire());
        }
        assert!(
            enc.finish().is_empty(),
            "stop already terminated the stream"
        );

        // Exactly one content_block_start for two deltas, and a clean shutdown.
        assert_eq!(wire.matches("event: content_block_start").count(), 1);
        assert_eq!(wire.matches("event: content_block_delta").count(), 2);
        assert_eq!(wire.matches("event: content_block_stop").count(), 1);
        assert!(wire.contains("event: message_start"));
        assert!(wire.contains("event: message_stop"));

        // And it re-parses into the same canonical events.
        let events = frames_to_events(&wire, "m");
        assert_eq!(
            events[0],
            StreamEvent::Start {
                id: "msg_1".into(),
                model: "gpt".into(),
                usage: Usage {
                    input_tokens: 4,
                    ..Usage::default()
                }
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::TextDelta {
                index: 0,
                text: "hi".into()
            }
        );
        assert_eq!(
            events[2],
            StreamEvent::TextDelta {
                index: 0,
                text: "!".into()
            }
        );
        assert_eq!(events[3], StreamEvent::BlockStop { index: 0 });
    }

    #[test]
    fn encoder_switches_block_kind_and_finishes_unterminated_streams() {
        let mut enc = AnthropicStreamEncoder::new("m");
        let mut wire = String::new();
        for ev in [
            StreamEvent::ThinkingDelta {
                index: 0,
                text: "think".into(),
            },
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::TextDelta {
                index: 1,
                text: "answer".into(),
            },
        ] {
            for f in enc.encode(&ev) {
                wire.push_str(&f.to_wire());
            }
        }
        for f in enc.finish() {
            wire.push_str(&f.to_wire());
        }
        assert!(wire.contains("\"type\":\"thinking\""));
        assert!(wire.contains("thinking_delta"));
        assert_eq!(wire.matches("event: content_block_stop").count(), 2);
        assert!(wire.contains("event: message_stop"));
        let events = frames_to_events(&wire, "m");
        assert!(events.contains(&StreamEvent::ThinkingDelta {
            index: 0,
            text: "think".into()
        }));
        assert!(events.contains(&StreamEvent::TextDelta {
            index: 1,
            text: "answer".into()
        }));
    }

    #[test]
    fn encoder_reports_errors_in_band() {
        let mut enc = AnthropicStreamEncoder::new("m");
        let frames = enc.error(&Error::Timeout(30));
        assert_eq!(frames[0].event.as_deref(), Some("error"));
        let v = frames[0].json().unwrap();
        assert_eq!(v["error"]["type"], "timeout_error");
    }
}
