//! Bridge between Anthropic thinking blocks and OpenAI Responses API
//! encrypted reasoning items.
//!
//! Anthropic keeps thinking signatures in the clear on `thinking` blocks.
//! OpenAI's Responses API stores them inside an opaque `encrypted_content`
//! blob. This module encodes the whole Anthropic block into that blob and
//! decodes it back, so reasoning can survive a round trip through the IR.

use serde_json::{json, Value};

use crate::ir::{ContentBlock, ThinkingConfig};

/// Prefix used to identify blobs produced by this bridge.
pub const PREFIX: &str = "zroutery-reasoning-v1:";

/// Budget the IR records for each OpenAI reasoning-effort level.
///
/// The IR carries reasoning as a token budget, so the effort vocabulary has to
/// be expressed in tokens. These are the values both decoders use and the
/// values the encoders map back from, which keeps `xhigh` and `max` from
/// collapsing into `high` on a round trip.
const LOW_BUDGET: u32 = 1024;
const MEDIUM_BUDGET: u32 = 4096;
const HIGH_BUDGET: u32 = 16384;
const XHIGH_BUDGET: u32 = 32768;
const MAX_BUDGET: u32 = 65536;

/// Decode an OpenAI `reasoning_effort` value into the IR thinking config.
///
/// `None` means the value is not part of the OpenAI effort vocabulary; the
/// caller turns that into its dialect's unsupported-content error. `none` and
/// `minimal` both disable reasoning in the IR, which has no level below a
/// budget.
pub fn decode_effort(effort: &str) -> Option<ThinkingConfig> {
    let enabled = |budget_tokens: u32| ThinkingConfig {
        enabled: true,
        budget_tokens: Some(budget_tokens),
    };
    Some(match effort {
        "none" => ThinkingConfig {
            enabled: false,
            budget_tokens: None,
        },
        "minimal" => enabled(512),
        "low" => enabled(LOW_BUDGET),
        "medium" => enabled(MEDIUM_BUDGET),
        "high" => enabled(HIGH_BUDGET),
        "xhigh" => enabled(XHIGH_BUDGET),
        "max" => enabled(MAX_BUDGET),
        _ => return None,
    })
}

/// The IR budget that stands for one reasoning-effort level.
///
/// `None` for a value outside the effort vocabulary (`none`/`minimal` have no
/// budget of their own).
pub fn budget_for_effort(effort: &str) -> Option<u32> {
    Some(match effort {
        "low" => LOW_BUDGET,
        "medium" => MEDIUM_BUDGET,
        "high" => HIGH_BUDGET,
        "xhigh" => XHIGH_BUDGET,
        "max" => MAX_BUDGET,
        _ => return None,
    })
}

/// Lower `budget` to the highest level a model declares, when it asks for more.
///
/// A client's picker can be older than the model set behind it: a virtual
/// class keeps offering `max` after a member that tops out at `high` joins.
/// Sending the unadvertised value makes the upstream refuse the request, so the
/// proxy lowers it here, to a level the model itself published. An empty list
/// means the model declares no levels and is left untouched.
pub fn clamp_budget_to_declared(budget: u32, levels: &[String]) -> Option<u32> {
    let ceiling = levels
        .iter()
        .filter_map(|level| budget_for_effort(level))
        .max()?;
    (budget > ceiling).then_some(ceiling)
}

/// Encode an IR thinking config as the closest OpenAI effort level.
///
/// The thresholds are the decode values above, so `high`, `xhigh` and `max`
/// survive a round trip instead of being rounded down to the nearest of the
/// three levels this bridge used to know.
pub fn encode_effort(thinking: &ThinkingConfig) -> &'static str {
    if !thinking.enabled {
        return "none";
    }
    match thinking.budget_tokens.unwrap_or(MEDIUM_BUDGET) {
        0..=512 => "minimal",
        budget if budget <= LOW_BUDGET => "low",
        budget if budget < HIGH_BUDGET => "medium",
        budget if budget < XHIGH_BUDGET => "high",
        budget if budget < MAX_BUDGET => "xhigh",
        _ => "max",
    }
}

