//! Wire protocol translation.
//!
//! Two decoders turn incoming payloads into the [`crate::ir`] types, two
//! encoders turn the IR into upstream payloads, and each dialect knows how to
//! serialise responses and streaming events back to a client.

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod reasoning_bridge;
pub mod responses;

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, StreamEvent, UnsupportedContentPolicy, Usage,
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
        ContentBlock::Citation { .. } => "citation".into(),
        ContentBlock::Annotation { .. } => "annotation".into(),
        _ => "unsupported content".into(),
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
/// variation between implementations.
pub fn encode_request(
    dialect: Dialect,
    req: &ChatRequest,
    upstream_model: &str,
    quirks: &ProviderQuirks,
) -> Result<Value> {
    match dialect {
        Dialect::Anthropic => anthropic::encode_request(req, upstream_model),
        Dialect::OpenAI => openai::encode_request_with(req, upstream_model, quirks),
        Dialect::OpenAIResponses => responses::encode_request(req, upstream_model),
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
