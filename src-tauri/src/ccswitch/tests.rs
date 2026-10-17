//! The CC Switch import fixtures.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use zroutery_core::config::{AppConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind};

use super::read::{provider_from_env, read_from_legacy_json};
use super::*;

fn env_block(map: Value) -> Value {
    map
}

#[test]
fn tier_defaults_become_tier_members() {
    let env = env_block(json!({
        "ANTHROPIC_BASE_URL": "https://relay.example/v1",
        "ANTHROPIC_AUTH_TOKEN": "sk-x",
        "ANTHROPIC_DEFAULT_OPUS_MODEL": "big-model[1M]",
        "ANTHROPIC_DEFAULT_SONNET_MODEL": "mid-model",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": "small-model",
        "ANTHROPIC_MODEL": "mid-model"
    }));
    let p = provider_from_env("id".into(), "Relay".into(), &env, false).unwrap();
    assert_eq!(p.base_url, "https://relay.example/v1");
    assert_eq!(p.api_key.as_deref(), Some("sk-x"));
    // [1M] is a window modifier, not part of the model name.
    let find = |name: &str| {
        p.models
            .iter()
            .find(|m| m.upstream_model == name)
            .and_then(|m| m.tier)
    };
    assert_eq!(find("big-model"), Some(ModelTier::Reasoning));
    assert_eq!(find("mid-model"), Some(ModelTier::Standard));
    assert_eq!(find("small-model"), Some(ModelTier::Fast));
    // ANTHROPIC_MODEL matched an existing entry, so no duplicate.
    assert_eq!(p.models.len(), 3);
}

#[test]
fn a_single_model_relay_collapses_to_sonnet() {
    let env = env_block(json!({
        "ANTHROPIC_BASE_URL": "https://relay.example/v1",
        "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.3[1M]",
        "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.3[1M]",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-5.3",
        "CLAUDE_CODE_SUBAGENT_MODEL": "glm-5.3[1m]"
    }));
    let p = provider_from_env("id".into(), "Relay".into(), &env, true).unwrap();
    assert_eq!(p.models.len(), 1);
    assert_eq!(p.models[0].upstream_model, "glm-5.3");
    // The class a conversation asks for, so sonnet-class works on day one.
    assert_eq!(p.models[0].tier, Some(ModelTier::Standard));
}

/// Two tier defaults naming the same model.
type SharedTierCase = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);
/// Two tier defaults naming different models, with the tiers they map to.
type DistinctTierCase = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    Vec<(&'static str, ModelTier)>,
);

#[test]
fn a_two_tier_relay_collapses_without_inventing_a_sonnet_entry() {
    // A relay may set any two of the three tier defaults naming the same
    // model. All of these were read through `sonnet`, which panicked on the
    // opus+haiku pair and aborted the entire provider import with it.
    let cases: [SharedTierCase; 3] = [
        (
            "opus+sonnet",
            Some("relay-model"),
            Some("relay-model"),
            None,
        ),
        ("opus+haiku", Some("relay-model"), None, Some("relay-model")),
        (
            "sonnet+haiku",
            None,
            Some("relay-model"),
            Some("relay-model"),
        ),
    ];
    for (name, opus, sonnet, haiku) in cases {
        let mut env = json!({
            "ANTHROPIC_BASE_URL": "https://relay.example/v1",
            "ANTHROPIC_AUTH_TOKEN": "sk-x",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": opus,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": sonnet,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": haiku,
        });
        // json! would keep the None placeholders as null, which the reader
        // already treats as unset; drop them so the shape matches a real
        // partial mapping.
        let obj = env.as_object_mut().unwrap();
        obj.retain(|_, value| !value.is_null());
        let p = provider_from_env("id".into(), "Relay".into(), &env, false)
            .unwrap_or_else(|| panic!("{name} was rejected"));
        assert_eq!(p.models.len(), 1, "{name}");
        assert_eq!(p.models[0].upstream_model, "relay-model", "{name}");
        assert_eq!(p.models[0].tier, Some(ModelTier::Standard), "{name}");
    }
}

