use zroutery_core::policy::{resolve_client, ClientContext, ClientMatcher, ClientProfile};

use super::support::*;

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
        application: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_upper = ClientContext {
        client_id: None,
        user_agent: Some("CURL/7.68.0"),
        api_key_prefix: None,
        application: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: Some("python-requests/2.28"),
        api_key_prefix: None,
        application: None,
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
        application: None,
        headers: &[],
        model: "gpt-4o",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        application: None,
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
        application: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: Some("sk-proj-abc"),
        application: None,
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
        application: None,
        headers: &headers_match,
        model: "gpt-4",
    };
    let ctx_case = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        application: None,
        headers: &headers_case,
        model: "gpt-4",
    };
    let ctx_miss = ClientContext {
        client_id: None,
        user_agent: None,
        api_key_prefix: None,
        application: None,
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
        application: None,
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
        application: None,
        headers: &[],
        model: "gpt-4",
    };
    let ctx_partial = ClientContext {
        client_id: Some("codex"),
        user_agent: None,
        api_key_prefix: None,
        application: None,
        headers: &[],
        model: "claude-3",
    };
    assert!(profile.matches(&ctx_both));
    assert!(!profile.matches(&ctx_partial));
}

#[test]
fn resolve_client_returns_first_match() {
    let profiles = vec![bot_profile(), codex_profile(), ua_profile()];
    let ctx = ClientContext {
        client_id: Some("codex"),
        user_agent: Some("curl/7.0"),
        api_key_prefix: Some("sk-bot-x"),
        application: None,
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
        application: None,
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
        application: None,
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
        application: None,
        headers: &[],
        model: "gpt-4",
    };
    let resolved = resolve_client(&profiles, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().id, "bot");
}
