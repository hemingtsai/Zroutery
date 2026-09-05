//! Stage 3 routing conformance tests.
//!
//! These tests exercise the full routing policy surface area:
//! - TaskProfile derivation from ChatRequest
//! - Complexity -> suggested_tier mapping
//! - Policy matcher evaluation with multiple conditions
//! - Eligibility (PolicyRequirements) checks
//! - Scoring (PolicyPreference) ranking
//! - Client profile resolution
//! - PolicyConfig serde round-trip

use serde_json::json;

use zroutery_core::config::{ModelCapabilities, ModelTier};
use zroutery_core::ir::{Capability, ChatRequest, ContentBlock, Dialect, MediaSource, ToolDef};
use zroutery_core::policy::{
    self, ClientContext, ClientMatcher, ClientProfile, Complexity, MatchContext, PolicyConfig,
    PolicyFallback, PolicyMatcher, PolicyPreference, PolicyRequirements, RejectionReason,
    RoutingPolicy, ScoringContext, TaskProfile, TaskType, resolve_client, score_candidate,
};

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
        PolicyMatcher::RequiresCapability {
            value: Capability::Tools,
        },
        PolicyMatcher::HasTools { value: true },
    ];

    // All three match.
    let mut ctx = simple_match_ctx("m");
    ctx.client_id = Some("codex");
    ctx.required_capabilities = &[Capability::Tools];
    ctx.has_tools = true;
    assert!(policy.matches(&ctx));

    // Missing one: capability.
    ctx.required_capabilities = &[];
    assert!(!policy.matches(&ctx));

    // Missing one: has_tools.
    ctx.required_capabilities = &[Capability::Tools];
    ctx.has_tools = false;
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
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::Tier {
        value: ModelTier::Reasoning,
    }];
    let mut ctx = simple_match_ctx("m");
    ctx.tier = Some(ModelTier::Reasoning);
    assert!(policy.matches(&ctx));
    ctx.tier = Some(ModelTier::Fast);
    assert!(!policy.matches(&ctx));
    ctx.tier = None;
    assert!(!policy.matches(&ctx));
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
    let mut policy = default_policy();
    policy.matchers = vec![PolicyMatcher::RequiresCapability {
        value: Capability::Vision,
    }];
    let mut ctx = simple_match_ctx("m");
    ctx.required_capabilities = &[Capability::Vision];
    assert!(policy.matches(&ctx));
    ctx.required_capabilities = &[Capability::Tools];
    assert!(!policy.matches(&ctx));
}

// ========================================================================
// 4. Eligibility: tier bounds, forbidden provider, capability, circuit open
// ========================================================================

#[test]
fn eligibility_all_pass() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        min_tier: Some(ModelTier::Fast),
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities {
        tools: true,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
    assert!(check.reasons.is_empty());
}

#[test]
fn eligibility_below_min_tier_rejected() {
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Fast), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::BelowMinTier)));
}

#[test]
fn eligibility_above_max_tier_rejected() {
    let reqs = PolicyRequirements {
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Frontier), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::AboveMaxTier)));
}

#[test]
fn eligibility_at_min_tier_accepted() {
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_at_max_tier_accepted() {
    let reqs = PolicyRequirements {
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Reasoning), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_forbidden_provider_rejected() {
    let reqs = PolicyRequirements {
        forbidden_providers: vec!["bad-provider".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "bad-provider", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::ProviderForbidden)));
}

#[test]
fn eligibility_forbidden_provider_other_accepted() {
    let reqs = PolicyRequirements {
        forbidden_providers: vec!["bad-provider".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "good-provider", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_missing_capability_rejected() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(check.reasons.iter().any(|r| matches!(
        r,
        RejectionReason::MissingCapability(Capability::Vision)
    )));
}

#[test]
fn eligibility_capability_present_accepted() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: true,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);
}

#[test]
fn eligibility_circuit_open_rejected() {
    let reqs = PolicyRequirements::default();
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, true);
    assert!(!check.eligible);
    assert!(check
        .reasons
        .iter()
        .any(|r| matches!(r, RejectionReason::CircuitOpen)));
}

#[test]
fn eligibility_multiple_failures_reported() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        forbidden_providers: vec!["bad".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "bad", Some(ModelTier::Standard), &caps, true);
    assert!(!check.eligible);
    assert_eq!(check.reasons.len(), 3);
}

