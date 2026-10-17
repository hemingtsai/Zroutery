//! The Anthropic dialect fixtures.

use serde_json::{json, Value};

use crate::error::Error;
use crate::ir::{
    ChatRequest, ContentBlock, Dialect, StopReason, StreamEvent, SystemPart, ToolChoice, Usage,
};
use crate::protocol::{SseDecoder, SseFrame, StreamEncoder, StreamParser};

use super::*;
fn frames_to_events(raw: &str, model: &str) -> Vec<StreamEvent> {
    let mut dec = SseDecoder::new();
    let mut parser = AnthropicStreamParser::new(model);
    let mut events = Vec::new();
    for f in dec.push(raw.as_bytes()) {
        events.extend(parser.push(&f).unwrap());
    }
    events.extend(parser.finish());
    events
}

#[test]
fn decodes_string_and_block_content() {
    let req = decode_request(json!({
        "model": "sonnet-class",
        "max_tokens": 100,
        "system": "be brief",
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
        ]
    }))
    .unwrap();
    assert_eq!(req.model, "sonnet-class");
    assert_eq!(req.system, vec![SystemPart::new("be brief")]);
    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.messages[0].content[0], ContentBlock::text("hi"));
    assert_eq!(req.max_tokens, Some(100));
    assert!(!req.stream);
}

#[test]
fn decodes_system_blocks_tools_and_thinking() {
    let req = decode_request(json!({
        "model": "m",
        "system": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
        "messages": [{"role": "user", "content": "x"}],
        "tools": [{"name": "get_weather", "description": "d", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "tool", "name": "get_weather"},
        "thinking": {"type": "enabled", "budget_tokens": 1024},
        "metadata": {"user_id": "u1"},
        "top_k": 40,
        "stop_sequences": ["STOP"],
        "beta_flag": true
    }))
    .unwrap();
    assert_eq!(req.system, vec![SystemPart::new("a"), SystemPart::new("b")]);
    assert_eq!(req.tools[0].name, "get_weather");
    assert_eq!(
        req.tool_choice,
        Some(ToolChoice::Specific {
            name: "get_weather".into()
        })
    );
    assert_eq!(req.thinking.unwrap().budget_tokens, Some(1024));
    assert_eq!(req.metadata_user.as_deref(), Some("u1"));
    assert_eq!(req.top_k, Some(40));
    assert_eq!(req.stop_sequences, vec!["STOP"]);
    assert_eq!(req.passthrough.get("beta_flag"), Some(&json!(true)));
}

#[test]
fn tool_choice_parallel_restriction_round_trips() {
    for kind in ["auto", "any", "tool", "none"] {
        for allow in [true, false] {
            let mut choice = json!({"type": kind, "disable_parallel_tool_use": !allow});
            if kind == "tool" {
                choice["name"] = json!("f");
            }
            let body = json!({
                "model": "m",
                "messages": [{"role": "user", "content": "q"}],
                "tools": [{"name": "f", "input_schema": {"type": "object"}}],
                "tool_choice": choice
            });
            let req = decode_request(body.clone()).unwrap();
            assert_eq!(
                req.parallel_tool_use,
                Some(allow),
                "decode lost the restriction: {body}"
            );
            let encoded = encode_request(&req, "claude").unwrap();
            assert_eq!(encoded["tool_choice"]["type"], kind);
            assert_eq!(
                encoded["tool_choice"]["disable_parallel_tool_use"],
                json!(!allow),
                "encode lost the restriction: {encoded}"
            );
            assert_eq!(
                decode_request(encoded).unwrap().parallel_tool_use,
                Some(allow)
            );
        }
    }

    // No flag at all stays absent rather than becoming an explicit default.
    let req = decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "q"}],
        "tool_choice": {"type": "auto"}
    }))
    .unwrap();
    assert_eq!(req.parallel_tool_use, None);
    let encoded = encode_request(&req, "claude").unwrap();
    assert!(encoded["tool_choice"]
        .get("disable_parallel_tool_use")
        .is_none());
}

#[test]
fn openai_parallel_restriction_is_encoded_for_anthropic() {
    let req = crate::protocol::openai::decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"type": "function", "function": {"name": "f"}}],
        "parallel_tool_calls": false
    }))
    .unwrap();
    let body = encode_request(&req, "claude").unwrap();
    assert_eq!(body["tool_choice"]["type"], "auto");
    assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);
}

