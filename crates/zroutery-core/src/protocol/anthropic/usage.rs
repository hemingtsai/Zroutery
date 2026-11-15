//! Strict multi-inference usage parsing shared by buffered, SSE and settlement.

use serde_json::Value;

use crate::ir::{Usage, UsageIteration};

pub(crate) fn decode_iterations(body: &Value) -> Result<Option<Vec<UsageIteration>>, ()> {
    let usage = body.get("message").unwrap_or(body).get("usage");
    let Some(value) = usage
        .and_then(|usage| usage.get("iterations"))
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let array = value.as_array().ok_or(())?;
    if array.is_empty() || array.len() > 4096 {
        return Err(());
    }
    let mut iterations = Vec::with_capacity(array.len());
    for value in array {
        let kind = value.get("type").and_then(Value::as_str).ok_or(())?;
        if kind.is_empty()
            || kind.len() > 64
            || !kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(());
        }
        let model = match value.get("model") {
            None | Some(Value::Null) => None,
            Some(Value::String(model))
                if !model.is_empty()
                    && model.len() <= 256
                    && !model.chars().any(char::is_control) =>
            {
                Some(model.clone())
            }
            _ => return Err(()),
        };
        let fresh = counter(value, "input_tokens", true)?.ok_or(())?;
        let output = counter(value, "output_tokens", true)?.ok_or(())?;
        let read = counter(value, "cache_read_input_tokens", false)?.unwrap_or(0);
        let write = counter(value, "cache_creation_input_tokens", false)?.unwrap_or(0);
        let (write_5m, write_1h) = match value.get("cache_creation") {
            None | Some(Value::Null) => (None, None),
            Some(details) if details.is_object() => {
                let five = counter(details, "ephemeral_5m_input_tokens", true)?.ok_or(())?;
                let hour = counter(details, "ephemeral_1h_input_tokens", true)?.ok_or(())?;
                if five.checked_add(hour) != Some(write) {
                    return Err(());
                }
                (Some(five), Some(hour))
            }
            _ => return Err(()),
        };
        let billed = billing(body, kind, model.as_deref(), output);
        iterations.push(UsageIteration {
            kind: kind.into(),
            model,
            usage: Usage {
                input_tokens: fresh
                    .checked_add(read)
                    .and_then(|n| n.checked_add(write))
                    .ok_or(())?,
                output_tokens: output,
                cache_read_tokens: read,
                cache_write_tokens: write,
                reasoning_tokens: 0,
            },
            cache_write_5m_tokens: write_5m,
            cache_write_1h_tokens: write_1h,
            billed,
            cost: None,
            cost_complete: false,
            rate_quote: None,
        });
    }
    crate::ir::iteration_total(&iterations).ok_or(())?;
    Ok(Some(iterations))
}

fn counter(value: &Value, key: &str, required: bool) -> Result<Option<u32>, ()> {
    match value.get(key) {
        None | Some(Value::Null) if !required => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .map(Some)
            .ok_or(()),
        _ => Err(()),
    }
}

fn billing(body: &Value, kind: &str, model: Option<&str>, output: u32) -> Option<bool> {
    if !matches!(
        kind,
        "message" | "fallback_message" | "compaction" | "advisor_message"
    ) {
        return None;
    }
    if output > 0 || matches!(kind, "compaction" | "advisor_message") {
        return Some(true);
    }
    let body = body.get("message").unwrap_or(body);
    if let Some(blocks) = body.get("content").and_then(Value::as_array) {
        for block in blocks {
            if block["type"] == "fallback"
                && block.pointer("/trigger/type").and_then(Value::as_str) == Some("refusal")
                && model.is_some()
                && block.pointer("/from/model").and_then(Value::as_str) == model
            {
                return refusal_billed(block.pointer("/trigger/category"));
            }
        }
    }
    if model.is_none_or(|model| {
        body.get("model")
            .and_then(Value::as_str)
            .is_none_or(|final_model| model == final_model)
    }) && (body.get("stop_reason").and_then(Value::as_str) == Some("refusal")
        || body.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("refusal"))
    {
        return refusal_billed(
            body.pointer("/stop_details/category")
                .or_else(|| body.pointer("/delta/stop_details/category")),
        );
    }
    if kind == "fallback_message" {
        Some(true)
    } else {
        None
    }
}

fn refusal_billed(category: Option<&Value>) -> Option<bool> {
    match category {
        Some(Value::String(category))
            if matches!(
                category.as_str(),
                "bio" | "frontier_llm" | "reasoning_extraction"
            ) =>
        {
            Some(true)
        }
        Some(Value::String(category)) if matches!(category.as_str(), "cyber" | "general_harms") => {
            Some(false)
        }
        Some(Value::Null) => Some(false),
        _ => None,
    }
}
