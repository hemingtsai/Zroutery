//! Failure classification for observation and routing decisions.
//!
//! Not all failures are equal. A 429 rate limit is transient and retryable;
//! an authentication error is permanent and should not retry. This module
//! classifies failures so the observation layer can make correct decisions
//! about health, circuit breaking, and fallback.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Classification of a request failure.
///
/// This is the canonical, public classification vocabulary.  `Error` remains
/// the structural/wire type; consumers that need a routing or health decision
/// must use [`ClassifiedFailure`] instead of re-inspecting an error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailureClass {
    /// Network-level failure (connection refused, DNS, TLS).
    Transport,
    /// Request or streaming timed out.
    Timeout,
    /// Provider returned 429 rate limit.
    RateLimit,
    /// Authentication/authorization failure (401, 403).
    Authentication,
    /// Provider is unavailable (502, 503, 504, 529).
    ProviderUnavailable,
    /// Upstream returned a terminal rejection of a request the gateway built
    /// (400, 412) without a deterministic local-configuration or budget
    /// marker.
    ///
    /// The request that reached the upstream was decoded, validated and
    /// re-encoded here, so a 400 is the provider refusing the shape this
    /// gateway produced — an unsupported parameter, a dated wire contract it
    /// does not implement, a model that cannot answer the translated body —
    /// not the client's raw payload. It counts as provider evidence: repeated
    /// rejections open the model's circuit, and a single rejection on a
    /// locally invalid request never reaches this class because the pipeline
    /// refuses it before an attempt.
    ///
    /// A rejection is also *fallbackable*: another candidate may translate the
    /// same request into a shape its provider accepts, so one provider's 400
    /// must not fail the client's request while candidates remain. It is not
    /// retryable — replaying the identical body to the same provider has no
    /// reason to end differently.
    ProviderRejected,
    /// Protocol-level error (malformed response, unexpected format).
    Protocol,
    /// Request requires capabilities the model does not have.
    Capability,
    /// Client sent an invalid request (400, validation error).
    InvalidRequest,
    /// Client cancelled the request.
    ClientCancelled,
    /// A provider is not configured with a usable API key.
    ///
    /// This is deliberately distinct from [`FailureClass::Authentication`]:
    /// another provider may be usable, but this provider's health must not be
    /// poisoned by a local configuration problem.
    MissingApiKey,
    /// A provider's upstream budget pool has been exhausted. This is a
    /// per-model-group, time-bounded exhaustion (e.g. AgentRouter's daily
    /// 10:00/19:00 batch grants), not a local account-level budget denial.
    QuotaExhausted,
    /// The wallet behind a provider has no funds for this request.
    ///
    /// HTTP 402 Payment Required is the status shape of this failure, and a
    /// billing marker on another status (`insufficient_quota`, `insufficient
    /// balance`, `payment required`) selects it too. It is deliberately not
    /// [`FailureClass::QuotaExhausted`]: a pool refills on a provider schedule
    /// and is worth retrying, while an empty wallet stays empty until somebody
    /// pays. The provider must therefore leave the routing set, not absorb one
    /// failed request per model.
    BalanceExhausted,
    /// A local budget/policy denied the request.
    OverBudget,
    /// Routing found no eligible candidate for the request.
    NoCandidate,
    /// A local configuration or internal request-preparation failure.
    Configuration,
    /// A stream ended after partial output without a normal terminal event.
    Interrupted,
    /// Unknown or unclassified failure.
    Unknown,
}

impl FailureClass {
    /// Every canonical class, useful for exhaustive table tests and adapters.
    ///
    /// New classes append here rather than slotting into their enum position:
    /// `FailureClass::ALL`'s index is the persisted ML failure-class rank, so
    /// reordering it would silently reinterpret recorded samples.
    pub const ALL: [Self; 18] = [
        Self::Transport,
        Self::Timeout,
        Self::RateLimit,
        Self::Authentication,
        Self::ProviderUnavailable,
        Self::ProviderRejected,
        Self::Protocol,
        Self::Capability,
        Self::InvalidRequest,
        Self::ClientCancelled,
        Self::MissingApiKey,
        Self::OverBudget,
        Self::NoCandidate,
        Self::Configuration,
        Self::QuotaExhausted,
        Self::Interrupted,
        Self::Unknown,
        Self::BalanceExhausted,
    ];

