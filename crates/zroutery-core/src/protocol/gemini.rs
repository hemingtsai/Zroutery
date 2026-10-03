//! Google Gemini native API dialect (`generateContent`).
//!
//! This module provides a practical subset of the Gemini translation: system
//! instructions, multi-part contents, function declarations/calls, image parts,
//! and a snapshot-style SSE parser/encoder.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use super::apply_content_policy;
use super::{explicit_placeholder, unsupported_content, SseFrame, StreamEncoder, StreamParser};
use crate::error::{Error, Result};
use crate::ir::{
    classify_media, ChatRequest, ChatResponse, ContentBlock, Dialect, MediaSource, Message, Role,
    StopReason, StreamEvent, StructuredOutput, SystemPart, ThinkingConfig, ToolChoice, ToolDef,
    ToolResultPart, Usage,
};

// ---------------------------------------------------------------- request in

pub fn decode_request(body: Value) -> Result<ChatRequest> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::invalid("Gemini request body must be a JSON object"))?;
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("model is required"))?
        .to_string();
    let mut req = ChatRequest::new(model, Dialect::Gemini);

    // Google's canonical JSON spelling is camelCase `systemInstruction`; the
    // snake_case form is accepted as a compatibility alias.  Reading only the
    // alias silently discarded whatever the official SDKs sent.
    let instruction = match (
        obj.get("systemInstruction")
            .filter(|value| !value.is_null()),
        obj.get("system_instruction")
            .filter(|value| !value.is_null()),
    ) {
        (Some(_), Some(_)) => {
            return Err(Error::invalid(
                "send either `systemInstruction` or `system_instruction`, not both",
            ));
        }
        (Some(canonical), None) => Some(canonical),
        (None, alias) => alias,
    };
    if let Some(instruction) = instruction {
        let parts = instruction
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::invalid("system instruction is missing `parts`"))?;
        for part in parts {
            let text = part
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| unsupported_content("system", Some("non-text part")))?;
            req.system.push(SystemPart::new(text));
        }
    }

    let contents = match obj.get("contents") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(contents)) => contents.as_slice(),
        Some(_) => return Err(Error::invalid("`contents` must be an array")),
    };
    // Gemini leaves `functionCall.id` and `functionResponse.id` optional, but
    // the IR requires both. A call without an id is minted one; a response
    // without an id is paired with the oldest unanswered call of the same name,
    // since a history lists responses in the order of the calls they answer.
    // Any explicit id is kept as sent.
    let mut used_call_ids: HashSet<String> = HashSet::new();
    let mut unanswered_calls: Vec<(String, String)> = Vec::new();
    let mut minted_call_ids: u32 = 0;
    for content in contents {
        let role = match content.get("role").and_then(Value::as_str) {
            Some("model") => Role::Assistant,
            Some("user") | None => Role::User,
            Some(other) => return Err(unsupported_content("content role", Some(other))),
        };
        let mut blocks = Vec::new();
        if let Some(parts) = content.get("parts").and_then(Value::as_array) {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    blocks.push(ContentBlock::text(text));
                } else if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid("functionCall is missing `name`"))?;
                    let id = match call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                    {
                        Some(id) => id.to_string(),
                        None => mint_call_id(&mut minted_call_ids, &used_call_ids),
                    };
                    used_call_ids.insert(id.clone());
                    unanswered_calls.push((name.to_string(), id.clone()));
                    blocks.push(ContentBlock::ToolUse {
                        id,
                        name: name.to_string(),
                        input: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    });
                } else if let Some(response) = part.get("functionResponse") {
                    let name = response
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid("functionResponse is missing `name`"))?;
                    let id = match response
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                    {
                        Some(id) => {
                            unanswered_calls.retain(|(_, call_id)| call_id != id);
                            id.to_string()
                        }
                        None => {
                            match unanswered_calls
                                .iter()
                                .position(|(call_name, _)| call_name == name)
                            {
                                Some(position) => unanswered_calls.remove(position).1,
                                None => mint_call_id(&mut minted_call_ids, &used_call_ids),
                            }
                        }
                    };
                    used_call_ids.insert(id.clone());
                    blocks.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        name: name.to_string(),
                        content: vec![ToolResultPart::Text {
                            text: response.get("response").unwrap_or(&Value::Null).to_string(),
                        }],
                        is_error: false,
                    });
                } else if let Some(data) = part.get("inlineData") {
                    let mime = data
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| Error::invalid("inlineData is missing `mimeType`"))?;
                    let payload = data
                        .get("data")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid("inlineData is missing `data`"))?;
                    blocks.push(classify_media(
                        mime,
                        MediaSource::Base64 {
                            media_type: mime.to_string(),
                            data: payload.to_string(),
                        },
                        None,
                    ));
                } else {
                    return Err(unsupported_content("message", Some("unknown part")));
                }
            }
        }
        req.messages.push(Message {
            role,
            content: blocks,
        });
    }

    if let Some(gc) = obj.get("generationConfig") {
        req.max_tokens = gc
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32);
        req.temperature = gc.get("temperature").and_then(Value::as_f64);
        req.top_p = gc.get("topP").and_then(Value::as_f64);
        req.stop_sequences = match gc.get("stopSequences") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(values)) => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| Error::invalid("stop sequence entries must be strings"))
                })
                .collect::<Result<Vec<_>>>()?,
            Some(_) => return Err(Error::invalid("`stopSequences` must be an array")),
        };
        // Thinking configuration was received and then discarded, so a caller
        // that asked for a thinking budget silently got the provider default.
        if let Some(thinking) = gc.get("thinkingConfig").filter(|value| !value.is_null()) {
            req.thinking = Some(decode_thinking_config(thinking)?);
        }
        // `responseMimeType` / `responseSchema` are the Gemini structured-output
        // knobs. They were accepted and ignored, so a caller that required JSON
        // could still receive free-form text.
        req.structured_output =
            decode_structured_output(gc.get("responseMimeType"), gc.get("responseSchema"))?;
    }

    match obj.get("tools") {
        None | Some(Value::Null) => {}
        Some(Value::Array(tools)) => {
            for tool in tools {
                let decls = tool
                    .get("functionDeclarations")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        Error::invalid("Gemini tool is missing `functionDeclarations`")
                    })?;
                for decl in decls {
                    let name = decl
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid("function declaration is missing `name`"))?;
                    let parameters = decl.get("parameters").filter(|value| !value.is_null());
                    let json_schema = decl
                        .get("parametersJsonSchema")
                        .filter(|value| !value.is_null());
                    // The Gemini API defines the two schema fields as mutually
                    // exclusive; accepting both would have to guess which one
                    // the caller meant.
                    let input_schema = match (parameters, json_schema) {
                        (Some(_), Some(_)) => {
                            return Err(Error::invalid(
                                "function declaration sets both `parameters` and `parametersJsonSchema`",
                            ));
                        }
                        // `parametersJsonSchema` already carries a standard JSON
                        // Schema, so it is kept verbatim instead of being
                        // replaced by an empty object schema.  `parameters` is
                        // a Gemini `Schema` and is normalized below.
                        (Some(gemini_schema), None) => {
                            let mut schema = gemini_schema.clone();
                            normalize_gemini_schema(&mut schema);
                            schema
                        }
                        (None, Some(schema)) => schema.clone(),
                        (None, None) => json!({"type": "object"}),
                    };
                    req.tools.push(ToolDef {
                        name: name.to_string(),
                        description: decl
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        input_schema,
                        cache_control: None,
                    });
                }
            }
        }
        Some(_) => return Err(Error::invalid("`tools` must be an array")),
    }

    req.tool_choice = match obj
        .get("toolConfig")
        .and_then(|c| c.get("functionCallingConfig"))
    {
        None | Some(Value::Null) => None,
        Some(config) => {
            let mode = config
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("functionCallingConfig is missing `mode`"))?;
            match mode {
                "NONE" => Some(ToolChoice::None),
                // `allowedFunctionNames` is only meaningful for ANY mode, so
                // AUTO keeps the full declaration set exactly as Gemini does.
                "AUTO" => Some(ToolChoice::Auto),
                "ANY" => match decode_allowed_function_names(config)? {
                    None => Some(ToolChoice::Any),
                    // The IR cannot name several allowed functions at once, so
                    // narrow the declarations instead: silently dropping the
                    // restriction made an excluded function selectable again.
                    Some(allowed) => {
                        req.tools.retain(|tool| allowed.contains(&tool.name));
                        if allowed.len() == 1 {
                            Some(ToolChoice::Specific {
                                name: allowed[0].clone(),
                            })
                        } else {
                            Some(ToolChoice::Any)
                        }
                    }
                },
                other => return Err(unsupported_content("function calling mode", Some(other))),
            }
        }
    };

    req.refresh_required_capabilities();
    Ok(req)
}

