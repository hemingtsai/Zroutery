//! Fail-closed protocol and media-policy tests.

use serde_json::json;
use zroutery_core::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, Message, Role, ToolResultPart,
    UnsupportedContentPolicy,
};
use zroutery_core::protocol::{
    anthropic, apply_content_policy, evaluate_content_policy, gemini, openai, ContentPolicyOutcome,
    ProviderQuirks, SseFrame, StreamParser,
};

fn audio_request() -> ChatRequest {
    let mut request = ChatRequest::new("m", Dialect::Gemini);
    request.messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Audio {
            source: MediaSource::Base64 {
                media_type: "audio/wav".into(),
                data: "AAAA".into(),
            },
            media_type: "audio/wav".into(),
        }],
    });
    request
}

#[test]
fn unknown_anthropic_content_is_rejected_instead_of_dropped() {
    let error = anthropic::decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": [{"type": "server_side_tool", "secret": "do-not-copy"}]}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unsupported message content"));
    assert!(!error.to_string().contains("do-not-copy"));
}

#[test]
fn unknown_openai_content_is_rejected_instead_of_dropped() {
    let error = openai::decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": [{"type": "input_video", "secret": "do-not-copy"}]}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unsupported message content"));
    assert!(!error.to_string().contains("do-not-copy"));
}

#[test]
fn unknown_responses_content_is_rejected_instead_of_dropped() {
    let error = zroutery_core::protocol::responses::decode_request(json!({
        "model": "m",
        "input": [{"type": "computer_call", "secret": "do-not-copy"}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unsupported input item content"));
    assert!(!error.to_string().contains("do-not-copy"));
}

#[test]
fn unknown_gemini_content_is_rejected_instead_of_dropped() {
    let error = gemini::decode_request(json!({
        "model": "m",
        "contents": [{"role": "user", "parts": [{"videoMetadata": {"secret": "do-not-copy"}}]}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unsupported message content"));
    assert!(!error.to_string().contains("do-not-copy"));
}

#[test]
fn transform_requires_a_replacement_and_drop_is_an_explicit_non_silent_outcome() {
    let block = ContentBlock::Audio {
        source: MediaSource::Base64 {
            media_type: "audio/wav".into(),
            data: "AAAA".into(),
        },
        media_type: "audio/wav".into(),
    };

    assert!(evaluate_content_policy(UnsupportedContentPolicy::Transform, &block).is_err());
    assert!(apply_content_policy(UnsupportedContentPolicy::Transform, &block).is_err());
    assert_eq!(
        evaluate_content_policy(UnsupportedContentPolicy::Drop, &block).unwrap(),
        ContentPolicyOutcome::Drop
    );
    assert_eq!(
        serde_json::to_string(&ContentPolicyOutcome::Drop).unwrap(),
        "{\"action\":\"drop\"}"
    );
    assert!(apply_content_policy(UnsupportedContentPolicy::Drop, &block).is_err());
    assert!(matches!(
        evaluate_content_policy(UnsupportedContentPolicy::Placeholder, &block).unwrap(),
        ContentPolicyOutcome::Replacement(ContentBlock::Text { .. })
    ));
    assert_eq!(
        UnsupportedContentPolicy::default(),
        UnsupportedContentPolicy::Reject
    );
}

#[test]
fn default_policy_rejects_media_loss_but_placeholder_is_explicit() {
    let request = audio_request();
    assert!(anthropic::encode_request(&request, "m").is_err());

    let mut placeholder = request.clone();
    placeholder.unsupported_content_policy = UnsupportedContentPolicy::Placeholder;
    let encoded = anthropic::encode_request(&placeholder, "m").unwrap();
    let content = encoded["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert!(content[0]["text"].as_str().unwrap().contains("Unsupported"));
}

#[test]
fn unknown_response_content_is_rejected_instead_of_dropped() {
    assert!(anthropic::decode_response(json!({
        "id": "m", "content": [{"type": "server_tool", "secret": "hidden"}]
    }))
    .is_err());
    assert!(openai::decode_response(json!({
        "id": "m", "choices": [{"message": {"content": [
            {"type": "input_video", "secret": "hidden"}
        ]}}]
    }))
    .is_err());
    assert!(zroutery_core::protocol::responses::decode_response(json!({
        "id": "r", "output": [{"type": "computer_call", "secret": "hidden"}]
    }))
    .is_err());
    assert!(gemini::decode_response(json!({
        "candidates": [{"content": {"parts": [{"videoMetadata": {"secret": "hidden"}}]}}]
    }))
    .is_err());
}

#[test]
fn image_tool_results_are_forwarded_or_explicitly_replaced() {
    let request = |source| {
        let mut request = ChatRequest::new("m", Dialect::Anthropic);
        request.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                name: "lookup".into(),
                content: vec![ToolResultPart::Image { source }],
                is_error: false,
            }],
        });
        request
    };

    // A chat upstream has no image part inside a `tool` message, but a vision
    // model can still receive one as a user image part. It is forwarded in a
    // follow-up user message, not stringified and not lost.
    let encoded = openai::encode_request_with(
        &request(MediaSource::Base64 {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        }),
        "m",
        &ProviderQuirks::default(),
    )
    .unwrap();
    let messages = encoded["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "tool");
    assert_eq!(messages[1]["role"], "user");
    let content = messages[1]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image_url");
    assert!(content[1]["image_url"]["url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png;base64,"));

    // A server-side file id is not resolvable by the upstream, so the explicit
    // policy still decides what happens to it.
    let mut reference = request(MediaSource::Reference {
        id: "file-1".into(),
    });
    assert!(openai::encode_request_with(&reference, "m", &ProviderQuirks::default()).is_err());
    reference.unsupported_content_policy = UnsupportedContentPolicy::Placeholder;
    let encoded = openai::encode_request_with(&reference, "m", &ProviderQuirks::default()).unwrap();
    assert!(encoded["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Unsupported"));
}

#[test]
fn unknown_stream_content_is_not_silently_ignored() {
    let mut anthropic = anthropic::AnthropicStreamParser::new("m");
    assert!(anthropic
        .push(&SseFrame {
            event: Some("content_block_start".into()),
            data: json!({"type": "content_block_start", "index": 0,
                         "content_block": {"type": "server_tool"}})
            .to_string(),
        })
        .is_err());

    let mut openai = openai::OpenAiStreamParser::new("m");
    assert!(openai
        .push(&SseFrame {
            event: None,
            data: json!({"choices": [{"index": 0, "delta": {
                "content": [{"type": "input_video", "secret": "hidden"}]
            }}]})
            .to_string(),
        })
        .is_err());

    let mut responses = zroutery_core::protocol::responses::ResponsesStreamParser::new("m");
    assert!(responses
        .push(&SseFrame {
            event: Some("response.content_part.added".into()),
            data: json!({"type": "response.content_part.added",
                         "part": {"type": "input_video"}})
            .to_string(),
        })
        .is_err());

    let mut gemini = gemini::GeminiStreamParser::new("m");
    assert!(gemini
        .push(&SseFrame {
            event: None,
            data: json!({"candidates": [{"content": {"parts": [
                {"videoMetadata": {"secret": "hidden"}}
            ]}}]})
            .to_string(),
        })
        .is_err());
}
