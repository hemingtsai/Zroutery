//! The policy fixtures: matching, eligibility, scoring, client overrides and the decision trace.

use super::scoring::tier_distance;
use super::*;
use crate::config::{ModelCapabilities, ModelTier};
use crate::ir::{Capability, ChatRequest};

fn ctx(model: &str) -> MatchContext<'_> {
    MatchContext {
        client_id: None,
        application: None,
        model,
        streaming: false,
        has_tools: false,
        has_vision: false,
        task: None,
    }
}

#[test]
fn empty_matchers_always_match() {
    let p = default_policy();
    assert!(p.matches(&ctx("any")));
}

#[test]
fn disabled_policy_never_matches() {
    let mut p = default_policy();
    p.enabled = false;
    assert!(!p.matches(&ctx("any")));
}

#[test]
fn client_matcher() {
    let mut p = default_policy();
    p.matchers = vec![PolicyMatcher::Client {
        value: "codex".into(),
    }];
    let mut c = ctx("m");
    c.client_id = Some("codex");
    assert!(p.matches(&c));
    c.client_id = Some("other");
    assert!(!p.matches(&c));
}

#[test]
fn capability_requirement_filters() {
    // With strict_capabilities = true, unknown capabilities are rejected.
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: true,
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(matches!(
        check.reasons[0],
        RejectionReason::MissingCapability(Capability::Vision)
    ));
}

#[test]
fn capability_unknown_is_not_a_policy_pass() {
    // An undeclared provider capability is not support. Explicit
    // remediation is represented by the router and marks a candidate
    // degraded; the policy check itself remains fail-closed.
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Vision],
        strict_capabilities: false,
        ..Default::default()
    };
    let caps = ModelCapabilities {
        vision: false,
        ..Default::default()
    };
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    assert!(check.has_unknown_capability());
}

#[test]
fn tier_bounds_filter() {
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        max_tier: Some(ModelTier::Reasoning),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();

    // Below min
    let check = reqs.check("m", "p", Some(ModelTier::Fast), &caps, false);
    assert!(!check.eligible);

    // At min
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
    assert!(check.eligible);

    // At max
    let check = reqs.check("m", "p", Some(ModelTier::Reasoning), &caps, false);
    assert!(check.eligible);

    // Above max
    let check = reqs.check("m", "p", Some(ModelTier::Frontier), &caps, false);
    assert!(!check.eligible);
}

#[test]
fn unknown_tier_rejected_when_bounds_set() {
    let reqs = PolicyRequirements {
        min_tier: Some(ModelTier::Standard),
        ..Default::default()
    };
    let caps = ModelCapabilities::default();

    // Candidate with no tier should be rejected when min_tier is set.
    let check = reqs.check("m", "p", None, &caps, false);
    assert!(!check.eligible);
    assert!(matches!(check.reasons[0], RejectionReason::UnknownTier));
}

#[test]
fn unknown_tier_accepted_when_no_bounds() {
    let reqs = PolicyRequirements::default();
    let caps = ModelCapabilities::default();

    // Candidate with no tier passes when no tier bounds are configured.
    let check = reqs.check("m", "p", None, &caps, false);
    assert!(check.eligible);
}

#[test]
fn forbidden_provider_filter() {
    let reqs = PolicyRequirements {
        forbidden_providers: vec!["bad-provider".into()],
        ..Default::default()
    };
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "bad-provider", Some(ModelTier::Standard), &caps, false);
    assert!(!check.eligible);
    let check = reqs.check(
        "m",
        "good-provider",
        Some(ModelTier::Standard),
        &caps,
        false,
    );
    assert!(check.eligible);
}

#[test]
fn circuit_open_rejects() {
    let reqs = PolicyRequirements::default();
    let caps = ModelCapabilities::default();
    let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, true);
    assert!(!check.eligible);
    assert!(matches!(check.reasons[0], RejectionReason::CircuitOpen));
}