/// Read `functionCallingConfig.allowedFunctionNames`.
///
/// `None` means the caller set no restriction. An explicitly empty list is
/// rejected rather than read as "no restriction", because widening a caller's
/// restriction back to the full tool set is the defect this guards against.
fn decode_allowed_function_names(config: &Value) -> Result<Option<Vec<String>>> {
    let names = match config.get("allowedFunctionNames") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(names)) => names,
        Some(_) => return Err(Error::invalid("`allowedFunctionNames` must be an array")),
    };
    let names = names
        .iter()
        .map(|name| {
            name.as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::invalid("allowedFunctionNames entries must be strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    if names.is_empty() {
        return Err(Error::invalid(
            "`allowedFunctionNames` cannot be empty when the mode is ANY",
        ));
    }
    Ok(Some(names))
}

/// Mint an id for a Gemini function call or response whose optional `id` was
/// omitted. The counter form keeps decoding deterministic, and `used` keeps a
/// minted id from colliding with one the client explicitly sent.
fn mint_call_id(counter: &mut u32, used: &HashSet<String>) -> String {
    loop {
        *counter += 1;
        let candidate = format!("gemini_call_{counter}");
        if !used.contains(&candidate) {
            return candidate;
        }
    }
}

/// Which spelling of the OpenAPI `Schema.type` enum a schema uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaTypeCase {
    /// Gemini `Schema` -> standard JSON Schema (`OBJECT` becomes `object`).
    Lower,
    /// Standard JSON Schema -> Gemini `Schema` (`object` becomes `OBJECT`).
    Upper,
}

