//! Dialect-level tests for the Chat Completions half.

use serde_json::{json, Value};

use crate::error::Error;
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, MediaSource, StopReason, StreamEvent, SystemPart,
    ThinkingConfig, ToolChoice, UnsupportedContentPolicy, Usage,
};
use crate::protocol::{ProviderQuirks, SseDecoder, SseFrame, StreamEncoder, StreamParser};

use super::response::{decode_usage, encode_usage};
use super::*;

fn parse(raw: &str) -> Vec<StreamEvent> {
    let mut dec = SseDecoder::new();
    let mut parser = OpenAiStreamParser::new("fallback");
    let mut events = Vec::new();
    for f in dec.push(raw.as_bytes()) {
        events.extend(parser.push(&f).unwrap());
    }
    events.extend(parser.finish());
    events
}

fn chunk(delta: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id":"chatcmpl-1","object":"chat.completion.chunk","model":"deepseek-v4-pro",
               "choices":[{"index":0,"delta":delta,"finish_reason":null}]})
    )
}

#[test]
fn decodes_system_user_and_tool_history() {
    let req = decode_request(json!({
        "model": "gpt-5.3-sol",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "get_weather", "arguments": "{\"city\":\"SH\"}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
            {"role": "user", "content": "thanks"}
        ],
        "max_completion_tokens": 256,
        "stop": "END",
        "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}],
        "tool_choice": "required",
        "reasoning_effort": "high",
        "user": "u9"
    }))
    .unwrap();

    assert_eq!(req.system, vec![SystemPart::new("be nice")]);
    assert_eq!(req.max_tokens, Some(256));
    assert_eq!(req.stop_sequences, vec!["END"]);
    assert_eq!(req.tool_choice, Some(ToolChoice::Any));
    assert_eq!(req.thinking.unwrap().budget_tokens, Some(16384));
    assert_eq!(req.metadata_user.as_deref(), Some("u9"));
    assert_eq!(req.messages.len(), 4);
    assert!(matches!(
        req.messages[1].content[0],
        ContentBlock::ToolUse { .. }
    ));
    assert!(matches!(
        req.messages[2].content[0],
        ContentBlock::ToolResult { .. }
    ));
    assert_eq!(req.messages[3].content[0], ContentBlock::text("thanks"));
}

#[test]
fn decodes_multimodal_user_content() {
    let req = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is this"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,QQ=="}}
        ]}]
    }))
    .unwrap();
    assert_eq!(req.messages[0].content.len(), 2);
    assert_eq!(
        req.messages[0].content[1],
        ContentBlock::Image {
            source: MediaSource::Base64 {
                media_type: "image/png".into(),
                data: "QQ==".into()
            }
        }
    );
}

#[test]
fn tool_history_round_trips_back_to_openai_shape() {
    let original = json!({
        "model": "m",
        "messages": [
            {"role": "system", "content": "s"},
            {"role": "user", "content": "q"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "f", "arguments": "{\"a\":1}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
        ]
    });
    let req = decode_request(original).unwrap();
    let body = encode_request(&req, "up").unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[2]["content"], Value::Null);
    assert_eq!(
        msgs[2]["tool_calls"][0]["function"]["arguments"],
        "{\"a\":1}"
    );
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_1");
    assert_eq!(msgs[3]["content"], "ok");
    // and again through the decoder
    let again = decode_request(body).unwrap();
    assert_eq!(again.messages, req.messages);
    assert_eq!(again.system, req.system);
}

#[test]
fn quirks_control_parameter_names() {
    let req = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 10,
        "temperature": 0.7,
        "stop": ["x"],
        "stream": true
    }))
    .unwrap();

    let plain = encode_request(&req, "up").unwrap();
    assert_eq!(plain["max_tokens"], 10);
    assert_eq!(plain["temperature"], 0.7);
    assert_eq!(
        plain["temperature"].to_string(),
        "0.7",
        "no float noise on the wire"
    );
    assert_eq!(plain["stream_options"]["include_usage"], true);

    let strict = ProviderQuirks {
        use_max_completion_tokens: true,
        drop_temperature: true,
        drop_stop: true,
        stream_usage: false,
        system_as_developer: true,
        ..ProviderQuirks::default()
    };
    let body = encode_request_with(&req, "up", &strict).unwrap();
    assert_eq!(body["max_completion_tokens"], 10);
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("temperature").is_none());
    assert!(body.get("stop").is_none());
    assert!(body.get("stream_options").is_none());
}

