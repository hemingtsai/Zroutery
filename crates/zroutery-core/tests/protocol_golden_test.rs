//! Protocol golden conformance tests.
//!
//! These tests verify that wire-format to IR conversions are correct by testing
//! against known-good fixtures.  Each test decodes a protocol-specific JSON
//! payload into the canonical IR, asserts on the IR fields, then re-encodes
//! and verifies key fields survive the round-trip.

use serde_json::json;

use zroutery_core::ir::{
    ContentBlock, Dialect, MediaSource, Role, StopReason, UnsupportedContentPolicy,
};
use zroutery_core::protocol::anthropic;
use zroutery_core::protocol::gemini;
use zroutery_core::protocol::openai;
use zroutery_core::protocol::responses;
use zroutery_core::protocol::ProviderQuirks;

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
    let assistant_msg = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let calls = assistant_msg["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_abc123");
    assert_eq!(calls[0]["function"]["name"], "get_weather");

    // The tool result message should be role "tool".
    let tool_msg = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
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
    assert_eq!(encoded["system"][0]["text"], "Be helpful.");
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
    assert_eq!(content[0]["thinking"], "Let me think about this carefully...");
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
            tool_use_id, content, ..
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

// ========================================================================
// 3. Gemini golden tests
// ========================================================================

#[test]
fn gemini_text_request_round_trip() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "system_instruction": {"parts": [{"text": "be brief"}]},
        "contents": [
            {"role": "user", "parts": [{"text": "hello"}]}
        ],
        "generationConfig": {
            "maxOutputTokens": 128,
            "temperature": 0.5,
            "topP": 0.9
        }
    });

    let req = gemini::decode_request(body).unwrap();
    assert_eq!(req.model, "gemini-2.0-flash");
    assert_eq!(req.system.len(), 1);
    assert_eq!(req.system[0].text, "be brief");
    assert_eq!(req.max_tokens, Some(128));
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.top_p, Some(0.9));
    assert_eq!(req.source_dialect, Dialect::Gemini);

    let encoded = gemini::encode_request(&req, "gemini-2.0-flash").unwrap();
    assert_eq!(encoded["model"], "gemini-2.0-flash");
    assert_eq!(encoded["generationConfig"]["maxOutputTokens"], 128);
    assert_eq!(encoded["generationConfig"]["temperature"], 0.5);
    assert_eq!(encoded["system_instruction"]["parts"][0]["text"], "be brief");
}

#[test]
fn gemini_inline_audio_decodes_correctly() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "inlineData": {
                    "mimeType": "audio/mp3",
                    "data": "SGVsbG8="
                }
            }]
        }]
    });
    let req = gemini::decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Audio {
            source: MediaSource::Base64 { media_type, data },
            media_type: mt,
        } => {
            assert_eq!(media_type, "audio/mp3");
            assert_eq!(mt, "audio/mp3");
            assert_eq!(data, "SGVsbG8=");
        }
        other => panic!("expected Audio, got {:?}", other),
    }
}

#[test]
fn gemini_inline_video_decodes_correctly() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "inlineData": {
                    "mimeType": "video/mp4",
                    "data": "AAAA"
                }
            }]
        }]
    });
    let req = gemini::decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Video {
            source: MediaSource::Base64 { media_type, data },
            media_type: mt,
        } => {
            assert_eq!(media_type, "video/mp4");
            assert_eq!(mt, "video/mp4");
            assert_eq!(data, "AAAA");
        }
        other => panic!("expected Video, got {:?}", other),
    }
}

#[test]
fn gemini_inline_pdf_decodes_to_document() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "inlineData": {
                    "mimeType": "application/pdf",
                    "data": "JVBERi0xLjQK"
                }
            }]
        }]
    });
    let req = gemini::decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Document {
            source: MediaSource::Base64 { media_type, data },
        } => {
            assert_eq!(media_type, "application/pdf");
            assert_eq!(data, "JVBERi0xLjQK");
        }
        other => panic!("expected Document, got {:?}", other),
    }
}