/// Rewrite a Gemini `Schema` into standard JSON Schema.
///
/// `Schema.type` is the uppercase OpenAPI enum (`OBJECT`, `STRING`, ...),
/// while the IR carries JSON Schema whose `type` values are lowercase.
/// Forwarding the Gemini object verbatim sent `"type": "OBJECT"` upstream,
/// which a provider that validates tool schemas rejects.
fn normalize_gemini_schema(schema: &mut Value) {
    retype_schema(schema, SchemaTypeCase::Lower);
}

/// Rewrite a standard JSON Schema into the Gemini `Schema` shape.
///
/// The reverse of [`normalize_gemini_schema`]: Gemini rejects lowercase
/// `Schema.type` values, so an OpenAI-origin `responseSchema` is upper-cased
/// before it is sent to a Gemini upstream.
fn gemini_schema_from_json(schema: &Value) -> Value {
    let mut schema = schema.clone();
    retype_schema(&mut schema, SchemaTypeCase::Upper);
    schema
}

fn retype_schema(schema: &mut Value, case: SchemaTypeCase) {
    let Some(map) = schema.as_object_mut() else {
        return;
    };
    if case == SchemaTypeCase::Lower
        && matches!(map.get("type"), Some(Value::String(name)) if name == "TYPE_UNSPECIFIED")
    {
        // An unspecified type means "any type" in JSON Schema, which is the
        // absence of the keyword.
        map.remove("type");
    } else if let Some(Value::String(name)) = map.get_mut("type") {
        retype_name(name, case);
    } else if let Some(Value::Array(types)) = map.get_mut("type") {
        for entry in types.iter_mut() {
            if let Value::String(name) = entry {
                retype_name(name, case);
            }
        }
    }

    // Recurse only where a schema nests other schemas, so a `type` key inside
    // `default`, `example` or `enum` data is left untouched.
    for key in [
        "items",
        "additionalProperties",
        "not",
        "anyOf",
        "oneOf",
        "allOf",
        "prefixItems",
    ] {
        if let Some(child) = map.get_mut(key) {
            retype_schema_children(child, case);
        }
    }
    for key in ["properties", "$defs", "definitions"] {
        if let Some(Value::Object(children)) = map.get_mut(key) {
            for child in children.values_mut() {
                retype_schema(child, case);
            }
        }
    }
}

