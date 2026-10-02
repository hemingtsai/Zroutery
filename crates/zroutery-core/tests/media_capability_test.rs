//! Canonical request-derived capability and media evidence tests.

use std::sync::Arc;

use serde_json::json;
use zroutery_core::config::{AppConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind};
use zroutery_core::ir::{
    Capability, ChatRequest, ContentBlock, Dialect, MediaSource, Message, Role, ThinkingConfig,
    ToolDef, ToolResultPart,
};
use zroutery_core::protocol::{anthropic, gemini, openai, responses};

fn base64(media_type: &str) -> MediaSource {
    MediaSource::Base64 {
        media_type: media_type.into(),
        data: "AAAA".into(),
    }
}

#[test]
fn capability_derivation_is_exhaustive_deduplicated_and_canonically_ordered() {
    let mut request = ChatRequest::new("m", Dialect::Anthropic);
    request.messages = vec![Message {
        role: Role::User,
        content: vec![
            ContentBlock::Document {
                source: base64("application/pdf"),
            },
            ContentBlock::File {
                source: base64("application/zip"),
                media_type: "application/zip".into(),
                name: Some("archive.zip".into()),
            },
            ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                name: "lookup".into(),
                content: vec![
                    ToolResultPart::Text {
                        text: "result".into(),
                    },
                    ToolResultPart::Image {
                        source: base64("image/png"),
                    },
                ],
                is_error: false,
            },
            ContentBlock::Audio {
                source: base64("audio/wav"),
                media_type: "audio/wav".into(),
            },
            ContentBlock::Video {
                source: base64("video/mp4"),
                media_type: "video/mp4".into(),
            },
            ContentBlock::Thinking {
                text: "reasoning".into(),
                signature: None,
            },
        ],
    }];
    request.tools.push(ToolDef {
        name: "lookup".into(),
        description: None,
        input_schema: json!({"type": "object"}),
        cache_control: None,
    });
    request.thinking = Some(ThinkingConfig {
        enabled: true,
        budget_tokens: Some(1024),
    });

    request.refresh_required_capabilities();
    assert_eq!(
        request.required_capabilities,
        vec![
            Capability::Vision,
            Capability::Audio,
            Capability::Video,
            Capability::Files,
            Capability::Tools,
            Capability::Thinking,
        ]
    );

    // Repeated forms and a changed traversal order do not change the evidence.
    request.messages[0].content.push(ContentBlock::Image {
        source: base64("image/jpeg"),
    });
    request.refresh_required_capabilities();
    assert_eq!(
        request.compute_required_capabilities(),
        request.required_capabilities
    );
    assert_eq!(
        request
            .compute_required_capabilities()
            .iter()
            .filter(|cap| *cap == &Capability::Vision)
            .count(),
        1
    );
}

#[test]
fn virtual_registry_capabilities_do_not_promote_unknown_members() {
    let mut config = AppConfig::default();
    config.providers.push(ProviderConfig::new(
        "p",
        "P",
        ProviderKind::OpenAICompatible,
    ));
    config.models.push(ModelEntry::for_upstream(
        "p",
        "audio",
        Some(ModelTier::Standard),
    ));
    config.models.push(ModelEntry::for_upstream(
        "p",
        "unknown",
        Some(ModelTier::Standard),
    ));
    config.models[0].capabilities.audio = true;
    let registry = zroutery_core::registry::Registry::new(Arc::new(config));
    let virtual_model = registry
        .list()
        .into_iter()
        .find(|model| model.virtual_model)
        .unwrap();
    assert!(!virtual_model.capabilities.audio);
}

#[test]
fn every_decoder_records_the_same_canonical_requirements() {
    let anthropic = anthropic::decode_request(json!({
        "model": "m",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "AAAA"}},
                {"type": "tool_result", "tool_use_id": "t1", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
                ]}
            ]
        }]
    }))
    .unwrap();
    assert!(anthropic.required_capabilities.contains(&Capability::Files));
    assert!(anthropic
        .required_capabilities
        .contains(&Capability::Vision));
    assert!(anthropic.required_capabilities.contains(&Capability::Tools));

    let openai = openai::decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": [
            {"type": "file", "media_type": "application/pdf", "file_data": "AAAA"},
            {"type": "input_audio", "input_audio": {"format": "wav", "data": "AAAA"}}
        ]}],
        "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}]
    }))
    .unwrap();
    assert!(openai.required_capabilities.contains(&Capability::Files));
    assert!(openai.required_capabilities.contains(&Capability::Audio));
    assert!(openai.required_capabilities.contains(&Capability::Tools));

    let responses = responses::decode_request(json!({
        "model": "m",
        "input": [{"type": "input_file", "file_data": "AAAA", "media_type": "application/pdf"}]
    }))
    .unwrap();
    assert!(responses.required_capabilities.contains(&Capability::Files));

    let responses_image_tool_result = responses::decode_request(json!({
        "model": "m",
        "input": [{
            "type": "function_call_output",
            "call_id": "call-1",
            "output": [{
                "type": "input_image",
                "image_url": {"url": "data:image/png;base64,AAAA"}
            }]
        }]
    }))
    .unwrap();
    assert!(responses_image_tool_result
        .required_capabilities
        .contains(&Capability::Vision));
    assert!(responses_image_tool_result
        .required_capabilities
        .contains(&Capability::Tools));

    let gemini = gemini::decode_request(json!({
        "model": "m",
        "contents": [{"role": "user", "parts": [{"inlineData": {"mimeType": "video/mp4", "data": "AAAA"}}]}]
    }))
    .unwrap();
    assert!(gemini.required_capabilities.contains(&Capability::Video));
}
