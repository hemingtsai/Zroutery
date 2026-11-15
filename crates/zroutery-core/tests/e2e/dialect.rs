use serde_json::{json, Value};

use super::support::*;

#[tokio::test]
async fn responses_thinking_history_can_reach_a_chat_upstream() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].capabilities.thinking = true;
    let h = Harness::start(cfg, mock).await;
    for streaming in [false, true] {
        let response = h.post("/v1/responses").json(&json!({
            "model": "standard-class", "stream": streaming,
            "input": [
                {"role": "user", "content": "hi"},
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "history reasoning"}]},
                {"role": "assistant", "content": [{"type": "output_text", "text": "hello"}]},
                {"role": "user", "content": "continue"}
            ]
        })).send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert_eq!(status, 200, "{text}");
        assert!(!text.contains("unsupported content type"));
    }
    assert_eq!(h.mock.count(), 2);
    for body in h.mock.bodies() {
        assert!(body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["reasoning_content"] == "history reasoning"));
    }
    h.shutdown().await;
}

#[tokio::test]
async fn anthropic_in_openai_out_non_streaming() {
    let h = Harness::new().await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 100,
            "system": "be brief",
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
    assert_eq!(resp.headers()["x-zroutery-provider"], "DeepSeek");
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], "deepseek-deepseek-v4-pro");
    assert_eq!(body["content"][0]["text"], "hello from mock");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["usage"]["input_tokens"], 11);
    assert_eq!(body["usage"]["output_tokens"], 7);

    // The upstream saw an OpenAI shaped request with the mapped model id.
    let sent = &h.mock.bodies()[0];
    // The provider is asked for its own name, not for our namespaced id.
    assert_eq!(sent["model"], "deepseek-v4-pro");
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(sent["messages"][0]["content"], "be brief");
    assert_eq!(sent["messages"][1]["role"], "user");
    assert_eq!(sent["messages"][1]["content"], "hi");
    assert_eq!(sent["max_tokens"], 100);
    assert_eq!(h.mock.keys()[0], "Bearer sk-deepseek");

    h.shutdown().await;
}

#[tokio::test]
async fn openai_in_openai_out_with_tool_calls() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Force the mock into tool-call mode via the upstream model name.
    cfg.models[1].upstream_model = "tools-model".into();
    let h = Harness::start(cfg, mock).await;

    let body: Value = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(body["choices"][0]["message"]["content"], Value::Null);
    let call = &body["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "get_weather");
    assert_eq!(call["function"]["arguments"], "{\"city\":\"SH\"}");
    assert_eq!(body["usage"]["total_tokens"], 18);

    let sent = &h.mock.bodies()[0];
    assert_eq!(sent["tools"][0]["function"]["name"], "get_weather");

    h.shutdown().await;
}

#[tokio::test]
async fn unsupported_reasoning_effort_fails_closed_without_dropping_the_control() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Make the quirk observable: without the transform, the encoded body would
    // carry reasoning_effort and the test would see it.
    cfg.providers[0].quirks.send_reasoning_effort = true;
    let h = Harness::start(cfg, mock).await;

    let response = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        422,
        "unknown class effort cannot be silently dropped"
    );
    assert_eq!(h.mock.count(), 0);

    h.shutdown().await;
}

#[tokio::test]
async fn unsupported_streaming_effort_is_also_refused_without_a_send() {
    // Agent clients stream. The preflight that drops the control must run on
    // both attempt loops, or the fix would exist only for buffered callers.
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.providers[0].quirks.send_reasoning_effort = true;
    let h = Harness::start(cfg, mock).await;

    let response = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "reasoning_effort": "high",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 422);
    // Drain the stream so the upstream attempt is complete before inspecting it.
    let _ = response.text().await.unwrap();
    assert_eq!(h.mock.count(), 0);

    h.shutdown().await;
}

#[tokio::test]
async fn echoed_reasoning_history_degrades_without_losing_the_turn() {
    let h = Harness::new().await;

    let response = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello", "reasoning_content": "prior reasoning"},
                {"role": "user", "content": "again"}
            ]
        }))
        .send()
        .await
        .unwrap();

    assert!(
        response.status().is_success(),
        "reasoning the client echoes back must not sink the next turn: {}",
        response.status()
    );
    assert_eq!(
        response
            .headers()
            .get("x-zroutery-degraded")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    let sent = &h.mock.bodies()[0];
    assert!(
        sent["messages"][1].get("reasoning_content").is_none(),
        "the thinking block was forwarded to a target that does not declare it: {sent}"
    );
    assert_eq!(sent["messages"][2]["content"], "again");

    h.shutdown().await;
}

#[tokio::test]
async fn a_declared_thinking_capability_is_not_degraded() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].capabilities.thinking = true;
    cfg.models[1].effort_levels = vec!["xhigh".into()];
    cfg.providers[0].quirks.send_reasoning_effort = true;
    let h = Harness::start(cfg, mock).await;

    let response = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "reasoning_effort": "xhigh",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    assert!(
        response.headers().get("x-zroutery-degraded").is_none(),
        "native thinking support is not a degraded answer"
    );
    assert_eq!(
        h.mock.bodies()[0]["reasoning_effort"],
        "xhigh",
        "a declared-thinking target keeps the level the client asked for"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn anthropic_client_streaming_over_an_openai_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "think-model".into();
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let wire = resp.text().await.unwrap();

    // Anthropic clients need the full event lifecycle, in order.
    let order: Vec<&str> = wire
        .lines()
        .filter_map(|l| l.strip_prefix("event: "))
        .collect();
    assert_eq!(order.first(), Some(&"message_start"));
    assert_eq!(order.last(), Some(&"message_stop"));
    assert!(order.contains(&"content_block_start"));
    assert!(order.contains(&"message_delta"));

    // Reasoning became a thinking block, and text its own block.
    assert!(wire.contains("\"type\":\"thinking\""));
    assert!(wire.contains("\"thinking\":\"step 1\""));
    assert!(wire.contains("\"text\":\"hel\""));
    assert!(wire.contains("\"text\":\"lo\""));
    // Usage from the trailing OpenAI chunk survived the translation.
    assert!(wire.contains("\"output_tokens\":2"));
    // Two blocks opened, two closed.
    assert_eq!(wire.matches("event: content_block_start").count(), 2);
    assert_eq!(wire.matches("event: content_block_stop").count(), 2);

    let stats = h.state.stats().summary();
    assert_eq!(stats.requests, 1);
    assert_eq!(stats.failures, 0);
    assert_eq!(stats.output_tokens, 2);

    h.shutdown().await;
}

#[tokio::test]
async fn openai_client_streaming_over_an_anthropic_provider() {
    let h = Harness::new().await;

    let wire = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "anthropic-claude-native",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true}
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(wire.contains("\"object\":\"chat.completion.chunk\""));
    assert!(wire.contains("\"content\":\"claude \""));
    assert!(wire.contains("\"content\":\"stream\""));
    assert!(wire.contains("\"finish_reason\":\"stop\""));
    assert!(wire.contains("\"prompt_tokens\":4"));
    assert!(wire.contains("\"completion_tokens\":6"));
    assert!(wire.trim_end().ends_with("data: [DONE]"));

    // The Anthropic upstream got an Anthropic shaped body and the right auth.
    let sent = &h.mock.bodies()[0];
    assert_eq!(sent["model"], "claude-native");
    assert_eq!(sent["messages"][0]["content"][0]["type"], "text");
    assert_eq!(sent["max_tokens"], 4096, "anthropic requires max_tokens");
    assert_eq!(h.mock.keys()[0], "sk-ant");

    h.shutdown().await;
}
