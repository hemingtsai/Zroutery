use serde_json::json;

use zroutery_core::ir::{ContentBlock, Dialect, MediaSource, StopReason};
use zroutery_core::protocol::gemini;

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
    assert_eq!(
        encoded["system_instruction"]["parts"][0]["text"],
        "be brief"
    );
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
    assert_eq!(resp_parts[0]["functionResponse"]["name"], "get_weather");
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
    let parts = encoded["candidates"][0]["content"]["parts"]
        .as_array()
        .unwrap();
    assert_eq!(parts[0]["text"], "The answer is 42.");
    assert_eq!(parts[1]["functionCall"]["name"], "f");
}
