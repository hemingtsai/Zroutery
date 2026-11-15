use std::sync::Arc;

use serde_json::{json, Value};
use zroutery_core::config::{MemorySecretStore, ModelTier};

use super::support::*;

// --------------------------------------------------- PL-02 storage policy

/// The id a streaming Responses request publishes, read from its first frame.
///
/// Borrows the response: dropping it would disconnect the client and cancel
/// the very stream under test.
async fn streaming_response_id(response: &mut reqwest::Response) -> String {
    let first = response.chunk().await.unwrap().expect("some output");
    let wire = String::from_utf8_lossy(&first).to_string();
    wire.split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the first frame names the response")
        .to_string()
}

/// `store: false` is a retention instruction: the buffered answer must not be
/// retrievable, and its input and output must not be in the store at all.
#[tokio::test]
async fn store_false_never_retains_a_buffered_response() {
    let h = Harness::new().await;

    let response = h
        .responses_request("alpha-good-model", Some(false), false)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let id = body["id"]
        .as_str()
        .expect("the answer names itself")
        .to_string();

    let get = h.get_response(&id).send().await.unwrap();
    assert_eq!(get.status(), 400, "store:false must not be retrievable");
    let text = get.text().await.unwrap();
    assert!(
        !text.contains("DO_NOT_STORE_MARKER"),
        "the private input is not even echoed in the refusal: {text}"
    );
    assert_eq!(
        h.state.response_store.len(),
        0,
        "nothing at all was written to the response store"
    );

    h.shutdown().await;
}

/// The API default and an explicit `store: true` keep working: the response is
/// retrievable and carries the input and output it kept.
#[tokio::test]
async fn store_true_and_default_still_retain_a_buffered_response() {
    let h = Harness::new().await;

    for store in [Some(true), None] {
        let response = h
            .responses_request("alpha-good-model", store, false)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        let id = body["id"]
            .as_str()
            .expect("the answer names itself")
            .to_string();

        let get = h.get_response(&id).send().await.unwrap();
        assert_eq!(get.status(), 200, "store: {store:?} keeps the response");
        let stored: Value = get.json().await.unwrap();
        assert_eq!(stored["status"], "completed");
        assert_eq!(stored["id"], id);
        assert!(
            serde_json::to_string(&stored["input"])
                .unwrap()
                .contains("DO_NOT_STORE_MARKER"),
            "the input is retrievable when retention was allowed"
        );
        assert!(
            !stored["output"].as_array().unwrap().is_empty(),
            "the output is retrievable when retention was allowed"
        );
    }

    h.shutdown().await;
}

/// A repaired response takes the same decision as a direct one: the rectified
/// retry must not resurrect `store: false`.
#[tokio::test]
async fn store_false_never_retains_a_repaired_response() {
    for store in [Some(false), None] {
        let h = Harness::new().await;

        let mut body = json!({
            "model": "alpha-blind-model",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_image", "image_url": "https://example.com/chart.png"},
                {"type": "input_text", "text": "DO_NOT_STORE_MARKER"}
            ]}],
        });
        if let Some(store) = store {
            body["store"] = json!(store);
        }
        let response = h.post("/v1/responses").json(&body).send().await.unwrap();
        assert_eq!(response.status(), 200, "the rectifier repaired the image");
        let answer: Value = response.json().await.unwrap();
        let id = answer["id"]
            .as_str()
            .expect("the answer names itself")
            .to_string();

        let get = h.get_response(&id).send().await.unwrap();
        if store == Some(false) {
            assert_eq!(
                get.status(),
                400,
                "a repaired answer still honours store:false"
            );
            assert!(!get.text().await.unwrap().contains("DO_NOT_STORE_MARKER"));
            assert_eq!(h.state.response_store.len(), 0);
        } else {
            assert_eq!(get.status(), 200, "the default keeps the repaired answer");
            let stored: Value = get.json().await.unwrap();
            assert_eq!(stored["status"], "completed");
        }

        h.shutdown().await;
    }
}