#[test]
fn thinking_maps_to_reasoning_effort_only_when_supported() {
    let mut req = ChatRequest::new("m", Dialect::Anthropic);
    req.thinking = Some(ThinkingConfig {
        enabled: true,
        budget_tokens: Some(20000),
    });
    assert!(encode_request(&req, "up")
        .unwrap()
        .get("reasoning_effort")
        .is_none());
    let quirks = ProviderQuirks {
        send_reasoning_effort: true,
        ..ProviderQuirks::default()
    };
    assert_eq!(
        encode_request_with(&req, "up", &quirks).unwrap()["reasoning_effort"],
        "high"
    );
}

#[test]
fn reasoning_effort_accepts_xhigh_and_max_and_round_trips_them() {
    for (effort, budget) in [("xhigh", 32768u32), ("max", 65536)] {
        let req = decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": effort
        }))
        .unwrap();
        assert_eq!(
            req.thinking.and_then(|thinking| thinking.budget_tokens),
            Some(budget),
            "effort={effort}"
        );
    }

    let quirks = ProviderQuirks {
        send_reasoning_effort: true,
        ..ProviderQuirks::default()
    };
    let req = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "max"
    }))
    .unwrap();
    assert_eq!(
        encode_request_with(&req, "up", &quirks).unwrap()["reasoning_effort"],
        "max"
    );
}

