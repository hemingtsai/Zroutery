//! The transport fixtures.

use reqwest::header::AUTHORIZATION;

use crate::config::{ProviderClientProfile, ProviderKind, ProviderQuirks};
use crate::ir::{Dialect, Message};

use super::catalogue::{extract_model_list, pricing_from_catalogue};
use super::errors::truncate;
use super::headers::{CLAUDE_CODE_SYSTEM_IDENTITY, CLAUDE_CODE_USER_AGENT};
use super::*;

fn provider(kind: ProviderKind) -> ProviderConfig {
    let mut p = ProviderConfig::new("p", "Test Provider", kind);
    p.timeout_secs = 5;
    p
}

#[test]
fn anthropic_headers_use_x_api_key() {
    let mut p = provider(ProviderKind::Anthropic);
    p.impersonate_claude_code = false; // Disable impersonation for this test
                                       // The legacy switch no longer decides anything on its own: an Anthropic
                                       // provider is on the Claude Code profile by default, so the fingerprint
                                       // has to be turned off through the profile to reach the beta value the
                                       // caller configured. `the_claude_code_profile_merges_with_an_anthropic_beta_from_extra_headers`
                                       // covers the other side of this.
    p.client_profile = ProviderClientProfile::Native;
    p.extra_headers
        .insert("anthropic-beta".into(), "output-128k".into());
    let h = build_headers(&p, Some("sk-ant-1")).unwrap();
    assert_eq!(h.get("x-api-key").unwrap(), "sk-ant-1");
    assert_eq!(
        h.get("anthropic-version").unwrap(),
        DEFAULT_ANTHROPIC_VERSION
    );
    assert_eq!(h.get("anthropic-beta").unwrap(), "output-128k");
    assert!(h.get(AUTHORIZATION).is_none());
}

#[test]
fn bearer_auth_sends_both_headers_for_anthropic_relays() {
    let mut p = provider(ProviderKind::Anthropic);
    p.client_profile = ProviderClientProfile::Native;
    p.bearer_auth = true;
    let h = build_headers(&p, Some("sk-relay")).unwrap();
    // Both headers: the relay reads Authorization, the standard API still
    // finds its x-api-key.
    assert_eq!(h.get("x-api-key").unwrap(), "sk-relay");
    assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer sk-relay");
}

#[test]
fn bearer_auth_does_not_apply_to_openai_providers() {
    let mut p = provider(ProviderKind::OpenAICompatible);
    p.bearer_auth = true;
    let h = build_headers(&p, Some("sk-1")).unwrap();
    assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer sk-1");
    // x-api-key would be redundant and unusual on the OpenAI dialect.
    assert!(h.get("x-api-key").is_none());
}

#[test]
fn openai_headers_use_bearer_and_no_version() {
    let h = build_headers(&provider(ProviderKind::OpenAICompatible), Some("sk-1")).unwrap();
    assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer sk-1");
    assert!(h.get("anthropic-version").is_none());
}

#[test]
fn missing_key_means_no_auth_header() {
    let h = build_headers(&provider(ProviderKind::OpenAICompatible), None).unwrap();
    assert!(h.get(AUTHORIZATION).is_none());
    let h = build_headers(&provider(ProviderKind::OpenAICompatible), Some("")).unwrap();
    assert!(h.get(AUTHORIZATION).is_none());
}

#[test]
fn custom_version_and_invalid_header_names() {
    let mut p = provider(ProviderKind::Anthropic);
    p.anthropic_version = Some("2024-10-22".into());
    assert_eq!(
        build_headers(&p, None)
            .unwrap()
            .get("anthropic-version")
            .unwrap(),
        "2024-10-22"
    );
    p.extra_headers.insert("bad header".into(), "v".into());
    assert!(build_headers(&p, None).is_err());
}

