use serde_json::{json, Value};

use zroutery_core::billing::Pricing;
use zroutery_core::config::{ModelEntry, ModelTier, RoutingStrategy};

use super::support::*;

#[tokio::test]
async fn classifier_requests_route_to_the_classifier_pool() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.classifier = zroutery_core::config::ClassifierConfig {
        enabled: true,
        candidates: vec![zroutery_core::config::ClassifierCandidate {
            model: "deepseek-deepseek-v4-flash".into(),
            priority: 10,
            enabled: true,
        }],
        ..zroutery_core::config::ClassifierConfig::default()
    };
    let h = Harness::start(cfg, mock).await;

    // An Auto Mode stage-1 shaped request, asking for a model the registry
    // does not even have: the classifier pool must answer regardless.
    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "claude-opus-4-8[1m]",
            "max_tokens": 64,
            "temperature": 0,
            "stop_sequences": ["</block>"],
            "system": [{"type": "text", "text":
                "You are a security monitor for autonomous AI coding agents."}],
            "messages": [{"role": "user", "content": "should this run?"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );
    assert_eq!(resp.headers()["x-zroutery-classifier"], "1");
    // The candidate's own model name went upstream.
    assert_eq!(h.mock.bodies()[0]["model"], "deepseek-v4-flash");
    // And it was recorded as classifier traffic, not main traffic.
    let kind = &h.state.stats().summary().per_kind;
    assert!(kind
        .iter()
        .any(|k| k.kind == "auto_mode" && k.requests == 1));

    h.shutdown().await;
}

#[tokio::test]
async fn codex_guardian_risk_and_review_keep_their_native_contracts() {
    for (model, instructions, answer, kind) in [
        ("guardian-risk", "You are acting as a first-step asynchronous security reviewer. Your first output token is high or low.", "low", "codex_guardian_risk"),
        ("guardian-review", "You are judging one planned coding-agent action. Derive outcome from risk_level and user_authorization. Return allow or deny.", "{\"outcome\":\"allow\"}", "codex_guardian"),
    ] {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.models[1].upstream_model = model.into();
        cfg.classifier.enabled = true;
        cfg.classifier.candidates = vec![zroutery_core::config::ClassifierCandidate {
            model: format!("deepseek-{model}"), priority: 0, enabled: true,
        }];
        let h = Harness::start(cfg, mock).await;
        for streaming in [false, true] {
            let response = h.post("/v1/responses").json(&json!({
                "model":"client-review-alias-not-in-registry", "stream":streaming,
                "instructions":instructions, "input":[{"role":"user", "content":"Review this planned file read."}]
            })).send().await.unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            assert_eq!(status, 200, "{text}");
            if streaming {
                assert!(text.contains("response.completed"), "{text}");
                assert!(!text.contains("<block>"), "{text}");
                let completed = text.lines().filter_map(|line| line.strip_prefix("data: "))
                    .filter_map(|data| serde_json::from_str::<Value>(data).ok())
                    .find(|event| event["type"] == "response.completed").unwrap();
                assert_eq!(completed["response"]["output"][0]["content"][0]["text"], answer);
            } else {
                let body: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(body["output"][0]["content"][0]["text"], answer);
            }
        }
        assert_eq!(h.mock.count(), 2);
        assert!(h.mock.bodies().iter().all(|body| body["model"] == model));
        assert!(h.state.stats().recent(2).iter().all(|record| record.kind.as_str() == kind && record.ok));
        h.shutdown().await;
    }
}

#[tokio::test]
async fn an_election_pins_the_cheap_fast_model_as_primary() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.routing.strategy = RoutingStrategy::Balanced;
    // Two sonnet members priced ten to one, both answered by the same mock at the
    // same speed, so price is what has to decide.
    cfg.models[1].pricing = Some(Pricing::new("USD", 0.2, 0.8));
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            // Priority puts this one first; the election is expected to overrule it.
            .with_priority(-100),
    );
    let last = cfg.models.len() - 1;
    cfg.models[last].pricing = Some(Pricing::new("USD", 2.0, 8.0));
    let h = Harness::start(cfg, mock).await;

    // Before any election the configured priority still rules.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");

    let election = h.state.hold_election().await;
    let sonnet = election.tiers.get(&ModelTier::Standard).unwrap();
    assert!(sonnet.priced, "both members are priced in one currency");
    assert_eq!(sonnet.winner(), Some("deepseek-deepseek-v4-pro"));
    assert!(sonnet.ranked[0].latency_ms.is_some());
    assert!(sonnet.ranked[0].note.as_ref().unwrap().contains("primary"));

    // Traffic follows the election from here on, not the priority.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );

    // A probe is a real call, so it doubles as the freshest health signal.
    let health = h.state.router().health_snapshot();
    assert!(health
        .iter()
        .any(|m| m.model_id == "openai-gpt-sonnet" && m.total_success > 0));

    h.shutdown().await;
}