fn retype_name(name: &mut str, case: SchemaTypeCase) {
    match case {
        SchemaTypeCase::Lower => name.make_ascii_lowercase(),
        SchemaTypeCase::Upper => name.make_ascii_uppercase(),
    }
}

fn retype_schema_children(value: &mut Value, case: SchemaTypeCase) {
    if let Value::Array(children) = value {
        for child in children.iter_mut() {
            retype_schema(child, case);
        }
    } else {
        retype_schema(value, case);
    }
}

/// Decode Gemini `generationConfig.responseMimeType` / `responseSchema`.
///
/// `responseSchema` takes precedence because it is the stronger constraint;
/// `application/json` without a schema is plain JSON-object mode. Any other
/// MIME type states a constraint the IR cannot express, so it is refused
/// instead of silently dropped.
fn decode_structured_output(
    mime: Option<&Value>,
    schema: Option<&Value>,
) -> Result<Option<StructuredOutput>> {
    let mime = match mime {
        None | Some(Value::Null) => None,
        Some(Value::String(mime)) => Some(mime.as_str()),
        Some(_) => return Err(Error::invalid("`responseMimeType` must be a string")),
    };
    if let Some(schema) = schema.filter(|value| !value.is_null()) {
        let mut schema = schema.clone();
        // `responseSchema` is a Gemini `Schema`; the IR carries JSON Schema.
        normalize_gemini_schema(&mut schema);
        return Ok(Some(StructuredOutput::JsonSchema {
            // Gemini schemas are unnamed.
            name: None,
            schema,
            // Gemini enforces the schema but exposes no strictness flag, so no
            // strict request is recorded on its behalf.
            strict: false,
        }));
    }
    match mime {
        Some("application/json") => Ok(Some(StructuredOutput::JsonObject)),
        // `text/plain` is the documented default and states no constraint.
        Some("text/plain") | Some("") | None => Ok(None),
        Some(other) => Err(unsupported_content("responseMimeType", Some(other))),
    }
}