    /// Determine the complete impact of this failure class.
    ///
    /// This match is the single policy table for retry, fallback, circuit,
    /// observation, and provider-fault decisions.  Stats deliberately remain
    /// observational: every classified terminal failure is recorded, including
    /// local/configuration and cancellation failures.  Use
    /// [`FailureImpact::records_stats`] when a caller needs that bit.
    pub const fn impact(&self) -> FailureImpact {
        match self {
            Self::Transport => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
            Self::Timeout => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
            Self::RateLimit => FailureImpact {
                affects_observation: true, // degraded, not dead
                affects_circuit: false,    // do not open the circuit for quota
                retryable: true,
                fallbackable: true,
                provider_fault: false,
            },
            Self::Authentication => FailureImpact {
                affects_observation: false, // credentials are not health
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            Self::ProviderUnavailable => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
            Self::ProviderRejected => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: false,   // replaying the same body cannot help
                fallbackable: true, // another candidate may accept the shape
                provider_fault: true,
            },
            Self::Protocol => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: false, // replaying the same request is not useful
                fallbackable: true,
                provider_fault: true,
            },
            Self::Capability => FailureImpact {
                affects_observation: false, // model limitation is not health
                affects_circuit: false,
                retryable: false,
                fallbackable: true,
                provider_fault: false,
            },
            Self::InvalidRequest => FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            Self::ClientCancelled => FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            Self::MissingApiKey => FailureImpact {
                affects_observation: false, // local secret-store problem
                affects_circuit: false,
                retryable: false,   // the same provider cannot fix it
                fallbackable: true, // another provider may be eligible
                provider_fault: false,
            },
            Self::OverBudget => FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            Self::QuotaExhausted => FailureImpact {
                affects_observation: true, // degraded, not dead: upstream pool is empty
                affects_circuit: false,    // do not open the circuit for quota (will recover)
                retryable: false,          // same group on same provider will still fail
                fallbackable: true,        // another group or provider may be eligible
                provider_fault: true,      // the pool belongs to the provider
            },
            Self::BalanceExhausted => FailureImpact {
                affects_observation: true, // an empty wallet is a provider fact
                affects_circuit: true,     // this model cannot pay for itself
                retryable: false,          // the same wallet is still empty
                fallbackable: true,        // another provider can serve the request
                provider_fault: true,      // the wallet belongs to the provider
            },
            Self::NoCandidate => FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            Self::Configuration => FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            Self::Interrupted => FailureImpact {
                affects_observation: false,
                affects_circuit: false,
                retryable: false, // never silently turn a partial stream into success
                fallbackable: false,
                provider_fault: false,
            },
            Self::Unknown => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true, // fail closed for provider health
            },
        }
    }

    /// Whether stats should retain this classified failure.
    pub const fn records_stats(&self) -> bool {
        self.impact().records_stats()
    }

    /// Classify an HTTP status code without a body.
    ///
    /// A 402 is the payment-required status and classifies as
    /// [`FailureClass::BalanceExhausted`]: in this gateway's upstream context
    /// it means the provider's wallet cannot pay, which is a provider fact, not
    /// evidence that the caller is over budget or missing a key. A bare 412
    /// stays a provider rejection.
    pub fn from_status(status: u16) -> Self {
        Self::from_upstream_status(status, None)
    }

    /// Classify an HTTP status code with provider response context.
    ///
    /// Some relays return 500 for a client validation error, and some report
    /// a model capability rejection with 400/415/422.  The body is only a
    /// disambiguator; the status and marker rules are fixed and deterministic.
    pub fn from_status_with_body(status: u16, body: &str) -> Self {
        Self::from_upstream_status(status, Some(body))
    }

    fn from_upstream_status(status: u16, body: Option<&str>) -> Self {
        if body.is_some_and(|body| is_session_binding_invalidated(status, body)) {
            // Changing the session id is required; replaying the same binding
            // or treating it as model-health evidence cannot repair it.
            return Self::InvalidRequest;
        }
        let lower = body.map(str::to_ascii_lowercase);
        if status >= 400 && lower.as_deref().is_some_and(is_tool_history_mismatch) {
            return Self::InvalidRequest;
        }
        if status == 403
            && lower.as_deref().is_some_and(|body| {
                body.contains("freetiererror")
                    || body.contains("can only be used from within opencode")
            })
        {
            return Self::Capability;
        }
        // A 5xx is normally transient, but one that names the request itself as
        // invalid is a mislabelled client error: retrying it, or failing over to
        // another provider with the same content, cannot help. The status is 5xx
        // and the body is the disambiguator, exactly as below.
        if status == 500
            && lower.as_deref().is_some_and(|body| {
                body.contains("invalid_request_error") || body.contains("invalid request")
            })
        {
            return Self::InvalidRequest;
        }
        // An empty wallet is a provider fact, and the loudest one a body can
        // carry. It is checked before the local-marker block so a 402 saying
        // "insufficient balance" is not read as a missing key, and before the
        // status table so a 429 carrying `insufficient_quota` is not read as a
        // transient rate limit.
        if let Some(body) = lower.as_deref() {
            if is_balance_exhausted_message(body) {
                return Self::BalanceExhausted;
            }
        }
        if status == 400 && lower.as_deref().is_some_and(is_rate_limit_message) {
            return Self::RateLimit;
        }
        // A status alone is not evidence of a local budget/key decision.
        // Only explicit, deterministic body markers may select local classes;
        // otherwise 402/412 become provider evidence below.
        if let Some(body) = lower.as_deref() {
            if matches!(status, 402 | 404 | 412 | 503) {
                if is_missing_key_message(body) {
                    return Self::MissingApiKey;
                }
                if is_quota_exhausted_message(body) {
                    return Self::QuotaExhausted;
                }
                if is_over_budget_message(body) {
                    return Self::OverBudget;
                }
                if is_no_candidate_message(body) {
                    return Self::NoCandidate;
                }
            }
        }
        match status {
            401 | 403 => return Self::Authentication,
            // A regional/legal refusal is a deterministic endpoint rejection,
            // not an authentication or request error. The same classifier
            // question may still be accepted by another configured model, but
            // replaying this endpoint cannot change its regional policy.
            451 => return Self::ProviderRejected,
            408 => return Self::Timeout,
            429 => return Self::RateLimit,
            502..=504 | 529 => return Self::ProviderUnavailable,
            _ => {}
        }
        if let Some(body) = lower.as_deref() {
            if is_capability_message(body) && matches!(status, 400 | 415 | 422 | 500 | 501) {
                return Self::Capability;
            }
        }
        match status {
            400 => Self::ProviderRejected,
            402 => Self::BalanceExhausted,
            412 => Self::ProviderRejected,
            413 => Self::InvalidRequest,
            500 => Self::Unknown,
            _ if status >= 400 => Self::Protocol,
            _ => Self::Unknown,
        }
    }

    /// Classify a non-structural error message.
    ///
    /// This is retained for adapters that receive a message before an
    /// [`crate::Error`] is available.  Once a structural `Error` exists,
    /// [`ClassifiedFailure::from_core_error`] is the authoritative path.
    pub fn from_error_message(message: &str) -> Self {
        let lower = message.to_ascii_lowercase();
        if is_session_binding_message(&lower) {
            Self::InvalidRequest
        } else if is_cancelled_message(&lower) {
            Self::ClientCancelled
        } else if is_interrupted_message(&lower) {
            Self::Interrupted
        } else if is_balance_exhausted_message(&lower) {
            // Checked before the timeout/rate-limit wording: a billing message
            // routinely carries "429" or "quota", and reading it as a
            // transient limit is what kept traffic on an unfunded provider.
            Self::BalanceExhausted
        } else if lower.contains("timeout") || lower.contains("timed out") {
            Self::Timeout
        } else if lower.contains("connection")
            || lower.contains("dns")
            || lower.contains("tls")
            || lower.contains("socket")
        {
            Self::Transport
        } else if lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains("429")
        {
            Self::RateLimit
        } else if is_missing_key_message(&lower) {
            Self::MissingApiKey
        } else if lower.contains("unauthorized")
            || lower.contains("authentication")
            || lower.contains("401")
            || lower.contains("403")
        {
            Self::Authentication
        } else if is_capability_message(&lower) {
            Self::Capability
        } else if is_quota_exhausted_message(&lower) {
            Self::QuotaExhausted
        } else if is_over_budget_message(&lower) {
            Self::OverBudget
        } else if is_no_candidate_message(&lower) {
            Self::NoCandidate
        } else if lower.contains("invalid request")
            || lower.contains("invalid_request")
            || lower.contains("bad request")
            || lower.contains("validation")
        {
            Self::InvalidRequest
        } else if lower.contains("configuration")
            || lower.contains("config")
            || lower.contains("internal error")
        {
            Self::Configuration
        } else {
            Self::Unknown
        }
    }
}

