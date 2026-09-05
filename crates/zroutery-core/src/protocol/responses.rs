//! OpenAI Responses API (`/v1/responses`) dialect.
//!
//! This is the newer OpenAI format built around items and parts. The decoder
//! lifts Responses items into the canonical IR, the encoder renders IR content
//! back into a Responses response, and the stream parser/encoder handle the SSE
//! lifecycle.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::reasoning_bridge;
use super::{SseFrame, StreamEncoder, StreamParser};
use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, Message, Role, StopReason,
    StreamEvent, SystemPart, ThinkingConfig, ToolChoice, ToolDef, ToolResultPart, Usage,
};

// ---------------------------------------------------------------- request in

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
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    req.system.push(SystemPart::new(text));
                } else if let Some(text) = part.as_str() {
                    req.system.push(SystemPart::new(text));
                }
            }
        }
        _ => {}
    }

    if let Some(input) = obj.get("input").and_then(Value::as_array) {
        for item in input {
            decode_input_item(item, &mut req)?;
        }
    }

    req.max_tokens = obj
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);

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
        Some(Value::Object(o)) => {
            o.get("name")
                .and_then(Value::as_str)
                .map(|n| ToolChoice::Specific {
                    name: n.to_string(),
                })
        }
        _ => None,
    };

    if let Some(effort) = obj
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
    {
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

    Ok(req)
}

fn decode_input_item(item: &Value, req: &mut ChatRequest) -> Result<()> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    match item_type {
        "function_call" => {
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input =
                serde_json::from_str(arguments).unwrap_or_else(|_| json!({"__raw": arguments}));
            req.messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input,
                }],
            });
        }
        "function_call_output" => {
            req.messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: String::new(),
                    content: vec![ToolResultPart::Text {
                        text: item
                            .get("output")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    is_error: false,
                }],
            });
        }
        "input_text" => {
            req.messages.push(Message::user_text(
                item.get("text").and_then(Value::as_str).unwrap_or_default(),
            ));
        }
        "input_image" => {
            let url = item
                .get("image_url")
                .and_then(|i| i.get("url"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            req.messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Image {
                    source: MediaSource::from_url(url),
                }],
            });
        }
        "reasoning" => {
            let block = reasoning_bridge::decode_reasoning_item(item).or_else(|| {
                item.get("summary")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|s| s.get("text"))
                    .and_then(Value::as_str)
                    .map(|text| ContentBlock::Thinking {
                        text: text.to_string(),
                        signature: None,
                    })
            });
            if let Some(block) = block {
                req.messages.push(Message {
                    role: Role::Assistant,
                    content: vec![block],
                });
            }
        }
        "message" => {
            let role = if item.get("role").and_then(Value::as_str) == Some("assistant") {
                Role::Assistant
            } else {
                Role::User
            };
            let mut content = Vec::new();
            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") | Some("input_text") => {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                content.push(ContentBlock::text(text));
                            }
                        }
                        Some("input_image") => {
                            if let Some(url) = part
                                .get("image_url")
                                .and_then(|i| i.get("url"))
                                .and_then(Value::as_str)
                            {
                                content.push(ContentBlock::Image {
                                    source: MediaSource::from_url(url),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            req.messages.push(Message { role, content });
        }
        _ => {}
    }
    Ok(())
}

// --------------------------------------------------------------- request out

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        let mut parts: Vec<Value> = Vec::new();
        let mut separate_items: Vec<Value> = Vec::new();

        for b in &m.content {
            match b {
                ContentBlock::Text { text, .. } => parts.push(json!({
                    "type": "input_text",
                    "text": text,
                })),
                ContentBlock::Image { source } => parts.push(json!({
                    "type": "input_image",
                    "image_url": {"url": source.to_data_url()},
                })),
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: tool_input,
                } => separate_items.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": tool_input.to_string(),
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let text = content
                        .iter()
                        .map(|p| match p {
                            ToolResultPart::Text { text } => text.clone(),
                            ToolResultPart::Image { .. } => "[image]".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    separate_items.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": text,
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
                    }
                }
                ContentBlock::RedactedThinking { .. } => {
                    if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                        separate_items.push(item);
                    }
                }
                ContentBlock::Document { .. } => {}
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
    Ok(Value::Object(body))
}