#[test]
fn gemini_function_call_round_trip() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "contents": [
            {"role": "user", "parts": [{"text": "What's the weather?"}]},
            {"role": "model", "parts": [{
                "functionCall": {"id": "c1", "name": "get_weather", "args": {"city": "Tokyo"}}
            }]},
            {"role": "user", "parts": [{
                "functionResponse": {"id": "c1", "name": "get_weather", "response": {"temp": 25}}
            }]}
        ],
        "tools": [{
            "functionDeclarations": [{
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object"}
            }]
        }]
    });

    let req = gemini::decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 3);
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.tools[0].name, "get_weather");

    match &req.messages[1].content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "c1");
            assert_eq!(name, "get_weather");
            assert_eq!(input["city"], "Tokyo");
        }
        other => panic!("expected ToolUse, got {:?}", other),
    }

    match &req.messages[2].content[0] {
        ContentBlock::ToolResult { tool_use_id, .. } => {
            assert_eq!(tool_use_id, "c1");
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }

    // Re-encode and verify function calls survive.
    let encoded = gemini::encode_request(&req, "gemini-2.0-flash").unwrap();
    let contents = encoded["contents"].as_array().unwrap();
    let model_parts = contents[1]["parts"].as_array().unwrap();
    assert_eq!(model_parts[0]["functionCall"]["name"], "get_weather");
    assert_eq!(model_parts[0]["functionCall"]["args"]["city"], "Tokyo");

    let resp_parts = contents[2]["parts"].as_array().unwrap();
    assert_eq!(
        resp_parts[0]["functionResponse"]["name"],
        "get_weather"
    );
}

#[test]
fn gemini_response_decode_encodes_round_trip() {
    let resp_wire = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "The answer is 42."},
                    {"functionCall": {"id": "c1", "name": "f", "args": {"x": 1}}}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 5,
            "totalTokenCount": 15
        }
    });

    let resp = gemini::decode_response(resp_wire).unwrap();
    assert_eq!(resp.text(), "The answer is 42.");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert_eq!(resp.content.len(), 2);

    match &resp.content[1] {
        ContentBlock::ToolUse { id, name, .. } => {
            assert_eq!(id, "c1");
            assert_eq!(name, "f");
        }
        other => panic!("expected ToolUse, got {:?}", other),
    }

    let encoded = gemini::encode_response(&resp);
    let parts = encoded["candidates"][0]["content"]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "The answer is 42.");
    assert_eq!(parts[1]["functionCall"]["name"], "f");
}

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
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
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
    // Audio inside a message gets encoded as input_audio in message content.
    let msg = input
        .iter()
        .find(|i| i["type"] == "message")
        .expect("should have a message item");
    let content = msg["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_audio");
    assert_eq!(content[0]["input_audio"]["data"], "SGVsbG8=");
    assert_eq!(content[0]["input_audio"]["format"], "wav");
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
    let fc = input
        .iter()
        .find(|i| i["type"] == "function_call")
        .unwrap();
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

// ========================================================================
// 5. Cross-protocol golden tests
// ========================================================================

#[test]
fn cross_protocol_openai_to_anthropic() {
    let openai_req = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helpful."},
            {"role": "user", "content": "hello"}
        ],
        "temperature": 0.5,
        "max_tokens": 256
    });

    let ir = openai::decode_request(openai_req).unwrap();
    assert_eq!(ir.source_dialect, Dialect::OpenAI);

    let anthropic_req = anthropic::encode_request(&ir, "claude-3-5-sonnet-20241022").unwrap();
    assert_eq!(anthropic_req["model"], "claude-3-5-sonnet-20241022");
    assert_eq!(anthropic_req["temperature"], 0.5);
    assert_eq!(anthropic_req["max_tokens"], 256);

    // System prompt becomes the "system" field.
    let system = anthropic_req["system"].as_array().unwrap();
    assert_eq!(system[0]["text"], "You are helpful.");

    // User message is preserved.
    let messages = anthropic_req["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "user");
}

