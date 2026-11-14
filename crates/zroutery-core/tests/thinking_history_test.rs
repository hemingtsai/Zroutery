//! Thinking history must remain reasoning, not fail or become user-visible text.
use serde_json::json;
use zroutery_core::ir::ContentBlock;
use zroutery_core::ir::StreamEvent;
use zroutery_core::protocol::{anthropic, gemini, responses};
use zroutery_core::protocol::{SseFrame, StreamParser};

#[test]
fn claude_empty_signature_history_reaches_responses() {
    for signature in [None, Some(""), Some("native-anthropic-signature")] {
        let mut block = json!({"type":"thinking", "thinking":"prior reasoning"});
        if let Some(signature) = signature {
            block["signature"] = json!(signature);
        }
        let req = anthropic::decode_request(json!({
            "model":"frontier-class", "max_tokens":4096,
            "messages":[{"role":"assistant", "content":[block,
                {"type":"text", "text":"answer"}]},
                {"role":"user", "content":"continue"}]
        }))
        .unwrap();
        let body = responses::encode_request(&req, "target").unwrap();
        let reasoning = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "reasoning")
            .unwrap();
        if signature.is_some_and(|signature| !signature.is_empty()) {
            assert!(reasoning["encrypted_content"].is_string());
        } else {
            assert_eq!(reasoning["summary"][0]["text"], "prior reasoning");
        }
        assert_eq!(
            body["input"].as_array().unwrap().last().unwrap()["content"][0]["text"],
            "continue"
        );
    }
}

#[test]
fn claude_thinking_reaches_gemini_without_a_foreign_signature() {
    let req = anthropic::decode_request(json!({
        "model":"frontier-class", "max_tokens":4096,
        "messages":[{"role":"assistant", "content":[
            {"type":"thinking", "thinking":"prior reasoning", "signature":"anthropic-only"},
            {"type":"text", "text":"answer"}]},
            {"role":"user", "content":"continue"}]
    }))
    .unwrap();
    let body = gemini::encode_request(&req, "target").unwrap();
    assert_eq!(
        body["contents"][0]["parts"][0],
        json!({"thought":true, "text":"prior reasoning"})
    );
    assert_eq!(body["contents"][1]["parts"][0]["text"], "continue");
}

#[test]
fn native_gemini_thought_history_keeps_its_signature() {
    let req = gemini::decode_request(json!({"model":"m", "contents":[
        {"role":"model", "parts":[{"thought":true, "text":"reasoning", "thoughtSignature":"native"},
            {"text":"answer"}]}]}))
    .unwrap();
    assert!(
        matches!(&req.messages[0].content[0], ContentBlock::Thinking { signature: Some(s), .. } if s == "native")
    );
    let encoded = gemini::encode_request(&req, "m").unwrap();
    assert_eq!(
        encoded["contents"][0]["parts"][0]["thoughtSignature"],
        "native"
    );
    assert_eq!(encoded["contents"][0]["parts"][1], json!({"text":"answer"}));
}

#[test]
fn gemini_thoughts_are_not_answer_text_in_buffered_or_streaming_replies() {
    let reply = json!({"candidates":[{"content":{"parts":[
        {"thought":true, "text":"private reasoning", "thoughtSignature":"native"},
        {"text":"answer"}]}, "finishReason":"STOP"}]});
    let decoded = gemini::decode_response(reply.clone()).unwrap();
    assert_eq!(decoded.text(), "answer");
    assert!(matches!(decoded.content[0], ContentBlock::Thinking { .. }));
    let mut parser = gemini::GeminiStreamParser::new("m");
    let events = parser
        .push(&SseFrame {
            event: None,
            data: reply.to_string(),
        })
        .unwrap();
    assert!(events.iter().any(|event| matches!(event, StreamEvent::ThinkingDelta { text, .. } if text == "private reasoning")));
    assert!(events.iter().any(|event| matches!(event, StreamEvent::ThinkingSignature { signature, .. } if signature == "native")));
    assert!(!events.iter().any(
        |event| matches!(event, StreamEvent::TextDelta { text, .. } if text == "private reasoning")
    ));
    let blocks = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ThinkingDelta { index, .. } | StreamEvent::TextDelta { index, .. } => {
                Some(*index)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_ne!(blocks[0], blocks[1]);
}

#[test]
fn opaque_redacted_history_still_requires_an_explicit_policy_for_gemini() {
    let req = anthropic::decode_request(json!({
        "model":"m", "max_tokens":4096, "messages":[{"role":"assistant", "content":[
            {"type":"redacted_thinking", "data":"anthropic-opaque"},
            {"type":"text", "text":"answer"}]}]
    }))
    .unwrap();
    let error = gemini::encode_request(&req, "target").unwrap_err();
    assert!(error.to_string().contains("redacted thinking"));
}
