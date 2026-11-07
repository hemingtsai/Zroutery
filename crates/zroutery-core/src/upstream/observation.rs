//! Allowlisted metadata capture before an error parser or normalizer discards a body.
use crate::ir::{AttemptObservation, Dialect, StreamEvent, Usage};
use serde_json::Value;

/// What the wire carried for one dialect's usage object.
enum WireUsage {
    /// The documented wire carried no usage here (or an explicitly null one).
    Absent,
    /// The wire carried a usage object that the protocol's documented shape
    /// cannot make trusted: a required total missing or malformed, or a
    /// present counter that is not a measurement. Nothing may be recorded
    /// and no decoder default may stand in for it.
    Invalid,
    /// A well-formed reading: totals stated, counters trusted.
    Measured(Usage),
}

/// One strictly read wire member.
enum Member {
    /// The member is absent (or explicitly null): documented "not reported".
    Absent,
    /// The member is present but not a trusted counter: negative,
    /// fractional, a string, or beyond the counter width.
    Invalid,
    Value(u32),
}

fn member(u: &Value, key: &str) -> Member {
    match u.get(key) {
        None | Some(Value::Null) => Member::Absent,
        Some(value) => match value.as_u64().and_then(|value| u32::try_from(value).ok()) {
            Some(value) => Member::Value(value),
            None => Member::Invalid,
        },
    }
}

/// Read a member nested inside detail objects. A missing or null
/// intermediate is "not reported", not malformed; a present intermediate
/// that is not an object is malformed evidence — a counter hiding under a
/// wrong-shaped parent must not become a documented zero.
fn nested_member(u: &Value, path: &[&str]) -> Member {
    let (last, parents) = path.split_last().expect("a member path is not empty");
    let mut at = u;
    for key in parents {
        match at.get(key) {
            None | Some(Value::Null) => return Member::Absent,
            Some(value) if value.is_object() => at = value,
            Some(_) => return Member::Invalid,
        }
    }
    member(at, last)
}

/// An optional counter: absent is the documented zero, a present malformed
/// value refuses the whole reading.
fn optional_counter(counter: Member) -> Result<u32, ()> {
    match counter {
        Member::Absent => Ok(0),
        Member::Value(value) => Ok(value),
        Member::Invalid => Err(()),
    }
}

