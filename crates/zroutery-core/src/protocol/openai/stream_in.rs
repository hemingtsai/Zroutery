//! The inbound SSE parser, which synthesises Anthropic block indices from chunks that carry none.

use serde_json::Value;

use super::request_in::flatten_content;
use super::response::{decode_usage, finish_reason_from_str};
use crate::error::{Error, Result};
use crate::ir::{StopReason, StreamEvent, Usage};

use crate::protocol::{SseFrame, StreamParser};

// ---------------------------------------------------------------- stream in

/// The kinds of block that stream as a single running block. Tool blocks are
/// not part of this enum: several of them can be open at the same time.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Block {
    Text,
    Thinking,
}

/// Rebuilds Anthropic style block structure from OpenAI chunks.
pub struct OpenAiStreamParser {
    model: String,
    id: Option<String>,
    started: bool,
    next_index: u32,
    /// The open text or thinking block, if any.  Tool calls are tracked apart
    /// from it: OpenAI delivers every parallel call under its own
    /// `tool_calls[].index` with no per-call end marker, so opening a second
    /// call says nothing about whether the first one is still streaming.
    current: Option<(u32, Block)>,
    /// OpenAI `tool_calls[].index` -> canonical block index.
    tool_slots: Vec<(u64, u32)>,
    /// Canonical indices of tool blocks that were started and not yet stopped.
    open_tools: Vec<u32>,
    /// Text/thinking deltas that arrived while a tool block was open.  An
    /// Anthropic stream carries one block at a time, so these are replayed
    /// after every tool block has stopped instead of being interleaved.
    deferred: Vec<(Block, String)>,
    usage: Usage,
    pending_stop: Option<(StopReason, Option<String>)>,
    emitted_stop: bool,
    /// Whether the upstream really ended the answer (`finish_reason` or
    /// `[DONE]`). A body that just stops arriving is truncated, not finished.
    normal_terminal: bool,
}

impl OpenAiStreamParser {
    pub fn new(model: &str) -> Self {
        OpenAiStreamParser {
            model: model.to_string(),
            id: None,
            started: false,
            next_index: 0,
            current: None,
            tool_slots: Vec::new(),
            open_tools: Vec::new(),
            deferred: Vec::new(),
            usage: Usage::default(),
            pending_stop: None,
            emitted_stop: false,
            normal_terminal: false,
        }
    }

    fn close_current(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((index, _)) = self.current.take() {
            out.push(StreamEvent::BlockStop { index });
        }
    }

    /// Return the index of an open block of `kind`, opening a new one if needed.
    fn block_for(&mut self, kind: Block, out: &mut Vec<StreamEvent>) -> u32 {
        if let Some((index, current)) = self.current {
            if current == kind {
                return index;
            }
        }
        self.open_block(kind, out)
    }

    /// Start a fresh text/thinking block, closing whatever was open before.
    fn open_block(&mut self, kind: Block, out: &mut Vec<StreamEvent>) -> u32 {
        self.close_current(out);
        let index = self.next_index;
        self.next_index += 1;
        self.current = Some((index, kind));
        index
    }

    /// Start a fresh tool block.  It closes the open text/thinking block (an
    /// Anthropic stream never interleaves them) but deliberately leaves every
    /// other tool block open until the stream ends.
    fn open_tool_block(&mut self, out: &mut Vec<StreamEvent>) -> u32 {
        self.close_current(out);
        let index = self.next_index;
        self.next_index += 1;
        self.open_tools.push(index);
        index
    }

    /// Emit a text or thinking delta.  While a tool block is open the content
    /// is buffered so the emitted lifecycle stays one-block-at-a-time.
    fn push_text_delta(&mut self, kind: Block, text: String, out: &mut Vec<StreamEvent>) {
        if self.open_tools.is_empty() {
            let index = self.block_for(kind, out);
            out.push(match kind {
                Block::Thinking => StreamEvent::ThinkingDelta { index, text },
                _ => StreamEvent::TextDelta { index, text },
            });
        } else {
            match self.deferred.last_mut() {
                Some((last, buffered)) if *last == kind => buffered.push_str(&text),
                _ => self.deferred.push((kind, text)),
            }
        }
    }

    /// Replay the buffered text/thinking as complete blocks.  Called once every
    /// tool block has stopped.
    fn flush_deferred(&mut self, out: &mut Vec<StreamEvent>) {
        for (kind, text) in std::mem::take(&mut self.deferred) {
            let index = self.open_block(kind, out);
            out.push(match kind {
                Block::Thinking => StreamEvent::ThinkingDelta { index, text },
                _ => StreamEvent::TextDelta { index, text },
            });
            self.close_current(out);
        }
    }

    fn ensure_started(&mut self, chunk: &Value, out: &mut Vec<StreamEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let id = chunk
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl_stream")
            .to_string();
        if let Some(m) = chunk.get("model").and_then(Value::as_str) {
            if !m.is_empty() {
                self.model = m.to_string();
            }
        }
        self.id = Some(id.clone());
        out.push(StreamEvent::Start {
            id,
            model: self.model.clone(),
            usage: self.usage,
        });
    }

