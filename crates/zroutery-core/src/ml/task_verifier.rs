//! Runtime task verification: judging a response while it is still in hand.
//!
//! # Why this runs here and not over the trace
//!
//! The obvious design — evaluate a stored history — does not work, and it is worth
//! being precise about why. `OutcomeTrainingSample` deliberately carries no response
//! body (see the privacy constraints on persisted traces), so a verifier reading a
//! trace has nothing to inspect. An implementation that inferred a verdict from usage
//! counts or from the task-type feature would be fabricating evidence, and every rate
//! derived from it would be wrong in a way nothing downstream could detect.
//!
//! So verification happens on the serving path, in the one place where both the
//! request's *demands* and the response's *actual content* are simultaneously
//! available, and only the resulting **verdict** — never the content — is persisted.
//!
//! # What is checked, and what that check is worth
//!
//! | Check | Verdict | Stated limit |
//! |---|---|---|
//! | tools were offered and `tool_choice` required one | [`ToolExecution`] | form only: a call with an id and parseable input. Not whether the call was *right*. |
//! | tools were offered, use was optional | [`ToolExecution`] | records whether a well-formed call was emitted; absence is **not** a failure when nothing was demanded. |
//! | `structured_output` requested | [`SchemaValidation`] | the output parses against the constraint's JSON schema grammar. Not that the content is *correct*. |
//!
//! Every one of these is a **form** check. A model can emit flawless JSON that answers
//! the wrong question, and this module will call it a pass. Each summary says so, and
//! [`TaskOutcomeEvidence::verifier`] carries
//! [`VerifierKind::is_mechanical`](crate::ml::task_evidence::VerifierKind::is_mechanical)
//! so a later reader can see these are mechanical checks rather than judgements.
//!
//! # The unknown case is the common case
//!
//! A chat turn with no tools and no structured-output demand has no check that applies.
//! It produces nothing, which is the correct outcome: there is no evidence, and
//! absence is not a failure. Expect most real traffic to be unverified, and read that
//! as the state of the evidence rather than as a defect.

use crate::ir::{ChatRequest, ChatResponse, ContentBlock};
use crate::ml::task_evidence::{EvidenceOrigin, TaskOutcomeEvidence, TaskStatus, VerifierKind};
use crate::policy::TaskType;

/// Rebuilds a [`ChatResponse`] from a stream, so the same checks can run.
///
/// # Why this is needed
///
/// A streamed answer never exists as a `ChatResponse`: it arrives as a `Start` frame
/// and then deltas. Verification therefore needs the content reassembled, and this is
/// the reassembly. It runs on the serving path alongside the stream, which is the only
/// place the content is ever available — the persisted trace keeps the verdict, not
/// the answer.
///
/// # What is deliberately not reassembled
///
/// `ThinkingDelta` and `RedactedThinking` are dropped. Reasoning text is not an
/// output a client consumes and not something any check here examines, so buffering it
/// would hold content in memory for no reader.
#[derive(Debug, Clone, Default)]
pub struct StreamResponseAccumulator {
    id: String,
    model: String,
    /// Text blocks by index, concatenated in arrival order at `finish`.
    text: std::collections::BTreeMap<u32, String>,
    /// Tool calls by index: the identity from `ToolUseStart` and the arguments
    /// accumulated from `ToolUseDelta` until `BlockStop`.
    tools: std::collections::BTreeMap<u32, (String, String, String)>,
    stop_reason: Option<crate::ir::StopReason>,
    usage: crate::ir::Usage,
    saw_stop: bool,
}

