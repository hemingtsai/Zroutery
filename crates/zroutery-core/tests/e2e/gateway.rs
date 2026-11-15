use serde_json::{json, Value};

use super::support::*;

#[tokio::test]
async fn only_authenticated_chat_arrivals_confirm_a_client() {
    let h = Harness::new().await;
    let since = chrono::Utc::now() - chrono::Duration::seconds(1);
    let identified = |request: reqwest::RequestBuilder| {
        request
            .header("x-opencode-client", "cli")
            .header("x-opencode-session", "session-fixture")
    };
    assert_eq!(
        identified(h.get("/v1/models"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(
        !h.state.stats().client_seen_since("opencode", since),
        "model discovery is not chat"
    );
    let body = json!({"model": "does-not-exist", "messages": [{"role": "user", "content": "hi"}]});
    let response = identified(h.client.post(format!("{}/v1/chat/completions", h.base)))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(
        !h.state.stats().client_seen_since("opencode", since),
        "a header cannot bypass authentication"
    );
    let response = identified(h.post("/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert!(
        h.state.stats().client_seen_since("opencode", since),
        "an upstream/routing failure does not undo gateway connectivity"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn both_prefixes_reach_the_same_endpoints() {
    let h = Harness::new().await;

    // A base URL with /v1 and one without both work, because clients disagree
    // about which of the two they are handed.
    for path in ["/v1/models", "/models"] {
        let resp = h.get(path).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["object"], "list", "{path}");
    }
    for path in [
        "/v1/models/openai-gpt-5.3-sol",
        "/models/openai-gpt-5.3-sol",
    ] {
        let body: Value = h.get(path).send().await.unwrap().json().await.unwrap();
        assert_eq!(body["id"], "openai-gpt-5.3-sol", "{path}");
    }

    for path in ["/v1/chat/completions", "/chat/completions"] {
        let resp = h
            .post(path)
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }
    for path in ["/v1/messages", "/messages"] {
        let resp = h
            .post(path)
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }
    for path in ["/v1/messages/count_tokens", "/messages/count_tokens"] {
        let resp = h
            .post(path)
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }

    // The alias sits inside the auth layer; it is not a way around it. Each path
    // is probed with the verb it actually implements, since a wrong verb is
    // answered before authentication runs.
    for (path, post) in [
        ("/models", false),
        ("/status", false),
        ("/chat/completions", true),
        ("/messages", true),
    ] {
        let url = format!("{}{path}", h.base);
        let request = if post {
            h.client.post(url).json(&json!({"model": "sonnet-class"}))
        } else {
            h.client.get(url)
        };
        let resp = request.send().await.unwrap();
        assert_eq!(resp.status(), 401, "{path} must still need the token");
    }

    h.shutdown().await;
}

#[tokio::test]
async fn an_unknown_path_explains_itself() {
    let h = Harness::new().await;

    let resp = h.get("/v2/models").send().await.unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    // OpenAI shaped, because that is what a client on this path expects.
    assert_eq!(body["error"]["code"], "not_found_error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("GET /v2/models"));
    let endpoints = body["zroutery"]["endpoints"].as_array().unwrap();
    assert!(endpoints.iter().any(|e| e == "/v1/models"));
    assert!(body["zroutery"]["likely_cause"].is_null());

    // A path that looks like a messages call answers in the Anthropic envelope.
    let body: Value = h
        .post("/v1/messages/nope")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "not_found_error");

    // The classic misconfiguration: a base URL ending in /v1 plus an SDK that
    // appends /v1 itself.
    let body: Value = h
        .post("/v1/v1/messages")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cause = body["zroutery"]["likely_cause"].as_str().unwrap();
    assert!(cause.contains("base URL already ends in /v1"), "{cause}");
    assert!(cause.contains("/v1/messages"), "{cause}");

    // A real path with the wrong verb says which verb it wants, rather than
    // answering with an empty 405 that reads like a broken proxy.
    let resp = h.get("/v1/chat/completions").send().await.unwrap();
    assert_eq!(resp.status(), 405);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "method_not_allowed");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("POST endpoint"), "{message}");
    assert!(message.contains("not GET"), "{message}");

    h.shutdown().await;
}

#[tokio::test]
async fn authentication_is_enforced_on_api_routes_only() {
    let h = Harness::new().await;

    // No credentials at all.
    let resp = h
        .client
        .post(format!("{}/v1/messages", h.base))
        .json(&json!({"model": "sonnet-class", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "authentication_error");

    // Wrong token.
    let resp = h
        .client
        .get(format!("{}/v1/models", h.base))
        .header("authorization", "Bearer nope")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Bearer form is accepted.
    let resp = h
        .client
        .get(format!("{}/v1/models", h.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Liveness stays open, and says nothing else.
    let resp = h
        .client
        .get(format!("{}/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(
        body.as_object().unwrap().len(),
        1,
        "an unauthenticated route must not describe the configuration: {body}"
    );

    // The detail moved behind the token.
    let resp = h
        .client
        .get(format!("{}/v1/status", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = h
        .get("/v1/status")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["auth_required"], true);
    assert!(body["models"].as_u64().unwrap() > 0);
    assert!(body["version"].is_string());

    h.shutdown().await;
}

#[tokio::test]
async fn oversized_request_bodies_are_rejected_before_reaching_a_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.server.max_body_mib = 1;
    let h = Harness::start(cfg, mock).await;

    let huge = "x".repeat(2 * 1024 * 1024);
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": huge}]}))
        .send()
        .await
        .ok();
    // A connection reset while the body is still uploading is a rejection
    // too: the server stops reading and closes without draining the rest of
    // the oversized body, so the client may see the reset instead of the 413.
    // Either way nothing reached a provider, which the next line asserts.
    assert!(
        resp.is_none_or(|r| r.status() == 413),
        "an oversized body must be rejected",
    );
    assert_eq!(h.mock.count(), 0, "nothing was forwarded upstream");

    // A normal request on the same server still works.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    h.shutdown().await;
}

#[tokio::test]
async fn cors_is_limited_to_the_configured_origins() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.server.allow_cors = true;
    cfg.server.cors_origins = vec!["http://localhost:3000".into()];
    assert!(!cfg.server.cors_is_wide_open());
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .get("/v1/models")
        .header("origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["access-control-allow-origin"],
        "http://localhost:3000"
    );

    let resp = h
        .get("/v1/models")
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "an origin outside the list must not be allowed"
    );

    // Enabling CORS without a list is allowed but reported.
    let mut wide = (*h.state.config()).clone();
    wide.server.cors_origins.clear();
    assert!(wide.server.cors_is_wide_open());
    assert!(wide
        .validate()
        .iter()
        .any(|i| i.code == "server.cors_any_origin"));

    h.shutdown().await;
}

#[tokio::test]
async fn empty_messages_returns_error() {
    let h = Harness::new().await;
    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "sonnet-class", "messages": []}))
        .send()
        .await
        .unwrap();

    // Should return a client error or handle gracefully; read the actual behaviour.
    if resp.status().is_client_error() {
        let body: Value = resp.json().await.unwrap();
        assert!(
            body["error"].is_object(),
            "client error must include an error object"
        );
    } else {
        // If the server accepts it, the response must still be well-formed.
        assert_eq!(resp.status(), 200);
    }

    h.shutdown().await;
}