    fn emit_stop(&mut self, out: &mut Vec<StreamEvent>) {
        if self.emitted_stop {
            return;
        }
        let (stop_reason, stop_sequence) = self
            .pending_stop
            .take()
            .unwrap_or((StopReason::Unknown, None));
        self.close_current(out);
        // Parallel calls stay open until here: their argument deltas may
        // arrive in any order, so no earlier point proves a call is finished.
        for index in std::mem::take(&mut self.open_tools) {
            out.push(StreamEvent::BlockStop { index });
        }
        self.flush_deferred(out);
        out.push(StreamEvent::Stop {
            stop_reason,
            stop_sequence,
            usage: self.usage,
        });
        self.emitted_stop = true;
    }
}

impl StreamParser for OpenAiStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        let data = frame.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            let mut out = Vec::new();
            if self.started {
                self.normal_terminal = true;
                self.emit_stop(&mut out);
            }
            return Ok(out);
        }

        let chunk = frame.json()?;
        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream stream error");
            return Err(Error::BadUpstreamPayload(msg.to_string()));
        }

        let mut out = Vec::new();
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            let parsed = decode_usage(Some(u));
            if parsed.total() > 0 {
                self.usage = parsed;
            }
        }

        let choices = chunk
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if choices.is_empty() {
            // Usage-only trailer: flush the stop we deferred.
            if self.pending_stop.is_some() {
                self.ensure_started(&chunk, &mut out);
                self.emit_stop(&mut out);
            }
            return Ok(out);
        }

        self.ensure_started(&chunk, &mut out);

        for choice in choices {
            // Only the first choice is supported; `n > 1` is not representable
            // in the Anthropic dialect.
            if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
                return Err(Error::BadUpstreamPayload(
                    "multiple response choices are not representable".into(),
                ));
            }
            let delta = choice.get("delta").or_else(|| choice.get("message"));

            if let Some(reasoning) =
                delta.and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
            {
                let text = match reasoning {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    _ => {
                        return Err(Error::BadUpstreamPayload(
                            "stream reasoning must be a string".into(),
                        ));
                    }
                };
                if !text.is_empty() {
                    self.push_text_delta(Block::Thinking, text, &mut out);
                }
            }

            if let Some(content) = delta.and_then(|d| d.get("content")) {
                // `content` is optional in the OpenAI delta shape, so an
                // explicit null is a legal chunk that merely carries no text
                // (role-only, tool-only and reasoning-only deltas all look
                // like this).  Only a genuinely wrong JSON type is an error,
                // and it is the upstream that produced it.
                let text = match content {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    Value::Array(_) => flatten_content(Some(content))
                        .map_err(|err| {
                            Error::BadUpstreamPayload(format!(
                                "stream content array is malformed: {err}"
                            ))
                        })?
                        .unwrap_or_default(),
                    _ => {
                        return Err(Error::BadUpstreamPayload(
                            "stream content must be a string, an array, or null".into(),
                        ))
                    }
                };
                if !text.is_empty() {
                    self.push_text_delta(Block::Text, text, &mut out);
                }
            }

            // `refusal` is a separate optional member of the delta shape, and
            // an explicit null means the turn carries no refusal text.
            if let Some(refusal) = delta
                .and_then(|d| d.get("refusal"))
                .and_then(Value::as_str)
                .filter(|refusal| !refusal.is_empty())
            {
                self.push_text_delta(Block::Text, refusal.to_string(), &mut out);
            }

            if let Some(calls) = delta
                .and_then(|d| d.get("tool_calls"))
                .and_then(Value::as_array)
            {
                for call in calls {
                    let slot = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let known = self
                        .tool_slots
                        .iter()
                        .find(|(s, _)| *s == slot)
                        .map(|(_, i)| *i);
                    let index = match known {
                        Some(i) => i,
                        None => {
                            let id = call.get("id").and_then(Value::as_str).ok_or_else(|| {
                                Error::BadUpstreamPayload("stream tool call is missing `id`".into())
                            })?;
                            let name = call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    Error::BadUpstreamPayload(
                                        "stream tool call is missing function `name`".into(),
                                    )
                                })?;
                            let i = self.open_tool_block(&mut out);
                            self.tool_slots.push((slot, i));
                            out.push(StreamEvent::ToolUseStart {
                                index: i,
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                            i
                        }
                    };
                    if let Some(args) = call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        out.push(StreamEvent::ToolUseDelta {
                            index,
                            partial_json: args.to_string(),
                        });
                    }
                }
            }

            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
            {
                // Defer the Stop event: a trailing usage chunk may still arrive.
                self.pending_stop = Some((finish_reason_from_str(reason), None));
                self.normal_terminal = true;
            }
        }

        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if self.normal_terminal {
            self.emit_stop(&mut out);
            return out;
        }
        // The body ended without `finish_reason` or `[DONE]`: the answer was
        // cut off. Close what is open so the client's block stream stays
        // well-formed, but never synthesize the `Stop` that would claim the
        // answer really finished.
        self.close_current(&mut out);
        for index in std::mem::take(&mut self.open_tools) {
            out.push(StreamEvent::BlockStop { index });
        }
        self.flush_deferred(&mut out);
        out
    }

    fn saw_normal_terminal(&self) -> bool {
        self.normal_terminal
    }

    fn reported_usage(&self) -> Usage {
        self.usage
    }
}
