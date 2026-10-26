//! Wire protocol translation.
//!
//! Two decoders turn incoming payloads into the [`crate::ir`] types, two
//! encoders turn the IR into upstream payloads, and each dialect knows how to
//! serialise responses and streaming events back to a client.
//!
//! # Where this module is tested
//!
//! Most of it is exercised from `tests/` rather than from inline `#[cfg(test)]`
//! blocks, and a file here with none is not an untested file:
//!
//! | Behaviour | Test |
//! |---|---|
//! | golden payloads in both dialects | `protocol_golden_test.rs` |
//! | Gemini request and response shapes | `gemini_test.rs` |
//! | media acceptance and rejection | `protocol_media_gate_test.rs` |
//! | streaming events and terminal states | `pipeline_lifecycle_test.rs` |
//!
//! The names describe the behaviour rather than the module, which is why the
//! mapping is written down: a coverage report that counts inline modules calls
//! a well-tested decoder untested and invites a duplicate harness.

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod reasoning_bridge;
pub mod responses;

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, StopReason, StreamEvent,
    UnsupportedContentPolicy, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The explicit result of evaluating an unsupported-content policy.
///
/// `Transform` is intentionally not represented as a successful result here:
/// a transform must provide a concrete replacement before it can be encoded.
/// `Drop` is represented as a distinct decision, but the fail-closed encoder
/// refuses to apply it until a caller can attach auditable evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ContentPolicyOutcome {
    /// Encode this explicit replacement in place of the original block.
    Replacement(ContentBlock),
    /// The caller explicitly selected a drop.  The encoder must not silently
    /// omit the block; it must reject or record the decision first.
    Drop,
}

/// Make a fail-closed, non-sensitive error for an unknown protocol content
/// type.  Only a short, sanitized type token is included in the client error;
/// the original payload is never copied into it.
pub(crate) fn unsupported_content(context: &str, kind: Option<&str>) -> Error {
    let kind = kind
        .map(sanitize_type)
        .unwrap_or_else(|| "unknown".to_string());
    Error::invalid(format!("unsupported {context} content type `{kind}`"))
}

/// Fail-closed error for an unknown item received from an upstream response.
/// The payload and its type string are intentionally not echoed.
pub(crate) fn unsupported_upstream_content(context: &str) -> Error {
    Error::BadUpstreamPayload(format!("unsupported {context} content"))
}

