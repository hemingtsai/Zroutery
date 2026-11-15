//! Loopback-only Claude Code server review. Tool inputs are data, never executed;
//! real paid-call identity and spend are exercised against the SQLite journal.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Notify;
use zroutery_core::billing::{Cost, Pricing};
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope, Ledger};
use zroutery_core::config::{
    AppConfig, ClassifierCandidate, MemorySecretStore, ModelEntry, ModelTier,
    ProviderClientProfile, ProviderConfig, ProviderKind,
};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone)]
struct Mock {
    calls: Arc<Mutex<Vec<(Value, HeaderMap)>>>,
    mode: &'static str,
    review_started: Arc<Notify>,
    release_review: Arc<Notify>,
}

fn output(mode: &str) -> Value {
    if mode == "text" {
        return json!([{"type":"text", "text":"fixture answer"}]);
    }
    if mode == "client" {
        return json!([
            {"type":"tool_use","id":"call_allowed","name":"Bash","input":{"command":"node -e \"process.stdout.write('ZR_CLIENT_ALLOWED\\n')\"","description":"Print isolated allowed test marker"}},
            {"type":"tool_use","id":"call_denied","name":"Bash","input":{"command":"node -e \"process.stdout.write('ZR_CLIENT_DENIED\\n')\"","description":"Print isolated denied test marker"}}
        ]);
    }
    let first = json!({"type":"tool_use", "id":"call_allowed", "name":"Bash",
        "input":{"command":"echo local safe fixture", "description":"opaque fixture"}});
    let second = json!({"type":"tool_use", "id":"call_denied", "name":"Bash",
        "input":{"command":"fixture: unrequested dangerous action", "description":"not executable"}});
    if mode == "duplicate" {
        return json!([first, {"type":"tool_use", "id":"call_allowed", "name":"Bash", "input":{}}]);
    }
    if mode == "one" {
        return json!([first]);
    }
    json!([first, second])
}

fn api_reply(mode: &str) -> Value {
    let mut reply = json!({"id":"msg_fixture", "type":"message", "role":"assistant", "model":"native-revision",
        "content":output(mode), "stop_reason":if mode == "text" {"end_turn"} else if mode == "cut" {"max_tokens"} else if mode == "unknown_stop" {"future_unconfirmed"} else {"tool_use"},
        "usage":{"input_tokens":10,"output_tokens":4}, "future_field":{"keep":true}});
    if mode == "native_results" {
        reply["safeguard_results"] = json!([{"type":"dangerous_tool_use","status":{"type":"unsupported"}, "native_extension":true}]);
    }
    reply
}

async fn messages(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.calls
        .lock()
        .unwrap()
        .push((body.clone(), headers.clone()));
    if mock.mode == "reject_review" && body.get("safeguards").is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"type":"error","error":{"type":"invalid_request_error",
            "message":"safeguards: Extra inputs are not permitted"}}),
            ),
        )
            .into_response();
    }
    let contains_results = body["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "tool_result"))
        })
    });
    let reply = if mock.mode == "client" && contains_results {
        json!({"id":"msg_finished","type":"message","role":"assistant","model":"native-revision",
            "content":[{"type":"text","text":"Client acceptance finished."}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":4}})
    } else {
        api_reply(mock.mode)
    };
    if body["stream"] != true {
        return Json(reply).into_response();
    }
    let frame = |kind: &str, data: Value| format!("event: {kind}\ndata: {data}\n\n");
    let mut opening = reply.clone();
    opening["content"] = json!([]);
    opening["stop_reason"] = Value::Null;
    opening["usage"]["output_tokens"] = json!(0);
    opening.as_object_mut().unwrap().remove("safeguard_results");
    let mut frames = frame(
        "message_start",
        json!({"type":"message_start", "message":opening}),
    );
    frames += &frame(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"live prefix"}}),
    );
    frames += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    for (index, block) in reply["content"].as_array().unwrap().iter().enumerate() {
        let index = index + 1;
        if block["type"] == "tool_use" {
            let mut start = block.clone();
            start["input"] = json!({});
            frames += &frame(
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":start}),
            );
            let input = if mock.mode == "invalid_arguments" && index == 2 {
                "{broken".to_string()
            } else {
                block["input"].to_string()
            };
            let mid = input.len() / 2;
            for fragment in [&input[..mid], &input[mid..]] {
                frames += &frame(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"input_json_delta","partial_json":fragment}}),
                );
            }
        } else {
            frames += &frame(
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":block}),
            );
        }
        frames += &frame(
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        );
    }
    let mut delta = json!({"type":"message_delta", "delta":{"stop_reason":reply["stop_reason"]}, "usage":reply["usage"]});
    if mock.mode == "native_results" {
        delta["delta"]["safeguard_results"] = reply["safeguard_results"].clone();
    }
    frames += &frame("message_delta", delta);
    frames += &frame(
        "future_progress",
        json!({"type":"future_progress", "kept":true}),
    );
    if mock.mode == "truncated" {
    } else if mock.mode == "tail_error" {
        frames += &frame(
            "error",
            json!({"type":"error","error":{"type":"overloaded_error","message":"fixture wording"}}),
        );
    } else {
        frames += &frame(
            "message_stop",
            json!({"type":"message_stop", "terminal_extension":true}),
        );
    }
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("request-id", "main-request-id")
        .body(axum::body::Body::from(frames))
        .unwrap()
}