#[test]
fn cross_protocol_anthropic_to_openai() {
    let anthropic_req = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 512,
        "system": [
            {"type": "text", "text": "You are a security monitor.",
             "cache_control": {"type": "ephemeral"}}
        ],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "transcript",
                                          "cache_control": {"type": "ephemeral"}}]}
        ],
        "temperature": 0,
        "stop_sequences": ["</block>"]
    });

    let ir = anthropic::decode_request(anthropic_req).unwrap();
    assert_eq!(ir.source_dialect, Dialect::Anthropic);

    let openai_req =
        openai::encode_request_with(&ir, "glm-5.3", &ProviderQuirks::default()).unwrap();
    assert_eq!(openai_req["model"], "glm-5.3");
    assert_eq!(openai_req["max_tokens"], 512);
    assert_eq!(openai_req["temperature"], 0.0);
    assert_eq!(openai_req["stop"], json!(["</block>"]));

    // System becomes the first message.
    let messages = openai_req["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are a security monitor.");
}

#[test]
fn cross_protocol_gemini_audio_to_openai() {
    let gemini_req = json!({
        "model": "gemini-2.0-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "inlineData": {
                    "mimeType": "audio/mp3",
                    "data": "SGVsbG8="
                }
            }]
        }]
    });

    let ir = gemini::decode_request(gemini_req).unwrap();
    // Verify audio is in the IR.
    match &ir.messages[0].content[0] {
        ContentBlock::Audio {
            source: MediaSource::Base64 { media_type, data },
            ..
        } => {
            assert_eq!(media_type, "audio/mp3");
            assert_eq!(data, "SGVsbG8=");
        }
        other => panic!("expected Audio, got {:?}", other),
    }

    // Encode to OpenAI format.
    let openai_req = openai::encode_request(&ir, "gpt-4-audio-preview").unwrap();
    let messages = openai_req["messages"].as_array().unwrap();
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_audio");
    // audio/mp3 normalizes to "mp3" format.
    assert_eq!(content[0]["input_audio"]["format"], "mp3");
    assert_eq!(content[0]["input_audio"]["data"], "SGVsbG8=");
}

#[test]
fn cross_protocol_openai_audio_to_gemini() {
    let openai_req = json!({
        "model": "gpt-4-audio-preview",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "input_audio",
                "input_audio": {
                    "format": "mp3",
                    "data": "SGVsbG8="
                }
            }]
        }]
    });

    let ir = openai::decode_request(openai_req).unwrap();
    // Encode to Gemini format.
    let gemini_req = gemini::encode_request(&ir, "gemini-2.0-flash").unwrap();
    let contents = gemini_req["contents"].as_array().unwrap();
    let parts = contents[0]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["inlineData"]["mimeType"], "audio/mp3");
    assert_eq!(parts[0]["inlineData"]["data"], "SGVsbG8=");
}

