//! Google Gemini native API dialect (`generateContent`).
//!
//! This module provides a practical subset of the Gemini translation: system
//! instructions, multi-part contents, function declarations/calls, image parts,
//! and a snapshot-style SSE parser/encoder.

use serde_json::{json, Map, Value};

use super::apply_content_policy;
use super::{SseFrame, StreamEncoder, StreamParser};
use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, Message, Role, StopReason,
    StreamEvent, SystemPart, ToolChoice, ToolDef, ToolResultPart, Usage,
};

// ---------------------------------------------------------------- request in

pub fn decode_request(body: Value) -> Result<ChatRequest> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::invalid("Gemini request body must be a JSON object"))?;
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("model is required"))?
        .to_string();
    let mut req = ChatRequest::new(model, Dialect::Gemini);

    if let Some(parts) = obj
        .get("system_instruction")
        .and_then(|s| s.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                req.system.push(SystemPart::new(text));
            }
        }
    }

    if let Some(contents) = obj.get("contents").and_then(Value::as_array) {
        for content in contents {
            let role = if content.get("role").and_then(Value::as_str) == Some("model") {
                Role::Assistant
            } else {
                Role::User
            };
            let mut blocks = Vec::new();
            if let Some(parts) = content.get("parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        blocks.push(ContentBlock::text(text));
                    } else if let Some(call) = part.get("functionCall") {
                        blocks.push(ContentBlock::ToolUse {
                            id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: call
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            input: call.get("args").cloned().unwrap_or_else(|| json!({})),
                        });
                    } else if let Some(response) = part.get("functionResponse") {
                        blocks.push(ContentBlock::ToolResult {
                            tool_use_id: response
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: response
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            content: vec![ToolResultPart::Text {
                                text: response.get("response").unwrap_or(&Value::Null).to_string(),
                            }],
                            is_error: false,
                        });
                    } else if let Some(data) = part.get("inlineData") {
                        blocks.push(ContentBlock::Image {
                            source: MediaSource::Base64 {
                                media_type: data
                                    .get("mimeType")
                                    .and_then(Value::as_str)
                                    .unwrap_or("application/octet-stream")
                                    .to_string(),
                                data: data
                                    .get("data")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            },
                        });
                    }
                }
            }
            req.messages.push(Message {
                role,
                content: blocks,
            });
        }
    }

    if let Some(gc) = obj.get("generationConfig") {
        req.max_tokens = gc
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32);
        req.temperature = gc.get("temperature").and_then(Value::as_f64);
        req.top_p = gc.get("topP").and_then(Value::as_f64);
        req.stop_sequences = match gc.get("stopSequences").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            None => Vec::new(),
        };
    }

    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if let Some(decls) = tool.get("functionDeclarations").and_then(Value::as_array) {
                for decl in decls {
                    let Some(name) = decl.get("name").and_then(Value::as_str) else {
                        tracing::warn!("skipping tool definition without a `name` field");
                        continue;
                    };
                    req.tools.push(ToolDef {
                        name: name.to_string(),
                        description: decl
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        input_schema: decl
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                        cache_control: None,
                    });
                }
            }
        }
    }

    req.tool_choice = obj
        .get("toolConfig")
        .and_then(|c| c.get("functionCallingConfig"))
        .and_then(|c| c.get("mode"))
        .and_then(Value::as_str)
        .map(|mode| match mode {
            "NONE" => ToolChoice::None,
            "ANY" => ToolChoice::Any,
            _ => ToolChoice::Auto,
        });

    Ok(req)
}

