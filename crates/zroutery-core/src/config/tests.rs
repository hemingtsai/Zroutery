//! The configuration fixtures: defaults, validation and every issue code validate can raise.

use super::app::AppConfig;
use super::*;
use crate::billing::BaseDepth;
use crate::ir::CapabilityState;

fn sample() -> AppConfig {
    let mut cfg = AppConfig::default();
    // A real install has a generated token; an empty one is its own issue.
    cfg.server.auth_token = "test-token".into();
    cfg.providers.push(ProviderConfig::new(
        "deepseek",
        "DeepSeek",
        ProviderKind::OpenAICompatible,
    ));
    cfg.models.push(ModelEntry::for_upstream(
        "deepseek",
        "deepseek-chat",
        Some(ModelTier::Standard),
    ));
    cfg
}

#[test]
fn exposed_ids_are_namespaced_by_provider() {
    let direct = ModelEntry::for_upstream("deepseek", "deepseek-chat", None);
    let proxied = ModelEntry::for_upstream("openrouter", "deepseek-chat", None);
    assert_eq!(direct.exposed_id(), "deepseek-deepseek-chat");
    assert_eq!(proxied.exposed_id(), "openrouter-deepseek-chat");
    assert_ne!(direct.exposed_id(), proxied.exposed_id());
}

#[test]
fn exposed_ids_stay_url_safe() {
    // OpenRouter style names carry a slash, and some providers use a colon.
    let m = ModelEntry::for_upstream("openrouter", "deepseek/deepseek-chat:free", None);
    assert_eq!(m.exposed_id(), "openrouter-deepseek-deepseek-chat-free");
    // Dots survive: they are common in version numbers.
    let m = ModelEntry::for_upstream("openai", " gpt-5.3-sol ", None);
    assert_eq!(m.exposed_id(), "openai-gpt-5.3-sol");
}

#[test]
fn models_answer_to_their_id_and_aliases() {
    let m = ModelEntry::for_upstream("deepseek", "deepseek-chat", None).with_alias("ds");
    assert!(m.answers_to("deepseek-deepseek-chat"));
    assert!(m.answers_to("ds"));
    assert!(!m.answers_to("deepseek-chat"));
}

#[test]
fn the_same_model_from_two_providers_is_allowed() {
    let mut cfg = sample();
    cfg.providers.push(ProviderConfig::new(
        "openrouter",
        "OpenRouter",
        ProviderKind::OpenAICompatible,
    ));
    cfg.models.push(ModelEntry::for_upstream(
        "openrouter",
        "deepseek-chat",
        Some(ModelTier::Standard),
    ));
    assert!(cfg
        .validate()
        .iter()
        .all(|i| i.severity != IssueSeverity::Error));
    assert_eq!(
        cfg.exposed_ids(),
        vec!["deepseek-deepseek-chat", "openrouter-deepseek-chat"]
    );
    assert_eq!(
        cfg.model("openrouter-deepseek-chat").unwrap().provider_id,
        "openrouter"
    );
    assert_eq!(
        cfg.model_by_key("deepseek", "deepseek-chat")
            .unwrap()
            .exposed_id(),
        "deepseek-deepseek-chat"
    );
}

#[test]
fn virtual_ids_round_trip() {
    for t in ModelTier::ALL {
        assert_eq!(ModelTier::from_virtual_id(t.virtual_id()), Some(t));
    }
    assert_eq!(ModelTier::from_virtual_id("gpt-5.3-sol"), None);
    // Anthropic aliases
    assert_eq!(
        ModelTier::from_virtual_id("haiku-class"),
        Some(ModelTier::Fast)
    );
    assert_eq!(
        ModelTier::from_virtual_id("sonnet-class"),
        Some(ModelTier::Standard)
    );
    assert_eq!(
        ModelTier::from_virtual_id("opus-class"),
        Some(ModelTier::Reasoning)
    );
    assert_eq!(
        ModelTier::from_virtual_id("fable-class"),
        Some(ModelTier::Frontier)
    );
    // OpenAI aliases
    assert_eq!(
        ModelTier::from_virtual_id("luna-class"),
        Some(ModelTier::Fast)
    );
    assert_eq!(
        ModelTier::from_virtual_id("terra-class"),
        Some(ModelTier::Standard)
    );
    assert_eq!(
        ModelTier::from_virtual_id("sol-class"),
        Some(ModelTier::Reasoning)
    );
    assert_eq!(
        ModelTier::from_virtual_id("astra-class"),
        Some(ModelTier::Frontier)
    );
}