/// Map `generationConfig.thinkingConfig` onto the IR thinking config.
///
/// Gemini expresses the budget in tokens and disables thinking with a budget
/// of zero; an absent budget means the provider default. Gemini 3's
/// `thinkingLevel` has no IR counterpart and is not mapped here, and neither
/// is `includeThoughts`.
fn decode_thinking_config(config: &Value) -> Result<ThinkingConfig> {
    match config
        .get("thinkingBudget")
        .filter(|value| !value.is_null())
    {
        None => Ok(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
        }),
        Some(budget) => {
            let budget = budget
                .as_u64()
                .ok_or_else(|| Error::invalid("`thinkingBudget` must be a non-negative integer"))?;
            let budget = budget.min(u32::MAX as u64) as u32;
            Ok(ThinkingConfig {
                enabled: budget > 0,
                budget_tokens: (budget > 0).then_some(budget),
            })
        }
    }
}

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
    if let Some(thinking) = &req.thinking {
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

// ---------------------------------------------------------------- responses

pub fn decode_response(body: Value) -> Result<ChatResponse> {
    if let Some(err) = body.get("error") {
        return Err(Error::BadUpstreamPayload(
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown Gemini error")
                .to_string(),
        ));
    }
    let candidate = match body.get("candidates") {
        None | Some(Value::Null) => None,
        Some(Value::Array(candidates)) => candidates.first(),
        Some(_) => {
            return Err(Error::BadUpstreamPayload(
                "Gemini candidates must be an array".into(),
            ));
        }
    };
    let mut content = Vec::new();
    let mut stop_reason = StopReason::Unknown;
    if let Some(candidate) = candidate {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    content.push(ContentBlock::text(text));
                } else if let Some(call) = part.get("functionCall") {
                    let id = call.get("id").and_then(Value::as_str).ok_or_else(|| {
                        Error::BadUpstreamPayload("functionCall is missing `id`".into())
                    })?;
                    let name = call.get("name").and_then(Value::as_str).ok_or_else(|| {
                        Error::BadUpstreamPayload("functionCall is missing `name`".into())
                    })?;
                    content.push(ContentBlock::ToolUse {
                        id: id.to_string(),
                        name: name.to_string(),
                        input: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    });
                } else {
                    return Err(Error::BadUpstreamPayload(
                        "unsupported Gemini response part".into(),
                    ));
                }
            }
        }
        stop_reason = match candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "STOP" => StopReason::EndTurn,
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" => StopReason::Refusal,
            _ => StopReason::Unknown,
        };
    }
    let usage = body
        .get("usageMetadata")
        .map(decode_usage_metadata)
        .unwrap_or_default();

    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("gemini_unknown")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content,
        stop_reason,
        stop_sequence: None,
        usage,
        passthrough: Map::new(),
    })
}

/// Decode a Gemini `usageMetadata` object into the IR contract.
///
/// Gemini's `promptTokenCount` is the total effective prompt size and already
/// includes `cachedContentTokenCount`, which is the cached (read) subset. The
/// cached count is clamped to the total so the IR subset invariant holds even
/// for a relay that reports the two inconsistently.
pub(crate) fn decode_usage_metadata(u: &Value) -> Usage {
    let counter = |key: &str| {
        u.get(key)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32
    };
    let input_tokens = counter("promptTokenCount");
    Usage {
        input_tokens,
        output_tokens: counter("candidatesTokenCount"),
        cache_read_tokens: counter("cachedContentTokenCount").min(input_tokens),
        cache_write_tokens: 0,
        reasoning_tokens: counter("thoughtsTokenCount"),
    }
}

/// Encode the IR contract to Gemini's cache-inclusive `usageMetadata`.
pub(crate) fn encode_usage(u: &Usage) -> Value {
    json!({
        "promptTokenCount": u.input_tokens,
        "candidatesTokenCount": u.output_tokens,
        "totalTokenCount": u.total(),
        "cachedContentTokenCount": u.cache_read_tokens.min(u.input_tokens),
        "thoughtsTokenCount": u.reasoning_tokens,
    })
}

pub fn encode_response(resp: &ChatResponse) -> Value {
    let parts: Vec<Value> = resp
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text, .. } => json!({"text": text}),
            ContentBlock::ToolUse { id, name, input } => json!({
                "functionCall": {"id": id, "name": name, "args": input}
            }),
            _ => explicit_placeholder(b)
                .as_text()
                .map(|text| json!({"text": text}))
                .unwrap_or_else(|| json!({"text": "[Unsupported content]"})),
        })
        .collect();
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": match resp.stop_reason {
                StopReason::MaxTokens => "MAX_TOKENS",
                StopReason::Refusal => "SAFETY",
                _ => "STOP",
            },
        }],
        "usageMetadata": encode_usage(&resp.usage),
    })
}

// ---------------------------------------------------------------- stream in

pub struct GeminiStreamParser {
    model: String,
    started: bool,
    /// Whether the upstream really ended the answer (`finishReason`). A body
    /// that just stops arriving is truncated.
    normal_terminal: bool,
    next_index: u32,
    text_index: Option<u32>,
    open_blocks: Vec<u32>,
    usage: Usage,
}

