//! The decision protocol: a model that answers typed questions instead of
//! generating text.
//!
//! # What it is
//!
//! The System One family — Jev and the models built in its shape — takes a
//! state and a set of typed questions and answers with calibrated
//! probabilities over the choices. It does not generate prose, and it cannot
//! answer a chat request at all.
//!
//! That is why it is a *protocol* and not a dialect: the request is not a
//! conversation, the response is not a message, and the answer is not text.
//!
//! # The shape
//!
//! A request is a state and a map of questions:
//!
//! ```json
//! {
//!   "state": "the transcript the client sent",
//!   "questions": {
//!     "decision": { "type": "noul", "instructions": "the question" }
//!   }
//! }
//! ```
//!
//! and the answer is the same map with the answer in place of the question,
//! under an `answers` envelope:
//!
//! ```json
//! { "answers": { "decision": { "type": "noul", "noul": 0.97 } } }
//! ```
//!
//! The envelope is part of the protocol, not a wrapper a host may omit. A
//! response that carries `answers` but not the answer is no answer at all; it
//! is not re-read from the top level, because that would turn a malformed
//! envelope into a verdict. A flat response is still accepted when `answers` is
//! absent, for hosts and fixtures written against the earlier shape.
//!
//! The request also names the model it is asking: the System One family routes
//! by the body's `model`, and a request without one is answered by whatever the
//! endpoint defaults to — which is not the model the operator configured.
//!
//! The family also has a `choice` (one of N options, with the distribution
//! over them) and a `score` (a point on an ordered scale). Only the binary
//! question is used here, because the Auto Mode asks a binary question: the
//! action proceeds or it does not.
//!
//! # What the answer is not
//!
//! The number is a probability, and a probability is not a verdict. Turning it
//! into one is the Auto Mode's business and lives with the Auto Mode — see
//! [`crate::classifier::verdict_from_decision`].

use serde_json::{json, Value};

use crate::ir::{ChatRequest, ContentBlock, Role};

/// The key the Auto Mode's question is asked under.
///
/// A fixed key, so the answer is read from a known place rather than from
/// whichever key the answer happens to use.
pub const QUESTION_KEY: &str = "decision";

/// The sentence that fixes what "yes" means.
///
/// The client's own classifier prompt asks the question; this only pins the
/// direction of the answer. The verdict mapping in
/// [`crate::classifier::verdict_from_decision`] treats a high `noul` as
/// *block*, and a model left to infer the polarity from a prompt that may be
/// phrased either way is a calibration risk the proxy does not need.
pub const BLOCK_POLARITY: &str =
    "Answer yes only when the action must not proceed; answer no when it may proceed.";

/// One answer from a decision endpoint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    /// The probability that the answer is "yes".
    ///
    /// The family's `noul` primitive is a binary question with the
    /// probability of "yes" as its answer. Higher means more likely yes.
    pub noul: f64,
}

/// The body a decision endpoint is asked with.
///
/// `model` is part of the body rather than the URL: the family's endpoint
/// serves several models at one path, and omitting the field asks whatever the
/// endpoint defaults to.
pub fn build_body(state: &str, instructions: &str, model: &str) -> Value {
    json!({
        "model": model,
        "state": state,
        "questions": {
            QUESTION_KEY: {
                "type": "noul",
                "instructions": format!("{instructions}\n\n{BLOCK_POLARITY}")
            }
        }
    })
}

/// Read the answer out of a decision endpoint's response.
///
/// The answer lives under `answers`. When `answers` is present, only it is
/// read: a malformed envelope is `None`, never a fallback to the top level,
/// because a security gate must not recover a verdict from a response whose
/// protocol it could not parse. A response with no `answers` key at all is read
/// as a flat document, which keeps hosts and fixtures written against the
/// earlier shape working.
///
/// `None` for anything that is not a probability: no answer, an answer of the
/// wrong type, a number outside `0..=1`, a number that is not a number. The
/// caller treats a missing answer as no verdict at all, which is the fail-closed
/// case.
pub fn parse_answer(answer: &Value) -> Option<Decision> {
    let answers = match answer.get("answers") {
        Some(answers) => answers,
        None => answer,
    };
    let noul = answers.get(QUESTION_KEY)?.get("noul")?.as_f64()?;
    if !noul.is_finite() || !(0.0..=1.0).contains(&noul) {
        return None;
    }
    Some(Decision { noul })
}

