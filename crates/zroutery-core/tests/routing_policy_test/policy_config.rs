use zroutery_core::config::ModelTier;
use zroutery_core::ir::Capability;
use zroutery_core::policy::{
    Complexity, PolicyConfig, PolicyFallback, PolicyMatcher, PolicyPreference, PolicyRequirements,
    RoutingPolicy, TaskType,
};

use super::support::*;

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
                PolicyMatcher::Client { value: "c".into() },
                PolicyMatcher::Application {
                    value: "app".into(),
                },
                PolicyMatcher::ModelPrefix { value: "mp".into() },
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
    assert_eq!(back.policies[0].matchers.len(), 8);
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