impl GeminiStreamParser {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            started: false,
            normal_terminal: false,
            next_index: 0,
            text_index: None,
            open_blocks: Vec::new(),
            usage: Usage::default(),
        }
    }
}

impl StreamParser for GeminiStreamParser {
    fn push(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>> {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|e| Error::BadUpstreamPayload(format!("bad Gemini SSE: {e}: {data}")))?;
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(StreamEvent::Start {
                id: format!("gemini-{}", uuid::Uuid::new_v4().simple()),
                model: self.model.clone(),
                usage: self.usage,
            });
        }
        if let Some(usage) = value.get("usageMetadata") {
            self.usage = decode_usage_metadata(usage);
        }

        if let Some(candidate) = value
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        let index = match self.text_index {
                            Some(i) => i,
                            None => {
                                let i = self.next_index;
                                self.next_index += 1;
                                self.text_index = Some(i);
                                self.open_blocks.push(i);
                                i
                            }
                        };
                        out.push(StreamEvent::TextDelta {
                            index,
                            text: text.to_string(),
                        });
                    } else if let Some(call) = part.get("functionCall") {
                        // Close the open text block if any.
                        if let Some(text_idx) = self.text_index.take() {
                            self.open_blocks.retain(|i| *i != text_idx);
                            out.push(StreamEvent::BlockStop { index: text_idx });
                        }
                        let index = self.next_index;
                        self.next_index += 1;
                        let id = call.get("id").and_then(Value::as_str).ok_or_else(|| {
                            Error::BadUpstreamPayload("stream functionCall is missing `id`".into())
                        })?;
                        let name = call.get("name").and_then(Value::as_str).ok_or_else(|| {
                            Error::BadUpstreamPayload(
                                "stream functionCall is missing `name`".into(),
                            )
                        })?;
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: id.to_string(),
                            name: name.to_string(),
                        });
                        if let Some(args) = call.get("args") {
                            out.push(StreamEvent::ToolUseDelta {
                                index,
                                partial_json: args.to_string(),
                            });
                        }
                        // Gemini sends functionCall as a complete block, so close it.
                        out.push(StreamEvent::BlockStop { index });
                    } else {
                        return Err(Error::BadUpstreamPayload(
                            "unsupported Gemini stream response part".into(),
                        ));
                    }
                }
            }
            if let Some(finish_reason) = candidate.get("finishReason").and_then(Value::as_str) {
                self.normal_terminal = true;
                // Close the running text block before the terminal event: a
                // client's block stream must not see a block stop after the
                // answer has already ended.
                if let Some(text_idx) = self.text_index.take() {
                    self.open_blocks.retain(|i| *i != text_idx);
                    out.push(StreamEvent::BlockStop { index: text_idx });
                }
                out.push(StreamEvent::Stop {
                    stop_reason: match finish_reason {
                        "MAX_TOKENS" => StopReason::MaxTokens,
                        "SAFETY" => StopReason::Refusal,
                        _ => StopReason::EndTurn,
                    },
                    stop_sequence: None,
                    usage: self.usage,
                });
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.normal_terminal {
            // `finishReason` already closed every block before the terminal
            // event, so nothing may follow `Stop`.
            return Vec::new();
        }
        let mut out = Vec::new();
        // Close whatever is still open so the client's block stream stays well
        // formed. A body that ended without a `finishReason` gets no `Stop`:
        // that event is what marks the answer finished, and this one was not.
        if let Some(text_idx) = self.text_index.take() {
            self.open_blocks.retain(|i| *i != text_idx);
            out.push(StreamEvent::BlockStop { index: text_idx });
        }
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(StreamEvent::BlockStop { index });
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

// --------------------------------------------------------------- stream out

pub struct GeminiStreamEncoder {
    done: bool,
    /// Buffered tool calls awaiting completion: (index, id, name, accumulated args).
    tool_buffers: Vec<(u32, String, String, String)>,
}

impl GeminiStreamEncoder {
    /// The model is not stored: Gemini's wire format names no model, and the
    /// client-facing id is not part of a streamed `candidates` frame.
    pub fn new(_model: &str) -> Self {
        Self {
            done: false,
            tool_buffers: Vec::new(),
        }
    }

    /// Flush all buffered tool calls as complete functionCall frames.
    fn flush_tools(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for (_, id, name, args) in self.tool_buffers.drain(..) {
            let parsed_args: Value = if args.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&args).unwrap_or(Value::String(args))
            };
            out.push(SseFrame {
                event: None,
                data: json!({
                    "candidates": [{"content": {"role": "model", "parts": [{
                        "functionCall": {"id": id, "name": name, "args": parsed_args}
                    }]}}]
                })
                .to_string(),
            });
        }
        out
    }
}