#[test]
fn distinct_two_tier_models_still_get_their_own_tiers() {
    // The collapse must not swallow two genuinely different models.
    let cases: [DistinctTierCase; 3] = [
        (
            "opus+sonnet",
            Some("big-model"),
            Some("mid-model"),
            None,
            vec![
                ("big-model", ModelTier::Reasoning),
                ("mid-model", ModelTier::Standard),
            ],
        ),
        (
            "opus+haiku",
            Some("big-model"),
            None,
            Some("small-model"),
            vec![
                ("big-model", ModelTier::Reasoning),
                ("small-model", ModelTier::Fast),
            ],
        ),
        (
            "sonnet+haiku",
            None,
            Some("mid-model"),
            Some("small-model"),
            vec![
                ("mid-model", ModelTier::Standard),
                ("small-model", ModelTier::Fast),
            ],
        ),
    ];
    for (name, opus, sonnet, haiku, expected) in cases {
        let mut env = json!({
            "ANTHROPIC_BASE_URL": "https://relay.example/v1",
            "ANTHROPIC_AUTH_TOKEN": "sk-x",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": opus,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": sonnet,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": haiku,
        });
        let obj = env.as_object_mut().unwrap();
        obj.retain(|_, value| !value.is_null());
        let p = provider_from_env("id".into(), "Relay".into(), &env, false)
            .unwrap_or_else(|| panic!("{name} was rejected"));
        let tiers: Vec<(&str, Option<ModelTier>)> = p
            .models
            .iter()
            .map(|m| (m.upstream_model.as_str(), m.tier))
            .collect();
        assert_eq!(
            tiers,
            expected
                .iter()
                .map(|(model, tier)| (*model, Some(*tier)))
                .collect::<Vec<_>>(),
            "{name}"
        );
    }
}

#[test]
fn the_subagent_model_joins_when_it_differs() {
    let env = env_block(json!({
        "ANTHROPIC_BASE_URL": "https://relay.example/v1",
        "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.3",
        "CLAUDE_CODE_SUBAGENT_MODEL": "glm-5.3-air[1M]"
    }));
    let p = provider_from_env("id".into(), "Relay".into(), &env, false).unwrap();
    assert_eq!(p.models.len(), 2);
    assert!(p
        .models
        .iter()
        .any(|m| m.upstream_model == "glm-5.3-air" && m.tier.is_none()));
}

#[test]
fn providers_without_a_base_url_are_dropped() {
    // CC Switch ships an empty "official" row.
    let env = env_block(json!({"ANTHROPIC_AUTH_TOKEN": "sk-x"}));
    assert!(provider_from_env("id".into(), "Empty".into(), &env, false).is_none());
}

#[test]
fn ids_are_slugified_and_made_unique() {
    let taken = |id: &str| id == "deepseek" || id == "deepseek-2";
    assert_eq!(unique_provider_id("DeepSeek", &taken), "deepseek-3");
    assert_eq!(unique_provider_id("Xiaomi MiMo", &taken), "xiaomi-mimo");
    assert_eq!(unique_provider_id("---", &taken), "provider");
}

#[test]
fn drafts_become_provider_configs_with_the_relay_defaults() {
    let draft = CcProvider {
        source_id: "abc".into(),
        name: "StepFun".into(),
        base_url: "https://api.stepfun.com/step_plan/".into(),
        api_key: Some("k".into()),
        models: vec![CcModel {
            upstream_model: "step-3.7-flash".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: true,
        timeout_ms: None,
    };
    let (provider, models) = to_zroutery(&draft, "stepfun".into(), 0, Some(3_000_000));
    assert_eq!(provider.id, "stepfun");
    assert_eq!(provider.base_url, "https://api.stepfun.com/step_plan");
    assert_eq!(provider.kind, ProviderKind::Anthropic);
    assert_eq!(
        provider.client_profile,
        zroutery_core::config::ProviderClientProfile::ClaudeCode
    );
    // 3,000,000 ms is 3000 s: within the clamp, carried over as-is.
    assert_eq!(provider.timeout_secs, 3000);
    // Beyond the hour it is clamped, not carried literally.
    let (provider, _) = to_zroutery(&draft, "stepfun".into(), 0, Some(9_000_000));
    assert_eq!(provider.timeout_secs, 3600);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].upstream_model, "step-3.7-flash");
    assert_eq!(models[0].tier, Some(ModelTier::Standard));
    assert_eq!(models[0].exposed_id(), "stepfun-step-3.7-flash");
}

