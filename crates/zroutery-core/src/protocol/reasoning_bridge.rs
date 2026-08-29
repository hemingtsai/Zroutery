//! Bridge between Anthropic thinking blocks and OpenAI Responses API
//! encrypted reasoning items.
//!
//! Anthropic keeps thinking signatures in the clear on `thinking` blocks.
//! OpenAI's Responses API stores them inside an opaque `encrypted_content`
//! blob. This module encodes the whole Anthropic block into that blob and
//! decodes it back, so reasoning can survive a round trip through the IR.

use serde_json::{json, Value};

use crate::ir::ContentBlock;

/// Prefix used to identify blobs produced by this bridge.
pub const PREFIX: &str = "zroutery-reasoning-v1:";

/// Encode an Anthropic thinking block into a Responses API reasoning item.
///
/// A normal `thinking` block is only encoded when it carries a signature:
/// unsigned reasoning text has no need for the opaque bridge. `redacted_thinking`
/// is always encoded because it is already opaque.
pub fn encode_thinking_block(block: &ContentBlock) -> Option<Value> {
    let payload = match block {
        ContentBlock::Thinking {
            text,
            signature: Some(signature),
        } if !signature.is_empty() => json!({
            "type": "thinking",
            "text": text,
            "signature": signature,
        }),
        ContentBlock::RedactedThinking { data } if !data.is_empty() => json!({
            "type": "redacted_thinking",
            "data": data,
        }),
        _ => return None,
    };
    let encoded = format!(
        "{PREFIX}{}",
        base64url_encode(payload.to_string().as_bytes())
    );
    Some(json!({
        "type": "reasoning",
        "encrypted_content": encoded,
        "summary": [],
    }))
}

/// Decode a Responses API reasoning item back into an Anthropic thinking block.
///
/// Returns `None` for anything that is not one of our prefixed blobs, and also
/// for corrupted base64/JSON so callers can safely fall back to ignoring it.
pub fn decode_reasoning_item(item: &Value) -> Option<ContentBlock> {
    let encrypted = item.get("encrypted_content")?.as_str()?;
    let raw = encrypted.strip_prefix(PREFIX)?;
    let bytes = base64url_decode(raw)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    match value.get("type")?.as_str()? {
        "thinking" => Some(ContentBlock::Thinking {
            text: value.get("text")?.as_str()?.to_string(),
            signature: Some(value.get("signature")?.as_str()?.to_string()),
        }),
        "redacted_thinking" => Some(ContentBlock::RedactedThinking {
            data: value.get("data")?.as_str()?.to_string(),
        }),
        _ => None,
    }
}

fn base64url_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in input.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        buf = (buf << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1u32 << bits) - 1;
        }
    }
    // Reject non-canonical trailing bits.
    if bits >= 6 || (bits > 0 && buf != 0) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trip() {
        let data = b"hello world\x00\xff";
        let encoded = base64url_encode(data);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert_eq!(base64url_decode(&encoded).unwrap(), data);
    }

    #[test]
    fn decode_rejects_corrupt_input() {
        let item = json!({"type": "reasoning", "encrypted_content": "zroutery-reasoning-v1:%%%"});
        assert!(decode_reasoning_item(&item).is_none());
        let item = json!({"type": "reasoning", "encrypted_content": "not-our-prefix"});
        assert!(decode_reasoning_item(&item).is_none());
    }
}