#[test]
fn response_with_reasoning_and_tool_calls() {
    let resp = decode_response(json!({
        "id": "chatcmpl-9",
        "model": "deepseek-v4-pro",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "here you go",
                "reasoning_content": "let me think",
                "tool_calls": [{"id": "call_1", "type": "function",
                                "function": {"name": "f", "arguments": "{}"}}]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 12, "completion_tokens": 34,
                  "prompt_tokens_details": {"cached_tokens": 5},
                  "completion_tokens_details": {"reasoning_tokens": 7}}
    }))
    .unwrap();

    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert!(matches!(resp.content[0], ContentBlock::Thinking { .. }));
    assert_eq!(resp.content[1], ContentBlock::text("here you go"));
    assert!(matches!(resp.content[2], ContentBlock::ToolUse { .. }));
    assert_eq!(resp.usage.input_tokens, 12);
    assert_eq!(resp.usage.cache_read_tokens, 5);
    assert_eq!(resp.usage.reasoning_tokens, 7);

    let back = encode_response(&resp);
    assert_eq!(back["choices"][0]["message"]["content"], "here you go");
    assert_eq!(
        back["choices"][0]["message"]["reasoning_content"],
        "let me think"
    );
    assert_eq!(back["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(back["usage"]["total_tokens"], 46);
}

#[test]
fn deepseek_cache_hit_field_is_understood() {
    // `prompt_tokens` is the inclusive total and the DeepSeek cache-hit
    // field is a subset of it, not something to add on top.
    let u = decode_usage(Some(&json!({
        "prompt_tokens": 100, "completion_tokens": 1, "prompt_cache_hit_tokens": 64
    })));
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.cache_read_tokens, 64);
    assert_eq!(u.fresh_input_tokens(), 36);

    // A relay that reports more cache hits than prompt tokens cannot make the
    // cached counter exceed the total the client sees.
    let contradictory = decode_usage(Some(&json!({
        "prompt_tokens": 100, "completion_tokens": 1,
        "prompt_tokens_details": {"cached_tokens": 900}
    })));
    assert_eq!(contradictory.input_tokens, 100);
    assert_eq!(contradictory.cache_read_tokens, 100);

    let wire = encode_usage(&contradictory);
    assert_eq!(wire["prompt_tokens"], 100);
    assert!(
        wire["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap()
            <= 100
    );
}

#[test]
fn encoded_prompt_total_includes_the_cached_tokens() {
    // An IR usage that came from Anthropic: fresh=100, read=10_000,
    // write=200, so the prompt total is 10,300. OpenAI clients get that
    // total with the read nested inside it.
    let u = Usage {
        input_tokens: 10_300,
        output_tokens: 0,
        cache_read_tokens: 10_000,
        cache_write_tokens: 200,
        reasoning_tokens: 0,
    };
    let wire = encode_usage(&u);
    assert_eq!(wire["prompt_tokens"], 10_300);
    assert_eq!(wire["total_tokens"], 10_300);
    assert_eq!(wire["prompt_tokens_details"]["cached_tokens"], 10_000);
    assert!(
        wire["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap()
            <= wire["prompt_tokens"].as_u64().unwrap()
    );

    // And the total survives a decode.
    let back = decode_usage(Some(&wire));
    assert_eq!(back.input_tokens, 10_300);
    assert_eq!(back.cache_read_tokens, 10_000);
}

#[test]
fn parses_text_stream_with_usage_trailer() {
    let finish = json!({"id":"chatcmpl-1","model":"deepseek-v4-pro",
                        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
    let trailer = json!({"id":"chatcmpl-1","model":"deepseek-v4-pro","choices":[],
                         "usage":{"prompt_tokens":9,"completion_tokens":2}});
    let raw = format!(
        "{}{}{}data: {finish}\n\ndata: {trailer}\n\ndata: [DONE]\n\n",
        chunk(json!({"role": "assistant", "content": ""})),
        chunk(json!({"content": "Hel"})),
        chunk(json!({"content": "lo"})),
    );
    let events = parse(&raw);
    assert_eq!(
        events[0],
        StreamEvent::Start {
            id: "chatcmpl-1".into(),
            model: "deepseek-v4-pro".into(),
            usage: Usage::default()
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            index: 0,
            text: "Hel".into()
        }
    );
    assert_eq!(
        events[2],
        StreamEvent::TextDelta {
            index: 0,
            text: "lo".into()
        }
    );
    assert_eq!(events[3], StreamEvent::BlockStop { index: 0 });
    match &events[4] {
        StreamEvent::Stop {
            stop_reason, usage, ..
        } => {
            assert_eq!(*stop_reason, StopReason::EndTurn);
            assert_eq!(usage.input_tokens, 9);
            assert_eq!(usage.output_tokens, 2);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(events.len(), 5, "no duplicate stop after [DONE]");
}

#[test]
fn reasoning_then_text_opens_two_blocks() {
    let raw = format!(
        "{}{}{}",
        chunk(json!({"reasoning_content": "думаю"})),
        chunk(json!({"content": "answer"})),
        "data: [DONE]\n\n"
    );
    let events = parse(&raw);
    assert_eq!(
        events[1],
        StreamEvent::ThinkingDelta {
            index: 0,
            text: "думаю".into()
        }
    );
    assert_eq!(events[2], StreamEvent::BlockStop { index: 0 });
    assert_eq!(
        events[3],
        StreamEvent::TextDelta {
            index: 1,
            text: "answer".into()
        }
    );
    assert!(matches!(events.last().unwrap(), StreamEvent::Stop { .. }));
}

#[test]
fn parses_incremental_tool_calls() {
    let raw = format!(
        "{}{}{}{}{}",
        chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                                      "function": {"name": "get_weather", "arguments": ""}}]})
        ),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"ci"}}]})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "ty\":1}"}}]})),
        chunk(
            json!({"tool_calls": [{"index": 1, "id": "call_2", "type": "function",
                                      "function": {"name": "other", "arguments": "{}"}}]})
        ),
        "data: [DONE]\n\n"
    );
    let events = parse(&raw);
    assert_eq!(
        events[1],
        StreamEvent::ToolUseStart {
            index: 0,
            id: "call_1".into(),
            name: "get_weather".into()
        }
    );
    assert_eq!(
        events[2],
        StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "{\"ci".into()
        }
    );
    assert_eq!(
        events[3],
        StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "ty\":1}".into()
        }
    );
    assert_eq!(
        events[4],
        StreamEvent::ToolUseStart {
            index: 1,
            id: "call_2".into(),
            name: "other".into()
        }
    );
    assert_eq!(
        events[5],
        StreamEvent::ToolUseDelta {
            index: 1,
            partial_json: "{}".into()
        }
    );
    // Both calls stay open until the stream ends, then stop in order.
    assert_eq!(events[6], StreamEvent::BlockStop { index: 0 });
    assert_eq!(events[7], StreamEvent::BlockStop { index: 1 });
    assert!(matches!(events[8], StreamEvent::Stop { .. }));
    assert_legal_lifecycle(&events);
}

