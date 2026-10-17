//! Encoding IR events back into an Anthropic SSE stream.

use serde_json::{json, Value};

use crate::error::Error;
use crate::ir::{Dialect, StreamEvent, Usage};
use crate::protocol::{SseFrame, StreamEncoder};

use super::response::{encode_usage, stop_reason_to_str};

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
            StreamEvent::Start { id, usage, .. } => {
                // Keep the model the client asked for; the upstream's own name
                // never reaches a client-facing frame.
                self.id = id.clone();
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
                // `MessageDeltaUsage` carries cumulative input and cache
                // counters too, so the last frame the client sees restates the
                // complete, cache-exclusive Anthropic usage even when the
                // `message_start` frame was emitted before the upstream
                // reported any (an OpenAI trailer, for example).
                out.push(Self::frame(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": stop_reason_to_str(*stop_reason),
                            "stop_sequence": stop_sequence,
                        },
                        "usage": encode_usage(usage),
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
                "usage": encode_usage(&self.usage),
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