fn sanitize_type(kind: &str) -> String {
    let mut out = String::new();
    for ch in kind.chars().take(64) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// Whether a dialect can carry a media source on its wire.
///
/// This is a property of the surface, not of the model: a model that can see
/// may still be served by a dialect whose body has no place for a server-side
/// file id. Routing and the preflight use this to decide whether an image can
/// travel natively or has to be replaced, so "the target cannot represent
/// this" is never reported as a malformed client request.
pub fn carries_media_source(dialect: Dialect, source: &MediaSource) -> bool {
    match dialect {
        // Chat Completions takes a data URL or a plain URL, and Anthropic
        // takes inline/base64 or a URL source; neither has a field for a
        // provider-side file id.
        Dialect::OpenAI | Dialect::Anthropic => !matches!(source, MediaSource::Reference { .. }),
        // Responses has `file_id` beside `image_url`.
        Dialect::OpenAIResponses => true,
        // Gemini's inlineData requires the bytes themselves; a URL or a file
        // id would have to be fetched first, which is not this layer's job.
        Dialect::Gemini => matches!(source, MediaSource::Base64 { .. }),
    }
}

/// Evaluate an unsupported-content policy without collapsing distinct actions.
///
/// A successful transform must carry an explicit replacement.  The current
/// protocol layer has no safe implicit transformer, so `Transform` fails
/// closed.  `Drop` is returned as an explicit outcome; callers that are about
/// to encode must use [`apply_content_policy`], which rejects that outcome
/// rather than silently losing the block.
pub fn evaluate_content_policy(
    policy: UnsupportedContentPolicy,
    block: &ContentBlock,
) -> Result<ContentPolicyOutcome> {
    match policy {
        UnsupportedContentPolicy::Reject => {
            let label = content_label(block);
            Err(Error::invalid(format!(
                "unsupported content type `{label}` for the target provider"
            )))
        }
        UnsupportedContentPolicy::Placeholder => Ok(ContentPolicyOutcome::Replacement(
            explicit_placeholder(block),
        )),
        UnsupportedContentPolicy::Transform => Err(Error::invalid(
            "content transform requires an explicit replacement",
        )),
        UnsupportedContentPolicy::Drop => Ok(ContentPolicyOutcome::Drop),
    }
}

/// Apply an unsupported-content policy at an encoding boundary.
///
/// Only an explicit replacement can be applied.  In particular, `Drop` is not
/// converted into `None`: a caller must either record an auditable decision or
/// receive an explicit error.
pub fn apply_content_policy(
    policy: UnsupportedContentPolicy,
    block: &ContentBlock,
) -> Result<Option<ContentBlock>> {
    match evaluate_content_policy(policy, block)? {
        ContentPolicyOutcome::Replacement(replacement) => Ok(Some(replacement)),
        ContentPolicyOutcome::Drop => Err(Error::invalid(
            "content was explicitly selected for drop but no auditable replacement or drop evidence was supplied",
        )),
    }
}

/// Normalize an audio MIME type to the format string OpenAI expects.
///
/// `audio/mpeg` and `audio/mp3` both become `"mp3"`, `audio/x-wav` becomes
/// `"wav"`, and anything else is passed through after stripping the `audio/`
/// prefix. A bare string without a prefix defaults to `"wav"`.
pub(crate) fn normalize_audio_format(media_type: &str) -> &str {
    match media_type.strip_prefix("audio/") {
        Some("mpeg") | Some("mp3") => "mp3",
        Some("wav") | Some("x-wav") => "wav",
        Some(other) => other,
        None => "wav",
    }
}

/// The explicit replacement used when a response encoder cannot represent a
/// block and has no request policy context.  It is deliberately visible text,
/// never a missing/null block.
pub(crate) fn explicit_placeholder(block: &ContentBlock) -> ContentBlock {
    ContentBlock::text(format!("[Unsupported: {}]", content_label(block)))
}

/// Human-readable, non-sensitive label for a content block, used in
/// placeholder text and error messages.
fn content_label(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { .. } => "text".into(),
        ContentBlock::Image {
            source: MediaSource::Reference { .. },
        } => "image (server-side file id)".into(),
        ContentBlock::Image { .. } => "image".into(),
        ContentBlock::Document { .. } => "document".into(),
        ContentBlock::File { media_type, .. } => {
            let category = sanitize_type(media_type.split('/').next().unwrap_or("file"));
            format!("file ({category})")
        }
        ContentBlock::Audio { media_type, .. } => {
            let category = sanitize_type(media_type.split('/').next().unwrap_or("audio"));
            format!("audio ({category})")
        }
        ContentBlock::Video { media_type, .. } => {
            let category = sanitize_type(media_type.split('/').next().unwrap_or("video"));
            format!("video ({category})")
        }
        ContentBlock::ToolUse { .. } => "tool use".into(),
        ContentBlock::ToolResult { .. } => "tool result".into(),
        ContentBlock::Thinking { .. } => "thinking".into(),
        ContentBlock::RedactedThinking { .. } => "redacted thinking".into(),
        ContentBlock::Citation { .. } => "citation".into(),
        ContentBlock::Annotation { .. } => "annotation".into(),
    }
}

/// Returns `true`; the default for the quirks that are on unless disabled.
fn yes() -> bool {
    true
}

/// Per provider deviations from the reference dialect.
///
/// These exist because "OpenAI compatible" is a spectrum: reasoning models
/// reject `max_tokens` and `temperature`, some gateways choke on
/// `stream_options`, and only a few accept `reasoning_effort`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderQuirks {
    /// Send `max_completion_tokens` instead of `max_tokens`.
    #[serde(default)]
    pub use_max_completion_tokens: bool,
    #[serde(default)]
    pub drop_temperature: bool,
    #[serde(default)]
    pub drop_top_p: bool,
    #[serde(default)]
    pub drop_stop: bool,
    /// Ask for a usage trailer on streaming responses.
    #[serde(default = "yes")]
    pub stream_usage: bool,
    /// Use `role: "developer"` for the system prompt.
    #[serde(default)]
    pub system_as_developer: bool,
    /// Translate thinking budgets into `reasoning_effort`.
    #[serde(default)]
    pub send_reasoning_effort: bool,
    /// Make Responses history portable across upstreams.
    ///
    /// Off by default because it trades reasoning continuity for portability:
    /// the upstream no longer sees the reasoning items and item ids that an
    /// earlier upstream issued. Turn it on for a relay that may serve one
    /// conversation through several accounts or resources (AgentRouter, CC
    /// Switch, an aggregate endpoint), where forwarding those references fails
    /// with "the requested item was created under a different resource". See
    /// `protocol::responses::sanitize`.
    ///
    /// Only the Responses dialect has anything to strip, so this is inert for
    /// every other dialect.
    #[serde(default)]
    pub portable_history: bool,
}

