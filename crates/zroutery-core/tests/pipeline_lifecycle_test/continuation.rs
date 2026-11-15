use serde_json::{json, Value};
use zroutery_core::failure::FailureClass;
use zroutery_core::ir::{StoredResponse, Usage};

use super::support::*;

// --------------------------------------------------- previous_response_id continuation

impl Harness {
    /// `POST /v1/responses` with an explicit body, so a test can name the
    /// `previous_response_id` it continues and the `input` shape it sends.
    async fn post_responses(&self, body: Value) -> reqwest::Response {
        self.post("/v1/responses").json(&body).send().await.unwrap()
    }

    /// A Responses body carrying one user turn.
    fn turn(input: &str, previous: Option<&str>, store: Option<bool>) -> Value {
        let mut body = json!({
            "model": "alpha-good-model",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": input}
            ]}],
        });
        if let Some(previous) = previous {
            body["previous_response_id"] = json!(previous);
        }
        if let Some(store) = store {
            body["store"] = json!(store);
        }
        body
    }

    /// The id a buffered Responses answer was stored under.
    async fn stored_turn(&self, body: Value) -> String {
        let response = self.post_responses(body).await;
        assert_eq!(response.status(), 200);
        let wire: Value = response.json().await.unwrap();
        wire["id"]
            .as_str()
            .expect("the answer names its id")
            .to_string()
    }
}

fn chain_response(index: usize) -> StoredResponse {
    StoredResponse::completed(
        format!("resp-chain-{index}"),
        "alpha-good-model".to_string(),
        vec![json!({"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": format!("CHAIN_{index}")}
        ]})],
        vec![json!({"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": format!("ANSWER_{index}")}
        ]})],
        Usage::default(),
        (index > 0).then(|| format!("resp-chain-{}", index - 1)),
        None,
    )
}

/// A follow-up turn replays the prior conversation into the upstream request:
/// the earlier user turn and the earlier assistant answer both reach the model
/// ahead of the new input. Before the fix the prior id was only stored as
/// metadata, so the model saw the new turn alone.
#[tokio::test]
async fn a_continuation_sends_the_prior_turn_to_the_model() {
    let h = Harness::new().await;

    let first = h
        .stored_turn(Harness::turn("RETAIN_PREVIOUS_CONTEXT", None, None))
        .await;
    let second = h
        .post_responses(Harness::turn("ONLY_NEW_TURN", Some(&first), None))
        .await;
    assert_eq!(second.status(), 200);
    let _ = second.text().await.unwrap();

    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 2, "two upstream sends");
    let sent = serde_json::to_string(&bodies[1]).unwrap();
    assert!(
        sent.contains("RETAIN_PREVIOUS_CONTEXT"),
        "the prior user turn reaches the model: {sent}"
    );
    assert!(
        sent.contains("hello from mock"),
        "the prior assistant answer reaches the model: {sent}"
    );
    assert!(
        sent.contains("ONLY_NEW_TURN"),
        "the new turn reaches the model: {sent}"
    );
    let prior = sent.find("RETAIN_PREVIOUS_CONTEXT").unwrap();
    let answer = sent.find("hello from mock").unwrap();
    let new_turn = sent.find("ONLY_NEW_TURN").unwrap();
    assert!(
        prior < answer && answer < new_turn,
        "the conversation is replayed in order: {sent}"
    );

    h.shutdown().await;
}

/// A chain of continuations replays every prior turn, oldest first, and stores
/// each request's own input rather than the expanded history, so the chain
/// cannot duplicate itself.
#[tokio::test]
async fn a_multi_turn_continuation_replays_the_whole_chain() {
    let h = Harness::new().await;

    let first = h.stored_turn(Harness::turn("TURN_ONE", None, None)).await;
    let second = h
        .stored_turn(Harness::turn("TURN_TWO", Some(&first), None))
        .await;
    let third = h
        .stored_turn(Harness::turn("TURN_THREE", Some(&second), None))
        .await;

    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 3);
    let sent = serde_json::to_string(&bodies[2]).unwrap();
    for marker in ["TURN_ONE", "TURN_TWO", "TURN_THREE"] {
        assert!(sent.contains(marker), "{marker} is replayed: {sent}");
        assert_eq!(
            sent.matches(marker).count(),
            1,
            "{marker} is replayed exactly once: {sent}"
        );
    }

    // The stored input for the newest turn is that turn's own input, not the
    // replayed history, so continuing it stays linear.
    let stored = h
        .state
        .response_store
        .get(&third)
        .expect("the third turn was stored");
    assert_eq!(stored.input.len(), 1, "{:?}", stored.input);
    let stored_input = serde_json::to_string(&stored.input).unwrap();
    assert!(stored_input.contains("TURN_THREE"));
    assert!(!stored_input.contains("TURN_ONE"), "{stored_input}");
    assert_eq!(
        stored.previous_response_id.as_deref(),
        Some(second.as_str())
    );

    h.shutdown().await;
}

