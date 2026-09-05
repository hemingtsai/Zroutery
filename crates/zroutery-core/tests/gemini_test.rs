//! Tests for the Gemini native API translation layer.

use serde_json::json;
use zroutery_core::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, Role, StopReason, StreamEvent, Usage,
};
use zroutery_core::protocol::gemini::{
    decode_request, decode_response, encode_request, encode_response, GeminiStreamEncoder,
    GeminiStreamParser,
};
use zroutery_core::protocol::{SseDecoder, StreamEncoder, StreamParser};

#[test]
fn request_decodes_system_contents_and_function_calls() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "system_instruction": {"parts": [{"text": "be brief"}]},
        "contents": [
            {"role": "user", "parts": [{"text": "hello"}]},
            {"role": "model", "parts": [{"functionCall": {"id": "c1", "name": "f", "args": {"x": 1}}}]},
            {"role": "user", "parts": [{"functionResponse": {"id": "c1", "name": "f", "response": {"ok": true}}}]}
        ],
        "generationConfig": {"maxOutputTokens": 128, "temperature": 0.5},
        "tools": [{"functionDeclarations": [{"name": "f", "parameters": {"type": "object"}}]}]
    });

    let req = decode_request(body).unwrap();
    assert_eq!(req.system[0].text, "be brief");
    assert_eq!(req.max_tokens, Some(128));
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.messages.len(), 3);
    assert!(matches!(
        req.messages[1].content[0],
        ContentBlock::ToolUse { .. }
    ));
    assert!(matches!(
        req.messages[2].content[0],
        ContentBlock::ToolResult { .. }
    ));
}

#[test]
fn response_round_trips_through_ir() {
    let resp = ChatResponse {
        id: "gemini_1".into(),
        model: "gemini-2.0-flash".into(),
        content: vec![
            ContentBlock::text("hi"),
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "f".into(),
                input: json!({"x": 1}),
            },
        ],
        stop_reason: StopReason::EndTurn,
        stop_sequence: None,
        usage: Usage {
            input_tokens: 4,
            output_tokens: 2,
            ..Usage::default()
        },
    };
    let wire = encode_response(&resp);
    let decoded = decode_response(wire).unwrap();
    assert_eq!(decoded.text(), "hi");
    assert!(matches!(decoded.content[1], ContentBlock::ToolUse { .. }));
    assert_eq!(decoded.usage.input_tokens, 4);
}

#[test]
fn request_round_trips_through_ir() {
    let mut req = ChatRequest::new("gemini-2.0-flash", Dialect::Gemini);
    req.system.push(zroutery_core::SystemPart::new("sys"));
    req.messages
        .push(zroutery_core::Message::user_text("hello"));
    req.messages.push(zroutery_core::Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "c1".into(),
            name: "f".into(),
            input: json!({"x": 1}),
        }],
    });
    req.max_tokens = Some(64);
    req.tool_choice = Some(zroutery_core::ToolChoice::Specific { name: "f".into() });
    let wire = encode_request(&req, "gemini-2.0-flash").unwrap();
    assert_eq!(
        wire["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
        "f"
    );

    let decoded = decode_request(wire).unwrap();
    assert_eq!(decoded.system[0].text, "sys");
    assert_eq!(decoded.messages.len(), 2);
    assert!(matches!(
        decoded.messages[1].content[0],
        ContentBlock::ToolUse { .. }
    ));
}

#[test]
fn stream_parser_emits_text_and_stop() {
    let raw = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]}}]}\n\n\
               data: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}\n\n";
    let mut dec = SseDecoder::new();
    let mut parser = GeminiStreamParser::new("gemini-2.0-flash");
    let mut events = Vec::new();
    for frame in dec.push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::Start { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::TextDelta { text, .. } if text == "hello")));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::Stop { .. })));
}

#[test]
fn stream_encoder_emits_text_and_completion() {
    let mut enc = GeminiStreamEncoder::new("gemini-2.0-flash");
    let mut frames = Vec::new();
    frames.extend(enc.encode(&StreamEvent::Start {
        id: "g1".into(),
        model: "gemini-2.0-flash".into(),
        usage: Usage::default(),
    }));
    frames.extend(enc.encode(&StreamEvent::TextDelta {
        index: 0,
        text: "hi".into(),
    }));
    frames.extend(enc.encode(&StreamEvent::Stop {
        stop_reason: StopReason::EndTurn,
        stop_sequence: None,
        usage: Usage::default(),
    }));
    assert!(frames[0].data.contains("\"text\":\"hi\""));
    assert!(frames[1].data.contains("\"finishReason\":\"STOP\""));
}

#[test]
fn request_decode_missing_model_returns_error() {
    let body = json!({
        "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
    });
    let result = decode_request(body);
    assert!(result.is_err());
}

#[test]
fn stream_parser_handles_malformed_data_gracefully() {
    let mut parser = GeminiStreamParser::new("gemini-2.0-flash");
    // Empty candidates array
    let result = parser.push(&zroutery_core::protocol::SseFrame {
        event: None,
        data: "{\"candidates\":[]}".to_string(),
    });
    // Should not panic
    assert!(result.is_ok());

    // Missing candidates key
    let result = parser.push(&zroutery_core::protocol::SseFrame {
        event: None,
        data: "{\"usageMetadata\":{}}".to_string(),
    });
    assert!(result.is_ok());

    // Invalid JSON
    let result = parser.push(&zroutery_core::protocol::SseFrame {
        event: None,
        data: "not json".to_string(),
    });
    // Should return error, not panic
    assert!(result.is_err());
}
