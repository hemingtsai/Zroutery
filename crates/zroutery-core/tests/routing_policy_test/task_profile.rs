use serde_json::json;

use zroutery_core::config::ModelTier;
use zroutery_core::ir::{Capability, ChatRequest, ContentBlock, Dialect, MediaSource, ToolDef};
use zroutery_core::policy::{Complexity, TaskProfile, TaskType};

use super::support::*;

// ========================================================================
// 1. TaskProfile::from_request
// ========================================================================

#[test]
fn task_profile_from_simple_request() {
    let req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.complexity, Complexity::Simple);
    assert_eq!(profile.task_type, TaskType::Chat);
    assert!(!profile.has_tools);
    assert!(!profile.has_vision);
    assert!(!profile.streaming);
    assert_eq!(profile.context_tokens, 0);
    assert_eq!(profile.estimated_output_tokens, 4096);
    assert!(profile.required_capabilities.is_empty());
}

#[test]
fn task_profile_from_request_with_tools() {
    let mut req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    req.tools = vec![ToolDef {
        name: "search".into(),
        description: Some("Search the web".into()),
        input_schema: json!({}),
        cache_control: None,
    }];
    let profile = TaskProfile::from_request(&req);

    assert!(profile.has_tools);
    assert_eq!(profile.task_type, TaskType::ToolUse);
    assert_eq!(profile.complexity, Complexity::Standard);
    assert_eq!(profile.suggested_tier(), ModelTier::Standard);
}

#[test]
fn task_profile_from_request_with_thinking() {
    let mut req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    req.thinking = Some(zroutery_core::ir::ThinkingConfig {
        enabled: true,
        budget_tokens: Some(10000),
    });
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.complexity, Complexity::Complex);
    assert_eq!(profile.suggested_tier(), ModelTier::Reasoning);
}

#[test]
fn task_profile_from_request_with_vision_content() {
    let mut req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    req.messages = vec![zroutery_core::ir::Message {
        role: zroutery_core::ir::Role::User,
        content: vec![ContentBlock::Image {
            source: MediaSource::Base64 {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            },
        }],
    }];
    let profile = TaskProfile::from_request(&req);

    assert!(profile.has_vision);
    assert_eq!(profile.task_type, TaskType::Vision);
    assert!(profile.required_capabilities.contains(&Capability::Vision));
}

#[test]
fn task_profile_from_request_streaming() {
    let mut req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    req.stream = true;
    let profile = TaskProfile::from_request(&req);

    assert!(profile.streaming);
}

#[test]
fn task_profile_from_request_max_tokens_estimated() {
    let mut req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    req.max_tokens = Some(8192);
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.estimated_output_tokens, 8192);
}

#[test]
fn task_profile_from_request_default_max_tokens_is_4096() {
    let req = ChatRequest::new("gpt-4", Dialect::OpenAI);
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.estimated_output_tokens, 4096);
}

// ========================================================================
// 2. suggested_tier() for each complexity level
// ========================================================================

#[test]
fn suggested_tier_simple_is_fast() {
    let profile = make_profile(Complexity::Simple);
    assert_eq!(profile.suggested_tier(), ModelTier::Fast);
}

#[test]
fn suggested_tier_standard_is_standard() {
    let profile = make_profile(Complexity::Standard);
    assert_eq!(profile.suggested_tier(), ModelTier::Standard);
}

#[test]
fn suggested_tier_complex_is_reasoning() {
    let profile = make_profile(Complexity::Complex);
    assert_eq!(profile.suggested_tier(), ModelTier::Reasoning);
}

#[test]
fn suggested_tier_frontier_is_frontier() {
    let profile = make_profile(Complexity::Frontier);
    assert_eq!(profile.suggested_tier(), ModelTier::Frontier);
}

#[test]
fn complexity_ordering_is_monotonic() {
    assert!(Complexity::Simple < Complexity::Standard);
    assert!(Complexity::Standard < Complexity::Complex);
    assert!(Complexity::Complex < Complexity::Frontier);
}