#[test]
fn all_constraints_pass() {
    let reqs = PolicyRequirements {
        required_capabilities: vec![Capability::Tools],
        min_tier: Some(ModelTier::Fast),
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
fn policy_matching_with_multiple_matchers() {
    let mut p = default_policy();
    p.matchers = vec![
        PolicyMatcher::Client {
            value: "codex".into(),
        },
        PolicyMatcher::HasTools { value: true },
    ];
    let mut c = ctx("m");
    c.client_id = Some("codex");
    c.has_tools = true;
    assert!(p.matches(&c));

    // Missing one matcher
    c.has_tools = false;
    assert!(!p.matches(&c));
}

#[test]
fn model_prefix_matcher() {
    let mut p = default_policy();
    p.matchers = vec![PolicyMatcher::ModelPrefix {
        value: "gpt".into(),
    }];
    assert!(p.matches(&ctx("gpt-4")));
    assert!(p.matches(&ctx("gpt-5.3-sol")));
    assert!(!p.matches(&ctx("claude-3")));
}

// ------------------------------------------------ TaskProfile

#[test]
fn task_profile_simple_request() {
    let req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.complexity, Complexity::Simple);
    assert_eq!(profile.task_type, TaskType::Chat);
    assert!(!profile.has_tools);
    assert!(!profile.has_vision);
    assert!(!profile.streaming);
    assert_eq!(profile.suggested_tier(), ModelTier::Fast);
}

#[test]
fn task_profile_with_tools() {
    let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    req.tools = vec![crate::ir::ToolDef {
        name: "search".into(),
        description: Some("Search the web".into()),
        input_schema: serde_json::json!({}),
        cache_control: None,
    }];
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.complexity, Complexity::Standard);
    assert_eq!(profile.task_type, TaskType::ToolUse);
    assert!(profile.has_tools);
    assert_eq!(profile.suggested_tier(), ModelTier::Standard);
}

#[test]
fn task_profile_with_vision() {
    let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    req.messages = vec![crate::ir::Message {
        role: crate::ir::Role::User,
        content: vec![crate::ir::ContentBlock::Image {
            source: crate::ir::MediaSource::Base64 {
                media_type: "image/png".into(),
                data: "abc".into(),
            },
        }],
    }];
    let profile = TaskProfile::from_request(&req);

    assert!(profile.has_vision);
    assert_eq!(profile.task_type, TaskType::Vision);
    assert!(profile.required_capabilities.contains(&Capability::Vision));
}

#[test]
fn task_profile_with_thinking_is_complex() {
    let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    req.thinking = Some(crate::ir::ThinkingConfig {
        enabled: true,
        budget_tokens: Some(10000),
    });
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.complexity, Complexity::Complex);
    assert_eq!(profile.suggested_tier(), ModelTier::Reasoning);
}

#[test]
fn task_profile_streaming() {
    let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    req.stream = true;
    let profile = TaskProfile::from_request(&req);

    assert!(profile.streaming);
}

#[test]
fn task_profile_max_tokens_estimate() {
    let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    req.max_tokens = Some(8192);
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.estimated_output_tokens, 8192);
}

#[test]
fn task_profile_default_max_tokens() {
    let req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
    let profile = TaskProfile::from_request(&req);

    assert_eq!(profile.estimated_output_tokens, 4096);
}

#[test]
fn suggested_tier_all_complexities() {
    let mut profile = TaskProfile {
        tier: None,
        required_capabilities: vec![],
        context_tokens: 0,
        estimated_output_tokens: 0,
        streaming: false,
        has_tools: false,
        has_vision: false,
        complexity: Complexity::Simple,
        task_type: TaskType::Chat,
    };

    assert_eq!(profile.suggested_tier(), ModelTier::Fast);
    profile.complexity = Complexity::Standard;
    assert_eq!(profile.suggested_tier(), ModelTier::Standard);
    profile.complexity = Complexity::Complex;
    assert_eq!(profile.suggested_tier(), ModelTier::Reasoning);
    profile.complexity = Complexity::Frontier;
    assert_eq!(profile.suggested_tier(), ModelTier::Frontier);
}

