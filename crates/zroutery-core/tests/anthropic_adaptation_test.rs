//! Messages extensions are native data, not executable tools or persisted bodies.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::budget::Ledger;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::{Capability, Dialect, UsageEvidence};
use zroutery_core::protocol::{self, anthropic, gemini, openai, responses};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<Vec<(Value, HeaderMap)>>>);

fn native_body(stream: bool) -> Value {
    json!({"model":"standard-class", "max_tokens":4096, "stream":stream,
        "thinking":{"type":"between_tools"},
        "output_config":{"effort":"high"},
        "system":[{"type":"text","text":"original attribution","cache_control":{"type":"ephemeral","ttl":"1h"}},
            {"type":"text","text":"original instructions"}],
        "tools":[{"name":"ToolSearch","input_schema":{"type":"object"}},
            {"name":"Read","input_schema":{"type":"object"},"strict":true,"defer_loading":true,"eager_input_streaming":true}],
        "messages":[{"role":"user","content":"local fixture"},
            {"role":"system","content":[],"output_config":{"effort":"low"}},
            {"role":"system","clear_at":"next_user_message","content":[
                {"type":"tool_addition","tool":{"type":"tool_reference","name":"Read"}},
                {"type":"text","text":"turn-scoped fixture"}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"search-original","content":[
                {"type":"tool_reference","tool_name":"Read"}]}]}],
        "future_request":{"keep":true}})
}

fn native_reply(compact: bool) -> Value {
    let content = if compact {
        json!([{"type":"compaction","content":"local summary","signature":"opaque-fixture"}])
    } else {
        json!([{"type":"thinking","thinking":"","signature":"opaque-fixture"},
            {"type":"server_tool_use","id":"server-original","name":"tool_search_tool_regex","input":{"pattern":"read"}},
            {"type":"tool_search_tool_result","tool_use_id":"server-original","content":[{"type":"tool_reference","tool_name":"Read"}]},
            {"type":"tool_use","id":"tool-original","name":"Read","input":{"file_path":"local-fixture"},"caller":{"type":"direct"}},
            {"type":"text","text":"local accepted","citations":[],"future_block":{"keep":true}}])
    };
    let usage = if compact {
        json!({"input_tokens":0,"output_tokens":0,"iterations":[{"type":"compaction","input_tokens":100,"output_tokens":10,
            "cache_read_input_tokens":20,"cache_creation_input_tokens":5,
            "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":5}}]})
    } else {
        json!({"input_tokens":20,"output_tokens":3})
    };
    json!({"id":"native-original","type":"message","role":"assistant","model":"upstream-declared",
        "content":content,"stop_reason":if compact {"compaction"} else {"tool_use"},"usage":usage,
        "input_transformations":[{"type":"thinking_dropped","path":"messages.1.content.0","reason":"fixture"}],
        "future_reply":{"keep":true}})
}

