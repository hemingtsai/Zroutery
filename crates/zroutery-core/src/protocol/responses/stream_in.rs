//! The inbound SSE parser: frames in, canonical stream events out.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::response::decode_usage;
use crate::error::{Error, Result};
use crate::ir::{StopReason, StreamEvent, Usage};
use crate::protocol::{unsupported_upstream_content, SseFrame, StreamParser};

pub struct ResponsesStreamParser {
    id: String,
    model: String,
    /// The name the upstream's own frames carried, verbatim. `model` stays the
    /// seeded configured model for the session-binding checks; this field is
    /// the one the Start events publish, so a frame that names no model stays
    /// absent instead of being back-filled from the routing plan.
    wire_model: String,
    /// The declaration events already published, so identical repeat frames
    /// produce no event and the terminal frame's response object is compared
    /// against the opening one: a relay that changed the model or the version
    /// part-way through the stream shows up as a conflict, not as a
    /// last-write win.
    last_declared: (Option<String>, Option<String>),
    started: bool,
    /// Whether the upstream really ended the response
    /// (`response.completed`/`response.incomplete`). A body that just stops
    /// arriving is truncated.
    normal_terminal: bool,
    next_index: u32,
    text_index: Option<u32>,
    thinking_index: Option<u32>,
    tool_indices: HashMap<String, u32>,
    /// Tool indices whose block stop has already been emitted.
    stopped_tools: HashSet<u32>,
    usage: Usage,
}

impl ResponsesStreamParser {
    pub fn new(model: &str) -> Self {
        Self {
            id: String::new(),
            model: model.to_string(),
            wire_model: String::new(),
            last_declared: (None, None),
            started: false,
            normal_terminal: false,
            next_index: 0,
            text_index: None,
            thinking_index: None,
            tool_indices: HashMap::new(),
            stopped_tools: HashSet::new(),
            usage: Usage::default(),
        }
    }

    /// Emit the block stop for a finished tool call exactly once.
    fn close_tool(&mut self, index: u32, out: &mut Vec<StreamEvent>) {
        if self.stopped_tools.insert(index) {
            out.push(StreamEvent::BlockStop { index });
        }
    }

    /// Publish the structured identity fields one response object carried.
    ///
    /// The response object rides `response.created`, `response.in_progress`
    /// and the terminal `response.completed`/`response.incomplete` frames, so
    /// the opening and the terminal declaration are both read: a relay that
    /// changed the model or the version between them is a conflict, and the
    /// terminal frame cannot overwrite the opening one by arriving last.
    fn observe_declared(&mut self, response: &Value, out: &mut Vec<StreamEvent>) {
        let model = response
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let version = response
            .get("model_version")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
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

    /// Resolve a tool index from an event that references it by item or call id.
    fn tool_index(&self, event: &Value) -> Option<u32> {
        let by_item = event
            .get("item_id")
            .or_else(|| event.get("id"))
            .and_then(Value::as_str)
            .and_then(|id| self.tool_indices.get(id).copied());
        by_item.or_else(|| {
            event
                .get("call_id")
                .and_then(Value::as_str)
                .and_then(|id| self.tool_indices.get(id).copied())
        })
    }

    fn open_index(&mut self, kind: &str) -> u32 {
        // Reuse an open block while deltas for the same part keep arriving.
        match kind {
            "text" => {
                if let Some(i) = self.text_index {
                    return i;
                }
            }
            "thinking" => {
                if let Some(i) = self.thinking_index {
                    return i;
                }
            }
            _ => {}
        }
        let index = self.next_index;
        self.next_index += 1;
        match kind {
            "text" => self.text_index = Some(index),
            "thinking" => self.thinking_index = Some(index),
            _ => {}
        }
        index
    }
}

impl StreamParser for ResponsesStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let event = frame.json()?;
        if let Some(error) = Error::session_binding_payload(&self.model, &event) {
            return Err(error);
        }
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .or(frame.event.as_deref())
            .or_else(|| {
                event
                    .get("error")
                    .filter(|error| !error.is_null())
                    .map(|_| "error")
            })
            .ok_or_else(|| {
                Error::BadUpstreamPayload("Responses stream event is missing `type`".into())
            })?;
        let mut out = Vec::new();

