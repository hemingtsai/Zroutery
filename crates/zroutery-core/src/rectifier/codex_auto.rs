//! Magpie Auto V10 policy, ported from yidu456/magpie-auto-codex-filter
//! cd68b072ddf3475203d42661d9cdca3a93212100 (proxy.cjs).
//! Copyright (c) 2026 alexcarterio and contributors. MIT; see
//! docs/licenses/magpie-auto-codex-filter.txt.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};

pub const MAX_RETRIES: u32 = 20;
pub const DEADLINE: Duration = Duration::from_secs(1800);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
pub const BUFFER_LIMIT: usize = 64 * 1024 * 1024;
pub const ERROR_LIMIT: usize = 64 * 1024;

/// Process-local counters, never prompts, response contents or credentials.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FilterStats {
    pub requests: u64,
    pub active_requests: u64,
    pub cleanup_requests: u64,
    pub removed_reasoning_items: u64,
    pub removed_item_ids: u64,
    pub cleanup_refused: u64,
    pub retries: [u64; 4],
    pub retries_accepted: [u64; 4],
    pub retries_exhausted: [u64; 4],
    pub success: u64,
    pub failed: u64,
    pub cancelled: u64,
}

#[derive(Debug, Default)]
pub struct FilterCounters(Mutex<FilterStats>);