#[test]
fn decodes_tool_use_and_tool_result_round_trip() {
    let original = json!({
        "model": "m",
        "max_tokens": 50,
        "messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SH"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "sunny", "is_error": false}
            ]}
        ]
    });
    let req = decode_request(original).unwrap();
    let encoded = encode_request(&req, "upstream-model").unwrap();
    assert_eq!(encoded["model"], "upstream-model");
    assert_eq!(encoded["messages"][1]["content"][1]["name"], "get_weather");
    assert_eq!(encoded["messages"][1]["content"][0]["signature"], "sig");
    assert_eq!(
        encoded["messages"][2]["content"][0]["content"][0]["text"],
        "sunny"
    );
    // Re-decoding the encoded form yields the same IR.
    let again = decode_request(encoded).unwrap();
    assert_eq!(again.messages, req.messages);
}

#[test]
fn encode_request_fills_required_max_tokens() {
    let req = decode_request(json!({"model": "m", "messages": []})).unwrap();
    let body = encode_request(&req, "m").unwrap();
    assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    assert!(body.get("system").is_none());
}

#[test]
fn strict_tool_schema_is_rejected_instead_of_dropped() {
    let req = crate::protocol::openai::decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"type": "function", "function": {"name": "f", "strict": true,
                   "parameters": {"type": "object"}}}]
    }))
    .unwrap();
    let err = encode_request(&req, "claude").unwrap_err();
    assert!(
        err.to_string().contains("strict"),
        "the strict requirement must not be dropped silently: {err}"
    );

    // An explicitly non-strict tool is ordinary and still encodes.
    let relaxed = crate::protocol::openai::decode_request(json!({
        "model": "m",
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"type": "function", "function": {"name": "f", "strict": false,
                   "parameters": {"type": "object"}}}]
    }))
    .unwrap();
    let body = encode_request(&relaxed, "claude").unwrap();
    assert_eq!(body["tools"][0]["name"], "f");
}

#[test]
fn translated_reasoning_budget_fits_anthropic_limits() {
    // An OpenAI `reasoning_effort` becomes an Anthropic thinking budget;
    // whatever the effort and the output cap, the emitted body must honour
    // 1024 <= budget_tokens < max_tokens.
    for (effort, cap, expected) in [
        ("low", None, 1024u32),
        ("medium", None, 4095),
        ("high", None, 4095),
        ("low", Some(64_000u32), 1024),
        ("medium", Some(64_000), 4096),
        ("high", Some(64_000), 16384),
        ("high", Some(2048), 2047),
        ("high", Some(1025), 1024),
    ] {
        let mut body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "q"}],
            "reasoning_effort": effort
        });
        if let Some(cap) = cap {
            body["max_tokens"] = json!(cap);
        }
        let req = crate::protocol::openai::decode_request(body).unwrap();
        let encoded = encode_request(&req, "claude").unwrap();
        let budget = encoded["thinking"]["budget_tokens"].as_u64().unwrap();
        let max_tokens = encoded["max_tokens"].as_u64().unwrap();
        assert_eq!(budget, expected as u64, "effort={effort} cap={cap:?}");
        assert!(
            budget >= MIN_THINKING_BUDGET as u64 && budget < max_tokens,
            "effort={effort} cap={cap:?} produced an invalid body: {encoded}"
        );
    }
}

#[test]
fn reasoning_budget_that_cannot_fit_the_cap_is_refused() {
    // `1024 <= budget < max_tokens` has no solution at these caps, so the
    // proxy must say so rather than emit an invalid body.
    for cap in [1u32, 1024] {
        let req = crate::protocol::openai::decode_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "q"}],
            "reasoning_effort": "high",
            "max_tokens": cap
        }))
        .unwrap();
        let err = encode_request(&req, "claude").unwrap_err();
        assert!(
            err.to_string().contains("thinking budget"),
            "cap={cap} must be refused explicitly, got {err}"
        );
    }
}

#[test]
fn native_anthropic_thinking_is_forwarded_verbatim() {
    // A client that speaks Anthropic owns its thinking parameters; only a
    // budget this proxy derived from another dialect is re-fitted.
    let req = decode_request(json!({
        "model": "m",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "q"}],
        "thinking": {"type": "enabled", "budget_tokens": 2048}
    }))
    .unwrap();
    let body = encode_request(&req, "claude").unwrap();
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["thinking"]["budget_tokens"], 2048);
}

#[test]
fn passthrough_is_not_leaked_across_dialects() {
    let mut req = ChatRequest::new("m", Dialect::OpenAI);
    req.passthrough
        .insert("frequency_penalty".into(), json!(0.5));
    let body = encode_request(&req, "m").unwrap();
    assert!(body.get("frequency_penalty").is_none());
}