impl StreamResponseAccumulator {
    /// Fold one stream event in.
    pub fn observe(&mut self, event: &crate::ir::StreamEvent) {
        use crate::ir::StreamEvent;
        match event {
            StreamEvent::Native { canonical, .. } => {
                for event in canonical {
                    self.observe(event);
                }
            }
            StreamEvent::Start { id, model, usage } => {
                self.id = id.clone();
                self.model = model.clone();
                self.usage = *usage;
            }
            StreamEvent::TextDelta { index, text, .. } => {
                self.text.entry(*index).or_default().push_str(text);
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.tools
                    .entry(*index)
                    .or_insert_with(|| (id.clone(), name.clone(), String::new()));
            }
            StreamEvent::ToolUseDelta {
                index,
                partial_json,
                ..
            } => {
                if let Some(entry) = self.tools.get_mut(index) {
                    entry.2.push_str(partial_json);
                }
            }
            StreamEvent::Stop {
                stop_reason, usage, ..
            } => {
                self.stop_reason = Some(*stop_reason);
                self.usage = *usage;
                self.saw_stop = true;
            }
            // Reasoning and block boundaries carry nothing a check reads. The JSON is
            // already accumulated by the time `BlockStop` arrives, and parsing it at
            // the end is equivalent to parsing it per block.
            _ => {}
        }
    }

    /// Whether the stream ended in a terminal `Stop` frame.
    ///
    /// A stream cut off before `Stop` is not a completed response, and judging a
    /// truncated answer would attribute a verdict to content the client never
    /// received.
    pub fn is_complete(&self) -> bool {
        self.saw_stop
    }

    /// Reassemble what was seen into a response the verifier can read.
    pub fn finish(self) -> crate::ir::ChatResponse {
        let mut content: Vec<crate::ir::ContentBlock> = Vec::new();
        for (_, (id, name, arguments)) in self.tools {
            // Arguments that never parsed become `Null`, which
            // [`tool_call_is_well_formed`] rejects. That is the right verdict for a
            // streamed, cut-off arguments blob: the client could not execute it.
            let input = serde_json::from_str::<serde_json::Value>(&arguments)
                .unwrap_or(serde_json::Value::Null);
            content.push(crate::ir::ContentBlock::ToolUse { id, name, input });
        }
        for (_, text) in self.text {
            if !text.is_empty() {
                content.push(crate::ir::ContentBlock::Text {
                    text,
                    cache_control: None,
                });
            }
        }
        crate::ir::ChatResponse {
            id: self.id,
            model: self.model,
            content,
            stop_reason: self.stop_reason.unwrap_or(crate::ir::StopReason::EndTurn),
            stop_sequence: None,
            usage: self.usage,
            passthrough: Default::default(),
        }
    }
}

/// Which checks to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifierConfig {
    /// Run every check whose precondition is met.
    #[default]
    All,
    /// Run none. Keeps the label-coverage baseline untouched.
    Off,
}

/// The identity of the attempt being judged, for attribution.
#[derive(Debug, Clone)]
pub struct VerdictContext {
    pub request_id: String,
    pub outcome_id: String,
    pub attempt_id: Option<String>,
    pub served_model: Option<String>,
    pub served_provider: Option<String>,
    pub task_type: TaskType,
    pub observed_at: i64,
}