// --------------------------------------------------------------- request out

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));

    if !req.system.is_empty() {
        body.insert(
            "system_instruction".into(),
            json!({"parts": req.system.iter().map(|s| json!({"text": s.text})).collect::<Vec<_>>()}),
        );
    }

    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        let mut parts: Vec<Value> = Vec::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text, .. } => parts.push(json!({"text": text})),
                ContentBlock::Image { source } => match source {
                    MediaSource::Base64 { media_type, data } => parts.push(json!({
                        "inlineData": {"mimeType": media_type, "data": data}
                    })),
                    MediaSource::Url { url } => parts.push(json!({"text": format!(
                        "[Image URL: {url} -- URL images not supported by Gemini API, use base64 encoding instead]"
                    )})),
                },
                ContentBlock::ToolUse { id, name, input } => parts.push(json!({
                    "functionCall": {"id": id, "name": name, "args": input}
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    name,
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
                    parts.push(json!({
                        "functionResponse": {"id": tool_use_id, "name": name, "response": {"text": text}}
                    }));
                }
                ContentBlock::Thinking { .. }
                | ContentBlock::RedactedThinking { .. }
                | ContentBlock::Document { .. } => {}
                // Gemini supports inlineData for any MIME type.
                ContentBlock::File {
                    source, media_type, ..
                }
                | ContentBlock::Audio {
                    source, media_type, ..
                }
                | ContentBlock::Video {
                    source, media_type, ..
                } => match source {
                    MediaSource::Base64 { data, .. } => parts.push(json!({
                        "inlineData": {"mimeType": media_type, "data": data}
                    })),
                    MediaSource::Url { url } => {
                        let kind = if matches!(b, ContentBlock::File { .. }) {
                            "File"
                        } else if matches!(b, ContentBlock::Audio { .. }) {
                            "Audio"
                        } else {
                            "Video"
                        };
                        parts.push(json!({"text": format!(
                            "[{kind} URL: {url} -- URL media not supported by Gemini API, use base64 encoding instead]"
                        )}))
                    }
                },
                // Citation and Annotation have no Gemini equivalent.
                ContentBlock::Citation { .. } | ContentBlock::Annotation { .. } => {
                    if let Some(replacement) =
                        apply_content_policy(req.unsupported_content_policy, b)?
                    {
                        if let Some(t) = replacement.as_text() {
                            parts.push(json!({"text": t}));
                        }
                    }
                }
            }
        }
        contents.push(json!({"role": role, "parts": parts}));
    }
    body.insert("contents".into(), Value::Array(contents));

    let mut gc = Map::new();
    if let Some(mt) = req.max_tokens {
        gc.insert("maxOutputTokens".into(), json!(mt));
    }
    if let Some(t) = req.temperature {
        gc.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.top_p {
        gc.insert("topP".into(), json!(p));
    }
    if !req.stop_sequences.is_empty() {
        gc.insert("stopSequences".into(), json!(req.stop_sequences));
    }
    if !gc.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gc));
    }

    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            json!([{
                "functionDeclarations": req.tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description.clone().unwrap_or_default(),
                    "parameters": t.input_schema,
                })).collect::<Vec<_>>()
            }]),
        );
    }

    if let Some(tc) = &req.tool_choice {
        let tool_config = match tc {
            ToolChoice::Auto => json!({"functionCallingConfig": {"mode": "AUTO"}}),
            ToolChoice::None => json!({"functionCallingConfig": {"mode": "NONE"}}),
            ToolChoice::Any => json!({"functionCallingConfig": {"mode": "ANY"}}),
            ToolChoice::Specific { name } => json!({
                "functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]}
            }),
        };
        body.insert("toolConfig".into(), tool_config);
    }

    Ok(Value::Object(body))
}

