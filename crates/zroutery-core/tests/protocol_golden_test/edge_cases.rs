use serde_json::json;

use zroutery_core::ir::{ContentBlock, StopReason, UnsupportedContentPolicy};
use zroutery_core::protocol::anthropic;
use zroutery_core::protocol::gemini;
use zroutery_core::protocol::openai;
use zroutery_core::protocol::responses;

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
        ("minimal", true, Some(512)),
        ("low", true, Some(1024u32)),
        ("medium", true, Some(4096)),
        ("high", true, Some(16384)),
        ("xhigh", true, Some(32768)),
        ("max", true, Some(65536)),
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
    assert!(ir.messages.iter().any(|m| m
        .content
        .iter()
        .any(|c| matches!(c, ContentBlock::Audio { .. }))));

    // Encode to Anthropic — audio is not natively supported, but should
    // not error if we set the policy to Drop or Placeholder.
    let mut ir_for_anthropic = ir.clone();
    ir_for_anthropic.unsupported_content_policy = UnsupportedContentPolicy::Placeholder;
    let anthropic_req =
        anthropic::encode_request(&ir_for_anthropic, "claude-3-5-sonnet-20241022").unwrap();
    let content = anthropic_req["messages"][0]["content"].as_array().unwrap();
    // Should have a placeholder text instead of audio.
    assert_eq!(content[0]["type"], "text");
    assert!(content[0]["text"].as_str().unwrap().contains("Unsupported"));

    // Encode to OpenAI — audio IS natively supported.
    let openai_req = openai::encode_request(&ir, "gpt-4-audio-preview").unwrap();
    let messages = openai_req["messages"].as_array().unwrap();
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_audio");
}