/// Run every applicable check over one response.
///
/// Pure and total: it reads the request and the response and returns evidence. It
/// cannot fail, cannot block, and makes no upstream call. A caller that does not want
/// the cost passes [`VerifierConfig::Off`].
pub fn verify(
    request: &ChatRequest,
    response: &ChatResponse,
    context: &VerdictContext,
    config: VerifierConfig,
) -> Vec<TaskOutcomeEvidence> {
    if config == VerifierConfig::Off {
        return Vec::new();
    }
    let mut verdicts = Vec::new();

    // -- Tool calls ----------------------------------------------------------
    //
    // Gated on tools actually being offered: a request with no tools cannot be
    // judged on whether the model called one.
    if !request.tools.is_empty() {
        let demanded = demands_tool_call(request);
        let calls: Vec<&ContentBlock> = response
            .content
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .collect();

        let status = if calls.is_empty() {
            // Demanded but absent is a real failure: the client required a call and
            // got prose. Optional-and-absent is not, and conflating the two would
            // charge every conversational answer with a tool schema for failing.
            if demanded {
                TaskStatus::Failed
            } else {
                TaskStatus::Unknown
            }
        } else if calls.iter().all(|block| tool_call_is_well_formed(block)) {
            TaskStatus::Succeeded
        } else {
            TaskStatus::Failed
        };

        let summary = match (status, demanded) {
            (TaskStatus::Failed, true) => {
                format!(
                    "a tool call was required but {} call(s) were emitted",
                    calls.len()
                )
            }
            (TaskStatus::Failed, false) => {
                format!(
                    "{} tool call(s) emitted, at least one without an id or with input that \
                     does not parse",
                    calls.len()
                )
            }
            (TaskStatus::Succeeded, _) => format!(
                "{} well-formed tool call(s) emitted for {} offered tool(s)",
                calls.len(),
                request.tools.len()
            ),
            _ => "tools were offered but no call was required and none was emitted".to_string(),
        };

        verdicts.push(evidence(
            context,
            VerifierKind::ToolExecution,
            status,
            summary,
        ));
    }

    // -- Structured output ---------------------------------------------------
    //
    // Only when the client actually stated a constraint. Checking JSON shape on an
    // unconstrained answer would manufacture a pass rate out of nothing.
    if let Some(structured) = &request.structured_output {
        let text = response.text();
        let status = if text.trim().is_empty() {
            TaskStatus::Failed
        } else if serde_json::from_str::<serde_json::Value>(text.trim()).is_ok() {
            TaskStatus::Succeeded
        } else {
            TaskStatus::Failed
        };
        let summary = match status {
            TaskStatus::Succeeded => "structured output was requested and parsed as JSON".into(),
            _ => format!(
                "structured output was requested ({} ) and the response did not parse",
                describe_constraint(structured)
            ),
        };
        verdicts.push(evidence(
            context,
            VerifierKind::SchemaValidation,
            status,
            summary,
        ));
    }

    verdicts
}

/// Whether the client required a tool call rather than merely permitting one.
///
/// `Auto` and `None` leave the decision to the model, so an absent call is a legitimate
/// answer and must not be scored as a failure. `Any` requires one; a named tool
/// requires that tool.
fn demands_tool_call(request: &ChatRequest) -> bool {
    match &request.tool_choice {
        Some(crate::ir::ToolChoice::Any) | Some(crate::ir::ToolChoice::Specific { .. }) => true,
        Some(crate::ir::ToolChoice::Auto) | None => false,
        // A dialect may add variants later; anything unrecognised is treated as
        // permissive, because inventing a failure from an unknown enum is the worse
        // error.
        Some(_) => false,
    }
}

/// A tool call is well formed when it names a tool and carries parseable input.
///
/// "Parseable" is checked by confirming the input round-trips as JSON. An upstream that
/// streamed a truncated arguments blob produces a call the client cannot execute, and
/// that is a real defect worth recording.
fn tool_call_is_well_formed(block: &ContentBlock) -> bool {
    let ContentBlock::ToolUse {
        id, name, input, ..
    } = block
    else {
        return false;
    };
    if id.trim().is_empty() || name.trim().is_empty() {
        return false;
    }
    // `input` is already a parsed `Value`, so a successful parse here is not a new
    // fact; what is checked is that it is a usable object rather than a bare null or
    // a truncated string.
    match input {
        serde_json::Value::Object(_) => true,
        serde_json::Value::Null => false,
        other => !other.to_string().is_empty(),
    }
}

/// A short, non-sensitive description of what was asked for.
fn describe_constraint(structured: &crate::ir::StructuredOutput) -> &'static str {
    match structured {
        crate::ir::StructuredOutput::JsonObject => "json object",
        crate::ir::StructuredOutput::JsonSchema { .. } => "json schema",
    }
}

