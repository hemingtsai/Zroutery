//! Failure classification for observation and routing decisions.
//!
//! Not all failures are equal. A 429 rate limit is transient and retryable;
//! an authentication error is permanent and should not retry. This module
//! classifies failures so the observation layer can make correct decisions
//! about health, circuit breaking, and fallback.

use serde::{Deserialize, Serialize};

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
    /// Upstream returned a terminal payment/precondition rejection (402/412)
    /// without a deterministic local-configuration or budget marker.
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
    pub const ALL: [Self; 16] = [
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
        Self::Interrupted,
        Self::Unknown,
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
                retryable: false, // no evidence that another candidate helps
                fallbackable: false,
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
    /// A bare 402/412 is a provider rejection; it is not evidence that the
    /// caller is over budget or missing a provider key.
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
        let lower = body.map(str::to_ascii_lowercase);
        if status == 500
            && lower.as_deref().is_some_and(|body| {
                body.contains("invalid_request_error") || body.contains("invalid request")
            })
        {
            return Self::InvalidRequest;
        }
        // A status alone is not evidence of a local budget/key decision.
        // Only explicit, deterministic body markers may select local classes;
        // otherwise 402/412 remain provider rejections below.
        if let Some(body) = lower.as_deref() {
            if matches!(status, 402 | 404 | 412 | 503) {
                if is_missing_key_message(body) {
                    return Self::MissingApiKey;
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
            400 => Self::InvalidRequest,
            402 | 412 => Self::ProviderRejected,
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
        if is_cancelled_message(&lower) {
            Self::ClientCancelled
        } else if is_interrupted_message(&lower) {
            Self::Interrupted
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

fn is_over_budget_message(message: &str) -> bool {
    message.contains("over budget")
        || message.contains("budget exceeded")
        || message.contains("budget")
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
            crate::Error::Unauthorized => {
                (FailureClass::Authentication, Some(error.status().as_u16()))
            }
            crate::Error::MissingApiKey(_) => {
                (FailureClass::MissingApiKey, Some(error.status().as_u16()))
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
                let class = if is_cancelled_message(&lower) {
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
    fn status_400_is_invalid_request() {
        assert_eq!(FailureClass::from_status(400), FailureClass::InvalidRequest);
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
        let classes = [
            FailureClass::Transport,
            FailureClass::Timeout,
            FailureClass::RateLimit,
            FailureClass::Authentication,
            FailureClass::ProviderUnavailable,
            FailureClass::ProviderRejected,
            FailureClass::Protocol,
            FailureClass::Capability,
            FailureClass::InvalidRequest,
            FailureClass::ClientCancelled,
            FailureClass::MissingApiKey,
            FailureClass::OverBudget,
            FailureClass::NoCandidate,
            FailureClass::Configuration,
            FailureClass::Interrupted,
            FailureClass::Unknown,
        ];
        for class in &classes {
            let impact = class.impact();
            // Most provider faults are actionable through retry/fallback;
            // ProviderRejected is the explicit terminal exception.
            if impact.provider_fault && !matches!(class, FailureClass::ProviderRejected) {
                assert!(
                    impact.retryable || impact.fallbackable,
                    "{:?} is provider_fault but neither retryable nor fallbackable",
                    class
                );
            }
        }
    }
}