#[test]
fn chat_urls() {
    let mut p = ProviderConfig::new("d", "DeepSeek", ProviderKind::OpenAICompatible);
    p.base_url = "https://api.deepseek.com/v1/".into();
    assert_eq!(p.chat_url(), "https://api.deepseek.com/v1/chat/completions");

    let a = ProviderConfig::new("a", "Anthropic", ProviderKind::Anthropic);
    assert_eq!(a.chat_url(), "https://api.anthropic.com/v1/messages");
}

#[test]
fn base_depth_matches_chat_url_v1_normalisation() {
    let mut openai_bare = ProviderConfig::new("o", "O", ProviderKind::OpenAICompatible);
    openai_bare.base_url = "https://api.example.com".into();
    assert_eq!(openai_bare.base_depth(), BaseDepth::ApiRoot);
    assert_eq!(
        openai_bare.chat_url(),
        "https://api.example.com/v1/chat/completions"
    );

    let mut openai_v1 = ProviderConfig::new("o", "O", ProviderKind::OpenAICompatible);
    openai_v1.base_url = "https://api.example.com/v1".into();
    assert_eq!(openai_v1.base_depth(), BaseDepth::Versioned);

    let anthropic_root = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
    assert_eq!(anthropic_root.base_depth(), BaseDepth::ApiRoot);

    let mut anthropic_v1 = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
    anthropic_v1.base_url = "https://relay.example/v1".into();
    assert_eq!(anthropic_v1.base_depth(), BaseDepth::Versioned);
}

#[test]
fn validate_flags_invalid_cors_origins() {
    let mut cfg = sample();
    cfg.server.allow_cors = true;
    cfg.server.cors_origins = vec!["https://good.example".into(), "not an origin".into()];
    let issues = cfg.validate();
    assert_eq!(
        issues
            .iter()
            .filter(|i| i.code == "server.cors_origin_invalid")
            .count(),
        1
    );

    // Whitespace-only entries are ignored, valid ones pass silently.
    cfg.server.cors_origins = vec!["  ".into(), "https://good.example".into()];
    let issues = cfg.validate();
    assert!(!issues
        .iter()
        .any(|i| i.code == "server.cors_origin_invalid"));

    // No check while CORS is off.
    cfg.server.allow_cors = false;
    cfg.server.cors_origins = vec!["garbage".into()];
    let issues = cfg.validate();
    assert!(!issues
        .iter()
        .any(|i| i.code == "server.cors_origin_invalid"));
}

#[test]
fn validate_flags_unclassified_and_empty_tiers() {
    let mut cfg = sample();
    cfg.models.push(ModelEntry::for_upstream(
        "deepseek",
        "deepseek-reasoner",
        None,
    ));
    let issues = cfg.validate();
    assert!(issues.iter().any(|i| i.code == "model.unclassified"
        && i.subject.as_deref() == Some("deepseek-deepseek-reasoner")));
    // standard is covered, reasoning/fast/frontier are not
    assert_eq!(issues.iter().filter(|i| i.code == "tier.empty").count(), 3);
    assert!(issues.iter().all(|i| i.severity == IssueSeverity::Warning));
}

#[test]
fn a_decision_model_is_not_nagged_about_a_tier() {
    let mut cfg = sample();
    cfg.providers.push(ProviderConfig::new(
        "jev",
        "Jev",
        ProviderKind::Decision,
    ));
    cfg.models
        .push(ModelEntry::for_upstream("jev", "jev-1.13-free", None));

    let issues = cfg.validate();
    assert!(
        !issues.iter().any(|i| i.code == "model.unclassified"
            && i.subject.as_deref() == Some("jev-jev-1.13-free")),
        "a decision model is deliberately outside every tier: {issues:?}"
    );
    assert!(!cfg
        .unclassified_models()
        .iter()
        .any(|m| m.exposed_id() == "jev-jev-1.13-free"));
}

#[test]
fn validate_flags_a_tier_whose_only_members_are_on_disabled_providers() {
    let mut cfg = sample();
    cfg.providers[0].enabled = false;
    let issues = cfg.validate();
    // standard is assigned but its provider is disabled, so it is effectively empty.
    assert!(issues
        .iter()
        .any(|i| i.code == "tier.empty" && i.subject.as_deref() == Some("standard-class")));
}

#[test]
fn validate_flags_impossible_timeouts_and_auth() {
    let mut cfg = sample();
    cfg.providers[0].timeout_secs = 0;
    assert!(cfg
        .validate()
        .iter()
        .any(|i| i.code == "provider.zero_timeout"));

    let mut cfg = sample();
    // An install that somehow has no token: requiring auth rejects everyone.
    cfg.server.auth_token.clear();
    assert!(cfg
        .validate()
        .iter()
        .any(|i| i.code == "server.empty_token"));

    let mut cfg = sample();
    cfg.server.require_auth = false;
    assert!(!cfg
        .validate()
        .iter()
        .any(|i| i.code == "server.empty_token"));
}

