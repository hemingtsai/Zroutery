//! Regression tests for OpenAI Responses interoperability defects.
//!
//! These exercise the wire shapes an official Responses client actually sends
//! and consumes, rather than only the frames this repository generates.

use serde_json::json;
use zroutery_core::ir::{ContentBlock, Dialect, Role, ToolResultPart};
use zroutery_core::protocol::responses::decode_request;

#[test]
fn easy_input_message_without_type_decodes() {
    let body = json!({"model": "m", "input": [{"role": "user", "content": "Hi"}]});
    let req = decode_request(body).unwrap();
    assert_eq!(req.source_dialect, Dialect::OpenAIResponses);
    assert_eq!(req.messages.len(), 1);
    assert!(matches!(req.messages[0].role, Role::User));
    assert_eq!(req.messages[0].content[0].as_text(), Some("Hi"));
}

#[test]
fn message_string_content_decodes_for_both_roles() {
    let body = json!({"model": "m", "input": [
        {"type": "message", "role": "user", "content": "Hi"},
        {"type": "message", "role": "assistant", "content": "Hello"},
    ]});
    let req = decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 2);
    assert!(matches!(req.messages[0].role, Role::User));
    assert!(matches!(req.messages[1].role, Role::Assistant));
    assert_eq!(req.messages[1].content[0].as_text(), Some("Hello"));
}

#[test]
fn system_and_developer_messages_lift_to_instructions() {
    let body = json!({"model": "m", "input": [
        {"role": "system", "content": "be nice"},
        {"role": "developer", "content": [{"type": "input_text", "text": "be terse"}]},
        {"role": "user", "content": "hi"},
    ]});
    let req = decode_request(body).unwrap();
    assert_eq!(req.system.len(), 2);
    assert_eq!(req.system[0].text, "be nice");
    assert_eq!(req.system[1].text, "be terse");
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].content[0].as_text(), Some("hi"));
}

#[test]
fn unsupported_input_items_still_error() {
    let body = json!({"model": "m", "input": [{"type": "computer_call", "call_id": "x"}]});
    assert!(decode_request(body).is_err());
    let body = json!({"model": "m", "input": [{"foo": 1}]});
    assert!(decode_request(body).is_err());
    let body =
        json!({"model": "m", "input": [{"type": "message", "role": "tool", "content": "x"}]});
    assert!(decode_request(body).is_err());
}

#[test]
fn message_image_url_string_decodes() {
    let body = json!({"model": "m", "input": [{
        "type": "message",
        "role": "user",
        "content": [{"type": "input_image", "image_url": "https://example.com/photo.png"}],
    }]});
    let req = decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Image { source } => {
            assert_eq!(source.to_data_url(), "https://example.com/photo.png");
        }
        other => panic!("expected image, got {other:?}"),
    }
}

#[test]
fn image_url_data_url_decodes() {
    let data_url = "data:image/png;base64,AAAA";
    let body = json!({"model": "m", "input": [
        {"type": "input_image", "image_url": data_url},
        {"type": "message", "role": "user", "content": [
            {"type": "input_image", "image_url": data_url},
        ]},
    ]});
    let req = decode_request(body).unwrap();
    for message in &req.messages {
        match &message.content[0] {
            ContentBlock::Image { source } => assert_eq!(source.to_data_url(), data_url),
            other => panic!("expected image, got {other:?}"),
        }
    }
}

#[test]
fn tool_result_image_url_string_decodes() {
    let body = json!({"model": "m", "input": [{
        "type": "function_call_output",
        "call_id": "call_1",
        "output": [{"type": "input_image", "image_url": "https://example.com/tool.png"}],
    }]});
    let req = decode_request(body).unwrap();
    let ContentBlock::ToolResult { content, .. } = &req.messages[0].content[0] else {
        panic!("expected tool result");
    };
    let ToolResultPart::Image { source } = &content[0] else {
        panic!("expected image part");
    };
    assert_eq!(source.to_data_url(), "https://example.com/tool.png");
}

#[test]
fn legacy_nested_image_url_still_decodes() {
    let body = json!({"model": "m", "input": [{
        "type": "message",
        "role": "user",
        "content": [{"type": "input_image", "image_url": {"url": "https://example.com/a.png"}}],
    }]});
    let req = decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Image { source } => {
            assert_eq!(source.to_data_url(), "https://example.com/a.png");
        }
        other => panic!("expected image, got {other:?}"),
    }
}
