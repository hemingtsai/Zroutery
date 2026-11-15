use zroutery_core::ir::{StopReason, StreamEvent, Usage};
use zroutery_core::protocol::responses::ResponsesStreamEncoder;
use zroutery_core::protocol::StreamEncoder;

// ============================================================
// ResponsesStreamEncoder regression tests
// ============================================================

/// Helper: collect all SSE frames from encoding a sequence of events.
fn encode_events(events: &[StreamEvent]) -> Vec<String> {
    let mut enc = ResponsesStreamEncoder::new("test-model");
    let mut frames = Vec::new();
    for e in events {
        for f in enc.encode(e) {
            frames.push(format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data));
        }
    }
    for f in enc.finish() {
        frames.push(format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data));
    }
    frames
}

fn find_event(frames: &[String], event_type: &str) -> Vec<serde_json::Value> {
    frames
        .iter()
        .filter(|f| f.starts_with(event_type))
        .map(|f| serde_json::from_str(f.lines().last().unwrap()).unwrap())
        .collect()
}

#[test]
fn stream_text_starts_at_output_index_0() {
    let events = encode_events(&[
        StreamEvent::Start {
            id: "r1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "hello".into(),
        },
        StreamEvent::BlockStop { index: 0 },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);
    let added = find_event(&events, "response.output_item.added");
    assert!(!added.is_empty());
    assert_eq!(
        added[0]["output_index"], 0,
        "first item should be at index 0"
    );
    assert_eq!(added[0]["item"]["type"], "message");
}

#[test]
fn stream_text_emits_output_text_done() {
    let events = encode_events(&[
        StreamEvent::Start {
            id: "r1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "hello".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);
    let done = find_event(&events, "response.output_text.done");
    assert!(!done.is_empty(), "should emit output_text.done");
    assert_eq!(done[0]["text"], "hello");
}

#[test]
fn stream_content_part_has_item_id() {
    let events = encode_events(&[
        StreamEvent::Start {
            id: "r1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "x".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);
    let added = find_event(&events, "response.content_part.added");
    assert!(!added.is_empty());
    assert!(
        added[0]["item_id"].as_str().is_some(),
        "content_part.added must have item_id"
    );
}

#[test]
fn stream_parallel_tools_no_interference() {
    let events = encode_events(&[
        StreamEvent::Start {
            id: "r1".into(),
            model: "m".into(),
            usage: Usage::default(),
        },
        StreamEvent::ToolUseStart {
            index: 0,
            id: "call_a".into(),
            name: "fn_a".into(),
        },
        StreamEvent::ToolUseStart {
            index: 1,
            id: "call_b".into(),
            name: "fn_b".into(),
        },
        StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "{\"x\":1}".into(),
        },
        StreamEvent::ToolUseDelta {
            index: 1,
            partial_json: "{\"y\":2}".into(),
        },
        StreamEvent::BlockStop { index: 1 }, // B finishes first
        StreamEvent::BlockStop { index: 0 }, // A finishes second
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage::default(),
        },
    ]);
    // Both tools should have added + done events.
    let added = find_event(&events, "response.output_item.added");
    assert_eq!(added.len(), 2, "should have 2 output_item.added");
    let done = find_event(&events, "response.output_item.done");
    assert_eq!(done.len(), 2, "should have 2 output_item.done");
    // Output should be sorted by output_index.
    let completed = find_event(&events, "response.completed");
    let output = completed[0]["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0]["type"], "function_call");
    assert_eq!(output[1]["type"], "function_call");
    // A was index 0, B was index 1.
    assert_eq!(output[0]["call_id"], "call_a");
    assert_eq!(output[1]["call_id"], "call_b");
    // Arguments should be strings, not parsed objects.
    assert!(
        output[0]["arguments"].is_string(),
        "arguments must be JSON string"
    );
    assert!(
        output[1]["arguments"].is_string(),
        "arguments must be JSON string"
    );
}

#[test]
fn stream_incomplete_tool_on_finish() {
    // Tool starts but never gets BlockStop — finish() should emit terminal events.
    let mut enc = ResponsesStreamEncoder::new("test-model");
    let mut frames = Vec::new();
    frames.extend(enc.encode(&StreamEvent::Start {
        id: "r1".into(),
        model: "m".into(),
        usage: Usage::default(),
    }));
    frames.extend(enc.encode(&StreamEvent::ToolUseStart {
        index: 0,
        id: "call_a".into(),
        name: "fn".into(),
    }));
    frames.extend(enc.encode(&StreamEvent::ToolUseDelta {
        index: 0,
        partial_json: "{\"x\":".into(),
    }));
    // No BlockStop — finish() called directly.
    frames.extend(enc.finish());
    let wire: Vec<String> = frames
        .iter()
        .map(|f| format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data))
        .collect();
    // Should have arguments.done + output_item.done with status=incomplete.
    let args_done = find_event(&wire, "response.function_call_arguments.done");
    assert_eq!(
        args_done.len(),
        1,
        "should emit arguments.done for incomplete tool"
    );
    let item_done = find_event(&wire, "response.output_item.done");
    assert_eq!(
        item_done.len(),
        1,
        "should emit output_item.done for incomplete tool"
    );
    let completed = find_event(&wire, "response.completed");
    assert_eq!(completed[0]["response"]["status"], "incomplete");
    let output = completed[0]["response"]["output"].as_array().unwrap();
    assert_eq!(output[0]["status"], "incomplete");
    // Arguments should be the partial JSON as a string.
    assert_eq!(output[0]["arguments"], "{\"x\":");
}

#[test]
fn stream_error_terminal_is_only_failed() {
    let mut enc = ResponsesStreamEncoder::new("test-model");
    let mut frames = Vec::new();
    frames.extend(enc.encode(&StreamEvent::Start {
        id: "r1".into(),
        model: "m".into(),
        usage: Usage::default(),
    }));
    frames.extend(enc.encode(&StreamEvent::TextDelta {
        index: 0,
        text: "partial".into(),
    }));
    frames.extend(enc.error(&zroutery_core::Error::internal("test error")));
    let wire: Vec<String> = frames
        .iter()
        .map(|f| format!("{}\n{}", f.event.as_deref().unwrap_or(""), f.data))
        .collect();
    // Should have response.failed but NO response.completed.
    let failed = find_event(&wire, "response.failed");
    assert_eq!(failed.len(), 1, "should have exactly one response.failed");
    let completed = find_event(&wire, "response.completed");
    assert!(
        completed.is_empty(),
        "should NOT have response.completed after error"
    );
}
