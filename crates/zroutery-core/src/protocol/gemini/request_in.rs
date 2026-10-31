//! Decoding an inbound Gemini generateContent request into the IR.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::ir::{
    classify_media, ChatRequest, ContentBlock, Dialect, MediaSource, Message, ReasoningRequest,
    Role, StructuredOutput, SystemPart, ThinkingConfig, ToolChoice, ToolDef, ToolResultPart,
};
use crate::protocol::unsupported_content;
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
    // The native resource handler sets this from the URL operation. The legacy
    // /generateContent compatibility route may also request streaming explicitly.
    req.stream = match obj.get("stream") {
        None => false,
        Some(Value::Bool(stream)) => *stream,
        Some(_) => return Err(Error::invalid("stream must be a boolean")),
    };

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
            if let Some(level) = thinking
                .get("thinkingLevel")
                .filter(|value| !value.is_null())
            {
                if thinking
                    .get("thinkingBudget")
                    .is_some_and(|value| !value.is_null())
                {
                    return Err(Error::invalid(
                        "send thinkingLevel or thinkingBudget, not both",
                    ));
                }
                let level = level
                    .as_str()
                    .ok_or_else(|| Error::invalid("thinkingLevel must be a native identifier"))?;
                let reasoning = ReasoningRequest::native(level).map_err(Error::invalid)?;
                req.thinking = Some(reasoning.legacy());
                req.reasoning = Some(reasoning);
            } else {
                let thinking = decode_thinking_config(thinking)?;
                req.reasoning = if !thinking.enabled {
                    Some(ReasoningRequest::Disabled)
                } else {
                    thinking
                        .budget_tokens
                        .map(|tokens| ReasoningRequest::TokenBudget { tokens })
                };
                req.thinking = Some(thinking);
            }
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
pub(super) fn gemini_schema_from_json(schema: &Value) -> Value {
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
/// of zero; an absent budget means the provider default. Native thinkingLevel
/// is handled separately and must never become an invented numerical budget.
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