#[test]
fn response_round_trip() {
    let body = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude",
        "content": [{"type": "text", "text": "hi"}],
        "stop_reason": "end_turn",
        "stop_sequence": Value::Null,
        "usage": {"input_tokens": 10, "output_tokens": 3, "cache_read_input_tokens": 2}
    });
    let resp = decode_response(body).unwrap();
    assert_eq!(resp.text(), "hi");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    // 10 cache-exclusive input tokens plus 2 cache reads are a 12 token IR
    // prompt total; encoding splits them back apart.
    assert_eq!(resp.usage.input_tokens, 12);
    assert_eq!(resp.usage.fresh_input_tokens(), 10);
    assert_eq!(resp.usage.cache_read_tokens, 2);
    let back = encode_response(&resp);
    assert_eq!(back["content"][0]["text"], "hi");
    assert_eq!(back["stop_reason"], "end_turn");
    assert_eq!(back["usage"]["input_tokens"], 10);
    assert_eq!(back["usage"]["cache_read_input_tokens"], 2);
}

#[test]
fn usage_normalizes_the_cache_counters_into_the_total() {
    // The report's synthetic frame: 100 fresh, 10,000 cache reads, 200 cache
    // writes. Anthropic's `input_tokens` is cache-exclusive, so the IR total
    // is 10,300 and the encoder splits it back into the same three numbers.
    let body = json!({
        "id": "msg_cache",
        "type": "message",
        "role": "assistant",
        "model": "claude",
        "content": [{"type": "text", "text": "hi"}],
        "stop_reason": "end_turn",
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": 100,
            "output_tokens": 0,
            "cache_read_input_tokens": 10_000,
            "cache_creation_input_tokens": 200,
        }
    });
    let resp = decode_response(body).unwrap();
    assert_eq!(resp.usage.input_tokens, 10_300);
    assert_eq!(resp.usage.fresh_input_tokens(), 100);
    assert_eq!(resp.usage.cache_read_tokens, 10_000);
    assert_eq!(resp.usage.cache_write_tokens, 200);
    assert!(
        resp.usage.cache_read_tokens + resp.usage.cache_write_tokens <= resp.usage.input_tokens
    );

    let back = encode_response(&resp);
    assert_eq!(back["usage"]["input_tokens"], 100);
    assert_eq!(back["usage"]["cache_read_input_tokens"], 10_000);
    assert_eq!(back["usage"]["cache_creation_input_tokens"], 200);

    // And the normalized numbers survive a second full round-trip.
    let again = decode_response(back).unwrap();
    assert_eq!(again.usage, resp.usage);
}

#[test]
fn stream_final_usage_restates_the_cache_counters() {
    // `MessageDeltaUsage` accepts the cumulative input and cache counters,
    // and the final delta is the last word on usage.
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":10000,\"cache_creation_input_tokens\":200,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":10000,\"cache_creation_input_tokens\":200,\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let events = frames_to_events(raw, "claude");
    let stop_usage = match events.last().unwrap() {
        StreamEvent::Stop { usage, .. } => *usage,
        other => panic!("unexpected tail {other:?}"),
    };
    assert_eq!(stop_usage.input_tokens, 10_300);
    assert_eq!(stop_usage.cache_read_tokens, 10_000);
    assert_eq!(stop_usage.cache_write_tokens, 200);
    assert_eq!(stop_usage.output_tokens, 5);

    // Re-encoding for an Anthropic client restates the complete final usage
    // on the last frame instead of only the output count.
    let mut enc = AnthropicStreamEncoder::new("claude");
    let mut wire = String::new();
    for ev in &events {
        for f in enc.encode(ev) {
            wire.push_str(&f.to_wire());
        }
    }
    let delta = wire
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find(|v| v["type"] == "message_delta")
        .expect("a message_delta frame");
    assert_eq!(delta["usage"]["input_tokens"], 100);
    assert_eq!(delta["usage"]["cache_read_input_tokens"], 10_000);
    assert_eq!(delta["usage"]["cache_creation_input_tokens"], 200);
    assert_eq!(delta["usage"]["output_tokens"], 5);
}

#[test]
fn stream_delta_usage_alone_fills_the_final_cache_counters() {
    // A stream whose `message_start` usage is empty (the OpenAI trailer
    // shape) must still report the cache counters it learns at the end.
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":60,\"cache_read_input_tokens\":40,\"output_tokens\":10}}\n\n",
    );
    let events = frames_to_events(raw, "claude");
    let stop_usage = match events.last().unwrap() {
        StreamEvent::Stop { usage, .. } => *usage,
        other => panic!("unexpected tail {other:?}"),
    };
    assert_eq!(stop_usage.input_tokens, 100);
    assert_eq!(stop_usage.cache_read_tokens, 40);
    assert_eq!(stop_usage.output_tokens, 10);
}