/// Anthropic lifecycle rules the parser must honour: a block receives no
/// delta after it stopped, and blocks stop in the order they were opened.
fn assert_legal_lifecycle(events: &[StreamEvent]) {
    let mut stopped: Vec<u32> = Vec::new();
    for event in events {
        match event {
            StreamEvent::BlockStop { index } => {
                assert!(
                    !stopped.contains(index),
                    "block {index} stopped twice: {events:?}"
                );
                if let Some(last) = stopped.last() {
                    assert!(
                        index > last,
                        "block {index} stopped before block {last}: {events:?}"
                    );
                }
                stopped.push(*index);
            }
            StreamEvent::TextDelta { index, .. }
            | StreamEvent::ThinkingDelta { index, .. }
            | StreamEvent::ThinkingSignature { index, .. }
            | StreamEvent::RedactedThinking { index, .. }
            | StreamEvent::ToolUseDelta { index, .. } => assert!(
                !stopped.contains(index),
                "delta for block {index} after its stop: {events:?}"
            ),
            _ => {}
        }
    }
}

fn tool_arguments(events: &[StreamEvent], wanted: u32) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolUseDelta {
                index,
                partial_json,
            } if *index == wanted => Some(partial_json.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn interleaved_parallel_tool_calls_keep_every_block_open() {
    // OpenAI delimits parallel calls by `tool_calls[].index`; their
    // argument deltas may arrive in any order, so a new call index proves
    // nothing about the calls already running.
    let raw = format!(
        "{}{}{}{}{}",
        chunk(json!({"tool_calls": [
            {"index": 0, "id": "call_a", "type": "function",
             "function": {"name": "fa", "arguments": "{\"x\":"}},
            {"index": 1, "id": "call_b", "type": "function",
             "function": {"name": "fb", "arguments": "{\"y\":"}}
        ]})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "1"}}]})),
        chunk(json!({"tool_calls": [{"index": 1, "function": {"arguments": "2}"}}]})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "}"}}]})),
        "data: [DONE]\n\n"
    );
    let events = parse(&raw);
    assert_legal_lifecycle(&events);

    let starts: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolUseStart { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![0, 1], "both calls must open: {events:?}");
    assert_eq!(tool_arguments(&events, 0), "{\"x\":1}");
    assert_eq!(tool_arguments(&events, 1), "{\"y\":2}");

    let tail = events.len() - 3;
    assert_eq!(events[tail], StreamEvent::BlockStop { index: 0 });
    assert_eq!(events[tail + 1], StreamEvent::BlockStop { index: 1 });
    assert!(matches!(events[tail + 2], StreamEvent::Stop { .. }));
}

#[test]
fn text_after_open_tools_is_replayed_in_order() {
    // Text and reasoning that arrive while tool arguments are still
    // streaming are replayed as their own blocks once the tools stopped,
    // so no block ever receives a delta after its stop.
    let raw = format!(
        "{}{}{}{}{}{}",
        chunk(json!({"reasoning_content": "plan"})),
        chunk(json!({"content": "first"})),
        chunk(json!({"tool_calls": [
            {"index": 0, "id": "call_a", "type": "function",
             "function": {"name": "fa", "arguments": "{}"}},
            {"index": 1, "id": "call_b", "type": "function",
             "function": {"name": "fb", "arguments": "{}"}}
        ]})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "extra"}}]})),
        chunk(json!({"content": "second"})),
        "data: [DONE]\n\n"
    );
    let events = parse(&raw);
    assert_legal_lifecycle(&events);

    assert!(events.contains(&StreamEvent::ThinkingDelta {
        index: 0,
        text: "plan".into()
    }));
    assert!(events.contains(&StreamEvent::TextDelta {
        index: 1,
        text: "first".into()
    }));
    // The late text survives, in a block of its own after the tools.
    let late = events
        .iter()
        .position(|event| matches!(event, StreamEvent::TextDelta { text, .. } if text == "second"))
        .expect("late text must not be dropped");
    for index in [2u32, 3] {
        let stop = events
            .iter()
            .position(|event| matches!(event, StreamEvent::BlockStop { index: i } if *i == index))
            .expect("tool block must stop");
        assert!(
            late > stop,
            "late text must follow the stop of block {index}: {events:?}"
        );
    }
    assert_eq!(tool_arguments(&events, 2), "{}extra");
}

