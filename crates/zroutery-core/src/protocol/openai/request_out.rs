//! Outbound half of the Chat Completions dialect: canonical IR in, a body out.

use serde_json::{json, Map, Value};

use super::request_in::arguments_json;
use crate::error::Result;
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, Message, Role, StructuredOutput, SystemPart,
    ToolChoice, ToolResultPart, UnsupportedContentPolicy, DEFAULT_STRUCTURED_OUTPUT_SCHEMA_NAME,
};
use crate::protocol::apply_content_policy;
use crate::protocol::reasoning_bridge;
use crate::protocol::ProviderQuirks;

// --------------------------------------------------------------- request out

/// The OpenAI dialect has no block structure on the system prompt, so the
/// parts are flattened back to text. A `cache_control` marker on a part is an
/// Anthropic concept and has no wire representation here; it simply does not
/// survive this direction.
fn system_text(system: &[SystemPart]) -> String {
    system
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Render an IR structured-output constraint as OpenAI `response_format`.
fn encode_response_format(structured: &StructuredOutput) -> Value {
    match structured {
        StructuredOutput::JsonObject => json!({"type": "json_object"}),
        StructuredOutput::JsonSchema {
            name,
            schema,
            strict,
        } => json!({
            "type": "json_schema",
            "json_schema": {
                "name": name
                    .as_deref()
                    .unwrap_or(DEFAULT_STRUCTURED_OUTPUT_SCHEMA_NAME),
                "schema": schema,
                "strict": strict,
            },
        }),
    }
}

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    encode_request_with(req, upstream_model, &ProviderQuirks::default())
}

pub fn encode_request_with(
    req: &ChatRequest,
    upstream_model: &str,
    quirks: &ProviderQuirks,
) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));

    let mut messages: Vec<Value> = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({
            "role": if quirks.system_as_developer { "developer" } else { "system" },
            "content": system_text(&req.system),
        }));
    }
    for m in &req.messages {
        encode_message_into(
            m,
            &mut messages,
            req.source_dialect == Dialect::OpenAI,
            req.unsupported_content_policy,
        )?;
    }
    body.insert("messages".into(), Value::Array(messages));

    if let Some(mt) = req.max_tokens {
        let field = if quirks.use_max_completion_tokens {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(field.into(), json!(mt));
    }
    if let Some(t) = req.temperature {
        if !quirks.drop_temperature {
            body.insert("temperature".into(), json!(t));
        }
    }
    if let Some(p) = req.top_p {
        if !quirks.drop_top_p {
            body.insert("top_p".into(), json!(p));
        }
    }
    if !req.stop_sequences.is_empty() && !quirks.drop_stop {
        body.insert("stop".into(), json!(req.stop_sequences));
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
        if quirks.stream_usage {
            body.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        let mut function = Map::new();
                        function.insert("name".into(), json!(t.name));
                        function.insert(
                            "description".into(),
                            json!(t.description.clone().unwrap_or_default()),
                        );
                        function.insert("parameters".into(), t.input_schema.clone());
                        // `strict` is part of the function definition, not a
                        // vendor extension: dropping it would turn "obey this
                        // schema exactly" into an ordinary tool.
                        if let Some(strict) = req.tool_strict.get(&t.name) {
                            function.insert("strict".into(), json!(strict));
                        }
                        json!({"type": "function", "function": Value::Object(function)})
                    })
                    .collect(),
            ),
        );
    }
    if let Some(tc) = &req.tool_choice {
        body.insert(
            "tool_choice".into(),
            match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Any => json!("required"),
                ToolChoice::Specific { name } => {
                    json!({"type": "function", "function": {"name": name}})
                }
            },
        );
    }
    if let Some(allow) = req.parallel_tool_use {
        body.insert("parallel_tool_calls".into(), json!(allow));
    }
    if let Some(th) = &req.thinking {
        if quirks.send_reasoning_effort {
            body.insert(
                "reasoning_effort".into(),
                json!(reasoning_bridge::encode_effort(th)),
            );
        }
    }
    if let Some(u) = &req.metadata_user {
        body.insert("user".into(), json!(u));
    }

    // Emit the structured-output constraint from the IR. This is what makes an
    // OpenAI -> OpenAI round trip keep `response_format` even though it is no
    // longer riding along in `passthrough`.
    if let Some(structured) = &req.structured_output {
        body.insert("response_format".into(), encode_response_format(structured));
    }

    if req.source_dialect == Dialect::OpenAI {
        for (k, v) in &req.passthrough {
            body.entry(k.clone()).or_insert(v.clone());
        }
    }

    Ok(Value::Object(body))
}