        match event_type {
            "response.failed" | "error" => {
                return Err(Error::stream_payload(&self.model, &event));
            }
            "response.created" => {
                let response = event.get("response").cloned().unwrap_or_default();
                self.id = response
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("resp_stream")
                    .to_string();
                if let Some(model) = response.get("model").and_then(Value::as_str) {
                    self.model = model.to_string();
                }
                // Verbatim, and never the seeded configured model: the frame
                // that names no model leaves the declaration absent.
                self.wire_model = response
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.started = true;
                out.push(StreamEvent::Start {
                    id: self.id.clone(),
                    model: self.wire_model.clone(),
                    usage: self.usage,
                });
                self.observe_declared(&response, &mut out);
            }
            "response.output_item.added" => {
                let item = event.get("item").ok_or_else(|| {
                    Error::BadUpstreamPayload("output item event is missing `item`".into())
                })?;
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let index = self.next_index;
                        self.next_index += 1;
                        let item_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::BadUpstreamPayload(
                                    "function call item is missing `id`".into(),
                                )
                            })?
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::BadUpstreamPayload(
                                    "function call item is missing `call_id`".into(),
                                )
                            })?
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::BadUpstreamPayload(
                                    "function call item is missing `name`".into(),
                                )
                            })?
                            .to_string();
                        self.tool_indices.insert(item_id, index);
                        self.tool_indices.insert(call_id.clone(), index);
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: call_id,
                            name,
                        });
                    }
                    Some("message") | Some("reasoning") => {}
                    Some(_) => {
                        return Err(unsupported_upstream_content("Responses stream output item"));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses stream output item is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.content_part.added" => {
                let part = event.get("part").ok_or_else(|| {
                    Error::BadUpstreamPayload("content part event is missing `part`".into())
                })?;
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        self.open_index("text");
                    }
                    Some("reasoning") | Some("summary_text") => {
                        self.open_index("thinking");
                    }
                    Some("refusal") => {
                        self.open_index("text");
                    }
                    Some(_) => {
                        return Err(unsupported_upstream_content(
                            "Responses stream content part",
                        ));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses stream content part is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.content_part.done" => {
                let part = event.get("part").ok_or_else(|| {
                    Error::BadUpstreamPayload("content part event is missing `part`".into())
                })?;
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => self.text_index = None,
                    Some("reasoning") | Some("summary_text") => self.thinking_index = None,
                    Some("refusal") => self.text_index = None,
                    Some(_) => {
                        return Err(unsupported_upstream_content(
                            "Responses stream content part",
                        ));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses stream content part is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.output_item.done" => {
                self.text_index = None;
                self.thinking_index = None;
                // A finished function_call item closes its tool block.
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        if let Some(index) = self.tool_index(item) {
                            self.close_tool(index, &mut out);
                        }
                    }
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let index = self.open_index("text");
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("output text delta is missing `delta`".into())
                })?;
                out.push(StreamEvent::TextDelta {
                    index,
                    text: delta.to_string(),
                });
            }
            "response.function_call_arguments.delta" => {
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let index = self.tool_indices.get(item_id).copied().or_else(|| {
                    event
                        .get("call_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.tool_indices.get(id).copied())
                });
                let index = index.ok_or_else(|| {
                    Error::BadUpstreamPayload(
                        "function argument delta does not reference a known tool call".into(),
                    )
                })?;
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("function argument delta is missing `delta`".into())
                })?;
                out.push(StreamEvent::ToolUseDelta {
                    index,
                    partial_json: delta.to_string(),
                });
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let index = self.open_index("thinking");
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    Error::BadUpstreamPayload("reasoning summary delta is missing `delta`".into())
                })?;
                out.push(StreamEvent::ThinkingDelta {
                    index,
                    text: delta.to_string(),
                });
            }
            "response.completed" | "response.incomplete" => {
                if !self.started {
                    self.started = true;
                    out.push(StreamEvent::Start {
                        id: self.id.clone(),
                        model: self.wire_model.clone(),
                        usage: self.usage,
                    });
                }
                if let Some(response) = event.get("response") {
                    if let Some(usage) = response.get("usage") {
                        self.usage = decode_usage(Some(usage));
                    }
                    // The terminal frame's own response object: its model and
                    // version are compared against what the opening frame
                    // declared, so a change is a conflict rather than a
                    // last-write win.
                    self.observe_declared(response, &mut out);
                }
                let stop_reason = if event_type == "response.incomplete" {
                    match event
                        .get("response")
                        .and_then(|r| r.pointer("/incomplete_details/reason"))
                        .and_then(Value::as_str)
                    {
                        Some("content_filter") => StopReason::Refusal,
                        _ => StopReason::MaxTokens,
                    }
                } else if response_has_tool_call(event.get("response")) {
                    // A response that yielded function calls ended for a tool
                    // round, not a natural turn end.
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                self.normal_terminal = true;
                out.push(StreamEvent::Stop {
                    stop_reason,
                    stop_sequence: None,
                    usage: self.usage,
                });
            }
            "response.reasoning_summary_part.added" | "response.reasoning_summary_part.done" => {
                let part = event.get("part").ok_or_else(|| {
                    Error::BadUpstreamPayload(
                        "reasoning summary part event is missing `part`".into(),
                    )
                })?;
                match part.get("type").and_then(Value::as_str) {
                    Some("summary_text") | Some("reasoning") | Some("reasoning_text") => {
                        if event_type == "response.reasoning_summary_part.added" {
                            self.open_index("thinking");
                        } else {
                            self.thinking_index = None;
                        }
                    }
                    Some(_) => {
                        return Err(unsupported_upstream_content(
                            "Responses reasoning summary part",
                        ));
                    }
                    None => {
                        return Err(Error::BadUpstreamPayload(
                            "Responses reasoning summary part is missing `type`".into(),
                        ));
                    }
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(index) = self.tool_index(&event) {
                    self.close_tool(index, &mut out);
                }
            }
            "response.in_progress" | "response.queued" => {
                // Progress frames restate the response object, model included;
                // the change-dedup keeps them quiet unless a value moved.
                if let Some(response) = event.get("response") {
                    self.observe_declared(response, &mut out);
                }
            }
            "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_text.done"
            | "response.reasoning_summary_text.done" => {}
            _ => {
                // Log only the sanitized event type, never prompt/response
                // payload. A generic client error must still be diagnosable.
                tracing::warn!(event_type = %event_type.chars().take(64).filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_')).collect::<String>(),
                    "unsupported Responses stream event");
                return Err(unsupported_upstream_content("Responses stream event"));
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        // A stream cut off after tool deltas still owes each open tool its
        // block stop, so the client's block stream is well formed — but no
        // `Stop` is synthesized: this body never named a terminal.
        let mut indices: Vec<u32> = self.tool_indices.values().copied().collect();
        indices.sort_unstable();
        indices.dedup();
        for index in indices {
            self.close_tool(index, &mut out);
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

/// Whether a terminal Responses payload contains at least one function call.
fn response_has_tool_call(response: Option<&Value>) -> bool {
    response
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        })
        .unwrap_or(false)
}