/// A provider built by `ProviderConfig::new` sends the Claude Code
/// fingerprint, and did not before the deprecated switch was migrated.
///
/// This is a behaviour change, not a refactor, and it is asserted here so it
/// cannot be reverted by accident. `ProviderConfig::new` has defaulted an
/// Anthropic provider to `ProviderClientProfile::ClaudeCode` since the profile
/// field landed, with a comment saying so; the header path keyed on
/// `impersonate_claude_code` instead, so the default was inert and every
/// freshly created Anthropic provider sent a native fingerprint. Migrating the
/// call site is what makes the documented default real.
///
/// What must NOT change with it is asserted below rather than assumed, because
/// a migration that turned "native by default" into "Claude Code everywhere"
/// would be a different and much worse change than the one intended.
#[test]
fn a_new_anthropic_provider_sends_the_claude_code_fingerprint() {
    let p = provider(ProviderKind::Anthropic);
    let h = build_headers(&p, Some("sk-ant-1")).unwrap();
    assert_eq!(
        h.get("user-agent").unwrap(),
        CLAUDE_CODE_USER_AGENT,
        "the documented default profile is not reaching the header path"
    );
    assert_eq!(h.get("x-app").unwrap(), "cli");
}

/// The opt-out still works, and still means native.
#[test]
fn an_explicit_native_profile_sends_no_fingerprint() {
    let mut p = provider(ProviderKind::Anthropic);
    p.client_profile = ProviderClientProfile::Native;
    let h = build_headers(&p, Some("sk-ant-1")).unwrap();
    assert!(h.get("x-app").is_none(), "Native must inject nothing");
    assert_ne!(
        h.get("user-agent").map(|v| v.as_bytes()),
        Some(CLAUDE_CODE_USER_AGENT.as_bytes()),
        "Native must not send the Claude Code user agent"
    );
}

/// A provider that never opted in keeps sending no fingerprint.
///
/// A document written before the profile field existed deserialises
/// `client_profile` as its default, `Auto`, which is not `ClaudeCode`. That is
/// the case this assertion protects: an existing installation must not start
/// impersonating because its configuration was merely re-read.
#[test]
fn an_auto_profile_sends_no_fingerprint() {
    let mut p = provider(ProviderKind::Anthropic);
    p.client_profile = ProviderClientProfile::Auto;
    let h = build_headers(&p, Some("sk-ant-1")).unwrap();
    assert!(h.get("x-app").is_none(), "Auto must inject nothing here");
}

#[test]
fn client_profile_claude_code_injects_clean_fingerprint() {
    let mut p = provider(ProviderKind::Anthropic);
    p.client_profile = ProviderClientProfile::ClaudeCode;
    let h = build_headers(&p, Some("sk-ant-1")).unwrap();

    // User-Agent must be claude-cli style.
    assert_eq!(h.get("user-agent").unwrap(), CLAUDE_CODE_USER_AGENT);

    // Core identity headers.
    assert_eq!(h.get("x-app").unwrap(), "cli");
    assert_eq!(
        h.get("anthropic-dangerous-direct-browser-access").unwrap(),
        "true"
    );

    // SDK-layer headers a real CLI never sends must stay absent.
    assert!(h.get("x-stainless-arch").is_none());
    assert!(h.get("x-stainless-package-version").is_none());
    assert!(h.get("x-claude-code-session-id").is_none());
    assert!(h.get("x-client-request-id").is_none());

    // anthropic-beta must contain claude-code marker
    let beta = h.get("anthropic-beta").unwrap().to_str().unwrap();
    assert!(beta.contains("claude-code-20250219"));

    // Auth header
    assert!(h.get("x-api-key").is_some());
}

#[test]
fn client_profile_claude_code_merges_existing_anthropic_beta() {
    let mut p = provider(ProviderKind::Anthropic);
    p.client_profile = ProviderClientProfile::ClaudeCode;
    p.extra_headers
        .insert("anthropic-beta".into(), "output-128k".into());
    let h = build_headers(&p, Some("sk-ant-1")).unwrap();
    let beta = h.get("anthropic-beta").unwrap().to_str().unwrap();
    assert!(beta.contains("claude-code-20250219"));
    assert!(beta.contains("output-128k"));
}