#[test]
fn eligibility_allowed_providers_restricts() {
    let reqs = PolicyRequirements {
        allowed_providers: vec!["openai".into(), "anthropic".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    assert!(reqs
        .check("m", "openai", Some(ModelTier::Standard), &caps, false)
        .eligible);
    assert!(!reqs
        .check("m", "deepseek", Some(ModelTier::Standard), &caps, false)
        .eligible);
}

#[test]
fn eligibility_forbidden_models_rejected() {
    let reqs = PolicyRequirements {
        forbidden_models: vec!["bad-model".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    assert!(!reqs
        .check("bad-model", "p", Some(ModelTier::Standard), &caps, false)
        .eligible);
    assert!(reqs
        .check("good-model", "p", Some(ModelTier::Standard), &caps, false)
        .eligible);
}

#[test]
fn eligibility_allowed_models_restricts() {
    let reqs = PolicyRequirements {
        allowed_models: vec!["gpt-4".into(), "gpt-5".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    assert!(reqs
        .check("gpt-4", "p", Some(ModelTier::Standard), &caps, false)
        .eligible);
    assert!(!reqs
        .check("claude-3", "p", Some(ModelTier::Standard), &caps, false)
        .eligible);
}

// ========================================================================
// 5. Scoring: prefers healthy, low latency, preferred tier
// ========================================================================

#[test]
fn scoring_perfect_candidate_scores_high() {
    let pref = PolicyPreference::default();
    let ctx = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert!(
        s.total_score >= 0.85,
        "expected >=0.85, got {}",
        s.total_score
    );
    assert_eq!(s.breakdown.health, 1.0);
    assert!((s.breakdown.latency - 1.0).abs() < 0.001);
    assert!((s.breakdown.cost - 1.0).abs() < 0.001);
    assert_eq!(s.breakdown.priority, 1.0);
}

#[test]
fn scoring_prefers_healthy_over_unhealthy() {
    let pref = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let healthy = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let unhealthy = make_scoring_ctx(0.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s_h = score_candidate(&pref, &healthy);
    let s_u = score_candidate(&pref, &unhealthy);
    assert!(
        s_h.total_score > s_u.total_score,
        "healthy {} should beat unhealthy {}",
        s_h.total_score,
        s_u.total_score
    );
}

#[test]
fn scoring_prefers_low_latency() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 1.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let fast = make_scoring_ctx(1.0, 200.0, Some(1.0), 0, Some(ModelTier::Standard));
    let slow = make_scoring_ctx(1.0, 2000.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s_f = score_candidate(&pref, &fast);
    let s_s = score_candidate(&pref, &slow);
    assert!(
        s_f.total_score > s_s.total_score,
        "fast {} should beat slow {}",
        s_f.total_score,
        s_s.total_score
    );
}

#[test]
fn scoring_prefers_cheaper_cost() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let cheap = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let expensive = make_scoring_ctx(1.0, 100.0, Some(50.0), 0, Some(ModelTier::Standard));
    let s_c = score_candidate(&pref, &cheap);
    let s_e = score_candidate(&pref, &expensive);
    assert!(s_c.total_score > s_e.total_score);
}

#[test]
fn scoring_prefers_preferred_tier() {
    let pref = PolicyPreference {
        preferred_tier: Some(ModelTier::Standard),
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 1.0,
        ..Default::default()
    };
    let exact = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let close = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Reasoning));
    let far = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Frontier));
    let s_exact = score_candidate(&pref, &exact);
    let s_close = score_candidate(&pref, &close);
    let s_far = score_candidate(&pref, &far);
    assert!(s_exact.total_score > s_close.total_score);
    assert!(s_close.total_score > s_far.total_score);
}

#[test]
fn scoring_prefers_lower_priority_number() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 1.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let high_prio = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let low_prio = make_scoring_ctx(1.0, 100.0, Some(1.0), 50, Some(ModelTier::Standard));
    let s_h = score_candidate(&pref, &high_prio);
    let s_l = score_candidate(&pref, &low_prio);
    assert!(s_h.total_score > s_l.total_score);
}

#[test]
fn scoring_unmeasured_latency_gets_benefit_of_doubt() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 1.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let unmeasured = make_scoring_ctx(1.0, 0.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &unmeasured);
    assert!(
        (s.breakdown.latency - 1.0).abs() < 0.001,
        "unmeasured should get full latency score, got {}",
        s.breakdown.latency
    );
}