// ---------------------------------------------------------------- responses

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if let Some(err) = body.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown upstream error");
        return Err(Error::BadUpstreamPayload(msg.to_string()));
    }
    let mut content = Vec::new();
    if let Some(output) = body.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some("output_text") = part.get("type").and_then(Value::as_str) {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    content.push(ContentBlock::text(text));
                                }
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    content.push(ContentBlock::ToolUse {
                        id: item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input: serde_json::from_str(arguments)
                            .unwrap_or_else(|_| json!({"__raw": arguments})),
                    });
                }
                Some("reasoning") => {
                    if let Some(block) = reasoning_bridge::decode_reasoning_item(item) {
                        content.push(block);
                    } else if let Some(summary) = item
                        .get("summary")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|s| s.get("text"))
                        .and_then(Value::as_str)
                    {
                        content.push(ContentBlock::Thinking {
                            text: summary.to_string(),
                            signature: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

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
        } else {
            StopReason::EndTurn
        },
        stop_sequence: None,
        usage: decode_usage(body.get("usage")),
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
                "arguments": input.to_string(),
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
            _ => {}
        }
    }
    let text = resp.text();
    json!({
        "id": resp.id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": if resp.stop_reason == StopReason::MaxTokens { "incomplete" } else { "completed" },
        "model": resp.model,
        "output": output,
        "output_text": text,
        "usage": encode_usage(&resp.usage),
    })
}

pub(crate) fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(u) = v.filter(|u| !u.is_null()) else {
        return Usage::default();
    };
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
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
    }
}

pub(crate) fn encode_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
        "total_tokens": u.total(),
        "input_tokens_details": {"cached_tokens": u.cache_read_tokens},
        "output_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
    })
}

// ---------------------------------------------------------------- stream in

pub struct ResponsesStreamParser {
    id: String,
    model: String,
    started: bool,
    stopped: bool,
    next_index: u32,
    text_index: Option<u32>,
    thinking_index: Option<u32>,
    tool_indices: HashMap<String, u32>,
    usage: Usage,
}

impl ResponsesStreamParser {
    pub fn new(model: &str) -> Self {
        Self {
            id: String::new(),
            model: model.to_string(),
            started: false,
            stopped: false,
            next_index: 0,
            text_index: None,
            thinking_index: None,
            tool_indices: HashMap::new(),
            usage: Usage::default(),
        }
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
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
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
                let item = event.get("item").cloned().unwrap_or_default();
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let index = self.next_index;
                    self.next_index += 1;
                    let item_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if !item_id.is_empty() {
                        self.tool_indices.insert(item_id.clone(), index);
                    }
                    if !call_id.is_empty() {
                        self.tool_indices.insert(call_id.clone(), index);
                    }
                    out.push(StreamEvent::ToolUseStart {
                        index,
                        id: call_id,
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
            }
            "response.content_part.added" => {
                let part = event.get("part").cloned().unwrap_or_default();
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        self.open_index("text");
                    }
                    Some("reasoning") => {
                        self.open_index("thinking");
                    }
                    _ => {}
                }
            }
            "response.content_part.done" => {
                let part = event.get("part").cloned().unwrap_or_default();
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => self.text_index = None,
                    Some("reasoning") => self.thinking_index = None,
                    _ => {}
                }
            }
            "response.output_item.done" => {
                self.text_index = None;
                self.thinking_index = None;
            }
            "response.output_text.delta" => {
                let index = self.open_index("text");
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    out.push(StreamEvent::TextDelta {
                        index,
                        text: delta.to_string(),
                    });
                }
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
                if let Some(index) = index {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        out.push(StreamEvent::ToolUseDelta {
                            index,
                            partial_json: delta.to_string(),
                        });
                    }
                }
            }
            "response.reasoning_summary_text.delta" => {
                let index = self.open_index("thinking");
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    out.push(StreamEvent::ThinkingDelta {
                        index,
                        text: delta.to_string(),
                    });
                }
            }
            "response.completed" => {
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
                self.stopped = true;
                out.push(StreamEvent::Stop {
                    stop_reason: StopReason::EndTurn,
                    stop_sequence: None,
                    usage: self.usage,
                });
            }
            _ => {}
        }
        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.started && !self.stopped {
            self.stopped = true;
            vec![StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: self.usage,
            }]
        } else {
            Vec::new()
        }
    }
}

