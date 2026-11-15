//! Claude Code's server-review wire contract, backed by actual classifier calls.
//! Request/response content lives only in this request; settlement keeps IDs,
//! execution quotes and counters, never tools, prompts or verdict explanations.

mod context;
mod reviewer;
mod stream;

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::budget::{Budget, BudgetScope};
use crate::config::ClassifierConfig;
use crate::ir::{ChatRequest, Dialect};
use crate::registry::Registry;

pub(super) use stream::ReviewFrames;

pub(super) const DANGEROUS_TOOL_USE: &str = "dangerous_tool_use";
const MAX_CONTEXT_BYTES: usize = 512 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 256 * 1024;
const MAX_TOOL_USES: usize = 128;

#[derive(Clone)]
pub(super) struct ReviewContext {
    registry: Arc<Registry>,
    classifier: ClassifierConfig,
    budgets: Vec<Budget>,
    headers: axum::http::HeaderMap,
    client_identity: Option<String>,
    payload: Value,
    context_supported: bool,
    context_complete: bool,
    conversation_complete: bool,
    requested_model: String,
}

#[derive(Clone, Copy)]
enum Unavailable {
    Error,
    Timeout,
    Refused,
    InputTooLong,
    Truncated,
}

impl Unavailable {
    fn wire(self) -> Value {
        let reason = match self {
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
            Self::InputTooLong => "input_too_long",
            Self::Truncated => "truncated",
        };
        json!({"type":"unavailable", "reason":reason})
    }
}

#[derive(Clone)]
struct ToolUse {
    id: String,
    name: String,
    input: Value,
    unavailable: Option<Unavailable>,
}

impl ToolUse {
    fn from_block(block: &Value, cut_off: bool) -> Option<Self> {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            return None;
        }
        let id = block.get("id").and_then(Value::as_str)?.to_string();
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        let unavailable = if cut_off || !input.is_object() {
            Some(Unavailable::Truncated)
        } else if id.is_empty()
            || name.is_empty()
            || id.len() > 256
            || id.chars().any(char::is_control)
        {
            Some(Unavailable::Error)
        } else if input.to_string().len() > MAX_TOOL_INPUT_BYTES {
            Some(Unavailable::InputTooLong)
        } else {
            None
        };
        Some(Self {
            id,
            name,
            input,
            unavailable,
        })
    }

    fn wire(&self) -> Value {
        json!({"id":self.id, "name":self.name, "input":self.input})
    }
}

impl ReviewContext {
    pub(super) fn for_request(req: &ChatRequest, registry: &Arc<Registry>) -> Option<Arc<Self>> {
        let config = registry.config();
        if req.source_dialect != Dialect::Anthropic
            || !config.classifier.enabled
            || !config.classifier.server_review
        {
            return None;
        }
        let safeguards = req.anthropic_body.as_ref()?.get("safeguards")?.as_array()?;
        let requested = safeguards
            .iter()
            .find(|entry| entry.get("type").and_then(Value::as_str) == Some(DANGEROUS_TOOL_USE))?;
        let context = requested
            .get("classifier_context")
            .cloned()
            .unwrap_or(Value::Null);
        let context_supported = safeguards.len() == 1
            && context.is_object()
            && context.get("v").and_then(Value::as_u64) == Some(1)
            && matches!(
                context.get("permission_mode").and_then(Value::as_str),
                Some("auto" | "plan")
            );
        let payload = context::review_payload(req, context);
        let context_complete = payload.to_string().len() <= MAX_CONTEXT_BYTES;
        // An upstream-only thread reference does not carry its earlier user
        // constraints here. Without native results, do not clear an action from
        // an incomplete local security transcript.
        let conversation_complete = req.anthropic_body.as_ref().is_none_or(|body| {
            body.pointer("/thread/type").and_then(Value::as_str) != Some("continue")
        });
        let mut headers = req.client_headers.clone();
        // Reviews are a different conversation. They must not renew the main
        // session, inherit its response headers, or recursively request review.
        for name in [
            "authorization",
            "x-api-key",
            "x-goog-api-key",
            "cookie",
            "session_id",
            "x-session-id",
            "x-opencode-session",
            "x-claude-code-session-id",
        ] {
            headers.remove(name);
        }
        Some(Arc::new(Self {
            registry: registry.clone(),
            classifier: config.classifier.clone(),
            budgets: config.budgets.clone(),
            headers,
            client_identity: req.client_identity.clone(),
            payload,
            context_supported,
            context_complete,
            requested_model: req.model.clone(),
            conversation_complete,
        }))
    }