#[test]
fn scoring_unknown_cost_is_neutral() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let unknown = make_scoring_ctx(1.0, 100.0, None, 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &unknown);
    assert!((s.breakdown.cost - 0.5).abs() < 0.001);
}

#[test]
fn scoring_weights_are_normalized() {
    let pref = PolicyPreference {
        preferred_tier: None,
        health_weight: 5.0,
        latency_weight: 5.0,
        cost_weight: 5.0,
        priority_weight: 5.0,
        tier_weight: 5.0,
    };
    let ctx = make_scoring_ctx(0.5, 500.0, Some(5.0), 25, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert!(
        s.total_score >= 0.0 && s.total_score <= 1.0,
        "score out of range: {}",
        s.total_score
    );
}

#[test]
fn scoring_zero_weights_produce_zero() {
    let pref = PolicyPreference {
        preferred_tier: None,
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
    };
    let ctx = make_scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert_eq!(s.total_score, 0.0);
}

// ========================================================================
// 6. Client profiles: user agent, model prefix, resolve_client
// ========================================================================

#[test]
fn client_profile_user_agent_case_insensitive() {
    let profile = ClientProfile {
        id: "curl".into(),
        name: "curl client".into(),
        matchers: vec![ClientMatcher::UserAgent {
            value: "curl".into(),
        }],
        policy_id: "default".into(),
    };
    let ctx_lower = ClientContext {
        client_id: None,
        user_agent: Some("curl/7.68.0"),
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_upper = ClientContext {
        client_id: None,
        user_agent: Some("CURL/7.68.0"),
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: Some("python-requests/2.28"),
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    assert!(profile.matches(&ctx_lower));
    assert!(profile.matches(&ctx_upper));
    assert!(!profile.matches(&ctx_miss));
}

#[test]
fn client_profile_model_prefix() {
    let profile = ClientProfile {
        id: "gpt-user".into(),
        name: "GPT model user".into(),
        matchers: vec![ClientMatcher::ModelPrefix {
            value: "gpt".into(),
        }],
        policy_id: "gpt-policy".into(),
    };
    let ctx_match = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4o",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "claude-3",
    };
    assert!(profile.matches(&ctx_match));
    assert!(!profile.matches(&ctx_miss));
}

#[test]
fn client_profile_api_key_prefix() {
    let profile = ClientProfile {
        id: "bot".into(),
        name: "Bot".into(),
        matchers: vec![ClientMatcher::ApiKeyPrefix {
            value: "sk-bot-".into(),
        }],
        policy_id: "bot-policy".into(),
    };
    let ctx_match = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: Some("sk-bot-abc123"),
        headers: &[],
        model: "gpt-4",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: Some("sk-proj-abc"),
        headers: &[],
        model: "gpt-4",
    };
    assert!(profile.matches(&ctx_match));
    assert!(!profile.matches(&ctx_miss));
}

#[test]
fn client_profile_header_case_insensitive_name() {
    let profile = ClientProfile {
        id: "custom".into(),
        name: "Custom".into(),
        matchers: vec![ClientMatcher::Header {
            name: "X-App".into(),
            value: "myapp".into(),
        }],
        policy_id: "custom-policy".into(),
    };
    let headers_match = [("X-App", "myapp")];
    let headers_case = [("x-app", "myapp")];
    let headers_miss = [("X-App", "other")];
    let ctx_match = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        headers: &headers_match,
        model: "gpt-4",
    };
    let ctx_case = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        headers: &headers_case,
        model: "gpt-4",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        headers: &headers_miss,
        model: "gpt-4",
    };
    assert!(profile.matches(&ctx_match));
    assert!(profile.matches(&ctx_case));
    assert!(!profile.matches(&ctx_miss));
}