/// A stored turn ending in a `function_call` is replayed with its matching
/// `function_call_output`, so a tool loop continues instead of breaking.
#[tokio::test]
async fn a_tool_call_and_its_output_are_replayed_in_a_continuation() {
    let h = Harness::new().await;

    let prior = StoredResponse::completed(
        "resp-prior-tool".to_string(),
        "alpha-good-model".to_string(),
        vec![json!({"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": "WEATHER_QUESTION"}
        ]})],
        vec![json!({"type": "function_call", "call_id": "call_1",
                    "name": "get_weather", "arguments": "{\"city\":\"SH\"}"})],
        Usage::default(),
        None,
        None,
    );
    h.state.response_store.put(prior);

    let response = h
        .post_responses(json!({
            "model": "alpha-good-model",
            "previous_response_id": "resp-prior-tool",
            "input": [{"type": "function_call_output", "call_id": "call_1", "output": "sunny"}],
        }))
        .await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();

    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 1);
    let sent = &bodies[0];
    let wire = serde_json::to_string(sent).unwrap();
    assert!(
        wire.contains("WEATHER_QUESTION") && wire.contains("get_weather"),
        "the prior tool call is replayed: {wire}"
    );
    assert!(
        wire.contains("sunny"),
        "the tool result is replayed as its own turn: {wire}"
    );
    let messages = sent["messages"].as_array().expect("messages");
    assert!(
        messages
            .iter()
            .any(|m| m["tool_calls"].as_array().is_some_and(|c| !c.is_empty())),
        "the assistant tool call survives the round trip: {wire}"
    );
    assert!(
        messages.iter().any(|m| m["role"] == "tool"),
        "the tool output reaches the model as a tool turn: {wire}"
    );

    h.shutdown().await;
}

/// A prior id that is unknown, deleted, or was never retained cannot be
/// continued: the request is refused and no provider is contacted, rather than
/// silently answering with half a conversation.
#[tokio::test]
async fn a_missing_or_deleted_prior_is_refused_not_ignored() {
    let h = Harness::new().await;

    let first = h.stored_turn(Harness::turn("DELETE_ME", None, None)).await;
    assert_eq!(
        h.delete_response(&first).send().await.unwrap().status(),
        200
    );

    let deleted = h
        .post_responses(Harness::turn("AFTER_DELETE", Some(&first), None))
        .await;
    assert_eq!(deleted.status(), 400, "a deleted prior is refused");
    assert_eq!(h.mock.count(), 1, "nothing was sent for the refused turn");

    let unknown = h
        .post_responses(Harness::turn(
            "AFTER_UNKNOWN",
            Some("resp-does-not-exist"),
            None,
        ))
        .await;
    assert_eq!(unknown.status(), 400, "an unknown prior is refused");
    assert_eq!(h.mock.count(), 1, "nothing was sent for the refused turn");

    wait_for_outcomes(&h, 3).await;
    let refusals = h
        .outcomes()
        .into_iter()
        .filter(|outcome| outcome.failure_class() == Some(FailureClass::InvalidRequest))
        .count();
    assert_eq!(refusals, 2, "each refusal is a recorded terminal outcome");

    h.shutdown().await;
}

/// A `store: false` response does not exist for continuation either: the id the
/// client saw is refused rather than half-remembered.
#[tokio::test]
async fn a_store_false_prior_cannot_be_continued() {
    let h = Harness::new().await;

    let first = h
        .stored_turn(Harness::turn("EPHEMERAL_TURN", None, Some(false)))
        .await;
    assert_eq!(h.state.response_store.len(), 0, "nothing was retained");

    let follow_up = h
        .post_responses(Harness::turn("CONTINUE_EPHEMERAL", Some(&first), None))
        .await;
    assert_eq!(follow_up.status(), 400, "a discarded prior cannot continue");
    assert_eq!(h.mock.count(), 1, "nothing was sent for the refused turn");

    h.shutdown().await;
}

