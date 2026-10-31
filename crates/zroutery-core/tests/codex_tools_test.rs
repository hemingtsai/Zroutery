use serde_json::{json, Value};
use zroutery_core::ir::Capability;
use zroutery_core::ir::{StopReason, StreamEvent, Usage};
use zroutery_core::protocol::{self, responses, StreamEncoder};

fn body() -> Value {
    json!({"model":"mock","parallel_tool_calls":false,"input":[
        {"role":"user","content":"run a command"},
        {"type":"additional_tools","role":"developer","tools":[
            {"type":"namespace","name":"functions","description":"tools","tools":[
                {"type":"function","name":"read","strict":false,"parameters":{"type":"object","properties":{"path":{"type":"string"}}}},
                {"type":"custom","name":"exec","description":"Execute a command","format":{"type":"text"}}
            ]}
        ]}
    ]})
}

#[test]
fn codex_additional_tools_are_registered_not_dropped_and_native_wire_is_preserved() {
    let request = responses::decode_request(body()).unwrap();
    assert_eq!(request.tools.len(), 2);
    assert_eq!(request.tools[0].name, "functions__read");
    assert_eq!(request.tools[1].name, "functions__exec");
    assert!(request.required_capabilities.contains(&Capability::Tools));
    let native = responses::encode_request(&request, "mock").unwrap();
    assert_eq!(native["input"], body()["input"]);
    let chat = protocol::openai::encode_request(&request, "mock").unwrap();
    assert_eq!(chat["tools"][0]["function"]["strict"], false);
    assert_eq!(
        chat["tools"][1]["function"]["parameters"]["required"],
        json!(["input"])
    );
    assert_eq!(chat["parallel_tool_calls"], false);
}

#[test]
fn custom_tool_history_and_namespace_identity_survive_both_directions() {
    let mut body = body();
    body["input"].as_array_mut().unwrap().extend([
        json!({"type":"custom_tool_call","call_id":"pair","namespace":"functions","name":"exec","input":"echo hello"}),
        json!({"type":"custom_tool_call_output","call_id":"pair","output":"hello"})]);
    let req = responses::decode_request(body.clone()).unwrap();
    let chat = protocol::openai::encode_request(&req, "mock").unwrap();
    let messages = chat["messages"].as_array().unwrap();
    assert!(messages
        .iter()
        .any(|m| m["tool_calls"][0]["function"]["name"] == "functions__exec"));
    let mut response = json!({"output":[{"type":"function_call","call_id":"pair2","name":"functions__exec","arguments":"{\"input\":\"echo bye\"}"}]});
    responses::restore_response_tools(&mut response, &req);
    assert_eq!(response["output"][0]["type"], "custom_tool_call");
    assert_eq!(response["output"][0]["namespace"], "functions");
    assert_eq!(response["output"][0]["name"], "exec");
    assert_eq!(response["output"][0]["input"], "echo bye");
    assert_eq!(
        responses::encode_request(&req, "mock").unwrap()["input"],
        body["input"]
    );
}

#[test]
fn staged_custom_stream_emits_only_raw_input_not_bridge_json() {
    let req = responses::decode_request(body()).unwrap();
    let mut enc = responses::ResponsesStreamEncoder::new("mock");
    let mut frames = Vec::new();
    for event in [
        StreamEvent::ToolUseStart {
            index: 0,
            id: "call".into(),
            name: "functions__exec".into(),
        },
        StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "{\"input\":\"echo".into(),
        },
        StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: " hello\"}".into(),
        },
        StreamEvent::BlockStop { index: 0 },
        StreamEvent::Stop {
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ] {
        frames.extend(enc.encode(&event));
    }
    responses::restore_stream_frames(&mut frames, &req);
    let values: Vec<Value> = frames.iter().map(|f| f.json().unwrap()).collect();
    let deltas: Vec<_> = values
        .iter()
        .filter(|v| v["type"] == "response.custom_tool_call_input.delta")
        .collect();
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0]["delta"], "echo hello");
    assert!(!values
        .iter()
        .any(|v| v["type"] == "response.function_call_arguments.delta"));
    let done = values
        .iter()
        .find(|v| v["type"] == "response.output_item.done")
        .unwrap();
    assert_eq!(done["item"]["input"], "echo hello");
}

#[test]
fn malformed_catalogs_unknown_items_and_alias_collisions_still_fail_closed() {
    for item in [
        json!({"type":"additional_tools","role":"user","tools":[]}),
        json!({"type":"additional_tools","role":"developer"}),
        json!({"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions"}]}),
        json!({"type":"computer_call"}),
    ] {
        assert!(responses::decode_request(json!({"model":"mock","input":[item]})).is_err());
    }
    let collision = json!({"model":"mock","tools":[{"type":"function","name":"functions__read"}],"input":body()["input"]});
    assert!(responses::decode_request(collision).is_err());
}

#[test]
fn explicit_image_remediation_updates_native_wire_without_rewriting_history() {
    let body = json!({"model":"mock","input":[{"type":"message","role":"user","id":"keep",
        "content":[{"type":"input_image","image_url":"https://example.com/image.png"}]}]});
    let mut req = responses::decode_request(body).unwrap();
    let slot = zroutery_core::media::collect::collect(&req)[0].0.clone();
    zroutery_core::media::transform::replace(
        &mut req,
        &slot,
        &zroutery_core::media::transform::Replacement::Placeholder("image unavailable".into()),
    );
    let wire = responses::encode_request(&req, "mock").unwrap();
    assert_eq!(wire["input"][0]["id"], "keep");
    assert_eq!(wire["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(wire["input"][0]["content"][0]["text"], "image unavailable");
}
