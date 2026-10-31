//! The outbound SSE encoder: canonical events in, chat completion chunks out.

use serde_json::{json, Map, Value};

use super::response::{encode_usage, finish_reason_to_str};
use crate::error::Error;
use crate::ir::{ContentBlock, Dialect, StreamEvent, Usage};
use crate::protocol::reasoning_bridge;

use crate::protocol::{SseFrame, StreamEncoder};

// --------------------------------------------------------------- stream out

/// Renders canonical events as OpenAI chat completion chunks.
pub struct OpenAiStreamEncoder {
    model: String,
    id: String,
    created: i64,
    include_usage: bool,
    role_sent: bool,
    tool_slots: Vec<(u32, u64)>,
    usage: Usage,
    done: bool,
}

impl OpenAiStreamEncoder {
    pub fn new(model: &str) -> Self {
        OpenAiStreamEncoder {
            model: model.to_string(),
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            created: chrono::Utc::now().timestamp(),
            include_usage: false,
            role_sent: false,
            tool_slots: Vec::new(),
            usage: Usage::default(),
            done: false,
        }
    }

    pub fn with_usage(mut self, include: bool) -> Self {
        self.include_usage = include;
        self
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>) -> SseFrame {
        let mut choice = Map::new();
        choice.insert("index".into(), json!(0));
        choice.insert("delta".into(), delta);
        choice.insert("logprobs".into(), Value::Null);
        choice.insert(
            "finish_reason".into(),
            match finish_reason {
                Some(r) => json!(r),
                None => Value::Null,
            },
        );
        SseFrame {
            event: None,
            data: json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [Value::Object(choice)],
            })
            .to_string(),
        }
    }

    fn ensure_role(&mut self, out: &mut Vec<SseFrame>) {
        if self.role_sent {
            return;
        }
        self.role_sent = true;
        out.push(self.chunk(json!({"role": "assistant", "content": ""}), None));
    }

    fn slot_for(&mut self, index: u32) -> u64 {
        if let Some((_, slot)) = self.tool_slots.iter().find(|(i, _)| *i == index) {
            return *slot;
        }
        let slot = self.tool_slots.len() as u64;
        self.tool_slots.push((index, slot));
        slot
    }
}

impl StreamEncoder for OpenAiStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        let mut out = Vec::new();
        match event {
            StreamEvent::Native { canonical, .. } => {
                for event in canonical {
                    out.extend(self.encode(event));
                }
            }
            StreamEvent::Start { id, usage, .. } => {
                // The upstream's own model name is ignored on purpose: a client
                // that asked for an exposed id is told that id, exactly as the
                // non-streaming encoder reports it.
                self.id = if id.starts_with("chatcmpl") {
                    id.clone()
                } else {
                    format!("chatcmpl-{id}")
                };
                self.usage = *usage;
                self.ensure_role(&mut out);
            }
            StreamEvent::TextDelta { text, .. } => {
                self.ensure_role(&mut out);
                out.push(self.chunk(json!({"content": text}), None));
            }
            StreamEvent::ThinkingDelta { text, .. } => {
                self.ensure_role(&mut out);
                out.push(self.chunk(json!({"reasoning_content": text}), None));
            }
            // No Chat Completions wire equivalent; signatures are only meaningful
            // when a later request is encoded through the reasoning bridge.
            StreamEvent::ThinkingSignature { .. } => {}
            StreamEvent::RedactedThinking { index, data } => {
                if let Some(item) =
                    reasoning_bridge::encode_thinking_block(&ContentBlock::RedactedThinking {
                        data: data.clone(),
                    })
                {
                    if let Some(encoded) = item.get("encrypted_content").and_then(Value::as_str) {
                        self.ensure_role(&mut out);
                        out.push(self.chunk(json!({"reasoning_content": encoded}), None));
                    }
                }
                let _ = index;
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.ensure_role(&mut out);
                let slot = self.slot_for(*index);
                out.push(self.chunk(
                    json!({"tool_calls": [{
                        "index": slot,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    }]}),
                    None,
                ));
            }
            StreamEvent::ToolUseDelta {
                index,
                partial_json,
            } => {
                self.ensure_role(&mut out);
                let slot = self.slot_for(*index);
                out.push(self.chunk(
                    json!({"tool_calls": [{
                        "index": slot,
                        "function": {"arguments": partial_json}
                    }]}),
                    None,
                ));
            }
            StreamEvent::BlockStop { .. } => {}
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                self.ensure_role(&mut out);
                self.usage = *usage;
                out.push(self.chunk(
                    json!({}),
                    Some(finish_reason_to_str(*stop_reason).unwrap_or("stop")),
                ));
                if self.include_usage {
                    out.push(SseFrame {
                        event: None,
                        data: json!({
                            "id": self.id,
                            "object": "chat.completion.chunk",
                            "created": self.created,
                            "model": self.model,
                            "choices": [],
                            "usage": encode_usage(usage),
                        })
                        .to_string(),
                    });
                }
                out.push(SseFrame {
                    event: None,
                    data: "[DONE]".into(),
                });
                self.done = true;
            }
            StreamEvent::Ping => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.done {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_role(&mut out);
        out.push(self.chunk(json!({}), Some("stop")));
        out.push(SseFrame {
            event: None,
            data: "[DONE]".into(),
        });
        self.done = true;
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        let mut out = vec![SseFrame {
            event: None,
            data: err.to_wire(Dialect::OpenAI).to_string(),
        }];
        if !self.done {
            out.push(SseFrame {
                event: None,
                data: "[DONE]".into(),
            });
            self.done = true;
        }
        out
    }
}