async fn chat(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    mock.calls.lock().unwrap().push((body.clone(), headers));
    let tools = output(mock.mode).as_array().unwrap().iter().filter(|block|block["type"] == "tool_use")
        .map(|block| {
            let mut tool = json!({"id":block["id"],"type":"function", "function":{"name":block["name"],"arguments":block["input"].to_string()}});
            if block["id"] == "call_denied" {
                match mock.mode {
                    "missing_arguments" => {tool["function"].as_object_mut().unwrap().remove("arguments");}
                    "empty_arguments" => tool["function"]["arguments"] = json!(""),
                    "null_arguments" => tool["function"]["arguments"] = Value::Null,
                    _ => {}
                }
            }
            tool
        })
        .collect::<Vec<_>>();
    if body["stream"] != true {
        return Json(json!({"id":"chat_fixture", "model":"main", "choices":[{"message":{"role":"assistant","content":"live prefix", "tool_calls":tools},
            "finish_reason":if mock.mode == "text" {"stop"} else {"tool_calls"}}],"usage":{"prompt_tokens":10,"completion_tokens":4}})).into_response();
    }
    let mut frames = format!(
        "data: {}\n\n",
        json!({"id":"chat_fixture","model":"main","choices":[{"index":0,"delta":{"content":"live prefix"}}]})
    );
    for (index, tool) in tools.into_iter().enumerate() {
        let mut tool = tool;
        tool["index"] = json!(index);
        frames += &format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"tool_calls":[tool]}}]})
        );
    }
    frames += &format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
    );
    frames += &format!(
        "data: {}\n\n",
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4}})
    );
    frames += "data: [DONE]\n\n";
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(frames))
        .unwrap()
}

fn reviewer_payload(body: &Value) -> Value {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .find_map(|message| {
            message["content"].as_str().map(str::to_string).or_else(|| {
                message["content"].as_array().and_then(|blocks| {
                    blocks
                        .iter()
                        .find_map(|block| block["text"].as_str().map(str::to_string))
                })
            })
        })
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap()
}

async fn review(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    mock.calls.lock().unwrap().push((body.clone(), headers));
    mock.review_started.notify_one();
    if mock.mode == "wait" || mock.mode == "timeout" {
        mock.release_review.notified().await;
    }
    if mock.mode == "client" {
        let text = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_str())
            .collect::<String>();
        if serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|payload| payload.get("pending_tool_calls").cloned())
            .is_none()
        {
            return (StatusCode::BAD_REQUEST,Json(json!({"error":{"message":"unexpected standalone classifier request in client acceptance"}}))).into_response();
        }
    }
    let payload = reviewer_payload(&body);
    let calls = payload["pending_tool_calls"].as_array().unwrap();
    let mut verdicts = serde_json::Map::new();
    for call in calls {
        if mock.mode == "missing" && call["id"] == "call_denied" {
            continue;
        }
        verdicts.insert(call["id"].as_str().unwrap().into(),json!({"flagged":call["id"] == "call_denied","explanation":"Policy requires explicit user intent."}));
    }
    let content = if mock.mode == "bad_json" {
        "not a verdict".into()
    } else if mock.mode == "duplicate_verdict" {
        r#"{"verdicts":{"call_allowed":{"flagged":false},"call_allowed":{"flagged":true}}}"#.into()
    } else {
        json!({"verdicts":verdicts}).to_string()
    };
    let status = if mock.mode == "http_error" {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let result = json!({"id":"review_fixture", "model":"review-revision", "choices":[{"message":{"role":"assistant","content":content},
        "finish_reason":if mock.mode == "review_truncated" {"length"} else {"stop"}}], "usage":{"prompt_tokens":20,"completion_tokens":5}});
    (
        status,
        [("request-id", "classifier-request-id")],
        Json(result),
    )
        .into_response()
}