#[test]
fn truncated_stream_closes_blocks_without_claiming_completion() {
    // Only a delta arrives and then the body ends. The parser still closes
    // the open block so the client's block stream is well formed, but it
    // must not synthesize the `Stop` that would present the cut as a
    // finished answer.
    let mut parser = OpenAiStreamParser::new("fallback");
    let mut events = Vec::new();
    for frame in SseDecoder::new().push(chunk(json!({"content": "partial"})).as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());

    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            index: 0,
            text: "partial".into()
        }
    );
    assert_eq!(events[2], StreamEvent::BlockStop { index: 0 });
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. })),
        "a truncated stream must not claim a normal stop: {events:?}"
    );
    assert!(
        !parser.saw_normal_terminal(),
        "no finish_reason and no [DONE] were observed"
    );
}

#[test]
fn a_half_written_tool_argument_is_closed_but_not_completed() {
    let mut parser = OpenAiStreamParser::new("fallback");
    let mut events = Vec::new();
    // The argument JSON is cut in half by the upstream body ending.
    let partial = r#"{"ci"#;
    let rest = r#"ty":"SH"#;
    let raw = format!(
        "{}{}",
        chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                                    "function": {"name": "get_weather", "arguments": partial}}]})
        ),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": rest}}]})),
    );
    for frame in SseDecoder::new().push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());

    assert_eq!(
        events.last(),
        Some(&StreamEvent::BlockStop { index: 0 }),
        "the half-written tool block is closed"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. })),
        "half a tool argument is not a finished answer: {events:?}"
    );
    assert!(!parser.saw_normal_terminal());
}

#[test]
fn a_normal_stop_before_eof_is_a_real_terminal() {
    let mut parser = OpenAiStreamParser::new("fallback");
    let mut events = Vec::new();
    let stop = format!(
        "data: {}\n\n",
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "m",
               "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
    );
    let raw = format!("{}{stop}", chunk(json!({"content": "hi"})));
    for frame in SseDecoder::new().push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    assert!(parser.saw_normal_terminal(), "finish_reason was observed");
    events.extend(parser.finish());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            ..
        })
    ));
}

#[test]
fn an_empty_upstream_body_has_no_terminal_and_no_events() {
    let mut parser = OpenAiStreamParser::new("fallback");
    assert!(
        parser.finish().is_empty(),
        "nothing arrived, nothing to close"
    );
    assert!(
        !parser.saw_normal_terminal(),
        "an empty 200 is not a completed answer"
    );
}

#[test]
fn a_truncated_stream_still_reports_the_usage_it_saw() {
    let mut parser = OpenAiStreamParser::new("fallback");
    let raw = format!(
        "{}{}",
        chunk(json!({"content": "partial"})),
        format_args!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-1", "model": "m", "choices": [],
                   "usage": {"prompt_tokens": 11, "completion_tokens": 7}})
        )
    );
    let mut events = Vec::new();
    for frame in SseDecoder::new().push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());
    assert!(!parser.saw_normal_terminal());
    assert_eq!(parser.reported_usage().input_tokens, 11);
    assert_eq!(parser.reported_usage().output_tokens, 7);
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::Stop { .. })));
}

#[test]
fn stream_error_payload_surfaces() {
    let mut p = OpenAiStreamParser::new("m");
    let f = SseFrame {
        event: None,
        data: r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#.into(),
    };
    assert!(p.push(&f).unwrap_err().to_string().contains("rate limited"));
}

#[test]
fn null_stream_content_is_not_an_error() {
    // `content` is an optional member of the OpenAI delta shape, so an
    // explicit null is a legal chunk that simply carries no text.
    let role_only = parse(&chunk(json!({"role": "assistant", "content": null})));
    assert_eq!(
        role_only.len(),
        1,
        "a role-only delta opens the message and carries no text: {role_only:?}"
    );
    assert!(matches!(role_only[0], StreamEvent::Start { .. }));
    assert!(
        !role_only
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { .. })),
        "an explicit null is not text: {role_only:?}"
    );

    let reasoning_only = parse(&chunk(
        json!({"content": null, "reasoning_content": "думаю"}),
    ));
    assert!(reasoning_only.contains(&StreamEvent::ThinkingDelta {
        index: 0,
        text: "думаю".into()
    }));

    let tool_only = parse(&chunk(json!({
        "content": null,
        "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                        "function": {"name": "get_weather", "arguments": "{}"}}]
    })));
    assert!(tool_only.contains(&StreamEvent::ToolUseStart {
        index: 0,
        id: "call_1".into(),
        name: "get_weather".into()
    }));
    assert!(tool_only.contains(&StreamEvent::ToolUseDelta {
        index: 0,
        partial_json: "{}".into()
    }));
}