#[test]
fn client_profile_empty_matchers_never_match() {
    let profile = ClientProfile {
        id: "empty".into(),
        name: "Empty".into(),
        matchers: vec![],
        policy_id: "default".into(),
    };
    let ctx = ClientContext {
        client_id: Some("anything"),
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    assert!(!profile.matches(&ctx));
}

#[test]
fn client_profile_multiple_matchers_all_must_pass() {
    let profile = ClientProfile {
        id: "multi".into(),
        name: "Multi".into(),
        matchers: vec![
            ClientMatcher::ClientId {
                value: "codex".into(),
            },
            ClientMatcher::ModelPrefix {
                value: "gpt".into(),
            },
        ],
        policy_id: "multi-policy".into(),
    };
    let ctx_both = ClientContext {
        client_id: Some("codex"),
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_partial = ClientContext {
        client_id: Some("codex"),
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "claude-3",
    };
    assert!(profile.matches(&ctx_both));
    assert!(!profile.matches(&ctx_partial));
}

#[test]
fn resolve_client_returns_first_match() {
    let profiles = vec![
        bot_profile(),
        codex_profile(),
        ua_profile(),
    ];
    let ctx = ClientContext {
        client_id: Some("codex"),
        user_agent: Some("curl/7.0"),
        api_key_prefix: Some("sk-bot-x"),
        headers: &[],
        model: "gpt-4",
    };
    let resolved = resolve_client(&profiles, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().id, "bot");
}

#[test]
fn resolve_client_returns_none_when_no_match() {
    let profiles = vec![codex_profile(), bot_profile()];
    let ctx = ClientContext {
        client_id: Some("unknown"),
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    assert!(resolve_client(&profiles, &ctx).is_none());
}

#[test]
fn resolve_client_empty_profiles() {
    let profiles: Vec<ClientProfile> = vec![];
    let ctx = ClientContext {
        client_id: Some("codex"),
        user_agent: None,
        api_key_prefix: None,
        headers: &[],
        model: "gpt-4",
    };
    assert!(resolve_client(&profiles, &ctx).is_none());
}

#[test]
fn resolve_client_second_profile_matches() {
    let profiles = vec![codex_profile(), bot_profile()];
    let ctx = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: Some("sk-bot-key123"),
        headers: &[],
        model: "gpt-4",
    };
    let resolved = resolve_client(&profiles, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().id, "bot");
}

// ========================================================================
// 7. PolicyConfig serde round-trip
// ========================================================================

#[test]
fn policy_config_serde_round_trip_empty() {
    let config = PolicyConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    let back: PolicyConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config, back);
}

#[test]
fn policy_config_serde_round_trip_with_policies() {
    let config = PolicyConfig {
        policies: vec![RoutingPolicy {
            id: "test-policy".into(),
            name: "Test Policy".into(),
            enabled: true,
            matchers: vec![
                PolicyMatcher::Client {
                    value: "codex".into(),
                },
                PolicyMatcher::HasTools { value: true },
            ],
            requirements: PolicyRequirements {
                required_capabilities: vec![Capability::Tools],
                min_tier: Some(ModelTier::Standard),
                max_tier: Some(ModelTier::Frontier),
                forbidden_providers: vec!["bad".into()],
                ..Default::default()
            },
            preference: PolicyPreference {
                preferred_tier: Some(ModelTier::Standard),
                health_weight: 0.3,
                latency_weight: 0.4,
                cost_weight: 0.1,
                priority_weight: 0.1,
                tier_weight: 0.1,
            },
            fallback: PolicyFallback::Escalate {
                enabled: true,
                max_steps: 3,
            },
        }],
        default_policy: Some("test-policy".into()),
        clients: vec![codex_profile()],
    };
    let json = serde_json::to_string(&config).unwrap();
    let back: PolicyConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config, back);
}

#[test]
fn policy_config_serde_round_trip_all_matcher_types() {
    let config = PolicyConfig {
        policies: vec![RoutingPolicy {
            id: "all-matchers".into(),
            name: "All Matchers".into(),
            enabled: true,
            matchers: vec![
                PolicyMatcher::Client {
                    value: "c".into(),
                },
                PolicyMatcher::Application {
                    value: "app".into(),
                },
                PolicyMatcher::Model {
                    value: "m".into(),
                },
                PolicyMatcher::ModelPrefix {
                    value: "mp".into(),
                },
                PolicyMatcher::Provider {
                    value: "p".into(),
                },
                PolicyMatcher::Tier {
                    value: ModelTier::Standard,
                },
                PolicyMatcher::RequiresCapability {
                    value: Capability::Vision,
                },
                PolicyMatcher::Streaming { value: true },
                PolicyMatcher::HasTools { value: true },
                PolicyMatcher::HasVision { value: false },
                PolicyMatcher::MinComplexity {
                    value: Complexity::Complex,
                },
                PolicyMatcher::TaskType {
                    value: TaskType::ToolUse,
                },
            ],
            requirements: PolicyRequirements::default(),
            preference: PolicyPreference::default(),
            fallback: PolicyFallback::Reject,
        }],
        default_policy: None,
        clients: vec![],
    };
    let json = serde_json::to_string(&config).unwrap();
    let back: PolicyConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config, back);
    assert_eq!(back.policies[0].matchers.len(), 12);
}

