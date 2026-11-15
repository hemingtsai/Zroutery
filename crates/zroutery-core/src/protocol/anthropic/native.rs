//! Preserve open native envelopes; refuse bridges that would discard semantics.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::ir::{Capability, ChatRequest, Dialect};

pub(crate) fn validate_bridge(req: &ChatRequest, target: Dialect) -> Result<()> {
    if req.anthropic_native_only && target != Dialect::Anthropic {
        let feature = req
            .anthropic_body
            .as_ref()
            .and_then(native_only_reason)
            .unwrap_or("native content");
        let recovery = match feature {
            "context management" => "capability_rejected: context_management: Extra inputs are not permitted",
            "tool search" => "capability_rejected: tool_search: defer_loading or tool_reference requires a native Messages upstream",
            "advisor tool" => "capability_rejected: Input tag 'advisor_20260301' requires a native Messages upstream",
            _ => "capability_rejected: anthropic_native_required",
        };
        return Err(Error::invalid(format!("{recovery}: Messages context, native tools or request extensions require an Anthropic Messages upstream; refusing to discard them")));
    }
    Ok(())
}

/// Only parameters with an implemented portable meaning may cross a dialect.
/// Everything else stays on the original wire, including future extensions.
pub(crate) fn native_only_reason(body: &Value) -> Option<&'static str> {
    let object = body.as_object()?;
    if body
        .get("context_management")
        .is_some_and(|value| !value.is_null())
    {
        return Some("context management");
    }
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("defer_loading").and_then(Value::as_bool) == Some(true)
                    || tool
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind.starts_with("tool_search"))
            })
        })
    {
        return Some("tool search");
    }
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| tools.iter().any(|tool| tool["type"] == "advisor_20260301"))
    {
        return Some("advisor tool");
    }
    for (key, value) in object {
        if !value.is_null()
            && !matches!(
                key.as_str(),
                "model"
                    | "messages"
                    | "system"
                    | "max_tokens"
                    | "temperature"
                    | "top_p"
                    | "top_k"
                    | "stop_sequences"
                    | "stream"
                    | "tools"
                    | "tool_choice"
                    | "thinking"
                    | "metadata"
                    | "output_config"
                    | "output_format"
                    | "safeguards"
                    | "cache_control"
            )
        {
            return Some("request extension");
        }
    }
    if let Some(config) = body.get("output_config").and_then(Value::as_object) {
        if config
            .iter()
            .any(|(key, value)| !value.is_null() && !matches!(key.as_str(), "effort" | "format"))
        {
            return Some("output configuration");
        }
    }
    if let Some(choice) = body.get("tool_choice").and_then(Value::as_object) {
        if choice
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "name" | "disable_parallel_tool_use"))
        {
            return Some("tool choice extension");
        }
    }
    if body
        .pointer("/output_config/format")
        .or_else(|| body.get("output_format"))
        .and_then(Value::as_object)
        .is_some_and(|format| {
            format
                .keys()
                .any(|key| !matches!(key.as_str(), "type" | "schema"))
        })
    {
        return Some("output format extension");
    }
    if let Some(thinking) = body.get("thinking").and_then(Value::as_object) {
        if thinking.iter().any(|(key, value)| {
            !value.is_null() && !matches!(key.as_str(), "type" | "budget_tokens")
        }) || thinking.get("type").and_then(Value::as_str) == Some("between_tools")
            || (thinking.get("type").and_then(Value::as_str) == Some("adaptive")
                && body.pointer("/output_config/effort").is_none())
        {
            return Some("native thinking");
        }
    }
    if let Some(metadata) = body.get("metadata").and_then(Value::as_object) {
        if metadata
            .keys()
            .any(|key| !matches!(key.as_str(), "user_id" | "application" | "session_id"))
        {
            return Some("metadata extension");
        }
    }
    if let Some(system) = body.get("system").and_then(Value::as_array) {
        if system.iter().any(|block| !portable_block(block)) {
            return Some("system extension");
        }
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            if message.as_object().is_some_and(|object| {
                object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "role" | "content"))
            }) {
                return Some("message extension");
            }
            if message
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| blocks.iter().any(|block| !portable_block(block)))
            {
                return Some("content extension");
            }
        }
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if tool
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "custom")
                || tool.as_object().is_some_and(|object| {
                    object.keys().any(|key| {
                        !matches!(
                            key.as_str(),
                            "type"
                                | "name"
                                | "description"
                                | "input_schema"
                                | "strict"
                                | "cache_control"
                        )
                    })
                })
            {
                return Some("native tool definition");
            }
        }
    }
    None
}

fn portable_block(block: &Value) -> bool {
    let Some(object) = block.as_object() else {
        return false;
    };
    let keys: &[&str] = match block.get("type").and_then(Value::as_str) {
        Some("text") => &["type", "text", "cache_control"],
        Some("thinking") => &["type", "thinking", "signature"],
        Some("redacted_thinking") => &["type", "data"],
        Some("image" | "document") => {
            if !matches!(
                block.pointer("/source/type").and_then(Value::as_str),
                Some("base64" | "url")
            ) {
                return false;
            }
            if block
                .get("source")
                .and_then(Value::as_object)
                .is_some_and(|source| {
                    source
                        .keys()
                        .any(|key| !matches!(key.as_str(), "type" | "url" | "data" | "media_type"))
                })
            {
                return false;
            }
            &["type", "source", "cache_control"]
        }
        Some("tool_use") => &["type", "id", "name", "input", "cache_control"],
        Some("tool_result") => {
            if block
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts.iter().any(|part| {
                        !matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("text" | "image")
                        ) || !portable_block(part)
                    })
                })
            {
                return false;
            }
            &[
                "type",
                "tool_use_id",
                "content",
                "is_error",
                "cache_control",
            ]
        }
        _ => return false,
    };
    object.keys().all(|key| keys.contains(&key.as_str()))
}

/// Derive hard media/tool requirements even when an opaque native block has
/// no portable projection. Do not inspect tool input schemas as message data.
pub(crate) fn native_requirements(body: &Value) -> Vec<Capability> {
    let mut requirements = Vec::new();
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        requirements.push(Capability::Tools);
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            visit_content(message.get("content"), &mut requirements);
        }
    }
    requirements.sort();
    requirements.dedup();
    requirements
}

fn visit_content(content: Option<&Value>, requirements: &mut Vec<Capability>) {
    let Some(parts) = content.and_then(Value::as_array) else {
        return;
    };
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("image") => requirements.push(Capability::Vision),
            Some("document") => requirements.push(Capability::Files),
            Some("thinking" | "redacted_thinking") => requirements.push(Capability::Thinking),
            Some(
                "tool_use" | "server_tool_use" | "tool_reference" | "tool_addition"
                | "tool_removal",
            ) => {
                requirements.push(Capability::Tools);
            }
            Some(kind) if kind == "tool_result" || kind.ends_with("_tool_result") => {
                requirements.push(Capability::Tools);
                visit_content(part.get("content"), requirements);
            }
            _ => {}
        }
    }
}