async fn native(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    mock.0.lock().unwrap().push((body.clone(), headers));
    if body.get("return_error").is_some() {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"type":"error","error":{
            "type":"invalid_request_error","message":"messages.2.output_config: Extra inputs are not permitted",
            "details":{"error_code":"fixture_capability"}},"request_id":"request-original"}))).into_response();
    }
    if let Some(reply) = body.get("fixture_reply") {
        return Json(reply.clone()).into_response();
    }
    if body.get("truncated_fixture").is_some() || body.get("stream_error_fixture").is_some() {
        let start = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"partial-original\",\"model\":\"native\",\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}\n\n";
        let block = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"partial fixture\"}}\n\n";
        let tail = if body.get("stream_error_fixture").is_some() {
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"native error wording\",\"details\":{\"keep\":true}}}\n\n"
        } else {
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n"
        };
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(format!("{start}{block}{tail}")))
            .unwrap();
    }
    let reply = native_reply(body.get("compaction").is_some());
    if body["stream"] != true {
        return Json(reply).into_response();
    }
    let frame = |kind: &str, data: Value| format!("event: {kind}\ndata: {data}\n\n");
    let mut opening = reply.clone();
    opening["content"] = json!([]);
    opening["stop_reason"] = Value::Null;
    opening["usage"] = json!({"input_tokens":20,"output_tokens":0});
    let mut sse = frame(
        "message_start",
        json!({"type":"message_start","message":opening}),
    );
    sse += &frame("ping", json!({"type":"ping","future_ping":true}));
    for (index, block) in reply["content"].as_array().unwrap().iter().enumerate() {
        sse += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":index,"content_block":block}),
        );
        sse += &frame(
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        );
    }
    sse += &frame(
        "future_progress",
        json!({"type":"future_progress","keep":true}),
    );
    sse += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":reply["stop_reason"],
        "stop_details":null,"future_delta":true},"usage":reply["usage"]}),
    );
    sse += &frame(
        "message_stop",
        json!({"type":"message_stop","future_terminal":true}),
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

async fn chat(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    mock.0.lock().unwrap().push((body, headers));
    Json(
        json!({"id":"chat-original","model":"chat","choices":[{"message":{"role":"assistant","content":"chat accepted"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":10,"completion_tokens":1}}),
    )
}

async fn count(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    mock.0.lock().unwrap().push((body, headers));
    Json(json!({"input_tokens":321,"future_count":{"keep":true}}))
}

async fn harness() -> (
    Arc<AppState>,
    ServerHandle,
    Mock,
    tokio::task::JoinHandle<()>,
) {
    let mock = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = axum::Router::new()
        .route("/v1/messages", post(native))
        .route("/v1/messages/count_tokens", post(count))
        .route("/v1/chat/completions", post(chat))
        .with_state(mock.clone());
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.auth_token = "local-key".into();
    config.server.bypass_proxy = true;
    config.routing.max_attempts = 1;
    for (id, kind) in [
        ("chat", ProviderKind::OpenAICompatible),
        ("native", ProviderKind::Anthropic),
    ] {
        let mut provider = ProviderConfig::new(id, id, kind);
        provider.base_url = format!("http://{addr}");
        provider.key_ref.clear();
        config.providers.push(provider);
        let mut model = ModelEntry::for_upstream(id, id, Some(ModelTier::Standard));
        model.priority = if id == "chat" { 0 } else { 10 };
        model.capabilities.tools = true;
        model.capabilities.thinking = true;
        model.capabilities.structured_output = true;
        model.effort_levels = vec!["low".into(), "high".into()];
        let mut price = Pricing::new("USD", 2.0, 4.0);
        price.cache_read_per_mtok = Some(0.2);
        price.cache_write_per_mtok = Some(2.5);
        price.cache_write_5m_per_mtok = Some(2.5);
        price.cache_write_1h_per_mtok = Some(4.0);
        model.pricing = Some(price);
        config.models.push(model);
    }
    let state = Arc::new(AppState::new(
        config,
        Arc::new(
            MemorySecretStore::new()
                .with("first-secret-ref", "first-fixture-key")
                .with("second-secret-ref", "second-fixture-key"),
        ),
    ));
    let gateway = ServerHandle::start(state.clone()).await.unwrap();
    (state, gateway, mock, task)
}

#[tokio::test]
async fn native_extensions_and_every_sse_frame_survive_without_server_review() {
    let (state, gateway, mock, task) = harness().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let body = native_body(stream);
        let response = client
            .post(format!("http://{}/v1/messages?beta=true", gateway.addr))
            .header("authorization", "Bearer local-key")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "first-future")
            .header("anthropic-beta", "second-future")
            .header("anthropic-workspace-id", "fixture-workspace")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-zroutery-model"], "native-native");
        let wire = response.text().await.unwrap();
        if stream {
            assert!(wire.contains("future_progress"));
            assert!(wire.contains("future_terminal"));
            assert!(wire.contains("future_delta"));
            assert!(wire.contains("future_ping"));
            assert!(wire.contains("input_transformations"));
            assert!(wire.contains("server_tool_use"));
            assert!(wire.contains("tool_search_tool_result"));
            assert!(wire.contains("opaque-fixture"));
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(&wire).unwrap(),
                native_reply(false)
            );
        }
        let (sent, headers) = mock.0.lock().unwrap().last().cloned().unwrap();
        let mut expected = body;
        expected["model"] = json!("native");
        assert_eq!(sent, expected);
        assert_eq!(
            headers
                .get_all("anthropic-beta")
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["first-future", "second-future"]
        );
        assert_eq!(headers["anthropic-workspace-id"], "fixture-workspace");
    }
    assert!(state
        .stats()
        .recent(2)
        .iter()
        .all(|record| record.ok && record.kind.is_main()));
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn exact_non_native_model_is_rejected_before_sending_extensions() {
    let (_, gateway, mock, task) = harness().await;
    let mut body = native_body(false);
    body["model"] = json!("chat-chat");
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/messages", gateway.addr))
        .header("authorization", "Bearer local-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("native Messages upstream"));
    assert!(mock.0.lock().unwrap().is_empty());
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn upstream_capability_errors_retain_the_original_schema_and_wording() {
    let (_, gateway, _, task) = harness().await;
    for stream in [false, true] {
        let mut body = native_body(stream);
        body["return_error"] = json!(true);
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let reply: Value = response.json().await.unwrap();
        assert_eq!(
            reply["error"]["message"],
            "messages.2.output_config: Extra inputs are not permitted"
        );
        assert_eq!(
            reply["error"]["details"]["error_code"],
            "fixture_capability"
        );
        assert_eq!(reply["request_id"], "request-original");
    }
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn signed_compaction_keeps_native_usage_and_settles_its_nonzero_iteration_once() {
    let (state, gateway, _, task) = harness().await;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(RuntimeStore::open(&root.path().join("runtime.db")).unwrap());
    state
        .attach_accounting(store.clone(), &Ledger::new(), "compaction fixture")
        .unwrap();
    for stream in [false, true] {
        let body = json!({"model":"native-native","max_tokens":4096,"stream":stream,
            "messages":[{"role":"user","content":"local history"}],"compaction":{"type":"summarize"}});
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .header("anthropic-beta", "compact-2026-09-04")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let wire = response.text().await.unwrap();
        assert!(wire.contains("opaque-fixture"));
        assert!(wire.contains("\"iterations\""));
    }
    let records = store.usage().unwrap();
    assert_eq!(records.len(), 2);
    for record in records {
        let attempts = store.usage_attempts(&record.id).unwrap();
        assert_eq!(attempts.len(), 1);
        let attempt = &attempts[0];
        assert_eq!(attempt.usage_evidence, Some(UsageEvidence::Measured));
        assert_eq!(attempt.usage.unwrap().input_tokens, 125);
        assert_eq!(attempt.usage.unwrap().output_tokens, 10);
        assert_eq!(
            attempt.usage_iterations.as_ref().unwrap()[0].cache_write_1h_tokens,
            Some(5)
        );
        let expected = 100.0 * 2e-6 + 20.0 * 0.2e-6 + 5.0 * 4e-6 + 10.0 * 4e-6;
        assert!((attempt.cost.as_ref().unwrap().amount - expected).abs() < 1e-12);
        let safe = serde_json::to_string(attempt).unwrap();
        assert!(!safe.contains("local history"));
        assert!(!safe.contains("opaque-fixture"));
        assert_eq!(record.accounting.unwrap().unknown_cost_attempts, 0);
    }
    gateway.stop().await;
    task.abort();
}

#[test]
fn structured_outputs_and_strict_tools_translate_in_both_directions() {
    let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false});
    let req = anthropic::decode_request(json!({"model":"m","max_tokens":100,
        "messages":[{"role":"user","content":"q"}],"tools":[{"name":"f","input_schema":schema,"strict":true}],
        "output_config":{"format":{"type":"json_schema","schema":schema},"effort":"high"},"thinking":{"type":"adaptive"}})).unwrap();
    assert!(!req.anthropic_native_only);
    assert!(req
        .required_capabilities
        .contains(&Capability::StructuredOutput));
    let chat = openai::encode_request(&req, "m").unwrap();
    assert_eq!(chat["tools"][0]["function"]["strict"], true);
    assert_eq!(chat["response_format"]["json_schema"]["schema"], schema);
    let response = responses::encode_request(&req, "m").unwrap();
    assert_eq!(response["tools"][0]["strict"], true);
    let back = anthropic::encode_request(&openai::decode_request(chat).unwrap(), "m").unwrap();
    assert_eq!(back["tools"][0]["strict"], true);
    assert_eq!(back["output_config"]["format"]["schema"], schema);
    assert_eq!(back["output_config"]["effort"], "high");
    assert!(gemini::encode_request(&req, "m").is_err());
}

#[test]
fn native_extensions_never_leak_into_any_direct_encoder() {
    let req = anthropic::decode_request(native_body(false)).unwrap();
    assert!(req.anthropic_native_only);
    for dialect in [Dialect::OpenAI, Dialect::OpenAIResponses, Dialect::Gemini] {
        assert!(protocol::encode_request(
            dialect,
            &req,
            "m",
            &Default::default(),
            anthropic::AnthropicApiVersion::Legacy
        )
        .is_err());
    }
    assert!(openai::encode_request(&req, "m").is_err());
    assert!(responses::encode_request(&req, "m").is_err());
    assert!(gemini::encode_request(&req, "m").is_err());
}

#[tokio::test]
async fn native_stream_requires_message_stop_and_preserves_midstream_error_without_replay() {
    let (state, gateway, mock, task) = harness().await;
    for field in ["truncated_fixture", "stream_error_fixture"] {
        let mut body = json!({"model":"native-native","stream":true,"max_tokens":100,
            "messages":[{"role":"user","content":"fixture"}]});
        body[field] = json!(true);
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let wire = response.text().await.unwrap();
        assert!(wire.contains("partial fixture"));
        assert!(!wire.contains("event: message_stop"));
        assert_eq!(wire.matches("event: error").count(), 1);
        if field == "stream_error_fixture" {
            assert!(wire.contains("native error wording"));
            assert!(wire.contains("\"keep\":true"));
        }
    }
    assert_eq!(
        mock.0.lock().unwrap().len(),
        2,
        "partial output must never replay"
    );
    assert!(state.stats().recent(2).iter().all(|record| !record.ok));
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn multi_model_iteration_prices_keep_currencies_unknown_parts_and_no_double_charge() {
    let (state, gateway, _, task) = harness().await;
    let mut config = (*state.config()).clone();
    let mut advisor = ModelEntry::for_upstream("native", "advisor", None);
    advisor.pricing = Some(Pricing::new("CNY", 3.0, 6.0));
    config.models.push(advisor);
    state.set_config(config);
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(RuntimeStore::open(&root.path().join("runtime.db")).unwrap());
    state
        .attach_accounting(store.clone(), &Ledger::new(), "iteration fixture")
        .unwrap();
    let reply = json!({"id":"iteration-reply","type":"message","role":"assistant","model":"native",
        "content":[{"type":"text","text":"fixture answer"}],"stop_reason":"end_turn",
        "usage":{"input_tokens":10,"output_tokens":2,"iterations":[
            {"type":"message","model":"native","input_tokens":10,"output_tokens":2},
            {"type":"advisor_message","model":"advisor","input_tokens":20,"output_tokens":3},
            {"type":"advisor_message","model":"unpriced-advisor","input_tokens":7,"output_tokens":1}]}});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/messages", gateway.addr))
        .header("x-api-key", "local-key")
        .json(&json!({"model":"native-native","max_tokens":100,
            "messages":[{"role":"user","content":"fixture"}],"fixture_reply":reply}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers().get("x-zroutery-cost").is_none(),
        "currencies must not be flattened"
    );
    assert_eq!(response.json::<Value>().await.unwrap(), reply);
    let record = store.usage().unwrap().pop().unwrap();
    let attempt = store.usage_attempts(&record.id).unwrap().pop().unwrap();
    assert_eq!(attempt.usage.unwrap().input_tokens, 37);
    assert_eq!(attempt.usage.unwrap().output_tokens, 6);
    assert_eq!(attempt.unknown_cost_parts, 1);
    let accounting = record.accounting.unwrap();
    assert_eq!(accounting.known_cost.0.len(), 2);
    assert!((accounting.known_cost.get("USD") - 28e-6).abs() < 1e-12);
    assert!((accounting.known_cost.get("CNY") - 78e-6).abs() < 1e-12);
    assert_eq!(accounting.unknown_cost_attempts, 1);
    let before = state.ledger();
    state.retry_accounting().unwrap();
    assert_eq!(state.ledger(), before);
    assert_eq!(
        attempt.execution.iteration_pricing["advisor"].currency,
        "CNY"
    );
    gateway.stop().await;
    task.abort();
}

#[test]
fn new_api_version_validation_keeps_native_prefix_and_schema_headers_consistent() {
    let mut provider = ProviderConfig::new("p", "p", ProviderKind::Anthropic);
    provider.anthropic_version = Some("2023-06-01".into());
    let mut req =
        anthropic::decode_request(json!({"model":"m","temperature":0,"top_p":1,"top_k":1,
        "messages":[{"role":"user","content":"fixture"}]}))
        .unwrap();
    req.client_headers
        .insert("anthropic-version", "2026-10-21".parse().unwrap());
    let body = zroutery_core::upstream::encode_for(&provider, &req, "m", None).unwrap();
    for field in ["temperature", "top_p", "top_k"] {
        assert!(body.get(field).is_none());
    }
    for (role, block) in [
        (
            "assistant",
            json!({"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"AAAA"}}),
        ),
        (
            "user",
            json!({"type":"text","text":"fixture","citations":[{"type":"page_location","page":1}]}),
        ),
    ] {
        let req = anthropic::decode_request(
            json!({"model":"m","messages":[{"role":role,"content":[block]}]}),
        )
        .unwrap();
        assert!(anthropic::encode_request_for(
            &req,
            "m",
            anthropic::AnthropicApiVersion::V2026_10_21
        )
        .is_err());
    }
    for reason in ["pause_turn", "compaction", "model_context_window_exceeded"] {
        assert_eq!(
            anthropic::stop_reason_to_str(anthropic::stop_reason_from_str(reason)),
            Some(reason)
        );
    }
}

#[tokio::test]
async fn embedded_safeguards_on_a_chat_bridge_fall_back_without_fabricated_results() {
    let (state, gateway, mock, task) = harness().await;
    let response = reqwest::Client::builder().no_proxy().build().unwrap().post(format!("http://{}/v1/messages",gateway.addr))
        .header("x-api-key","local-key").header("anthropic-beta","dangerous-tool-use-2026-09-03")
        .json(&json!({"model":"chat-chat","max_tokens":100,"messages":[{"role":"user","content":"fixture"}],
            "safeguards":[{"type":"dangerous_tool_use","classifier_context":{"v":1,"permission_mode":"auto"}}]}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200);
    let reply: Value = response.json().await.unwrap();
    assert!(reply.get("safeguard_results").is_none());
    assert_eq!(reply["content"][0]["text"], "chat accepted");
    let (sent, headers) = mock.0.lock().unwrap().last().cloned().unwrap();
    assert!(sent.get("safeguards").is_none());
    assert!(headers.get("anthropic-beta").is_none());
    assert!(state.stats().recent(1)[0].kind.is_main());
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn message_threads_keep_the_original_model_and_credential_after_priority_changes() {
    use zroutery_core::config::{AccountConfig, InferenceKeyConfig};
    for stream in [false, true] {
        let (state, gateway, mock, task) = harness().await;
        let mut config = (*state.config()).clone();
        config
            .providers
            .iter_mut()
            .find(|provider| provider.id == "native")
            .unwrap()
            .accounts
            .push(AccountConfig {
                account_id: "thread-account".into(),
                key_ref: "panel-not-used".into(),
                enabled: true,
                binding: None,
                probe_kind: None,
                effective_rates: Vec::new(),
                maintenance: Default::default(),
                inference_keys: vec![
                    InferenceKeyConfig {
                        id: "first".into(),
                        key_ref: "first-secret-ref".into(),
                        priority: 0,
                        ..Default::default()
                    },
                    InferenceKeyConfig {
                        id: "second".into(),
                        key_ref: "second-secret-ref".into(),
                        priority: 1,
                        ..Default::default()
                    },
                ],
            });
        state.set_config(config);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let create = json!({"model":"standard-class","max_tokens":100,"stream":stream,
            "messages":[{"role":"user","content":"first local turn"}],"thread":{"type":"create"}});
        let response = client
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .json(&create)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let _ = response.bytes().await.unwrap();
        let (_, headers) = mock.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(headers["x-api-key"], "first-fixture-key");
        let mut config = (*state.config()).clone();
        let provider = config
            .providers
            .iter_mut()
            .find(|provider| provider.id == "native")
            .unwrap();
        provider.accounts[0].inference_keys[0].priority = 10;
        provider.accounts[0].inference_keys[1].priority = 0;
        state.set_config(config);
        let continued = json!({"model":"standard-class","max_tokens":100,"stream":stream,
            "messages":[{"role":"user","content":"second local turn"}],
            "thread":{"type":"continue","previous_message_id":"native-original"}});
        let response = client
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .json(&continued)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            200,
            "thread must retain its credential despite the attempt cap"
        );
        let _ = response.bytes().await.unwrap();
        let (sent, headers) = mock.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(headers["x-api-key"], "first-fixture-key");
        assert_eq!(sent["messages"], continued["messages"]);
        assert!(sent.get("system").is_none());
        assert!(sent.get("tools").is_none());
        let mut config = (*state.config()).clone();
        config
            .providers
            .iter_mut()
            .find(|provider| provider.id == "native")
            .unwrap()
            .accounts[0]
            .inference_keys[0]
            .enabled = false;
        state.set_config(config);
        let response = client
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .json(&continued)
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success());
        assert_eq!(
            mock.0.lock().unwrap().len(),
            2,
            "a continuation must never cross into the sibling key"
        );
        gateway.stop().await;
        task.abort();
    }
}

#[tokio::test]
async fn invalid_iterations_remain_unknown_and_do_not_rescue_zero_or_top_level_usage() {
    let (state, gateway, _, task) = harness().await;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(RuntimeStore::open(&root.path().join("runtime.db")).unwrap());
    state
        .attach_accounting(store.clone(), &Ledger::new(), "invalid iterations fixture")
        .unwrap();
    for count in [json!(-1), json!("10"), json!(4_294_967_296u64)] {
        let reply = json!({"id":"bad-usage","type":"message","role":"assistant","model":"native","content":[],
            "stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":5,
                "iterations":[{"type":"compaction","input_tokens":count,"output_tokens":1}]}});
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{}/v1/messages", gateway.addr))
            .header("x-api-key", "local-key")
            .json(&json!({"model":"native-native","max_tokens":100,
                "messages":[{"role":"user","content":"fixture"}],"fixture_reply":reply}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            200,
            "accounting must not replay an executed native response"
        );
        assert!(response.headers().get("x-zroutery-cost").is_none());
        let _ = response.bytes().await.unwrap();
    }
    for record in store.usage().unwrap() {
        let attempt = store.usage_attempts(&record.id).unwrap().pop().unwrap();
        assert_eq!(attempt.usage_evidence, Some(UsageEvidence::Invalid));
        assert!(attempt.usage.is_none());
        assert!(attempt.cost.is_none());
        assert_eq!(record.accounting.unwrap().unknown_cost_attempts, 1);
    }
    assert_eq!(state.ledger(), Ledger::new());
    gateway.stop().await;
    task.abort();
}

#[test]
fn per_message_effort_is_used_for_eligibility_without_rewriting_the_cached_top_level() {
    let body = json!({"model":"m","messages":[{"role":"user","content":"first"},
        {"role":"system","content":[],"output_config":{"effort":"low"}},
        {"role":"user","content":"second"}],"output_config":{"effort":"high"}});
    let request = anthropic::decode_request(body.clone()).unwrap();
    assert_eq!(request.reasoning.as_ref().unwrap().effort(), Some("low"));
    let encoded = anthropic::encode_request(&request, "m").unwrap();
    assert_eq!(encoded["output_config"], body["output_config"]);
    assert_eq!(encoded["messages"], body["messages"]);
}

#[tokio::test]
async fn native_token_counting_forwards_the_original_shape_without_inference_or_settlement() {
    let (state, gateway, mock, task) = harness().await;
    let body = json!({"model":"standard-class","system":[{"type":"text","text":"prefix","cache_control":{"type":"ephemeral"}}],
        "messages":[{"role":"assistant","content":[{"type":"compaction","content":"fixture summary","signature":"opaque"}]},
            {"role":"user","content":"next turn"}],"future_counter_setting":true});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!(
            "http://{}/v1/messages/count_tokens?beta=true",
            gateway.addr
        ))
        .header("authorization", "Bearer local-key")
        .header("anthropic-beta", "compact-2026-09-04")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"input_tokens":321,"future_count":{"keep":true}})
    );
    let (sent, headers) = mock.0.lock().unwrap().last().cloned().unwrap();
    let mut expected = body;
    expected["model"] = json!("native");
    assert_eq!(sent, expected);
    assert_eq!(headers["anthropic-beta"], "compact-2026-09-04");
    assert!(state.stats().recent(1).is_empty());
    assert_eq!(state.ledger(), Ledger::new());
    gateway.stop().await;
    task.abort();
}