#[test]
fn invalid_stream_content_is_an_upstream_payload_error() {
    // A genuinely wrong JSON type is the upstream's bug, not the client's.
    for content in [json!(42), json!({"text": "nope"})] {
        let data = json!({
            "id": "chatcmpl-1",
            "model": "m",
            "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]
        })
        .to_string();
        let err = OpenAiStreamParser::new("m")
            .push(&SseFrame { event: None, data })
            .unwrap_err();
        assert!(
            matches!(err, Error::BadUpstreamPayload(_)),
            "content {content} must be an upstream payload error, got {err:?}"
        );
    }
}

#[test]
fn tool_strict_flags_round_trip_through_openai() {
    let original = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [
            {"type": "function", "function": {"name": "exact", "strict": true,
             "parameters": {"type": "object"}}},
            {"type": "function", "function": {"name": "loose", "strict": false,
             "parameters": {"type": "object"}}},
            {"type": "function", "function": {"name": "silent",
             "parameters": {"type": "object"}}}
        ]
    });
    let req = decode_request(original).unwrap();
    assert_eq!(req.tool_strict.get("exact"), Some(&true));
    assert_eq!(req.tool_strict.get("loose"), Some(&false));
    assert_eq!(req.tool_strict.get("silent"), None);

    let body = encode_request(&req, "up").unwrap();
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(
        tools[0]["function"]["strict"],
        json!(true),
        "an explicit strict request must survive: {body}"
    );
    assert_eq!(tools[1]["function"]["strict"], json!(false));
    assert!(
        tools[2]["function"].get("strict").is_none(),
        "an absent flag must stay absent: {body}"
    );

    let again = decode_request(body).unwrap();
    assert_eq!(again.tool_strict, req.tool_strict);

    let err = decode_request(json!({
        "model": "m",
        "messages": [],
        "tools": [{"type": "function", "function": {"name": "f", "strict": "yes"}}]
    }))
    .unwrap_err();
    assert!(err.to_string().contains("`strict` must be a boolean"));
}

#[test]
fn parallel_tool_calls_round_trips() {
    let req = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"type": "function", "function": {"name": "f"}}],
        "parallel_tool_calls": false
    }))
    .unwrap();
    assert_eq!(req.parallel_tool_use, Some(false));
    let body = encode_request(&req, "up").unwrap();
    assert_eq!(body["parallel_tool_calls"], json!(false));
    assert_eq!(decode_request(body).unwrap().parallel_tool_use, Some(false));

    let absent = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "q"}]
    }))
    .unwrap();
    assert_eq!(absent.parallel_tool_use, None);
    assert!(encode_request(&absent, "up")
        .unwrap()
        .get("parallel_tool_calls")
        .is_none());

    let err = decode_request(json!({
        "model": "m",
        "messages": [],
        "parallel_tool_calls": "no"
    }))
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("`parallel_tool_calls` must be a boolean"));
}

#[test]
fn anthropic_parallel_restriction_maps_to_openai() {
    let req = crate::protocol::anthropic::decode_request(json!({
        "model": "m",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"name": "f", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "any", "disable_parallel_tool_use": true}
    }))
    .unwrap();
    let body = encode_request(&req, "up").unwrap();
    assert_eq!(body["tool_choice"], json!("required"));
    assert_eq!(
        body["parallel_tool_calls"],
        json!(false),
        "the Anthropic restriction must reach OpenAI: {body}"
    );

    let allowed = crate::protocol::anthropic::decode_request(json!({
        "model": "m",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"name": "f", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "auto", "disable_parallel_tool_use": false}
    }))
    .unwrap();
    assert_eq!(
        encode_request(&allowed, "up").unwrap()["parallel_tool_calls"],
        json!(true)
    );
}