impl FilterCounters {
    pub fn snapshot(&self) -> FilterStats {
        crate::sync::lock(&self.0).clone()
    }
    pub(crate) fn update(&self, update: impl FnOnce(&mut FilterStats)) {
        update(&mut crate::sync::lock(&self.0));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    Resource,
    RateLimit,
    Transport,
    Availability,
}

impl RetryKind {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn limit(self) -> u32 {
        match self {
            Self::Resource => 5,
            Self::RateLimit => 20,
            Self::Transport | Self::Availability => 10,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::RateLimit => "rate_limit",
            Self::Transport => "transport",
            Self::Availability => "availability",
        }
    }
    pub fn delay(self, index: u32, floor: Duration, jitter_ms: u64) -> Duration {
        let delays = match self {
            Self::Resource | Self::Transport => [1, 2, 4, 8, 15],
            Self::RateLimit => [10, 20, 40, 60, 60],
            Self::Availability => [5, 10, 20, 30, 60],
        };
        Duration::from_secs(delays[(index as usize).min(4)]).max(floor)
            + if self == Self::RateLimit {
                Duration::from_millis(jitter_ms.min(999))
            } else {
                Duration::ZERO
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryDecision {
    pub kind: RetryKind,
    pub code: &'static str,
    pub cleanup: bool,
}

fn detail(value: &Value) -> &Value {
    value
        .get("error")
        .filter(|v| !v.is_null())
        .or_else(|| value.pointer("/response/error").filter(|v| !v.is_null()))
        .unwrap_or(value)
}

fn business_error(error: &Value) -> bool {
    let text =
        format!("{} {} {}", error["code"], error["type"], error["message"]).to_ascii_lowercase();
    [
        "quota",
        "budget",
        "billing",
        "balance",
        "credit",
        "spend_limit",
        "spend limit",
        "spend.limit",
    ]
    .iter()
    .any(|word| text.contains(word))
        || ["code", "type"].iter().any(|field| {
            matches!(
                error[*field].as_str(),
                Some(
                    "invalid_api_key"
                        | "authentication_error"
                        | "permission_denied"
                        | "unauthorized"
                        | "forbidden"
                )
            )
        })
}

/// Exact targeted errors only; generic signature/decryption/400s do not qualify.
pub fn classify_value(value: &Value, status: u16, sse: bool) -> Option<RetryDecision> {
    if crate::failure::is_session_binding_invalidated_value(status, value) {
        return None;
    }
    let error = detail(value);
    if business_error(error) {
        return None;
    }
    let missing = value["type"] == "response.failed"
        && value
            .get("error")
            .filter(|v| !v.is_null())
            .or_else(|| value.pointer("/response/error").filter(|v| !v.is_null()))
            .is_none_or(|v| {
                !["code", "type", "message"]
                    .iter()
                    .any(|key| v.get(*key).is_some_and(truthy))
            });
    let code = if missing {
        ""
    } else {
        error
            .get("code")
            .and_then(Value::as_str)
            .or_else(|| error.get("type").and_then(Value::as_str))
            .unwrap_or("")
    };
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    let event = value.get("type").and_then(Value::as_str).unwrap_or("");
    let resource = (error["type"] == "invalid_request_error"
        || matches!(event, "error" | "response.failed"))
        && message
            .contains("The requested item was created under a different Azure OpenAI resource.")
        && message.contains("Use the same resource that created the item to access it.");
    if (status == 400 || sse) && (resource || error["code"] == "invalid_encrypted_content") {
        return Some(RetryDecision {
            kind: RetryKind::Resource,
            code: if resource {
                "different_azure"
            } else {
                "invalid_encrypted_content"
            },
            cleanup: true,
        });
    }
    let lower = message.to_ascii_lowercase();
    if (sse || matches!(status, 400 | 429))
        && (matches!(
            code,
            "rate_limit_exceeded" | "rate_limit_error" | "too_many_requests"
        ) || lower.contains("exceeded token rate limit")
            || (lower.contains("rate limit exceeded")
                && ["your requests", "token", "requests per minute"]
                    .iter()
                    .any(|s| lower.contains(s))))
    {
        return Some(RetryDecision {
            kind: RetryKind::RateLimit,
            code: "rate_limit",
            cleanup: false,
        });
    }
    if sse
        && (matches!(
            code,
            "stream_incomplete"
                | "stream_disconnected"
                | "upstream_idle_timeout"
                | "upstream_aborted"
        ) || [
            "stream closed before response.completed",
            "idle timeout waiting for sse",
            "stream disconnected before completion",
        ]
        .iter()
        .any(|s| lower.contains(s))
            || missing)
    {
        return Some(RetryDecision {
            kind: RetryKind::Transport,
            code: if event == "response.failed" && code.is_empty() {
                "upstream_failed_without_details"
            } else {
                "stream_incomplete"
            },
            cleanup: false,
        });
    }
    if (sse || status == 503)
        && (code == "no_available_channel"
            || lower.contains("no available channel")
            || message.contains("无可用渠道")
            || message.contains("没有可用渠道"))
    {
        return Some(RetryDecision {
            kind: RetryKind::Availability,
            code: "no_available_channel",
            cleanup: false,
        });
    }
    None
}

/// Known ALB HTML 400 only, including the exact CC Switch error wrapper.
pub fn is_alb_page(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    lower.contains("<html")
        && compact.contains("</html>")
        && compact.contains("<title>400badrequest</title>")
        && compact.contains("<h1>400badrequest</h1>")
        && compact.contains("<center>alb</center>")
}

pub fn classify_error(error: &Error) -> Option<RetryDecision> {
    if error.is_session_binding_invalidated() {
        return None;
    }
    if matches!(error, Error::Upstream { status: 503, .. }) && error.classified().retryable() {
        return Some(RetryDecision {
            kind: RetryKind::Availability,
            code: "service_unavailable",
            cleanup: false,
        });
    }
    match error {
        Error::Transport { .. } | Error::Timeout(_) | Error::InterruptedStream { .. } => {
            Some(RetryDecision {
                kind: RetryKind::Transport,
                code: "stream_incomplete",
                cleanup: false,
            })
        }
        Error::Upstream { status, body, .. } if body.len() <= ERROR_LIMIT => {
            let value = serde_json::from_str::<Value>(body).ok();
            if *status == 400
                && match &value {
                    Some(value) => {
                        value.pointer("/error/code").and_then(Value::as_str)
                            == Some("cc_switch_upstream_error")
                            && value
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .is_some_and(is_alb_page)
                    }
                    None => is_alb_page(body),
                }
            {
                return Some(RetryDecision {
                    kind: RetryKind::Transport,
                    code: "alb_bad_request",
                    cleanup: false,
                });
            }
            value.as_ref().and_then(|v| {
                classify_value(
                    v,
                    *status,
                    *status == 200
                        && matches!(v["type"].as_str(), Some("error" | "response.failed")),
                )
            })
        }
        _ => None,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReport {
    pub reasoning: usize,
    pub ids: usize,
}

/// Validate the entire history before changing it. Refusal is atomic.
pub fn clean_history(body: &mut Value) -> Result<CleanupReport> {
    let object = body.as_object().ok_or_else(|| {
        Error::invalid("invalid_request_json: Responses request must be a JSON object")
    })?;
    if object.get("include").is_some_and(|v| !v.is_array()) {
        return Err(Error::invalid("invalid_include: include must be an array"));
    }
    if object
        .get("input")
        .is_some_and(|v| !v.is_string() && !v.is_array())
    {
        return Err(Error::invalid(
            "invalid_input: input must be a string or an array",
        ));
    }
    if ["previous_response_id", "conversation"]
        .iter()
        .any(|key| object.get(*key).is_some_and(truthy))
    {
        return Err(Error::invalid("response_reference_not_portable: complete message/tool history is required; start a new session with a text summary"));
    }
    if object
        .get("context_management")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item["type"] == "compaction"))
    {
        return Err(Error::invalid("encrypted_compaction_disabled: encrypted compaction cannot safely cross accounts; use a text summary"));
    }
    if object
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item["type"] != "reasoning"
                    && (matches!(
                        item["type"].as_str(),
                        Some(
                            "compaction"
                                | "compaction_summary"
                                | "context_compaction"
                                | "item_reference"
                        )
                    ) || item.get("encrypted_content").is_some_and(truthy))
            })
        })
    {
        return Err(Error::invalid("encrypted_context_not_portable: encrypted context or item references cannot safely cross accounts; history has NOT been silently discarded"));
    }
    let mut report = CleanupReport::default();
    body["store"] = json!(false);
    if let Some(include) = body.get_mut("include").and_then(Value::as_array_mut) {
        include.retain(|item| item != "reasoning.encrypted_content");
    }
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        input.retain(|item| {
            let remove = item["type"] == "reasoning";
            report.reasoning += usize::from(remove);
            !remove
        });
        for item in input {
            let portable = matches!(
                item["type"].as_str(),
                Some(
                    "message"
                        | "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output"
                )
            ) || (item.get("type").is_none()
                && item["role"].is_string()
                && item.get("content").is_some());
            if portable {
                if let Some(item) = item.as_object_mut() {
                    report.ids += usize::from(item.remove("id").is_some());
                }
            }
        }
    }
    Ok(report)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    }
}

/// Per-client-request allowances, shared across failure classes and candidates.
#[derive(Debug, Default)]
pub struct RetryBudget {
    pub total: u32,
    pub by_kind: [u32; 4],
    pub cleaned: bool,
}

impl RetryBudget {
    pub fn schedule(
        &mut self,
        decision: RetryDecision,
        floor: Duration,
        remaining: Duration,
        jitter: u64,
    ) -> Option<Duration> {
        let index = decision.kind.index();
        if self.total >= MAX_RETRIES || self.by_kind[index] >= decision.kind.limit() {
            return None;
        }
        let delay = decision.kind.delay(self.by_kind[index], floor, jitter);
        if delay >= remaining {
            return None;
        }
        self.total += 1;
        self.by_kind[index] += 1;
        Some(delay)
    }
}