pub(super) fn capture(observation: &AttemptObservation, dialect: Dialect, body: &Value) {
    let inner = body
        .get("response")
        .or_else(|| body.get("message"))
        .unwrap_or(body);
    let mut declaration = crate::ir::declaration_from_wire(body);
    if !std::ptr::eq(inner, body) {
        declaration.merge(&crate::ir::declaration_from_wire(inner));
    }
    let wire = match dialect {
        Dialect::OpenAIResponses => {
            inner
                .get("usage")
                .filter(|u| !u.is_null())
                .map_or(WireUsage::Absent, |usage| {
                    // `input_tokens` and `output_tokens` are required members of
                    // the documented usage object; a usage object that cannot
                    // state its own totals cannot be trusted for any axis, so
                    // the whole reading is refused instead of filling an axis
                    // with zero. The detail counters are optional: absent is the
                    // documented zero, a present malformed value refuses.
                    let input = match member(usage, "input_tokens") {
                        Member::Value(value) => value,
                        _ => return WireUsage::Invalid,
                    };
                    let output = match member(usage, "output_tokens") {
                        Member::Value(value) => value,
                        _ => return WireUsage::Invalid,
                    };
                    let cache_read = match optional_counter(nested_member(
                        usage,
                        &["input_tokens_details", "cached_tokens"],
                    )) {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    let reasoning = match optional_counter(nested_member(
                        usage,
                        &["output_tokens_details", "reasoning_tokens"],
                    )) {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    WireUsage::Measured(Usage {
                        input_tokens: input,
                        output_tokens: output,
                        cache_read_tokens: cache_read,
                        cache_write_tokens: 0,
                        reasoning_tokens: reasoning,
                    })
                })
        }
        Dialect::OpenAI => {
            inner
                .get("usage")
                .filter(|u| !u.is_null())
                .map_or(WireUsage::Absent, |usage| {
                    // A chat usage object states both totals; the SSE frames that
                    // carry none send an explicit null instead of a partial
                    // object. Detail counters are optional; a relay-declared
                    // cache-write counter is merged into the prompt total.
                    let prompt = match member(usage, "prompt_tokens") {
                        Member::Value(value) => value,
                        _ => return WireUsage::Invalid,
                    };
                    let completion = match member(usage, "completion_tokens") {
                        Member::Value(value) => value,
                        _ => return WireUsage::Invalid,
                    };
                    // The two cache-write spellings carry different subset
                    // semantics: the top-level Anthropic-style counter is
                    // cache-exclusive (it is added to the prompt total),
                    // while a `prompt_tokens_details.cache_write_tokens`
                    // breakdown is already part of `prompt_tokens` and must
                    // not be added on top. Both present and disagreeing is
                    // self-contradictory wire evidence.
                    let creation =
                        match optional_counter(member(usage, "cache_creation_input_tokens")) {
                            Ok(value) => value,
                            Err(()) => return WireUsage::Invalid,
                        };
                    let detail_write = match optional_counter(nested_member(
                        usage,
                        &["prompt_tokens_details", "cache_write_tokens"],
                    )) {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    let (input, write) = if creation > 0 {
                        if detail_write != 0 && detail_write != creation {
                            return WireUsage::Invalid;
                        }
                        match prompt.checked_add(creation) {
                            Some(value) => (value, creation),
                            None => return WireUsage::Invalid,
                        }
                    } else if detail_write > 0 {
                        (prompt, detail_write)
                    } else {
                        (prompt, 0)
                    };
                    let read = match member(usage, "prompt_cache_hit_tokens") {
                        Member::Absent => {
                            match optional_counter(nested_member(
                                usage,
                                &["prompt_tokens_details", "cached_tokens"],
                            )) {
                                Ok(value) => value,
                                Err(()) => return WireUsage::Invalid,
                            }
                        }
                        Member::Value(value) => value,
                        Member::Invalid => return WireUsage::Invalid,
                    };
                    let reasoning = match optional_counter(nested_member(
                        usage,
                        &["completion_tokens_details", "reasoning_tokens"],
                    )) {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    WireUsage::Measured(Usage {
                        input_tokens: input,
                        output_tokens: completion,
                        cache_read_tokens: read,
                        cache_write_tokens: write,
                        reasoning_tokens: reasoning,
                    })
                })
        }
        Dialect::Anthropic => {
            inner
                .get("usage")
                .filter(|u| !u.is_null())
                .map_or(WireUsage::Absent, |usage| {
                    // `message_delta` restates cumulative counters one field at a
                    // time, so a usage object that names only some totals is a
                    // legal partial frame, not malformed evidence: it records
                    // nothing and flags nothing. But every member that IS
                    // present must still be a measurement — a malformed
                    // counter refuses the reading even when the frame would
                    // otherwise be a legal partial.
                    let fresh = member(usage, "input_tokens");
                    let output = member(usage, "output_tokens");
                    let read = match optional_counter(member(usage, "cache_read_input_tokens")) {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    let write = match optional_counter(member(usage, "cache_creation_input_tokens"))
                    {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    let (fresh, output) = match (fresh, output) {
                        (Member::Value(fresh), Member::Value(output)) => (fresh, output),
                        (Member::Invalid, _) | (_, Member::Invalid) => {
                            return WireUsage::Invalid;
                        }
                        // A legal partial frame: some totals simply were not
                        // restated here.
                        _ => return WireUsage::Absent,
                    };
                    // `input_tokens` is cache-exclusive; the IR total is their sum
                    // so a cache hit is not lost.
                    let input = match fresh
                        .checked_add(read)
                        .and_then(|value| value.checked_add(write))
                    {
                        Some(value) => value,
                        None => return WireUsage::Invalid,
                    };
                    WireUsage::Measured(Usage {
                        input_tokens: input,
                        output_tokens: output,
                        cache_read_tokens: read,
                        cache_write_tokens: write,
                        reasoning_tokens: 0,
                    })
                })
        }
        Dialect::Gemini => {
            inner
                .get("usageMetadata")
                .filter(|u| !u.is_null())
                .map_or(WireUsage::Absent, |usage| {
                    // Chunks restate the full `usageMetadata`, so a usage object
                    // that cannot state its totals is malformed evidence, not a
                    // partial frame. Optional counters: absent is the documented
                    // zero, a present malformed value refuses the reading.
                    let prompt = match member(usage, "promptTokenCount") {
                        Member::Value(value) => value,
                        _ => return WireUsage::Invalid,
                    };
                    let candidates = match member(usage, "candidatesTokenCount") {
                        Member::Value(value) => value,
                        _ => return WireUsage::Invalid,
                    };
                    let reasoning = match optional_counter(member(usage, "thoughtsTokenCount")) {
                        Ok(value) => value,
                        Err(()) => return WireUsage::Invalid,
                    };
                    let cache_read =
                        match optional_counter(member(usage, "cachedContentTokenCount")) {
                            Ok(value) => value,
                            Err(()) => return WireUsage::Invalid,
                        };
                    // Gemini bills candidate text and thoughts as separate output
                    // counters: their sum with the prompt is the documented grand
                    // total, and both are billed output. The IR keeps reasoning
                    // tokens inside the output total, so this reading matches the
                    // decoded response — settlement must not see two different
                    // totals for one wire object.
                    let output = match candidates.checked_add(reasoning) {
                        Some(value) => value,
                        None => return WireUsage::Invalid,
                    };
                    WireUsage::Measured(Usage {
                        input_tokens: prompt,
                        output_tokens: output,
                        cache_read_tokens: cache_read,
                        cache_write_tokens: 0,
                        reasoning_tokens: reasoning,
                    })
                })
        }
    };
    // A reading that violates its own subset invariants is not a measurement:
    // the wire contradicted itself, which is malformed evidence rather than a
    // smaller but trusted count.
    let wire = match wire {
        WireUsage::Measured(usage)
            if usage
                .cache_read_tokens
                .checked_add(usage.cache_write_tokens)
                .is_some_and(|cache| cache <= usage.input_tokens)
                && usage.reasoning_tokens <= usage.output_tokens =>
        {
            WireUsage::Measured(usage)
        }
        WireUsage::Measured(_) => WireUsage::Invalid,
        other => other,
    };
    observation.observe(
        &declaration,
        match &wire {
            WireUsage::Measured(usage) => Some(*usage),
            _ => None,
        },
    );
    if matches!(wire, WireUsage::Invalid) {
        observation.observe_invalid_usage();
    }
}

pub(super) fn capture_event(observation: &AttemptObservation, event: &StreamEvent) {
    match event {
        StreamEvent::UpstreamDeclaration { declaration } => observation.observe(declaration, None),
        // The opening frame's usage is a partial-in-time reading: real
        // evidence, superseded by the terminal restatement. The parser built
        // it from lenient counters, so a refused wire reading is not
        // resurrected here.
        StreamEvent::Start { usage, .. } => {
            observation.observe_parsed_usage(&Default::default(), *usage)
        }
        // The parsed terminal frame is the stream's last word on usage.
        StreamEvent::Stop { usage, .. } => {
            observation.observe_terminal_usage(&Default::default(), *usage)
        }
        StreamEvent::Native { canonical, .. } => {
            for event in canonical {
                capture_event(observation, event);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{DeclaredIdentity, ObservedResponse, UsageEvidence};
    use serde_json::json;

    fn usage(input: u32, output: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    fn captured(dialect: Dialect, body: Value) -> ObservedResponse {
        let observation = AttemptObservation::default();
        capture(&observation, dialect, &body);
        observation.snapshot()
    }

    fn declared(seen: &ObservedResponse) -> Option<&str> {
        match &seen.declaration.model {
            DeclaredIdentity::Declared(model) => Some(model),
            _ => None,
        }
    }

    // ------------------------------------------------------------- Gemini

    #[test]
    fn gemini_thinking_tokens_are_billed_output() {
        // The counter arithmetic real thinking replies satisfy:
        // prompt + candidates + thoughts == total.
        let seen = captured(
            Dialect::Gemini,
            json!({
                "model": "gemini-2.5-pro",
                "usageMetadata": {
                    "promptTokenCount": 55021,
                    "candidatesTokenCount": 923,
                    "thoughtsTokenCount": 785,
                    "totalTokenCount": 56729
                }
            }),
        );
        let usage = seen.usage.expect("a well-formed usage is captured");
        assert_eq!(usage.input_tokens, 55021);
        assert_eq!(usage.output_tokens, 923 + 785);
        assert_eq!(usage.reasoning_tokens, 785);
        assert_eq!(seen.usage_evidence, UsageEvidence::Partial);
    }

    #[test]
    fn gemini_absent_optional_counters_are_zero_but_totals_are_required() {
        let seen = captured(
            Dialect::Gemini,
            json!({
                "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 3}
            }),
        );
        let usage = seen.usage.expect("absent optional counters are zeros");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.reasoning_tokens, 0);

        // A usage object without its totals is not a zero total.
        let seen = captured(
            Dialect::Gemini,
            json!({"usageMetadata": {"promptTokenCount": 100}}),
        );
        assert!(seen.usage.is_none(), "a missing total stays unknown");
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
    }

    #[test]
    fn gemini_malformed_optional_counters_are_not_measured_zero() {
        for bad in [
            json!(-5),
            json!(5_000_000_000i64),
            json!("many"),
            json!(7.5),
        ] {
            let seen = captured(
                Dialect::Gemini,
                json!({
                    "model": "gemini-2.5-pro",
                    "usageMetadata": {
                        "promptTokenCount": 100,
                        "candidatesTokenCount": 10,
                        "thoughtsTokenCount": bad
                    }
                }),
            );
            assert!(
                seen.usage.is_none(),
                "thoughts {bad} must not become a trusted zero"
            );
            assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
            assert_eq!(declared(&seen), Some("gemini-2.5-pro"));
        }
        // The same refusal for the cache subset.
        let seen = captured(
            Dialect::Gemini,
            json!({
                "usageMetadata": {
                    "promptTokenCount": 100,
                    "candidatesTokenCount": 10,
                    "cachedContentTokenCount": -1
                }
            }),
        );
        assert!(seen.usage.is_none());
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
    }

    #[test]
    fn a_gemini_cache_subset_above_the_prompt_is_not_a_trusted_reading() {
        let seen = captured(
            Dialect::Gemini,
            json!({
                "usageMetadata": {
                    "promptTokenCount": 100,
                    "cachedContentTokenCount": 800,
                    "candidatesTokenCount": 3
                }
            }),
        );
        assert!(seen.usage.is_none(), "the subset invariant is evidence");
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
    }

    // ---------------------------------------------------------- Responses

    #[test]
    fn responses_captures_the_documented_usage_shape() {
        let seen = captured(
            Dialect::OpenAIResponses,
            json!({
                "model": "gpt-x",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 5,
                    "input_tokens_details": {"cached_tokens": 40},
                    "output_tokens_details": {"reasoning_tokens": 2}
                }
            }),
        );
        let usage = seen.usage.expect("a documented usage shape is captured");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.reasoning_tokens, 2);
    }

    #[test]
    fn responses_missing_or_malformed_totals_stay_unknown() {
        for usage in [
            json!({"output_tokens": 5}),
            json!({"input_tokens": 100}),
            json!({"input_tokens": -1, "output_tokens": 5}),
            json!({"input_tokens": 100, "output_tokens": "5"}),
            json!({"input_tokens": 5_000_000_000i64, "output_tokens": 5}),
        ] {
            let seen = captured(
                Dialect::OpenAIResponses,
                json!({"model": "gpt-x", "usage": usage}),
            );
            assert!(
                seen.usage.is_none(),
                "usage {usage} must not fill an axis with a trusted zero"
            );
            assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
            assert_eq!(declared(&seen), Some("gpt-x"), "the declaration survives");
        }
    }

    #[test]
    fn responses_detail_counters_are_strict_and_subset_checked() {
        // A malformed detail counter is not a measured zero.
        for bad_cached in [json!(-3), json!("many"), json!(7.5)] {
            let seen = captured(
                Dialect::OpenAIResponses,
                json!({"usage": {
                    "input_tokens": 100,
                    "output_tokens": 5,
                    "input_tokens_details": {"cached_tokens": bad_cached}
                }}),
            );
            assert!(
                seen.usage.is_none(),
                "cached_tokens {bad_cached} must not become a trusted zero"
            );
            assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
        }
        // A cached subset above its own prompt total is self-contradictory
        // evidence, not a clamped-but-trusted count.
        let seen = captured(
            Dialect::OpenAIResponses,
            json!({"usage": {
                "input_tokens": 100,
                "output_tokens": 5,
                "input_tokens_details": {"cached_tokens": 800}
            }}),
        );
        assert!(seen.usage.is_none());
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
        // An absent detail is the documented zero.
        let seen = captured(
            Dialect::OpenAIResponses,
            json!({"usage": {"input_tokens": 100, "output_tokens": 5}}),
        );
        let usage = seen.usage.expect("absent details are zeros");
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.reasoning_tokens, 0);
    }

    // ------------------------------------------------- OpenAI and Anthropic

    #[test]
    fn openai_negative_or_overflowing_counters_stay_unknown() {
        let seen = captured(
            Dialect::OpenAI,
            json!({
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 5,
                    "prompt_tokens_details": {"cached_tokens": -3}
                }
            }),
        );
        assert!(seen.usage.is_none());
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
    }

    #[test]
    fn anthropic_input_merges_the_cache_counters_it_excludes() {
        let seen = captured(
            Dialect::Anthropic,
            json!({
                "usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 30,
                    "cache_creation_input_tokens": 5,
                    "output_tokens": 2
                }
            }),
        );
        let usage = seen
            .usage
            .expect("a documented Anthropic usage is captured");
        assert_eq!(usage.input_tokens, 45);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_write_tokens, 5);
        assert_eq!(usage.fresh_input_tokens(), 10);
    }

    /// A `message_delta` usage object that names only its output counter is
    /// the documented per-field restatement, not malformed evidence.
    #[test]
    fn an_anthropic_partial_frame_is_legal_not_invalid() {
        let seen = captured(Dialect::Anthropic, json!({"usage": {"output_tokens": 15}}));
        assert!(seen.usage.is_none(), "a partial frame records nothing");
        assert_eq!(
            seen.usage_evidence,
            UsageEvidence::Absent,
            "a legal partial must not be flagged as bad data"
        );
    }

    // ------------------------------------------------------- parsed events

    #[test]
    fn parsed_events_separate_the_opening_partial_from_the_terminal() {
        let observation = AttemptObservation::default();
        capture_event(
            &observation,
            &StreamEvent::Start {
                id: "s".into(),
                model: String::new(),
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 1,
                    ..Usage::default()
                },
            },
        );
        assert_eq!(
            observation.snapshot().usage_evidence,
            UsageEvidence::Partial
        );
        // An opening frame that carries no counters is not a reset.
        capture_event(
            &observation,
            &StreamEvent::Start {
                id: "s".into(),
                model: String::new(),
                usage: Usage::default(),
            },
        );
        assert_eq!(
            observation.snapshot().usage_evidence,
            UsageEvidence::Partial
        );
        capture_event(
            &observation,
            &StreamEvent::Stop {
                stop_reason: crate::ir::StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 15,
                    ..Usage::default()
                },
            },
        );
        let seen = observation.snapshot();
        assert_eq!(seen.usage_evidence, UsageEvidence::Measured);
        let usage = seen.usage.expect("the terminal restatement is retained");
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 15);
    }

    #[test]
    fn a_truncated_stream_keeps_its_opening_reading_as_partial() {
        let observation = AttemptObservation::default();
        capture_event(
            &observation,
            &StreamEvent::Start {
                id: "s".into(),
                model: String::new(),
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 1,
                    ..Usage::default()
                },
            },
        );
        let seen = observation.snapshot();
        assert_eq!(seen.usage_evidence, UsageEvidence::Partial);
        let usage = seen.usage.expect("the opening reading is real evidence");
        assert_eq!(usage.input_tokens, 500);
    }

    /// A legal partial frame records nothing; a malformed member anywhere in
    /// it refuses the reading even when the frame would otherwise be legal.
    #[test]
    fn an_anthropic_malformed_member_refuses_even_a_partial_frame() {
        for usage in [
            json!({"output_tokens": "abc"}),
            json!({"cache_read_input_tokens": -1}),
            json!({"output_tokens": 5, "cache_creation_input_tokens": "many"}),
        ] {
            let seen = captured(Dialect::Anthropic, json!({ "usage": usage }));
            assert!(
                seen.usage.is_none(),
                "malformed {usage} must not ride a legal partial"
            );
            assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
        }
    }

    // --------------------------------------------- decoded backfill gating

    #[test]
    fn a_terminal_restatement_cannot_resurrect_refused_usage() {
        let observation = AttemptObservation::default();
        // The raw capture refused the wire usage; the parser normalized the
        // same malformed counters into a non-zero total. The terminal event
        // must not confirm what the wire refused.
        observation.observe_invalid_usage();
        observation.observe_terminal_usage(
            &Default::default(),
            Usage {
                input_tokens: 100,
                output_tokens: 4,
                ..Usage::default()
            },
        );
        let seen = observation.snapshot();
        assert!(seen.usage.is_none());
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
        // A raw capture of a well-formed frame recovers, and the terminal
        // restatement confirms from there.
        observation.observe(
            &Default::default(),
            Some(Usage {
                input_tokens: 100,
                output_tokens: 15,
                ..Usage::default()
            }),
        );
        assert_eq!(
            observation.snapshot().usage_evidence,
            UsageEvidence::Partial
        );
        observation.observe_terminal_usage(
            &Default::default(),
            Usage {
                input_tokens: 100,
                output_tokens: 15,
                ..Usage::default()
            },
        );
        assert_eq!(
            observation.snapshot().usage_evidence,
            UsageEvidence::Measured
        );
    }

    #[test]
    fn a_decoded_restatement_keeps_a_seen_conflict_sticky() {
        let observation = AttemptObservation::default();
        observation.observe(&Default::default(), Some(usage(500, 15)));
        observation.observe(&Default::default(), Some(usage(3, 50)));
        assert_eq!(
            observation.snapshot().usage_evidence,
            UsageEvidence::Conflicting
        );
        // The decoded re-extraction may refine the retained values but the
        // conflict stays the send's headline fact.
        observation.observe_decoded(&Default::default(), usage(500, 20));
        let seen = observation.snapshot();
        assert_eq!(seen.usage_evidence, UsageEvidence::Conflicting);
        assert_eq!(seen.usage, Some(usage(500, 20)));
        // And a conflicting send has no accounting usage.
        assert_eq!(seen.accounting_usage(), None);
    }

    /// `accounting_usage` is the one answer settlement may read: known cost
    /// on Partial evidence, unknown everywhere else.
    #[test]
    fn accounting_usage_never_bills_an_unconfirmed_reading() {
        let partial = AttemptObservation::default();
        partial.observe(&Default::default(), Some(usage(500, 1)));
        assert_eq!(
            partial.snapshot().accounting_usage(),
            Some(usage(500, 1)),
            "a failed attempt's known charge is retained"
        );

        let confirmed = AttemptObservation::default();
        confirmed.observe_terminal_usage(&Default::default(), usage(500, 15));
        assert_eq!(
            confirmed.snapshot().accounting_usage(),
            Some(usage(500, 15))
        );

        let invalid = AttemptObservation::default();
        invalid.observe_invalid_usage();
        assert_eq!(invalid.snapshot().accounting_usage(), None);

        let conflicting = AttemptObservation::default();
        conflicting.observe(&Default::default(), Some(usage(500, 15)));
        conflicting.observe(&Default::default(), Some(usage(3, 50)));
        assert_eq!(conflicting.snapshot().accounting_usage(), None);

        let absent = AttemptObservation::default();
        assert_eq!(absent.snapshot().accounting_usage(), None);
    }

    #[test]
    fn usage_evidence_serializes_snake_case_for_attempt_persistence() {
        for (evidence, wire) in [
            (UsageEvidence::Absent, "absent"),
            (UsageEvidence::Partial, "partial"),
            (UsageEvidence::Measured, "measured"),
            (UsageEvidence::Invalid, "invalid"),
            (UsageEvidence::Conflicting, "conflicting"),
        ] {
            assert_eq!(serde_json::to_value(evidence).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<UsageEvidence>(json!(wire)).unwrap(),
                evidence
            );
        }
    }

    #[test]
    fn a_wrong_shaped_detail_parent_is_not_a_documented_zero() {
        for bad_parent in [json!(-3), json!("many"), json!(7.5)] {
            let seen = captured(
                Dialect::OpenAIResponses,
                json!({"usage": {
                    "input_tokens": 100,
                    "output_tokens": 5,
                    "input_tokens_details": bad_parent
                }}),
            );
            assert!(
                seen.usage.is_none(),
                "details parent {bad_parent} must not hide its counters as zeros"
            );
            assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
        }
    }

    // ------------------------------------------------------ OpenAI details

    #[test]
    fn a_details_cache_write_breakdown_is_not_added_to_the_prompt_total() {
        // `prompt_tokens_details.cache_write_tokens` is a prompt-inclusive
        // breakdown, unlike the cache-exclusive top-level counter.
        let seen = captured(
            Dialect::OpenAI,
            json!({"usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cache_write_tokens": 20}
            }}),
        );
        let usage = seen.usage.expect("a well-formed usage is captured");
        assert_eq!(usage.input_tokens, 100, "the breakdown is inside the total");
        assert_eq!(usage.cache_write_tokens, 20);
        assert_eq!(usage.fresh_input_tokens(), 80);
        assert!(seen.usage_evidence == UsageEvidence::Partial);

        // The cache-exclusive counter still merges into the total.
        let seen = captured(
            Dialect::OpenAI,
            json!({"usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "cache_creation_input_tokens": 20
            }}),
        );
        let usage = seen.usage.expect("the exclusive counter is captured");
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cache_write_tokens, 20);

        // Both spellings present and disagreeing is self-contradictory.
        let seen = captured(
            Dialect::OpenAI,
            json!({"usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "cache_creation_input_tokens": 30,
                "prompt_tokens_details": {"cache_write_tokens": 20}
            }}),
        );
        assert!(seen.usage.is_none());
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);

        // Both spellings agreeing is one reading.
        let seen = captured(
            Dialect::OpenAI,
            json!({"usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "cache_creation_input_tokens": 20,
                "prompt_tokens_details": {"cache_write_tokens": 20}
            }}),
        );
        let usage = seen.usage.expect("agreeing spellings are one reading");
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cache_write_tokens, 20);
    }

    #[test]
    fn a_lenient_decoder_backfill_is_refused_after_an_invalid_reading() {
        let observation = AttemptObservation::default();
        observation.observe_invalid_usage();
        observation.observe_decoded(
            &Default::default(),
            Usage {
                input_tokens: 100,
                output_tokens: 5,
                ..Usage::default()
            },
        );
        let seen = observation.snapshot();
        assert!(seen.usage.is_none(), "filled zeros are not accounting");
        assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
        // A later well-formed raw reading recovers.
        observation.observe(
            &Default::default(),
            Some(Usage {
                input_tokens: 100,
                output_tokens: 5,
                ..Usage::default()
            }),
        );
        let seen = observation.snapshot();
        assert!(seen.usage.is_some());
        assert_eq!(seen.usage_evidence, UsageEvidence::Partial);
    }

    #[test]
    fn a_decoded_restatement_never_flags_the_same_wire_as_conflicting() {
        let observation = AttemptObservation::default();
        // The raw capture merged the relay-declared cache write.
        observation.observe(
            &Default::default(),
            Some(Usage {
                input_tokens: 120,
                output_tokens: 5,
                cache_write_tokens: 20,
                ..Usage::default()
            }),
        );
        // The lenient decoder dropped it; the same-source re-extraction keeps
        // the richer reading and confirms the send as decoded.
        observation.observe_decoded(
            &Default::default(),
            Usage {
                input_tokens: 100,
                output_tokens: 5,
                ..Usage::default()
            },
        );
        let seen = observation.snapshot();
        assert_eq!(seen.usage_evidence, UsageEvidence::Measured);
        let usage = seen.usage.expect("the retained reading stands");
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cache_write_tokens, 20);
    }

    #[test]
    fn a_conflict_is_visible_and_sticky_not_silently_billed() {
        let observation = AttemptObservation::default();
        observation.observe(
            &Default::default(),
            Some(Usage {
                input_tokens: 500,
                output_tokens: 15,
                ..Usage::default()
            }),
        );
        observation.observe(
            &Default::default(),
            Some(Usage {
                input_tokens: 3,
                output_tokens: 50,
                ..Usage::default()
            }),
        );
        let seen = observation.snapshot();
        assert_eq!(seen.usage_evidence, UsageEvidence::Conflicting);
        let usage = seen.usage.expect("the retained reading stays as evidence");
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 15);
        // A dominating restatement refines the values but the conflict stays.
        observation.observe_terminal_usage(
            &Default::default(),
            Usage {
                input_tokens: 500,
                output_tokens: 20,
                ..Usage::default()
            },
        );
        let seen = observation.snapshot();
        assert_eq!(seen.usage_evidence, UsageEvidence::Conflicting);
        assert_eq!(seen.usage.unwrap().output_tokens, 20);
        // And a confirmed reading is not reopened by later malformed noise.
        let confirmed = AttemptObservation::default();
        confirmed.observe_terminal_usage(
            &Default::default(),
            Usage {
                input_tokens: 500,
                output_tokens: 15,
                ..Usage::default()
            },
        );
        confirmed.observe_invalid_usage();
        assert_eq!(confirmed.snapshot().usage_evidence, UsageEvidence::Measured);
    }
}