// ------------------------------------------------ MinComplexity / TaskType matchers

fn profile_with_complexity(c: Complexity) -> TaskProfile {
    TaskProfile {
        tier: None,
        required_capabilities: vec![],
        context_tokens: 0,
        estimated_output_tokens: 0,
        streaming: false,
        has_tools: false,
        has_vision: false,
        complexity: c,
        task_type: TaskType::Chat,
    }
}

#[test]
fn min_complexity_matcher() {
    let mut p = default_policy();
    p.matchers = vec![PolicyMatcher::MinComplexity {
        value: Complexity::Standard,
    }];

    let profile_simple = profile_with_complexity(Complexity::Simple);
    let profile_standard = profile_with_complexity(Complexity::Standard);
    let profile_complex = profile_with_complexity(Complexity::Complex);

    let mut c = ctx("m");
    c.task = Some(&profile_simple);
    assert!(
        !p.matches(&c),
        "Simple should not match MinComplexity::Standard"
    );

    c.task = Some(&profile_standard);
    assert!(
        p.matches(&c),
        "Standard should match MinComplexity::Standard"
    );

    c.task = Some(&profile_complex);
    assert!(
        p.matches(&c),
        "Complex should match MinComplexity::Standard"
    );
}

#[test]
fn min_complexity_matcher_no_task() {
    let mut p = default_policy();
    p.matchers = vec![PolicyMatcher::MinComplexity {
        value: Complexity::Simple,
    }];
    let c = ctx("m");
    assert!(!p.matches(&c), "No task profile should not match");
}

#[test]
fn task_type_matcher() {
    let mut p = default_policy();
    p.matchers = vec![PolicyMatcher::TaskType {
        value: TaskType::ToolUse,
    }];

    let mut profile_tool = profile_with_complexity(Complexity::Standard);
    profile_tool.task_type = TaskType::ToolUse;
    let mut c = ctx("m");
    c.task = Some(&profile_tool);
    assert!(p.matches(&c));

    let mut profile_chat = profile_with_complexity(Complexity::Standard);
    profile_chat.task_type = TaskType::Chat;
    c.task = Some(&profile_chat);
    assert!(!p.matches(&c));
}

#[test]
fn task_type_matcher_no_task() {
    let mut p = default_policy();
    p.matchers = vec![PolicyMatcher::TaskType {
        value: TaskType::Chat,
    }];
    let c = ctx("m");
    assert!(!p.matches(&c), "No task profile should not match");
}

#[test]
fn complexity_ordering() {
    assert!(Complexity::Simple < Complexity::Standard);
    assert!(Complexity::Standard < Complexity::Complex);
    assert!(Complexity::Complex < Complexity::Frontier);
}

#[test]
fn task_type_default_is_chat() {
    assert_eq!(TaskType::default(), TaskType::Chat);
}

#[test]
fn complexity_default_is_standard() {
    assert_eq!(Complexity::default(), Complexity::Standard);
}

// ------------------------------------------------ Scoring

fn scoring_ctx<'a>(
    health: f64,
    latency: f64,
    cost: Option<f64>,
    priority: i32,
    tier: Option<ModelTier>,
) -> ScoringContext<'a> {
    ScoringContext {
        health,
        avg_latency_ms: latency,
        input_per_mtok: cost,
        output_per_mtok: cost,
        priority,
        tier,
        task: None,
    }
}