async fn responses_review(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    mock.calls.lock().unwrap().push((body.clone(), headers));
    let payload: Value = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user")
        .flat_map(|item| item["content"].as_array().unwrap())
        .find_map(|part| part["text"].as_str())
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap();
    let verdicts: serde_json::Map<String, Value> = payload["pending_tool_calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| {
            (
                call["id"].as_str().unwrap().into(),
                json!({"flagged":false}),
            )
        })
        .collect();
    let mut reply = json!({"id":"responses_reviewer","model":"review-revision","status":"completed",
        "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":json!({"verdicts":verdicts}).to_string()}]}],
        "usage":{"input_tokens":20,"output_tokens":5}});
    match mock.mode {
        "responses_missing_status" => {
            reply.as_object_mut().unwrap().remove("status");
        }
        "responses_in_progress" => reply["status"] = json!("in_progress"),
        "responses_unknown_status" => reply["status"] = json!("future_status"),
        _ => {}
    }
    Json(reply)
}

async fn decide(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    mock.calls.lock().unwrap().push((body.clone(), headers));
    let payload: Value = serde_json::from_str(body["state"].as_str().unwrap()).unwrap();
    let pending = payload["pending_tool_calls"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "a probability must never approve a batch");
    let probability = if mock.mode == "uncertain" {
        0.5
    } else if pending[0]["id"] == "call_denied" {
        0.97
    } else {
        0.01
    };
    Json(
        json!({"model":"jev-revision", "answers":{"decision":{"type":if mock.mode == "wrong_decision_type" {"choice"} else {"noul"},"noul":probability}}, "usage":{"input_tokens":20,"output_tokens":5}}),
    )
}

struct Harness {
    state: Arc<AppState>,
    gateway: ServerHandle,
    mock: Mock,
    task: tokio::task::JoinHandle<()>,
    store: Arc<RuntimeStore>,
    _root: tempfile::TempDir,
}