/// Cancelling a stream leaves a content-free placeholder, never a copy of the
/// input the client sent.
#[tokio::test]
async fn a_cancelled_stream_does_not_retain_the_request_content() {
    let h = Harness::new().await;

    let mut response = h
        .responses_request("alpha-cancel-model", None, true)
        .send()
        .await
        .unwrap();
    let id = streaming_response_id(&mut response).await;

    let cancelled = h
        .post(&format!("/v1/responses/{id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), 200);
    let cancel_body = cancelled.text().await.unwrap();
    assert!(!cancel_body.contains("DO_NOT_STORE_MARKER"));

    wait_for_outcomes(&h, 1).await;

    let get = h.get_response(&id).send().await.unwrap();
    assert_eq!(
        get.status(),
        200,
        "the cancelled placeholder is retrievable"
    );
    let stored: Value = get.json().await.unwrap();
    assert_eq!(stored["status"], "cancelled");
    assert!(
        stored["input"].as_array().unwrap().is_empty()
            && stored["output"].as_array().unwrap().is_empty(),
        "cancellation must not re-store the request content: {stored}"
    );

    h.shutdown().await;
}

/// A `store: false` stream that is cancelled keeps nothing, not even the
/// placeholder: the client asked for the response to be discarded.
#[tokio::test]
async fn a_cancelled_store_false_stream_is_not_retained() {
    let h = Harness::new().await;

    let mut response = h
        .responses_request("alpha-cancel-model", Some(false), true)
        .send()
        .await
        .unwrap();
    let id = streaming_response_id(&mut response).await;

    let cancelled = h
        .post(&format!("/v1/responses/{id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), 200, "the cancellation itself succeeds");
    let cancel_body = cancelled.text().await.unwrap();
    assert!(!cancel_body.contains("DO_NOT_STORE_MARKER"));

    wait_for_outcomes(&h, 1).await;

    let get = h.get_response(&id).send().await.unwrap();
    assert_eq!(
        get.status(),
        400,
        "store:false means there is nothing to retrieve"
    );
    assert_eq!(h.state.response_store.len(), 0);

    h.shutdown().await;
}

// ------------------------------------------------------- response identity

/// One upstream id shape is cancelled by the client, the other is a request the
/// proxy refuses: the id the client saw in its first frame is the only key a
/// cancel accepts, and the placeholder GET answers with says cancelled.
async fn cancelling_by_the_published_id_works(h: &Harness, model: &str, upstream_id: &str) {
    let mut response = h
        .post("/v1/responses")
        .json(&json!({"model": model, "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let visible = streaming_response_id(&mut response).await;
    assert!(
        visible.starts_with("resp-"),
        "the client learns the registered id, not the upstream's: {visible}"
    );
    assert_ne!(
        visible, upstream_id,
        "the upstream id is not the identity the proxy registered"
    );

    // The upstream's own id names nothing the proxy knows about.
    let miss = h
        .post(&format!("/v1/responses/{upstream_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        miss.status(),
        400,
        "an id the proxy never registered cannot cancel anything"
    );

    let cancelled = h
        .post(&format!("/v1/responses/{visible}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        cancelled.status(),
        200,
        "cancelling by the visible id works"
    );
    let _ = cancelled.text().await;

    wait_for_outcomes(h, 1).await;

    let get = h.get_response(&visible).send().await.unwrap();
    assert_eq!(get.status(), 200);
    let stored: Value = get.json().await.unwrap();
    assert_eq!(stored["status"], "cancelled");
    assert_eq!(stored["id"], visible);

    // The recorded outcome names the identity the client was given.
    assert_eq!(h.outcome().response_id.as_deref(), Some(visible.as_str()));
}

/// An OpenAI-shaped upstream id (`chatcmpl-…`) is never published: the client
/// can cancel the stream it is reading, and only by the id from the first frame.
#[tokio::test]
async fn an_openai_upstream_id_is_never_the_cancel_key() {
    let h = Harness::new().await;
    cancelling_by_the_published_id_works(&h, "alpha-hold-model", "chatcmpl-mock").await;
    h.shutdown().await;
}

/// The same rule for an Anthropic-shaped upstream id (`msg_…`).
#[tokio::test]
async fn an_anthropic_upstream_id_is_never_the_cancel_key() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.providers
        .push(anthropic_provider("gamma", "Gamma", addr));
    cfg.models
        .push(model("gamma", "hold-model", 10, ModelTier::Fast));
    let h = Harness::start_with_secrets(
        cfg,
        mock,
        Arc::new(
            MemorySecretStore::new()
                .with("provider:alpha", "sk-alpha")
                .with("provider:beta", "sk-beta")
                .with("provider:gamma", "sk-gamma"),
        ),
    )
    .await;

    cancelling_by_the_published_id_works(&h, "gamma-hold-model", "msg_upstream").await;
    h.shutdown().await;
}

// -------------------------------------------------- streaming response storage

/// Every JSON payload in a complete Responses SSE body, in order.
fn sse_payloads(wire: &str) -> Vec<Value> {
    wire.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .collect()
}

/// The terminal frame's `response` object: what the client was finally told.
fn terminal_response(wire: &str) -> Value {
    sse_payloads(wire)
        .into_iter()
        .find(|payload| {
            matches!(
                payload["type"].as_str(),
                Some("response.completed")
                    | Some("response.incomplete")
                    | Some("response.failed")
                    | Some("response.cancelled")
            )
        })
        .unwrap_or_else(|| panic!("the stream reached no terminal frame: {wire}"))["response"]
        .clone()
}

impl Harness {
    /// `DELETE /v1/responses/{id}`.
    pub(super) fn delete_response(&self, id: &str) -> reqwest::RequestBuilder {
        self.client
            .delete(format!("{}/v1/responses/{id}", self.base))
            .header("x-api-key", TOKEN)
    }

    /// Run a Responses stream to its end and return the whole body.
    async fn stream_responses(&self, model: &str, store: Option<bool>) -> String {
        let response = self
            .responses_request(model, store, true)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.text().await.unwrap()
    }
}

/// A stream that reaches its normal terminal is stored: the later `GET` returns
/// the id, output and usage the client just read, and the input it sent.
#[tokio::test]
async fn a_completed_stream_is_retrievable_with_what_it_streamed() {
    let h = Harness::new().await;

    let wire = h.stream_responses("alpha-good-model", None).await;
    let terminal = terminal_response(&wire);
    assert_eq!(terminal["status"], "completed", "{wire}");
    let id = terminal["id"].as_str().unwrap().to_string();

    let get = h.get_response(&id).send().await.unwrap();
    assert_eq!(get.status(), 200, "a completed stream is retrievable");
    let stored: Value = get.json().await.unwrap();
    assert_eq!(stored["status"], "completed");
    assert_eq!(stored["id"], id);
    assert_eq!(
        stored["output"], terminal["output"],
        "the stored output is exactly what was streamed"
    );
    assert_eq!(
        stored["usage"]["input_tokens"],
        terminal["usage"]["input_tokens"]
    );
    assert_eq!(
        stored["usage"]["output_tokens"],
        terminal["usage"]["output_tokens"]
    );
    assert!(
        serde_json::to_string(&stored["input"])
            .unwrap()
            .contains("DO_NOT_STORE_MARKER"),
        "the input is retrievable when retention was allowed: {stored}"
    );

    h.shutdown().await;
}

/// A truncated stream is not an answer: nothing is stored, so it can never come
/// back as a completed response.
#[tokio::test]
async fn an_interrupted_stream_is_never_retrievable_as_completed() {
    let h = Harness::new().await;

    let wire = h.stream_responses("alpha-truncate-model", None).await;
    let terminal = terminal_response(&wire);
    assert_eq!(terminal["status"], "failed", "{wire}");
    let id = terminal["id"].as_str().unwrap().to_string();

    let get = h.get_response(&id).send().await.unwrap();
    if get.status() == 200 {
        let stored: Value = get.json().await.unwrap();
        assert_ne!(
            stored["status"], "completed",
            "a truncated stream must never be stored as a completed answer: {stored}"
        );
    } else {
        assert_eq!(get.status(), 400, "and nothing is retrievable for it");
    }

    h.shutdown().await;
}

/// `store: false` is a retention instruction for streams too: nothing is
/// written, so the id answers with "not found".
#[tokio::test]
async fn store_false_never_retains_a_streamed_response() {
    let h = Harness::new().await;

    let wire = h.stream_responses("alpha-good-model", Some(false)).await;
    let id = terminal_response(&wire)["id"].as_str().unwrap().to_string();

    let get = h.get_response(&id).send().await.unwrap();
    assert_eq!(get.status(), 400, "store:false must not be retrievable");
    let text = get.text().await.unwrap();
    assert!(
        !text.contains("DO_NOT_STORE_MARKER"),
        "the private input is not even echoed in the refusal: {text}"
    );
    assert_eq!(
        h.state.response_store.len(),
        0,
        "nothing at all was written to the response store"
    );

    h.shutdown().await;
}

/// A stored stream can be deleted, and once deleted it is gone.
#[tokio::test]
async fn a_deleted_streamed_response_cannot_be_fetched() {
    let h = Harness::new().await;

    let wire = h.stream_responses("alpha-good-model", None).await;
    let id = terminal_response(&wire)["id"].as_str().unwrap().to_string();
    assert_eq!(h.get_response(&id).send().await.unwrap().status(), 200);

    let deleted = h.delete_response(&id).send().await.unwrap();
    assert_eq!(deleted.status(), 200);
    let body: Value = deleted.json().await.unwrap();
    assert_eq!(body["deleted"], true);

    assert_eq!(
        h.get_response(&id).send().await.unwrap().status(),
        400,
        "a deleted response cannot be fetched"
    );

    h.shutdown().await;
}