/// A string `input` is the user turn it means, not an empty item list: it is
/// stored as that turn and reaches the model on the first send.
#[tokio::test]
async fn a_string_input_is_stored_as_the_turn_it_means() {
    let h = Harness::new().await;

    let response = h
        .post_responses(json!({"model": "alpha-good-model", "input": "STRING_INPUT_MARKER"}))
        .await;
    assert_eq!(response.status(), 200);
    let wire: Value = response.json().await.unwrap();
    let id = wire["id"].as_str().unwrap().to_string();

    let stored = h.state.response_store.get(&id).expect("stored");
    let stored_input = serde_json::to_string(&stored.input).unwrap();
    assert!(
        stored_input.contains("STRING_INPUT_MARKER"),
        "a string input is kept as the turn it means: {stored_input}"
    );
    assert_eq!(stored.input.len(), 1);

    let sent = serde_json::to_string(&h.mock.bodies()[0]).unwrap();
    assert!(sent.contains("STRING_INPUT_MARKER"), "{sent}");

    h.shutdown().await;
}

/// The continuation chain is bounded: the maximum length is accepted, one more
/// turn is refused, and an oversized history is refused before any send.
#[tokio::test]
async fn a_continuation_chain_is_bounded() {
    let h = Harness::new().await;

    // The maximum chain: 32 stored turns, newest continuing the one before.
    for index in 0..32 {
        h.state.response_store.put(chain_response(index));
    }
    let at_limit = h
        .post_responses(Harness::turn("AT_LIMIT", Some("resp-chain-31"), None))
        .await;
    assert_eq!(at_limit.status(), 200, "the maximum chain still continues");
    let _ = at_limit.text().await.unwrap();
    let sent = serde_json::to_string(&h.mock.bodies()[0]).unwrap();
    assert!(
        sent.contains("CHAIN_0"),
        "the oldest turn is replayed: {sent}"
    );
    let sends_after_limit = h.mock.count();

    // One turn beyond the maximum: refused, with nothing sent.
    h.state.response_store.put(StoredResponse::completed(
        "resp-chain-32".to_string(),
        "alpha-good-model".to_string(),
        vec![json!({"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": "CHAIN_32"}
        ]})],
        vec![],
        Usage::default(),
        Some("resp-chain-31".to_string()),
        None,
    ));
    let over_limit = h
        .post_responses(Harness::turn("OVER_LIMIT", Some("resp-chain-32"), None))
        .await;
    assert_eq!(
        over_limit.status(),
        400,
        "a chain past the limit is refused"
    );
    assert_eq!(
        h.mock.count(),
        sends_after_limit,
        "nothing was sent for the refused chain"
    );

    // An oversized history is refused too.
    let huge = "x".repeat(1_100_000);
    h.state.response_store.put(StoredResponse::completed(
        "resp-huge".to_string(),
        "alpha-good-model".to_string(),
        vec![json!({"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": huge}
        ]})],
        vec![],
        Usage::default(),
        None,
        None,
    ));
    let oversized = h
        .post_responses(Harness::turn("AFTER_HUGE", Some("resp-huge"), None))
        .await;
    assert_eq!(oversized.status(), 400, "an oversized history is refused");
    assert_eq!(h.mock.count(), sends_after_limit);

    h.shutdown().await;
}

/// A chain that loops is refused rather than walked forever.
#[tokio::test]
async fn a_looping_continuation_chain_is_refused() {
    let h = Harness::new().await;

    let mut first = chain_response(0);
    first.previous_response_id = Some("resp-cycle-1".to_string());
    let mut second = chain_response(1);
    second.id = "resp-cycle-1".to_string();
    second.previous_response_id = Some("resp-cycle-0".to_string());
    first.id = "resp-cycle-0".to_string();
    h.state.response_store.put(first);
    h.state.response_store.put(second);

    let response = h
        .post_responses(Harness::turn("IN_A_LOOP", Some("resp-cycle-0"), None))
        .await;
    assert_eq!(response.status(), 400, "a looping chain is refused");
    assert_eq!(h.mock.count(), 0, "nothing was sent for the refused chain");

    h.shutdown().await;
}
