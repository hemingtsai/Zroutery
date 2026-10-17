//! Encoding the IR back into an Anthropic Messages request for the upstream.

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, Message, Role, ToolChoice, ToolResultPart,
    UnsupportedContentPolicy,
};
use crate::protocol::{apply_content_policy, explicit_placeholder};

use super::{DEFAULT_MAX_TOKENS, DEFAULT_THINKING_BUDGET, MIN_THINKING_BUDGET};

// --------------------------------------------------------------- request out

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));
    let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    body.insert("max_tokens".into(), json!(max_tokens));

    if !req.system.is_empty() {
        body.insert(
            "system".into(),
            Value::Array(
                req.system
                    .iter()
                    .map(|part| {
                        with_cache(
                            json!({"type": "text", "text": part.text}),
                            &part.cache_control,
                        )
                    })
                    .collect(),
            ),
        );
    }

    let messages: Result<Vec<Value>> = req
        .messages
        .iter()
        .map(|m| encode_message(m, req.unsupported_content_policy))
        .collect();
    body.insert("messages".into(), Value::Array(messages?));

    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.top_p {
        body.insert("top_p".into(), json!(p));
    }
    if let Some(k) = req.top_k {
        body.insert("top_k".into(), json!(k));
    }
    if !req.stop_sequences.is_empty() {
        body.insert("stop_sequences".into(), json!(req.stop_sequences));
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
    }
    if req.tool_strict.values().any(|strict| *strict) {
        // Anthropic tool definitions carry no strict-schema flag, so
        // forwarding the call would silently drop the client's requirement
        // that the model obey the schema exactly.
        return Err(Error::invalid(
            "strict tool schemas are not representable in the anthropic dialect",
        ));
    }
    if req.structured_output.is_some() {
        // Anthropic Messages has no structured-output constraint: neither a
        // JSON schema nor a plain json-object mode can be stated on the wire.
        // The `StructuredOutput` requirement also keeps candidates that cannot
        // honour the constraint out of the eligible set; this is the explicit
        // refusal for any path that reaches the encoder anyway, so the caller
        // never receives unconstrained prose as if the schema had been applied.
        return Err(Error::invalid(
            "structured output is not representable in the anthropic dialect",
        ));
    }
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        let mut m = Map::new();
                        m.insert("name".into(), json!(t.name));
                        if let Some(d) = &t.description {
                            m.insert("description".into(), json!(d));
                        }
                        m.insert("input_schema".into(), t.input_schema.clone());
                        with_cache(Value::Object(m), &t.cache_control)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(tc) = &req.tool_choice {
        let mut choice = Map::new();
        match tc {
            ToolChoice::Auto => choice.insert("type".into(), json!("auto")),
            ToolChoice::Any => choice.insert("type".into(), json!("any")),
            ToolChoice::None => choice.insert("type".into(), json!("none")),
            ToolChoice::Specific { name } => {
                choice.insert("type".into(), json!("tool"));
                choice.insert("name".into(), json!(name))
            }
        };
        // Anthropic states the concurrency limit inside the tool choice, so it
        // must be re-attached for every kind rather than dropped.
        if let Some(allow) = req.parallel_tool_use {
            choice.insert("disable_parallel_tool_use".into(), json!(!allow));
        }
        body.insert("tool_choice".into(), Value::Object(choice));
    } else if req.parallel_tool_use == Some(false) && !req.tools.is_empty() {
        // The restriction arrived without a tool choice (the OpenAI shape).
        // Anthropic's default choice is `auto`; state it and the limit.
        body.insert(
            "tool_choice".into(),
            json!({"type": "auto", "disable_parallel_tool_use": true}),
        );
    }
    if let Some(th) = &req.thinking {
        if th.enabled {
            let mut budget = th.budget_tokens.unwrap_or(DEFAULT_THINKING_BUDGET);
            if req.source_dialect != Dialect::Anthropic {
                // This budget was derived from another dialect's reasoning
                // knob, so it must obey the Anthropic constraint: at least
                // 1024 and strictly below max_tokens.  The client's own
                // max_tokens is the ceiling, so the budget is lowered rather
                // than the cap raised; when even the floor does not fit, the
                // request is refused instead of sent as an invalid body.
                budget = budget.min(max_tokens.saturating_sub(1));
                if budget < MIN_THINKING_BUDGET {
                    return Err(Error::invalid(format!(
                        "thinking budget cannot satisfy \
                         1024 <= budget_tokens < max_tokens ({max_tokens})"
                    )));
                }
            }
            body.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
        }
    }
    if let Some(u) = &req.metadata_user {
        body.insert("metadata".into(), json!({"user_id": u}));
    }

    // Vendor extensions are only forwarded within the same dialect.
    if req.source_dialect == Dialect::Anthropic {
        for (k, v) in &req.passthrough {
            body.entry(k.clone()).or_insert(v.clone());
        }
    }

    Ok(Value::Object(body))
}