#[tokio::test]
async fn an_election_ranks_a_broken_model_last_and_says_why() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.routing.strategy = RoutingStrategy::Balanced;
    // A second opus member that always fails; the election has to notice.
    cfg.models[3].upstream_model = "broken-model".into();
    cfg.models[3].tier = Some(ModelTier::Reasoning);
    let h = Harness::start(cfg, mock).await;

    let election = h.state.hold_election().await;
    let opus = election.tiers.get(&ModelTier::Reasoning).unwrap();
    assert_eq!(opus.winner(), Some("openai-gpt-5.3-sol"));
    let last = opus.ranked.last().unwrap();
    assert_eq!(last.model_id, "anthropic-broken-model");
    assert!(last.score.is_none());
    assert!(last.note.as_ref().unwrap().contains("did not answer"));

    // Neither is priced, so latency decided and the reason is on the record.
    assert!(!opus.priced);
    assert!(opus
        .note
        .as_ref()
        .unwrap()
        .contains("not every model has a price"));

    h.shutdown().await;
}

#[tokio::test]
async fn a_model_added_after_an_election_is_used_but_not_promoted() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.routing.strategy = RoutingStrategy::Balanced;
    let h = Harness::start(cfg, mock).await;

    let election = h.state.hold_election().await;
    assert_eq!(
        election.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("deepseek-deepseek-v4-pro")
    );

    // Add a member whose priority would otherwise put it first.
    let mut next = (*h.state.config()).clone();
    next.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(-100),
    );
    h.state.set_config(next);

    // It is reachable, but the measured model keeps the primary slot until an
    // election has something to say about the newcomer.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );
    assert!(h
        .state
        .registry()
        .tier_members(ModelTier::Standard)
        .iter()
        .any(|m| m.upstream_model == "gpt-sonnet"));

    h.shutdown().await;
}

#[tokio::test]
async fn unknown_and_unclassified_routing_errors() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Remove every opus member so the class is empty.
    cfg.models.retain(|m| m.tier != Some(ModelTier::Reasoning));
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "totally-unknown", "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "not_found_error"
    );

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "opus-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["type"],
        "overloaded_error"
    );

    assert_eq!(h.mock.count(), 0);
    let summary = h.state.stats().summary();
    assert_eq!(summary.requests, 2);
    assert_eq!(summary.failures, 2);

    h.shutdown().await;
}

#[tokio::test]
async fn claude_style_model_names_are_routed_by_class() {
    let h = Harness::new().await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "claude-sonnet-4-5-20250929",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn config_can_be_swapped_while_running() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr), mock).await;

    let mut cfg = (*h.state.config()).clone();
    cfg.models.retain(|m| m.upstream_model != "deepseek-v4-pro");
    cfg.models.push(ModelEntry::for_upstream(
        "openai",
        "gpt-sonnet",
        Some(ModelTier::Standard),
    ));
    h.state.set_config(cfg);

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");

    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    h.shutdown().await;
}

#[tokio::test]
async fn the_same_model_from_two_providers_stays_addressable() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Both providers offer a model with the very same upstream name, which is
    // what happens as soon as an aggregator sits next to a direct account.
    cfg.models.push(ModelEntry::for_upstream(
        "openai",
        "deepseek-v4-pro",
        Some(ModelTier::Standard),
    ));
    let h = Harness::start(cfg, mock).await;

    let listing: Value = h
        .get("/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = listing["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"deepseek-deepseek-v4-pro"), "{ids:?}");
    assert!(ids.contains(&"openai-deepseek-v4-pro"), "{ids:?}");

    // Each id reaches its own provider, and each provider is asked for the bare
    // model name with its own key.
    for (id, provider, key) in [
        ("deepseek-deepseek-v4-pro", "DeepSeek", "Bearer sk-deepseek"),
        ("openai-deepseek-v4-pro", "OpenAI", "Bearer sk-openai"),
    ] {
        let resp = h
            .post("/v1/messages")
            .json(&json!({"model": id, "max_tokens": 16,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{id}");
        assert_eq!(resp.headers()["x-zroutery-model"], id);
        assert_eq!(resp.headers()["x-zroutery-provider"], provider);
        assert_eq!(h.mock.bodies().last().unwrap()["model"], "deepseek-v4-pro");
        assert_eq!(h.mock.keys().last().unwrap(), key);
    }

    // They are separate members of the same class, so they can cover for each
    // other and are accounted for separately.
    let health = h.state.router().health_snapshot();
    assert_eq!(health.len(), 2);
    assert_eq!(health[0].model_id, "deepseek-deepseek-v4-pro");
    assert_eq!(health[1].model_id, "openai-deepseek-v4-pro");
    assert!(health.iter().all(|m| m.total_success == 1));

    h.shutdown().await;
}

#[tokio::test]
async fn ids_from_before_0_2_keep_working() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // What `AppConfig::normalize` leaves behind for a 0.1.x configuration: the
    // old free-form id survives as an alias next to the derived one.
    cfg.models[1].aliases.push("deepseek-v4-pro".into());
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "deepseek-v4-pro",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The answer reports the current id, so clients can migrate when they like.
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );

    h.shutdown().await;
}