// ---------------------------------------------------------------- responses

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if let Some(err) = body.get("error") {
        return Err(Error::BadUpstreamPayload(
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown Gemini error")
                .to_string(),
        ));
    }
    let candidate = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first());
    let mut content = Vec::new();
    let mut stop_reason = StopReason::Unknown;
    if let Some(candidate) = candidate {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    content.push(ContentBlock::text(text));
                } else if let Some(call) = part.get("functionCall") {
                    content.push(ContentBlock::ToolUse {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: call
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    });
                }
            }
        }
        stop_reason = match candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "STOP" => StopReason::EndTurn,
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" => StopReason::Refusal,
            _ => StopReason::Unknown,
        };
    }
    let usage = body
        .get("usageMetadata")
        .map(|u| Usage {
            input_tokens: u
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32,
            output_tokens: u
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32,
            reasoning_tokens: u
                .get("thoughtsTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32,
            ..Usage::default()
        })
        .unwrap_or_default();

    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("gemini_unknown")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content,
        stop_reason,
        stop_sequence: None,
        usage,
        passthrough: Map::new(),
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    let parts: Vec<Value> = resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(json!({"text": text})),
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "functionCall": {"id": id, "name": name, "args": input}
            })),
            _ => None,
        })
        .collect();
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": match resp.stop_reason {
                StopReason::MaxTokens => "MAX_TOKENS",
                StopReason::Refusal => "SAFETY",
                _ => "STOP",
            },
        }],
        "usageMetadata": {
            "promptTokenCount": resp.usage.input_tokens,
            "candidatesTokenCount": resp.usage.output_tokens,
            "totalTokenCount": resp.usage.total(),
            "thoughtsTokenCount": resp.usage.reasoning_tokens,
        },
    })
}

// ---------------------------------------------------------------- stream in

pub struct GeminiStreamParser {
    model: String,
    started: bool,
    stopped: bool,
    next_index: u32,
    text_index: Option<u32>,
    open_blocks: Vec<u32>,
    usage: Usage,
}

impl GeminiStreamParser {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            started: false,
            stopped: false,
            next_index: 0,
            text_index: None,
            open_blocks: Vec::new(),
            usage: Usage::default(),
        }
    }
}

impl StreamParser for GeminiStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|e| Error::BadUpstreamPayload(format!("bad Gemini SSE: {e}: {data}")))?;
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(StreamEvent::Start {
                id: format!("gemini-{}", uuid::Uuid::new_v4().simple()),
                model: self.model.clone(),
                usage: self.usage,
            });
        }
        if let Some(usage) = value.get("usageMetadata") {
            self.usage = Usage {
                input_tokens: usage
                    .get("promptTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
                output_tokens: usage
                    .get("candidatesTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
                reasoning_tokens: usage
                    .get("thoughtsTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
                ..Usage::default()
            };
        }

        if let Some(candidate) = value
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        let index = match self.text_index {
                            Some(i) => i,
                            None => {
                                let i = self.next_index;
                                self.next_index += 1;
                                self.text_index = Some(i);
                                self.open_blocks.push(i);
                                i
                            }
                        };
                        out.push(StreamEvent::TextDelta {
                            index,
                            text: text.to_string(),
                        });
                    } else if let Some(call) = part.get("functionCall") {
                        // Close the open text block if any.
                        if let Some(text_idx) = self.text_index.take() {
                            self.open_blocks.retain(|i| *i != text_idx);
                            out.push(StreamEvent::BlockStop { index: text_idx });
                        }
                        let index = self.next_index;
                        self.next_index += 1;
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: call
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        });
                        if let Some(args) = call.get("args") {
                            out.push(StreamEvent::ToolUseDelta {
                                index,
                                partial_json: args.to_string(),
                            });
                        }
                        // Gemini sends functionCall as a complete block, so close it.
                        out.push(StreamEvent::BlockStop { index });
                    }
                }
            }
            if let Some(finish_reason) = candidate.get("finishReason").and_then(Value::as_str) {
                self.stopped = true;
                out.push(StreamEvent::Stop {
                    stop_reason: match finish_reason {
                        "MAX_TOKENS" => StopReason::MaxTokens,
                        "SAFETY" => StopReason::Refusal,
                        _ => StopReason::EndTurn,
                    },
                    stop_sequence: None,
                    usage: self.usage,
                });
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.started && !self.stopped {
            self.stopped = true;
            let mut out = Vec::new();
            // Emit BlockStop for any remaining open blocks.
            if let Some(text_idx) = self.text_index.take() {
                self.open_blocks.retain(|i| *i != text_idx);
                out.push(StreamEvent::BlockStop { index: text_idx });
            }
            for index in std::mem::take(&mut self.open_blocks) {
                out.push(StreamEvent::BlockStop { index });
            }
            out.push(StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: self.usage,
            });
            out
        } else {
            Vec::new()
        }
    }
}