#[test]
fn perfect_candidate_scores_high() {
    let pref = PolicyPreference::default();
    let ctx = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
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
fn unhealthy_candidate_penalized() {
    let pref = PolicyPreference {
        health_weight: 1.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let healthy = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let unhealthy = scoring_ctx(0.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s_h = score_candidate(&pref, &healthy);
    let s_u = score_candidate(&pref, &unhealthy);
    assert!(s_h.total_score > s_u.total_score);
}

#[test]
fn lower_latency_scores_higher() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 1.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let fast = scoring_ctx(1.0, 200.0, Some(1.0), 0, Some(ModelTier::Standard));
    let slow = scoring_ctx(1.0, 2000.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s_f = score_candidate(&pref, &fast);
    let s_s = score_candidate(&pref, &slow);
    assert!(
        s_f.total_score > s_s.total_score,
        "fast {} > slow {}",
        s_f.total_score,
        s_s.total_score
    );
    // 200ms → 1000/200 = 1.0 (clamped), 2000ms → 1000/2000 = 0.5
    assert!((s_f.breakdown.latency - 1.0).abs() < 0.001);
    assert!((s_s.breakdown.latency - 0.5).abs() < 0.001);
}

#[test]
fn cheaper_cost_scores_higher() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let cheap = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let expensive = scoring_ctx(1.0, 100.0, Some(50.0), 0, Some(ModelTier::Standard));
    let s_c = score_candidate(&pref, &cheap);
    let s_e = score_candidate(&pref, &expensive);
    assert!(s_c.total_score > s_e.total_score);
}

#[test]
fn lower_priority_number_scores_higher() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 1.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let high_prio = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let low_prio = scoring_ctx(1.0, 100.0, Some(1.0), 50, Some(ModelTier::Standard));
    let s_h = score_candidate(&pref, &high_prio);
    let s_l = score_candidate(&pref, &low_prio);
    assert!(s_h.total_score > s_l.total_score);
}

#[test]
fn closer_tier_scores_higher() {
    let pref = PolicyPreference {
        preferred_tier: Some(ModelTier::Standard),
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 1.0,
    };
    let exact = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let close = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Reasoning));
    let far = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Frontier));
    let s_exact = score_candidate(&pref, &exact);
    let s_close = score_candidate(&pref, &close);
    let s_far = score_candidate(&pref, &far);
    assert!(s_exact.total_score > s_close.total_score);
    assert!(s_close.total_score > s_far.total_score);
}

#[test]
fn unmeasured_latency_benefit_of_doubt() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 1.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let unmeasured = scoring_ctx(1.0, 0.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &unmeasured);
    assert!(
        (s.breakdown.latency - 1.0).abs() < 0.001,
        "unmeasured should get full latency score, got {}",
        s.breakdown.latency
    );
}

#[test]
fn unknown_cost_is_neutral() {
    let pref = PolicyPreference {
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
        ..Default::default()
    };
    let unknown = scoring_ctx(1.0, 100.0, None, 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &unknown);
    assert!((s.breakdown.cost - 0.5).abs() < 0.001);
}

#[test]
fn weights_are_normalized() {
    // Weights that don't sum to 1 should still produce a [0,1] score.
    let pref = PolicyPreference {
        preferred_tier: None,
        health_weight: 5.0,
        latency_weight: 5.0,
        cost_weight: 5.0,
        priority_weight: 5.0,
        tier_weight: 5.0,
    };
    let ctx = scoring_ctx(0.5, 500.0, Some(5.0), 25, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert!(
        s.total_score >= 0.0 && s.total_score <= 1.0,
        "score out of range: {}",
        s.total_score
    );
}

#[test]
fn zero_weights_produce_zero_score() {
    let pref = PolicyPreference {
        preferred_tier: None,
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 0.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
    };
    let ctx = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
    let s = score_candidate(&pref, &ctx);
    assert_eq!(s.total_score, 0.0);
}

#[test]
fn tier_distance_symmetry() {
    assert_eq!(tier_distance(ModelTier::Fast, ModelTier::Frontier), 3.0);
    assert_eq!(tier_distance(ModelTier::Frontier, ModelTier::Fast), 3.0);
    assert_eq!(tier_distance(ModelTier::Standard, ModelTier::Standard), 0.0);
    assert_eq!(tier_distance(ModelTier::Fast, ModelTier::Reasoning), 2.0);
}

// ------------------------------------------------ Client Profiles

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

fn header_profile() -> ClientProfile {
    ClientProfile {
        id: "custom".into(),
        name: "Custom header client".into(),
        matchers: vec![ClientMatcher::Header {
            name: "X-App".into(),
            value: "myapp".into(),
        }],
        policy_id: "custom-policy".into(),
    }
}

fn model_prefix_profile() -> ClientProfile {
    ClientProfile {
        id: "gpt-user".into(),
        name: "GPT model user".into(),
        matchers: vec![ClientMatcher::ModelPrefix {
            value: "gpt".into(),
        }],
        policy_id: "gpt-policy".into(),
    }
}

fn client_ctx<'a>(
    client_id: Option<&'a str>,
    user_agent: Option<&'a str>,
    api_key_prefix: Option<&'a str>,
    headers: &'a [(&'a str, &'a str)],
    model: &'a str,
) -> ClientContext<'a> {
    ClientContext {
        client_id,
        user_agent,
        api_key_prefix,
        application: None,
        headers,
        model,
    }
}