#[test]
fn cross_protocol_openai_tools_to_anthropic() {
    let openai_req = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": "Search for something"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc",
                    "type": "function",
                    "function": {
                        "name": "search",
                        "arguments": "{\"query\":\"rust\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "tool_call_id": "call_abc",
                "content": "results here"
            }
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search tool",
                "parameters": {"type": "object"}
            }
        }]
    });

    let ir = openai::decode_request(openai_req).unwrap();

    // Encode to Anthropic format.
    let anthropic_req = anthropic::encode_request(&ir, "claude-3-5-sonnet-20241022").unwrap();
    let messages = anthropic_req["messages"].as_array().unwrap();

    // Tool calls should be in the assistant message.
    let assistant_msg = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let tool_use = assistant_msg["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "tool_use")
        .unwrap();
    assert_eq!(tool_use["id"], "call_abc");
    assert_eq!(tool_use["name"], "search");

    // Tool result should be in a user message.
    let user_msg = messages
        .iter()
        .find(|m| m["role"] == "user" && m["content"].as_array().map_or(false, |a| a.iter().any(|b| b["type"] == "tool_result")))
        .expect("should have user message with tool_result");
    let tool_result = user_msg["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "tool_result")
        .unwrap();
    assert_eq!(tool_result["tool_use_id"], "call_abc");

    // Tools definition should be present.
    let tools = anthropic_req["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "search");
}

#[test]
fn cross_protocol_gemini_tools_to_openai() {
    let gemini_req = json!({
        "model": "gemini-2.0-flash",
        "contents": [
            {"role": "user", "parts": [{"text": "call a function"}]},
            {"role": "model", "parts": [{
                "functionCall": {"id": "c1", "name": "my_func", "args": {"k": "v"}}
            }]},
            {"role": "user", "parts": [{
                "functionResponse": {"id": "c1", "name": "my_func", "response": {"ok": true}}
            }]}
        ],
        "tools": [{
            "functionDeclarations": [{
                "name": "my_func",
                "description": "A function",
                "parameters": {"type": "object"}
            }]
        }]
    });

    let ir = gemini::decode_request(gemini_req).unwrap();
    let openai_req = openai::encode_request(&ir, "gpt-4").unwrap();

    let messages = openai_req["messages"].as_array().unwrap();
    // Assistant message should have tool_calls.
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let calls = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["id"], "c1");
    assert_eq!(calls[0]["function"]["name"], "my_func");

    // Tool result should be a "tool" role message.
    let tool = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(tool["tool_call_id"], "c1");
}

#[test]
fn cross_protocol_gemini_image_to_anthropic() {
    let gemini_req = json!({
        "model": "gemini-2.0-flash",
        "contents": [{
            "role": "user",
            "parts": [
                {"text": "What is this?"},
                {
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": "iVBORw0KGgo="
                    }
                }
            ]
        }]
    });

    let ir = gemini::decode_request(gemini_req).unwrap();
    assert_eq!(ir.messages[0].content.len(), 2);

    match &ir.messages[0].content[1] {
        ContentBlock::Image {
            source: MediaSource::Base64 { media_type, data },
        } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "iVBORw0KGgo=");
        }
        other => panic!("expected Image, got {:?}", other),
    }

    let anthropic_req = anthropic::encode_request(&ir, "claude-3-5-sonnet-20241022").unwrap();
    let content = anthropic_req["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
}

#[test]
fn cross_protocol_openai_reasoning_to_anthropic_thinking() {
    let openai_req = json!({
        "model": "deepseek-r1",
        "messages": [
            {"role": "user", "content": "think step by step"},
            {
                "role": "assistant",
                "reasoning_content": "Let me reason through this...",
                "content": "The answer is 42."
            }
        ]
    });

    let ir = openai::decode_request(openai_req).unwrap();
    assert_eq!(ir.messages[1].content.len(), 2);

    // First block should be Thinking, second should be Text.
    match &ir.messages[1].content[0] {
        ContentBlock::Thinking { text, .. } => {
            assert_eq!(text, "Let me reason through this...");
        }
        other => panic!("expected Thinking, got {:?}", other),
    }
    match &ir.messages[1].content[1] {
        ContentBlock::Text { text, .. } => {
            assert_eq!(text, "The answer is 42.");
        }
        other => panic!("expected Text, got {:?}", other),
    }

    // Encode to Anthropic format — thinking survives.
    let anthropic_req = anthropic::encode_request(&ir, "claude-3-5-sonnet-20241022").unwrap();
    let assistant = anthropic_req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let content = assistant["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "Let me reason through this...");
}

#[test]
fn cross_protocol_anthropic_thinking_to_openai_reasoning() {
    let anthropic_req = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 512,
        "messages": [
            {"role": "user", "content": "think carefully"},
            {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "Reasoning here...", "signature": "sig1"},
                    {"type": "text", "text": "Final answer."}
                ]
            }
        ]
    });

    let ir = anthropic::decode_request(anthropic_req).unwrap();
    // Encode to OpenAI — thinking becomes reasoning_content.
    let openai_req = openai::encode_request(&ir, "deepseek-r1").unwrap();
    let messages = openai_req["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    // Note: The OpenAI encoder echoes reasoning only for OpenAI-sourced requests
    // (source_dialect == OpenAI). For Anthropic-sourced, thinking blocks are
    // dropped in assistant messages. So content should just be the text.
    let content = assistant["content"].as_str().unwrap();
    assert_eq!(content, "Final answer.");
    // reasoning_content should not be present since source_dialect is Anthropic.
    assert!(assistant.get("reasoning_content").is_none());
}