fn evidence(
    context: &VerdictContext,
    verifier: VerifierKind,
    status: TaskStatus,
    summary: String,
) -> TaskOutcomeEvidence {
    TaskOutcomeEvidence::new(
        format!(
            "{}-{}-{}",
            context.request_id,
            context.attempt_id.as_deref().unwrap_or("request"),
            verifier_wire(verifier)
        ),
        context.request_id.clone(),
        context.outcome_id.clone(),
        context.attempt_id.clone(),
        context.served_model.clone(),
        context.served_provider.clone(),
        context.task_type,
        verifier,
        status,
        context.observed_at,
        EvidenceOrigin::RuntimeVerifier,
    )
    .with_summary(summary)
}

fn verifier_wire(verifier: VerifierKind) -> &'static str {
    match verifier {
        VerifierKind::Compilation => "compile",
        VerifierKind::SchemaValidation => "schema",
        VerifierKind::ToolExecution => "tool",
        VerifierKind::StateVerification => "state",
        VerifierKind::AnswerCheck => "answer",
        VerifierKind::ExplicitRating => "rating",
        VerifierKind::BehaviouralSignal => "behaviour",
        VerifierKind::ModelJudge => "judge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ToolDef;
    use serde_json::json;

    /// `ChatRequest` implements no `Default`, so each fixture states its fields.
    fn request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: Vec::new(),
            messages: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            stream: false,
            tools: Vec::new(),
            tool_strict: Default::default(),
            tool_choice: None,
            parallel_tool_use: None,
            thinking: None,
            reasoning: None,
            structured_output: None,
            metadata_user: None,
            passthrough: Default::default(),
            source_dialect: crate::ir::Dialect::OpenAI,
            responses_tools: None,
            responses_body: None,
            anthropic_body: None,
            responses_native_only: false,
            unsupported_content_policy: Default::default(),
            required_capabilities: Vec::new(),
            client_identity: None,
            client_headers: Default::default(),
            reply_headers: Default::default(),
        }
    }

    fn context() -> VerdictContext {
        VerdictContext {
            request_id: "req-1".into(),
            outcome_id: "out-1".into(),
            attempt_id: Some("att-1".into()),
            served_model: Some("m-1".into()),
            served_provider: Some("p-1".into()),
            task_type: TaskType::ToolUse,
            observed_at: 1_700_000_000,
        }
    }

    fn request_with_tools() -> ChatRequest {
        let mut request = request();
        request.tools = vec![ToolDef {
            name: "search".into(),
            description: Some("search the web".into()),
            input_schema: json!({"type": "object"}),
            cache_control: None,
        }];
        request.tool_choice = Some(crate::ir::ToolChoice::Any);
        request
    }

    fn tool_response(input: serde_json::Value, id: &str) -> ChatResponse {
        ChatResponse {
            id: "r".into(),
            model: "m".into(),
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "search".into(),
                input,
            }],
            stop_reason: crate::ir::StopReason::ToolUse,
            stop_sequence: None,
            usage: Default::default(),
            passthrough: Default::default(),
        }
    }

    #[test]
    fn a_required_tool_call_that_is_emitted_well_formed_succeeds() {
        let verdicts = verify(
            &request_with_tools(),
            &tool_response(json!({"q": "rust"}), "call-1"),
            &context(),
            VerifierConfig::All,
        );
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].status, TaskStatus::Succeeded);
        assert_eq!(verdicts[0].verifier, VerifierKind::ToolExecution);
    }

    #[test]
    fn a_required_tool_call_that_never_arrives_fails() {
        // The client demanded a call and received prose: a real task failure.
        let response = ChatResponse {
            id: "r".into(),
            model: "m".into(),
            content: vec![ContentBlock::Text {
                text: "I could not search".into(),
                cache_control: None,
            }],
            stop_reason: crate::ir::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
            passthrough: Default::default(),
        };
        let verdicts = verify(
            &request_with_tools(),
            &response,
            &context(),
            VerifierConfig::All,
        );
        assert_eq!(verdicts[0].status, TaskStatus::Failed);
    }

    #[test]
    fn an_optional_tool_call_that_never_arrives_is_unknown_not_failed() {
        // The distinction that keeps a conversational answer with a tool schema from
        // being counted as a failure.
        let mut request = request_with_tools();
        request.tool_choice = Some(crate::ir::ToolChoice::Auto);
        let response = ChatResponse {
            id: "r".into(),
            model: "m".into(),
            content: vec![ContentBlock::Text {
                text: "just talking".into(),
                cache_control: None,
            }],
            stop_reason: crate::ir::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
            passthrough: Default::default(),
        };
        let verdicts = verify(&request, &response, &context(), VerifierConfig::All);
        assert_eq!(verdicts[0].status, TaskStatus::Unknown);
    }

    #[test]
    fn a_truncated_tool_call_is_a_failure() {
        // `null` input is what a streamed, cut-off arguments blob decodes to.
        let verdicts = verify(
            &request_with_tools(),
            &tool_response(serde_json::Value::Null, "call-1"),
            &context(),
            VerifierConfig::All,
        );
        assert_eq!(verdicts[0].status, TaskStatus::Failed);
    }

    #[test]
    fn a_request_with_no_tools_produces_no_tool_verdict() {
        let request = request();
        let response = ChatResponse {
            id: "r".into(),
            model: "m".into(),
            content: vec![],
            stop_reason: crate::ir::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
            passthrough: Default::default(),
        };
        // Not Unknown, not Failed: nothing was checked, so nothing is claimed.
        assert!(verify(&request, &response, &context(), VerifierConfig::All).is_empty());
    }

    #[test]
    fn structured_output_that_does_not_parse_fails_and_one_that_does_succeeds() {
        let mut request = request();
        request.structured_output = Some(crate::ir::StructuredOutput::JsonSchema {
            name: None,
            schema: json!({"type": "object"}),
            strict: false,
        });
        let bad = ChatResponse {
            id: "r".into(),
            model: "m".into(),
            content: vec![ContentBlock::Text {
                text: "not json at all".into(),
                cache_control: None,
            }],
            stop_reason: crate::ir::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
            passthrough: Default::default(),
        };
        assert_eq!(
            verify(&request, &bad, &context(), VerifierConfig::All)[0].status,
            TaskStatus::Failed
        );

        let good = ChatResponse {
            content: vec![ContentBlock::Text {
                text: "{\"answer\": 42}".into(),
                cache_control: None,
            }],
            ..bad.clone()
        };
        assert_eq!(
            verify(&request, &good, &context(), VerifierConfig::All)[0].status,
            TaskStatus::Succeeded
        );

        // No constraint stated: nothing is checked, so no pass is manufactured.
        request.structured_output = None;
        assert!(verify(&request, &good, &context(), VerifierConfig::All).is_empty());
    }

    #[test]
    fn the_config_switch_suppresses_every_verdict() {
        let verdicts = verify(
            &request_with_tools(),
            &tool_response(json!({}), "call-1"),
            &context(),
            VerifierConfig::Off,
        );
        assert!(verdicts.is_empty());
    }

    #[test]
    fn every_verdict_is_attributable_to_the_attempt_that_served() {
        let verdicts = verify(
            &request_with_tools(),
            &tool_response(json!({}), "call-1"),
            &context(),
            VerifierConfig::All,
        );
        assert!(verdicts[0].is_attempt_attributable());
        assert_eq!(verdicts[0].served_model.as_deref(), Some("m-1"));
    }

    #[test]
    fn a_streamed_tool_call_is_reassembled_and_verified() {
        use crate::ir::StreamEvent;

        let mut accumulator = StreamResponseAccumulator::default();
        accumulator.observe(&StreamEvent::Start {
            id: "r".into(),
            model: "m".into(),
            usage: Default::default(),
        });
        accumulator.observe(&StreamEvent::ToolUseStart {
            index: 0,
            id: "call-1".into(),
            name: "search".into(),
        });
        // Arguments arrive in fragments, as they do on a real stream.
        accumulator.observe(&StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "{\"city\":".into(),
        });
        accumulator.observe(&StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "\"oslo\"}".into(),
        });
        accumulator.observe(&StreamEvent::BlockStop { index: 0 });
        accumulator.observe(&StreamEvent::Stop {
            stop_reason: crate::ir::StopReason::ToolUse,
            stop_sequence: None,
            usage: Default::default(),
        });
        assert!(accumulator.is_complete());

        let request = request_with_tools();
        let verdicts = verify(
            &request,
            &accumulator.finish(),
            &context(),
            VerifierConfig::All,
        );
        assert_eq!(verdicts.len(), 1);
        assert_eq!(
            verdicts[0].status,
            TaskStatus::Succeeded,
            "the reassembled call should verify: {:?}",
            verdicts[0].summary
        );
    }

    #[test]
    fn a_streamed_tool_call_with_unparseable_arguments_fails() {
        use crate::ir::StreamEvent;

        let mut accumulator = StreamResponseAccumulator::default();
        accumulator.observe(&StreamEvent::ToolUseStart {
            index: 0,
            id: "call-1".into(),
            name: "search".into(),
        });
        // A truncated arguments blob, which is what a cut-off stream produces.
        accumulator.observe(&StreamEvent::ToolUseDelta {
            index: 0,
            partial_json: "{\"city\": \"os".into(),
        });
        accumulator.observe(&StreamEvent::Stop {
            stop_reason: crate::ir::StopReason::ToolUse,
            stop_sequence: None,
            usage: Default::default(),
        });

        let verdicts = verify(
            &request_with_tools(),
            &accumulator.finish(),
            &context(),
            VerifierConfig::All,
        );
        assert_eq!(verdicts[0].status, TaskStatus::Failed);
    }

    #[test]
    fn a_stream_that_never_ended_is_not_a_complete_response() {
        use crate::ir::StreamEvent;

        let mut accumulator = StreamResponseAccumulator::default();
        accumulator.observe(&StreamEvent::TextDelta {
            index: 0,
            text: "half an ans".into(),
        });
        // No `Stop`: the client never received a finished answer.
        assert!(!accumulator.is_complete());
    }

    #[test]
    fn streamed_text_is_reassembled_for_a_json_check() {
        use crate::ir::StreamEvent;

        let mut accumulator = StreamResponseAccumulator::default();
        for fragment in ["{\"answer\":", " 42}"] {
            accumulator.observe(&StreamEvent::TextDelta {
                index: 0,
                text: fragment.into(),
            });
        }
        accumulator.observe(&StreamEvent::Stop {
            stop_reason: crate::ir::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
        });

        let mut request = request();
        request.structured_output = Some(crate::ir::StructuredOutput::JsonObject);
        let verdicts = verify(
            &request,
            &accumulator.finish(),
            &context(),
            VerifierConfig::All,
        );
        assert_eq!(verdicts[0].status, TaskStatus::Succeeded);
    }

    #[test]
    fn the_summary_states_that_only_form_was_checked() {
        // A reader must not mistake a schema pass for a correctness verdict.
        let mut request = request_with_tools();
        request.tool_choice = Some(crate::ir::ToolChoice::Auto);
        let verdicts = verify(
            &request,
            &tool_response(json!({"q": "x"}), "call-1"),
            &context(),
            VerifierConfig::All,
        );
        let summary = verdicts[0].summary.to_lowercase();
        assert!(
            summary.contains("well-formed"),
            "the summary must describe the check that ran: {summary}"
        );
    }
}