    pub(super) fn admission_scopes(&self) -> Vec<BudgetScope> {
        let providers: BTreeSet<_> = self
            .classifier
            .candidates
            .iter()
            .filter(|candidate| candidate.enabled)
            .filter_map(|candidate| self.registry.entry(&candidate.model).ok())
            .map(|model| model.provider_id.as_str())
            .collect();
        self.budgets
            .iter()
            .filter(|budget| budget.enabled)
            .filter_map(|budget| match &budget.scope {
                BudgetScope::Global => Some(BudgetScope::Global),
                BudgetScope::Provider { id } if providers.contains(id.as_str()) => {
                    Some(budget.scope.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(super) async fn stamp_response(
        &self,
        state: Arc<super::AppState>,
        response: &mut Value,
        parent: &str,
        pending: Option<crate::accounting::AttemptFact>,
    ) {
        if response
            .get("safeguard_results")
            .is_some_and(|value| !value.is_null())
        {
            return;
        }
        let blocks = response.get("content").and_then(Value::as_array);
        let cut = matches!(
            response.get("stop_reason").and_then(Value::as_str),
            Some("max_tokens" | "model_context_window_exceeded")
        );
        let tools: Vec<_> = blocks
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, block)| {
                ToolUse::from_block(
                    block,
                    cut && blocks.is_some_and(|blocks| index + 1 == blocks.len()),
                )
            })
            .collect();
        let reason = response.get("stop_reason").and_then(Value::as_str);
        let tools = tools
            .into_iter()
            .map(|mut tool| {
                if reason == Some("refusal") {
                    tool.unavailable = Some(Unavailable::Refused);
                } else if !matches!(
                    reason,
                    Some(
                        "end_turn"
                            | "tool_use"
                            | "stop_sequence"
                            | "max_tokens"
                            | "model_context_window_exceeded"
                    )
                ) {
                    tool.unavailable = Some(Unavailable::Error);
                }
                tool
            })
            .collect();
        response["safeguard_results"] = self.evaluate(state, tools, parent, pending).await;
    }

    async fn evaluate(
        &self,
        state: Arc<super::AppState>,
        mut tools: Vec<ToolUse>,
        parent: &str,
        pending: Option<crate::accounting::AttemptFact>,
    ) -> Value {
        if !self.context_supported {
            return unsupported();
        }
        let mut counts = std::collections::BTreeMap::new();
        for tool in &tools {
            *counts.entry(tool.id.clone()).or_insert(0usize) += 1;
        }
        let too_many = tools.len() > MAX_TOOL_USES;
        for tool in &mut tools {
            if counts.get(&tool.id).is_some_and(|count| *count > 1) {
                tool.unavailable = Some(Unavailable::Error);
            } else if too_many || !self.context_complete {
                tool.unavailable = Some(Unavailable::InputTooLong);
            } else if !self.conversation_complete {
                tool.unavailable = Some(Unavailable::Error);
            }
        }
        let ready: Vec<_> = tools
            .iter()
            .filter(|tool| tool.unavailable.is_none())
            .cloned()
            .collect();
        let mut verdicts = Map::new();
        if !ready.is_empty() {
            match self.classify(state, &ready, parent, pending).await {
                reviewer::ReviewOutcome::Unsupported => return unsupported(),
                reviewer::ReviewOutcome::Verdicts(results) => verdicts = results,
                reviewer::ReviewOutcome::Unavailable(reason) => {
                    for tool in &ready {
                        verdicts.insert(tool.id.clone(), reason.wire());
                    }
                }
            }
        }
        for tool in tools {
            if let Some(reason) = tool.unavailable {
                verdicts.insert(tool.id, reason.wire());
            } else {
                verdicts
                    .entry(tool.id)
                    .or_insert_with(|| Unavailable::Error.wire());
            }
        }
        json!([{"type":DANGEROUS_TOOL_USE, "status":{"type":"available", "tool_uses":verdicts}}])
    }
}

fn unsupported() -> Value {
    json!([{"type":DANGEROUS_TOOL_USE, "status":{"type":"unsupported"}}])
}

/// A deterministic pre-output rejection of the review extension can be retried
/// without that exact pair. All signed history and other beta values stay intact.
pub(super) fn recover_rejected_review(
    state: &super::AppState,
    body: &mut Value,
    client: &mut crate::config::ClientIdentity,
    error: &crate::error::Error,
) -> bool {
    let config = state.config();
    if !config.classifier.enabled
        || !config.classifier.server_review
        || client.omit_server_review
        || !client.native_anthropic
        || !body
            .get("safeguards")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.len() == 1
                    && items[0].get("type").and_then(Value::as_str) == Some(DANGEROUS_TOOL_USE)
            })
    {
        return false;
    }
    let crate::error::Error::Upstream {
        status: 400 | 422,
        body: raw,
        ..
    } = error
    else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let rejected = (message.contains("safeguards") || message.contains("dangerous_tool_use"))
        && [
            "extra inputs",
            "unknown",
            "unsupported",
            "not supported",
            "unrecognized",
        ]
        .iter()
        .any(|token| message.contains(token));
    let beta_rejected = (message.contains("unexpected value") || message.contains("beta_header:"))
        && message.contains("anthropic-beta")
        && message.contains("dangerous-tool-use-");
    if !rejected && !beta_rejected {
        return false;
    }
    body.as_object_mut()
        .expect("decoded Messages object")
        .remove("safeguards");
    client.omit_server_review = true;
    true
}
