//! The outbound SSE encoder: canonical events in, response frames out.

use std::cell::Cell;
use std::collections::HashMap;

use serde_json::{json, Value};

use super::response::encode_usage;
use crate::error::Error;
use crate::ir::{ContentBlock, StopReason, StreamEvent};
use crate::protocol::reasoning_bridge;
use crate::protocol::{SseFrame, StreamEncoder};

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputItemKind {
    Text,
    Thinking,
}

/// Per-tool-call state, fully independent of the global Text/Thinking state.
struct ToolOutputState {
    output_index: u32,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

pub struct ResponsesStreamEncoder {
    id: String,
    model: String,
    created_at: i64,
    done: bool,
    next_output_index: u32,
    /// Monotonic SSE `sequence_number`, stamped on every emitted frame.
    next_sequence: Cell<u64>,
    /// True once the upstream reported a token-limit stop.
    truncated: bool,
    // --- Text/Thinking state (sequential, one at a time) ---
    current_output_index: u32,
    content_index: u32,
    current_kind: Option<OutputItemKind>,
    content_part_open: bool,
    output_item_open: bool,
    current_item_id: String,
    current_text: String,
    current_thinking: String,
    /// Upstream block index of the thinking item currently open.
    current_thinking_source: Option<u32>,
    /// Anthropic thinking signatures awaiting the end of their block.
    thinking_signatures: HashMap<u32, String>,
    // --- Tool state (parallel, per-index) ---
    tool_states: HashMap<u32, ToolOutputState>,
    // --- Output accumulator (indexed by output_index) ---
    output_items: HashMap<u32, Value>,
}

impl ResponsesStreamEncoder {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("resp-{}", uuid::Uuid::new_v4().simple()),
            model: model.to_string(),
            created_at: chrono::Utc::now().timestamp(),
            done: false,
            next_output_index: 0,
            next_sequence: Cell::new(0),
            truncated: false,
            current_output_index: 0,
            content_index: 0,
            current_kind: None,
            content_part_open: false,
            output_item_open: false,
            current_item_id: String::new(),
            current_text: String::new(),
            current_thinking: String::new(),
            current_thinking_source: None,
            thinking_signatures: HashMap::new(),
            tool_states: HashMap::new(),
            output_items: HashMap::new(),
        }
    }

    fn frame(&self, event_type: &str, data: Value) -> SseFrame {
        // Every official Responses event carries a monotonic sequence_number.
        let mut data = data;
        if let Value::Object(map) = &mut data {
            map.insert("sequence_number".into(), json!(self.next_sequence.get()));
        }
        self.next_sequence.set(self.next_sequence.get() + 1);
        SseFrame {
            event: Some(event_type.to_string()),
            data: data.to_string(),
        }
    }

    /// Allocate the next output_index.
    fn alloc_output_index(&mut self) -> u32 {
        let i = self.next_output_index;
        self.next_output_index += 1;
        i
    }

    fn close_content_part(&mut self) -> Option<SseFrame> {
        if !self.content_part_open {
            return None;
        }
        self.content_part_open = false;
        let (event_type, index_field, part) = match self.current_kind {
            Some(OutputItemKind::Text) => (
                "response.content_part.done",
                "content_index",
                json!({
                    "type": "output_text",
                    "text": self.current_text,
                    "annotations": [],
                    "logprobs": [],
                }),
            ),
            Some(OutputItemKind::Thinking) => (
                "response.reasoning_summary_part.done",
                "summary_index",
                json!({
                    "type": "summary_text",
                    "text": self.current_thinking,
                }),
            ),
            _ => return None,
        };
        let mut data = json!({
            "type": event_type,
            "item_id": self.current_item_id,
            "output_index": self.current_output_index,
            "part": part,
        });
        data[index_field] = json!(self.content_index);
        Some(self.frame(event_type, data))
    }

    /// Collect output_items into a sorted Vec by output_index.
    ///
    /// Borrows rather than drains: the assembled output is also what a stored
    /// response replays, so publishing it once must not consume it.
    fn sorted_output(&self) -> Vec<Value> {
        let mut items: Vec<_> = self.output_items.iter().collect();
        items.sort_by_key(|(index, _)| **index);
        items.into_iter().map(|(_, item)| item.clone()).collect()
    }

    /// Finalize the current Text/Thinking output item into output_items.
    fn finalize_output_item(&mut self) {
        let status = if self.truncated {
            "incomplete"
        } else {
            "completed"
        };
        match self.current_kind {
            Some(OutputItemKind::Text) => {
                if !self.current_text.is_empty() {
                    self.output_items.insert(self.current_output_index, json!({
                        "id": self.current_item_id,
                        "type": "message",
                        "role": "assistant",
                        "status": status,
                        "content": [{"type": "output_text", "text": self.current_text, "annotations": []}],
                    }));
                    self.current_text.clear();
                }
            }
            Some(OutputItemKind::Thinking) => {
                // A streamed thinking block is only replayable when its
                // signature travels with the reasoning item, so pack it into
                // the same opaque envelope the non-streaming encoder uses.
                let signature = self
                    .current_thinking_source
                    .and_then(|index| self.thinking_signatures.remove(&index));
                let block = ContentBlock::Thinking {
                    text: self.current_thinking.clone(),
                    signature,
                };
                let encrypted = reasoning_bridge::encode_thinking_block(&block)
                    .and_then(|item| item.get("encrypted_content").cloned());
                if !self.current_thinking.is_empty() || encrypted.is_some() {
                    let summary = if self.current_thinking.is_empty() {
                        json!([])
                    } else {
                        json!([{"type": "summary_text", "text": self.current_thinking}])
                    };
                    let mut item = json!({
                        "id": self.current_item_id,
                        "type": "reasoning",
                        "summary": summary,
                    });
                    if let Some(encrypted) = encrypted {
                        item["encrypted_content"] = encrypted;
                    }
                    self.output_items.insert(self.current_output_index, item);
                    self.current_thinking.clear();
                }
            }
            _ => {}
        }
    }

    /// Close the current Text/Thinking output item.
    /// Returns multiple events: terminal text/thinking event, content_part.done,
    /// output_item.done. Only operates on Text/Thinking global state.
    fn close_output_item(&mut self) -> Vec<SseFrame> {
        if !self.output_item_open {
            return Vec::new();
        }
        let mut out = Vec::new();

        // 1. Emit terminal text/thinking event (before content_part.done).
        if self.content_part_open {
            match self.current_kind {
                Some(OutputItemKind::Text) => {
                    out.push(self.frame(
                        "response.output_text.done",
                        json!({
                            "type": "response.output_text.done",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "content_index": self.content_index,
                            "text": self.current_text,
                            "logprobs": [],
                        }),
                    ));
                }
                Some(OutputItemKind::Thinking) => {
                    out.push(self.frame(
                        "response.reasoning_summary_text.done",
                        json!({
                            "type": "response.reasoning_summary_text.done",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "summary_index": self.content_index,
                            "text": self.current_thinking,
                        }),
                    ));
                }
                _ => {}
            }
        }

        // 2. Emit content_part.done (with item_id).
        if let Some(f) = self.close_content_part() {
            out.push(f);
        }

        // 3. Finalize into output_items.
        self.finalize_output_item();

        // 4. Emit output_item.done with the complete item.
        self.output_item_open = false;
        let done_item = self
            .output_items
            .get(&self.current_output_index)
            .cloned()
            .unwrap_or(json!({
                "id": self.current_item_id,
                "type": "message",
                "status": "completed",
            }));
        out.push(self.frame(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": self.current_output_index,
                "item": done_item,
            }),
        ));
        self.current_kind = None;
        self.current_thinking_source = None;
        out
    }

    /// Emit response.output_item.added for a new item.
    fn emit_output_item_added(&self, output_index: u32, item: Value) -> SseFrame {
        self.frame(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": item,
            }),
        )
    }

    /// Open a reasoning output item unless one is already open.
    fn ensure_thinking_item(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        if self.current_kind != Some(OutputItemKind::Thinking) {
            out.extend(self.close_output_item());
            self.current_output_index = self.alloc_output_index();
            self.content_index = 0;
            self.current_kind = Some(OutputItemKind::Thinking);
            self.output_item_open = true;
            self.current_thinking.clear();
            self.current_item_id = format!("rs_{}", self.current_output_index);
            out.push(self.emit_output_item_added(
                self.current_output_index,
                json!({
                    "id": self.current_item_id,
                    "type": "reasoning",
                    "summary": [],
                }),
            ));
        }
        out
    }
}