// --------------------------------------------------------------- stream out

/// Tracks the kind of output item currently being streamed, so that
/// `output_index` is incremented on every distinct item boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputItemKind {
    Text,
    Thinking,
    Tool,
}

pub struct ResponsesStreamEncoder {
    id: String,
    model: String,
    created_at: i64,
    done: bool,
    output_index: u32,
    content_index: u32,
    current_kind: Option<OutputItemKind>,
    tool_item_ids: HashMap<u32, String>,
}

impl ResponsesStreamEncoder {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("resp-{}", uuid::Uuid::new_v4().simple()),
            model: model.to_string(),
            created_at: chrono::Utc::now().timestamp(),
            done: false,
            output_index: 0,
            content_index: 0,
            current_kind: None,
            tool_item_ids: HashMap::new(),
        }
    }

    fn frame(&self, event_type: &str, data: Value) -> SseFrame {
        SseFrame {
            event: Some(event_type.to_string()),
            data: data.to_string(),
        }
    }
}

impl StreamEncoder for ResponsesStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        let mut out = Vec::new();
        match event {
            StreamEvent::Start { id, model, .. } => {
                if !id.is_empty() {
                    self.id = id.clone();
                }
                if !model.is_empty() {
                    self.model = model.clone();
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
                if self.current_kind != Some(OutputItemKind::Text) {
                    self.output_index += 1;
                    self.content_index = 0;
                    self.current_kind = Some(OutputItemKind::Text);
                }
                out.push(self.frame(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": format!("msg_{}", self.output_index),
                        "output_index": self.output_index,
                        "content_index": self.content_index,
                        "delta": text,
                    }),
                ));
            }
            StreamEvent::ThinkingDelta { text, .. } => {
                if self.current_kind != Some(OutputItemKind::Thinking) {
                    self.output_index += 1;
                    self.content_index = 0;
                    self.current_kind = Some(OutputItemKind::Thinking);
                }
                out.push(self.frame(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "type": "response.reasoning_summary_text.delta",
                        "item_id": format!("rs_{}", self.output_index),
                        "output_index": self.output_index,
                        "content_index": self.content_index,
                        "delta": text,
                    }),
                ));
            }
            StreamEvent::ToolUseStart {
                index, id, name, ..
            } => {
                let item_id = format!("fc_{id}");
                self.tool_item_ids.insert(*index, item_id.clone());
                out.push(self.frame(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": self.output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": "",
                            "status": "in_progress",
                        }
                    }),
                ));
                self.output_index += 1;
                self.current_kind = Some(OutputItemKind::Tool);
            }
            StreamEvent::ToolUseDelta {
                partial_json,
                index,
                ..
            } => {
                let item_id = self
                    .tool_item_ids
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("fc_{index}"));
                out.push(self.frame(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": item_id,
                        "output_index": self.output_index.saturating_sub(1),
                        "content_index": 0,
                        "delta": partial_json,
                    }),
                ));
            }
            StreamEvent::Stop { usage, .. } => {
                out.push(self.frame(
                    "response.completed",
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "completed",
                            "model": self.model,
                            "output": [],
                            "usage": encode_usage(usage),
                        }
                    }),
                ));
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
        vec![self.frame(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": self.id,
                    "object": "response",
                    "created_at": self.created_at,
                    "status": "completed",
                    "model": self.model,
                    "output": [],
                }
            }),
        )]
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        let mut out = vec![self.frame(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": {
                    "id": self.id,
                    "object": "response",
                    "status": "failed",
                    "model": self.model,
                    "error": err.to_wire(Dialect::OpenAIResponses),
                }
            }),
        )];
        if !self.done {
            self.done = true;
            out.push(self.frame(
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": self.id,
                        "object": "response",
                        "status": "failed",
                        "model": self.model,
                        "output": [],
                    }
                }),
            ));
        }
        out
    }
}
