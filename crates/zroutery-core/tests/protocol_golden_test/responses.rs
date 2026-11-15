use serde_json::json;

use zroutery_core::ir::{ContentBlock, Dialect, MediaSource, StopReason};
use zroutery_core::protocol::responses;

// ========================================================================
// 4. Responses API golden tests
// ========================================================================

#[test]
fn responses_text_request_round_trip() {
    let body = json!({
        "model": "gpt-5",
        "instructions": "Be concise.",
        "max_output_tokens": 128,
        "input": [
            {"type": "input_text", "text": "What is 2+2?"}
        ]
    });

    let req = responses::decode_request(body).unwrap();
    assert_eq!(req.model, "gpt-5");
    assert_eq!(req.system.len(), 1);
    assert_eq!(req.system[0].text, "Be concise.");
    assert_eq!(req.max_tokens, Some(128));
    assert_eq!(req.source_dialect, Dialect::OpenAIResponses);
    assert_eq!(req.messages.len(), 1);

    let encoded = responses::encode_request(&req, "upstream-model").unwrap();
    assert_eq!(encoded["model"], "upstream-model");
    assert_eq!(encoded["instructions"], "Be concise.");
    assert_eq!(encoded["max_output_tokens"], 128);
    let input = encoded["input"].as_array().unwrap();
    assert_eq!(input[0]["type"], "input_text");
    assert_eq!(input[0]["text"], "What is 2+2?");
}

#[test]
fn responses_input_audio_decodes() {
    let body = json!({
        "model": "gpt-5",
        "input": [{
            "type": "input_audio",
            "input_audio": {
                "data": "SGVsbG8=",
                "format": "wav"
            }
        }]
    });

    let req = responses::decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 1);
    match &req.messages[0].content[0] {
        ContentBlock::Audio {
            source: MediaSource::Base64 { media_type, data },
            media_type: mt,
        } => {
            assert_eq!(media_type, "audio/wav");
            assert_eq!(mt, "audio/wav");
            assert_eq!(data, "SGVsbG8=");
        }
        other => panic!("expected Audio, got {:?}", other),
    }

    // Re-encode to Responses wire format.
    let encoded = responses::encode_request(&req, "gpt-5").unwrap();
    let input = encoded["input"].as_array().unwrap();
    // A native Responses upstream keeps the client's original item placement.
    // Moving an item into a new message can shift positional tool declarations.
    assert_eq!(input[0]["type"], "input_audio");
    assert_eq!(input[0]["input_audio"]["data"], "SGVsbG8=");
    assert_eq!(input[0]["input_audio"]["format"], "wav");
}

#[test]
fn responses_function_call_output() {
    let body = json!({
        "model": "gpt-5",
        "input": [
            {"type": "input_text", "text": "Get weather"},
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"Tokyo\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "sunny, 25C"
            }
        ],
        "tools": [{
            "type": "function",
            "name": "get_weather",
            "parameters": {"type": "object"}
        }]
    });

    let req = responses::decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 3);
    assert_eq!(req.tools.len(), 1);

    match &req.messages[1].content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "get_weather");
            assert_eq!(input["city"], "Tokyo");
        }
        other => panic!("expected ToolUse, got {:?}", other),
    }

    match &req.messages[2].content[0] {
        ContentBlock::ToolResult { tool_use_id, .. } => {
            assert_eq!(tool_use_id, "call_1");
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }

    let encoded = responses::encode_request(&req, "gpt-5").unwrap();
    let input = encoded["input"].as_array().unwrap();
    let fc = input.iter().find(|i| i["type"] == "function_call").unwrap();
    assert_eq!(fc["call_id"], "call_1");
    assert_eq!(fc["name"], "get_weather");

    let fco = input
        .iter()
        .find(|i| i["type"] == "function_call_output")
        .unwrap();
    assert_eq!(fco["call_id"], "call_1");
    assert_eq!(fco["output"], "sunny, 25C");
}

#[test]
fn responses_sampling_params_decode() {
    let body = json!({
        "model": "gpt-5",
        "temperature": 0.8,
        "top_p": 0.95,
        "input": [{"type": "input_text", "text": "hi"}]
    });

    let req = responses::decode_request(body).unwrap();
    assert_eq!(req.temperature, Some(0.8));
    assert_eq!(req.top_p, Some(0.95));

    let encoded = responses::encode_request(&req, "gpt-5").unwrap();
    assert_eq!(encoded["temperature"], 0.8);
    assert_eq!(encoded["top_p"], 0.95);
}

#[test]
fn responses_response_round_trip() {
    let resp_wire = json!({
        "id": "resp_123",
        "object": "response",
        "model": "gpt-5",
        "status": "completed",
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "Hello!", "annotations": []}]
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15
        }
    });

    let resp = responses::decode_response(resp_wire).unwrap();
    assert_eq!(resp.id, "resp_123");
    assert_eq!(resp.text(), "Hello!");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);

    let encoded = responses::encode_response(&resp);
    assert_eq!(encoded["object"], "response");
    assert_eq!(encoded["output_text"], "Hello!");
}
