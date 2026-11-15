use serde_json::{json, Value};

use super::support::*;

#[tokio::test]
async fn model_listing_exposes_real_and_virtual_models() {
    let h = Harness::new().await;

    let body: Value = h
        .get("/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["object"], "list");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"deepseek-deepseek-v4-flash"));
    assert!(ids.contains(&"deepseek-deepseek-v4-pro"));
    assert!(ids.contains(&"openai-gpt-5.3-sol"));
    assert!(ids.contains(&"anthropic-claude-native"));
    assert!(ids.contains(&"reasoning-class"));
    assert!(ids.contains(&"standard-class"));
    assert!(ids.contains(&"fast-class"));

    // Both dialects find what they expect on each entry.
    let entry = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "standard-class")
        .unwrap();
    assert_eq!(entry["object"], "model");
    assert_eq!(entry["type"], "model");
    assert!(entry["created"].is_i64());
    assert!(entry["created_at"].is_string());
    assert_eq!(entry["zroutery"]["virtual"], true);
    assert_eq!(entry["zroutery"]["member_count"], 1);

    // The unclassified model is listed but has no class.
    let entry = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "anthropic-claude-native")
        .unwrap();
    assert_eq!(entry["zroutery"]["tier"], Value::Null);
    assert_eq!(entry["owned_by"], "Anthropic");

    let single: Value = h
        .get("/v1/models/openai-gpt-5.3-sol")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(single["id"], "openai-gpt-5.3-sol");
    assert_eq!(single["zroutery"]["tier"], "reasoning");

    assert_eq!(h.get("/v1/models/nope").send().await.unwrap().status(), 404);

    h.shutdown().await;
}

#[tokio::test]
async fn the_model_listing_carries_the_context_window() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    let entry = cfg
        .models
        .iter_mut()
        .find(|m| m.upstream_model == "deepseek-v4-flash")
        .expect("the harness lists this model");
    entry.context_window = Some(1_000_000);
    let h = Harness::start(cfg, mock).await;

    let body: Value = h
        .get("/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "deepseek-deepseek-v4-flash")
        .expect("the model is listed");
    assert_eq!(listed["context_window"], 1_000_000);
    assert_eq!(listed["zroutery"]["context_window"], 1_000_000);

    // A model whose window was never declared does not get an invented one.
    let unknown = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "openai-gpt-5.3-sol")
        .expect("the model is listed");
    assert!(unknown["context_window"].is_null(), "{unknown}");

    h.shutdown().await;
}

#[tokio::test]
async fn count_tokens_endpoint_answers_anthropic_clients() {
    let h = Harness::new().await;
    let body: Value = h
        .post("/v1/messages/count_tokens")
        .json(&json!({
            "model": "sonnet-class",
            "messages": [{"role": "user", "content": "count these characters please"}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["input_tokens"].as_u64().unwrap() > 0);
    assert_eq!(h.mock.count(), 0, "estimated locally, no upstream call");
    // Unpriced models simply say which model answered.
    assert_eq!(body["zroutery"]["estimated"], true);
    assert_eq!(body["zroutery"]["model"], "deepseek-deepseek-v4-pro");
    assert!(body["zroutery"]["estimated_input_cost"].is_null());
    h.shutdown().await;
}