impl Default for ProviderQuirks {
    fn default() -> Self {
        ProviderQuirks {
            use_max_completion_tokens: false,
            drop_temperature: false,
            drop_top_p: false,
            drop_stop: false,
            stream_usage: true,
            system_as_developer: false,
            send_reasoning_effort: false,
            portable_history: false,
        }
    }
}

/// Decode an inbound request body of the given dialect into the IR.
pub fn decode_request(dialect: Dialect, body: Value) -> Result<ChatRequest> {
    let mut request = match dialect {
        Dialect::Anthropic => anthropic::decode_request(body),
        Dialect::OpenAI => openai::decode_request(body),
        Dialect::OpenAIResponses => responses::decode_request(body),
        Dialect::Gemini => gemini::decode_request(body),
    }?;
    request.refresh_required_capabilities();
    Ok(request)
}

/// Encode the IR into an upstream request body for the given dialect.
///
/// `quirks` only affect the OpenAI dialect; the Anthropic API has no comparable
/// variation between implementations — with one exception, and that one is
/// dated: the 2026-10-21 revision removes the sampling parameters and confines
/// documents to user messages, so the Anthropic arm carries the version whose
/// behaviour the request must obey. The other arms ignore it.
pub fn encode_request(
    dialect: Dialect,
    req: &ChatRequest,
    upstream_model: &str,
    quirks: &ProviderQuirks,
    anthropic_version: anthropic::AnthropicApiVersion,
) -> Result<Value> {
    match dialect {
        Dialect::Anthropic => anthropic::encode_request_for(req, upstream_model, anthropic_version),
        Dialect::OpenAI => openai::encode_request_with(req, upstream_model, quirks),
        Dialect::OpenAIResponses => {
            responses::encode_request_with(req, upstream_model, quirks.portable_history)
        }
        Dialect::Gemini => gemini::encode_request(req, upstream_model),
    }
}

/// Decode a non streaming upstream response body into the IR.
pub fn decode_response(dialect: Dialect, body: Value) -> Result<ChatResponse> {
    match dialect {
        Dialect::Anthropic => anthropic::decode_response(body),
        Dialect::OpenAI => openai::decode_response(body),
        Dialect::OpenAIResponses => responses::decode_response(body),
        Dialect::Gemini => gemini::decode_response(body),
    }
}

/// Encode an IR response for a client speaking `dialect`.
pub fn encode_response(dialect: Dialect, resp: &ChatResponse) -> Value {
    match dialect {
        Dialect::Anthropic => anthropic::encode_response(resp),
        Dialect::OpenAI => openai::encode_response(resp),
        Dialect::OpenAIResponses => responses::encode_response(resp),
        Dialect::Gemini => gemini::encode_response(resp),
    }
}

/// One `event:`/`data:` block from an SSE stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

impl SseFrame {
    /// Serialise back to the wire, including the trailing blank line.
    pub fn to_wire(&self) -> String {
        match &self.event {
            Some(e) => format!("event: {e}\ndata: {}\n\n", self.data),
            None => format!("data: {}\n\n", self.data),
        }
    }

    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.data)
            .map_err(|e| Error::BadUpstreamPayload(format!("bad SSE json: {e}: {}", self.data)))
    }
}

/// Incremental SSE parser that tolerates chunk boundaries anywhere.
///
/// Bytes are buffered undecoded until a frame's terminating blank line has
/// arrived, so a chunk boundary inside a multi-byte UTF-8 sequence holds the
/// unfinished bytes instead of replacing them. A frame that is complete but
/// still carries invalid UTF-8 is decoded lossily, because SSE is a text
/// protocol and a few upstreams emit raw bytes in error bodies; that keeps the
/// parser alive without corrupting text that merely arrived in pieces.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes, returning every complete frame that became available.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(end) = frame_end(&self.buffer) {
            let raw: Vec<u8> = self.buffer.drain(..end).collect();
            if let Some(frame) = parse_frame(&String::from_utf8_lossy(&raw)) {
                frames.push(frame);
            }
        }
        frames
    }

    /// Flush a trailing frame that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<SseFrame> {
        let raw = std::mem::take(&mut self.buffer);
        parse_frame(&String::from_utf8_lossy(&raw))
    }
}

