//! Rectifier for providers that reject images or multimodal content.
//!
//! The fix replaces image/image_url blocks with a text placeholder instead of
//! deleting them, so message structure and cache markers stay intact.

use serde_json::{json, Value};

use super::{error_text, messages_mut, Rectifier, RectifyResult};
use crate::error::Error;

pub struct MediaFallbackRectifier;

impl Rectifier for MediaFallbackRectifier {
    fn should_apply(&self, error: &Error, _body: &Value) -> bool {
        let status_matches = matches!(
            error,
            Error::Upstream { status, .. } if matches!(status, 400 | 415 | 422 | 501)
        );
        if !status_matches {
            return false;
        }
        let msg = error_text(error);
        let has_media = ["image", "vision", "multimodal"]
            .iter()
            .any(|n| msg.contains(n));
        let has_rejection = ["unsupported", "not supported", "text only"]
            .iter()
            .any(|n| msg.contains(n));
        has_media && has_rejection
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
                for block in blocks.iter_mut() {
                    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                    if !matches!(kind, "image" | "image_url" | "input_image") {
                        continue;
                    }
                    let cache_control = block.get("cache_control").cloned();
                    let mut replacement = json!({"type": "text", "text": "[Unsupported Image]"});
                    if let Some(cache_control) = cache_control {
                        replacement["cache_control"] = cache_control;
                    }
                    *block = replacement;
                    applied = true;
                }
            }
        }

        RectifyResult {
            applied,
            details: if applied {
                "replaced image blocks with a text placeholder".to_string()
            } else {
                "no image blocks to replace".to_string()
            },
        }
    }

    fn name(&self) -> &'static str {
        "media_fallback"
    }
}
