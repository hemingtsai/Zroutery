//! Parsing a Gemini SSE stream into IR events.

use serde_json::Value;

use super::response::decode_usage_metadata;
use crate::protocol::{SseFrame, StreamParser};

use crate::error::{Error, Result};
use crate::ir::{StopReason, StreamEvent, Usage};
// ---------------------------------------------------------------- stream in

pub struct GeminiStreamParser {
    started: bool,
    /// Whether the upstream really ended the answer (`finishReason`). A body
    /// that just stops arriving is truncated.
    normal_terminal: bool,
    next_index: u32,
    text_index: Option<u32>,
    open_blocks: Vec<u32>,
    usage: Usage,
    /// The declaration events already published, so a chunk that restates the
    /// same `modelVersion` produces no event and a chunk that changes it does.
    last_declared: (Option<String>, Option<String>),
}

impl GeminiStreamParser {
    /// `_model` is accepted for parser-signature compatibility with the other
    /// dialects. The configured model is a routing fact, not a declaration;
    /// only the structured fields a chunk itself carries are published.
    pub fn new(_model: &str) -> Self {
        Self {
            started: false,
            normal_terminal: false,
            next_index: 0,
            text_index: None,
            open_blocks: Vec::new(),
            usage: Usage::default(),
            last_declared: (None, None),
        }
    }

    /// Publish the structured identity fields one chunk carried.
    ///
    /// Gemini names its version in a per-chunk `modelVersion` member and may
    /// echo a `model` member; both spellings a chunk could use are read, and
    /// each value is published the first time it is seen and every time it
    /// changes, so a stream that revises its own declaration stays a conflict.
    fn observe_declared(&mut self, value: &Value, out: &mut Vec<StreamEvent>) {
        let model = value
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty());
        let version = value
            .get("modelVersion")
            .or_else(|| value.get("model_version"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|version| !version.is_empty());
        if model.is_none() && version.is_none() {
            return;
        }
        if model == self.last_declared.0.as_deref() && version == self.last_declared.1.as_deref() {
            return;
        }
        let mut declaration = crate::ir::UpstreamModelDeclaration::default();
        declaration.observe_model(model);
        declaration.observe_model_version(version);
        self.last_declared = (model.map(str::to_string), version.map(str::to_string));
        out.push(StreamEvent::UpstreamDeclaration { declaration });
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
            // The seeded configured model is a routing fact, not a
            // declaration; substituting it here would record a declaration the
            // provider never made. An absent name stays absent, and whatever
            // identity fields the chunks themselves carry are published below.
            out.push(StreamEvent::Start {
                id: format!("gemini-{}", uuid::Uuid::new_v4().simple()),
                model: String::new(),
                usage: self.usage,
            });
        }
        self.observe_declared(&value, &mut out);
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