/// Index just past the first blank line that separates two frames.
///
/// A blank line is two line endings, and both `\n` and `\r\n` end in `\n`, so
/// the separator is either `\n\n` (LF/LF, LF/CRLF and CRLF/LF) or `\n\r\n`
/// (CRLF/CRLF). Matching on the raw bytes is safe: `\n` and `\r` are ASCII and
/// never appear as UTF-8 continuation bytes, and a buffer ending in `\n\r`
/// reports nothing so a CRLF split across chunks is not cut short.
fn frame_end(buffer: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buffer.len() {
        if buffer[i] == b'\n' {
            if buffer[i + 1] == b'\n' {
                return Some(i + 2);
            }
            if buffer[i + 1] == b'\r' && buffer.get(i + 2) == Some(&b'\n') {
                return Some(i + 3);
            }
        }
        i += 1;
    }
    None
}

fn parse_frame(raw: &str) -> Option<SseFrame> {
    let mut event = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in raw.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data_lines.push(value),
            _ => {}
        }
    }
    if data_lines.is_empty() && event.is_none() {
        return None;
    }
    Some(SseFrame {
        event,
        data: data_lines.join("\n"),
    })
}

/// Translate an upstream SSE frame into canonical events.
///
/// Implementations are stateful because OpenAI chunks carry no block structure.
pub trait StreamParser: Send {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>>;
    /// Called when the upstream body ends, to close dangling blocks.
    ///
    /// This is *not* proof that the answer finished: an upstream relay that
    /// drops the connection mid-answer reaches here too. [`Self::saw_normal_terminal`]
    /// is the only evidence of a genuine ending.
    fn finish(&mut self) -> Vec<StreamEvent>;
    /// Whether a genuine normal terminal event was observed before the body
    /// ended: an OpenAI `finish_reason` or `[DONE]`, an Anthropic
    /// `message_delta` carrying a `stop_reason` or `message_stop`, a
    /// Responses `response.completed`/`response.incomplete`, or a Gemini
    /// `finishReason`.
    ///
    /// A body that ends without one of these was truncated, not completed,
    /// however much valid content preceded the cut.
    fn saw_normal_terminal(&self) -> bool;
    /// Usage the body reported before it ended.
    ///
    /// Read when the body ended without a normal terminal, so the partial
    /// answer's known spend is still recorded honestly.
    fn reported_usage(&self) -> Usage;
}

/// Turn canonical events into SSE frames for a client.
pub trait StreamEncoder: Send {
    /// Adopt the response id the pipeline owns for this stream.
    ///
    /// The Responses dialect names its whole response, and the proxy registers
    /// that response under an id of its own before the first frame leaves, so
    /// every frame the client is shown has to carry the registered identity:
    /// an id the client cannot use against `cancel` or `GET` is not an
    /// identity. Dialects without a response-level id ignore this.
    fn set_response_id(&mut self, _id: &str) {}
    /// The output items this stream published, once it has ended.
    ///
    /// A stored response has to replay exactly what the client was shown, so
    /// the accumulator that built the terminal frame's `output` is the source
    /// of truth for it, not a second reconstruction from canonical events.
    /// `None` for dialects with no response-level output.
    fn response_output(&self) -> Option<Vec<Value>> {
        None
    }
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame>;
    /// Trailing frames, e.g. OpenAI's `[DONE]`.
    fn finish(&mut self) -> Vec<SseFrame>;
    /// Frame used to report a mid stream error.
    fn error(&mut self, err: &Error) -> Vec<SseFrame>;
}

pub fn stream_parser(dialect: Dialect, model: &str) -> Box<dyn StreamParser> {
    match dialect {
        Dialect::Anthropic => Box::new(anthropic::AnthropicStreamParser::new(model)),
        Dialect::OpenAI => Box::new(openai::OpenAiStreamParser::new(model)),
        Dialect::OpenAIResponses => Box::new(responses::ResponsesStreamParser::new(model)),
        Dialect::Gemini => Box::new(gemini::GeminiStreamParser::new(model)),
    }
}