#[test]
fn client_matcher_client_id() {
    let profile = codex_profile();
    let ctx_match = client_ctx(Some("codex"), None, None, &[], "gpt-4");
    let ctx_miss = client_ctx(Some("other"), None, None, &[], "gpt-4");
    let ctx_none = client_ctx(None, None, None, &[], "gpt-4");
    assert!(profile.matches(&ctx_match));
    assert!(!profile.matches(&ctx_miss));
    assert!(!profile.matches(&ctx_none));
}

#[test]
fn client_matcher_api_key_prefix() {
    let profile = bot_profile();
    let ctx_match = client_ctx(None, None, Some("sk-bot-abc123"), &[], "gpt-4");
    let ctx_miss = client_ctx(None, None, Some("sk-proj-abc"), &[], "gpt-4");
    let ctx_none = client_ctx(None, None, None, &[], "gpt-4");
    assert!(profile.matches(&ctx_match));
    assert!(!profile.matches(&ctx_miss));
    assert!(!profile.matches(&ctx_none));
}

#[test]
fn client_matcher_user_agent_case_insensitive() {
    let profile = ua_profile();
    let ctx_lower = client_ctx(None, Some("curl/7.68.0"), None, &[], "gpt-4");
    let ctx_upper = client_ctx(None, Some("CURL/7.68.0"), None, &[], "gpt-4");
    let ctx_miss = client_ctx(None, Some("python-requests/2.28"), None, &[], "gpt-4");
    assert!(profile.matches(&ctx_lower));
    assert!(profile.matches(&ctx_upper));
    assert!(!profile.matches(&ctx_miss));
}

#[test]
fn client_matcher_header() {
    let profile = header_profile();
    let headers_match = [("X-App", "myapp")];
    let headers_miss = [("X-App", "other")];
    let headers_case = [("x-app", "myapp")];
    let ctx_match = client_ctx(None, None, None, &headers_match, "gpt-4");
    let ctx_miss = client_ctx(None, None, None, &headers_miss, "gpt-4");
    let ctx_case = client_ctx(None, None, None, &headers_case, "gpt-4");
    assert!(profile.matches(&ctx_match));
    assert!(!profile.matches(&ctx_miss));
    // Header name is case-insensitive
    assert!(profile.matches(&ctx_case));
}

#[test]
fn client_matcher_model_prefix() {
    let profile = model_prefix_profile();
    let ctx_match = client_ctx(None, None, None, &[], "gpt-4o");
    let ctx_miss = client_ctx(None, None, None, &[], "claude-3");
    assert!(profile.matches(&ctx_match));
    assert!(!profile.matches(&ctx_miss));
}

#[test]
fn empty_matchers_never_match() {
    let profile = ClientProfile {
        id: "empty".into(),
        name: "Empty".into(),
        matchers: vec![],
        policy_id: "default".into(),
    };
    let ctx = client_ctx(Some("anything"), None, None, &[], "gpt-4");
    assert!(!profile.matches(&ctx));
}

