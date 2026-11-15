use zroutery_core::config::{ModelCapabilities, ModelTier};
use zroutery_core::ir::Capability;
use zroutery_core::policy::{
    Complexity, PolicyMatcher, PolicyRequirements, RejectionReason, TaskType,
};

use super::support::*;

// ========================================================================
// 3. Policy matching with multiple matchers
// ========================================================================

#[test]
fn policy_empty_matchers_always_matches() {
    let policy = default_policy();
    let ctx = simple_match_ctx("any-model");
    assert!(policy.matches(&ctx));
}

#[test]
fn policy_disabled_never_matches() {
    let mut policy = default_policy();
    policy.enabled = false;
    let ctx = simple_match_ctx("any-model");
    assert!(!policy.matches(&ctx));
}

#[test]
fn policy_single_client_matcher() {
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::Client {
        value: "codex".into(),
    }];
    let mut ctx = simple_match_ctx("m");
    ctx.client_id = Some("codex");
    assert!(policy.matches(&ctx));
    ctx.client_id = Some("other");
    assert!(!policy.matches(&ctx));
}

#[test]
fn policy_multiple_matchers_all_must_pass() {
    let mut policy = default_policy();
    policy.matchers = vec![
        PolicyMatcher::Client {
            value: "codex".into(),
        },
        PolicyMatcher::HasTools { value: true },
        PolicyMatcher::Streaming { value: true },
    ];

    // All three match.
    let mut ctx = simple_match_ctx("m");
    ctx.client_id = Some("codex");
    ctx.has_tools = true;
    ctx.streaming = true;
    assert!(policy.matches(&ctx));

    // Missing one: has_tools.
    ctx.has_tools = false;
    assert!(!policy.matches(&ctx));

    // Missing one: streaming.
    ctx.has_tools = true;
    ctx.streaming = false;
    assert!(!policy.matches(&ctx));
}

#[test]
fn policy_model_prefix_matcher() {
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::ModelPrefix {
        value: "gpt".into(),
    }];
    assert!(policy.matches(&simple_match_ctx("gpt-4")));
    assert!(policy.matches(&simple_match_ctx("gpt-5.3-sol")));
    assert!(!policy.matches(&simple_match_ctx("claude-3")));
}

#[test]
fn policy_streaming_matcher() {
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::Streaming { value: true }];
    let mut ctx = simple_match_ctx("m");
    ctx.streaming = true;
    assert!(policy.matches(&ctx));
    ctx.streaming = false;
    assert!(!policy.matches(&ctx));
}

#[test]
fn policy_has_vision_matcher() {
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::HasVision { value: true }];
    let mut ctx = simple_match_ctx("m");
    ctx.has_vision = true;
    assert!(policy.matches(&ctx));
    ctx.has_vision = false;
    assert!(!policy.matches(&ctx));
}

#[test]
fn policy_tier_matcher() {
    // Tier matching is now a PolicyRequirement, not a PolicyMatcher.
    // This test verifies that a policy with min/max tier requirements
    // correctly filters candidates via eligibility, not matchers.
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Reasoning),
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Reasoning), &caps, false);
    assert!(check.eligible);
    let check = reqs.check("m", "p", Some(ModelTier::Fast), &caps, false);
    assert!(!check.eligible);
    let check = reqs.check("m", "p", None, &caps, false);
    // A candidate with no tier declared cannot satisfy a tier bound, so it is
    // rejected with RejectionReason::UnknownTier rather than passing by accident.
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::UnknownTier)));
}

#[test]
fn policy_min_complexity_matcher_with_task_profile() {
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::MinComplexity {
        value: Complexity::Standard,
    }];

    let simple = make_profile(Complexity::Simple);
    let standard = make_profile(Complexity::Standard);
    let complex = make_profile(Complexity::Complex);

    let mut ctx = simple_match_ctx("m");
    ctx.task = Some(&simple);
    assert!(!policy.matches(&ctx));

    ctx.task = Some(&standard);
    assert!(policy.matches(&ctx));

    ctx.task = Some(&complex);
    assert!(policy.matches(&ctx));
}

#[test]
fn policy_task_type_matcher() {
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::TaskType {
        value: TaskType::ToolUse,
    }];

    let mut tool_profile = make_profile(Complexity::Standard);
    tool_profile.task_type = TaskType::ToolUse;
    let mut ctx = simple_match_ctx("m");
    ctx.task = Some(&tool_profile);
    assert!(policy.matches(&ctx));

    let mut chat_profile = make_profile(Complexity::Standard);
    chat_profile.task_type = TaskType::Chat;
    ctx.task = Some(&chat_profile);
    assert!(!policy.matches(&ctx));
}

#[test]
fn policy_requires_capability_matcher() {
    // RequiresCapability is now a PolicyRequirement, not a PolicyMatcher.
    // This test verifies that capability filtering works via eligibility check.
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: true, // strict mode rejects unknown
        ..Default::default()
    };
    let caps_vision = ModelCapabilities {
        vision: true,
        ..Default::default()
    };
    let caps_tools = ModelCapabilities {
        tools: true,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps_vision, false);
    assert!(check.eligible);
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps_tools, false);
    assert!(!check.eligible);
}
