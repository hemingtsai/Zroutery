//! Wire protocol translation.
//!
//! Two decoders turn incoming payloads into the [`crate::ir`] types, two
//! encoders turn the IR into upstream payloads, and each dialect knows how to
//! serialise responses and streaming events back to a client.

pub mod anthropic;
pub mod openai;

use crate::error::{Error, Result};
use crate::ir::{ChatRequest, ChatResponse, Dialect, StreamEvent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    match dialect {
        Dialect::Anthropic => anthropic::decode_request(body),
        Dialect::OpenAI => openai::decode_request(body),
    }
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
    }
}

/// Decode a non streaming upstream response body into the IR.
pub fn decode_response(dialect: Dialect, body: Value) -> Result<ChatResponse> {
    match dialect {
        Dialect::Anthropic => anthropic::decode_response(body),
        Dialect::OpenAI => openai::decode_response(body),
    }
}

/// Encode an IR response for a client speaking `dialect`.
pub fn encode_response(dialect: Dialect, resp: &ChatResponse) -> Value {
    match dialect {
        Dialect::Anthropic => anthropic::encode_response(resp),
        Dialect::OpenAI => openai::encode_response(resp),
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
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes, returning every complete frame that became available.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer
            .push_str(&String::from_utf8_lossy(chunk).replace("\r\n", "\n"));
        let mut frames = Vec::new();
        while let Some(pos) = self.buffer.find("\n\n") {
            let raw: String = self.buffer.drain(..pos + 2).collect();
            if let Some(frame) = parse_frame(&raw) {
                frames.push(frame);
            }
        }
        frames
    }

    /// Flush a trailing frame that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<SseFrame> {
        let raw = std::mem::take(&mut self.buffer);
        parse_frame(&raw)
    }
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
    fn finish(&mut self) -> Vec<StreamEvent>;
}

/// Turn canonical events into SSE frames for a client.
pub trait StreamEncoder: Send {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