/// A relay's session/model binding expired, not its reasoning ciphertext.
/// Keep this marker shared by classification, wire errors and retry policy.
pub(crate) fn is_session_binding_invalidated(status: u16, body: &str) -> bool {
    if !binding_status_permits(status) {
        return false;
    }
    match serde_json::from_str::<Value>(body) {
        Ok(value) => is_session_binding_invalidated_value(status, &value),
        Err(_) => {
            let lower = body.trim().to_ascii_lowercase();
            is_session_binding_message(&lower)
                && (status != 200 || lower.starts_with("session binding invalidated"))
                && !is_balance_exhausted_message(&lower)
                && !is_over_budget_message(&lower)
                && !is_quota_exhausted_message(&lower)
        }
    }
}

pub(crate) fn is_session_binding_invalidated_value(status: u16, value: &Value) -> bool {
    if !binding_status_permits(status) {
        return false;
    }
    let explicit_error = value.get("error").is_some_and(|error| !error.is_null())
        || value
            .pointer("/response/error")
            .is_some_and(|error| !error.is_null());
    if status == 200
        && !explicit_error
        && (value.get("choices").is_some()
            || value.get("output").is_some()
            || value.get("success") == Some(&Value::Bool(true)))
    {
        // Text produced by a model can quote an error; it is not an error.
        return false;
    }
    let mut detail = value;
    for _ in 0..4 {
        if binding_business_error(detail) {
            return false;
        }
        match detail
            .get("error")
            .filter(|error| !error.is_null())
            .or_else(|| {
                detail
                    .pointer("/response/error")
                    .filter(|error| !error.is_null())
            }) {
            Some(inner) => detail = inner,
            None => break,
        }
    }
    if binding_business_error(detail) {
        return false;
    }
    if ["code", "type"].iter().any(|field| {
        detail.get(*field).is_some_and(|code| {
            code.as_u64() == Some(30013)
                || matches!(
                    code.as_str().map(str::trim),
                    Some("30013" | "session_binding_invalidated")
                )
        })
    }) {
        return true;
    }
    detail
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| detail.as_str())
        .is_some_and(|message| is_session_binding_message(&message.to_ascii_lowercase()))
}

fn binding_status_permits(status: u16) -> bool {
    (status == 200 || status >= 400) && !matches!(status, 401..=403)
}

fn is_session_binding_message(lower: &str) -> bool {
    lower.contains("session binding invalidated")
}