/// Preserve a native value. The pipeline validates declared support, direct
/// same-protocol exceptions and bridge evidence before encoding it.
pub fn request_effort(req: &crate::ir::ChatRequest) -> Option<&str> {
    req.reasoning
        .as_ref()
        .and_then(crate::ir::ReasoningRequest::effort)
        .or_else(|| req.thinking.as_ref().map(encode_effort))
}

/// Translate native effort as a native identifier, never through an invented
/// numeric ceiling. Exact model/surface support is still checked by routing.
/// Anthropic and OpenAI both encode these five names; their implementations
/// need not spend identical tokens at the same name. Unknown/minimal values do
/// not gain a bridge merely because another model once advertised them.
pub fn native_effort_bridge(
    source: crate::ir::Dialect,
    target: crate::ir::Dialect,
    value: &str,
) -> bool {
    use crate::ir::Dialect;
    if source == target {
        return true;
    }
    match (source, target) {
        (Dialect::OpenAI, Dialect::OpenAIResponses)
        | (Dialect::OpenAIResponses, Dialect::OpenAI) => true,
        (Dialect::Anthropic, Dialect::OpenAI | Dialect::OpenAIResponses)
        | (Dialect::OpenAI | Dialect::OpenAIResponses, Dialect::Anthropic) => {
            matches!(value, "low" | "medium" | "high" | "xhigh" | "max")
        }
        _ => false,
    }
}

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

    #[test]
    fn effort_vocabulary_includes_xhigh_and_max() {
        for (effort, budget) in [
            ("none", None),
            ("minimal", Some(512)),
            ("low", Some(LOW_BUDGET)),
            ("medium", Some(MEDIUM_BUDGET)),
            ("high", Some(HIGH_BUDGET)),
            ("xhigh", Some(XHIGH_BUDGET)),
            ("max", Some(MAX_BUDGET)),
        ] {
            let thinking = decode_effort(effort)
                .unwrap_or_else(|| panic!("`{effort}` must be part of the vocabulary"));
            assert_eq!(thinking.budget_tokens, budget, "effort={effort}");
        }
        assert!(decode_effort("ultra").is_none());
    }

    #[test]
    fn effort_round_trips_every_level() {
        for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            let thinking = decode_effort(effort).unwrap();
            assert_eq!(encode_effort(&thinking), effort, "effort={effort}");
        }
        // An Anthropic-style budget between the levels rounds to the lower one
        // rather than inventing a level the vocabulary does not have.
        let thinking = ThinkingConfig {
            enabled: true,
            budget_tokens: Some(20_000),
        };
        assert_eq!(encode_effort(&thinking), "high");
    }

    #[test]
    fn a_budget_is_lowered_to_the_levels_the_model_declares() {
        let levels = |list: &[&str]| {
            list.iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };

        // A class picker that still offers `max` while the served model tops
        // out at `xhigh` must not forward `max`.
        assert_eq!(
            clamp_budget_to_declared(MAX_BUDGET, &levels(&["low", "medium", "high", "xhigh"])),
            Some(XHIGH_BUDGET)
        );
        // The model's own ceiling is kept.
        assert_eq!(
            clamp_budget_to_declared(
                MAX_BUDGET,
                &levels(&["low", "medium", "high", "xhigh", "max"])
            ),
            None
        );
        // A request inside the declared range is untouched.
        assert_eq!(
            clamp_budget_to_declared(HIGH_BUDGET, &levels(&["low", "medium", "high"])),
            None
        );
        // No declaration is not a declaration of no support.
        assert_eq!(clamp_budget_to_declared(MAX_BUDGET, &levels(&[])), None);
        // Values outside the vocabulary cannot set a ceiling.
        assert_eq!(
            clamp_budget_to_declared(MAX_BUDGET, &levels(&["turbo"])),
            None
        );
    }
}
