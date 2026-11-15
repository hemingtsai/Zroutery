use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::json;

use zroutery_core::config::{AppConfig, MemorySecretStore};
use zroutery_core::server::AppState;

use super::support::*;

fn account_keys_cfg(addr: SocketAddr) -> (AppConfig, Arc<MemorySecretStore>) {
    use zroutery_core::config::{AccountConfig, InferenceKeyConfig};
    let mut cfg = config_for(addr);
    cfg.routing.max_attempts = 3;
    cfg.routing.circuit_breaker.failure_threshold = 1;
    cfg.providers[0].accounts = vec![
        AccountConfig {
            account_id: "a".into(),
            key_ref: "panel-a".into(),
            enabled: true,
            inference_keys: vec![
                InferenceKeyConfig {
                    id: "first".into(),
                    key_ref: "key-a1".into(),
                    priority: 0,
                    enabled: true,
                    note: String::new(),
                },
                InferenceKeyConfig {
                    id: "sibling".into(),
                    key_ref: "key-a2".into(),
                    priority: 1,
                    enabled: true,
                    note: String::new(),
                },
            ],
            ..Default::default()
        },
        AccountConfig {
            account_id: "b".into(),
            key_ref: "panel-b".into(),
            enabled: true,
            inference_keys: vec![InferenceKeyConfig {
                id: "backup".into(),
                key_ref: "key-b".into(),
                priority: 2,
                enabled: true,
                note: String::new(),
            }],
            ..Default::default()
        },
    ];
    let mut keys = secrets();
    let store = Arc::get_mut(&mut keys).unwrap();
    store.insert("panel-a", "private-personal-token");
    store.insert("panel-b", "another-personal-token");
    store.insert("key-a1", "sk-empty-wallet");
    store.insert("key-a2", "sk-same-empty-wallet");
    store.insert("key-b", "sk-funded-wallet");
    (cfg, keys)
}

#[tokio::test]
async fn account_wallet_failure_skips_sibling_keys_but_not_other_accounts() {
    for streaming in [false, true] {
        let (addr, mock) = start_mock().await;
        let (mut cfg, keys) = account_keys_cfg(addr);
        cfg.providers[0].accounts[0]
            .inference_keys
            .extend(
                (0..5).map(|index| zroutery_core::config::InferenceKeyConfig {
                    id: format!("extra-{index}"),
                    key_ref: format!("empty-{index}"),
                    priority: 1,
                    enabled: true,
                    note: String::new(),
                }),
            );
        let h = Harness::from_state(Arc::new(AppState::new(cfg, keys)), mock).await;
        for _ in 0..2 {
            let response = h
                .post("/v1/chat/completions")
                .json(&json!({
                    "model":"standard-class", "stream":streaming,
                    "messages":[{"role":"user","content":"hi"}]
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let _ = response.text().await.unwrap();
        }
        assert_eq!(
            h.mock.keys(),
            [
                "Bearer sk-empty-wallet",
                "Bearer sk-funded-wallet",
                "Bearer sk-funded-wallet"
            ]
        );
        assert!(h.state.router().is_account_gated("deepseek", "a"));
        assert!(!h.state.router().is_provider_gated("deepseek"));
        assert!(!h.state.router().is_circuit_open("deepseek-deepseek-v4-pro"));
        let record = &h.state.stats().recent(1)[0];
        assert_eq!(record.account_id.as_deref(), Some("b"));
        assert_eq!(record.inference_key_id.as_deref(), Some("backup"));
        h.state.set_config((*h.state.config()).clone());
        assert!(
            h.state.router().is_account_gated("deepseek", "a"),
            "a config save preserves cooldown"
        );
        h.shutdown().await;
    }
}

#[tokio::test]
async fn invalid_inference_key_tries_the_same_accounts_next_key() {
    let (addr, mock) = start_mock().await;
    let (cfg, mut keys) = account_keys_cfg(addr);
    Arc::get_mut(&mut keys)
        .unwrap()
        .insert("key-a1", "sk-invalid-inference");
    let h = Harness::from_state(Arc::new(AppState::new(cfg, keys)), mock).await;
    let response = h
        .post("/v1/chat/completions")
        .json(&json!({"model":"standard-class", "messages":[{"role":"user","content":"hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        h.mock.keys(),
        ["Bearer sk-invalid-inference", "Bearer sk-same-empty-wallet"]
    );
    assert!(h.state.router().is_key_gated("deepseek", "a", "first"));
    assert!(!h.state.router().is_account_gated("deepseek", "a"));
    h.shutdown().await;
}

#[tokio::test]
async fn inference_keys_are_sent_in_priority_order_not_id_or_declaration_order() {
    let (addr, mock) = start_mock().await;
    let (mut cfg, mut keys) = account_keys_cfg(addr);
    let account = &mut cfg.providers[0].accounts[0];
    account.inference_keys[0].priority = 20;
    account.inference_keys[1].priority = 0;
    Arc::get_mut(&mut keys)
        .unwrap()
        .insert("key-a1", "sk-invalid-inference");
    Arc::get_mut(&mut keys)
        .unwrap()
        .insert("key-a2", "sk-healthy-priority-first");

    let harness = Harness::from_state(Arc::new(AppState::new(cfg, keys)), mock).await;
    let response = harness
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "standard-class",
            "messages": [{"role": "user", "content": "local fixture"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(
        harness.mock.keys(),
        ["Bearer sk-healthy-priority-first"],
        "priority 0 must be sent before priority 20 even though its key id sorts later"
    );
    let record = &harness.state.stats().recent(1)[0];
    assert_eq!(record.inference_key_id.as_deref(), Some("sibling"));
    harness.shutdown().await;
}

#[tokio::test]
async fn disabled_inference_pool_never_borrows_the_provider_or_panel_key() {
    let (addr, mock) = start_mock().await;
    let (mut cfg, keys) = account_keys_cfg(addr);
    for account in &mut cfg.providers[0].accounts {
        account.enabled = false;
    }
    let h = Harness::from_state(Arc::new(AppState::new(cfg, keys)), mock).await;
    let response = h
        .post("/v1/chat/completions")
        .json(&json!({"model":"standard-class", "messages":[{"role":"user","content":"hi"}]}))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(h.mock.count(), 0);
    h.shutdown().await;
}

#[cfg(feature = "newapi")]
#[tokio::test]
async fn an_unreachable_account_panel_does_not_block_main_routing() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    let provider = &mut cfg.providers[0];
    provider.timeout_secs = 1;
    let mut account = zroutery_core::config::AccountConfig {
        account_id: "panel-account".into(),
        key_ref: "panel-token".into(),
        enabled: true,
        ..Default::default()
    };
    account.maintenance.panel_url = Some("http://127.0.0.1:1".into());
    account.maintenance.new_api_user = Some("354394".into());
    provider.accounts.push(account);
    let mut keys = secrets();
    Arc::get_mut(&mut keys)
        .unwrap()
        .insert("panel-token", "personal-panel-token");
    let state = Arc::new(AppState::new(cfg, keys));
    let h = Harness::from_state(state, mock).await;
    let report = h.state.reconcile_provider_accounts("deepseek").await;
    assert_eq!(report.outcomes.len(), 1);
    let response = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "standard-class", "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        h.mock.count(),
        1,
        "the inference endpoint is separate from the failed panel"
    );
    assert!(!h.state.router().is_provider_gated("deepseek"));
    h.shutdown().await;
}
