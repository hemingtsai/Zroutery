//! Make an outbound Responses request portable between upstreams.
//!
//! # The failure this exists for
//!
//! Codex sends its whole conversation back on every turn, including items the
//! previous upstream issued: reasoning items with opaque `encrypted_content`,
//! and top-level `id`s on messages and tool calls. Those values belong to the
//! account and resource that created them. A router that may serve the next
//! turn from another provider — or the same provider through another account or
//! a CC Switch/AgentRouter relay — forwards a request that names resources the
//! new upstream cannot resolve, and the answer is
//! `The requested item was created under a different … resource`.
//!
//! The fix is the one alexcarterio/AgentRouter-Codex-Filter applies in front of
//! CC Switch:
//!
//! * drop every `reasoning` item from `input`;
//! * drop `reasoning.encrypted_content` from `include`;
//! * set `store: false`, so the upstream does not keep a response the router
//!   cannot address later;
//! * remove the server-issued `id` from items whose whole content travels with
//!   them (messages, function calls/results, custom tool calls/results);
//! * do not forward `previous_response_id`: the router reconstructs the history
//!   it needs and the id names an upstream resource.
//!
//! `call_id` is not the item id — it pairs a call with its result — so it is
//! preserved, as are phases, tool arguments, results and nested file/resource
//! references.
//!
//! # What is deliberately not stripped
//!
//! Only the five item kinds whose content is self-contained lose their ids.
//! Other tool/resource items may need their ids to be resolvable at all, and a
//! blanket recursive strip would break them.

use serde_json::{json, Value};

/// What one pass removed, for the debug log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Portability {
    pub reasoning_items_removed: usize,
    pub include_entries_removed: usize,
    pub item_ids_removed: usize,
    pub previous_response_id_removed: bool,
}

/// The item kinds whose content travels with them, so the server-issued id is
/// optional.
const PORTABLE_ITEMS: &[&str] = &[
    "message",
    "function_call",
    "function_call_output",
    "custom_tool_call",
    "custom_tool_call_output",
];

/// Strip what cannot cross an upstream boundary, in place.
pub fn make_portable(body: &mut Value) -> Portability {
    let mut report = Portability::default();
    let Some(object) = body.as_object_mut() else {
        return report;
    };

    // Nothing in this history is addressable at the next upstream, and a stored
    // response would be a resource the router cannot name again.
    object.insert("store".into(), json!(false));

    if let Some(include) = object.get_mut("include").and_then(Value::as_array_mut) {
        let before = include.len();
        include.retain(|entry| entry.as_str() != Some("reasoning.encrypted_content"));
        report.include_entries_removed = before - include.len();
    }

    if object.remove("previous_response_id").is_some() {
        report.previous_response_id_removed = true;
    }

    let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) else {
        return report;
    };
    let before = input.len();
    // The router's own history is complete; an upstream-issued reasoning item
    // is opaque at best and belongs to the resource that issued it at worst.
    input.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
    report.reasoning_items_removed = before - input.len();

    for item in input.iter_mut() {
        if !is_portable(item) {
            continue;
        }
        if let Some(object) = item.as_object_mut() {
            if object.remove("id").is_some() {
                report.item_ids_removed += 1;
            }
        }
    }
    report
}

/// Whether `item` carries its whole content, making its server id optional.
fn is_portable(item: &Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| PORTABLE_ITEMS.contains(&kind))
    {
        return true;
    }
    // The official EasyInputMessage has no `type`: a bare role + content object
    // is an ordinary message.
    !object.contains_key("type") && object.contains_key("role") && object.contains_key("content")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({
            "model": "gpt-5.3-sol",
            "store": true,
            "previous_response_id": "resp_abc",
            "include": ["reasoning.encrypted_content", "message.output_text.logprobs"],
            "input": [
                {"role": "user", "content": "hello", "id": "msg_1"},
                {"type": "reasoning", "encrypted_content": "opaque", "summary": []},
                {"type": "message", "role": "assistant", "content": [], "id": "msg_2", "phase": "final"},
                {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{}", "id": "fc_1"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok", "id": "fco_1"},
                {"type": "item_reference", "id": "msg_9"}
            ]
        })
    }

    #[test]
    fn history_that_names_an_upstream_resource_is_removed() {
        let mut body = body();
        let report = make_portable(&mut body);

        assert_eq!(report.reasoning_items_removed, 1);
        assert_eq!(report.include_entries_removed, 1);
        assert_eq!(report.item_ids_removed, 4);
        assert!(report.previous_response_id_removed);
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["include"], json!(["message.output_text.logprobs"]));
        assert!(body.get("previous_response_id").is_none());

        let input = body["input"].as_array().expect("input");
        assert!(
            !input
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning")),
            "reasoning items cannot cross an upstream: {input:?}"
        );
        // The pair the call and its result need survives, and the phase is not
        // an id.
        let call = input
            .iter()
            .find(|item| item["type"] == "function_call")
            .expect("the call");
        assert_eq!(call["call_id"], "call_1");
        assert!(call.get("id").is_none());
        let message = input
            .iter()
            .find(|item| item["type"] == "message")
            .expect("the message");
        assert_eq!(message["phase"], "final");
        assert!(message.get("id").is_none());
        // A bare message loses its id too.
        assert!(input[0].get("id").is_none());
        // An item whose content is not self-contained keeps its id.
        let reference = input
            .iter()
            .find(|item| item["type"] == "item_reference")
            .expect("the reference");
        assert_eq!(reference["id"], "msg_9");
    }

    #[test]
    fn a_portable_body_is_left_alone_apart_from_the_store_flag() {
        let mut body = json!({
            "store": false,
            "input": [{"role": "user", "content": "hello"}]
        });
        let report = make_portable(&mut body);
        assert_eq!(report, Portability::default());
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_input_string_is_not_rewritten() {
        let mut body = json!({"input": "just a prompt", "store": true});
        let report = make_portable(&mut body);
        assert_eq!(report.reasoning_items_removed, 0);
        assert_eq!(body["input"], "just a prompt");
        assert_eq!(body["store"], json!(false));
    }

    #[test]
    fn non_object_bodies_are_not_touched() {
        let mut body = json!(["not", "a", "request"]);
        assert_eq!(make_portable(&mut body), Portability::default());
    }
}
