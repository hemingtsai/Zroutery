//! Tests for the OpenAI Responses API translation layer.

use serde_json::json;
use zroutery_core::ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, Role, StopReason, StreamEvent, Usage,
};
use zroutery_core::protocol::reasoning_bridge::encode_thinking_block;
use zroutery_core::protocol::responses::{
    decode_request, decode_response, encode_request, encode_response, ResponsesStreamEncoder,
    ResponsesStreamParser,
};
use zroutery_core::protocol::{SseDecoder, SseFrame, StreamEncoder, StreamParser};

#[test]
fn request_decodes_instructions_lifting_and_reasoning_bridge() {
    let reasoning_item = encode_thinking_block(&ContentBlock::Thinking {
        text: "past reasoning".to_string(),
        signature: Some("sig-1".to_string()),
    })
    .unwrap();

    let body = json!({
        "model": "gpt-5",
        "instructions": "be concise",
        "max_output_tokens": 256,
        "tool_choice": "required",
        "reasoning": {"effort": "high"},
        "tools": [{
            "type": "function",
            "name": "get_weather",
            "description": "weather",
            "parameters": {"type": "object"}
        }],
        "input": [
            {"type": "input_text", "text": "what's the weather"},
            {"type": "function_call", "call_id": "call_1", "name": "get_weather",
             "arguments": "{\"city\":\"SH\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "sunny"},
            reasoning_item
        ]
    });

    let req = decode_request(body).unwrap();
    assert_eq!(req.model, "gpt-5");
    assert_eq!(req.system[0].text, "be concise");
    assert_eq!(req.max_tokens, Some(256));
    assert_eq!(req.tool_choice, Some(zroutery_core::ToolChoice::Any));
    assert_eq!(req.thinking.unwrap().budget_tokens, Some(16384));
    assert_eq!(req.messages.len(), 4);
    assert!(matches!(req.messages[1].content[0], ContentBlock::ToolUse { .. }));
    assert!(matches!(req.messages[2].content[0], ContentBlock::ToolResult { .. }));
    assert!(matches!(req.messages[3].content[0], ContentBlock::Thinking { .. }));
}

#[test]
fn response_encodes_text_tool_use_and_reasoning() {
    let resp = ChatResponse {
        id: "resp_1".into(),
        model: "gpt-5".into(),
        content: vec![
            ContentBlock::text("hello"),
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "get_weather".into(),
                input: json!({"city": "SH"}),
            },
            ContentBlock::Thinking {
                text: "thinking text".into(),
                signature: Some("sig-2".into()),
            },
        ],
        stop_reason: StopReason::EndTurn,
        stop_sequence: None,
        usage: Usage::default(),
    };

    let value = encode_response(&resp);
    assert_eq!(value["object"], "response");
    assert_eq!(value["output_text"], "hello");
    let output = value["output"].as_array().unwrap();
    assert_eq!(output.len(), 3);
    assert_eq!(output[0]["type"], "message");
    assert_eq!(output[1]["type"], "function_call");
    assert_eq!(output[2]["type"], "reasoning");
    assert!(output[2]["encrypted_content"].is_string());
}

#[test]
fn stream_parser_handles_responses_lifecycle() {
    let raw = concat!(
        "event: response.created\ndata: ",
        r#"{"type":"response.created","response":{"id":"resp_1","model":"m"}}"#,
        "\n\n",
        "event: response.output_item.added\ndata: ",
        r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"f","arguments":""}}"#,
        "\n\n",
        "event: response.output_text.delta\ndata: ",
        r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"Hel"}"#,
        "\n\n",
        "event: response.reasoning_summary_text.delta\ndata: ",
        r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_1","output_index":1,"content_index":0,"delta":"think"}"#,
        "\n\n",
        "event: response.function_call_arguments.delta\ndata: ",
        r#"{"type":"response.function_call_arguments.delta","item_id":"call_1","output_index":0,"content_index":0,"delta":"{\"city\":\"SH\"}"}"#,
        "\n\n",
        "event: response.completed\ndata: ",
        r#"{"type":"response.completed","response":{"id":"resp_1","model":"m","usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}"#,
        "\n\n",
    );

    let mut dec = SseDecoder::new();
    let mut parser = ResponsesStreamParser::new("m");
    let mut events = Vec::new();
    for frame in dec.push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());

    assert!(events.iter().any(|e| matches!(e, StreamEvent::Start { .. })));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::TextDelta { text, .. } if text == "Hel")));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::ThinkingDelta { text, .. } if text == "think")));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolUseStart { id, .. } if id == "call_1")));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolUseDelta { partial_json, .. } if partial_json.contains("city"))));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::Stop { .. })));
}

