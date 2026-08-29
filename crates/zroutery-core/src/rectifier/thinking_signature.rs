//! Rectifier for Anthropic thinking-signature errors.
//!
//! When a request carries a thinking block that is stale, malformed or out of
//! place, the provider rejects it with a signature validation error. The fix is
//! to strip the offending thinking/redacted-thinking blocks and their signature
//! fields so the same provider can accept the repaired request.

use serde_json::Value;

use super::{contains_all, error_text, messages_mut, Rectifier, RectifyResult};
use crate::error::Error;

pub struct ThinkingSignatureRectifier;

impl Rectifier for ThinkingSignatureRectifier {
    fn should_apply(&self, error: &Error, _body: &Value) -> bool {
        let msg = error_text(error);
        contains_all(&msg, &["invalid", "signature", "thinking", "block"])
            || (msg.contains("thought signature")
                && (msg.contains("not valid") || msg.contains("invalid")))
            || msg.contains("must start with a thinking block")
            || (contains_all(&msg, &["expected", "found", "tool_use"])
                && (msg.contains("thinking") || msg.contains("redacted_thinking")))
            || contains_all(&msg, &["signature", "field required"])
            || contains_all(&msg, &["signature", "extra inputs are not permitted"])
            || ((msg.contains("thinking") || msg.contains("redacted_thinking"))
                && msg.contains("cannot be modified"))
    }

    fn rectify(&self, body: &mut Value) -> RectifyResult {
        let mut applied = false;
        if let Some(messages) = messages_mut(body) {
            for message in messages.iter_mut() {
                let Some(content) = message.get_mut("content") else {
                    continue;
                };
                let Some(blocks) = content.as_array_mut() else {
                    continue;
                };

                blocks.retain(|block| {
                    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                    let remove = kind == "thinking" || kind == "redacted_thinking";
                    if remove {
                        applied = true;
                    }
                    !remove
                });

                for block in blocks.iter_mut() {
                    if block.get("signature").is_some() {
                        block.as_object_mut().map(|obj| obj.remove("signature"));
                        applied = true;
                    }
                }
            }
        }

        // If thinking is enabled but the last assistant message does not start
        // with a thinking block, remove the top-level thinking configuration.
        let thinking_ok = body
            .get("thinking")
            .and_then(Value::as_object)
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str)
            == Some("enabled");
        if thinking_ok {
            let last_assistant = body
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|ms| {
                    ms.iter()
                        .rev()
                        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
                });
            if let Some(last_assistant) = last_assistant {
                let starts_with_thinking = last_assistant
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|blocks| blocks.first())
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    == Some("thinking");
                if !starts_with_thinking {
                    if let Some(obj) = body.as_object_mut() {
                        obj.remove("thinking");
                    }
                    applied = true;
                }
            }
        }

        RectifyResult {
            applied,
            details: if applied {
                "removed thinking/redacted_thinking blocks and signatures".to_string()
            } else {
                "no thinking signature content to repair".to_string()
            },
        }
    }

    fn name(&self) -> &'static str {
        "thinking_signature"
    }
}
