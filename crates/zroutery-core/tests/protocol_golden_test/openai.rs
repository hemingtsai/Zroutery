use serde_json::json;

use zroutery_core::ir::{ContentBlock, Dialect, MediaSource, Role, StopReason};
use zroutery_core::protocol::openai;

// ========================================================================
// 1. OpenAI Chat Completions golden tests
// ========================================================================

#[test]
fn openai_text_request_round_trip() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
        "temperature": 0.7,
        "max_tokens": 100
    });
    let req = openai::decode_request(input.clone()).unwrap();
    assert_eq!(req.model, "gpt-4");
    assert_eq!(req.temperature, Some(0.7));
    assert_eq!(req.max_tokens, Some(100));
    assert_eq!(req.source_dialect, Dialect::OpenAI);
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].role, Role::User);

    // Re-encode and verify key fields survive.
    let encoded = openai::encode_request(&req, "gpt-4").unwrap();
    assert_eq!(encoded["model"], "gpt-4");
    assert_eq!(encoded["temperature"], 0.7);
    assert_eq!(encoded["max_tokens"], 100);
    assert_eq!(encoded["messages"][0]["role"], "user");
    assert_eq!(encoded["messages"][0]["content"], "hello");
}

#[test]
fn openai_tool_call_round_trip() {
    let input = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": "What's the weather?"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc123",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Tokyo\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "tool_call_id": "call_abc123",
                "content": "sunny, 25C"
            }
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
            }
        }],
        "tool_choice": "auto"
    });

    let req = openai::decode_request(input.clone()).unwrap();
    assert_eq!(req.model, "gpt-4");
    assert_eq!(req.messages.len(), 3);
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.tools[0].name, "get_weather");

    // The assistant message should contain a ToolUse block.
    let assistant = &req.messages[1];
    assert_eq!(assistant.role, Role::Assistant);
    match &assistant.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_abc123");
            assert_eq!(name, "get_weather");
            assert_eq!(input["city"], "Tokyo");
        }
        other => panic!("expected ToolUse, got {:?}", other),
    }

    // The tool result should be a ToolResult block inside a user message.
    let tool_result_msg = &req.messages[2];
    match &tool_result_msg.content[0] {
        ContentBlock::ToolResult { tool_use_id, .. } => {
            assert_eq!(tool_use_id, "call_abc123");
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }

    // Re-encode and verify tool calls survive.
    let encoded = openai::encode_request(&req, "gpt-4").unwrap();
    let messages = encoded["messages"].as_array().unwrap();
    // The assistant message should have tool_calls.
    let assistant_msg = messages.iter().find(|m| m["role"] == "assistant").unwrap();
    let calls = assistant_msg["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_abc123");
    assert_eq!(calls[0]["function"]["name"], "get_weather");

    // The tool result message should be role "tool".
    let tool_msg = messages.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(tool_msg["tool_call_id"], "call_abc123");
}

#[test]
fn openai_input_audio_decodes_to_ir_audio() {
    let input = json!({
        "model": "gpt-4-audio-preview",
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "input_audio",
                    "input_audio": {
                        "format": "wav",
                        "data": "SGVsbG8gV29ybGQ="
                    }
                }
            ]
        }]
    });

    let req = openai::decode_request(input).unwrap();
    assert_eq!(req.messages.len(), 1);
    match &req.messages[0].content[0] {
        ContentBlock::Audio {
            source: MediaSource::Base64 { media_type, data },
            media_type: mt,
        } => {
            assert_eq!(media_type, "audio/wav");
            assert_eq!(mt, "audio/wav");
            assert_eq!(data, "SGVsbG8gV29ybGQ=");
        }
        other => panic!("expected Audio, got {:?}", other),
    }

    // Re-encode to OpenAI wire format.
    let encoded = openai::encode_request(&req, "gpt-4-audio-preview").unwrap();
    let messages = encoded["messages"].as_array().unwrap();
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_audio");
    assert_eq!(content[0]["input_audio"]["format"], "wav");
    assert_eq!(content[0]["input_audio"]["data"], "SGVsbG8gV29ybGQ=");
}

#[test]
fn openai_response_with_tool_calls() {
    let input = json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_xyz",
                    "type": "function",
                    "function": {
                        "name": "search",
                        "arguments": "{\"query\":\"rust lang\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });

    let resp = openai::decode_response(input).unwrap();
    assert_eq!(resp.id, "chatcmpl-123");
    assert_eq!(resp.model, "gpt-4");
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert_eq!(resp.content.len(), 1);

    match &resp.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_xyz");
            assert_eq!(name, "search");
            assert_eq!(input["query"], "rust lang");
        }
        other => panic!("expected ToolUse, got {:?}", other),
    }

    // Re-encode and verify tool calls survive.
    let encoded = openai::encode_response(&resp);
    let choices = encoded["choices"].as_array().unwrap();
    let msg = &choices[0]["message"];
    let calls = msg["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["function"]["name"], "search");
    assert_eq!(encoded["usage"]["prompt_tokens"], 10);
}

#[test]
fn openai_system_message_round_trip() {
    let input = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "hi"}
        ]
    });

    let req = openai::decode_request(input).unwrap();
    assert_eq!(req.system.len(), 1);
    assert_eq!(req.system[0].text, "You are a helpful assistant.");
    assert_eq!(req.messages.len(), 1);

    let encoded = openai::encode_request(&req, "gpt-4").unwrap();
    let messages = encoded["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are a helpful assistant.");
}