#[test]
fn multiple_matchers_all_must_pass() {
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
    let ctx_both = client_ctx(Some("codex"), None, None, &[], "gpt-4");
    let ctx_partial = client_ctx(Some("codex"), None, None, &[], "claude-3");
    assert!(profile.matches(&ctx_both));
    assert!(!profile.matches(&ctx_partial));
}

#[test]
fn resolve_client_returns_first_match() {
    let profiles = vec![bot_profile(), codex_profile(), ua_profile()];
    let ctx = client_ctx(
        Some("codex"),
        Some("curl/7.0"),
        Some("sk-bot-x"),
        &[],
        "gpt-4",
    );
    // bot_profile matches on ApiKeyPrefix, which comes first
    let resolved = resolve_client(&profiles, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().id, "bot");
}

#[test]
fn resolve_client_returns_none_when_no_match() {
    let profiles = vec![codex_profile(), bot_profile()];
    let ctx = client_ctx(Some("unknown"), None, None, &[], "gpt-4");
    assert!(resolve_client(&profiles, &ctx).is_none());
}

#[test]
fn resolve_client_empty_profiles() {
    let profiles: Vec<ClientProfile> = vec![];
    let ctx = client_ctx(Some("codex"), None, None, &[], "gpt-4");
    assert!(resolve_client(&profiles, &ctx).is_none());
}

#[test]
fn resolve_client_second_profile_matches() {
    let profiles = vec![codex_profile(), bot_profile()];
    let ctx = client_ctx(None, None, Some("sk-bot-key123"), &[], "gpt-4");
    let resolved = resolve_client(&profiles, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().id, "bot");
}

#[test]
fn policy_config_serializes_clients() {
    let config = PolicyConfig {
        policies: vec![],
        default_policy: None,
        clients: vec![codex_profile()],
    };
    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("\"clients\""));
    assert!(json.contains("\"codex\""));
    let deserialized: PolicyConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.clients.len(), 1);
    assert_eq!(deserialized.clients[0].id, "codex");
}

#[test]
fn policy_config_clients_default_empty() {
    let json = r#"{"policies": []}"#;
    let config: PolicyConfig = serde_json::from_str(json).unwrap();
    assert!(config.clients.is_empty());
}

// ------------------------------------------------ Decision Trace

#[test]
fn task_profile_summary_from_profile() {
    let profile = TaskProfile {
        tier: Some(ModelTier::Standard),
        required_capabilities: vec![Capability::Vision, Capability::Tools],
        context_tokens: 5000,
        estimated_output_tokens: 2048,
        streaming: true,
        has_tools: true,
        has_vision: true,
        complexity: Complexity::Complex,
        task_type: TaskType::ToolUse,
    };

    let summary = TaskProfileSummary::from(&profile);
    assert_eq!(summary.complexity, "complex");
    assert_eq!(summary.task_type, "tooluse");
    assert_eq!(summary.context_tokens, 5000);
    assert_eq!(summary.estimated_output_tokens, 2048);
    assert!(summary.streaming);
    assert!(summary.has_tools);
    assert!(summary.has_vision);
    assert!(summary
        .required_capabilities
        .contains(&"vision".to_string()));
    assert!(summary.required_capabilities.contains(&"tools".to_string()));
}

#[test]
fn task_profile_summary_default() {
    let summary = TaskProfileSummary::default();
    assert!(summary.complexity.is_empty());
    assert!(summary.task_type.is_empty());
    assert_eq!(summary.context_tokens, 0);
    assert!(!summary.streaming);
    assert!(summary.required_capabilities.is_empty());
}