#[test]
fn encoder_emits_valid_chunk_sequence() {
    let mut enc = OpenAiStreamEncoder::new("sonnet-class").with_usage(true);
    let mut wire = String::new();
    for ev in [
        StreamEvent::Start {
            id: "msg_1".into(),
            model: "sonnet-class".into(),
            usage: Usage {
                input_tokens: 3,
                ..Usage::default()
            },
        },
        StreamEvent::ThinkingDelta {
            index: 0,
            text: "think".into(),
        },
        StreamEvent::BlockStop { index: 0 },
        StreamEvent::TextDelta {
            index: 1,
            text: "hi".into(),
        },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
                ..Usage::default()
            },
        },
    ] {
        for f in enc.encode(&ev) {
            wire.push_str(&f.to_wire());
        }
    }
    assert!(enc.finish().is_empty());
    assert!(wire.ends_with("data: [DONE]\n\n"));
    assert!(wire.contains("\"reasoning_content\":\"think\""));
    assert!(wire.contains("\"finish_reason\":\"stop\""));
    assert!(wire.contains("\"prompt_tokens\":3"));
    // role, reasoning, text, finish, usage
    assert_eq!(wire.matches("chatcmpl-msg_1").count(), 5);

    // Feeding our own output back through the parser is lossless for text.
    let events = parse(&wire);
    assert!(events.contains(&StreamEvent::ThinkingDelta {
        index: 0,
        text: "think".into()
    }));
    assert!(events.contains(&StreamEvent::TextDelta {
        index: 1,
        text: "hi".into()
    }));
}

#[test]
fn streamed_chunks_report_the_client_facing_model() {
    // The upstream names what it served ("deepseek-v4-flash"); the client
    // asked for an exposed id and must see that id, exactly as the
    // non-streaming encoder reports it.
    let mut enc = OpenAiStreamEncoder::new("deepseek-deepseek-v4-flash");
    let frames = enc.encode(&StreamEvent::Start {
        id: "chatcmpl-upstream".into(),
        model: "deepseek-v4-flash".into(),
        usage: Usage::default(),
    });
    let chunk: Value = serde_json::from_str(&frames[0].data).unwrap();
    assert_eq!(chunk["model"], "deepseek-deepseek-v4-flash");
}

#[test]
fn encoder_maps_block_indices_to_tool_call_slots() {
    let mut enc = OpenAiStreamEncoder::new("m");
    let mut wire = String::new();
    for ev in [
        StreamEvent::ToolUseStart {
            index: 3,
            id: "a".into(),
            name: "fa".into(),
        },
        StreamEvent::ToolUseStart {
            index: 7,
            id: "b".into(),
            name: "fb".into(),
        },
        StreamEvent::ToolUseDelta {
            index: 7,
            partial_json: "{}".into(),
        },
    ] {
        for f in enc.encode(&ev) {
            wire.push_str(&f.to_wire());
        }
    }
    assert!(wire.contains("\"index\":0,\"id\":\"a\""));
    assert!(wire.contains("\"index\":1,\"id\":\"b\""));
    // the delta for block 7 reuses slot 1
    assert_eq!(wire.matches("\"index\":1").count(), 2);
}

#[test]
fn encoder_without_usage_option_omits_the_trailer() {
    let mut enc = OpenAiStreamEncoder::new("m");
    let frames = enc.encode(&StreamEvent::Stop {
        stop_reason: StopReason::MaxTokens,
        stop_sequence: None,
        usage: Usage {
            output_tokens: 5,
            ..Usage::default()
        },
    });
    let wire: String = frames.iter().map(|f| f.to_wire()).collect();
    assert!(!wire.contains("prompt_tokens"));
    assert!(wire.contains("\"finish_reason\":\"length\""));
}

#[test]
fn error_frames_terminate_the_stream() {
    let mut enc = OpenAiStreamEncoder::new("m");
    let frames = enc.error(&Error::Unauthorized);
    assert!(frames[0].data.contains("authentication_error"));
    assert_eq!(frames[1].data, "[DONE]");
}

#[test]
fn assistant_reasoning_content_survives_the_ir_round_trip() {
    let req = decode_request(json!({
        "model": "deepseek-v4-flash",
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello", "reasoning_content": "thinking hard"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "ls", "arguments": "{}"}}
            ], "reasoning_content": "need to list"},
            {"role": "tool", "tool_call_id": "call_1", "content": "a.txt"},
        ]
    }))
    .unwrap();
    let body = encode_request(&req, "deepseek-v4-flash").unwrap();
    let msgs = body["messages"].as_array().unwrap();

    // Plain assistant text turn keeps its reasoning.
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["reasoning_content"], "thinking hard");
    assert_eq!(msgs[1]["content"], "hello");

    // Tool-call turn keeps both reasoning and the calls.
    assert_eq!(msgs[2]["reasoning_content"], "need to list");
    assert!(msgs[2]["tool_calls"].is_array());

    // Tool result round-trips with its call id.
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_1");
}

