//! Regression tests for OpenAI Responses interoperability defects.
//!
//! These exercise the wire shapes an official Responses client actually sends
//! and consumes, rather than only the frames this repository generates.

use serde_json::{json, Value};
use zroutery_core::ir::{
    ContentBlock, Dialect, Role, StopReason, StreamEvent, ToolResultPart, Usage,
};
use zroutery_core::protocol::responses::{
    decode_request, decode_response, encode_request, ResponsesStreamEncoder, ResponsesStreamParser,
};
use zroutery_core::protocol::{SseDecoder, SseFrame, StreamEncoder, StreamParser};

#[test]
fn easy_input_message_without_type_decodes() {
    let body = json!({"model": "m", "input": [{"role": "user", "content": "Hi"}]});
    let req = decode_request(body).unwrap();
    assert_eq!(req.source_dialect, Dialect::OpenAIResponses);
    assert_eq!(req.messages.len(), 1);
    assert!(matches!(req.messages[0].role, Role::User));
    assert_eq!(req.messages[0].content[0].as_text(), Some("Hi"));
}

#[test]
fn message_string_content_decodes_for_both_roles() {
    let body = json!({"model": "m", "input": [
        {"type": "message", "role": "user", "content": "Hi"},
        {"type": "message", "role": "assistant", "content": "Hello"},
    ]});
    let req = decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 2);
    assert!(matches!(req.messages[0].role, Role::User));
    assert!(matches!(req.messages[1].role, Role::Assistant));
    assert_eq!(req.messages[1].content[0].as_text(), Some("Hello"));
}

#[test]
fn system_and_developer_messages_lift_to_instructions() {
    let body = json!({"model": "m", "input": [
        {"role": "system", "content": "be nice"},
        {"role": "developer", "content": [{"type": "input_text", "text": "be terse"}]},
        {"role": "user", "content": "hi"},
    ]});
    let req = decode_request(body).unwrap();
    assert_eq!(req.system.len(), 2);
    assert_eq!(req.system[0].text, "be nice");
    assert_eq!(req.system[1].text, "be terse");
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].content[0].as_text(), Some("hi"));
}

#[test]
fn unsupported_input_items_still_error() {
    let body = json!({"model": "m", "input": [{"type": "computer_call", "call_id": "x"}]});
    assert!(decode_request(body).is_err());
    let body = json!({"model": "m", "input": [{"foo": 1}]});
    assert!(decode_request(body).is_err());
    let body =
        json!({"model": "m", "input": [{"type": "message", "role": "tool", "content": "x"}]});
    assert!(decode_request(body).is_err());
}

#[test]
fn message_image_url_string_decodes() {
    let body = json!({"model": "m", "input": [{
        "type": "message",
        "role": "user",
        "content": [{"type": "input_image", "image_url": "https://example.com/photo.png"}],
    }]});
    let req = decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Image { source } => {
            assert_eq!(source.to_data_url(), "https://example.com/photo.png");
        }
        other => panic!("expected image, got {other:?}"),
    }
}

#[test]
fn image_url_data_url_decodes() {
    let data_url = "data:image/png;base64,AAAA";
    let body = json!({"model": "m", "input": [
        {"type": "input_image", "image_url": data_url},
        {"type": "message", "role": "user", "content": [
            {"type": "input_image", "image_url": data_url},
        ]},
    ]});
    let req = decode_request(body).unwrap();
    for message in &req.messages {
        match &message.content[0] {
            ContentBlock::Image { source } => assert_eq!(source.to_data_url(), data_url),
            other => panic!("expected image, got {other:?}"),
        }
    }
}

#[test]
fn tool_result_image_url_string_decodes() {
    let body = json!({"model": "m", "input": [{
        "type": "function_call_output",
        "call_id": "call_1",
        "output": [{"type": "input_image", "image_url": "https://example.com/tool.png"}],
    }]});
    let req = decode_request(body).unwrap();
    let ContentBlock::ToolResult { content, .. } = &req.messages[0].content[0] else {
        panic!("expected tool result");
    };
    let ToolResultPart::Image { source } = &content[0] else {
        panic!("expected image part");
    };
    assert_eq!(source.to_data_url(), "https://example.com/tool.png");
}