#[test]
fn stream_delta_merge_keeps_counters_it_omits() {
    // `message_start` reported 10,000 cache reads and 200 cache writes; the
    // final delta restates only the fresh input, the cache reads and the
    // output. The omitted write counter must survive the cumulative merge
    // rather than being recomputed away.
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":10000,\"cache_creation_input_tokens\":200,\"output_tokens\":0}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":10000,\"output_tokens\":5}}\n\n",
    );
    let events = frames_to_events(raw, "claude");
    let stop_usage = match events.last().unwrap() {
        StreamEvent::Stop { usage, .. } => *usage,
        other => panic!("unexpected tail {other:?}"),
    };
    assert_eq!(stop_usage.input_tokens, 10_300);
    assert_eq!(stop_usage.fresh_input_tokens(), 100);
    assert_eq!(stop_usage.cache_read_tokens, 10_000);
    assert_eq!(stop_usage.cache_write_tokens, 200);
    assert_eq!(stop_usage.output_tokens, 5);
}

#[test]
fn error_response_becomes_error() {
    let err = decode_response(json!({
        "type": "error",
        "error": {"type": "overloaded_error", "message": "too busy"}
    }))
    .unwrap_err();
    assert!(err.to_string().contains("too busy"));
}

#[test]
fn parses_a_full_text_stream() {
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_9\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"He\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"llo\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let events = frames_to_events(raw, "fallback");
    assert_eq!(
        events[0],
        StreamEvent::Start {
            id: "msg_9".into(),
            model: "claude-x".into(),
            usage: Usage {
                input_tokens: 7,
                ..Usage::default()
            }
        }
    );
    assert!(events.contains(&StreamEvent::Ping));
    assert!(events.contains(&StreamEvent::TextDelta {
        index: 0,
        text: "He".into()
    }));
    match events.last().unwrap() {
        StreamEvent::Stop {
            stop_reason, usage, ..
        } => {
            assert_eq!(*stop_reason, StopReason::EndTurn);
            assert_eq!(usage.input_tokens, 7);
            assert_eq!(usage.output_tokens, 5);
        }
        other => panic!("unexpected tail {other:?}"),
    }
}

#[test]
fn parses_thinking_and_tool_use_stream() {
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"c\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"fn\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\n",
    );
    let events = frames_to_events(raw, "c");
    assert!(events.contains(&StreamEvent::ThinkingDelta {
        index: 0,
        text: "plan".into()
    }));
    assert!(events.contains(&StreamEvent::ThinkingSignature {
        index: 0,
        signature: "sig".into()
    }));
    assert!(events.contains(&StreamEvent::ToolUseStart {
        index: 1,
        id: "tu".into(),
        name: "fn".into()
    }));
    assert!(events.contains(&StreamEvent::ToolUseDelta {
        index: 1,
        partial_json: "{\"a\":".into()
    }));
    assert!(matches!(
        events.last().unwrap(),
        StreamEvent::Stop {
            stop_reason: StopReason::ToolUse,
            ..
        }
    ));
}

#[test]
fn parser_closes_dangling_blocks_on_truncated_stream() {
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"c\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
    );
    let events = frames_to_events(raw, "c");
    assert_eq!(
        events[events.len() - 1],
        StreamEvent::BlockStop { index: 0 },
        "the dangling block is still closed: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. })),
        "a body that merely stopped arriving is not a finished message: {events:?}"
    );
}

#[test]
fn truncated_parser_reports_no_normal_terminal_and_keeps_usage() {
    let mut parser = AnthropicStreamParser::new("c");
    let raw = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"c\",\"usage\":{\"input_tokens\":42,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"fn\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
    );
    let mut events = Vec::new();
    for frame in SseDecoder::new().push(raw.as_bytes()) {
        events.extend(parser.push(&frame).unwrap());
    }
    events.extend(parser.finish());

    assert!(!parser.saw_normal_terminal());
    assert_eq!(parser.reported_usage().input_tokens, 42);
    assert_eq!(events.last(), Some(&StreamEvent::BlockStop { index: 0 }));
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::Stop { .. })));
}

#[test]
fn a_normal_message_terminal_is_a_real_terminal() {
    for tail in [
        concat!(
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
        // `message_stop` alone still names a genuine end.
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ] {
        let raw = format!(
            "{}{tail}",
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"c\"}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            )
        );
        let mut parser = AnthropicStreamParser::new("c");
        let mut events = Vec::new();
        for frame in SseDecoder::new().push(raw.as_bytes()) {
            events.extend(parser.push(&frame).unwrap());
        }
        assert!(parser.saw_normal_terminal(), "tail: {tail}");
        events.extend(parser.finish());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::Stop { .. })),
            "a real terminal still yields a Stop: {events:?}"
        );
    }
}

