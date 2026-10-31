//! Codex tool catalogs: preserve native declarations and bridge callable tools.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::ir::{ChatRequest, ResponsesToolName, ResponsesTools, ToolDef};
use crate::protocol::{unsupported_content, SseFrame};

/// Register a catalog, flattening namespaces only in the portable IR view.
pub(super) fn decode_tools(tools: &[Value], req: &mut ChatRequest) -> Result<()> {
    for tool in tools {
        match tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function")
        {
            "namespace" => {
                let namespace = required_name(tool)?;
                let children = tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .ok_or_else(|| Error::invalid("namespace `tools` must be an array"))?;
                for child in children {
                    decode_callable(child, Some(namespace), req)?;
                }
            }
            "tool_search"
            | "web_search"
            | "web_search_preview"
            | "file_search"
            | "code_interpreter"
            | "image_generation"
            | "computer_use_preview"
            | "mcp" => {
                req.responses_native_only = true;
            }
            _ => decode_callable(tool, None, req)?,
        }
    }
    Ok(())
}

fn required_name(tool: &Value) -> Result<&str> {
    tool.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::invalid("tool definition is missing `name`"))
}

fn decode_callable(tool: &Value, namespace: Option<&str>, req: &mut ChatRequest) -> Result<()> {
    let kind = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    let custom = match kind {
        "function" => false,
        "custom" => true,
        other => return Err(unsupported_content("tool definition", Some(other))),
    };
    let name = required_name(tool)?;
    let alias = namespace.map_or_else(|| name.to_string(), |ns| format!("{ns}__{name}"));
    let identity = ResponsesToolName {
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        custom,
    };
    let context = req
        .responses_tools
        .get_or_insert_with(ResponsesTools::default);
    if context
        .names
        .get(&alias)
        .is_some_and(|existing| existing != &identity)
    {
        return Err(Error::invalid(
            "Responses tool names collide after namespace conversion",
        ));
    }
    context.names.insert(alias.clone(), identity);

    let strict = match tool.get("strict") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(strict)) => Some(*strict),
        Some(_) => return Err(Error::invalid("tool `strict` must be a boolean")),
    };
    req.tool_strict.remove(&alias);
    if let Some(strict) = strict {
        req.tool_strict.insert(alias.clone(), strict);
    }
    let mut description = tool
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let input_schema = if custom {
        // Chat/Anthropic require JSON arguments. A single string field carries
        // the freeform input without parsing or changing the tool's language.
        let mut hint = "Pass the tool's freeform input verbatim in the `input` string.".to_string();
        if let Some(format) = tool.get("format") {
            hint.push_str(&format!(" Input format: {format}"));
        }
        description = Some(match description {
            Some(text) => format!("{text}\n\n{hint}"),
            None => hint,
        });
        json!({"type": "object", "properties": {"input": {"type": "string"}},
            "required": ["input"], "additionalProperties": false})
    } else {
        tool.get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"}))
    };
    let definition = ToolDef {
        name: alias.clone(),
        description,
        input_schema,
        cache_control: None,
    };
    // A later declaration updates a tool rather than emitting duplicate names.
    if let Some(existing) = req.tools.iter_mut().find(|tool| tool.name == alias) {
        *existing = definition;
    } else {
        req.tools.push(definition);
    }
    Ok(())
}

pub(super) fn call_name(item: &Value) -> Result<String> {
    let name = required_name(item)?;
    Ok(item
        .get("namespace")
        .and_then(Value::as_str)
        .map_or_else(|| name.to_string(), |ns| format!("{ns}__{name}")))
}

/// Recover a custom tool's raw string from the bridge's JSON arguments.
pub(super) fn custom_input(input: &Value) -> String {
    input
        .get("input")
        .and_then(Value::as_str)
        .or_else(|| input.as_str())
        .unwrap_or("")
        .to_string()
}

/// Restore names and freeform calls in a client-facing Responses output item.
pub(super) fn restore_call(item: &mut Value, context: &ResponsesTools) {
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return;
    }
    let Some(identity) = item
        .get("name")
        .and_then(Value::as_str)
        .and_then(|name| context.names.get(name))
    else {
        return;
    };
    if identity.custom {
        let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
        let parsed =
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.into()));
        let input = custom_input(&parsed);
        item.as_object_mut()
            .expect("call is an object")
            .remove("arguments");
        item["type"] = json!("custom_tool_call");
        item["input"] = json!(input);
    }
    item["name"] = json!(identity.name);
    if let Some(namespace) = &identity.namespace {
        item["namespace"] = json!(namespace);
    }
}

/// Apply the request's tool identities before publishing or storing a response.
pub fn restore_response_tools(body: &mut Value, req: &ChatRequest) {
    let Some(context) = &req.responses_tools else {
        return;
    };
    if let Some(output) = body.get_mut("output").and_then(Value::as_array_mut) {
        for item in output {
            restore_call(item, context);
        }
    }
}

/// Staging lets us decode complete custom input before publishing its deltas.
/// Never expose the JSON bridge envelope as a freeform tool's executable input.
pub fn restore_stream_frames(frames: &mut Vec<SseFrame>, req: &ChatRequest) {
    let Some(context) = &req.responses_tools else {
        return;
    };
    let mut custom = std::collections::BTreeMap::new();
    for frame in frames.iter() {
        if let Ok(value) = frame.json() {
            if let Some(item) = value.get("item") {
                let name = item["name"].as_str().unwrap_or("");
                if context
                    .names
                    .get(name)
                    .is_some_and(|identity| identity.custom)
                {
                    let id = item["id"].as_str().unwrap_or("").to_string();
                    let args = item["arguments"].as_str().unwrap_or("");
                    let input = serde_json::from_str::<Value>(args)
                        .map(|v| custom_input(&v))
                        .unwrap_or_default();
                    custom.insert(id, input);
                }
            }
        }
    }
    let mut emitted = std::collections::BTreeSet::new();
    frames.retain_mut(|frame| {
        let Ok(mut value) = frame.json() else {
            return true;
        };
        let kind = value["type"].as_str().unwrap_or("").to_string();
        if let Some(input) = value["item_id"].as_str().and_then(|id| custom.get(id)) {
            if kind == "response.function_call_arguments.delta" {
                if !emitted.insert(value["item_id"].as_str().unwrap_or("").to_string()) {
                    return false;
                }
                value["type"] = json!("response.custom_tool_call_input.delta");
                value["delta"] = json!(input);
                frame.event = Some("response.custom_tool_call_input.delta".into());
            } else if kind == "response.function_call_arguments.done" {
                value["type"] = json!("response.custom_tool_call_input.done");
                value.as_object_mut().unwrap().remove("arguments");
                value["input"] = json!(input);
                frame.event = Some("response.custom_tool_call_input.done".into());
            }
        }
        if let Some(item) = value.get_mut("item") {
            restore_call(item, context);
        }
        if let Some(response) = value.get_mut("response") {
            restore_response_tools(response, req);
        }
        frame.data = value.to_string();
        true
    });
    for (index, frame) in frames.iter_mut().enumerate() {
        if let Ok(mut value) = frame.json() {
            value["sequence_number"] = json!(index);
            frame.data = value.to_string();
        }
    }
}