#[test]
fn task_profile_summary_serde_roundtrip() {
    let summary = TaskProfileSummary {
        complexity: "standard".into(),
        task_type: "chat".into(),
        context_tokens: 1000,
        estimated_output_tokens: 4096,
        streaming: false,
        has_tools: false,
        has_vision: false,
        required_capabilities: vec!["tools".into(), "vision".into()],
    };
    let json = serde_json::to_string(&summary).unwrap();
    let back: TaskProfileSummary = serde_json::from_str(&json).unwrap();
    assert_eq!(back.complexity, "standard");
    assert_eq!(back.context_tokens, 1000);
    assert_eq!(back.required_capabilities.len(), 2);
}

#[test]
fn route_decision_serde_roundtrip() {
    let decision = RouteDecision {
        decision_id: "dec-001".into(),
        timestamp: 1700000000,
        task: TaskProfileSummary {
            complexity: "complex".into(),
            task_type: "tooluse".into(),
            context_tokens: 50000,
            estimated_output_tokens: 8192,
            streaming: true,
            has_tools: true,
            has_vision: false,
            required_capabilities: vec!["tools".into()],
        },
        policy_id: "code-policy".into(),
        client_id: Some("codex".into()),
        candidates: vec![
            CandidateDecision {
                model_id: "gpt-4".into(),
                provider_id: "openai".into(),
                tier: Some("standard".into()),
                eligible: true,
                rejection: None,
                score: Some(ScoreBreakdown {
                    health: 0.95,
                    latency: 0.8,
                    cost: 0.6,
                    priority: 1.0,
                    tier: 0.9,
                }),
                final_score: Some(0.85),
            },
            CandidateDecision {
                model_id: "llama-3".into(),
                provider_id: "ollama".into(),
                tier: Some("fast".into()),
                eligible: false,
                rejection: Some("below_min_tier".into()),
                score: None,
                final_score: None,
            },
        ],
        selected: Some("gpt-4".into()),
        fallback_chain: vec!["claude-3".into()],
        reason: DecisionReason::PolicySelected,
        policy_revision: PolicyRevision {
            policy_id: "code-policy".into(),
            policy_enabled: true,
            requirements_hash: 12345,
            preference_hash: 67890,
        },
        // No learned model was consulted for this fixture decision.
        ml_ranking: None,
    };

    let json = serde_json::to_string(&decision).unwrap();
    let back: RouteDecision = serde_json::from_str(&json).unwrap();

    assert_eq!(back.decision_id, "dec-001");
    assert_eq!(back.timestamp, 1700000000);
    assert_eq!(back.task.complexity, "complex");
    assert_eq!(back.policy_id, "code-policy");
    assert_eq!(back.client_id, Some("codex".into()));
    assert_eq!(back.candidates.len(), 2);
    assert!(back.candidates[0].eligible);
    assert!(!back.candidates[1].eligible);
    assert_eq!(back.candidates[1].rejection, Some("below_min_tier".into()));
    assert_eq!(back.selected, Some("gpt-4".into()));
    assert_eq!(back.fallback_chain, vec!["claude-3"]);
}

#[test]
fn candidate_decision_with_score() {
    let cd = CandidateDecision {
        model_id: "gpt-4".into(),
        provider_id: "openai".into(),
        tier: Some("standard".into()),
        eligible: true,
        rejection: None,
        score: Some(ScoreBreakdown {
            health: 1.0,
            latency: 0.9,
            cost: 0.7,
            priority: 0.8,
            tier: 1.0,
        }),
        final_score: Some(0.88),
    };
    let json = serde_json::to_string(&cd).unwrap();
    let back: CandidateDecision = serde_json::from_str(&json).unwrap();
    assert!(back.eligible);
    assert!(back.score.is_some());
    assert!((back.final_score.unwrap() - 0.88).abs() < 0.001);
    assert!(back.rejection.is_none());
}

#[test]
fn candidate_decision_without_score() {
    let cd = CandidateDecision {
        model_id: "llama-3".into(),
        provider_id: "ollama".into(),
        tier: None,
        eligible: false,
        rejection: Some("circuit_open".into()),
        score: None,
        final_score: None,
    };
    let json = serde_json::to_string(&cd).unwrap();
    let back: CandidateDecision = serde_json::from_str(&json).unwrap();
    assert!(!back.eligible);
    assert!(back.score.is_none());
    assert!(back.final_score.is_none());
    assert_eq!(back.rejection.as_deref(), Some("circuit_open"));
}