impl Harness {
    async fn new(mode: &'static str, bridge: bool, decision: bool) -> Self {
        let mock = Mock {
            calls: Default::default(),
            mode,
            review_started: Arc::new(Notify::new()),
            release_review: Arc::new(Notify::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = axum::Router::new()
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(chat))
            .route("/review/v1/chat/completions", post(review))
            .route("/review/v1/responses", post(responses_review))
            .route("/v1/systemone", post(decide))
            .with_state(mock.clone());
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.require_auth = false;
        config.server.bypass_proxy = true;
        config.routing.max_attempts = 1;
        let mut main = ProviderConfig::new(
            "main",
            "Main",
            if bridge {
                ProviderKind::OpenAICompatible
            } else {
                ProviderKind::Anthropic
            },
        );
        main.base_url = format!("http://{addr}");
        main.key_ref.clear();
        main.client_profile = ProviderClientProfile::Native;
        let mut reviewer = ProviderConfig::new(
            "review",
            "Review",
            if decision {
                ProviderKind::Decision
            } else {
                ProviderKind::OpenAICompatible
            },
        );
        reviewer.base_url = if decision {
            format!("http://{addr}")
        } else {
            format!("http://{addr}/review")
        };
        reviewer.key_ref = "review-key-ref".into();
        reviewer.decision.threshold = 0.6;
        config.providers = vec![main, reviewer];
        let mut model = ModelEntry::for_upstream("main", "main", Some(ModelTier::Standard));
        model.capabilities.tools = true;
        model.capabilities.thinking = true;
        model.capabilities.structured_output = true;
        model.effort_levels = vec!["low".into(), "medium".into(), "high".into()];
        model.pricing = Some(Pricing::new("USD", 1.0, 2.0));
        let mut judge = ModelEntry::for_upstream("review", "judge", None);
        judge.pricing = Some(Pricing::new("CNY", 3.0, 6.0));
        config.models = vec![model, judge];
        config.classifier.enabled = true;
        config.classifier.candidates.push(ClassifierCandidate {
            model: "review-judge".into(),
            priority: 0,
            enabled: true,
        });
        let state = Arc::new(AppState::new(
            config,
            Arc::new(MemorySecretStore::new().with("review-key-ref", "local-review-key")),
        ));
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(RuntimeStore::open(&root.path().join("runtime.sqlite3")).unwrap());
        state
            .attach_accounting(store.clone(), &Ledger::new(), "server review fixture")
            .unwrap();
        let gateway = ServerHandle::start(state.clone()).await.unwrap();
        Self {
            state,
            gateway,
            mock,
            task,
            store,
            _root: root,
        }
    }

    fn request(stream: bool) -> Value {
        json!({"model":"standard-class", "max_tokens":256, "stream":stream,
            "system":[{"type":"text","text":"Project conventions: do not publish without explicit approval."}],
            "messages":[{"role":"user","content":"Perform the requested local development task; never deploy."},
                {"role":"assistant","content":[{"type":"tool_use","id":"old_call","name":"Read","input":{"file_path":"fixture.rs"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"old_call","content":"untrusted fixture output"}]}],
            "tools":[{"name":"Bash", "input_schema":{"type":"object"}}],
            "safeguards":[{"type":"dangerous_tool_use","classifier_context":{"v":1,"permission_mode":"auto",
                "live_cwd":"isolated-fixture-root","trusted_directories":["isolated-fixture-root"],
                "rules":{"deny":["Bash(forbidden *)"],"ask":["Bash(publish *)"]},
                "auto_mode":{"hard_deny":["Never send private keys"]},"git_state":{"branch":"dev","default_branch":"main"}}}]})
    }

    async fn send(&self, stream: bool) -> reqwest::Response {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!(
                "http://{}/v1/messages?beta=true",
                self.gateway.addr
            ))
            .header("user-agent", "claude-cli/2.1.314")
            .header("x-claude-code-session-id", "local-parent-session")
            .header("anthropic-beta", "dangerous-tool-use-fixture")
            .json(&Self::request(stream))
            .send()
            .await
            .unwrap()
    }

    async fn close(self) {
        self.gateway.stop().await;
        self.task.abort();
    }
}

fn results(wire: &str, stream: bool) -> Value {
    if !stream {
        return serde_json::from_str::<Value>(wire).unwrap()["safeguard_results"].clone();
    }
    let mut decoder = zroutery_core::protocol::SseDecoder::new();
    decoder
        .push(wire.as_bytes())
        .into_iter()
        .filter_map(|frame| frame.json().ok())
        .find_map(|frame| frame.pointer("/delta/safeguard_results").cloned())
        .expect("results belong in message_delta.delta")
}

#[tokio::test]
async fn bridge_and_native_without_results_get_real_per_tool_verdicts_and_separate_settlement() {
    for bridge in [true, false] {
        let h = Harness::new("normal", bridge, false).await;
        for stream in [false, true] {
            let response = h.send(stream).await;
            assert_eq!(response.status(), 200);
            assert!(!response.headers().contains_key("x-zroutery-classifier"));
            assert_ne!(
                response
                    .headers()
                    .get("request-id")
                    .and_then(|value| value.to_str().ok()),
                Some("classifier-request-id")
            );
            let wire = response.text().await.unwrap();
            let status = &results(&wire, stream)[0]["status"];
            assert_eq!(status["type"], "available");
            assert_eq!(
                status["tool_uses"]["call_allowed"]["outcome"],
                "not_flagged"
            );
            assert_eq!(status["tool_uses"]["call_denied"]["outcome"], "flagged");
            if stream {
                assert!(wire.contains("live prefix"));
                assert_eq!(wire.matches("event: message_stop").count(), 1);
            }
        }
        let calls = h.mock.calls.lock().unwrap().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|(body, _)| body["model"] == "main")
                .count(),
            2
        );
        let reviews: Vec<_> = calls
            .iter()
            .filter(|(body, _)| body["model"] == "judge")
            .collect();
        assert_eq!(reviews.len(), 2);
        for (body, headers) in reviews {
            assert_eq!(headers["authorization"], "Bearer local-review-key");
            assert!(!headers.contains_key("x-claude-code-session-id"));
            assert!(body.get("safeguards").is_none());
            assert!(body.get("tools").is_none());
            let payload = reviewer_payload(body);
            assert_eq!(
                payload["pending_tool_calls"][0]["input"],
                output("normal")[0]["input"]
            );
            assert_eq!(
                payload["client_environment"],
                Harness::request(false)["safeguards"][0]["classifier_context"]
            );
            assert!(payload["conversation"].to_string().contains("never deploy"));
            assert!(payload["conversation"]
                .to_string()
                .contains("untrusted fixture output"));
        }
        let records = h.store.usage().unwrap();
        assert_eq!(records.len(), 4);
        for record in records.iter().filter(|record| !record.kind.is_main()) {
            let attempt = h.store.usage_attempts(&record.id).unwrap().pop().unwrap();
            assert!(records
                .iter()
                .any(|main| main.kind.is_main()
                    && Some(&main.id) == attempt.parent_request_id.as_ref()));
            assert_eq!(attempt.execution.upstream_model, "judge");
            assert_eq!(attempt.cost.unwrap().currency, "CNY");
            assert!(!serde_json::to_string(&attempt.execution)
                .unwrap()
                .contains("untrusted fixture output"));
        }
        let mut expected = Ledger::new();
        expected.charge(
            chrono::Local::now(),
            "main",
            Some(ModelTier::Standard),
            &Cost {
                currency: "USD".into(),
                amount: 36e-6,
            },
        );
        expected.charge(
            chrono::Local::now(),
            "review",
            None,
            &Cost {
                currency: "CNY".into(),
                amount: 180e-6,
            },
        );
        let expected = serde_json::to_value(expected).unwrap();
        let actual = serde_json::to_value(h.state.ledger()).unwrap();
        for (key, amount) in expected["entries"].as_object().unwrap() {
            assert!(
                (actual["entries"][key].as_f64().unwrap() - amount.as_f64().unwrap()).abs() < 1e-12,
                "currency/scoped spend must match every send"
            );
        }
        for record in records {
            assert!(record.ok);
        }
        let before = h.state.ledger();
        h.state.retry_accounting().unwrap();
        assert_eq!(
            h.state.ledger(),
            before,
            "all real sends settle exactly once"
        );
        h.close().await;
    }
}