#[test]
fn cross_protocol_openai_to_gemini_basic() {
    let openai_req = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "Be brief."},
            {"role": "user", "content": "hello"}
        ],
        "temperature": 0.3,
        "max_tokens": 64
    });

    let ir = openai::decode_request(openai_req).unwrap();
    let gemini_req = gemini::encode_request(&ir, "gemini-2.0-flash").unwrap();

    assert_eq!(gemini_req["model"], "gemini-2.0-flash");
    assert_eq!(
        gemini_req["system_instruction"]["parts"][0]["text"],
        "Be brief."
    );
    assert_eq!(gemini_req["generationConfig"]["temperature"], 0.3);
    assert_eq!(gemini_req["generationConfig"]["maxOutputTokens"], 64);
    let contents = gemini_req["contents"].as_array().unwrap();
    assert_eq!(contents[0]["role"], "user");
}

#[test]
fn cross_protocol_gemini_to_anthropic_basic() {
    let gemini_req = json!({
        "model": "gemini-2.0-flash",
        "system_instruction": {"parts": [{"text": "Be helpful."}]},
        "contents": [
            {"role": "user", "parts": [{"text": "What is Rust?"}]}
        ],
        "generationConfig": {
            "maxOutputTokens": 256,
            "temperature": 0.7
        }
    });

    let ir = gemini::decode_request(gemini_req).unwrap();
    let anthropic_req = anthropic::encode_request(&ir, "claude-3-5-sonnet-20241022").unwrap();

    assert_eq!(anthropic_req["model"], "claude-3-5-sonnet-20241022");
    assert_eq!(anthropic_req["max_tokens"], 256);
    assert_eq!(anthropic_req["temperature"], 0.7);
    let system = anthropic_req["system"].as_array().unwrap();
    assert_eq!(system[0]["text"], "Be helpful.");
}

#[test]
fn cross_protocol_response_to_openai_basic() {
    let responses_req = json!({
        "model": "gpt-5",
        "instructions": "Be concise.",
        "max_output_tokens": 100,
        "input": [
            {"type": "input_text", "text": "hello"}
        ],
        "temperature": 0.6
    });

    let ir = responses::decode_request(responses_req).unwrap();
    assert_eq!(ir.source_dialect, Dialect::OpenAIResponses);

    // Encode to OpenAI Chat Completions format.
    let openai_req = openai::encode_request(&ir, "gpt-4").unwrap();
    assert_eq!(openai_req["model"], "gpt-4");
    assert_eq!(openai_req["max_tokens"], 100);
    assert_eq!(openai_req["temperature"], 0.6);
    let messages = openai_req["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "Be concise.");
    assert_eq!(messages[1]["role"], "user");
}

#[test]
fn cross_protocol_openai_to_responses_basic() {
    let openai_req = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "Be helpful."},
            {"role": "user", "content": "What is 2+2?"}
        ],
        "temperature": 0.5,
        "max_tokens": 64
    });

    let ir = openai::decode_request(openai_req).unwrap();
    let responses_req = responses::encode_request(&ir, "gpt-5").unwrap();

    assert_eq!(responses_req["model"], "gpt-5");
    assert_eq!(responses_req["instructions"], "Be helpful.");
    assert_eq!(responses_req["max_output_tokens"], 64);
    assert_eq!(responses_req["temperature"], 0.5);
    let input = responses_req["input"].as_array().unwrap();
    assert_eq!(input[0]["type"], "message");
}

// ========================================================================
// 6. Edge cases and regression tests
// ========================================================================

#[test]
fn openai_content_array_text_preserved_as_separate_blocks() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "world"}
            ]
        }]
    });

    let req = openai::decode_request(input).unwrap();
    // Each text entry in a user content array becomes its own ContentBlock.
    assert_eq!(req.messages[0].content.len(), 2);
    match &req.messages[0].content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "Hello "),
        other => panic!("expected Text, got {:?}", other),
    }
    match &req.messages[0].content[1] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "world"),
        other => panic!("expected Text, got {:?}", other),
    }

    // Re-encode: when all blocks are text, the encoder flattens to a string.
    let encoded = openai::encode_request(&req, "gpt-4").unwrap();
    let messages = encoded["messages"].as_array().unwrap();
    // Single-role text content gets concatenated.
    let content = messages[0]["content"].as_str().unwrap();
    assert_eq!(content, "Hello world");
}