/// `include_usage` only matters for the OpenAI dialect, where the usage trailer
/// is opt in via `stream_options`.
pub fn stream_encoder(
    dialect: Dialect,
    model: &str,
    include_usage: bool,
) -> Box<dyn StreamEncoder> {
    match dialect {
        Dialect::Anthropic => Box::new(anthropic::AnthropicStreamEncoder::new(model)),
        Dialect::OpenAI => {
            Box::new(openai::OpenAiStreamEncoder::new(model).with_usage(include_usage))
        }
        Dialect::OpenAIResponses => Box::new(responses::ResponsesStreamEncoder::new(model)),
        Dialect::Gemini => Box::new(gemini::GeminiStreamEncoder::new(model)),
    }
}

/// Folds a stream of canonical [`StreamEvent`]s back into one [`ChatResponse`].
///
/// A client that asked for a non-streaming reply from an upstream that only
/// streams (OpenCode's free tier forces `stream: true`) still gets a single JSON
/// body: the events are collected here and the result encoded for the client's
/// dialect by [`encode_response`]. Blocks are placed by their stream index, so
/// text, thinking and tool calls interleave in the order they arrived.
#[derive(Debug, Default)]
pub struct StreamCollector {
    id: String,
    model: String,
    usage: Usage,
    blocks: std::collections::BTreeMap<u32, PartialBlock>,
    stop_reason: Option<StopReason>,
    stop_sequence: Option<String>,
}

#[derive(Debug)]
enum PartialBlock {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Redacted(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
}

impl StreamCollector {
    /// Absorb one canonical event.
    pub fn push(&mut self, event: &StreamEvent) {
        use std::collections::btree_map::Entry;
        match event {
            StreamEvent::Start { id, model, usage } => {
                self.id = id.clone();
                self.model = model.clone();
                self.usage = *usage;
            }
            StreamEvent::TextDelta { index, text } => match self.blocks.entry(*index) {
                Entry::Occupied(mut slot) => {
                    if let PartialBlock::Text(existing) = slot.get_mut() {
                        existing.push_str(text);
                    }
                }
                Entry::Vacant(slot) => {
                    slot.insert(PartialBlock::Text(text.clone()));
                }
            },
            StreamEvent::ThinkingDelta { index, text } => match self.blocks.entry(*index) {
                Entry::Occupied(mut slot) => {
                    if let PartialBlock::Thinking { text: existing, .. } = slot.get_mut() {
                        existing.push_str(text);
                    }
                }
                Entry::Vacant(slot) => {
                    slot.insert(PartialBlock::Thinking {
                        text: text.clone(),
                        signature: None,
                    });
                }
            },
            StreamEvent::ThinkingSignature { index, signature } => {
                if let Some(PartialBlock::Thinking {
                    signature: slot, ..
                }) = self.blocks.get_mut(index)
                {
                    *slot = Some(signature.clone());
                }
            }
            StreamEvent::RedactedThinking { index, data } => {
                self.blocks
                    .insert(*index, PartialBlock::Redacted(data.clone()));
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.blocks.insert(
                    *index,
                    PartialBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        json: String::new(),
                    },
                );
            }
            StreamEvent::ToolUseDelta {
                index,
                partial_json,
            } => {
                if let Some(PartialBlock::ToolUse { json, .. }) = self.blocks.get_mut(index) {
                    json.push_str(partial_json);
                }
            }
            StreamEvent::BlockStop { .. } | StreamEvent::Ping => {}
            StreamEvent::Stop {
                stop_reason,
                stop_sequence,
                usage,
            } => {
                self.stop_reason = Some(*stop_reason);
                self.stop_sequence = stop_sequence.clone();
                self.usage = *usage;
            }
        }
    }

    /// The assembled response.
    pub fn finish(self) -> ChatResponse {
        ChatResponse {
            id: self.id,
            model: self.model,
            content: self
                .blocks
                .into_values()
                .map(PartialBlock::into_block)
                .collect(),
            stop_reason: self.stop_reason.unwrap_or(StopReason::Unknown),
            stop_sequence: self.stop_sequence,
            usage: self.usage,
            passthrough: Default::default(),
        }
    }
}