#[test]
fn encode_for_applies_the_model_output_cap() {
    let mut req = ChatRequest::new("sonnet-class", Dialect::Anthropic);
    req.messages.push(Message::user_text("hi"));
    req.max_tokens = Some(64_000);

    let p = provider(ProviderKind::OpenAICompatible);
    let body = encode_for(&p, &req, "deepseek-v4-pro", Some(8192)).unwrap();
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["model"], "deepseek-v4-pro");

    // No cap configured: the client value survives.
    let body = encode_for(&p, &req, "deepseek-v4-pro", None).unwrap();
    assert_eq!(body["max_tokens"], 64_000);
}

#[test]
fn encode_for_respects_provider_quirks() {
    let mut req = ChatRequest::new("m", Dialect::OpenAI);
    req.messages.push(Message::user_text("hi"));
    req.max_tokens = Some(100);
    let mut p = provider(ProviderKind::OpenAICompatible);
    p.quirks = ProviderQuirks {
        use_max_completion_tokens: true,
        ..ProviderQuirks::default()
    };
    let body = encode_for(&p, &req, "gpt-5.3-sol", None).unwrap();
    assert_eq!(body["max_completion_tokens"], 100);
}

#[test]
fn impersonation_injects_identity_even_without_a_system_prompt() {
    let mut req = ChatRequest::new("m", Dialect::Anthropic);
    req.messages.push(Message::user_text("hi"));
    let mut p = provider(ProviderKind::Anthropic);
    p.client_profile = ProviderClientProfile::ClaudeCode;
    let body = encode_for(&p, &req, "claude-sonnet-4-5", None).unwrap();
    let system = body["system"].as_array().unwrap();
    assert_eq!(system[0]["text"], CLAUDE_CODE_SYSTEM_IDENTITY);
}

#[test]
fn classifier_mode_keeps_stop_and_temperature_despite_quirks() {
    // A provider configured to drop the fields a classifier depends on:
    // in classifier mode the verdict protocol wins over compatibility.
    let mut req = ChatRequest::new("claude-opus-4-8[1m]", Dialect::Anthropic);
    req.messages.push(Message::user_text("transcript"));
    req.max_tokens = Some(64);
    req.temperature = Some(0.0);
    req.stop_sequences = vec!["</block>".into()];

    let mut p = provider(ProviderKind::OpenAICompatible);
    p.quirks = ProviderQuirks {
        drop_stop: true,
        drop_temperature: true,
        drop_top_p: true,
        ..ProviderQuirks::default()
    };

    let body = encode_for_mode(&p, &req, "glm-5.3", None, EncodeMode::Classifier).unwrap();
    // The array-ness is the contract: `"stop": ["</block>"]`, not a joined
    // string and not dropped.
    assert_eq!(
        body["stop"],
        serde_json::json!(["</block>"]),
        "classifier stop sequences must survive as an array"
    );
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["model"], "glm-5.3");

    // Normal mode keeps honouring the quirks.
    let body = encode_for_mode(&p, &req, "glm-5.3", None, EncodeMode::Normal).unwrap();
    assert!(body.get("stop").is_none());
    assert!(body.get("temperature").is_none());
}

#[test]
fn truncate_keeps_utf8_intact() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("中文测试内容", 3), "中文测…");
}

#[test]
fn catalogue_prices_become_per_million_tokens() {
    // OpenRouter publishes dollars per single token, as strings.
    let entry = serde_json::json!({
        "id": "deepseek/deepseek-chat",
        "pricing": {
            "prompt": "0.00000027",
            "completion": "0.0000011",
            "input_cache_read": "0.00000007"
        }
    });
    let pricing = pricing_from_catalogue(&entry).unwrap();
    assert_eq!(pricing.currency, "USD");
    assert!((pricing.input_per_mtok - 0.27).abs() < 1e-9);
    assert!((pricing.output_per_mtok - 1.10).abs() < 1e-9);
    assert!((pricing.cache_read_per_mtok.unwrap() - 0.07).abs() < 1e-9);
    assert!(pricing.cache_write_per_mtok.is_none());
}

#[test]
fn catalogues_without_usable_prices_are_left_alone() {
    for entry in [
        serde_json::json!({"id": "m"}),
        serde_json::json!({"id": "m", "pricing": {}}),
        serde_json::json!({"id": "m", "pricing": {"prompt": "0", "completion": "0"}}),
        serde_json::json!({"id": "m", "pricing": {"prompt": "abc", "completion": "1"}}),
    ] {
        assert!(pricing_from_catalogue(&entry).is_none(), "{entry}");
    }
}

