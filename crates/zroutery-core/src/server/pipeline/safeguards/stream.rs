//! Collect only tool blocks from the actual client-facing frames. Text and
//! thinking continue live; hold only the tail until the real terminal arrives.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use super::{ReviewContext, ToolUse, Unavailable, MAX_TOOL_INPUT_BYTES, MAX_TOOL_USES};
use crate::error::{Error, Result};
use crate::protocol::SseFrame;

const MAX_TAIL_BYTES: usize = 512 * 1024;

pub(in crate::server::pipeline) struct ReviewFrames {
    context: Arc<ReviewContext>,
    tools: BTreeMap<u32, StreamedTool>,
    duplicate_tools: Vec<ToolUse>,
    last_block: Option<u32>,
    cut_off: bool,
    terminal_reason: Option<String>,
    require_streamed_arguments: bool,
    answered_upstream: bool,
    tail: Vec<SseFrame>,
    tail_bytes: usize,
    holding: bool,
    complete: bool,
    overflow: bool,
}

struct StreamedTool {
    tool: ToolUse,
    partial_json: String,
    closed: bool,
}

impl ReviewFrames {
    pub(in crate::server::pipeline) fn new(
        context: Arc<ReviewContext>,
        require_streamed_arguments: bool,
    ) -> Self {
        Self {
            context,
            tools: BTreeMap::new(),
            duplicate_tools: Vec::new(),
            last_block: None,
            cut_off: false,
            terminal_reason: None,
            require_streamed_arguments,
            answered_upstream: false,
            tail: Vec::new(),
            tail_bytes: 0,
            holding: false,
            complete: false,
            overflow: false,
        }
    }

