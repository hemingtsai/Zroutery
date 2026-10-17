//! Encoding IR events back into a Gemini SSE stream.

use serde_json::{json, Value};

use super::response::encode_usage;
use crate::protocol::{SseFrame, StreamEncoder};

use crate::error::Error;
use crate::ir::{Dialect, StopReason, StreamEvent};
// --------------------------------------------------------------- stream out

pub struct GeminiStreamEncoder {
    done: bool,
    /// Buffered tool calls awaiting completion: (index, id, name, accumulated args).
    tool_buffers: Vec<(u32, String, String, String)>,
}

impl GeminiStreamEncoder {
    /// The model is not stored: Gemini's wire format names no model, and the
    /// client-facing id is not part of a streamed `candidates` frame.
    pub fn new(_model: &str) -> Self {
        Self {
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
            // Gemini's wire format names no model, so nothing is echoed here;
            // the upstream's own name in the event is deliberately dropped.
            StreamEvent::Start { .. } => Vec::new(),
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
            StreamEvent::ToolUseStart {
                index, id, name, ..
            } => {
                // Flush any previously buffered tool calls.
                let out = self.flush_tools();
                self.tool_buffers
                    .push((*index, id.clone(), name.clone(), String::new()));
                out
            }
            StreamEvent::ToolUseDelta {
                partial_json,
                index,
                ..
            } => {
                if let Some(buf) = self
                    .tool_buffers
                    .iter_mut()
                    .find(|(i, _, _, _)| *i == *index)
                {
                    buf.3.push_str(partial_json);
                }
                Vec::new()
            }
            StreamEvent::BlockStop { index, .. } => {
                // Flush the tool buffer for this index if it exists.
                let mut out = Vec::new();
                if let Some(pos) = self
                    .tool_buffers
                    .iter()
                    .position(|(i, _, _, _)| *i == *index)
                {
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
                        "usageMetadata": encode_usage(usage),
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