impl PartialBlock {
    fn into_block(self) -> ContentBlock {
        match self {
            PartialBlock::Text(text) => ContentBlock::Text {
                text,
                cache_control: None,
            },
            PartialBlock::Thinking { text, signature } => {
                ContentBlock::Thinking { text, signature }
            }
            PartialBlock::Redacted(data) => ContentBlock::RedactedThinking { data },
            PartialBlock::ToolUse { id, name, json } => ContentBlock::ToolUse {
                id,
                name,
                // Arguments stream as a JSON string; an unparseable tail becomes
                // Null rather than a fabricated object.
                input: serde_json::from_str(&json).unwrap_or(Value::Null),
            },
        }
    }
}

/// Fold a whole event sequence into one response.
pub fn collect_stream(events: impl IntoIterator<Item = StreamEvent>) -> ChatResponse {
    let mut collector = StreamCollector::default();
    for event in events {
        collector.push(&event);
    }
    collector.finish()
}

#[cfg(test)]
mod collector_tests {
    use super::*;

    #[test]
    fn collects_text_thinking_and_usage() {
        let events = vec![
            StreamEvent::Start {
                id: "id1".into(),
                model: "m".into(),
                usage: Usage::default(),
            },
            StreamEvent::ThinkingDelta {
                index: 0,
                text: "think".into(),
            },
            StreamEvent::ThinkingSignature {
                index: 0,
                signature: "sig".into(),
            },
            StreamEvent::TextDelta {
                index: 1,
                text: "hello ".into(),
            },
            StreamEvent::TextDelta {
                index: 1,
                text: "world".into(),
            },
            StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Usage::default()
                },
            },
        ];
        let resp = collect_stream(events);
        assert_eq!(resp.id, "id1");
        assert_eq!(resp.model, "m");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.content.len(), 2);
        assert_eq!(
            resp.content[0],
            ContentBlock::Thinking {
                text: "think".into(),
                signature: Some("sig".into())
            }
        );
        assert_eq!(
            resp.content[1],
            ContentBlock::Text {
                text: "hello world".into(),
                cache_control: None
            }
        );
    }

    #[test]
    fn collects_a_tool_call() {
        let events = vec![
            StreamEvent::ToolUseStart {
                index: 0,
                id: "call_1".into(),
                name: "bash".into(),
            },
            StreamEvent::ToolUseDelta {
                index: 0,
                partial_json: "{\"cmd\":".into(),
            },
            StreamEvent::ToolUseDelta {
                index: 0,
                partial_json: "\"ls\"}".into(),
            },
            StreamEvent::Stop {
                stop_reason: StopReason::ToolUse,
                stop_sequence: None,
                usage: Usage::default(),
            },
        ];
        let resp = collect_stream(events);
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "bash");
                assert_eq!(input["cmd"], "ls");
            }
            other => panic!("expected a tool call, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_stop_event_defaults_to_unknown() {
        let resp = collect_stream(vec![StreamEvent::TextDelta {
            index: 0,
            text: "hi".into(),
        }]);
        assert_eq!(resp.stop_reason, StopReason::Unknown);
    }

    #[test]
    fn a_redacted_thinking_block_is_preserved() {
        let resp = collect_stream(vec![StreamEvent::RedactedThinking {
            index: 0,
            data: "opaque".into(),
        }]);
        assert_eq!(
            resp.content[0],
            ContentBlock::RedactedThinking {
                data: "opaque".into()
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Dialect;

    #[test]
    fn classifier_round_trip_preserves_every_judging_field() {
        // The full Auto Mode stage-1 shape, decoded from Anthropic and encoded
        // for an OpenAI-compatible upstream. Every field here is part of how
        // the classifier is judged: the system prompt carries the policy, the
        // transcript carries the evidence, the stop sequence terminates the
        // verdict, and temperature 0 makes the verdict reproducible.
        let incoming = serde_json::json!({
            "model": "claude-opus-4-8[1m]",
            "max_tokens": 64,
            "temperature": 0,
            "stop_sequences": ["</block>"],
            "system": [
                {"type": "text", "text": "You are a security monitor.",
                 "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "transcript",
                                              "cache_control": {"type": "ephemeral"}}]}
            ]
        });
        let req = anthropic::decode_request(incoming).unwrap();
        let body =
            openai::encode_request_with(&req, "glm-5.3", &ProviderQuirks::default()).unwrap();

        assert_eq!(body["model"], "glm-5.3");
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["temperature"], 0.0);
        // The stop sequence survives *as an array*, the exact wire shape that
        // broke real classifier traffic on a compatible endpoint once.
        assert_eq!(body["stop"], serde_json::json!(["</block>"]));
        // System becomes the first message; content and cache markers survive
        // in their OpenAI-appropriate places (system text, message text).
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"],
            "You are a security monitor."
        );
        assert_eq!(body["messages"][1]["content"], "transcript");

        // And an Anthropic upstream keeps the same request in its own dialect.
        let body = anthropic::encode_request(&req, "claude-opus-4-8").unwrap();
        assert_eq!(body["stop_sequences"], serde_json::json!(["</block>"]));
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 64);
        let _ = Dialect::Anthropic;
    }

    #[test]
    fn splits_frames_across_chunk_boundaries() {
        let mut d = SseDecoder::new();
        assert!(d.push(b"event: message_st").is_empty());
        assert!(d.push(b"art\ndata: {\"a\":").is_empty());
        let frames = d.push(b"1}\n\nevent: ping\ndata: {}\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event.as_deref(), Some("message_start"));
        assert_eq!(frames[0].json().unwrap()["a"], 1);
        assert_eq!(frames[1].event.as_deref(), Some("ping"));
    }

    #[test]
    fn handles_crlf_comments_and_multiline_data() {
        let mut d = SseDecoder::new();
        let frames = d.push(b": keep-alive\r\n\r\ndata: line1\r\ndata: line2\r\n\r\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "line1\nline2");
    }

    #[test]
    fn openai_done_sentinel_and_unterminated_tail() {
        let mut d = SseDecoder::new();
        let frames = d.push(b"data: [DONE]\n\ndata: {\"trailing\":true}");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "[DONE]");
        let tail = d.finish().unwrap();
        assert_eq!(tail.json().unwrap()["trailing"], true);
    }

    #[test]
    fn utf8_split_at_every_byte_boundary_survives() {
        // A chunk boundary has no reason to respect UTF-8, so feed the frame one
        // byte at a time: the CJK text and the emoji must come back unchanged.
        let wire = "data: {\"text\":\"中文🙂 组合é\"}\n\n";
        let mut d = SseDecoder::new();
        let mut frames = Vec::new();
        for byte in wire.as_bytes() {
            frames.extend(d.push(std::slice::from_ref(byte)));
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].json().unwrap()["text"], "中文🙂 组合é");
    }

    #[test]
    fn utf8_split_at_every_byte_boundary_keeps_tool_arguments() {
        // Tool arguments carry file names and paths; a mangled byte here is not
        // a display problem, it is an unusable function call.
        let arguments = "{\"path\":\"项目/报告.txt\",\"note\":\"🙂\"}";
        let wire = format!(
            "data: {{\"arguments\":{}}}\n\n",
            serde_json::json!(arguments)
        );
        let mut d = SseDecoder::new();
        let mut frames = Vec::new();
        for byte in wire.as_bytes() {
            frames.extend(d.push(std::slice::from_ref(byte)));
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].json().unwrap()["arguments"], arguments);
    }

    #[test]
    fn crlf_frames_split_at_every_byte_boundary_are_still_frames() {
        let wire = "event: ping\r\ndata: {\"text\":\"中文\"}\r\n\r\n";
        let mut d = SseDecoder::new();
        let mut frames = Vec::new();
        for byte in wire.as_bytes() {
            frames.extend(d.push(std::slice::from_ref(byte)));
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("ping"));
        assert_eq!(frames[0].json().unwrap()["text"], "中文");
    }

    #[test]
    fn invalid_bytes_inside_a_complete_frame_are_still_tolerated() {
        // The counterpart to the split-sequence case: bytes that are invalid in
        // a frame that did arrive complete stay lossy rather than fatal.
        let mut d = SseDecoder::new();
        let frames = d.push(b"data: \xff\xfe\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "\u{fffd}\u{fffd}");
    }

    #[test]
    fn frame_round_trips_to_wire() {
        let f = SseFrame {
            event: Some("ping".into()),
            data: "{}".into(),
        };
        assert_eq!(f.to_wire(), "event: ping\ndata: {}\n\n");
        let mut d = SseDecoder::new();
        assert_eq!(d.push(f.to_wire().as_bytes()), vec![f]);
    }
}