#[tokio::test]
async fn text_only_and_native_results_do_not_call_the_classifier() {
    for mode in ["text", "native_results"] {
        let h = Harness::new(mode, false, false).await;
        for stream in [false, true] {
            let wire = h.send(stream).await.text().await.unwrap();
            let result = results(&wire, stream);
            if mode == "text" {
                assert_eq!(result[0]["status"]["tool_uses"], json!({}));
            } else {
                assert_eq!(result, api_reply(mode)["safeguard_results"]);
            }
        }
        assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
        h.close().await;
    }
}

#[tokio::test]
async fn failure_missing_and_duplicate_verdicts_never_become_an_allow_or_replay_main() {
    for mode in [
        "bad_json",
        "duplicate_verdict",
        "http_error",
        "missing",
        "review_truncated",
    ] {
        let h = Harness::new(mode, false, false).await;
        let wire = h.send(true).await.text().await.unwrap();
        let result = results(&wire, true);
        let uses = &result[0]["status"]["tool_uses"];
        assert_eq!(uses["call_denied"]["type"], "unavailable");
        assert_eq!(
            uses["call_allowed"]["type"],
            if mode == "missing" {
                "evaluated"
            } else {
                "unavailable"
            }
        );
        {
            let calls = h.mock.calls.lock().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|(body, _)| body["model"] == "main")
                    .count(),
                1
            );
        }
        h.close().await;
    }
}

#[tokio::test]
async fn systemone_judges_each_action_and_uncertain_probability_is_unavailable() {
    for mode in ["normal", "uncertain", "wrong_decision_type"] {
        let h = Harness::new(mode, true, true).await;
        let wire = h.send(false).await.text().await.unwrap();
        let result = results(&wire, false);
        if mode == "normal" {
            assert_eq!(
                result[0]["status"]["tool_uses"]["call_allowed"]["outcome"],
                "not_flagged"
            );
            assert_eq!(
                result[0]["status"]["tool_uses"]["call_denied"]["outcome"],
                "flagged"
            );
        } else {
            assert_eq!(
                result[0]["status"]["tool_uses"]["call_allowed"]["type"],
                "unavailable"
            );
            assert_eq!(
                result[0]["status"]["tool_uses"]["call_denied"]["type"],
                "unavailable"
            );
        }
        {
            let calls = h.mock.calls.lock().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|(body, _)| body.get("questions").is_some())
                    .count(),
                2
            );
        }
        assert_eq!(h.store.usage().unwrap().len(), 3);
        h.close().await;
    }
}