#[test]
fn policy_config_serde_round_trip_all_fallback_variants() {
    let variants = vec![
        PolicyFallback::Reject,
        PolicyFallback::Escalate {
            enabled: true,
            max_steps: 2,
        },
        PolicyFallback::Degrade {
            enabled: false,
            max_steps: 5,
        },
        PolicyFallback::IgnoreRequirements,
    ];
    for fallback in variants {
        let config = PolicyConfig {
            policies: vec![RoutingPolicy {
                id: "fb".into(),
                name: "FB".into(),
                enabled: true,
                matchers: vec![],
                requirements: PolicyRequirements::default(),
                preference: PolicyPreference::default(),
                fallback: fallback.clone(),
            }],
            default_policy: None,
            clients: vec![],
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: PolicyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.policies[0].fallback, fallback);
    }
}

#[test]
fn policy_config_serde_round_trip_clients() {
    let config = PolicyConfig {
        policies: vec![],
        default_policy: None,
        clients: vec![codex_profile(), bot_profile()],
    };
    let json = serde_json::to_string(&config).unwrap();
    let back: PolicyConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config, back);
    assert_eq!(back.clients.len(), 2);
    assert_eq!(back.clients[0].id, "codex");
    assert_eq!(back.clients[1].id, "bot");
}

#[test]
fn policy_config_clients_default_empty() {
    let json = r#"{"policies": []}"#;
    let config: PolicyConfig = serde_json::from_str(json).unwrap();
    assert!(config.clients.is_empty());
}

#[test]
fn policy_config_default_policy_is_none_by_default() {
    let config = PolicyConfig::default();
    assert!(config.default_policy.is_none());
}

// ========================================================================
// Helpers
// ========================================================================

fn make_profile(complexity: Complexity) -> TaskProfile {
    TaskProfile {
        tier: None,
        required_capabilities: vec![],
        context_tokens: 0,
        estimated_output_tokens: 0,
        streaming: false,
        has_tools: false,
        has_vision: false,
        complexity,
        task_type: TaskType::Chat,
    }
}

fn default_policy() -> RoutingPolicy {
    policy::default_policy()
}

fn simple_match_ctx<'a>(model: &'a str) -> MatchContext<'a> {
    MatchContext {
        client_id: None,
        application: None,
        model,
        provider: "p",
        tier: Some(ModelTier::Standard),
        required_capabilities: &[],
        streaming: false,
        has_tools: false,
        has_vision: false,
        task: None,
    }
}

fn make_scoring_ctx(
    health: f64,
    latency: f64,
    cost: Option<f64>,
    priority: i32,
    tier: Option<ModelTier>,
) -> ScoringContext {
    ScoringContext {
        health,
        avg_latency_ms: latency,
        cost_per_mtok: cost,
        priority,
        tier,
    }
}

fn codex_profile() -> ClientProfile {
    ClientProfile {
        id: "codex".into(),
        name: "Codex CLI".into(),
        matchers: vec![ClientMatcher::ClientId {
            value: "codex".into(),
        }],
        policy_id: "code-policy".into(),
    }
}

fn bot_profile() -> ClientProfile {
    ClientProfile {
        id: "bot".into(),
        name: "Bot via API key".into(),
        matchers: vec![ClientMatcher::ApiKeyPrefix {
            value: "sk-bot-".into(),
        }],
        policy_id: "bot-policy".into(),
    }
}

fn ua_profile() -> ClientProfile {
    ClientProfile {
        id: "curl".into(),
        name: "curl client".into(),
        matchers: vec![ClientMatcher::UserAgent {
            value: "curl".into(),
        }],
        policy_id: "default".into(),
    }
}
