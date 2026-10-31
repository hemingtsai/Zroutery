//! Decoding an inbound Anthropic Messages request into the IR.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, Message, Role, SystemPart, ThinkingConfig,
    ToolChoice, ToolDef, ToolResultPart,
};
use crate::protocol::unsupported_content;

use super::KNOWN_KEYS;

// ---------------------------------------------------------------- request in

pub fn decode_request(body: Value) -> Result<ChatRequest> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::invalid("request body must be a JSON object"))?;

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("`model` is required"))?;

    let mut req = ChatRequest::new(model, Dialect::Anthropic);
    req.anthropic_body = Some(body.clone());

    req.system = match obj.get("system") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![SystemPart::new(s.clone())],
        Some(Value::Array(items)) => items
            .iter()
            .map(|block| {
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("system content block is missing `type`"))?;
                if kind != "text" {
                    return Err(unsupported_content("system", Some(kind)));
                }
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("system text block is missing `text`"))?;
                // A breakpoint on a system block is the caller's decision about
                // money, so it travels with the text.
                Ok(SystemPart {
                    text: text.to_string(),
                    cache_control: block.get("cache_control").cloned(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        Some(_) => return Err(Error::invalid("`system` must be a string or an array")),
    };

    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("`messages` is required"))?;
    for m in messages {
        // Current Claude Code includes textual system entries mid-transcript.
        // Preserve their position; do not hoist a reminder before earlier turns.
        if matches!(
            m.get("role").and_then(Value::as_str),
            Some("system") | Some("developer")
        ) {
            let parts = match m.get("content") {
                Some(Value::String(text)) => vec![SystemPart::new(text)],
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .map(|block| {
                        if block.get("type").and_then(Value::as_str) != Some("text") {
                            return Err(Error::invalid("system messages only support text blocks"));
                        }
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("system text is missing"))?;
                        Ok(SystemPart {
                            text: text.into(),
                            cache_control: block.get("cache_control").cloned(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                None | Some(Value::Null) => Vec::new(),
                _ => return Err(Error::invalid("system message content must be text")),
            };
            req.messages.push(Message {
                role: Role::System,
                content: parts
                    .into_iter()
                    .map(|part| ContentBlock::Text {
                        text: part.text,
                        cache_control: part.cache_control,
                    })
                    .collect(),
            });
            continue;
        }
        req.messages.push(decode_message(m)?);
    }

    req.max_tokens = obj
        .get("max_tokens")
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.temperature = obj.get("temperature").and_then(Value::as_f64);
    req.top_p = obj.get("top_p").and_then(Value::as_f64);
    req.top_k = obj
        .get("top_k")
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32);
    req.stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if let Some(stops) = obj.get("stop_sequences") {
        match stops {
            Value::Null => {}
            Value::Array(values) => {
                req.stop_sequences = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| Error::invalid("stop sequence entries must be strings"))
                    })
                    .collect::<Result<Vec<_>>>()?;
            }
            _ => return Err(Error::invalid("`stop_sequences` must be an array")),
        }
    }

    match obj.get("tools") {
        None | Some(Value::Null) => {}
        Some(Value::Array(tools)) => {
            for t in tools {
                let name = t
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("tool definition is missing `name`"))?;
                req.tools.push(ToolDef {
                    name: name.to_string(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_schema: t
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                    cache_control: t.get("cache_control").cloned(),
                });
            }
        }
        Some(_) => return Err(Error::invalid("`tools` must be an array")),
    }

    let mut parallel_tool_use = None;
    req.tool_choice = match obj.get("tool_choice") {
        None | Some(Value::Null) => None,
        Some(tc) => {
            let kind = tc
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_choice is missing `type`"))?;
            match tc.get("disable_parallel_tool_use") {
                None | Some(Value::Null) => {}
                Some(Value::Bool(disabled)) => parallel_tool_use = Some(!disabled),
                Some(_) => {
                    return Err(Error::invalid(
                        "`disable_parallel_tool_use` must be a boolean",
                    ));
                }
            }
            match kind {
                "auto" => Some(ToolChoice::Auto),
                "any" => Some(ToolChoice::Any),
                "none" => Some(ToolChoice::None),
                "tool" => {
                    let name = tc
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid("tool_choice is missing `name`"))?;
                    Some(ToolChoice::Specific {
                        name: name.to_string(),
                    })
                }
                other => return Err(unsupported_content("tool_choice", Some(other))),
            }
        }
    };
    req.parallel_tool_use = parallel_tool_use;

    if let Some(th) = obj.get("thinking") {
        let kind = th
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("thinking is missing `type`"))?;
        let enabled = match kind {
            "enabled" | "adaptive" => true,
            "disabled" => false,
            other => return Err(unsupported_content("thinking", Some(other))),
        };
        req.thinking = Some(ThinkingConfig {
            enabled,
            budget_tokens: th
                .get("budget_tokens")
                .and_then(Value::as_u64)
                .map(|v| v.min(u32::MAX as u64) as u32),
        });
        req.reasoning = if !enabled {
            Some(crate::ir::ReasoningRequest::Disabled)
        } else {
            req.thinking
                .and_then(|thinking| thinking.budget_tokens)
                .map(|tokens| crate::ir::ReasoningRequest::TokenBudget { tokens })
        };
    }

    req.metadata_user = obj
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(effort) = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
    {
        let reasoning = crate::ir::ReasoningRequest::native(effort).map_err(Error::invalid)?;
        req.thinking = Some(reasoning.legacy());
        req.reasoning = Some(reasoning);
    }

    for (k, v) in obj {
        if !KNOWN_KEYS.contains(&k.as_str()) {
            req.passthrough.insert(k.clone(), v.clone());
        }
    }

    req.refresh_required_capabilities();
    Ok(req)
}

