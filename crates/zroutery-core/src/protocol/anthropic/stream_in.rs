//! Parsing an Anthropic SSE stream into IR events.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::ir::{StopReason, StreamEvent, Usage};
use crate::protocol::{unsupported_upstream_content, SseFrame, StreamParser};

use super::response::{decode_usage, stop_reason_from_str, usage_counter};

// ---------------------------------------------------------------- stream in

/// Parses an Anthropic SSE stream into canonical events.
pub struct AnthropicStreamParser {
    model: String,
    usage: Usage,
    started: bool,
    open: Vec<u32>,
    stopped: bool,
    /// Whether the upstream really ended the message (`stop_reason` or
    /// `message_stop`). A body that just stops arriving is truncated.
    normal_terminal: bool,
}

impl AnthropicStreamParser {
    pub fn new(model: &str) -> Self {
        AnthropicStreamParser {
            model: model.to_string(),
            usage: Usage::default(),
            started: false,
            open: Vec::new(),
            stopped: false,
            normal_terminal: false,
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
                    Some("text") => {
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
                    Some(_) => {
                        return Err(unsupported_upstream_content("Anthropic stream block"));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Anthropic stream content block is missing `type`".into(),
                        ));
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
                    Some(_) => {
                        return Err(unsupported_upstream_content("Anthropic stream delta"));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Anthropic stream delta is missing `type`".into(),
                        ));
                    }
                }
            }
            "content_block_stop" => {
                self.open.retain(|i| *i != index);
                vec![StreamEvent::BlockStop { index }]
            }
            "message_delta" => {
                let delta = v.get("delta");
                if let Some(u) = v.get("usage") {
                    // `MessageDeltaUsage` is cumulative *per field*, including
                    // the cache counters, and the final delta is the last word
                    // on usage. Merging field by field means a delta that only
                    // restates some counters does not drop the ones the
                    // `message_start` frame already reported.
                    if let Some(output) = usage_counter(u, "output_tokens") {
                        self.usage.output_tokens = output;
                    }
                    let fresh = usage_counter(u, "input_tokens");
                    let read = usage_counter(u, "cache_read_input_tokens");
                    let write = usage_counter(u, "cache_creation_input_tokens");
                    if fresh.is_some() || read.is_some() || write.is_some() {
                        if let Some(read) = read {
                            self.usage.cache_read_tokens = read;
                        }
                        if let Some(write) = write {
                            self.usage.cache_write_tokens = write;
                        }
                        // `input_tokens` is cache-exclusive; a delta that omits
                        // it keeps the fresh part already reported.
                        let fresh = fresh
                            .map(u64::from)
                            .unwrap_or_else(|| u64::from(self.usage.fresh_input_tokens()));
                        let total = fresh
                            + u64::from(self.usage.cache_read_tokens)
                            + u64::from(self.usage.cache_write_tokens);
                        self.usage.input_tokens = total.min(u32::MAX as u64) as u32;
                    }
                }
                match delta
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    Some(reason) => {
                        self.normal_terminal = true;
                        self.stopped = true;
                        vec![StreamEvent::Stop {
                            stop_reason: stop_reason_from_str(reason),
                            stop_sequence: delta
                                .and_then(|d| d.get("stop_sequence"))
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            usage: self.usage,
                        }]
                    }
                    // A delta that only restates usage is not the end of the
                    // message: `message_stop`, or a later delta carrying the
                    // reason, is.
                    None => Vec::new(),
                }
            }
            "message_stop" => {
                self.normal_terminal = true;
                Vec::new()
            }
            "ping" => vec![StreamEvent::Ping],
            "error" => {
                let msg = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream stream error");
                return Err(Error::BadUpstreamPayload(msg.to_string()));
            }
            _ => {
                return Err(unsupported_upstream_content("Anthropic stream event"));
            }
        };
        Ok(events)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for index in std::mem::take(&mut self.open) {
            out.push(StreamEvent::BlockStop { index });
        }
        // Only a message that really ended gets the `Stop`; a body that stopped
        // arriving closes its blocks but claims nothing.
        if self.normal_terminal && !self.stopped {
            out.push(StreamEvent::Stop {
                stop_reason: StopReason::Unknown,
                stop_sequence: None,
                usage: self.usage,
            });
            self.stopped = true;
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
