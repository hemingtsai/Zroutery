//! Replacing image blocks with their descriptions.

use crate::ir::{ChatRequest, ContentBlock, ToolResultPart};

use super::collect::ImageSlot;

/// What to put where an image was.
#[derive(Debug, Clone, PartialEq)]
pub enum Replacement {
    /// The vision model's description, wrapped so the receiving model (and
    /// the user reading the transcript) can tell description from reality.
    Description(String),
    /// The honest placeholder: the image existed, and could not be carried.
    Placeholder(String),
}

impl Replacement {
    fn text(&self) -> String {
        match self {
            Replacement::Description(text) => format!("[Image description: {text}]"),
            Replacement::Placeholder(text) => text.clone(),
        }
    }
}

/// Replace the image at `slot` in the request with `replacement`.
///
/// The block's neighbours, cache markers on the surrounding structure and the
/// rest of the message are untouched — a swap, not a rewrite. Returns false
/// when the slot no longer matches an image (the request changed underneath),
/// which the caller treats as "nothing replaced".
pub fn replace(req: &mut ChatRequest, slot: &ImageSlot, replacement: &Replacement) -> bool {
    let (message_index, block_index) = match slot {
        ImageSlot::Message { message_index, block_index }
        | ImageSlot::ToolResult { message_index, block_index, .. } => (*message_index, *block_index),
    };
    let Some(message) = req.messages.get_mut(message_index) else { return false };

    match slot {
        ImageSlot::Message { .. } => {
            let Some(block) = message.content.get_mut(block_index) else { return false };
            if !matches!(block, ContentBlock::Image { .. }) {
                return false;
            }
            *block = ContentBlock::text(replacement.text());
            true
        }
        ImageSlot::ToolResult { part_index, .. } => {
            let Some(block) = message.content.get_mut(block_index) else { return false };
            let ContentBlock::ToolResult { content, .. } = block else { return false };
            let Some(part) = content.get_mut(*part_index) else { return false };
            if !matches!(part, ToolResultPart::Image { .. }) {
                return false;
            }
            *part = ToolResultPart::Text {
                text: replacement.text(),
            };
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Dialect, MediaSource, Message, Role};

    #[test]
    fn replaces_message_images_keeping_cache_markers() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message {
            role: Role::User,
            content: vec![
                ContentBlock::Image {
                    source: MediaSource::from_url("https://example.com/a.png"),
                },
                ContentBlock::text("what is this"),
            ],
        });
        let slot = ImageSlot::Message { message_index: 0, block_index: 0 };
        assert!(replace(&mut req, &slot, &Replacement::Description("a cat".into())));
        assert_eq!(req.messages[0].content[0], ContentBlock::text("[Image description: a cat]"));
        assert_eq!(req.messages[0].content[1], ContentBlock::text("what is this"));
    }

    #[test]
    fn replaces_tool_result_image_parts() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tu".into(),
                name: String::new(),
                content: vec![ToolResultPart::Image {
                    source: MediaSource::from_url("https://example.com/s.png"),
                }],
                is_error: false,
            }],
        });
        let slot = ImageSlot::ToolResult { message_index: 0, block_index: 0, part_index: 0 };
        assert!(replace(&mut req, &slot, &Replacement::Placeholder("[Unsupported Image]".into())));
        match &req.messages[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(
                    content[0],
                    ToolResultPart::Text { text: "[Unsupported Image]".into() }
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_slot_that_no_longer_matches_is_reported_not_patched() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message::user_text("no image here"));
        let slot = ImageSlot::Message { message_index: 0, block_index: 0 };
        assert!(!replace(&mut req, &slot, &Replacement::Description("x".into())));
        // The text block is untouched, not replaced with a bogus description.
        assert_eq!(req.messages[0].content[0], ContentBlock::text("no image here"));
    }
}