// --------------------------------------------------------------- stream out

pub struct GeminiStreamEncoder {
    model: String,
    done: bool,
    /// Buffered tool calls awaiting completion: (index, id, name, accumulated args).
    tool_buffers: Vec<(u32, String, String, String)>,
}

impl GeminiStreamEncoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            done: false,
            tool_buffers: Vec::new(),
        }
    }

    /// Flush all buffered tool calls as complete functionCall frames.
    fn flush_tools(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for (_, id, name, args) in self.tool_buffers.drain(..) {
            let parsed_args: Value = if args.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&args).unwrap_or(Value::String(args))
            };
            out.push(SseFrame {
                event: None,
                data: json!({
                    "candidates": [{"content": {"role": "model", "parts": [{
                        "functionCall": {"id": id, "name": name, "args": parsed_args}
                    }]}}]
                })
                .to_string(),
            });
        }
        out
    }
}

impl StreamEncoder for GeminiStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        match event {
            StreamEvent::Start { model, .. } => {
                self.model = model.clone();
                Vec::new()
            }
            StreamEvent::TextDelta { text, .. } => {
                // Flush any buffered tool calls before emitting text.
                let mut out = self.flush_tools();
                out.push(SseFrame {
                    event: None,
                    data: json!({
                        "candidates": [{"content": {"role": "model", "parts": [{"text": text}]}}]
                    })
                    .to_string(),
                });
                out
            }
            StreamEvent::ToolUseStart { index, id, name, .. } => {
                // Flush any previously buffered tool calls.
                let out = self.flush_tools();
                self.tool_buffers.push((*index, id.clone(), name.clone(), String::new()));
                out
            }
            StreamEvent::ToolUseDelta {
                partial_json,
                index,
                ..
            } => {
                if let Some(buf) = self.tool_buffers.iter_mut().find(|(i, _, _, _)| *i == *index) {
                    buf.3.push_str(partial_json);
                }
                Vec::new()
            }
            StreamEvent::BlockStop { index, .. } => {
                // Flush the tool buffer for this index if it exists.
                let mut out = Vec::new();
                if let Some(pos) = self.tool_buffers.iter().position(|(i, _, _, _)| *i == *index) {
                    let (_, id, name, args) = self.tool_buffers.remove(pos);
                    let parsed_args: Value = if args.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&args).unwrap_or(Value::String(args))
                    };
                    out.push(SseFrame {
                        event: None,
                        data: json!({
                            "candidates": [{"content": {"role": "model", "parts": [{
                                "functionCall": {"id": id, "name": name, "args": parsed_args}
                            }]}}]
                        })
                        .to_string(),
                    });
                }
                out
            }
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                let mut out = self.flush_tools();
                self.done = true;
                let finish_reason = match stop_reason {
                    StopReason::MaxTokens => "MAX_TOKENS",
                    StopReason::Refusal => "SAFETY",
                    _ => "STOP",
                };
                out.push(SseFrame {
                    event: None,
                    data: json!({
                        "candidates": [{
                            "content": {"role": "model", "parts": []},
                            "finishReason": finish_reason
                        }],
                        "usageMetadata": {
                            "promptTokenCount": usage.input_tokens,
                            "candidatesTokenCount": usage.output_tokens,
                            "totalTokenCount": usage.total(),
                            "thoughtsTokenCount": usage.reasoning_tokens,
                        }
                    })
                    .to_string(),
                });
                out
            }
            _ => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.done {
            return Vec::new();
        }
        self.done = true;
        let mut out = self.flush_tools();
        out.push(SseFrame {
            event: None,
            data: json!({
                "candidates": [{"content": {"role": "model", "parts": []}, "finishReason": "STOP"}]
            })
            .to_string(),
        });
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        self.done = true;
        vec![SseFrame {
            event: None,
            data: json!({
                "error": err.to_wire(Dialect::Gemini)
            })
            .to_string(),
        }]
    }
}
