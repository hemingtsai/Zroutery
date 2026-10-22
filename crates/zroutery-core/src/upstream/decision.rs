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
//! and the answer is the same map with the answer in place of the question:
//!
//! ```json
//! { "decision": { "type": "noul", "noul": 0.97 } }
//! ```
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
pub fn build_body(state: &str, instructions: &str) -> Value {
    json!({
        "state": state,
        "questions": {
            QUESTION_KEY: { "type": "noul", "instructions": instructions }
        }
    })
}

/// Read the answer out of a decision endpoint's response.
///
/// `None` for anything that is not a probability: no answer, an answer of the
/// wrong type, a number outside `0..=1`, a number that is not a number. The
/// caller treats a missing answer as no verdict at all, which is the fail-closed
/// case.
pub fn parse_answer(answer: &Value) -> Option<Decision> {
    let noul = answer.get(QUESTION_KEY)?.get("noul")?.as_f64()?;
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
    let state = req
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (state, instructions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Message, SystemPart};

    #[test]
    fn the_body_asks_one_binary_question() {
        let body = build_body("the transcript", "is this dangerous");
        assert_eq!(body["state"], "the transcript");
        assert_eq!(body["questions"][QUESTION_KEY]["type"], "noul");
        assert_eq!(
            body["questions"][QUESTION_KEY]["instructions"],
            "is this dangerous"
        );
    }

    #[test]
    fn a_probability_is_read() {
        let answer = json!({ "decision": { "type": "noul", "noul": 0.97 } });
        assert_eq!(parse_answer(&answer), Some(Decision { noul: 0.97 }));
    }

    #[test]
    fn an_answer_that_is_not_a_probability_is_no_answer() {
        for answer in [
            json!({}),
            json!({ "decision": {} }),
            json!({ "decision": { "type": "noul" } }),
            json!({ "decision": { "type": "noul", "noul": "high" } }),
            json!({ "decision": { "type": "noul", "noul": 1.5 } }),
            json!({ "decision": { "type": "noul", "noul": -0.1 } }),
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
        assert_eq!(state, "the tool call\n\nthe question");
        assert_eq!(instructions, "You are a security monitor.");
    }
}
