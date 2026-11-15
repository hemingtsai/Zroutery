use serde_json::json;

use zroutery_core::ir::{ContentBlock, Dialect, MediaSource};
use zroutery_core::protocol::anthropic;
use zroutery_core::protocol::anthropic::AnthropicStreamEncoder;
use zroutery_core::protocol::gemini;
use zroutery_core::protocol::openai;
use zroutery_core::protocol::openai::OpenAiStreamParser;
use zroutery_core::protocol::responses;
use zroutery_core::protocol::{ProviderQuirks, SseDecoder, StreamEncoder, StreamParser};

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
    let assistant_msg = messages.iter().find(|m| m["role"] == "assistant").unwrap();
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
        .find(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|b| b["type"] == "tool_result"))
        })
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
    let assistant = messages.iter().find(|m| m["role"] == "assistant").unwrap();
    let calls = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["id"], "c1");
    assert_eq!(calls[0]["function"]["name"], "my_func");

    // Tool result should be a "tool" role message.
    let tool = messages.iter().find(|m| m["role"] == "tool").unwrap();
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
    // Text has a Chat Completions reasoning representation; a signature from
    // another provider is deliberately not claimed to be valid for this one.
    let openai_req = openai::encode_request(&ir, "deepseek-r1").unwrap();
    let messages = openai_req["messages"].as_array().unwrap();
    let assistant = messages.iter().find(|m| m["role"] == "assistant").unwrap();
    let content = assistant["content"].as_str().unwrap();
    assert_eq!(content, "Final answer.");
    assert_eq!(assistant["reasoning_content"], "Reasoning here...");
    assert!(assistant.get("reasoning").is_none());
    assert!(!openai_req.to_string().contains("sig1"));
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

#[test]
fn cross_protocol_anthropic_cache_usage_becomes_an_openai_total() {
    // The report's frame: 100 fresh input tokens, 10,000 cache reads, 200 cache
    // writes, no output. Anthropic reports the first number cache-exclusively,
    // so the IR total and therefore the OpenAI `prompt_tokens` is 10,300.
    let anthropic_wire = json!({
        "id": "msg_cache",
        "type": "message",
        "role": "assistant",
        "model": "claude-3-5-sonnet-20241022",
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 100,
            "output_tokens": 0,
            "cache_read_input_tokens": 10_000,
            "cache_creation_input_tokens": 200
        }
    });

    let ir = anthropic::decode_response(anthropic_wire).unwrap();
    assert_eq!(ir.usage.input_tokens, 10_300);

    let openai_wire = openai::encode_response(&ir);
    let prompt = openai_wire["usage"]["prompt_tokens"].as_u64().unwrap();
    let cached = openai_wire["usage"]["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap();
    assert_eq!(
        prompt, 10_300,
        "the prompt total must include the cache tokens"
    );
    assert_eq!(cached, 10_000);
    assert!(
        cached <= prompt,
        "cached_tokens must never exceed the prompt total"
    );

    // Decoding it back keeps the same total: no cache token is lost or doubled.
    let back = openai::decode_response(openai_wire).unwrap();
    assert_eq!(back.usage.input_tokens, 10_300);
    assert_eq!(back.usage.cache_read_tokens, 10_000);
}

#[test]
fn cross_protocol_openai_cache_usage_round_trips_to_anthropic() {
    // An OpenAI-form usage: a 10,100 token prompt of which 10,000 were served
    // from cache. Re-encoded for Anthropic it must split back into the same
    // cache-exclusive input plus its cache counter.
    let openai_wire = json!({
        "id": "chatcmpl-cache",
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10_100,
            "completion_tokens": 7,
            "total_tokens": 10_107,
            "prompt_tokens_details": {"cached_tokens": 10_000}
        }
    });

    let ir = openai::decode_response(openai_wire).unwrap();
    assert_eq!(ir.usage.input_tokens, 10_100);
    assert_eq!(ir.usage.cache_read_tokens, 10_000);

    let anthropic_wire = anthropic::encode_response(&ir);
    assert_eq!(anthropic_wire["usage"]["input_tokens"], 100);
    assert_eq!(anthropic_wire["usage"]["cache_read_input_tokens"], 10_000);
    assert_eq!(anthropic_wire["usage"]["cache_creation_input_tokens"], 0);
    assert_eq!(anthropic_wire["usage"]["output_tokens"], 7);

    let back = anthropic::decode_response(anthropic_wire).unwrap();
    assert_eq!(back.usage.input_tokens, 10_100);
    assert_eq!(back.usage.cache_read_tokens, 10_000);
    assert_eq!(back.usage.output_tokens, 7);
}

#[test]
fn cross_protocol_openai_stream_trailer_reaches_the_anthropic_final_usage() {
    // OpenAI sends no usage until the trailer, so the Anthropic `message_start`
    // frame necessarily carries zeros. The final `message_delta` must restate
    // the complete normalized usage or a streamed client can never learn it.
    let raw = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\n",
        "data: [DONE]\n\n",
    );

    let mut decoder = SseDecoder::new();
    let mut parser = OpenAiStreamParser::new("gpt-4");
    let mut events = Vec::new();
    for frame in decoder.push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());

    let mut encoder = AnthropicStreamEncoder::new("gpt-4");
    let mut wire = String::new();
    for event in &events {
        for frame in encoder.encode(event) {
            wire.push_str(&frame.to_wire());
        }
    }

    // Aggregate what an Anthropic client sees: `message_start` first, then the
    // final `message_delta` overriding it with the cache-exclusive numbers.
    let mut visible_input = 0u64;
    let mut visible_cache_read = 0u64;
    let mut visible_output = 0u64;
    for data in wire.lines().filter_map(|line| line.strip_prefix("data: ")) {
        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let usage = match value["type"].as_str() {
            Some("message_start") => value["message"]["usage"].clone(),
            Some("message_delta") => value["usage"].clone(),
            _ => continue,
        };
        if let Some(n) = usage["input_tokens"].as_u64() {
            visible_input = n;
        }
        if let Some(n) = usage["cache_read_input_tokens"].as_u64() {
            visible_cache_read = n;
        }
        if let Some(n) = usage["output_tokens"].as_u64() {
            visible_output = n;
        }
    }
    assert_eq!(visible_input, 60, "fresh input must reach the client");
    assert_eq!(visible_cache_read, 40, "cache reads must reach the client");
    assert_eq!(
        visible_input + visible_cache_read,
        100,
        "the client can reconstruct the full prompt total"
    );
    assert_eq!(visible_output, 10);
}