#[test]
fn validate_flags_no_auth_on_a_reachable_proxy() {
    let mut exposed = AppConfig::default();
    exposed.server.require_auth = false;
    exposed.server.host = "0.0.0.0".into();
    assert!(exposed
        .validate()
        .iter()
        .any(|i| i.code == "server.no_auth_reachable"));

    // Loopback with auth off is still only a local risk: no combined warning.
    let mut local = AppConfig::default();
    local.server.require_auth = false;
    assert!(!local
        .validate()
        .iter()
        .any(|i| i.code == "server.no_auth_reachable"));

    // Wide-open CORS is the other way to become reachable.
    let mut cors = AppConfig::default();
    cors.server.require_auth = false;
    cors.server.allow_cors = true;
    assert!(cors
        .validate()
        .iter()
        .any(|i| i.code == "server.no_auth_reachable"));
}

#[test]
fn validate_rejects_reserved_ids_duplicates_and_orphans() {
    let mut cfg = sample();
    // An alias must not shadow a virtual tier id.
    cfg.models[0].aliases.push("sonnet-class".into());
    // The very same provider and model listed twice.
    cfg.models.push(ModelEntry::for_upstream(
        "deepseek",
        "deepseek-chat",
        Some(ModelTier::Reasoning),
    ));
    cfg.models.push(ModelEntry::for_upstream(
        "nope",
        "whatever",
        Some(ModelTier::Fast),
    ));
    let issues = cfg.validate();
    assert!(issues.iter().any(|i| i.code == "model.reserved_id"));
    assert!(issues.iter().any(|i| i.code == "model.duplicate_id"
        && i.subject.as_deref() == Some("deepseek-deepseek-chat")));
    assert!(issues.iter().any(|i| i.code == "model.orphan"));
}

#[test]
fn validate_rejects_duplicate_provider_ids() {
    let mut cfg = sample();
    cfg.providers.push(ProviderConfig::new(
        "deepseek",
        "DeepSeek Again",
        ProviderKind::OpenAICompatible,
    ));
    let issues = cfg.validate();
    assert!(issues
        .iter()
        .any(|i| i.code == "provider.duplicate_id" && i.subject.as_deref() == Some("deepseek")));
}

#[test]
fn validate_rejects_an_alias_shared_by_two_models() {
    let mut cfg = sample();
    cfg.providers.push(ProviderConfig::new(
        "openrouter",
        "OpenRouter",
        ProviderKind::OpenAICompatible,
    ));
    cfg.models[0].aliases.push("chat".into());
    cfg.models.push(
        ModelEntry::for_upstream("openrouter", "deepseek-chat", Some(ModelTier::Standard))
            .with_alias("chat"),
    );
    assert!(cfg
        .validate()
        .iter()
        .any(|i| i.code == "model.duplicate_id" && i.subject.as_deref() == Some("chat")));
}

#[test]
fn exposure_detection() {
    let mut s = ServerConfig::default();
    assert!(!s.is_exposed());
    s.host = "0.0.0.0".into();
    assert!(s.is_exposed());
}

#[test]
fn config_json_round_trip() {
    let mut cfg = sample();
    cfg.normalize();
    let json = serde_json::to_string(&cfg).unwrap();
    // Identity is persisted, the derived id is not.
    assert!(json.contains("\"upstream_model\":\"deepseek-chat\""));
    assert!(!json.contains("\"id\":\"deepseek-deepseek-chat\""));
    // Legacy supports_* fields should not appear in serialized output.
    assert!(!json.contains("\"supports_tools\""));
    let mut back: AppConfig = serde_json::from_str(&json).unwrap();
    back.normalize();
    assert_eq!(cfg, back);
}

#[test]
fn legacy_breaker_fields_migrate_into_circuit_breaker() {
    let cfg: AppConfig = serde_json::from_str(
        r#"{
            "routing": {
                "break_after_failures": 2,
                "cooldown_secs": 7
            }
        }"#,
    )
    .unwrap();
    assert_eq!(cfg.routing.circuit_breaker.failure_threshold, 4);
    assert_eq!(cfg.routing.circuit_breaker.timeout_secs, 60);

    let mut cfg = cfg;
    cfg.normalize();
    assert_eq!(cfg.routing.circuit_breaker.failure_threshold, 2);
    assert_eq!(cfg.routing.circuit_breaker.timeout_secs, 7);
    assert!(cfg.routing.break_after_failures.is_none());
    assert!(cfg.routing.cooldown_secs.is_none());
}