#[tokio::test]
async fn partial_or_reused_tool_inputs_cannot_receive_approval() {
    for mode in ["cut", "invalid_arguments", "duplicate", "unknown_stop"] {
        let h = Harness::new(mode, false, false).await;
        let wire = h.send(true).await.text().await.unwrap();
        let uses = results(&wire, true)[0]["status"]["tool_uses"].clone();
        let invalid_id = if mode == "duplicate" {
            "call_allowed"
        } else {
            "call_denied"
        };
        assert_eq!(uses[invalid_id]["type"], "unavailable");
        if mode == "unknown_stop" {
            assert_eq!(uses["call_allowed"]["type"], "unavailable");
            assert_eq!(
                h.mock.calls.lock().unwrap().len(),
                1,
                "unknown termination cannot acquire invented review approval"
            );
        }
        for (body, _) in h
            .mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(body, _)| body["model"] == "judge")
        {
            assert!(!reviewer_payload(body)["pending_tool_calls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|call| call["id"] == invalid_id));
        }
        h.close().await;
    }
}

#[tokio::test]
async fn truncated_or_error_tail_never_runs_review_or_manufactures_message_stop() {
    for mode in ["truncated", "tail_error"] {
        let h = Harness::new(mode, false, false).await;
        let wire = h.send(true).await.text().await.unwrap();
        assert!(wire.contains("live prefix"));
        assert!(wire.contains("event: error"));
        assert!(!wire.contains("safeguard_results"));
        assert!(!wire.contains("event: message_stop"));
        assert_eq!(h.mock.calls.lock().unwrap().len(), 1);
        assert!(!h.store.usage().unwrap()[0].ok);
        h.close().await;
    }
}

#[tokio::test]
async fn review_wait_keeps_text_live_and_finishes_only_after_the_actual_verdict() {
    let h = Harness::new("wait", false, false).await;
    let response = h.send(true).await;
    let mut bytes = response.bytes_stream();
    let mut wire = String::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let chunk = bytes.next().await.unwrap().unwrap();
            wire.push_str(std::str::from_utf8(&chunk).unwrap());
            if wire.contains("live prefix") {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(!wire.contains("message_stop"));
    let chunk = tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let chunk = bytes.next().await.unwrap().unwrap();
            wire.push_str(std::str::from_utf8(&chunk).unwrap());
            if wire.contains("event: ping") {
                return chunk;
            }
        }
    })
    .await
    .unwrap();
    assert!(!chunk.is_empty());
    assert!(!wire.contains("event: message_stop"));
    h.mock.release_review.notify_one();
    while let Some(chunk) = bytes.next().await {
        wire.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
    }
    assert_eq!(
        results(&wire, true)[0]["status"]["tool_uses"]["call_allowed"]["outcome"],
        "not_flagged"
    );
    assert_eq!(
        h.mock.calls.lock().unwrap().len(),
        2,
        "a ping must not restart the reviewer"
    );
    h.close().await;
}