    pub(in crate::server::pipeline) fn observe(
        &mut self,
        frames: Vec<SseFrame>,
    ) -> Result<Vec<SseFrame>> {
        let mut live = Vec::new();
        for frame in frames {
            let data = frame.json()?;
            let kind = frame
                .event
                .as_deref()
                .or_else(|| data.get("type").and_then(Value::as_str));
            self.answered_upstream |= [
                data.get("safeguard_results"),
                data.pointer("/delta/safeguard_results"),
                data.pointer("/message/safeguard_results"),
            ]
            .into_iter()
            .flatten()
            .any(|value| !value.is_null());
            let index = data
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| u32::try_from(index).ok());
            match kind {
                Some("error") => {
                    // A raw upstream error after stop_reason is still a failure,
                    // not a reviewed completion. Do not hide it in the tail.
                    self.tail.clear();
                    self.holding = false;
                }
                Some("content_block_start") => {
                    self.last_block = index;
                    if let (Some(index), Some(block)) = (index, data.get("content_block")) {
                        if let Some(tool) = ToolUse::from_block(block, false) {
                            if self.tools.len() >= MAX_TOOL_USES {
                                self.overflow = true;
                            } else {
                                if let Some(mut previous) = self.tools.remove(&index) {
                                    previous.tool.unavailable = Some(Unavailable::Error);
                                    if self.duplicate_tools.len() < MAX_TOOL_USES {
                                        self.duplicate_tools.push(previous.tool);
                                    } else {
                                        self.overflow = true;
                                    }
                                }
                                self.tools.insert(
                                    index,
                                    StreamedTool {
                                        tool,
                                        partial_json: String::new(),
                                        closed: false,
                                    },
                                );
                            }
                        }
                    }
                }
                Some("content_block_delta") => {
                    if data.pointer("/delta/type").and_then(Value::as_str)
                        == Some("input_json_delta")
                    {
                        if let Some(tool) = index.and_then(|index| self.tools.get_mut(&index)) {
                            if tool.closed {
                                tool.tool.unavailable = Some(Unavailable::Error);
                            }
                            if let Some(fragment) =
                                data.pointer("/delta/partial_json").and_then(Value::as_str)
                            {
                                if tool.partial_json.len().saturating_add(fragment.len())
                                    > MAX_TOOL_INPUT_BYTES
                                {
                                    tool.tool.unavailable = Some(Unavailable::InputTooLong);
                                    tool.partial_json.clear();
                                } else if tool.tool.unavailable.is_none() {
                                    tool.partial_json.push_str(fragment);
                                }
                            } else {
                                tool.tool.unavailable = Some(Unavailable::Truncated);
                            }
                        }
                    }
                }
                Some("content_block_stop") => {
                    if let Some(tool) = index.and_then(|index| self.tools.get_mut(&index)) {
                        tool.closed = true;
                    }
                }
                Some("message_delta")
                    if data
                        .pointer("/delta/stop_reason")
                        .is_some_and(|value| !value.is_null()) =>
                {
                    self.holding = true;
                    self.terminal_reason = data
                        .pointer("/delta/stop_reason")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    self.cut_off |= matches!(
                        data.pointer("/delta/stop_reason").and_then(Value::as_str),
                        Some("max_tokens" | "model_context_window_exceeded")
                    );
                }
                Some("message_stop") => {
                    self.holding = true;
                    self.complete = true;
                }
                _ => {}
            }
            if self.holding {
                self.tail_bytes = self.tail_bytes.saturating_add(frame.data.len());
                if self.tail_bytes > MAX_TAIL_BYTES {
                    return Err(Error::BadUpstreamPayload(
                        "Messages review terminal metadata exceeded its bound".into(),
                    ));
                }
                self.tail.push(frame);
            } else {
                live.push(frame);
            }
        }
        Ok(live)
    }

    pub(in crate::server::pipeline) fn complete(&self) -> bool {
        self.complete
    }
    pub(in crate::server::pipeline) fn answered_upstream(&self) -> bool {
        self.answered_upstream
    }

    pub(in crate::server::pipeline) fn evaluate(
        &mut self,
        state: Arc<super::super::AppState>,
        parent: String,
        pending: Option<crate::accounting::AttemptFact>,
    ) -> BoxFuture<'static, Value> {
        let mut tools = std::mem::take(&mut self.duplicate_tools);
        for (index, mut streamed) in std::mem::take(&mut self.tools) {
            if self.terminal_reason.as_deref() == Some("refusal") {
                streamed.tool.unavailable = Some(Unavailable::Refused);
            } else if !matches!(
                self.terminal_reason.as_deref(),
                Some(
                    "end_turn"
                        | "tool_use"
                        | "stop_sequence"
                        | "max_tokens"
                        | "model_context_window_exceeded"
                )
            ) {
                streamed.tool.unavailable = Some(Unavailable::Error);
            }
            if self.overflow {
                streamed.tool.unavailable = Some(Unavailable::InputTooLong);
            }
            if !streamed.closed || (self.cut_off && self.last_block == Some(index)) {
                streamed.tool.unavailable = Some(Unavailable::Truncated);
            }
            if streamed.tool.unavailable.is_none() && !streamed.partial_json.is_empty() {
                match serde_json::from_str::<Value>(&streamed.partial_json) {
                    Ok(value) if value.is_object() => streamed.tool.input = value,
                    _ => streamed.tool.unavailable = Some(Unavailable::Truncated),
                }
            } else if streamed.tool.unavailable.is_none() && self.require_streamed_arguments {
                streamed.tool.unavailable = Some(Unavailable::Truncated);
            }
            tools.push(streamed.tool);
        }
        let context = self.context.clone();
        Box::pin(async move { context.evaluate(state, tools, &parent, pending).await })
    }

    pub(in crate::server::pipeline) fn release(&mut self, results: Option<Value>) -> Vec<SseFrame> {
        let mut tail = std::mem::take(&mut self.tail);
        if let Some(results) = results {
            let target = tail.iter().rposition(|frame| {
                frame.event.as_deref() == Some("message_delta")
                    || frame.json().is_ok_and(|data| {
                        data.get("type").and_then(Value::as_str) == Some("message_delta")
                    })
            });
            if let Some(index) = target {
                if let Ok(mut data) = tail[index].json() {
                    data["delta"]["safeguard_results"] = results;
                    tail[index].data = data.to_string();
                }
            } else {
                let frame = SseFrame {
                    event: Some("message_delta".into()),
                    data: json!({"type":"message_delta", "delta":{"safeguard_results":results}})
                        .to_string(),
                };
                tail.insert(0, frame);
            }
        }
        tail
    }
}