#[test]
fn gemini_system_instruction_array() {
    let body = json!({
        "model": "gemini-2.0-flash",
        "system_instruction": {
            "parts": [
                {"text": "Part 1."},
                {"text": "Part 2."}
            ]
        },
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
    });

    let req = gemini::decode_request(body).unwrap();
    assert_eq!(req.system.len(), 2);
    assert_eq!(req.system[0].text, "Part 1.");
    assert_eq!(req.system[1].text, "Part 2.");
}

#[test]
fn anthropic_system_array_with_cache_control() {
    let input = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 256,
        "system": [
            {"type": "text", "text": "Cached system prompt.", "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": "More instructions."}
        ],
        "messages": [{"role": "user", "content": "hi"}]
    });

    let req = anthropic::decode_request(input).unwrap();
    assert_eq!(req.system.len(), 2);
    assert!(req.system[0].cache_control.is_some());
    assert!(req.system[1].cache_control.is_none());

    let encoded = anthropic::encode_request(&req, "claude-3-5-sonnet-20241022").unwrap();
    let system = encoded["system"].as_array().unwrap();
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn openai_max_completion_tokens_overrides_max_tokens() {
    let input = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 100,
        "max_completion_tokens": 200
    });

    let req = openai::decode_request(input).unwrap();
    // max_completion_tokens takes precedence.
    assert_eq!(req.max_tokens, Some(200));
}

#[test]
fn anthropic_response_stop_reasons() {
    let cases = vec![
        ("end_turn", StopReason::EndTurn),
        ("max_tokens", StopReason::MaxTokens),
        ("stop_sequence", StopReason::StopSequence),
        ("tool_use", StopReason::ToolUse),
    ];

    for (wire, expected) in cases {
        let resp = json!({
            "id": "msg_1",
            "type": "message",
            "model": "claude-3",
            "stop_reason": wire,
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "content": [{"type": "text", "text": "ok"}]
        });
        let decoded = anthropic::decode_response(resp).unwrap();
        assert_eq!(decoded.stop_reason, expected, "wire stop_reason={wire}");
    }
}