#[tokio::test]
async fn total_review_timeout_returns_unavailable_without_failing_or_replaying_generation() {
    let h = Harness::new("timeout", false, false).await;
    let mut config = (*h.state.config()).clone();
    config.classifier.server_review_timeout_secs = 1;
    h.state.set_config(config);
    let wire = tokio::time::timeout(Duration::from_secs(5), async {
        h.send(true).await.text().await.unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        results(&wire, true)[0]["status"]["tool_uses"]["call_allowed"],
        json!({"type":"unavailable","reason":"timeout"})
    );
    assert_eq!(wire.matches("event: message_stop").count(), 1);
    assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
    assert!(h
        .store
        .usage()
        .unwrap()
        .iter()
        .any(|record| record.kind.is_main() && record.ok));
    h.close().await;
}

#[tokio::test]
async fn shared_global_budget_admits_main_and_review_once_and_includes_main_pending_spend() {
    for limit in [1.0, 10e-6] {
        let h = Harness::new("one", true, false).await;
        let mut config = (*h.state.config()).clone();
        config.budgets.push(Budget::new(
            BudgetScope::Global,
            BudgetPeriod::Day,
            "USD",
            limit,
        ));
        config.budgets.push(Budget::new(
            BudgetScope::Global,
            BudgetPeriod::Day,
            "CNY",
            1.0,
        ));
        h.state.set_config(config);
        let wire = tokio::time::timeout(Duration::from_secs(5), async {
            h.send(false).await.text().await.unwrap()
        })
        .await
        .unwrap();
        let result = results(&wire, false);
        assert_eq!(
            result[0]["status"]["type"],
            if limit < 1.0 {
                "unsupported"
            } else {
                "available"
            }
        );
        assert_eq!(
            h.mock.calls.lock().unwrap().len(),
            if limit < 1.0 { 1 } else { 2 }
        );
        h.close().await;
    }
}

#[tokio::test]
async fn explicit_switch_off_and_unknown_context_preserve_native_fallback_without_approval() {
    let h = Harness::new("one", true, false).await;
    let mut config = (*h.state.config()).clone();
    config.classifier.server_review = false;
    h.state.set_config(config);
    let wire = h.send(false).await.text().await.unwrap();
    assert!(serde_json::from_str::<Value>(&wire)
        .unwrap()
        .get("safeguard_results")
        .is_none());
    assert_eq!(h.mock.calls.lock().unwrap().len(), 1);
    let mut config = (*h.state.config()).clone();
    config.classifier.server_review = true;
    h.state.set_config(config);
    let mut request = Harness::request(false);
    request["safeguards"][0]["classifier_context"]["v"] = json!(999);
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/messages", h.gateway.addr))
        .json(&request)
        .send()
        .await
        .unwrap();
    let result = response.json::<Value>().await.unwrap();
    assert_eq!(
        result["safeguard_results"][0]["status"]["type"],
        "unsupported"
    );
    assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
    let mut request = Harness::request(false);
    request["messages"][0]["content"] = json!("constraint ".repeat(60_000));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/messages", h.gateway.addr))
        .json(&request)
        .send()
        .await
        .unwrap();
    let reply = response.json::<Value>().await.unwrap();
    assert_eq!(
        reply["safeguard_results"][0]["status"]["tool_uses"]["call_allowed"],
        json!({"type":"unavailable","reason":"input_too_long"})
    );
    assert_eq!(
        h.mock.calls.lock().unwrap().len(),
        3,
        "an overlong security transcript is not silently clipped and approved"
    );
    h.close().await;
}

#[tokio::test]
async fn disconnect_while_review_is_waiting_cancels_its_attempt_and_never_replays() {
    let h = Harness::new("wait", false, false).await;
    let response = h.send(true).await;
    let mut bytes = response.bytes_stream();
    let prefix = tokio::time::timeout(Duration::from_secs(2), bytes.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!prefix.is_empty());
    tokio::time::timeout(Duration::from_secs(2), h.mock.review_started.notified())
        .await
        .unwrap();
    drop(bytes);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if h.store.usage().unwrap().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
    let records = h.store.usage().unwrap();
    assert!(records.iter().all(|record| !record.ok));
    let review = records
        .iter()
        .find(|record| !record.kind.is_main())
        .unwrap();
    let attempt = h.store.usage_attempts(&review.id).unwrap().pop().unwrap();
    assert_eq!(
        attempt.state,
        zroutery_core::accounting::AttemptState::Cancelled
    );
    assert!(
        attempt.cost.is_none(),
        "unreported usage remains unknown on a cancelled review"
    );
    h.close().await;
}

#[tokio::test]
async fn native_extension_rejection_recovers_without_changing_history_or_other_betas() {
    let h = Harness::new("reject_review", false, false).await;
    for stream in [false, true] {
        let wire = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{}/v1/messages", h.gateway.addr))
            .header(
                "anthropic-beta",
                "ordinary-native-fixture,dangerous-tool-use-fixture",
            )
            .json(&Harness::request(stream))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            results(&wire, stream)[0]["status"]["tool_uses"]["call_allowed"]["outcome"],
            "not_flagged"
        );
    }
    let calls = h.mock.calls.lock().unwrap().clone();
    let main: Vec<_> = calls
        .iter()
        .filter(|(body, _)| body["model"] == "main")
        .collect();
    assert_eq!(
        main.len(),
        4,
        "only the explicit pre-output rejection may be retried"
    );
    for pair in main.as_chunks::<2>().0 {
        assert!(pair[0].0.get("safeguards").is_some());
        assert!(pair[1].0.get("safeguards").is_none());
        assert_eq!(
            pair[1].1["anthropic-beta"], "ordinary-native-fixture",
            "recover only the rejected review beta pair"
        );
        assert_eq!(pair[0].0["messages"], pair[1].0["messages"]);
        assert_eq!(pair[0].0["system"], pair[1].0["system"]);
        assert_eq!(pair[0].0["tools"], pair[1].0["tools"]);
    }
    assert_eq!(
        h.store
            .usage()
            .unwrap()
            .iter()
            .filter(|record| record.kind.is_main())
            .count(),
        2
    );
    h.close().await;
}

#[tokio::test]
async fn chat_bridge_never_approves_missing_empty_or_null_arguments() {
    for mode in ["missing_arguments", "empty_arguments", "null_arguments"] {
        let h = Harness::new(mode, true, false).await;
        for stream in [false, true] {
            let response = h.send(stream).await;
            assert_eq!(response.status(), 200);
            let uses =
                results(&response.text().await.unwrap(), stream)[0]["status"]["tool_uses"].clone();
            assert_eq!(uses["call_allowed"]["outcome"], "not_flagged");
            assert_eq!(
                uses["call_denied"],
                json!({"type":"unavailable","reason":"truncated"}),
                "a bridge must not invent the unreported argument object"
            );
        }
        for (body, _) in h
            .mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(body, _)| body["model"] == "judge")
        {
            assert!(!reviewer_payload(body)["pending_tool_calls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|call| call["id"] == "call_denied"));
        }
        h.close().await;
    }
}

#[tokio::test]
async fn responses_reviewer_requires_a_completed_wire_status() {
    use zroutery_core::config::ApiSurface;
    for mode in [
        "normal",
        "responses_missing_status",
        "responses_in_progress",
        "responses_unknown_status",
    ] {
        let h = Harness::new(mode, true, false).await;
        let mut config = (*h.state.config()).clone();
        config
            .providers
            .iter_mut()
            .find(|provider| provider.id == "review")
            .unwrap()
            .api_surface = ApiSurface::Responses;
        h.state.set_config(config);
        let wire = h.send(false).await.text().await.unwrap();
        let uses = results(&wire, false)[0]["status"]["tool_uses"].clone();
        if mode == "normal" {
            assert_eq!(uses["call_allowed"]["outcome"], "not_flagged");
        } else {
            assert_eq!(uses["call_allowed"]["type"], "unavailable");
            assert_eq!(uses["call_denied"]["type"], "unavailable");
        }
        assert_eq!(
            h.mock.calls.lock().unwrap().len(),
            2,
            "a reviewer status failure cannot replay main generation"
        );
        h.close().await;
    }
}

#[tokio::test]
async fn an_exact_decision_model_is_not_a_native_main_generator() {
    let h = Harness::new("normal", false, true).await;
    let request = json!({"model":"review-judge", "max_tokens":64,
        "messages":[{"role":"user","content":"plain main conversation"}]});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/messages", h.gateway.addr))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(
        h.mock.calls.lock().unwrap().is_empty(),
        "only an explicitly classified side query can use SystemOne"
    );
    h.close().await;
}