fn decode_message(m: &Value) -> Result<Message> {
    let role = match m.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some(other) => return Err(unsupported_content("message role", Some(other))),
        None => return Err(Error::invalid("message is missing `role`")),
    };
    let content = match m.get("content") {
        Some(Value::String(s)) => vec![ContentBlock::text(s.clone())],
        Some(Value::Array(blocks)) => {
            let mut out = Vec::with_capacity(blocks.len());
            for b in blocks {
                if let Some(block) = decode_block(b)? {
                    out.push(block);
                }
            }
            out
        }
        Some(Value::Null) | None => Vec::new(),
        Some(_) => return Err(Error::invalid("`content` must be a string or an array")),
    };
    Ok(Message { role, content })
}

pub(super) fn decode_block(b: &Value) -> Result<Option<ContentBlock>> {
    let kind = b
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("content block is missing `type`"))?;
    let block = match kind {
        "text" => ContentBlock::Text {
            text: b
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("text block is missing `text`"))?
                .to_string(),
            cache_control: b.get("cache_control").cloned(),
        },
        "thinking" => ContentBlock::Thinking {
            text: b
                .get("thinking")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("thinking block is missing `thinking`"))?
                .to_string(),
            signature: b
                .get("signature")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "redacted_thinking" => ContentBlock::RedactedThinking {
            data: b
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("redacted_thinking block is missing `data`"))?
                .to_string(),
        },
        "image" => ContentBlock::Image {
            source: decode_source(b.get("source"))?,
        },
        "document" => ContentBlock::Document {
            source: decode_source(b.get("source"))?,
        },
        "tool_use" => ContentBlock::ToolUse {
            id: b
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_use block is missing `id`"))?
                .to_string(),
            name: b
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_use block is missing `name`"))?
                .to_string(),
            input: b.get("input").cloned().unwrap_or_else(|| json!({})),
        },
        "tool_result" => {
            let tool_use_id = b
                .get("tool_use_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("tool_result block is missing `tool_use_id`"))?
                .to_string();
            let content = match b.get("content") {
                Some(Value::String(s)) => vec![ToolResultPart::Text { text: s.clone() }],
                Some(Value::Array(parts)) => parts
                    .iter()
                    .map(|part| {
                        let part_kind = part
                            .get("type")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("tool result part is missing `type`"))?;
                        match part_kind {
                            "text" => Ok(ToolResultPart::Text {
                                text: part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        Error::invalid("tool result text part is missing `text`")
                                    })?
                                    .to_string(),
                            }),
                            "image" => Ok(ToolResultPart::Image {
                                source: decode_source(part.get("source"))?,
                            }),
                            other => Err(unsupported_content("tool result", Some(other))),
                        }
                    })
                    .collect::<Result<Vec<_>>>()?,
                None | Some(Value::Null) => Vec::new(),
                Some(_) => {
                    return Err(Error::invalid(
                        "tool_result content must be a string or an array",
                    ));
                }
            };
            ContentBlock::ToolResult {
                tool_use_id,
                name: String::new(),
                content,
                is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }
        }
        other => return Err(unsupported_content("message", Some(other))),
    };
    Ok(Some(block))
}

fn decode_source(source: Option<&Value>) -> Result<MediaSource> {
    let s = source.ok_or_else(|| Error::invalid("media block is missing `source`"))?;
    let kind = s
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("media source is missing `type`"))?;
    match kind {
        "base64" => Ok(MediaSource::Base64 {
            media_type: s
                .get("media_type")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| Error::invalid("base64 media source is missing `media_type`"))?
                .to_string(),
            data: s
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("base64 media source is missing `data`"))?
                .to_string(),
        }),
        "url" => Ok(MediaSource::Url {
            url: s
                .get("url")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| Error::invalid("url media source is missing `url`"))?
                .to_string(),
        }),
        other => Err(unsupported_content("media source", Some(other))),
    }
}