#[test]
fn openai_response_stop_reasons() {
    let cases = vec![
        ("stop", StopReason::EndTurn),
        ("length", StopReason::MaxTokens),
        ("tool_calls", StopReason::ToolUse),
        ("content_filter", StopReason::Refusal),
    ];

    for (wire, expected) in cases {
        let resp = json!({
            "id": "chatcmpl-1",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": wire
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let decoded = openai::decode_response(resp).unwrap();
        assert_eq!(decoded.stop_reason, expected, "wire finish_reason={wire}");
    }
}

#[test]
fn responses_reasoning_effort_decode() {
    let cases = vec![
        ("none", false, None),
        ("minimal", false, None),
        ("low", true, Some(1024u32)),
        ("medium", true, Some(4096)),
        ("high", true, Some(16384)),
    ];

    for (effort, expected_enabled, expected_budget) in cases {
        let body = json!({
            "model": "gpt-5",
            "reasoning": {"effort": effort},
            "input": [{"type": "input_text", "text": "hi"}]
        });
        let req = responses::decode_request(body).unwrap();
        let th = req.thinking.unwrap();
        assert_eq!(
            th.enabled, expected_enabled,
            "effort={effort} enabled mismatch"
        );
        assert_eq!(
            th.budget_tokens, expected_budget,
            "effort={effort} budget mismatch"
        );
    }
}

#[test]
fn cross_protocol_round_trip_preserves_audio_capability() {
    // Start with Gemini audio request.
    let gemini_req = json!({
        "model": "gemini-2.0-flash",
        "contents": [{
            "role": "user",
            "parts": [{
                "inlineData": {
                    "mimeType": "audio/wav",
                    "data": "UklGRg=="
                }
            }]
        }]
    });

    let ir = gemini::decode_request(gemini_req).unwrap();
    // Verify the IR contains Audio content.
    assert!(ir
        .messages
        .iter()
        .any(|m| m.content.iter().any(|c| matches!(c, ContentBlock::Audio { .. }))));

    // Encode to Anthropic — audio is not natively supported, but should
    // not error if we set the policy to Drop or Placeholder.
    let mut ir_for_anthropic = ir.clone();
    ir_for_anthropic.unsupported_content_policy = UnsupportedContentPolicy::Placeholder;
    let anthropic_req =
        anthropic::encode_request(&ir_for_anthropic, "claude-3-5-sonnet-20241022").unwrap();
    let content = anthropic_req["messages"][0]["content"].as_array().unwrap();
    // Should have a placeholder text instead of audio.
    assert_eq!(content[0]["type"], "text");
    assert!(content[0]["text"]
        .as_str()
        .unwrap()
        .contains("Unsupported"));

    // Encode to OpenAI — audio IS natively supported.
    let openai_req = openai::encode_request(&ir, "gpt-4-audio-preview").unwrap();
    let messages = openai_req["messages"].as_array().unwrap();
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_audio");
}

// ============================================================
// ResponsesStreamEncoder regression tests
// ============================================================

use zroutery_core::protocol::StreamEncoder;
use zroutery_core::protocol::responses::ResponsesStreamEncoder;
use zroutery_core::ir::{StreamEvent, Usage};

/// Helper: collect all SSE frames from encoding a sequence of events.
fn encode_events(events: &[StreamEvent]) -> Vec<String> {
    let mut enc = ResponsesStreamEncoder::new("test-model");
    let mut frames = Vec::new();
    for e in events {
        for f in enc.encode(e) {
            frames.push(format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data));
        }
    }
    for f in enc.finish() {
        frames.push(format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data));
    }
    frames
}

fn find_event(frames: &[String], event_type: &str) -> Vec<serde_json::Value> {
    frames.iter()
        .filter(|f| f.starts_with(event_type))
        .map(|f| serde_json::from_str(f.lines().last().unwrap()).unwrap())
        .collect()
}

#[test]
fn stream_text_starts_at_output_index_0() {
    let events = encode_events(&[
        StreamEvent::Start { id: "r1".into(), model: "m".into(), usage: Usage::default() },
        StreamEvent::TextDelta { index: 0, text: "hello".into() },
        StreamEvent::BlockStop { index: 0 },
        StreamEvent::Stop { stop_reason: StopReason::EndTurn, stop_sequence: None, usage: Usage::default() },
    ]);
    let added = find_event(&events, "response.output_item.added");
    assert!(!added.is_empty());
    assert_eq!(added[0]["output_index"], 0, "first item should be at index 0");
    assert_eq!(added[0]["item"]["type"], "message");
}

#[test]
fn stream_text_emits_output_text_done() {
    let events = encode_events(&[
        StreamEvent::Start { id: "r1".into(), model: "m".into(), usage: Usage::default() },
        StreamEvent::TextDelta { index: 0, text: "hello".into() },
        StreamEvent::Stop { stop_reason: StopReason::EndTurn, stop_sequence: None, usage: Usage::default() },
    ]);
    let done = find_event(&events, "response.output_text.done");
    assert!(!done.is_empty(), "should emit output_text.done");
    assert_eq!(done[0]["text"], "hello");
}

#[test]
fn stream_content_part_has_item_id() {
    let events = encode_events(&[
        StreamEvent::Start { id: "r1".into(), model: "m".into(), usage: Usage::default() },
        StreamEvent::TextDelta { index: 0, text: "x".into() },
        StreamEvent::Stop { stop_reason: StopReason::EndTurn, stop_sequence: None, usage: Usage::default() },
    ]);
    let added = find_event(&events, "response.content_part.added");
    assert!(!added.is_empty());
    assert!(added[0]["item_id"].as_str().is_some(), "content_part.added must have item_id");
}

