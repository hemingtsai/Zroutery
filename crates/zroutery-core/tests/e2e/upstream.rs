use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::json;

use zroutery_core::billing::{BalanceConfig, BalancePreset, BalanceProbe};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

use super::support::*;

#[tokio::test]
async fn a_balance_is_fetched_with_the_providers_own_key() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // The built-in presets point at real vendors, so the mock is driven by a
    // custom probe; the presets themselves are covered by unit tests.
    cfg.providers[0].balance = BalanceConfig {
        preset: BalancePreset::Custom,
        custom: Some(BalanceProbe {
            path: "/user/balance".into(),
            remaining_pointer: Some("/balance_infos/0/total_balance".into()),
            currency_pointer: Some("/balance_infos/0/currency".into()),
            ..BalanceProbe::default()
        }),
    };
    let provider = cfg.providers[0].clone();
    let h = Harness::start(cfg, mock).await;

    let balance = h
        .state
        .upstream()
        .fetch_balance(
            &provider,
            Some("sk-deepseek"),
            &provider.balance.probe(provider.base_depth()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(balance.currency, "CNY");
    assert_eq!(balance.remaining, Some(48.75));

    // A provider that publishes nothing is not asked at all.
    let quiet = &h.state.config().providers[1];
    assert!(!quiet.balance.is_supported(quiet.base_depth()));

    // A pointer into thin air is an error rather than a silent zero.
    let mut broken = provider.clone();
    broken.balance.custom = Some(BalanceProbe {
        path: "/user/balance".into(),
        remaining_pointer: Some("/nope".into()),
        ..BalanceProbe::default()
    });
    let err = h
        .state
        .upstream()
        .fetch_balance(
            &broken,
            Some("sk-deepseek"),
            &broken.balance.probe(broken.base_depth()).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no balance found"), "{err}");

    h.shutdown().await;
}

#[tokio::test]
async fn the_sub2api_preset_reads_a_relay_of_either_dialect() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // A relay reached through the OpenAI dialect: its base already ends in /v1,
    // so the probe asks for `/usage`.
    cfg.providers[0].balance = BalanceConfig {
        preset: BalancePreset::Sub2Api,
        custom: None,
    };
    // The same relay reached as Anthropic: the base is the API root, so the probe
    // has to ask for `/v1/usage` instead.
    cfg.providers[2].balance = BalanceConfig {
        preset: BalancePreset::Sub2Api,
        custom: None,
    };
    let openai_style = cfg.providers[0].clone();
    let anthropic_style = cfg.providers[2].clone();
    let h = Harness::start(cfg, mock).await;

    for provider in [&openai_style, &anthropic_style] {
        let key = if provider.kind == ProviderKind::Anthropic {
            "sk-ant"
        } else {
            "sk-deepseek"
        };
        let probe = provider.balance.probe(provider.base_depth()).unwrap();
        let balance = h
            .state
            .upstream()
            .fetch_balance(provider, Some(key), &probe)
            .await
            .unwrap();
        // The relay reports the key's quota, not just a wallet total.
        assert_eq!(balance.currency, "USD", "{}", provider.name);
        assert_eq!(balance.remaining, Some(7.25));
        assert_eq!(balance.total, Some(20.0));
        assert_eq!(balance.used, Some(12.75));

        // Sub2API accepts either credential header, so each dialect sending its
        // own is enough; this is what actually goes on the wire.
        let headers = zroutery_core::upstream::build_headers(provider, Some(key)).unwrap();
        match provider.kind {
            ProviderKind::Anthropic => assert_eq!(headers["x-api-key"], key),
            ProviderKind::OpenAICompatible => {
                assert_eq!(headers["authorization"], format!("Bearer {key}"))
            }
            // `config_for` only builds the two dialects above, so this arm cannot
            // be reached today. It is written out rather than folded into a
            // wildcard so that adding a provider kind here fails this test
            // instead of silently leaving the Sub2API preset claim about
            // credential headers unverified for that dialect. Both OpenCode kinds
            // authenticate with an OAuth token rather than an API key, which is a
            // different question from the one this test asks.
            other => panic!("the Sub2API preset test does not cover {other:?}"),
        }
    }

    h.shutdown().await;
}

#[tokio::test]
async fn provider_model_discovery_dedupes_and_sorts() {
    let (addr, mock) = start_mock().await;
    let cfg = config_for(addr);
    let provider = cfg.providers[0].clone();
    let h = Harness::start(cfg, mock).await;

    let models = h
        .state
        .upstream()
        .list_models(&provider, Some("sk-deepseek"))
        .await
        .unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m-a", "m-b", "m-priced"]);
    // Prices come along when the catalogue publishes them, per million tokens.
    assert!(models[0].pricing.is_none());
    let priced = models[2].pricing.as_ref().unwrap();
    assert_eq!(priced.currency, "USD");
    assert!((priced.input_per_mtok - 0.5).abs() < 1e-9);
    assert!((priced.output_per_mtok - 2.0).abs() < 1e-9);

    h.shutdown().await;
}

#[tokio::test]
async fn transport_error_returns_502() {
    // Nothing listens on port 1 — the connection will be refused or time out.
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();

    let mut provider = ProviderConfig::new("dead", "DeadProvider", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{addr}");
    provider.key_ref = "provider:dead".into();
    provider.timeout_secs = 2;
    cfg.providers = vec![provider];
    cfg.models = vec![ModelEntry::for_upstream(
        "dead",
        "dead-model",
        Some(ModelTier::Standard),
    )];

    let _mock = Mock::default();
    let secrets = Arc::new(MemorySecretStore::new().with("provider:dead", "sk-dead"));
    let state = Arc::new(AppState::new(cfg, secrets));
    let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
    let base = format!("http://{}", server.addr);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("x-api-key", TOKEN)
        .json(&json!({"model": "sonnet-class", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    // Should get a server error (502/503), not a panic or hang.
    assert!(
        resp.status().is_server_error(),
        "expected 5xx, got {}",
        resp.status()
    );

    server.stop().await;
}