fn encode_message(m: &Message, policy: UnsupportedContentPolicy) -> Result<Value> {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut content = Vec::new();
    for b in &m.content {
        match b {
            // Types not natively supported by Anthropic: apply the policy.
            ContentBlock::File { .. }
            | ContentBlock::Audio { .. }
            | ContentBlock::Video { .. }
            | ContentBlock::Citation { .. }
            | ContentBlock::Annotation { .. } => {
                if let Some(replacement) = apply_content_policy(policy, b)? {
                    content.push(encode_block(&replacement));
                }
            }
            // Image/Document with Reference source cannot be encoded natively
            // by Anthropic; apply the content policy.
            ContentBlock::Image {
                source: MediaSource::Reference { .. },
            }
            | ContentBlock::Document {
                source: MediaSource::Reference { .. },
            } => {
                if let Some(replacement) = apply_content_policy(policy, b)? {
                    content.push(encode_block(&replacement));
                }
            }
            _ => content.push(encode_block(b)),
        }
    }
    Ok(json!({
        "role": role,
        "content": Value::Array(content),
    }))
}

pub(crate) fn encode_block(b: &ContentBlock) -> Value {
    match b {
        ContentBlock::Text {
            text,
            cache_control,
        } => with_cache(json!({"type": "text", "text": text}), cache_control),
        ContentBlock::Thinking { text, signature } => {
            let mut m = Map::new();
            m.insert("type".into(), json!("thinking"));
            m.insert("thinking".into(), json!(text));
            m.insert(
                "signature".into(),
                json!(signature.clone().unwrap_or_default()),
            );
            Value::Object(m)
        }
        ContentBlock::RedactedThinking { data } => {
            json!({"type": "redacted_thinking", "data": data})
        }
        ContentBlock::Image { source } => json!({"type": "image", "source": encode_source(source)}),
        ContentBlock::Document { source } => {
            json!({"type": "document", "source": encode_source(source)})
        }
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => {
            let parts: Vec<Value> = content
                .iter()
                .map(|p| match p {
                    ToolResultPart::Text { text } => json!({"type": "text", "text": text}),
                    ToolResultPart::Image { source } => {
                        json!({"type": "image", "source": encode_source(source)})
                    }
                })
                .collect();
            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": parts,
                "is_error": is_error,
            })
        }
        // Response encoding has no request policy context; make an explicit
        // visible replacement instead of emitting a null block.
        ContentBlock::File { .. }
        | ContentBlock::Audio { .. }
        | ContentBlock::Video { .. }
        | ContentBlock::Citation { .. }
        | ContentBlock::Annotation { .. } => encode_block(&explicit_placeholder(b)),
    }
}

/// Put a `cache_control` marker back on an encoded block.
///
/// A prompt cache breakpoint is the caller's decision about money, so it travels
/// through the proxy untouched rather than being re-derived or quietly dropped.
fn with_cache(mut value: Value, cache_control: &Option<Value>) -> Value {
    if let (Some(object), Some(marker)) = (value.as_object_mut(), cache_control) {
        object.insert("cache_control".into(), marker.clone());
    }
    value
}

fn encode_source(s: &MediaSource) -> Value {
    match s {
        MediaSource::Base64 { media_type, data } => {
            json!({"type": "base64", "media_type": media_type, "data": data})
        }
        MediaSource::Url { url } => json!({"type": "url", "url": url}),
        MediaSource::Reference { id } => json!({"type": "url", "url": id}),
    }
}