#[test]
fn an_empty_upstream_body_has_no_terminal_and_no_events() {
    let mut parser = AnthropicStreamParser::new("c");
    assert!(parser.finish().is_empty());
    assert!(!parser.saw_normal_terminal());
}

#[test]
fn stream_error_frame_surfaces_as_error() {
    let mut parser = AnthropicStreamParser::new("m");
    let frame = SseFrame {
        event: Some("error".into()),
        data: r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#.into(),
    };
    assert!(parser
        .push(&frame)
        .unwrap_err()
        .to_string()
        .contains("busy"));
}

#[test]
fn encoder_emits_block_lifecycle() {
    let mut enc = AnthropicStreamEncoder::new("m");
    let mut wire = String::new();
    for f in enc.encode(&StreamEvent::Start {
        id: "msg_1".into(),
        model: "gpt".into(),
        usage: Usage {
            input_tokens: 4,
            ..Usage::default()
        },
    }) {
        wire.push_str(&f.to_wire());
    }
    for f in enc.encode(&StreamEvent::TextDelta {
        index: 0,
        text: "hi".into(),
    }) {
        wire.push_str(&f.to_wire());
    }
    for f in enc.encode(&StreamEvent::TextDelta {
        index: 0,
        text: "!".into(),
    }) {
        wire.push_str(&f.to_wire());
    }
    for f in enc.encode(&StreamEvent::Stop {
        stop_reason: StopReason::EndTurn,
        stop_sequence: None,
        usage: Usage {
            input_tokens: 4,
            output_tokens: 2,
            ..Usage::default()
        },
    }) {
        wire.push_str(&f.to_wire());
    }
    assert!(
        enc.finish().is_empty(),
        "stop already terminated the stream"
    );

    // Exactly one content_block_start for two deltas, and a clean shutdown.
    assert_eq!(wire.matches("event: content_block_start").count(), 1);
    assert_eq!(wire.matches("event: content_block_delta").count(), 2);
    assert_eq!(wire.matches("event: content_block_stop").count(), 1);
    assert!(wire.contains("event: message_start"));
    assert!(wire.contains("event: message_stop"));

    // And it re-parses into the same canonical events. The `model` is the
    // one the encoder was built with — the id the client asked for — not
    // the upstream name carried by the event.
    let events = frames_to_events(&wire, "m");
    assert_eq!(
        events[0],
        StreamEvent::Start {
            id: "msg_1".into(),
            model: "m".into(),
            usage: Usage {
                input_tokens: 4,
                ..Usage::default()
            }
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            index: 0,
            text: "hi".into()
        }
    );
    assert_eq!(
        events[2],
        StreamEvent::TextDelta {
            index: 0,
            text: "!".into()
        }
    );
    assert_eq!(events[3], StreamEvent::BlockStop { index: 0 });
}

#[test]
fn encoder_switches_block_kind_and_finishes_unterminated_streams() {
    let mut enc = AnthropicStreamEncoder::new("m");
    let mut wire = String::new();
    for ev in [
        StreamEvent::ThinkingDelta {
            index: 0,
            text: "think".into(),
        },
        StreamEvent::BlockStop { index: 0 },
        StreamEvent::TextDelta {
            index: 1,
            text: "answer".into(),
        },
    ] {
        for f in enc.encode(&ev) {
            wire.push_str(&f.to_wire());
        }
    }
    for f in enc.finish() {
        wire.push_str(&f.to_wire());
    }
    assert!(wire.contains("\"type\":\"thinking\""));
    assert!(wire.contains("thinking_delta"));
    assert_eq!(wire.matches("event: content_block_stop").count(), 2);
    assert!(wire.contains("event: message_stop"));
    let events = frames_to_events(&wire, "m");
    assert!(events.contains(&StreamEvent::ThinkingDelta {
        index: 0,
        text: "think".into()
    }));
    assert!(events.contains(&StreamEvent::TextDelta {
        index: 1,
        text: "answer".into()
    }));
}

#[test]
fn encoder_reports_errors_in_band() {
    let mut enc = AnthropicStreamEncoder::new("m");
    let frames = enc.error(&Error::Timeout(30));
    assert_eq!(frames[0].event.as_deref(), Some("error"));
    let v = frames[0].json().unwrap();
    assert_eq!(v["error"]["type"], "timeout_error");
}