// -- model discovery normalization tests --

#[test]
fn standard_openai_model_list() {
    let json = serde_json::json!({
        "object": "list",
        "data": [
            {"id": "gpt-4", "object": "model"},
            {"id": "gpt-3.5-turbo", "object": "model"}
        ]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 2);
    // extract_model_list preserves input order; sorting happens in list_models.
    assert_eq!(models[0].id, "gpt-4");
    assert_eq!(models[1].id, "gpt-3.5-turbo");
}

#[test]
fn mimo_style_models_key() {
    // MiMo and some other providers use "models" instead of "data".
    let json = serde_json::json!({
        "models": [
            {"id": "MiMo-7B-RL"},
            {"id": "MiMo-7B-SFT"}
        ]
    });
    let models = extract_model_list(&json, "Xiaomi MiMo");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "MiMo-7B-RL");
    assert_eq!(models[1].id, "MiMo-7B-SFT");
}

#[test]
fn mimo_style_model_id_field() {
    // Some providers use "model" or "model_id" instead of "id".
    let json = serde_json::json!({
        "data": [
            {"model": "MiMo-7B-RL"},
            {"model_id": "MiMo-7B-SFT"},
            {"name": "MiMo-7B-Chat"}
        ]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 3);
    assert!(models.iter().any(|m| m.id == "MiMo-7B-RL"));
    assert!(models.iter().any(|m| m.id == "MiMo-7B-SFT"));
    assert!(models.iter().any(|m| m.id == "MiMo-7B-Chat"));
}

#[test]
fn bare_array_model_list() {
    let json = serde_json::json!([
        {"id": "model-a"},
        {"id": "model-b"}
    ]);
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "model-a");
    assert_eq!(models[1].id, "model-b");
}

#[test]
fn empty_data_array_returns_empty() {
    let json = serde_json::json!({"data": []});
    let models = extract_model_list(&json, "test");
    assert!(models.is_empty());
}

#[test]
fn unrecognized_format_returns_empty_with_diagnostic() {
    // A response that has no recognizable model list at all.
    let json = serde_json::json!({"error": "not found"});
    let models = extract_model_list(&json, "test");
    assert!(models.is_empty());
}

#[test]
fn model_id_whitespace_is_trimmed() {
    let json = serde_json::json!({
        "data": [
            {"id": "  model-a  "},
            {"id": "model-b"}
        ]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "model-a");
    assert_eq!(models[1].id, "model-b");
}

#[test]
fn entries_without_any_id_field_are_skipped() {
    let json = serde_json::json!({
        "data": [
            {"id": "model-a"},
            {"object": "model"},
            {"id": "model-b"}
        ]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "model-a");
    assert_eq!(models[1].id, "model-b");
}

#[test]
fn empty_id_strings_are_skipped() {
    let json = serde_json::json!({
        "data": [
            {"id": ""},
            {"id": "  "},
            {"id": "model-a"}
        ]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "model-a");
}

#[test]
fn standard_format_preserves_pricing() {
    let json = serde_json::json!({
        "data": [{
            "id": "m-priced",
            "pricing": {"prompt": "0.0000005", "completion": "0.000002"}
        }]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 1);
    let pricing = models[0].pricing.as_ref().unwrap();
    assert_eq!(pricing.currency, "USD");
    assert!((pricing.input_per_mtok - 0.5).abs() < 1e-9);
    assert!((pricing.output_per_mtok - 2.0).abs() < 1e-9);
}

#[test]
fn non_standard_key_preserves_pricing() {
    let json = serde_json::json!({
        "models": [{
            "id": "m-priced",
            "pricing": {"prompt": "0.0000005", "completion": "0.000002"}
        }]
    });
    let models = extract_model_list(&json, "test");
    assert_eq!(models.len(), 1);
    let pricing = models[0].pricing.as_ref().unwrap();
    assert_eq!(pricing.currency, "USD");
}