#[test]
fn normalize_keeps_pre_0_2_ids_working_as_aliases() {
    // A configuration written by 0.1.x, where the id was free-form.
    let mut cfg: AppConfig = serde_json::from_str(
        r#"{"providers":[{"id":"deepseek","name":"DeepSeek","kind":"openai_compatible","base_url":"http://x"}],
            "models":[{"id":"deepseek-v4-pro","provider_id":"deepseek","upstream_model":"deepseek-v4-pro","class":"sonnet"}]}"#,
    )
    .unwrap();
    let notes = cfg.normalize();

    assert_eq!(cfg.models[0].exposed_id(), "deepseek-deepseek-v4-pro");
    assert_eq!(cfg.models[0].aliases, vec!["deepseek-v4-pro"]);
    assert!(notes[0].contains("deepseek-v4-pro"));
    // Clients pinned to the old id keep resolving.
    assert!(cfg.model("deepseek-v4-pro").is_some());
    assert!(cfg.model("deepseek-deepseek-v4-pro").is_some());
    // Running it again changes nothing.
    assert!(cfg.normalize().is_empty());
    assert_eq!(cfg.models[0].aliases, vec!["deepseek-v4-pro"]);
}

#[test]
fn normalize_drops_redundant_and_empty_aliases() {
    let mut cfg: AppConfig = serde_json::from_str(
        r#"{"models":[{"id":"p-m","provider_id":"p","upstream_model":"m",
                       "aliases":["dup","dup","  ","p-m"]}]}"#,
    )
    .unwrap();
    cfg.normalize();
    assert_eq!(cfg.models[0].aliases, vec!["dup"]);
}

#[test]
fn classifier_config_round_trips_and_stays_off_by_default() {
    let mut cfg = sample();
    cfg.normalize();
    assert!(!cfg.classifier.enabled);
    assert!(cfg.classifier.candidates.is_empty());
    let json = serde_json::to_string(&cfg).unwrap();
    let mut back: AppConfig = serde_json::from_str(&json).unwrap();
    back.normalize();
    assert_eq!(cfg, back);

    // And a config that predates classifier routing loads without it.
    let legacy: AppConfig =
        serde_json::from_str(r#"{"server":{"auth_token":"zr-x"},"models":[]}"#).unwrap();
    assert!(!legacy.classifier.enabled);
}

#[test]
fn classifier_candidates_are_validated_against_the_registry() {
    let mut cfg = sample();
    cfg.classifier.enabled = true;

    // A candidate naming a model that does not exist is reported — as a warning,
    // not an error: the router skips it at routing time, and an error here once
    // blocked every save a panel attempted until the stale entry was hand-fixed.
    cfg.classifier.candidates.push(ClassifierCandidate {
        model: "nope-missing".into(),
        priority: 10,
        enabled: true,
    });
    let issues = cfg.validate();
    assert!(issues
        .iter()
        .any(|i| i.code == "classifier.candidate_unknown"));
    assert!(
        issues
            .iter()
            .find(|i| i.code == "classifier.candidate_unknown")
            .is_some_and(|i| i.severity == IssueSeverity::Warning),
        "a dangling candidate warns instead of blocking every save"
    );

    // A valid candidate on an enabled model is fine...
    cfg.classifier.candidates[0].model = "deepseek-deepseek-chat".into();
    assert!(!cfg
        .validate()
        .iter()
        .any(|i| i.code.starts_with("classifier.")));

    // ...until the model itself is disabled, which is only a warning.
    cfg.models[0].enabled = false;
    assert!(cfg
        .validate()
        .iter()
        .any(|i| i.code == "classifier.candidate_disabled"));
}

#[test]
fn an_enabled_classifier_with_no_live_candidate_warns() {
    let mut cfg = sample();
    cfg.classifier.enabled = true;
    cfg.classifier.candidates.push(ClassifierCandidate {
        model: "deepseek-deepseek-chat".into(),
        priority: 10,
        enabled: false,
    });
    assert!(cfg
        .validate()
        .iter()
        .any(|i| i.code == "classifier.no_candidates"));

    // Nothing is checked while the feature is off.
    cfg.classifier.enabled = false;
    assert!(!cfg
        .validate()
        .iter()
        .any(|i| i.code.starts_with("classifier.")));
}

#[test]
fn partial_json_uses_defaults() {
    let cfg: AppConfig = serde_json::from_str(
        r#"{"providers":[{"id":"p","name":"P","kind":"openai_compatible","base_url":"http://x"}],
            "models":[{"provider_id":"p","upstream_model":"m"}]}"#,
    )
    .unwrap();
    assert_eq!(cfg.server.port, 8787);
    assert!(cfg.server.require_auth);
    assert!(cfg.models[0].enabled);
    assert_eq!(cfg.models[0].weight, 1);
    assert_eq!(cfg.models[0].tier, None);
    assert_eq!(cfg.models[0].exposed_id(), "p-m");
}

