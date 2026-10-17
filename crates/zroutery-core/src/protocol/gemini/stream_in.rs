//! Parsing a Gemini SSE stream into IR events.

use serde_json::Value;

use super::response::decode_usage_metadata;
use crate::protocol::{SseFrame, StreamParser};

use crate::error::{Error, Result};
use crate::ir::{StopReason, StreamEvent, Usage};
// ---------------------------------------------------------------- stream in

pub struct GeminiStreamParser {
    model: String,
    started: bool,
    /// Whether the upstream really ended the answer (`finishReason`). A body
    /// that just stops arriving is truncated.
    normal_terminal: bool,
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
            normal_terminal: false,
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
            self.usage = decode_usage_metadata(usage);
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
                        let id = call.get("id").and_then(Value::as_str).ok_or_else(|| {
                            Error::BadUpstreamPayload("stream functionCall is missing `id`".into())
                        })?;
                        let name = call.get("name").and_then(Value::as_str).ok_or_else(|| {
                            Error::BadUpstreamPayload(
                                "stream functionCall is missing `name`".into(),
                            )
                        })?;
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: id.to_string(),
                            name: name.to_string(),
                        });
                        if let Some(args) = call.get("args") {
                            out.push(StreamEvent::ToolUseDelta {
                                index,
                                partial_json: args.to_string(),
                            });
                        }
                        // Gemini sends functionCall as a complete block, so close it.
                        out.push(StreamEvent::BlockStop { index });
                    } else {
                        return Err(Error::BadUpstreamPayload(
                            "unsupported Gemini stream response part".into(),
                        ));
                    }
                }
            }
            if let Some(finish_reason) = candidate.get("finishReason").and_then(Value::as_str) {
                self.normal_terminal = true;
                // Close the running text block before the terminal event: a
                // client's block stream must not see a block stop after the
                // answer has already ended.
                if let Some(text_idx) = self.text_index.take() {
                    self.open_blocks.retain(|i| *i != text_idx);
                    out.push(StreamEvent::BlockStop { index: text_idx });
                }
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
        if self.normal_terminal {
            // `finishReason` already closed every block before the terminal
            // event, so nothing may follow `Stop`.
            return Vec::new();
        }
        let mut out = Vec::new();
        // Close whatever is still open so the client's block stream stays well
        // formed. A body that ended without a `finishReason` gets no `Stop`:
        // that event is what marks the answer finished, and this one was not.
        if let Some(text_idx) = self.text_index.take() {
            self.open_blocks.retain(|i| *i != text_idx);
            out.push(StreamEvent::BlockStop { index: text_idx });
        }
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(StreamEvent::BlockStop { index });
        }
        out
    }

    fn saw_normal_terminal(&self) -> bool {
        self.normal_terminal
    }

    fn reported_usage(&self) -> Usage {
        self.usage
    }
}