#[test]
fn stream_parallel_tools_no_interference() {
    let events = encode_events(&[
        StreamEvent::Start { id: "r1".into(), model: "m".into(), usage: Usage::default() },
        StreamEvent::ToolUseStart { index: 0, id: "call_a".into(), name: "fn_a".into() },
        StreamEvent::ToolUseStart { index: 1, id: "call_b".into(), name: "fn_b".into() },
        StreamEvent::ToolUseDelta { index: 0, partial_json: "{\"x\":1}".into() },
        StreamEvent::ToolUseDelta { index: 1, partial_json: "{\"y\":2}".into() },
        StreamEvent::BlockStop { index: 1 }, // B finishes first
        StreamEvent::BlockStop { index: 0 }, // A finishes second
        StreamEvent::Stop { stop_reason: StopReason::EndTurn, stop_sequence: None, usage: Usage::default() },
    ]);
    // Both tools should have added + done events.
    let added = find_event(&events, "response.output_item.added");
    assert_eq!(added.len(), 2, "should have 2 output_item.added");
    let done = find_event(&events, "response.output_item.done");
    assert_eq!(done.len(), 2, "should have 2 output_item.done");
    // Output should be sorted by output_index.
    let completed = find_event(&events, "response.completed");
    let output = completed[0]["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0]["type"], "function_call");
    assert_eq!(output[1]["type"], "function_call");
    // A was index 0, B was index 1.
    assert_eq!(output[0]["call_id"], "call_a");
    assert_eq!(output[1]["call_id"], "call_b");
    // Arguments should be strings, not parsed objects.
    assert!(output[0]["arguments"].is_string(), "arguments must be JSON string");
    assert!(output[1]["arguments"].is_string(), "arguments must be JSON string");
}

#[test]
fn stream_incomplete_tool_on_finish() {
    // Tool starts but never gets BlockStop — finish() should emit terminal events.
    let mut enc = ResponsesStreamEncoder::new("test-model");
    let mut frames = Vec::new();
    frames.extend(enc.encode(&StreamEvent::Start { id: "r1".into(), model: "m".into(), usage: Usage::default() }));
    frames.extend(enc.encode(&StreamEvent::ToolUseStart { index: 0, id: "call_a".into(), name: "fn".into() }));
    frames.extend(enc.encode(&StreamEvent::ToolUseDelta { index: 0, partial_json: "{\"x\":".into() }));
    // No BlockStop — finish() called directly.
    frames.extend(enc.finish());
    let wire: Vec<String> = frames.iter().map(|f| format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data)).collect();
    // Should have arguments.done + output_item.done with status=incomplete.
    let args_done = find_event(&wire, "response.function_call_arguments.done");
    assert_eq!(args_done.len(), 1, "should emit arguments.done for incomplete tool");
    let item_done = find_event(&wire, "response.output_item.done");
    assert_eq!(item_done.len(), 1, "should emit output_item.done for incomplete tool");
    let completed = find_event(&wire, "response.completed");
    assert_eq!(completed[0]["response"]["status"], "incomplete");
    let output = completed[0]["response"]["output"].as_array().unwrap();
    assert_eq!(output[0]["status"], "incomplete");
    // Arguments should be the partial JSON as a string.
    assert_eq!(output[0]["arguments"], "{\"x\":");
}

#[test]
fn stream_error_terminal_is_only_failed() {
    let mut enc = ResponsesStreamEncoder::new("test-model");
    let mut frames = Vec::new();
    frames.extend(enc.encode(&StreamEvent::Start { id: "r1".into(), model: "m".into(), usage: Usage::default() }));
    frames.extend(enc.encode(&StreamEvent::TextDelta { index: 0, text: "partial".into() }));
    frames.extend(enc.error(&zroutery_core::Error::internal("test error")));
    let wire: Vec<String> = frames.iter().map(|f| format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data)).collect();
    // Should have response.failed but NO response.completed.
    let failed = find_event(&wire, "response.failed");
    assert_eq!(failed.len(), 1, "should have exactly one response.failed");
    let completed = find_event(&wire, "response.completed");
    assert!(completed.is_empty(), "should NOT have response.completed after error");
}