fn binding_business_error(value: &Value) -> bool {
    let protected = ["code", "type"].iter().any(|field| {
        matches!(
            value.get(*field).and_then(Value::as_str),
            Some(
                "invalid_api_key"
                    | "authentication_error"
                    | "permission_denied"
                    | "unauthorized"
                    | "forbidden"
                    | "insufficient_quota"
                    | "insufficient_balance"
                    | "budget_exceeded"
                    | "rate_limit_exceeded"
                    | "rate_limit_error"
                    | "too_many_requests"
            )
        )
    });
    let lower = value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    protected
        || is_balance_exhausted_message(&lower)
        || is_over_budget_message(&lower)
        || is_quota_exhausted_message(&lower)
}

fn is_cancelled_message(message: &str) -> bool {
    message.contains("cancel") || message.contains("abort")
}

fn is_interrupted_message(message: &str) -> bool {
    message.contains("interrupt")
        || message.contains("premature")
        || message.contains("stream ended")
        || message.contains("stream closed")
        || message.contains("stream dropped")
        || message.contains("connection closed")
        || message.contains("disconnect")
        || message.contains("destroyed")
        || message.contains("broken pipe")
}

fn is_missing_key_message(message: &str) -> bool {
    message.contains("missing api key")
        || message.contains("missing key")
        || message.contains("no api key")
        || message.contains("api key not configured")
}

fn is_tool_history_mismatch(message: &str) -> bool {
    message.contains("tool_call_mismatch")
        || message.contains("tool_mismatch")
        || message.contains("tool calls and tool results do not match")
}

fn is_over_budget_message(message: &str) -> bool {
    message.contains("budget_exceeded")
        || message.contains("over budget")
        || message.contains("budget exceeded")
}

fn is_quota_exhausted_message(message: &str) -> bool {
    message.contains("budget pool quota has been exhausted")
}

/// Some relays use 400 for a token/request window. Keep this decision in the
/// canonical classifier so transport retry and scoped cooling cannot disagree.
fn is_rate_limit_message(message: &str) -> bool {
    message.contains("rate_limit_exceeded")
        || message.contains("exceeded token rate limit")
        || (message.contains("rate limit exceeded")
            && (message.contains("requests per minute") || message.contains("token")))
}

/// Markers that say the wallet behind a provider cannot pay for this request.
///
/// Deliberately narrower than "quota": an exhausted per-minute limit or a
/// pool that refills on a schedule is not an empty wallet, and treating one as
/// the other would take a provider out of routing that recovers by itself.
fn is_balance_exhausted_message(message: &str) -> bool {
    message.contains("insufficient_quota")
        || message.contains("insufficient quota")
        || message.contains("insufficient balance")
        || message.contains("insufficient credit")
        || message.contains("insufficient funds")
        || message.contains("credit balance is too low")
        || message.contains("exceeded your current quota")
        || message.contains("payment required")
        || message.contains("no credits remaining")
        || message.contains("credit balance exhausted")
        || message.contains("account balance exhausted")
        || message.contains("余额不足")
        || message.contains("额度不足")
        || message.contains("欠费")
        || message.contains("请充值")
}

fn is_no_candidate_message(message: &str) -> bool {
    message.contains("no candidate")
        || message.contains("no model is available")
        || message.contains("no enabled")
}

fn is_capability_message(message: &str) -> bool {
    message.contains("capability")
        || message.contains("unsupported")
        || message.contains("not supported")
        || message.contains("not support")
        || message.contains("does not support")
        || message.contains("does not have")
}

/// The impact of a failure on routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureImpact {
    /// Should this failure count against the provider's observation health?
    pub affects_observation: bool,
    /// Should this failure count against the circuit breaker?
    pub affects_circuit: bool,
    /// Is this failure retryable on the same candidate?
    pub retryable: bool,
    /// Should the router try a different candidate after this failure?
    pub fallbackable: bool,
    /// Is this a provider-attributable failure (vs client/config error)?
    pub provider_fault: bool,
}

impl FailureImpact {
    /// Stats retain every classified terminal failure, including local and
    /// cancellation failures.  Health mutation is governed by the other bits.
    pub const fn records_stats(&self) -> bool {
        true
    }

    /// Alias for [`FailureImpact::records_stats`] used by store adapters.
    pub const fn affects_stats(&self) -> bool {
        self.records_stats()
    }
}

/// A classified failure with context.
///
/// Construct this through [`ClassifiedFailure::from_core_error`] (or one of its
/// explicit terminal constructors) rather than assembling a second policy in
/// a caller.  The legacy [`ClassifiedFailure::from_error`] entry point accepts
/// only a message and is a compatibility adapter, not the structural mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedFailure {
    pub class: FailureClass,
    pub status: Option<u16>,
    pub message: String,
    pub impact: FailureImpact,
}

impl ClassifiedFailure {
    /// Classify an HTTP response while retaining its message for diagnostics.
    pub fn from_status(status: u16, message: String) -> Self {
        let class = FailureClass::from_status_with_body(status, &message);
        Self::from_parts(class, Some(status), message)
    }