#[test]
fn anthropic_sourced_thinking_is_not_echoed_as_reasoning_content() {
    let mut req = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .unwrap();
    req.source_dialect = Dialect::Anthropic;
    req.messages.push(crate::ir::Message {
        role: crate::ir::Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                text: "chain of thought".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::text("answer"),
        ],
    });
    assert!(encode_request(&req, "m").is_err());
    req.unsupported_content_policy = UnsupportedContentPolicy::Placeholder;
    let body = encode_request(&req, "m").unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert!(msgs[1].get("reasoning_content").is_none());
    assert!(msgs[1]["content"].as_str().unwrap().contains("Unsupported"));
    assert!(msgs[1]["content"].as_str().unwrap().contains("answer"));
}

#[test]
fn the_official_file_id_part_decodes_and_re_encodes() {
    let body = json!({
        "model": "m",
        "messages": [{
            "role": "user",
            "content": [{"type": "file", "file": {"file_id": "file-123"}}]
        }]
    });
    let req = decode_request(body).unwrap();
    assert_eq!(
        req.messages[0].content,
        vec![ContentBlock::File {
            source: MediaSource::Reference {
                id: "file-123".into()
            },
            media_type: "application/octet-stream".into(),
            name: None,
        }]
    );
    let wire = encode_request(&req, "m").unwrap();
    assert_eq!(
        wire["messages"][0]["content"][0],
        json!({"type": "file", "file": {"file_id": "file-123"}})
    );
}

#[test]
fn the_official_file_data_part_decodes_and_re_encodes() {
    let body = json!({
        "model": "m",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "file",
                "file": {
                    "file_data": "data:application/pdf;base64,QQ==",
                    "filename": "a.pdf"
                }
            }]
        }]
    });
    let req = decode_request(body).unwrap();
    assert_eq!(
        req.messages[0].content,
        vec![ContentBlock::File {
            source: MediaSource::Base64 {
                media_type: "application/pdf".into(),
                data: "QQ==".into(),
            },
            media_type: "application/octet-stream".into(),
            name: Some("a.pdf".into()),
        }]
    );
    let wire = encode_request(&req, "m").unwrap();
    assert_eq!(
        wire["messages"][0]["content"][0],
        json!({
            "type": "file",
            "file": {
                "file_data": "data:application/pdf;base64,QQ==",
                "filename": "a.pdf"
            }
        })
    );
}

#[test]
fn a_file_without_a_reference_still_fails_closed() {
    let body = json!({
        "model": "m",
        "messages": [{
            "role": "user",
            "content": [{"type": "file", "file": {"filename": "a.pdf"}}]
        }]
    });
    let err = decode_request(body).unwrap_err().to_string();
    assert!(err.contains("file content is missing"), "{err}");
}

#[test]
fn a_refusal_only_answer_keeps_its_text_and_field() {
    let body = json!({
        "id": "chatcmpl-1",
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "refusal": "I cannot assist with that request."
            },
            "finish_reason": "stop"
        }]
    });
    let resp = decode_response(body).unwrap();
    assert_eq!(resp.text(), "I cannot assist with that request.");
    assert_eq!(
        resp.passthrough.get("refusal").and_then(Value::as_str),
        Some("I cannot assist with that request.")
    );

    // The same dialect gets OpenAI's own shape back, not an empty success.
    let wire = encode_response(&resp);
    assert_eq!(wire["choices"][0]["message"]["content"], Value::Null);
    assert_eq!(
        wire["choices"][0]["message"]["refusal"],
        "I cannot assist with that request."
    );

    // Another dialect still shows the explanation as text.
    let other = crate::protocol::anthropic::encode_response(&resp);
    assert_eq!(
        other["content"][0]["text"],
        "I cannot assist with that request."
    );
}

#[test]
fn a_streamed_refusal_delta_is_not_dropped() {
    let mut parser = OpenAiStreamParser::new("fallback");
    let mut frames = SseDecoder::new().push(
        b"data: {\"id\":\"chatcmpl-1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"no\"},\"finish_reason\":null}]}\n\n",
    );
    frames.extend(SseDecoder::new().push(
        b"data: {\"id\":\"chatcmpl-1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"refusal\":null},\"finish_reason\":\"stop\"}]}\n\n",
    ));
    let mut events = Vec::new();
    for frame in frames {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::TextDelta { text, .. } if text == "no"
        )),
        "{events:?}"
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
}