/// The whole production path for a timeout: read the file CC Switch wrote,
/// build the draft, convert the draft. The converter alone was already
/// covered, which is exactly why the deadline never survived an import.
#[test]
fn a_configured_api_timeout_survives_read_draft_and_import() {
    let dir = std::env::temp_dir().join(format!("ccswitch-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"providers": {"claude": [
            {"id": "p1", "name": "Relay One", "is_current": true,
             "settings_config": {"env": {
                "ANTHROPIC_BASE_URL": "https://one.example/v1",
                "ANTHROPIC_AUTH_TOKEN": "sk-1",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "m1",
                "API_TIMEOUT_MS": "3000000"}}}
        ]}}"#,
    )
    .unwrap();
    // The variable is the only way in, so the read path is the real one.
    std::env::set_var("ZROUTERY_CC_SWITCH_DIR", &dir);
    let providers = read_providers().expect("the temporary installation is readable");
    std::env::remove_var("ZROUTERY_CC_SWITCH_DIR");

    assert_eq!(providers.len(), 1);
    // 3,000,000 ms is 3000 s; it is inside the accepted range, so the draft
    // carries it and the import must not fall back to the default.
    assert_eq!(providers[0].timeout_ms, Some(3_000_000));
    let (provider, models) = to_zroutery(
        &providers[0],
        "relay-one".into(),
        0,
        providers[0].timeout_ms,
    );
    assert_eq!(provider.timeout_secs, 3000);
    assert_eq!(models.len(), 1);

    // A value outside the accepted range is refused rather than clamped:
    // "effectively forever" is not a deadline, so the default applies.
    for raw in ["30000000", "0", "-5", "", "soon"] {
        let env = env_block(json!({
            "ANTHROPIC_BASE_URL": "https://one.example/v1",
            "ANTHROPIC_AUTH_TOKEN": "sk-1",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "m1",
            "API_TIMEOUT_MS": raw,
        }));
        let draft = provider_from_env("p1".into(), "Relay One".into(), &env, true).unwrap();
        assert_eq!(draft.timeout_ms, None, "accepted `{raw}`");
        let (provider, _) = to_zroutery(&draft, "relay-one".into(), 0, draft.timeout_ms);
        assert_eq!(provider.timeout_secs, 600, "default lost for `{raw}`");
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn legacy_json_shape_parses() {
    let dir = std::env::temp_dir().join(format!("ccswitch-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"providers": {"claude": [
            {"id": "p1", "name": "Relay One",
             "settings_config": {"env": {
                "ANTHROPIC_BASE_URL": "https://one.example/v1",
                "ANTHROPIC_AUTH_TOKEN": "sk-1",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "m1"}}},
            {"id": "p2", "name": "Empty", "settings_config": {"env": {}}}
        ]}}"#,
    )
    .unwrap();
    let providers = read_from_legacy_json(&dir.join("config.json")).unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, "Relay One");
    assert_eq!(providers[0].models.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// Reads this machine's real CC Switch database. Opt-in —
/// `cargo test -p zroutery ccswitch -- --ignored` — because it depends
/// on a local installation. What it checks is the parsing contract: every
/// provider that has a base URL yields at least one model, and no key
/// material appears in the model list.
#[test]
#[ignore = "needs a local CC Switch installation"]
fn the_real_local_database_parses() {
    let providers = read_providers().expect("cc-switch is installed on this machine");
    assert!(!providers.is_empty(), "found no providers at all");
    for p in &providers {
        assert!(!p.base_url.is_empty(), "{} has no base url", p.name);
        for m in &p.models {
            assert!(!m.upstream_model.contains('['), "modifier survived: {m:?}");
            assert!(!m.upstream_model.contains(']'), "modifier survived: {m:?}");
        }
        println!(
            "[{}] {} -> {} models: [{}]",
            p.source_id,
            p.name,
            p.base_url,
            p.models
                .iter()
                .map(|m| format!(
                    "{}{}",
                    m.upstream_model,
                    m.tier
                        .map(|c| format!("({})", c.as_str()))
                        .unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

#[test]
fn api_key_is_not_serialized_in_draft() {
    let draft = CcProvider {
        source_id: "s1".into(),
        name: "Relay".into(),
        base_url: "https://relay.example/v1".into(),
        api_key: Some("sk-secret-token-12345".into()),
        models: vec![CcModel {
            upstream_model: "m1".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    };
    let json = serde_json::to_string(&draft).unwrap();
    assert!(
        !json.contains("sk-secret-token-12345"),
        "api_key leaked into serialized CcProvider: {json}"
    );
    assert!(
        !json.contains("api_key"),
        "api_key field present in serialized CcProvider: {json}"
    );
}

#[test]
fn api_key_is_not_serialized_in_draft_preview() {
    let provider = CcProvider {
        source_id: "s1".into(),
        name: "Relay".into(),
        base_url: "https://relay.example/v1".into(),
        api_key: Some("sk-top-secret".into()),
        models: vec![CcModel {
            upstream_model: "m1".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    };
    let preview = CcProviderDraft {
        provider,
        target_id: "relay".into(),
        already_imported: false,
    };
    let json = serde_json::to_string(&preview).unwrap();
    assert!(
        !json.contains("sk-top-secret"),
        "api_key leaked through CcProviderDraft: {json}"
    );
}

#[test]
fn validate_draft_rejects_empty_base_url() {
    let draft = CcProvider {
        source_id: "s1".into(),
        name: "Empty".into(),
        base_url: "".into(),
        api_key: None,
        models: vec![CcModel {
            upstream_model: "m1".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    };
    let err = validate_draft(&draft).unwrap_err();
    assert!(err.contains("no base url"), "unexpected error: {err}");
}

#[test]
fn validate_draft_rejects_no_models() {
    let draft = CcProvider {
        source_id: "s1".into(),
        name: "Empty".into(),
        base_url: "https://example.com/v1".into(),
        api_key: None,
        models: vec![],
        is_current: false,
        timeout_ms: None,
    };
    let err = validate_draft(&draft).unwrap_err();
    assert!(err.contains("no model entries"), "unexpected error: {err}");
}

#[test]
fn validate_draft_rejects_empty_upstream_model() {
    let draft = CcProvider {
        source_id: "s1".into(),
        name: "Bad".into(),
        base_url: "https://example.com/v1".into(),
        api_key: None,
        models: vec![CcModel {
            upstream_model: "  ".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    };
    let err = validate_draft(&draft).unwrap_err();
    assert!(
        err.contains("no upstream model name"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_draft_accepts_good_input() {
    let draft = CcProvider {
        source_id: "s1".into(),
        name: "Good".into(),
        base_url: "https://example.com/v1".into(),
        api_key: Some("sk-x".into()),
        models: vec![CcModel {
            upstream_model: "m1".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    };
    assert!(validate_draft(&draft).is_ok());
}

#[test]
fn import_batch_validation_catches_duplicate_providers() {
    let provider = ProviderConfig::new("same-id", "A", ProviderKind::Anthropic);
    let models = vec![ModelEntry::for_upstream(
        "same-id",
        "m1",
        Some(ModelTier::Standard),
    )];
    let batch = vec![(provider.clone(), models.clone()), (provider, models)];
    let (warnings, errors) = validate_import_batch(&batch);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("same-id") && e.contains("2 times")),
        "expected duplicate provider error, got: {errors:?}"
    );
    let _ = warnings;
}

#[test]
fn import_batch_validation_catches_duplicate_model_ids() {
    let p1 = ProviderConfig::new("p1", "A", ProviderKind::Anthropic);
    let p2 = ProviderConfig::new("p2", "B", ProviderKind::Anthropic);
    let m1 = ModelEntry::for_upstream("p1", "m1", Some(ModelTier::Standard));
    let m2 = ModelEntry::for_upstream("p1", "m1", Some(ModelTier::Fast));
    let batch = vec![(p1.clone(), vec![m1]), (p2, vec![m2])];
    let (_warnings, errors) = validate_import_batch(&batch);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("p1-m1") && e.contains("2 times")),
        "expected duplicate model error, got: {errors:?}"
    );
}

#[test]
fn import_batch_validation_warns_on_unclassified_models() {
    let provider = ProviderConfig::new("p1", "A", ProviderKind::Anthropic);
    let model = ModelEntry::for_upstream("p1", "m1", None);
    let batch = vec![(provider, vec![model])];
    let (warnings, _errors) = validate_import_batch(&batch);
    assert!(
        warnings.iter().any(|w| w.contains("no tier")),
        "expected unclassified warning, got: {warnings:?}"
    );
}

#[test]
fn import_report_counts_correctly() {
    let p1 = ProviderConfig::new("p1", "A", ProviderKind::Anthropic);
    let p2 = ProviderConfig::new("p2", "B", ProviderKind::Anthropic);
    let models1 = vec![
        ModelEntry::for_upstream("p1", "m1", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p1", "m2", Some(ModelTier::Fast)),
    ];
    let models2 = vec![ModelEntry::for_upstream(
        "p2",
        "m3",
        Some(ModelTier::Reasoning),
    )];
    let imported = vec![(p1, models1), (p2, models2)];
    let report = build_report("cc-switch.db", &imported, vec![], vec![]);
    assert_eq!(report.source, "cc-switch.db");
    assert_eq!(report.providers_imported, 2);
    assert_eq!(report.models_imported, 3);
    assert!(report.warnings.is_empty());
    assert!(report.errors.is_empty());
}

#[test]
fn secrets_are_never_in_provider_config() {
    let draft = CcProvider {
        source_id: "s1".into(),
        name: "Secret Relay".into(),
        base_url: "https://relay.example/v1".into(),
        api_key: Some("sk-super-secret-key".into()),
        models: vec![CcModel {
            upstream_model: "m1".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    };
    let (provider, _models) = to_zroutery(&draft, "relay".into(), 0, None);
    assert_ne!(provider.key_ref, "sk-super-secret-key");
    assert!(
        provider.key_ref.contains("relay"),
        "key_ref should reference the provider id, not the secret: {}",
        provider.key_ref,
    );
}

/// One CC Switch entry, with the key that makes it an account.
fn source_entry(id: &str, name: &str, url: &str, key: &str) -> CcProvider {
    CcProvider {
        source_id: id.into(),
        name: name.into(),
        base_url: url.into(),
        api_key: (!key.is_empty()).then(|| key.to_string()),
        models: vec![CcModel {
            upstream_model: "m1".into(),
            tier: Some(ModelTier::Standard),
        }],
        is_current: false,
        timeout_ms: None,
    }
}

/// The provider an import of `entry` would write.
fn imported(entry: &CcProvider, id: String) -> ProviderConfig {
    to_zroutery(entry, id, 0, entry.timeout_ms).0
}

#[test]
fn two_accounts_on_one_endpoint_are_two_configurations() {
    // The same relay, two subscriptions: different CC Switch entries, the
    // same URL, different credentials.
    let first = source_entry("src-a", "Relay", "https://relay.example/v1", "sk-a");
    let second = source_entry("src-b", "Relay", "https://relay.example/v1", "sk-b");

    let config = AppConfig::default();
    let plan = plan_import(
        &config,
        &[account_of(&first), account_of(&second)],
        &BTreeMap::new(),
    );
    let first_id = match &plan[0].decision {
        ImportDecision::Planned(id) => id.clone(),
        other => panic!("the first account must be importable, got {other:?}"),
    };
    let second_id = match &plan[1].decision {
        ImportDecision::Planned(id) => id.clone(),
        other => panic!("the second account must be importable, got {other:?}"),
    };
    assert_ne!(
        first_id, second_id,
        "two accounts on one endpoint must not collapse to one id"
    );

    // Import the first; the second must still be importable.
    let mut config = AppConfig::default();
    config.providers.push(imported(&first, first_id.clone()));
    let plan = plan_import(
        &config,
        &[account_of(&first), account_of(&second)],
        &BTreeMap::new(),
    );
    assert_eq!(
        plan[0].decision,
        ImportDecision::AlreadyImported(first_id.clone(), AlreadyImported::SameSource),
    );
    assert!(
        matches!(&plan[1].decision, ImportDecision::Planned(id) if id == &second_id),
        "the second account became unimportable: {:?}",
        plan[1].decision,
    );
}

#[test]
fn the_same_source_entry_is_recognised_after_a_round_trip_through_config() {
    let entry = source_entry("src-a", "Relay", "https://relay.example/v1", "sk-a");
    let id = source_target_id(&entry.name, &entry.source_id);
    let mut config = AppConfig::default();
    config.providers.push(imported(&entry, id.clone()));
    save_and_reload(&mut config);
    let plan = plan_import(&config, &[account_of(&entry)], &BTreeMap::new());
    assert_eq!(
        plan[0].decision,
        ImportDecision::AlreadyImported(id, AlreadyImported::SameSource),
        "re-importing the same entry must be recognised after a save/load cycle",
    );
}

#[test]
fn a_provider_without_a_marker_is_matched_by_endpoint_and_credential() {
    // A provider imported before the source marker existed: same endpoint,
    // same stored credential, plain name-derived id. The host supplies the
    // fingerprints because the key itself never enters the configuration.
    let entry = source_entry("src-a", "Relay", "https://relay.example/v1", "sk-a");
    let mut legacy = ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
    legacy.base_url = "https://relay.example/v1".into();
    let mut config = AppConfig::default();
    config.providers.push(legacy);
    let stored = BTreeMap::from([("provider:relay".to_string(), credential_fingerprint("sk-a"))]);

    let plan = plan_import(&config, &[account_of(&entry)], &stored);
    assert_eq!(
        plan[0].decision,
        ImportDecision::AlreadyImported("relay".into(), AlreadyImported::SameAccount),
    );
}

#[test]
fn a_different_credential_on_the_endpoint_is_a_second_account() {
    // Same endpoint, different stored key: this is the second account, not
    // a re-import, and it has to stay importable.
    let entry = source_entry("src-b", "Relay", "https://relay.example/v1", "sk-b");
    let mut other = ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
    other.base_url = "https://relay.example/v1".into();
    let mut config = AppConfig::default();
    config.providers.push(other);
    let stored = BTreeMap::from([("provider:relay".to_string(), credential_fingerprint("sk-a"))]);

    let plan = plan_import(&config, &[account_of(&entry)], &stored);
    assert!(
        matches!(plan[0].decision, ImportDecision::Planned(_)),
        "a second account was mistaken for the first: {:?}",
        plan[0].decision,
    );
}

#[test]
fn a_keyless_provider_is_not_mistaken_for_an_account_with_a_key() {
    let entry = source_entry("src-a", "Relay", "https://relay.example/v1", "sk-a");
    let mut keyless = ProviderConfig::new("relay", "Relay", ProviderKind::Anthropic);
    keyless.base_url = "https://relay.example/v1".into();
    keyless.key_ref = String::new();
    let mut config = AppConfig::default();
    config.providers.push(keyless);

    let plan = plan_import(&config, &[account_of(&entry)], &BTreeMap::new());
    assert!(
        matches!(plan[0].decision, ImportDecision::Planned(_)),
        "a keyless provider is not the same account: {:?}",
        plan[0].decision,
    );
}

#[test]
fn source_target_ids_are_stable_and_name_derived() {
    let id = source_target_id("Relay One", "src-a");
    assert!(id.starts_with("relay-one-"), "unrecognisable id: {id}");
    assert_eq!(id, source_target_id("Relay One", "src-a"));
    assert_ne!(id, source_target_id("Relay One", "src-b"));
}

/// The configuration parse is the persistence boundary: an id that does
/// not survive it cannot deduplicate anything.
fn save_and_reload(config: &mut AppConfig) {
    let text = serde_json::to_string(config).unwrap();
    *config = serde_json::from_str(&text).unwrap();
}

#[test]
fn import_of_ten_produces_no_config_drift() {
    let drafts: Vec<CcProvider> = (0..10)
        .map(|i| CcProvider {
            source_id: format!("src-{i}"),
            name: format!("Provider {i}"),
            base_url: format!("https://relay-{i}.example/v1"),
            api_key: Some(format!("sk-{i}")),
            models: vec![CcModel {
                upstream_model: format!("model-{i}"),
                tier: Some(ModelTier::Standard),
            }],
            is_current: i == 0,
            timeout_ms: None,
        })
        .collect();

    let run_import = || -> Vec<(ProviderConfig, Vec<ModelEntry>)> {
        let mut taken_ids = std::collections::BTreeSet::new();
        drafts
            .iter()
            .map(|d| {
                let id = unique_provider_id(&d.name, &|id| taken_ids.contains(id));
                taken_ids.insert(id.clone());
                to_zroutery(d, id, if d.is_current { 0 } else { 10 }, None)
            })
            .collect()
    };

    let first = run_import();
    for run in 1..10 {
        let subsequent = run_import();
        assert_eq!(first.len(), subsequent.len(), "run {run} length mismatch");
        for (a, b) in first.iter().zip(subsequent.iter()) {
            assert_eq!(a.0.id, b.0.id, "run {run}: provider id mismatch");
            assert_eq!(a.0.base_url, b.0.base_url, "run {run}: base_url mismatch");
            assert_eq!(a.1.len(), b.1.len(), "run {run}: model count mismatch");
            for (ma, mb) in a.1.iter().zip(b.1.iter()) {
                assert_eq!(
                    ma.exposed_id(),
                    mb.exposed_id(),
                    "run {run}: model id mismatch"
                );
                assert_eq!(ma.tier, mb.tier, "run {run}: tier mismatch");
            }
        }
    }
}