#[test]
fn legacy_nested_image_url_still_decodes() {
    let body = json!({"model": "m", "input": [{
        "type": "message",
        "role": "user",
        "content": [{"type": "input_image", "image_url": {"url": "https://example.com/a.png"}}],
    }]});
    let req = decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Image { source } => {
            assert_eq!(source.to_data_url(), "https://example.com/a.png");
        }
        other => panic!("expected image, got {other:?}"),
    }
}

// --------------------------------------------------------------- RSP-03

fn frame_payload(frame: &SseFrame) -> Value {
    frame.json().unwrap()
}

fn frames_for(events: &[StreamEvent]) -> Vec<SseFrame> {
    let mut enc = ResponsesStreamEncoder::new("m");
    let mut frames = Vec::new();
    for event in events {
        frames.extend(enc.encode(event));
    }
    frames.extend(enc.finish());
    frames
}

fn output_text_deltas(frames: &[SseFrame]) -> String {
    frames
        .iter()
        .filter(|f| f.event.as_deref() == Some("response.output_text.delta"))
        .map(|f| frame_payload(f)["delta"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn text_content_part_starts_with_empty_text() {
    let frames = frames_for(&[
        StreamEvent::Start {
            id: "resp_1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "Hello".into(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: " world".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);

    let added: Vec<Value> = frames
        .iter()
        .filter(|f| f.event.as_deref() == Some("response.content_part.added"))
        .map(frame_payload)
        .collect();
    assert_eq!(added.len(), 1, "one text content part");
    assert_eq!(added[0]["part"]["type"], "output_text");
    assert_eq!(added[0]["part"]["text"], "");
    assert!(added[0]["part"]["annotations"].is_array());

    // The deltas a client accumulates must reproduce the model output.
    assert_eq!(output_text_deltas(&frames), "Hello world");
    let done_part = frames
        .iter()
        .find(|f| f.event.as_deref() == Some("response.content_part.done"))
        .map(frame_payload)
        .unwrap();
    assert_eq!(done_part["part"]["text"], "Hello world");

    let completed = frames
        .iter()
        .find(|f| f.event.as_deref() == Some("response.completed"))
        .map(frame_payload)
        .unwrap();
    assert_eq!(
        completed["response"]["output"][0]["content"][0]["text"],
        "Hello world"
    );

    // Every frame carries a monotonic sequence number.
    let sequences: Vec<u64> = frames
        .iter()
        .map(|f| frame_payload(f)["sequence_number"].as_u64().unwrap())
        .collect();
    assert_eq!(sequences, (0..frames.len() as u64).collect::<Vec<_>>());

    // Newer SDKs require logprobs on the text delta and done events.
    for frame in &frames {
        let payload = frame_payload(frame);
        if matches!(
            frame.event.as_deref(),
            Some("response.output_text.delta") | Some("response.output_text.done")
        ) {
            assert!(
                payload["logprobs"].is_array(),
                "{} must carry logprobs",
                frame.event.as_deref().unwrap()
            );
        }
    }
}

// --------------------------------------------------------------- RSP-04

#[test]
fn max_tokens_stream_stop_reports_incomplete() {
    let frames = frames_for(&[
        StreamEvent::Start {
            id: "resp_1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "truncated".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::MaxTokens,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);

    let last = frames.last().expect("terminal frame");
    assert_eq!(last.event.as_deref(), Some("response.incomplete"));
    let payload = frame_payload(last);
    assert_eq!(payload["response"]["status"], "incomplete");
    assert_eq!(
        payload["response"]["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(payload["response"]["output"][0]["status"], "incomplete");
    assert_eq!(
        payload["response"]["output"][0]["content"][0]["text"],
        "truncated"
    );
}

#[test]
fn normal_stream_stop_still_reports_completed() {
    let frames = frames_for(&[
        StreamEvent::Start {
            id: "resp_1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "done".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);

    let last = frames.last().expect("terminal frame");
    assert_eq!(last.event.as_deref(), Some("response.completed"));
    let payload = frame_payload(last);
    assert_eq!(payload["response"]["status"], "completed");
    assert!(payload["response"]["incomplete_details"].is_null());
    assert_eq!(payload["response"]["output"][0]["status"], "completed");
}

// --------------------------------------------------------------- RSP-05

fn streamed_output_items(frames: &[SseFrame]) -> Vec<Value> {
    let terminal = frames
        .iter()
        .find(|f| {
            matches!(
                f.event.as_deref(),
                Some("response.completed") | Some("response.incomplete")
            )
        })
        .map(frame_payload)
        .expect("terminal frame");
    terminal["response"]["output"].as_array().unwrap().clone()
}

#[test]
fn streamed_thinking_signature_survives_replay() {
    let frames = frames_for(&[
        StreamEvent::Start {
            id: "resp_1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::ThinkingDelta {
            index: 0,
            text: "reasoning summary".into(),
        },
        StreamEvent::ThinkingSignature {
            index: 0,
            signature: "sig-abc".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);

    let items = streamed_output_items(&frames);
    let reasoning = items
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("reasoning item");
    assert_eq!(reasoning["summary"][0]["text"], "reasoning summary");
    assert!(
        reasoning["encrypted_content"].is_string(),
        "streamed reasoning must carry its signature: {reasoning}"
    );
    let done = frames
        .iter()
        .filter(|f| f.event.as_deref() == Some("response.output_item.done"))
        .map(frame_payload)
        .find(|payload| payload["item"]["type"] == "reasoning")
        .expect("reasoning output_item.done");
    assert_eq!(
        done["item"]["encrypted_content"],
        reasoning["encrypted_content"]
    );

    // Feeding the streamed output back as the next turn's input must restore
    // the signature, not an empty one.
    let body = json!({"model": "m", "input": [reasoning.clone()]});
    let req = decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::Thinking { text, signature } => {
            assert_eq!(text, "reasoning summary");
            assert_eq!(signature.as_deref(), Some("sig-abc"));
        }
        other => panic!("expected thinking block, got {other:?}"),
    }
}

#[test]
fn streamed_redacted_thinking_survives_replay() {
    let frames = frames_for(&[
        StreamEvent::Start {
            id: "resp_1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::RedactedThinking {
            index: 0,
            data: "opaque-blob".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);

    let items = streamed_output_items(&frames);
    let reasoning = items
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("redacted reasoning item");
    assert!(reasoning["encrypted_content"].is_string());

    let body = json!({"model": "m", "input": [reasoning.clone()]});
    let req = decode_request(body).unwrap();
    match &req.messages[0].content[0] {
        ContentBlock::RedactedThinking { data } => assert_eq!(data, "opaque-blob"),
        other => panic!("expected redacted thinking block, got {other:?}"),
    }
}

// --------------------------------------------------------------- LD1

#[test]
fn null_response_error_decodes() {
    let body = json!({"id": "resp_1", "status": "completed", "error": null, "output": []});
    let resp = decode_response(body).unwrap();
    assert_eq!(resp.id, "resp_1");
    assert!(resp.content.is_empty());
}

#[test]
fn non_null_response_error_still_fails() {
    let body = json!({
        "id": "resp_1",
        "status": "failed",
        "error": {"code": "server_error", "message": "boom"},
        "output": [],
    });
    let err = decode_response(body).unwrap_err();
    assert!(err.to_string().contains("boom"), "got {err}");
}

// --------------------------------------------------------------- LD2

#[test]
fn parser_accepts_incomplete_and_reasoning_summary_parts() {
    let raw = concat!(
        "event: response.created\ndata: ",
        r#"{"type":"response.created","response":{"id":"resp_1","model":"m"}}"#,
        "\n\n",
        "event: response.reasoning_summary_part.added\ndata: ",
        r#"{"type":"response.reasoning_summary_part.added","item_id":"rs_1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}"#,
        "\n\n",
        "event: response.reasoning_summary_text.delta\ndata: ",
        r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_1","output_index":0,"summary_index":0,"delta":"one"}"#,
        "\n\n",
        "event: response.reasoning_summary_part.done\ndata: ",
        r#"{"type":"response.reasoning_summary_part.done","item_id":"rs_1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":"one"}}"#,
        "\n\n",
        "event: response.reasoning_summary_text.delta\ndata: ",
        r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_1","output_index":1,"summary_index":0,"delta":"two"}"#,
        "\n\n",
        "event: response.incomplete\ndata: ",
        r#"{"type":"response.incomplete","response":{"id":"resp_1","model":"m","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        "\n\n",
    );

    let mut decoder = SseDecoder::new();
    let mut parser = ResponsesStreamParser::new("m");
    let mut events = Vec::new();
    for frame in decoder.push(raw.as_bytes()) {
        events.extend(
            parser
                .push(&frame)
                .unwrap_or_else(|err| panic!("{:?} rejected: {err}", frame.event)),
        );
    }
    events.extend(parser.finish());

    let thinking: Vec<(u32, String)> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ThinkingDelta { index, text } => Some((*index, text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        thinking,
        vec![(0, "one".to_string()), (1, "two".to_string())],
        "the summary part done event must close the open reasoning index"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::Stop {
                stop_reason: StopReason::MaxTokens,
                ..
            }
        )),
        "response.incomplete must stop the stream as MaxTokens"
    );
}

// --------------------------------------------------------------- LD3

#[test]
fn native_reasoning_input_keeps_payload_and_all_summaries() {
    let body = json!({"model": "m", "input": [{
        "type": "reasoning",
        "id": "rs_native",
        "encrypted_content": "native-opaque-blob",
        "summary": [
            {"type": "summary_text", "text": "first"},
            {"type": "summary_text", "text": "second"},
        ],
    }]});
    let req = decode_request(body).unwrap();

    let content = &req.messages[0].content;
    assert_eq!(content.len(), 2, "summary text plus the opaque payload");
    match &content[0] {
        ContentBlock::Thinking { text, signature } => {
            assert_eq!(text, "first\n\nsecond", "every summary entry survives");
            assert!(signature.is_none());
        }
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &content[1] {
        ContentBlock::RedactedThinking { data } => assert_eq!(data, "native-opaque-blob"),
        other => panic!("expected redacted thinking block, got {other:?}"),
    }

    // Re-encoding for a native upstream echoes the opaque payload unchanged
    // and keeps the whole summary.
    let wire = encode_request(&req, "m").unwrap();
    let input = wire["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|item| item["type"] == "reasoning"
                && item["encrypted_content"] == "native-opaque-blob"),
        "opaque payload must round trip verbatim: {wire}"
    );
    assert!(
        input
            .iter()
            .any(|item| item["type"] == "reasoning"
                && item["summary"][0]["text"] == "first\n\nsecond"),
        "full summary must round trip: {wire}"
    );

    // And the wire shape decodes again.
    let decoded = decode_request(wire).unwrap();
    assert!(decoded
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .any(
            |block| matches!(block, ContentBlock::RedactedThinking { data }
            if data == "native-opaque-blob")
        ));
}

#[test]
fn native_reasoning_response_keeps_encrypted_content() {
    let body = json!({
        "id": "resp_1",
        "status": "completed",
        "output": [{
            "type": "reasoning",
            "id": "rs_1",
            "summary": [],
            "encrypted_content": "native-blob",
        }],
    });
    let resp = decode_response(body).unwrap();
    assert_eq!(resp.content.len(), 1);
    match &resp.content[0] {
        ContentBlock::RedactedThinking { data } => assert_eq!(data, "native-blob"),
        other => panic!("expected redacted thinking block, got {other:?}"),
    }
}
