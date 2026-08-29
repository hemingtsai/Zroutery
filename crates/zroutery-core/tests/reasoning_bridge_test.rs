//! Tests for the reasoning bridge between Anthropic thinking blocks and
//! OpenAI Responses-style encrypted reasoning items.

use serde_json::json;
use zroutery_core::ir::ContentBlock;
use zroutery_core::protocol::reasoning_bridge::{decode_reasoning_item, encode_thinking_block};

#[test]
fn thinking_block_round_trips() {
    let block = ContentBlock::Thinking {
        text: "let me think carefully".to_string(),
        signature: Some("sig_abc123".to_string()),
    };
    let item = encode_thinking_block(&block).expect("signed thinking should encode");
    assert_eq!(item["type"], "reasoning");
    assert!(item["encrypted_content"]
        .as_str()
        .unwrap()
        .starts_with("zroutery-reasoning-v1:"));

    let decoded = decode_reasoning_item(&item).expect("valid bridge should decode");
    assert_eq!(decoded, block);
}

#[test]
fn redacted_thinking_block_round_trips() {
    let block = ContentBlock::RedactedThinking {
        data: "encrypted-blob".to_string(),
    };
    let item = encode_thinking_block(&block).expect("redacted thinking should encode");
    let decoded = decode_reasoning_item(&item).expect("valid bridge should decode");
    assert_eq!(decoded, block);
}

#[test]
fn empty_signature_is_not_encoded() {
    let block = ContentBlock::Thinking {
        text: "unsigned reasoning".to_string(),
        signature: None,
    };
    assert!(encode_thinking_block(&block).is_none());

    let empty = ContentBlock::Thinking {
        text: "empty signature".to_string(),
        signature: Some(String::new()),
    };
    assert!(encode_thinking_block(&empty).is_none());
}

#[test]
fn corrupted_base64_safely_degrades() {
    let bad = json!({
        "type": "reasoning",
        "encrypted_content": "zroutery-reasoning-v1:%%%not-base64%%%"
    });
    assert!(decode_reasoning_item(&bad).is_none());

    let wrong_prefix = json!({
        "type": "reasoning",
        "encrypted_content": "other-vendor:abc"
    });
    assert!(decode_reasoning_item(&wrong_prefix).is_none());

    let missing = json!({"type": "reasoning"});
    assert!(decode_reasoning_item(&missing).is_none());

    let malformed_json = json!({
        "type": "reasoning",
        "encrypted_content": "zroutery-reasoning-v1:aGVsbG8"
    });
    assert!(decode_reasoning_item(&malformed_json).is_none());
}

#[test]
fn openai_assistant_reasoning_item_decodes_through_bridge() {
    // This is the shape a history turn takes after the OpenAI encoder has used
    // the bridge: the assistant message carries `reasoning[]` plus content.
    let item = encode_thinking_block(&ContentBlock::Thinking {
        text: "history reasoning".to_string(),
        signature: Some("sig-history".to_string()),
    })
    .unwrap();
    let body = json!({
        "model": "m",
        "messages": [{
            "role": "assistant",
            "content": "hello",
            "reasoning": [item]
        }]
    });
    let req = zroutery_core::protocol::openai::decode_request(body).unwrap();
    assert!(matches!(
        req.messages[0].content[0],
        ContentBlock::Thinking { .. }
    ));
    assert_eq!(req.messages[0].content[1], ContentBlock::text("hello"));
}

#[test]
fn openai_encoder_emits_bridged_reasoning_items_for_history() {
    let mut req = zroutery_core::ChatRequest::new("m", zroutery_core::Dialect::OpenAI);
    req.messages.push(zroutery_core::Message {
        role: zroutery_core::Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                text: "past".to_string(),
                signature: Some("sig-past".to_string()),
            },
            ContentBlock::text("answer"),
        ],
    });
    let body = zroutery_core::protocol::openai::encode_request(&req, "upstream").unwrap();
    let reasoning = &body["messages"][0]["reasoning"];
    assert!(reasoning.is_array());
    assert_eq!(reasoning[0]["type"], "reasoning");
}
