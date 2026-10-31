//! Encoding the IR back into a Gemini generateContent request.

use serde_json::{json, Map, Value};

use super::request_in::gemini_schema_from_json;

use crate::error::Result;
use crate::ir::{
    ChatRequest, ContentBlock, MediaSource, Role, StructuredOutput, ToolChoice, ToolResultPart,
};
use crate::protocol::apply_content_policy;
// --------------------------------------------------------------- request out

pub fn encode_request(req: &ChatRequest, upstream_model: &str) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(upstream_model));

    if !req.system.is_empty() {
        body.insert(
            "system_instruction".into(),
            json!({"parts": req.system.iter().map(|s| json!({"text": s.text})).collect::<Vec<_>>()}),
        );
    }

    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "model",
            Role::System => {
                return Err(crate::error::Error::invalid(
                    "Gemini cannot preserve mid-conversation system instructions",
                ))
            }
        };
        let mut parts: Vec<Value> = Vec::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text, .. } => parts.push(json!({"text": text})),
                ContentBlock::Image { source } => match source {
                    MediaSource::Base64 { media_type, data } => parts.push(json!({
                        "inlineData": {"mimeType": media_type, "data": data}
                    })),
                    MediaSource::Url { .. } => {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(text) = replacement.as_text() {
                                parts.push(json!({"text": text}));
                            }
                        }
                    }
                    MediaSource::Reference { .. } => {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(text) = replacement.as_text() {
                                parts.push(json!({"text": text}));
                            }
                        }
                    }
                },
                ContentBlock::ToolUse { id, name, input } => parts.push(json!({
                    "functionCall": {"id": id, "name": name, "args": input}
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    name,
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
                                if let Some(replacement) =
                                    apply_content_policy(req.unsupported_content_policy, &image)?
                                {
                                    if let Some(text) = replacement.as_text() {
                                        text_parts.push(text.to_string());
                                    }
                                }
                            }
                        }
                    }
                    parts.push(json!({
                        "functionResponse": {"id": tool_use_id, "name": name, "response": {"text": text_parts.join("\n")}}
                    }));
                }
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                    if let Some(replacement) =
                        apply_content_policy(req.unsupported_content_policy, b)?
                    {
                        if let Some(text) = replacement.as_text() {
                            parts.push(json!({"text": text}));
                        }
                    }
                }
                // Gemini supports inlineData for any MIME type.
                ContentBlock::Document { source } => match source {
                    MediaSource::Base64 { media_type, data } => parts.push(json!({
                        "inlineData": {"mimeType": media_type, "data": data}
                    })),
                    MediaSource::Url { .. } => {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"text": t}));
                            }
                        }
                    }
                    MediaSource::Reference { .. } => {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"text": t}));
                            }
                        }
                    }
                },
                ContentBlock::File {
                    source, media_type, ..
                }
                | ContentBlock::Audio {
                    source, media_type, ..
                }
                | ContentBlock::Video {
                    source, media_type, ..
                } => match source {
                    MediaSource::Base64 { data, .. } => parts.push(json!({
                        "inlineData": {"mimeType": media_type, "data": data}
                    })),
                    MediaSource::Url { .. } => {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"text": t}));
                            }
                        }
                    }
                    MediaSource::Reference { .. } => {
                        if let Some(replacement) =
                            apply_content_policy(req.unsupported_content_policy, b)?
                        {
                            if let Some(t) = replacement.as_text() {
                                parts.push(json!({"text": t}));
                            }
                        }
                    }
                },
                // Citation and Annotation have no Gemini equivalent.
                ContentBlock::Citation { .. } | ContentBlock::Annotation { .. } => {
                    if let Some(replacement) =
                        apply_content_policy(req.unsupported_content_policy, b)?
                    {
                        if let Some(t) = replacement.as_text() {
                            parts.push(json!({"text": t}));
                        }
                    }
                }
            }
        }
        contents.push(json!({"role": role, "parts": parts}));
    }
    body.insert("contents".into(), Value::Array(contents));

    let mut gc = Map::new();
    if let Some(mt) = req.max_tokens {
        gc.insert("maxOutputTokens".into(), json!(mt));
    }
    if let Some(t) = req.temperature {
        gc.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.top_p {
        gc.insert("topP".into(), json!(p));
    }
    if !req.stop_sequences.is_empty() {
        gc.insert("stopSequences".into(), json!(req.stop_sequences));
    }
    if let Some(crate::ir::ReasoningRequest::NativeEffort { value }) = &req.reasoning {
        gc.insert("thinkingConfig".into(), json!({"thinkingLevel": value}));
    } else if let Some(thinking) = &req.thinking {
        let mut config = Map::new();
        match (thinking.enabled, thinking.budget_tokens) {
            (true, Some(budget)) => {
                config.insert("thinkingBudget".into(), json!(budget));
            }
            // A zero budget is how Gemini is told to think less than its
            // default, including not at all.
            (false, _) => {
                config.insert("thinkingBudget".into(), json!(0));
            }
            // Enabled with no explicit budget asks for the provider default,
            // which an empty thinking config expresses.
            (true, None) => {}
        }
        gc.insert("thinkingConfig".into(), Value::Object(config));
    }
    // Structured output is a `generationConfig` constraint; emitting it from
    // the IR is what keeps an OpenAI -> Gemini translation from losing it.
    if let Some(structured) = &req.structured_output {
        gc.insert("responseMimeType".into(), json!("application/json"));
        if let StructuredOutput::JsonSchema { schema, .. } = structured {
            gc.insert("responseSchema".into(), gemini_schema_from_json(schema));
        }
    }
    if !gc.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gc));
    }

    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            json!([{
                "functionDeclarations": req.tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description.clone().unwrap_or_default(),
                    "parameters": t.input_schema,
                })).collect::<Vec<_>>()
            }]),
        );
    }

    if let Some(tc) = &req.tool_choice {
        let tool_config = match tc {
            ToolChoice::Auto => json!({"functionCallingConfig": {"mode": "AUTO"}}),
            ToolChoice::None => json!({"functionCallingConfig": {"mode": "NONE"}}),
            ToolChoice::Any => json!({"functionCallingConfig": {"mode": "ANY"}}),
            ToolChoice::Specific { name } => json!({
                "functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]}
            }),
        };
        body.insert("toolConfig".into(), tool_config);
    }

    Ok(Value::Object(body))
}