/// The state and the question, read out of a classifier request.
///
/// The state is everything the client sent — the transcript and the question
/// the security monitor is being asked — and the instructions are the role the
/// client gave the model. A decision model is asked the same question, in its
/// own protocol, and the answer is about the same thing.
///
/// The whole of the client content is the state, not the last message: a
/// security monitor judges a *transcript*, and a state that carried only the
/// final turn would be judged on the wrong thing.
pub fn question_of(req: &ChatRequest) -> (String, String) {
    let instructions = req
        .system
        .iter()
        .map(|part| part.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let state = if req
        .messages
        .iter()
        .any(|message| message.role != Role::User)
    {
        // Keep assistant/system turns and tool evidence rather than judging
        // only the last user message. This JSON is transient request content,
        // never persisted in records or diagnostics.
        req.anthropic_body
            .as_ref()
            .and_then(|body| body.get("messages"))
            .map(Value::to_string)
            .unwrap_or_else(|| serde_json::to_string(&req.messages).unwrap_or_default())
    } else {
        req.messages
            .iter()
            .filter(|message| message.role == Role::User)
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    (state, instructions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Message, SystemPart};

    #[test]
    fn the_body_names_the_model_and_asks_one_binary_question() {
        let body = build_body("the transcript", "is this dangerous", "jev-1.13-free");
        assert_eq!(body["model"], "jev-1.13-free");
        assert_eq!(body["state"], "the transcript");
        assert_eq!(body["questions"][QUESTION_KEY]["type"], "noul");
        let instructions = body["questions"][QUESTION_KEY]["instructions"]
            .as_str()
            .expect("instructions are a string");
        assert!(instructions.starts_with("is this dangerous"));
        assert!(instructions.contains(BLOCK_POLARITY));
    }

    #[test]
    fn a_probability_is_read_from_the_answers_envelope() {
        let answer = json!({ "answers": { "decision": { "type": "noul", "noul": 0.97 } } });
        assert_eq!(parse_answer(&answer), Some(Decision { noul: 0.97 }));
    }

    #[test]
    fn a_flat_answer_is_read_when_there_is_no_envelope() {
        let answer = json!({ "decision": { "type": "noul", "noul": 0.02 } });
        assert_eq!(parse_answer(&answer), Some(Decision { noul: 0.02 }));
    }

    #[test]
    fn a_malformed_envelope_is_no_answer_rather_than_a_flat_one() {
        // The top-level probability must not rescue a response whose envelope
        // does not carry it: that is exactly the kind of quiet recovery a
        // security gate must not do.
        for answer in [
            json!({ "answers": null, "decision": { "type": "noul", "noul": 0.97 } }),
            json!({ "answers": {}, "decision": { "type": "noul", "noul": 0.97 } }),
            json!({ "answers": { "decision": {} }, "decision": { "noul": 0.97 } }),
            json!({ "answers": { "decision": { "noul": "high" } }, "decision": { "noul": 0.97 } }),
        ] {
            assert_eq!(parse_answer(&answer), None, "for {answer}");
        }
    }

    #[test]
    fn an_answer_that_is_not_a_probability_is_no_answer() {
        for answer in [
            json!({}),
            json!({ "answers": {} }),
            json!({ "answers": { "decision": {} } }),
            json!({ "answers": { "decision": { "type": "noul" } } }),
            json!({ "answers": { "decision": { "type": "noul", "noul": "high" } } }),
            json!({ "answers": { "decision": { "type": "noul", "noul": 1.5 } } }),
            json!({ "answers": { "decision": { "type": "noul", "noul": -0.1 } } }),
        ] {
            assert_eq!(parse_answer(&answer), None, "for {answer}");
        }
    }

    #[test]
    fn the_state_is_the_whole_transcript_and_the_question_is_the_role() {
        let mut req = ChatRequest::new("claude-opus-4-8", crate::ir::Dialect::Anthropic);
        req.system = vec![SystemPart::new("You are a security monitor.")];
        req.messages = vec![
            Message::user_text("the tool call"),
            Message::assistant_text("the tool's answer"),
            Message::user_text("the question"),
        ];
        let (state, instructions) = question_of(&req);
        assert!(
            state.contains("the tool call")
                && state.contains("the tool's answer")
                && state.contains("the question")
        );
        assert_eq!(instructions, "You are a security monitor.");
    }
}