#[test]
fn chat_url_tolerates_base_urls_with_or_without_v1() {
    // OpenAI-compatible: bare hosts get /v1, /v1 bases stay single.
    let mut p = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    p.base_url = "https://api.example.com".into();
    assert_eq!(p.chat_url(), "https://api.example.com/v1/chat/completions");
    let mut v1 = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    v1.base_url = "https://ps.air-outer.com/v1/".into();
    assert_eq!(
        v1.chat_url(),
        "https://ps.air-outer.com/v1/chat/completions"
    );
    let mut v1 = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    v1.base_url = "https://ps.air-outer.com/v1/".into();
    assert_eq!(
        v1.chat_url(),
        "https://ps.air-outer.com/v1/chat/completions"
    );

    // Anthropic: a /v1 base must not become /v1/v1/messages.
    let a = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
    assert_eq!(a.chat_url(), "https://api.anthropic.com/v1/messages");
    let mut av1 = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
    av1.base_url = "https://relay.example/v1".into();
    assert_eq!(av1.chat_url(), "https://relay.example/v1/messages");
}

#[test]
fn models_url_avoids_double_v1() {
    // base_url without /v1
    let p1 = ProviderConfig::new("p1", "P1", ProviderKind::Anthropic);
    assert!(p1.models_url().ends_with("/v1/models"));

    // base_url already ends with /v1
    let mut p2 = ProviderConfig::new("p2", "P2", ProviderKind::Anthropic);
    p2.base_url = "https://example.com/v1".to_string();
    assert_eq!(p2.models_url(), "https://example.com/v1/models");

    // base_url ends with /v1/
    let mut p3 = ProviderConfig::new("p3", "P3", ProviderKind::Anthropic);
    p3.base_url = "https://example.com/v1/".to_string();
    assert_eq!(p3.models_url(), "https://example.com/v1/models");

    // OpenAI compatible without /v1 also gets /v1/models
    let p4 = ProviderConfig::new("p4", "P4", ProviderKind::OpenAICompatible);
    assert!(p4.models_url().ends_with("/v1/models"));

    // OpenAI compatible with /v1 in base_url
    let mut p5 = ProviderConfig::new("p5", "P5", ProviderKind::OpenAICompatible);
    p5.base_url = "https://api.openai.com/v1".to_string();
    assert_eq!(p5.models_url(), "https://api.openai.com/v1/models");
}

#[test]
fn capability_state_true_is_supported() {
    let caps = ModelCapabilities {
        vision: true,
        ..ModelCapabilities::default()
    };
    assert_eq!(
        caps.capability_state(crate::ir::Capability::Vision),
        CapabilityState::Supported,
    );
}

#[test]
fn capability_state_false_is_unknown() {
    let caps = ModelCapabilities::default();
    assert_eq!(
        caps.capability_state(crate::ir::Capability::Vision),
        CapabilityState::Unknown,
    );
    assert_eq!(
        caps.capability_state(crate::ir::Capability::Tools),
        CapabilityState::Unknown,
    );
}

#[test]
fn capability_state_serde_round_trip() {
    for state in [
        CapabilityState::Supported,
        CapabilityState::Unsupported,
        CapabilityState::Unknown,
    ] {
        let json = serde_json::to_string(&state).unwrap();
        let back: CapabilityState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }
}

#[test]
fn strict_capability_filter_defaults_to_false() {
    let cfg = RoutingConfig::default();
    assert!(!cfg.strict_capability_filter);
}

#[test]
fn strict_capability_filter_serde_round_trip() {
    let cfg = RoutingConfig {
        strict_capability_filter: true,
        ..RoutingConfig::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(json.contains("\"strict_capability_filter\":true"));
    let back: RoutingConfig = serde_json::from_str(&json).unwrap();
    assert!(back.strict_capability_filter);
}

#[test]
fn strict_capability_filter_absent_in_json_defaults_false() {
    let cfg: RoutingConfig = serde_json::from_str("{}").unwrap();
    assert!(!cfg.strict_capability_filter);
}