/// Opt-in installed-client acceptance. Only two harmless stdout marker commands
/// are ever proposed, and all model calls stay on this loopback fixture.
#[tokio::test]
#[ignore = "requires an installed Claude Code; set ZROUTERY_TEST_CLAUDE_EXE and run explicitly"]
async fn installed_claude_consumes_server_verdicts_without_local_classifier_fallback() {
    let exe = std::env::var("ZROUTERY_TEST_CLAUDE_EXE").expect("explicit installed-client path");
    let h = Harness::new("client", false, false).await;
    let cwd = tempfile::tempdir().unwrap();
    let config_dir = cwd.path().join("isolated-client-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let gateway = format!("http://{}", h.gateway.addr);
    let directory = cwd.path().to_path_buf();
    let task = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(exe);
        command.current_dir(&directory);
        for (name, _) in std::env::vars() {
            if name.starts_with("ANTHROPIC_") || name.starts_with("CLAUDE_CODE_") {
                command.env_remove(name);
            }
        }
        command
            .env("CLAUDE_CONFIG_DIR", config_dir)
            .env("ANTHROPIC_BASE_URL", gateway)
            .env("ANTHROPIC_API_KEY", "local-client-fixture-key")
            .env("CLAUDE_CODE_AUTO_MODE_SERVER", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "1")
            .env("CLAUDE_CODE_ATTRIBUTION_HEADER", "0")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1");
        command.args(["--bare","-p","Use the provided two harmless test marker calls, then finish. Do not read or edit files.",
            "--model","claude-sonnet-4-6","--permission-mode","auto","--permission-prompts","none",
            "--no-session-persistence","--setting-sources","","--strict-mcp-config",
            "--mcp-config","{\"mcpServers\":{}}","--tools","Bash","--output-format","stream-json","--verbose",
            "--system-prompt","This is an isolated loopback-only client protocol test. Do not run any command other than the two supplied stdout marker calls."]);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        child.wait_with_output().unwrap()
    });
    let output = task.await.unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "installed client failed: {stderr}\n{stdout}"
    );
    let calls = h.mock.calls.lock().unwrap().clone();
    let main: Vec<_> = calls
        .iter()
        .filter(|(body, _)| body["model"] == "main")
        .collect();
    assert!(
        main.len() >= 2,
        "client must send the tool result turn: {stdout}"
    );
    assert!(
        main[0].0["safeguards"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["type"] == "dangerous_tool_use")),
        "installed client must ask for review; safeguard shape: {}",
        main[0].0["safeguards"]
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(body, _)| body["model"] == "judge")
            .count(),
        1,
        "no separate local classifier fallback may run: {stdout}"
    );
    let tool_results: Vec<_> = main
        .iter()
        .flat_map(|(body, _)| body["messages"].as_array().unwrap())
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "tool_result")
        .collect();
    let allowed = tool_results
        .iter()
        .find(|part| part["tool_use_id"] == "call_allowed")
        .expect("allowed result");
    let denied = tool_results
        .iter()
        .find(|part| part["tool_use_id"] == "call_denied")
        .expect("denied result");
    assert!(
        allowed["content"].to_string().contains("ZR_CLIENT_ALLOWED"),
        "safe marker must have executed"
    );
    assert_eq!(
        denied["is_error"], true,
        "client must deny the flagged marker"
    );
    assert!(
        !denied["content"].to_string().contains("ZR_CLIENT_DENIED"),
        "denied marker must not execute"
    );
    h.close().await;
}
