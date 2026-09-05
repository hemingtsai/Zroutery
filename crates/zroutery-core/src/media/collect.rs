//! Finding every image a request carries, in whatever dialect it arrived.
//!
//! Images live in two places: user-message image blocks, and image parts
//! inside tool results. Both dialects spell them differently, but the IR
//! normalises them into `ContentBlock::Image` / `ToolResultPart::Image`, so
//! collection happens on the IR — one implementation, every dialect.

use crate::ir::{ChatRequest, ContentBlock, MediaSource, ToolResultPart};

/// One image found in a request, addressable for replacement.
///
/// `message_index` + `block_index` identify the user-message block;
/// `ToolResult` additionally carries the part index inside that result.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageSlot {
    Message { message_index: usize, block_index: usize },
    ToolResult { message_index: usize, block_index: usize, part_index: usize },
}

/// Every image slot in the request, in traversal order.
///
/// Returns owned `MediaSource` values (cloned from the request).  A
/// drain-based API that takes ownership of the images and leaves
/// placeholders would avoid the clone cost, but it would require the
/// caller to reconstruct the request afterwards.  For the current
/// desktop-proxy use case the clone is negligible compared to the
/// upstream round-trip.
pub fn collect(req: &ChatRequest) -> Vec<(ImageSlot, MediaSource)> {
    let mut found = Vec::new();
    for (message_index, message) in req.messages.iter().enumerate() {
        for (block_index, block) in message.content.iter().enumerate() {
            match block {
                ContentBlock::Image { source } => {
                    found.push((
                        ImageSlot::Message { message_index, block_index },
                        source.clone(),
                    ));
                }
                ContentBlock::ToolResult { content, .. } => {
                    for (part_index, part) in content.iter().enumerate() {
                        if let ToolResultPart::Image { source } = part {
                            found.push((
                                ImageSlot::ToolResult {
                                    message_index,
                                    block_index,
                                    part_index,
                                },
                                source.clone(),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Dialect, Message};

    #[test]
    fn finds_message_and_tool_result_images() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message {
            role: crate::ir::Role::User,
            content: vec![
                ContentBlock::text("look at this"),
                ContentBlock::Image {
                    source: MediaSource::from_url("https://example.com/a.png"),
                },
            ],
        });
        req.messages.push(Message {
            role: crate::ir::Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tu".into(),
                name: String::new(),
                content: vec![
                    ToolResultPart::Text { text: "shot:".into() },
                    ToolResultPart::Image {
                        source: MediaSource::from_url("https://example.com/b.png"),
                    },
                ],
                is_error: false,
            }],
        });

        let found = collect(&req);
        assert_eq!(found.len(), 2);
        assert!(matches!(found[0].0, ImageSlot::Message { message_index: 0, block_index: 1 }));
        assert!(
            matches!(found[1].0, ImageSlot::ToolResult { message_index: 1, block_index: 0, part_index: 1 })
        );
    }
}