#[test]
fn decision_reason_direct_serializes() {
    let reason = DecisionReason::Direct;
    let json = serde_json::to_string(&reason).unwrap();
    assert!(json.contains("\"direct\""));
    let back: DecisionReason = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, DecisionReason::Direct));
}

#[test]
fn decision_reason_policy_selected_serializes() {
    let reason = DecisionReason::PolicySelected;
    let json = serde_json::to_string(&reason).unwrap();
    assert!(json.contains("\"policy_selected\""));
    let back: DecisionReason = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, DecisionReason::PolicySelected));
}

#[test]
fn decision_reason_fallback_serializes() {
    let reason = DecisionReason::Fallback {
        from: "gpt-4".into(),
        reason: "timeout".into(),
    };
    let json = serde_json::to_string(&reason).unwrap();
    assert!(json.contains("fallback"));
    assert!(json.contains("gpt-4"));
    assert!(json.contains("timeout"));
    let back: DecisionReason = serde_json::from_str(&json).unwrap();
    if let DecisionReason::Fallback { from, reason } = back {
        assert_eq!(from, "gpt-4");
        assert_eq!(reason, "timeout");
    } else {
        panic!("expected Fallback variant");
    }
}

#[test]
fn decision_reason_escalated_serializes() {
    let reason = DecisionReason::Escalated {
        from_tier: "fast".into(),
    };
    let json = serde_json::to_string(&reason).unwrap();
    assert!(json.contains("escalated"));
    assert!(json.contains("fast"));
    let back: DecisionReason = serde_json::from_str(&json).unwrap();
    if let DecisionReason::Escalated { from_tier } = back {
        assert_eq!(from_tier, "fast");
    } else {
        panic!("expected Escalated variant");
    }
}

#[test]
fn decision_reason_degraded_serializes() {
    let reason = DecisionReason::Degraded {
        from_tier: "frontier".into(),
    };
    let json = serde_json::to_string(&reason).unwrap();
    assert!(json.contains("degraded"));
    let back: DecisionReason = serde_json::from_str(&json).unwrap();
    if let DecisionReason::Degraded { from_tier } = back {
        assert_eq!(from_tier, "frontier");
    } else {
        panic!("expected Degraded variant");
    }
}

#[test]
fn decision_reason_no_candidate_serializes() {
    let reason = DecisionReason::NoCandidate;
    let json = serde_json::to_string(&reason).unwrap();
    assert!(json.contains("no_candidate"));
    let back: DecisionReason = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, DecisionReason::NoCandidate));
}

#[test]
fn rejection_reason_display() {
    assert_eq!(
        RejectionReason::MissingCapability(Capability::Vision).to_string(),
        "missing_capability:vision"
    );
    assert_eq!(
        RejectionReason::MissingCapability(Capability::Tools).to_string(),
        "missing_capability:tools"
    );
    assert_eq!(RejectionReason::BelowMinTier.to_string(), "below_min_tier");
    assert_eq!(RejectionReason::AboveMaxTier.to_string(), "above_max_tier");
    assert_eq!(RejectionReason::UnknownTier.to_string(), "unknown_tier");
    assert_eq!(
        RejectionReason::ProviderForbidden.to_string(),
        "provider_forbidden"
    );
    assert_eq!(
        RejectionReason::ProviderNotAllowed.to_string(),
        "provider_not_allowed"
    );
    assert_eq!(
        RejectionReason::ModelForbidden.to_string(),
        "model_forbidden"
    );
    assert_eq!(
        RejectionReason::ModelNotAllowed.to_string(),
        "model_not_allowed"
    );
    assert_eq!(RejectionReason::CircuitOpen.to_string(), "circuit_open");
}