    /// Classify a message that does not yet have a structural `Error`.
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let class = FailureClass::from_error_message(&message);
        Self::from_parts(class, None, message)
    }

    /// Compatibility spelling for callers that used the old message helper.
    pub fn from_error_message(message: impl Into<String>) -> Self {
        Self::from_message(message)
    }

    /// Legacy compatibility constructor for the original message-only API.
    ///
    /// This deliberately keeps the original `String` signature and message
    /// classification semantics.  Structural errors must use
    /// [`ClassifiedFailure::from_core_error`].
    pub fn from_error(message: String) -> Self {
        Self::from_message(message)
    }

    /// The sole exhaustive structural `Error`-to-classification constructor.
    pub fn from_core_error<E>(error: E) -> Self
    where
        E: std::borrow::Borrow<crate::Error>,
    {
        let error = error.borrow();
        let (class, status) = match error {
            crate::Error::InvalidRequest(_) | crate::Error::TooLarge { .. } => {
                (FailureClass::InvalidRequest, Some(error.status().as_u16()))
            }
            crate::Error::UnknownModel(_) => {
                (FailureClass::NoCandidate, Some(error.status().as_u16()))
            }
            crate::Error::UnknownRoute(_) => {
                (FailureClass::InvalidRequest, Some(error.status().as_u16()))
            }
            crate::Error::OverBudget(_) => {
                (FailureClass::OverBudget, Some(error.status().as_u16()))
            }
            crate::Error::NoCandidate(_) => {
                (FailureClass::NoCandidate, Some(error.status().as_u16()))
            }
            crate::Error::RateLimited { .. } => {
                // No upstream was contacted; the gate is existing availability
                // evidence, not another observed failure of a healthy model.
                (FailureClass::NoCandidate, Some(error.status().as_u16()))
            }
            crate::Error::UnsupportedReasoning { .. } => {
                (FailureClass::InvalidRequest, Some(error.status().as_u16()))
            }
            crate::Error::Unauthorized => {
                (FailureClass::Authentication, Some(error.status().as_u16()))
            }
            crate::Error::MissingApiKey(_) => {
                (FailureClass::MissingApiKey, Some(error.status().as_u16()))
            }
            crate::Error::Upstream { status, body, .. }
                if is_session_binding_invalidated(*status, body) =>
            {
                (FailureClass::InvalidRequest, Some(400))
            }
            crate::Error::Upstream {
                status: 200, body, ..
            } => {
                // A staged SSE failure arrived over HTTP 200, not a successful
                // attempt. Preserve the transport status for the filter's exact
                // trigger rules, but classify its explicit error centrally.
                let value = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
                let detail = value
                    .get("error")
                    .filter(|v| !v.is_null())
                    .or_else(|| value.pointer("/response/error").filter(|v| !v.is_null()))
                    .unwrap_or(&value);
                let code = detail
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| detail.get("type").and_then(serde_json::Value::as_str))
                    .unwrap_or("");
                let auth_type = detail
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            "invalid_api_key"
                                | "authentication_error"
                                | "permission_denied"
                                | "unauthorized"
                                | "forbidden"
                        )
                    });
                let effective = if auth_type {
                    401
                } else {
                    match code {
                        "rate_limit_exceeded" | "rate_limit_error" | "too_many_requests" => 429,
                        "invalid_api_key"
                        | "authentication_error"
                        | "permission_denied"
                        | "unauthorized"
                        | "forbidden" => 401,
                        "insufficient_quota" | "insufficient_balance" => 402,
                        "invalid_request_error"
                        | "invalid_encrypted_content"
                        | "tool_call_mismatch" => 400,
                        _ => 502,
                    }
                };
                (
                    FailureClass::from_status_with_body(effective, body),
                    Some(effective),
                )
            }
            crate::Error::Upstream { status, body, .. } => (
                FailureClass::from_status_with_body(*status, body),
                Some(*status),
            ),
            crate::Error::Transport { .. } => {
                (FailureClass::Transport, Some(error.status().as_u16()))
            }
            crate::Error::BadUpstreamPayload(message) => {
                let lower = message.to_ascii_lowercase();
                let class = if is_session_binding_invalidated(200, message) {
                    FailureClass::InvalidRequest
                } else if is_cancelled_message(&lower) {
                    FailureClass::ClientCancelled
                } else if is_interrupted_message(&lower) {
                    FailureClass::Interrupted
                } else {
                    FailureClass::Protocol
                };
                (class, Some(error.status().as_u16()))
            }
            // The body ended after partial output without ever naming a normal
            // terminal: the partial answer is an interruption, never a success,
            // and it is not evidence that more retrying would help.
            crate::Error::InterruptedStream { .. } => {
                (FailureClass::Interrupted, Some(error.status().as_u16()))
            }
            crate::Error::Timeout(_) => (FailureClass::Timeout, Some(error.status().as_u16())),
            crate::Error::Persistence => {
                (FailureClass::Configuration, Some(error.status().as_u16()))
            }
            crate::Error::Internal(message) => {
                let lower = message.to_ascii_lowercase();
                let class = if is_cancelled_message(&lower) {
                    FailureClass::ClientCancelled
                } else if is_interrupted_message(&lower) {
                    FailureClass::Interrupted
                } else {
                    FailureClass::Configuration
                };
                (class, Some(error.status().as_u16()))
            }
        };
        let message = error.safe_message();
        Self::from_parts(class, status, message)
    }

    /// Construct an explicit client-cancellation terminal state.
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::from_parts(FailureClass::ClientCancelled, None, message.into())
    }

    /// Construct an explicit interrupted-stream terminal state.
    pub fn interrupted(message: impl Into<String>) -> Self {
        Self::from_parts(FailureClass::Interrupted, None, message.into())
    }

    fn from_parts(class: FailureClass, status: Option<u16>, message: String) -> Self {
        Self {
            class,
            status,
            message,
            impact: class.impact(),
        }
    }

    /// A classified failure is never a successful terminal result.
    pub const fn is_success(&self) -> bool {
        false
    }

    pub const fn retryable(&self) -> bool {
        self.impact.retryable
    }

    pub const fn fallbackable(&self) -> bool {
        self.impact.fallbackable
    }

    pub const fn affects_circuit(&self) -> bool {
        self.impact.affects_circuit
    }

    pub const fn affects_observation(&self) -> bool {
        self.impact.affects_observation
    }

    pub const fn records_stats(&self) -> bool {
        self.impact.records_stats()
    }

    pub const fn provider_fault(&self) -> bool {
        self.impact.provider_fault
    }
}