impl StreamEncoder for ResponsesStreamEncoder {
    fn set_response_id(&mut self, id: &str) {
        self.id = id.to_string();
    }

    fn response_output(&self) -> Option<Vec<Value>> {
        Some(self.sorted_output())
    }

    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        let mut out = Vec::new();
        match event {
            StreamEvent::Start { id, .. } => {
                // The upstream model name in the event is ignored: the frames
                // carry the exposed id the client asked for. When the pipeline
                // owns the response identity it has already set that id and
                // rewrites this event, so the upstream id is only adopted by a
                // caller that has no identity of its own to publish.
                if !id.is_empty() {
                    self.id = id.clone();
                }
                out.push(self.frame(
                    "response.created",
                    json!({
                        "type": "response.created",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "in_progress",
                            "model": self.model,
                            "output": [],
                        }
                    }),
                ));
            }
            StreamEvent::TextDelta { text, .. } => {
                // Switch to Text kind if needed.
                if self.current_kind != Some(OutputItemKind::Text) {
                    out.extend(self.close_output_item());
                    self.current_output_index = self.alloc_output_index();
                    self.content_index = 0;
                    self.current_kind = Some(OutputItemKind::Text);
                    self.output_item_open = true;
                    self.current_text.clear();
                    self.current_item_id = format!("msg_{}", self.current_output_index);
                    // Emit output_item.added
                    out.push(self.emit_output_item_added(
                        self.current_output_index,
                        json!({
                            "id": self.current_item_id,
                            "type": "message",
                            "role": "assistant",
                            "status": "in_progress",
                            "content": [],
                        }),
                    ));
                }
                // Emit content_part.added if needed. The official
                // ResponseOutputText part starts with an empty text; the SDK's
                // stream state machine appends deltas to it and raises a
                // TypeError when it is missing.
                if !self.content_part_open {
                    self.content_part_open = true;
                    out.push(self.frame(
                        "response.content_part.added",
                        json!({
                            "type": "response.content_part.added",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "content_index": self.content_index,
                            "part": {"type": "output_text", "text": "", "annotations": [], "logprobs": []},
                        }),
                    ));
                }
                out.push(self.frame(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": self.current_item_id,
                        "output_index": self.current_output_index,
                        "content_index": self.content_index,
                        "delta": text,
                        "logprobs": [],
                    }),
                ));
                self.current_text.push_str(text);
            }
            StreamEvent::ThinkingDelta { text, index } => {
                out.extend(self.ensure_thinking_item());
                self.current_thinking_source = Some(*index);
                if !self.content_part_open {
                    self.content_part_open = true;
                    out.push(self.frame(
                        "response.reasoning_summary_part.added",
                        json!({
                            "type": "response.reasoning_summary_part.added",
                            "item_id": self.current_item_id,
                            "output_index": self.current_output_index,
                            "summary_index": self.content_index,
                            "part": {"type": "summary_text", "text": ""},
                        }),
                    ));
                }
                out.push(self.frame(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "type": "response.reasoning_summary_text.delta",
                        "item_id": self.current_item_id,
                        "output_index": self.current_output_index,
                        "summary_index": self.content_index,
                        "delta": text,
                    }),
                ));
                self.current_thinking.push_str(text);
            }
            StreamEvent::ThinkingSignature { index, signature } => {
                // The signature arrives after its block's deltas and is only
                // useful when it travels with the reasoning item.
                if !signature.is_empty() {
                    out.extend(self.ensure_thinking_item());
                    self.current_thinking_source = Some(*index);
                    self.thinking_signatures.insert(*index, signature.clone());
                }
            }
            StreamEvent::RedactedThinking { data, .. } => {
                // Redacted thinking is already opaque: echo it verbatim as its
                // own reasoning item, exactly like the non-streaming encoder.
                if let Some(item) =
                    reasoning_bridge::encode_thinking_block(&ContentBlock::RedactedThinking {
                        data: data.clone(),
                    })
                {
                    let output_index = self.alloc_output_index();
                    let item_id = format!("rs_{output_index}");
                    let mut done = item;
                    done["id"] = json!(item_id);
                    self.output_items.insert(output_index, done.clone());
                    out.push(self.emit_output_item_added(
                        output_index,
                        json!({"id": item_id, "type": "reasoning", "summary": []}),
                    ));
                    out.push(self.frame(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": done,
                        }),
                    ));
                }
            }
            StreamEvent::ToolUseStart {
                index, id, name, ..
            } => {
                // Does NOT close Text/Thinking — tools are independent.
                let oi = self.alloc_output_index();
                let item_id = format!("fc_{id}");
                self.tool_states.insert(
                    *index,
                    ToolOutputState {
                        output_index: oi,
                        item_id: item_id.clone(),
                        call_id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    },
                );
                out.push(self.emit_output_item_added(
                    oi,
                    json!({
                        "id": item_id,
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": "",
                        "status": "in_progress",
                    }),
                ));
            }
            StreamEvent::ToolUseDelta {
                partial_json,
                index,
                ..
            } => {
                if let Some(ts) = self.tool_states.get_mut(index) {
                    ts.arguments.push_str(partial_json);
                    let item_id = ts.item_id.clone();
                    let output_index = ts.output_index;
                    let delta = partial_json.clone();
                    let frame = self.frame(
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "delta": delta,
                        }),
                    );
                    out.push(frame);
                }
            }
            StreamEvent::BlockStop { index } => {
                if let Some(ts) = self.tool_states.remove(index) {
                    let oi = ts.output_index;
                    let args_display = if ts.arguments.is_empty() {
                        "{}".to_string()
                    } else {
                        ts.arguments.clone()
                    };
                    out.push(self.frame(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": ts.item_id,
                            "output_index": oi,
                            "content_index": 0,
                            "arguments": ts.arguments,
                        }),
                    ));
                    let item = json!({
                        "type": "function_call",
                        "id": ts.item_id,
                        "call_id": ts.call_id,
                        "name": ts.name,
                        "arguments": args_display,
                        "status": "completed",
                    });
                    self.output_items.insert(oi, item.clone());
                    out.push(self.frame(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": oi,
                            "item": item,
                        }),
                    ));
                    // Does NOT touch global current_* state.
                }
            }
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                self.truncated = *stop_reason == StopReason::MaxTokens;
                out.extend(self.close_output_item());
                // Flush incomplete tool calls with full terminal events.
                let incomplete_tools: Vec<_> = self.tool_states.drain().map(|(_, v)| v).collect();
                for ts in incomplete_tools {
                    out.push(self.frame(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": ts.item_id,
                            "output_index": ts.output_index,
                            "content_index": 0,
                            "arguments": if ts.arguments.is_empty() { "{}" } else { &ts.arguments },
                        }),
                    ));
                    let item = json!({
                        "type": "function_call",
                        "id": ts.item_id,
                        "call_id": ts.call_id,
                        "name": ts.name,
                        "arguments": if ts.arguments.is_empty() { "{}".to_string() } else { ts.arguments },
                        "status": "incomplete",
                    });
                    self.output_items.insert(ts.output_index, item.clone());
                    out.push(self.frame(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": ts.output_index,
                            "item": item,
                        }),
                    ));
                }
                let output = Value::Array(self.sorted_output());
                // A stream cut off by the output token limit is not a success:
                // report the terminal status the non-streaming encoder reports.
                let terminal = if self.truncated {
                    json!({
                        "type": "response.incomplete",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "incomplete",
                            "model": self.model,
                            "output": output,
                            "incomplete_details": {"reason": "max_output_tokens"},
                            "usage": encode_usage(usage),
                        }
                    })
                } else {
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": self.id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "completed",
                            "model": self.model,
                            "output": output,
                            "usage": encode_usage(usage),
                        }
                    })
                };
                let event_type = terminal["type"]
                    .as_str()
                    .expect("terminal frame carries a type")
                    .to_string();
                out.push(self.frame(&event_type, terminal));
                self.done = true;
            }
            _ => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.done {
            return Vec::new();
        }
        self.done = true;
        let mut out = Vec::new();
        out.extend(self.close_output_item());
        let has_incomplete = !self.tool_states.is_empty();
        let incomplete_tools: Vec<_> = self.tool_states.drain().map(|(_, v)| v).collect();
        for ts in incomplete_tools {
            out.push(self.frame(
                "response.function_call_arguments.done",
                json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": ts.item_id,
                    "output_index": ts.output_index,
                    "content_index": 0,
                    "arguments": if ts.arguments.is_empty() { "{}" } else { &ts.arguments },
                }),
            ));
            let item = json!({
                "type": "function_call",
                "id": ts.item_id,
                "call_id": ts.call_id,
                "name": ts.name,
                "arguments": if ts.arguments.is_empty() { "{}".to_string() } else { ts.arguments },
                "status": "incomplete",
            });
            self.output_items.insert(ts.output_index, item.clone());
            out.push(self.frame(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": ts.output_index,
                    "item": item,
                }),
            ));
        }
        let output = Value::Array(self.sorted_output());
        let status = if has_incomplete {
            "incomplete"
        } else {
            "completed"
        };
        out.push(self.frame(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": self.id,
                    "object": "response",
                    "created_at": self.created_at,
                    "status": status,
                    "model": self.model,
                    "output": output,
                }
            }),
        ));
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        let mut out = Vec::new();
        if !self.done {
            self.done = true;
            out.extend(self.close_output_item());
        }
        let output = Value::Array(self.sorted_output());
        out.push(self.frame(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": {
                    "id": self.id,
                    "object": "response",
                    "status": "failed",
                    "model": self.model,
                    "error": {
                        "code": err.kind(),
                        "message": err.safe_message(),
                    },
                    "output": output,
                }
            }),
        ));
        out
    }
}
