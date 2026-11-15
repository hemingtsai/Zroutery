use serde_json::json;

use zroutery_core::ir::{ContentBlock, Dialect, MediaSource, StopReason};
use zroutery_core::protocol::anthropic;

// ========================================================================
// 2. Anthropic golden tests
// ========================================================================

#[test]
fn anthropic_text_request_round_trip() {
    let input = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 1024,
        "system": "Be helpful.",
        "messages": [{"role": "user", "content": "hello"}],
        "temperature": 0.5,
        "stop_sequences": ["END"]
    });

    let req = anthropic::decode_request(input).unwrap();
    assert_eq!(req.model, "claude-3-5-sonnet-20241022");
    assert_eq!(req.max_tokens, Some(1024));
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.stop_sequences, vec!["END"]);
    assert_eq!(req.source_dialect, Dialect::Anthropic);
    assert_eq!(req.system.len(), 1);
    assert_eq!(req.system[0].text, "Be helpful.");
    assert_eq!(req.messages.len(), 1);

    let encoded = anthropic::encode_request(&req, "claude-3-5-sonnet-20241022").unwrap();
    assert_eq!(encoded["model"], "claude-3-5-sonnet-20241022");
    assert_eq!(encoded["max_tokens"], 1024);
    assert_eq!(encoded["temperature"], 0.5);
    assert_eq!(encoded["stop_sequences"], json!(["END"]));
    assert_eq!(
        encoded["system"], "Be helpful.",
        "same-protocol forwarding retains the native string representation"
    );
}

#[test]
fn anthropic_image_request() {
    let input = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 256,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "What is this?"},
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "iVBORw0KGgo="
                    }
                }
            ]
        }]
    });

    let req = anthropic::decode_request(input).unwrap();
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].content.len(), 2);

    match &req.messages[0].content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "What is this?"),
        other => panic!("expected Text, got {:?}", other),
    }
    match &req.messages[0].content[1] {
        ContentBlock::Image {
            source: MediaSource::Base64 { media_type, data },
        } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "iVBORw0KGgo=");
        }
        other => panic!("expected Image, got {:?}", other),
    }

    // Re-encode and verify image survives.
    let encoded = anthropic::encode_request(&req, "claude-3-5-sonnet-20241022").unwrap();
    let content = encoded["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
}

#[test]
fn anthropic_document_request() {
    let input = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 512,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                    "data": "JVBERi0xLjQK"
                }
            }]
        }]
    });

    let req = anthropic::decode_request(input).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Document {
            source: MediaSource::Base64 { media_type, data },
        } => {
            assert_eq!(media_type, "application/pdf");
            assert_eq!(data, "JVBERi0xLjQK");
        }
        other => panic!("expected Document, got {:?}", other),
    }

    let encoded = anthropic::encode_request(&req, "claude-3-5-sonnet-20241022").unwrap();
    let content = encoded["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "document");
    assert_eq!(content[0]["source"]["media_type"], "application/pdf");
}

#[test]
fn anthropic_thinking_response() {
    let input = json!({
        "id": "msg_123",
        "type": "message",
        "model": "claude-3-5-sonnet-20241022",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 50,
            "output_tokens": 30,
            "cache_read_tokens": 0,
            "cache_creation_input_tokens": 0
        },
        "content": [
            {
                "type": "thinking",
                "thinking": "Let me think about this carefully...",
                "signature": "sig_abc"
            },
            {
                "type": "text",
                "text": "Here is my answer."
            }
        ]
    });

    let resp = anthropic::decode_response(input).unwrap();
    assert_eq!(resp.id, "msg_123");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 50);
    assert_eq!(resp.usage.output_tokens, 30);
    assert_eq!(resp.content.len(), 2);

    match &resp.content[0] {
        ContentBlock::Thinking { text, signature } => {
            assert_eq!(text, "Let me think about this carefully...");
            assert_eq!(signature.as_deref(), Some("sig_abc"));
        }
        other => panic!("expected Thinking, got {:?}", other),
    }
    match &resp.content[1] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "Here is my answer."),
        other => panic!("expected Text, got {:?}", other),
    }

    // Re-encode and verify thinking survives.
    let encoded = anthropic::encode_response(&resp);
    let content = encoded["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(
        content[0]["thinking"],
        "Let me think about this carefully..."
    );
    assert_eq!(content[0]["signature"], "sig_abc");
    assert_eq!(content[1]["type"], "text");
}

#[test]
fn anthropic_tool_use_round_trip() {
    let input = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 256,
        "messages": [
            {"role": "user", "content": "Search for something"},
            {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "search",
                    "input": {"query": "rust async"}
                }]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_123",
                    "content": "Found 5 results"
                }]
            }
        ],
        "tools": [{
            "name": "search",
            "description": "Search the web",
            "input_schema": {"type": "object", "properties": {"query": {"type": "string"}}}
        }]
    });

    let req = anthropic::decode_request(input).unwrap();
    assert_eq!(req.messages.len(), 3);
    assert_eq!(req.tools.len(), 1);

    match &req.messages[1].content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "toolu_123");
            assert_eq!(name, "search");
            assert_eq!(input["query"], "rust async");
        }
        other => panic!("expected ToolUse, got {:?}", other),
    }

    match &req.messages[2].content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => {
            assert_eq!(tool_use_id, "toolu_123");
            match &content[0] {
                zroutery_core::ir::ToolResultPart::Text { text } => {
                    assert_eq!(text, "Found 5 results");
                }
                _ => panic!("expected Text part"),
            }
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }

    let encoded = anthropic::encode_request(&req, "claude-3-5-sonnet-20241022").unwrap();
    let tool_block = &encoded["messages"][1]["content"][0];
    assert_eq!(tool_block["type"], "tool_use");
    assert_eq!(tool_block["name"], "search");
}
