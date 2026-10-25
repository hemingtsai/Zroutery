//! Strip Codex's OpenAI-internal item attribution field only when a compatible
//! Responses upstream explicitly rejects it. Ordinary metadata and nested tool
//! arguments/results are not transport fields and must remain untouched.

use serde_json::Value;

use super::{Rectifier, RectifyResult};
use crate::error::Error;

pub const INTERNAL_FIELD: &str = "internal_chat_message_metadata_passthrough";

pub struct CodexMetadataRectifier;

impl Rectifier for CodexMetadataRectifier {
    fn should_apply(&self, error: &Error, body: &Value) -> bool {
        let Error::Upstream {
            status: 400 | 422,
            body: message,
            ..
        } = error
        else {
            return false;
        };
        let message = message.to_ascii_lowercase();
        message.contains(INTERNAL_FIELD)
            && [
                "unknown parameter",
                "unknown field",
                "unrecognized",
                "unrecognised",
                "unexpected",
                "extra inputs are not permitted",
                "not allowed",
                "unsupported parameter",
            ]
            .iter()
            .any(|signature| message.contains(signature))
            && body
                .get("input")
                .and_then(Value::as_array)
                .is_some_and(|input| {
                    input.iter().any(|item| {
                        item.as_object()
                            .is_some_and(|item| item.contains_key(INTERNAL_FIELD))
                    })
                })
    }

    fn rectify(&self, body: &mut Value) -> RectifyResult {
        let removed = strip_internal_item_metadata(body);
        RectifyResult {
            applied: removed > 0,
            details: format!(
                "removed Codex internal item metadata from {removed} Responses input items"
            ),
        }
    }

    fn name(&self) -> &'static str {
        "codex_metadata"
    }
}

/// Remove the exact direct member on every input item. Not recursive: the same
/// string inside tool data, message text or normal metadata is user content.
pub fn strip_internal_item_metadata(body: &mut Value) -> usize {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return 0;
    };
    input
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .map(|item| usize::from(item.remove(INTERNAL_FIELD).is_some()))
        .sum()
}