impl StreamEncoder for GeminiStreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<SseFrame> {
        match event {
            // Gemini's wire format names no model, so nothing is echoed here;
            // the upstream's own name in the event is deliberately dropped.
            StreamEvent::Start { .. } => Vec::new(),
            StreamEvent::TextDelta { text, .. } => {
                // Flush any buffered tool calls before emitting text.
                let mut out = self.flush_tools();
                out.push(SseFrame {
                    event: None,
                    data: json!({
                        "candidates": [{"content": {"role": "model", "parts": [{"text": text}]}}]
                    })
                    .to_string(),
                });
                out
            }
            StreamEvent::ToolUseStart {
                index, id, name, ..
            } => {
                // Flush any previously buffered tool calls.
                let out = self.flush_tools();
                self.tool_buffers
                    .push((*index, id.clone(), name.clone(), String::new()));
                out
            }
            StreamEvent::ToolUseDelta {
                partial_json,
                index,
                ..
            } => {
                if let Some(buf) = self
                    .tool_buffers
                    .iter_mut()
                    .find(|(i, _, _, _)| *i == *index)
                {
                    buf.3.push_str(partial_json);
                }
                Vec::new()
            }
            StreamEvent::BlockStop { index, .. } => {
                // Flush the tool buffer for this index if it exists.
                let mut out = Vec::new();
                if let Some(pos) = self
                    .tool_buffers
                    .iter()
                    .position(|(i, _, _, _)| *i == *index)
                {
                    let (_, id, name, args) = self.tool_buffers.remove(pos);
                    let parsed_args: Value = if args.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&args).unwrap_or(Value::String(args))
                    };
                    out.push(SseFrame {
                        event: None,
                        data: json!({
                            "candidates": [{"content": {"role": "model", "parts": [{
                                "functionCall": {"id": id, "name": name, "args": parsed_args}
                            }]}}]
                        })
                        .to_string(),
                    });
                }
                out
            }
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                let mut out = self.flush_tools();
                self.done = true;
                let finish_reason = match stop_reason {
                    StopReason::MaxTokens => "MAX_TOKENS",
                    StopReason::Refusal => "SAFETY",
                    _ => "STOP",
                };
                out.push(SseFrame {
                    event: None,
                    data: json!({
                        "candidates": [{
                            "content": {"role": "model", "parts": []},
                            "finishReason": finish_reason
                        }],
                        "usageMetadata": encode_usage(usage),
                    })
                    .to_string(),
                });
                out
            }
            _ => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<SseFrame> {
        if self.done {
            return Vec::new();
        }
        self.done = true;
        let mut out = self.flush_tools();
        out.push(SseFrame {
            event: None,
            data: json!({
                "candidates": [{"content": {"role": "model", "parts": []}, "finishReason": "STOP"}]
            })
            .to_string(),
        });
        out
    }

    fn error(&mut self, err: &Error) -> Vec<SseFrame> {
        self.done = true;
        vec![SseFrame {
            event: None,
            data: json!({
                "error": err.to_wire(Dialect::Gemini)
            })
            .to_string(),
        }]
    }
}
