//! Operator-reported Claude Code 2.1.281 gateway envelope compatibility.
use serde_json::json;
use zroutery_core::ir::{Dialect, Role};

#[test]
fn system_messages_keep_their_position_and_never_become_user_turns() {
    let body = json!({"model":"opus","system":"outer","messages":[
        {"role":"system","content":[{"type":"text","text":"review instruction","cache_control":{"type":"ephemeral"}}]},
        {"role":"user","content":"test command"}
    ],"max_tokens":200});
    let request = zroutery_core::protocol::decode_request(Dialect::Anthropic, body).unwrap();
    assert_eq!(request.system.len(), 1);
    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.messages[0].role, Role::System);
    assert_eq!(request.messages[1].role, Role::User);
    let encoded = zroutery_core::protocol::anthropic::encode_request(&request, "mock").unwrap();
    assert_eq!(encoded["messages"][0]["role"], "system");
    assert_eq!(
        encoded["messages"][0]["content"][0]["text"],
        "review instruction"
    );
    assert_eq!(
        encoded["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn non_text_system_content_is_refused_and_never_auto_approved() {
    for content in [
        json!([{"type":"tool_use","id":"x","name":"Bash","input":{}}]),
        json!(42),
    ] {
        let body = json!({"model":"opus","messages":[{"role":"system","content":content}],"max_tokens":200});
        assert!(zroutery_core::protocol::decode_request(Dialect::Anthropic, body).is_err());
    }
    assert_eq!(
        zroutery_core::classifier::parse_verdict("parse failed"),
        zroutery_core::classifier::ClassifierVerdict::Unparseable
    );
}

#[test]
fn cross_protocol_system_instructions_retain_their_position_or_are_explicitly_refused() {
    let body = json!({"model":"standard-class","system":"outer","messages":[
        {"role":"user","content":"earlier turn"},
        {"role":"assistant","content":"earlier answer"},
        {"role":"system","content":"review instruction"},
        {"role":"user","content":"fixture"}
    ],"max_tokens":200});
    let request = zroutery_core::protocol::decode_request(Dialect::Anthropic, body).unwrap();
    let openai = zroutery_core::protocol::openai::encode_request(&request, "mock").unwrap();
    assert_eq!(openai["messages"][3]["role"], "system");
    assert_eq!(
        openai["messages"][3]["content"][0]["text"],
        "review instruction"
    );
    assert_eq!(openai["messages"][4]["role"], "user");
    assert!(
        zroutery_core::protocol::gemini::encode_request(&request, "mock").is_err(),
        "a protocol that cannot retain system authority must not turn it into a user message"
    );
}

#[tokio::test]
async fn the_gateway_accepts_claudes_system_role_and_forwards_it_to_the_native_upstream() {
    use axum::{routing::post, Json};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use zroutery_core::config::{
        AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
    };
    use zroutery_core::server::{AppState, ServerHandle};

    let captured = Arc::new(Mutex::new(Vec::new()));
    let requests = captured.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let requests = requests.clone();
                async move {
                    requests.lock().unwrap().push(body);
                    Json(
                        json!({"id":"answer","type":"message","role":"assistant","model":"mock",
                    "content":[{"type":"text","text":"fixture answer"}],"stop_reason":"end_turn",
                    "usage":{"input_tokens":10,"output_tokens":1}}),
                    )
                }
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    let mut provider = ProviderConfig::new("p", "Mock", ProviderKind::Anthropic);
    provider.key_ref.clear();
    provider.base_url = format!("http://{address}");
    config.providers.push(provider);
    config
        .models
        .push(ModelEntry::for_upstream("p", "mock", Some(ModelTier::Fast)));
    let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let gateway = ServerHandle::start(state).await.unwrap();
    let client = reqwest::Client::new();
    let body = json!({"model":"fast-class","messages":[
        {"role":"user","content":"earlier turn"},
        {"role":"assistant","content":"earlier answer"},
        {"role":"system","content":[{"type":"text","text":"review instruction","cache_control":{"type":"ephemeral"}}]},
        {"role":"user","content":"fixture"}
    ],"max_tokens":200});
    let count = client
        .post(format!("http://{}/v1/messages/count_tokens", gateway.addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(count.status(), 200);
    assert!(
        captured.lock().unwrap().is_empty(),
        "counting must not submit inference"
    );
    let response = client
        .post(format!("http://{}/v1/messages", gateway.addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    {
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "mock");
        assert_eq!(requests[0]["messages"], body["messages"]);
    }
    gateway.stop().await;
    mock.abort();
}