/// Anthropic keeps tool results inside user messages; OpenAI needs separate
/// `role: "tool"` messages, and they must come before any plain user text.
fn encode_message_into(
    m: &Message,
    out: &mut Vec<Value>,
    echo_reasoning: bool,
    policy: UnsupportedContentPolicy,
) -> Result<()> {
    match m.role {
        Role::Assistant => {
            let mut text = String::new();
            let mut reasoning = String::new();
            let mut reasoning_items: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t, .. } => text.push_str(t),
                    ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments_json(input)},
                    })),
                    // Reasoning history is echoed only when the source dialect
                    // supports it.  Otherwise it follows the same explicit
                    // unsupported-content policy as every other block; it is
                    // never silently discarded.
                    ContentBlock::Thinking { text: t, signature } => {
                        if echo_reasoning {
                            if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                                reasoning_items.push(item);
                            } else {
                                reasoning.push_str(t);
                            }
                            let _ = signature;
                        } else if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(replacement_text) = replacement.as_text() {
                                text.push_str(replacement_text);
                            }
                        }
                    }
                    ContentBlock::RedactedThinking { .. } => {
                        if echo_reasoning {
                            if let Some(item) = reasoning_bridge::encode_thinking_block(b) {
                                reasoning_items.push(item);
                            }
                        } else if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(replacement_text) = replacement.as_text() {
                                text.push_str(replacement_text);
                            }
                        }
                    }
                    // Media and structured content that cannot be represented in
                    // an assistant turn follows the explicit policy.
                    ContentBlock::Audio { .. }
                    | ContentBlock::Document { .. }
                    | ContentBlock::File { .. }
                    | ContentBlock::Video { .. }
                    | ContentBlock::Image { .. }
                    | ContentBlock::ToolResult { .. }
                    | ContentBlock::Citation { .. }
                    | ContentBlock::Annotation { .. } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                text.push_str(t);
                            }
                        }
                    }
                }
            }
            let mut msg = Map::new();
            msg.insert("role".into(), json!("assistant"));
            if text.is_empty() && !tool_calls.is_empty() {
                msg.insert("content".into(), Value::Null);
            } else {
                msg.insert("content".into(), json!(text));
            }
            if !reasoning.is_empty() {
                msg.insert("reasoning_content".into(), json!(reasoning));
            }
            if !reasoning_items.is_empty() {
                msg.insert("reasoning".into(), Value::Array(reasoning_items));
            }
            if !tool_calls.is_empty() {
                msg.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            out.push(Value::Object(msg));
        }
        Role::User => {
            let mut parts: Vec<Value> = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        let mut text_parts = Vec::new();
                        for part in content {
                            match part {
                                ToolResultPart::Text { text } => text_parts.push(text.clone()),
                                ToolResultPart::Image { source } => {
                                    let image = ContentBlock::Image {
                                        source: source.clone(),
                                    };
                                    if let Some(replacement) = apply_content_policy(policy, &image)?
                                    {
                                        if let Some(text) = replacement.as_text() {
                                            text_parts.push(text.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": text_parts.join("\n"),
                        }));
                    }
                    ContentBlock::Text { text, .. } => {
                        parts.push(json!({"type": "text", "text": text}))
                    }
                    ContentBlock::Image {
                        source: MediaSource::Reference { .. },
                    } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"type": "text", "text": t}));
                            }
                        }
                    }
                    ContentBlock::Image { source } => parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": source.to_data_url()},
                    })),
                    ContentBlock::Audio { source, media_type } => {
                        let format = crate::protocol::normalize_audio_format(media_type);
                        match source {
                            MediaSource::Base64 { data, .. } => {
                                parts.push(json!({
                                    "type": "input_audio",
                                    "input_audio": {"data": data, "format": format},
                                }));
                            }
                            MediaSource::Url { .. } | MediaSource::Reference { .. } => {
                                // URL audio cannot be represented as input_audio;
                                // apply the policy.
                                if let Some(replacement) = apply_content_policy(policy, b)? {
                                    if let Some(t) = replacement.as_text() {
                                        parts.push(json!({"type": "text", "text": t}));
                                    }
                                }
                            }
                        }
                    }
                    // An input file is representable in the official dialect,
                    // so a same-protocol request carries it instead of
                    // rejecting content the upstream understands.
                    ContentBlock::File {
                        source: MediaSource::Reference { id },
                        ..
                    } => parts.push(json!({
                        "type": "file",
                        "file": {"file_id": id},
                    })),
                    ContentBlock::File {
                        source: source @ MediaSource::Base64 { .. },
                        name,
                        ..
                    } => {
                        let mut file = Map::new();
                        file.insert("file_data".into(), json!(source.to_data_url()));
                        if let Some(name) = name {
                            file.insert("filename".into(), json!(name));
                        }
                        parts.push(json!({"type": "file", "file": file}));
                    }
                    ContentBlock::Document { .. }
                    | ContentBlock::File { .. }
                    | ContentBlock::Video { .. }
                    | ContentBlock::Citation { .. }
                    | ContentBlock::Annotation { .. }
                    | ContentBlock::ToolUse { .. }
                    | ContentBlock::Thinking { .. }
                    | ContentBlock::RedactedThinking { .. } => {
                        if let Some(replacement) = apply_content_policy(policy, b)? {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"type": "text", "text": t}));
                            }
                        }
                    }
                }
            }
            if !parts.is_empty() {
                // Keep the simple string form when there is only text.
                let all_text = parts.iter().all(|p| p["type"] == "text");
                let content = if all_text {
                    json!(parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(""))
                } else {
                    Value::Array(parts)
                };
                out.push(json!({"role": "user", "content": content}));
            }
        }
    }
    Ok(())
}