impl From<&crate::Error> for ClassifiedFailure {
    fn from(error: &crate::Error) -> Self {
        Self::from_core_error(error)
    }
}

impl From<crate::Error> for ClassifiedFailure {
    fn from(error: crate::Error) -> Self {
        Self::from_core_error(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Every FailureClass maps to the correct impact flags
    // -------------------------------------------------------------------

    #[test]
    fn transport_impact() {
        let i = FailureClass::Transport.impact();
        assert!(i.affects_observation);
        assert!(i.affects_circuit);
        assert!(i.retryable);
        assert!(i.fallbackable);
        assert!(i.provider_fault);
    }

    #[test]
    fn timeout_impact() {
        let i = FailureClass::Timeout.impact();
        assert!(i.affects_observation);
        assert!(i.affects_circuit);
        assert!(i.retryable);
        assert!(i.fallbackable);
        assert!(i.provider_fault);
    }

    #[test]
    fn rate_limit_impact() {
        let i = FailureClass::RateLimit.impact();
        assert!(i.affects_observation);
        assert!(!i.affects_circuit, "rate limits should not open circuit");
        assert!(i.retryable);
        assert!(i.fallbackable);
        assert!(!i.provider_fault, "rate limit is caller's fault");
    }

    #[test]
    fn authentication_impact() {
        let i = FailureClass::Authentication.impact();
        assert!(!i.affects_observation, "auth failure is a config issue");
        assert!(!i.affects_circuit);
        assert!(!i.retryable);
        assert!(!i.fallbackable, "same key will fail everywhere");
        assert!(!i.provider_fault);
    }

    #[test]
    fn provider_unavailable_impact() {
        let i = FailureClass::ProviderUnavailable.impact();
        assert!(i.affects_observation);
        assert!(i.affects_circuit);
        assert!(i.retryable);
        assert!(i.fallbackable);
        assert!(i.provider_fault);
    }

    #[test]
    fn protocol_impact() {
        let i = FailureClass::Protocol.impact();
        assert!(i.affects_observation);
        assert!(i.affects_circuit);
        assert!(
            !i.retryable,
            "same request will produce same protocol error"
        );
        assert!(i.fallbackable);
        assert!(i.provider_fault);
    }

    #[test]
    fn capability_impact() {
        let i = FailureClass::Capability.impact();
        assert!(!i.affects_observation, "capability is a model limitation");
        assert!(!i.affects_circuit);
        assert!(!i.retryable);
        assert!(i.fallbackable, "try a different model");
        assert!(!i.provider_fault);
    }

    #[test]
    fn invalid_request_impact() {
        let i = FailureClass::InvalidRequest.impact();
        assert!(!i.affects_observation);
        assert!(!i.affects_circuit);
        assert!(!i.retryable);
        assert!(!i.fallbackable);
        assert!(!i.provider_fault);
    }

    #[test]
    fn client_cancelled_impact() {
        let i = FailureClass::ClientCancelled.impact();
        assert!(!i.affects_observation);
        assert!(!i.affects_circuit);
        assert!(!i.retryable);
        assert!(!i.fallbackable);
        assert!(!i.provider_fault);
    }

    #[test]
    fn unknown_impact() {
        let i = FailureClass::Unknown.impact();
        assert!(i.affects_observation);
        assert!(i.affects_circuit);
        assert!(i.retryable);
        assert!(i.fallbackable);
        assert!(i.provider_fault, "unknown should assume provider fault");
    }

    // -------------------------------------------------------------------
    // from_status for each common status code
    // -------------------------------------------------------------------

    #[test]
    fn status_400_is_a_provider_rejection() {
        // The body that reached the upstream was built by this gateway, so the
        // refusal is evidence about the provider/model contract and counts
        // toward the circuit. A locally invalid request never gets here: the
        // pipeline refuses it before an attempt and classifies the structural
        // `Error::InvalidRequest` directly.
        assert_eq!(
            FailureClass::from_status(400),
            FailureClass::ProviderRejected
        );
        // Another candidate may accept the shape this one refused, so a 400
        // fails over instead of ending the client's request. That holds even
        // when the body calls the request invalid: the body the provider saw was
        // built here, so the refusal is about the provider/model contract. A
        // request this pipeline itself refuses never reaches this path and keeps
        // `InvalidRequest`.
        assert!(FailureClass::ProviderRejected.impact().fallbackable);
        assert!(!FailureClass::ProviderRejected.impact().retryable);
        assert_eq!(
            FailureClass::from_status_with_body(
                400,
                r#"{"error":{"type":"invalid_request_error","message":"bad request"}}"#
            ),
            FailureClass::ProviderRejected
        );
    }

    #[test]
    fn status_402_is_balance_exhausted() {
        // 402 means the provider wants payment. In this gateway's upstream
        // context that is the provider's wallet, so it is a provider fact that
        // must remove the provider from routing, not a client budget denial.
        assert_eq!(
            FailureClass::from_status(402),
            FailureClass::BalanceExhausted
        );
        let impact = FailureClass::BalanceExhausted.impact();
        assert!(impact.affects_observation);
        assert!(impact.affects_circuit);
        assert!(!impact.retryable);
        assert!(!impact.retryable);
        assert!(impact.fallbackable);
        assert!(impact.provider_fault);

        // 412 is a precondition failure, not a payment one.
        assert_eq!(
            FailureClass::from_status(412),
            FailureClass::ProviderRejected
        );
    }

    #[test]
    fn billing_markers_win_over_the_status_table() {
        assert_eq!(
            FailureClass::from_status_with_body(
                402,
                r#"{"error":{"message":"Insufficient balance"}}"#
            ),
            FailureClass::BalanceExhausted
        );
        // OpenAI reports a wallet with no funds as 429 `insufficient_quota`.
        // Reading that as a transient rate limit kept sending traffic to a
        // provider that answers every request the same way.
        assert_eq!(
            FailureClass::from_status_with_body(
                429,
                r#"{"error":{"type":"insufficient_quota","message":"You exceeded your current quota"}}"#
            ),
            FailureClass::BalanceExhausted
        );
        assert_eq!(
            FailureClass::from_status_with_body(402, "余额不足，请充值"),
            FailureClass::BalanceExhausted
        );
        // A real rate limit stays a rate limit.
        assert_eq!(
            FailureClass::from_status_with_body(429, "rate limit exceeded"),
            FailureClass::RateLimit
        );
        assert_eq!(
            FailureClass::from_status_with_body(400, "invalid billing details"),
            FailureClass::ProviderRejected,
            "mentioning billing does not prove an empty wallet"
        );
    }

    #[test]
    fn tool_mismatch_is_terminal_but_a_zen_client_gate_can_fail_over() {
        let failure = ClassifiedFailure::from_status(400, r#"{"error":{"code":"tool_call_mismatch","message":"tool calls and tool results do not match"}}"#.into());
        assert_eq!(failure.class, FailureClass::InvalidRequest);
        assert!(!failure.retryable() && !failure.fallbackable());
        assert!(!failure.affects_circuit());
        let zen = ClassifiedFailure::from_status(403, r#"{"error":{"type":"FreeTierError","message":"OpenCode's free tier can only be used from within OpenCode"}}"#.into());
        assert_eq!(zen.class, FailureClass::Capability);
        assert!(zen.fallbackable());
        assert!(!zen.retryable());
    }

    #[test]
    fn status_401_is_authentication() {
        assert_eq!(FailureClass::from_status(401), FailureClass::Authentication);
    }

    #[test]
    fn status_403_is_authentication() {
        assert_eq!(FailureClass::from_status(403), FailureClass::Authentication);
    }

    #[test]
    fn status_451_is_provider_rejected_and_can_fail_over() {
        let class = FailureClass::from_status(451);
        assert_eq!(class, FailureClass::ProviderRejected);
        let impact = class.impact();
        assert!(!impact.retryable);
        assert!(impact.fallbackable);
        assert!(impact.provider_fault);
    }

    #[test]
    fn status_408_is_timeout() {
        assert_eq!(FailureClass::from_status(408), FailureClass::Timeout);
    }

    #[test]
    fn status_429_is_rate_limit() {
        assert_eq!(FailureClass::from_status(429), FailureClass::RateLimit);
    }

    #[test]
    fn status_500_is_unknown() {
        assert_eq!(FailureClass::from_status(500), FailureClass::Unknown);
    }

    #[test]
    fn status_502_is_provider_unavailable() {
        assert_eq!(
            FailureClass::from_status(502),
            FailureClass::ProviderUnavailable
        );
    }

    #[test]
    fn status_503_is_provider_unavailable() {
        assert_eq!(
            FailureClass::from_status(503),
            FailureClass::ProviderUnavailable
        );
    }

    #[test]
    fn status_504_is_provider_unavailable() {
        assert_eq!(
            FailureClass::from_status(504),
            FailureClass::ProviderUnavailable
        );
    }

    #[test]
    fn status_422_is_protocol() {
        assert_eq!(FailureClass::from_status(422), FailureClass::Protocol);
    }

    #[test]
    fn status_below_400_is_unknown() {
        assert_eq!(FailureClass::from_status(200), FailureClass::Unknown);
        assert_eq!(FailureClass::from_status(301), FailureClass::Unknown);
    }

    // -------------------------------------------------------------------
    // from_error_message for common error patterns
    // -------------------------------------------------------------------

    #[test]
    fn error_message_timeout() {
        assert_eq!(
            FailureClass::from_error_message("request timed out"),
            FailureClass::Timeout
        );
        assert_eq!(
            FailureClass::from_error_message("Connection timeout after 30s"),
            FailureClass::Timeout
        );
    }

    #[test]
    fn error_message_transport() {
        assert_eq!(
            FailureClass::from_error_message("connection refused"),
            FailureClass::Transport
        );
        assert_eq!(
            FailureClass::from_error_message("DNS resolution failed"),
            FailureClass::Transport
        );
        assert_eq!(
            FailureClass::from_error_message("TLS handshake error"),
            FailureClass::Transport
        );
    }

    #[test]
    fn error_message_rate_limit() {
        assert_eq!(
            FailureClass::from_error_message("rate limit exceeded"),
            FailureClass::RateLimit
        );
        assert_eq!(
            FailureClass::from_error_message("HTTP 429 Too Many Requests"),
            FailureClass::RateLimit
        );
    }

    #[test]
    fn error_message_authentication() {
        assert_eq!(
            FailureClass::from_error_message("Unauthorized access"),
            FailureClass::Authentication
        );
        assert_eq!(
            FailureClass::from_error_message("HTTP 401"),
            FailureClass::Authentication
        );
        assert_eq!(
            FailureClass::from_error_message("HTTP 403 Forbidden"),
            FailureClass::Authentication
        );
    }

    #[test]
    fn error_message_capability() {
        assert_eq!(
            FailureClass::from_error_message("model does not support vision"),
            FailureClass::Capability
        );
        assert_eq!(
            FailureClass::from_error_message("feature unsupported"),
            FailureClass::Capability
        );
        assert_eq!(
            FailureClass::from_error_message("tool use not supported"),
            FailureClass::Capability
        );
    }

    #[test]
    fn error_message_cancelled() {
        assert_eq!(
            FailureClass::from_error_message("request cancelled by user"),
            FailureClass::ClientCancelled
        );
        assert_eq!(
            FailureClass::from_error_message("stream aborted"),
            FailureClass::ClientCancelled
        );
    }

    #[test]
    fn error_message_unknown_fallback() {
        assert_eq!(
            FailureClass::from_error_message("something unexpected"),
            FailureClass::Unknown
        );
    }

    // -------------------------------------------------------------------
    // Critical invariants
    // -------------------------------------------------------------------

    #[test]
    fn invariant_invalid_request_never_affects_observation() {
        assert!(!FailureClass::InvalidRequest.impact().affects_observation);
    }

    #[test]
    fn invariant_authentication_never_affects_circuit() {
        assert!(!FailureClass::Authentication.impact().affects_circuit);
    }

    #[test]
    fn invariant_client_cancelled_never_affects_observation() {
        assert!(!FailureClass::ClientCancelled.impact().affects_observation);
    }

    #[test]
    fn invariant_capability_always_fallbackable_never_retryable() {
        let i = FailureClass::Capability.impact();
        assert!(i.fallbackable, "capability should be fallbackable");
        assert!(!i.retryable, "capability should not be retryable");
    }

    #[test]
    fn invariant_transport_always_retryable_and_fallbackable() {
        let i = FailureClass::Transport.impact();
        assert!(i.retryable, "transport should be retryable");
        assert!(i.fallbackable, "transport should be fallbackable");
    }

    // -------------------------------------------------------------------
    // ClassifiedFailure constructors
    // -------------------------------------------------------------------

    #[test]
    fn classified_failure_from_status() {
        let f = ClassifiedFailure::from_status(429, "rate limited".into());
        assert_eq!(f.class, FailureClass::RateLimit);
        assert_eq!(f.status, Some(429));
        assert_eq!(f.message, "rate limited");
        assert!(f.impact.fallbackable);
    }

    #[test]
    fn classified_failure_from_error() {
        let f = ClassifiedFailure::from_message("connection refused");
        assert_eq!(f.class, FailureClass::Transport);
        assert!(f.status.is_none());
        assert!(f.impact.retryable);
    }

    // -------------------------------------------------------------------
    // All FailureClass variants produce valid FailureImpact
    // -------------------------------------------------------------------

    #[test]
    fn all_variants_produce_impact() {
        for class in FailureClass::ALL {
            let impact = class.impact();
            // Every provider fault has to be actionable somehow: either the
            // same candidate may be retried or another one may be tried.
            if impact.provider_fault {
                assert!(
                    impact.retryable || impact.fallbackable,
                    "{class:?} is provider_fault but neither retryable nor fallbackable"
                );
            }
            // A class that touches neither health view is a local decision and
            // must never be attributed to the provider.
            if !impact.affects_observation && !impact.affects_circuit {
                assert!(
                    !impact.provider_fault,
                    "{class:?} is a local decision and cannot blame the provider"
                );
            }
            assert!(impact.records_stats());
        }
    }
}