#[test]
fn stream_encoder_emits_responses_sse() {
    let mut enc = ResponsesStreamEncoder::new("m");
    let mut frames: Vec<SseFrame> = Vec::new();
    frames.extend(enc.encode(&StreamEvent::Start {
        id: "resp_1".into(),
        model: "m".into(),
        usage: Usage::default(),
    }));
    frames.extend(enc.encode(&StreamEvent::TextDelta {
        index: 0,
        text: "hello".into(),
    }));
    frames.extend(enc.encode(&StreamEvent::ToolUseStart {
        index: 1,
        id: "call_1".into(),
        name: "f".into(),
    }));
    frames.extend(enc.encode(&StreamEvent::ToolUseDelta {
        index: 1,
        partial_json: "{\"a\":1}".into(),
    }));
    frames.extend(enc.encode(&StreamEvent::ThinkingDelta {
        index: 2,
        text: "think".into(),
    }));
    frames.extend(enc.encode(&StreamEvent::Stop {
        stop_reason: StopReason::EndTurn,
        stop_sequence: None,
        usage: Usage::default(),
    }));

    assert_eq!(frames[0].event.as_deref(), Some("response.created"));
    assert_eq!(frames[1].event.as_deref(), Some("response.output_text.delta"));
    assert_eq!(frames[2].event.as_deref(), Some("response.output_item.added"));
    assert_eq!(frames[3].event.as_deref(), Some("response.function_call_arguments.delta"));
    assert_eq!(frames[4].event.as_deref(), Some("response.reasoning_summary_text.delta"));
    assert_eq!(frames[5].event.as_deref(), Some("response.completed"));
}

#[test]
fn responses_request_round_trips_through_ir() {
    let mut req = ChatRequest::new("gpt-5", Dialect::OpenAIResponses);
    req.system.push(zroutery_core::SystemPart::new("sys"));
    req.max_tokens = Some(128);
    req.messages.push(zroutery_core::Message::user_text("hello"));
    req.messages.push(zroutery_core::Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "f".into(),
            input: json!({"x": 1}),
        }],
    });

    let wire = encode_request(&req, "upstream-model").unwrap();
    let decoded = decode_request(wire).unwrap();
    assert_eq!(decoded.system[0].text, "sys");
    assert_eq!(decoded.max_tokens, Some(128));
    assert_eq!(decoded.messages.len(), 2);
    assert!(matches!(
        decoded.messages[1].content[0],
        ContentBlock::ToolUse { .. }
    ));
}

#[test]
fn responses_response_round_trips_through_ir() {
    let resp = ChatResponse {
        id: "resp_rt".into(),
        model: "m".into(),
        content: vec![
            ContentBlock::text("hi"),
            ContentBlock::ToolUse {
                id: "call_9".into(),
                name: "f".into(),
                input: json!({"z": 2}),
            },
        ],
        stop_reason: StopReason::EndTurn,
        stop_sequence: None,
        usage: Usage {
            input_tokens: 2,
            output_tokens: 1,
            reasoning_tokens: 0,
            ..Usage::default()
        },
    };

    let wire = encode_response(&resp);
    let decoded = decode_response(wire).unwrap();
    assert_eq!(decoded.text(), "hi");
    assert!(matches!(decoded.content[1], ContentBlock::ToolUse { .. }));
    assert_eq!(decoded.usage.input_tokens, 2);
}
