use serde_json::{json, Value};

use zroutery_core::config::{ModelEntry, ModelTier, ProviderConfig, ProviderKind};

use super::support::*;

#[tokio::test]
async fn an_opencode_rate_limit_skips_every_model_of_the_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    let mut zen = ProviderConfig::new("zen", "OpenCode Zen", ProviderKind::OpenCodeZenFree);
    zen.base_url = format!("http://{addr}");
    cfg.providers = vec![zen];
    // One model answers 429; the sibling would answer normally. Because the
    // free tier meters by account/IP, the 429 must also take the sibling out
    // for the cooldown instead of failing over into the same exhausted pool.
    cfg.models = vec![
        ModelEntry::for_upstream("zen", "limited-model", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("zen", "ok-model", Some(ModelTier::Standard)),
    ];
    let h = Harness::start(cfg, mock).await;

    let response = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    // The client sees the upstream's rate limit, not a generic "no model":
    // the pool was skipped because of the 429, and that is what was left in
    // `last_error`.
    assert_eq!(response.status(), 429);
    let sent = h.mock.bodies();
    assert_eq!(
        sent.len(),
        1,
        "the sibling was tried after the provider was rate limited: {sent:?}"
    );
    assert_eq!(sent[0]["model"], "limited-model");

    h.shutdown().await;
}

#[tokio::test]
async fn failover_moves_to_the_next_model_in_the_class() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Two sonnet candidates: the preferred one always fails.
    cfg.models[1].upstream_model = "broken-model".into();
    cfg.models[1].priority = 0;
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from mock");

    assert_eq!(h.mock.count(), 2, "the broken model was tried first");
    let health = h.state.router().health_snapshot();
    let broken = health
        .iter()
        .find(|m| m.model_id == "deepseek-broken-model")
        .unwrap();
    assert_eq!(broken.total_failure, 1);
    let good = health
        .iter()
        .find(|m| m.model_id == "openai-gpt-sonnet")
        .unwrap();
    assert_eq!(good.total_success, 1);

    let record = &h.state.stats().recent(1)[0];
    assert_eq!(record.attempts, 2);
    assert_eq!(record.resolved_model.as_deref(), Some("openai-gpt-sonnet"));

    h.shutdown().await;
}

/// A 400 from an upstream is provider evidence about the shape this gateway
/// translated, not a statement that the client's content is unusable: another
/// candidate may accept it, so the request moves on instead of ending at the
/// first refusal. When every candidate refuses, the client still gets the
/// refusal — failover never invents an answer.
#[tokio::test]
async fn an_upstream_rejection_fails_over_then_fails_closed() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "refuse-model".into();
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");
    assert_eq!(
        h.mock.count(),
        2,
        "the refusal fails over to the next candidate, it is not replayed"
    );
    h.shutdown().await;

    // Every candidate refuses the translated shape.
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "refuse-model".into();
    cfg.models.push(
        ModelEntry::for_upstream("openai", "refuse-gpt", Some(ModelTier::Standard))
            .with_priority(10),
    );
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(
        h.mock.count(),
        2,
        "each candidate is tried once; a refusal is never replayed"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn an_empty_wallet_fails_over_and_is_then_gated() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "broke-model".into();
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    let h = Harness::start(cfg, mock).await;

    let ask = || {
        h.post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                          "messages": [{"role": "user", "content": "hi"}]}))
    };

    // The empty wallet is evidence about the provider, not the client: the
    // request is served by the funded candidate instead of failing with 402.
    let resp = ask().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");
    assert_eq!(h.mock.count(), 2, "the empty provider was tried once");

    // One 402 gates the whole provider. The next request must not spend an
    // attempt on it again.
    let resp = ask().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        h.mock.count(),
        3,
        "the gated provider is skipped, not retried"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn circuit_breaker_skips_a_failing_model() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "broken-model".into();
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    cfg.routing.circuit_breaker.failure_threshold = 1;
    let h = Harness::start(cfg, mock).await;

    for _ in 0..2 {
        let status = h
            .post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200);
    }

    // First call: broken + good = 2 upstream calls. Second call: the breaker is
    // open, so the broken model is demoted and only the good one is used.
    assert_eq!(h.mock.count(), 3);
    assert!(h.state.router().is_cooling("deepseek-broken-model"));

    h.shutdown().await;
}

#[tokio::test]
async fn missing_api_key_fails_over_and_reports_clearly() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.providers[0].key_ref = "provider:nonexistent".into();
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 412);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no API key"));
    assert_eq!(h.mock.count(), 0);

    h.shutdown().await;
}

#[tokio::test]
async fn rate_limit_triggers_failover() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Priority is explicit: equal priorities order by exposed id, which would
    // put the healthy model first and skip the 429 path this test covers.
    cfg.models = vec![
        ModelEntry::for_upstream("deepseek", "limited-v4", Some(ModelTier::Standard))
            .with_priority(0),
        ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard))
            .with_priority(10),
    ];
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "sonnet-class", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    // Should succeed via failover to the second model.
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );
    // Two upstream calls: one 429, one success.
    assert_eq!(h.mock.bodies()[0]["model"], "limited-v4");
    assert_eq!(h.mock.bodies()[1]["model"], "deepseek-v4-pro");
    assert_eq!(h.mock.count(), 2);

    h.shutdown().await;
}
