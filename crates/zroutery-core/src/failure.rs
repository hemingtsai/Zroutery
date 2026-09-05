//! Failure classification for observation and routing decisions.
//!
//! Not all failures are equal. A 429 rate limit is transient and retryable;
//! an authentication error is permanent and should not retry. This module
//! classifies failures so the observation layer can make correct decisions
//! about health, circuit breaking, and fallback.

use serde::{Deserialize, Serialize};

/// Classification of a request failure.
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
    /// Provider is unavailable (502, 503, 504).
    ProviderUnavailable,
    /// Protocol-level error (malformed response, unexpected format).
    Protocol,
    /// Request requires capabilities the model doesn't have.
    Capability,
    /// Client sent an invalid request (400, validation error).
    InvalidRequest,
    /// Client cancelled the request.
    ClientCancelled,
    /// Unknown or unclassified failure.
    Unknown,
}

/// The impact of a failure on routing decisions.
#[derive(Debug, Clone)]
pub struct FailureImpact {
    /// Should this failure count against the provider's observation health?
    pub affects_observation: bool,
    /// Should this failure count against the circuit breaker?
    pub affects_circuit: bool,
    /// Is this failure retryable on the same candidate?
    pub retryable: bool,
    /// Should the router try a different candidate after this failure?
    pub fallbackable: bool,
    /// Is this a provider-attributable failure (vs client error)?
    pub provider_fault: bool,
}

impl FailureClass {
    /// Determine the routing impact of this failure class.
    pub fn impact(&self) -> FailureImpact {
        match self {
            FailureClass::Transport => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
            FailureClass::Timeout => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
            FailureClass::RateLimit => FailureImpact {
                affects_observation: true, // degraded, not dead
                affects_circuit: false,    // don't open circuit for rate limits
                retryable: true,
                fallbackable: true,
                provider_fault: false, // caller's fault for too many requests
            },
            FailureClass::Authentication => FailureImpact {
                affects_observation: false, // config issue, not provider health
                affects_circuit: false,
                retryable: false,
                fallbackable: false, // same key will fail on all providers
                provider_fault: false,
            },
            FailureClass::ProviderUnavailable => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true,
            },
            FailureClass::Protocol => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: false, // same request will produce same error
                fallbackable: true,
                provider_fault: true,
            },
            FailureClass::Capability => FailureImpact {
                affects_observation: false, // model limitation, not health
                affects_circuit: false,
                retryable: false,
                fallbackable: true, // try a different model
                provider_fault: false,
            },
            FailureClass::InvalidRequest => FailureImpact {
                affects_observation: false, // client error
                affects_circuit: false,
                retryable: false,
                fallbackable: false, // same request will fail everywhere
                provider_fault: false,
            },
            FailureClass::ClientCancelled => FailureImpact {
                affects_observation: false, // not a provider failure
                affects_circuit: false,
                retryable: false,
                fallbackable: false,
                provider_fault: false,
            },
            FailureClass::Unknown => FailureImpact {
                affects_observation: true,
                affects_circuit: true,
                retryable: true,
                fallbackable: true,
                provider_fault: true, // assume provider fault when unknown
            },
        }
    }

    /// Classify an HTTP status code.
    pub fn from_status(status: u16) -> Self {
        match status {
            400 => FailureClass::InvalidRequest,
            401 | 403 => FailureClass::Authentication,
            408 => FailureClass::Timeout,
            429 => FailureClass::RateLimit,
            502 | 503 | 504 => FailureClass::ProviderUnavailable,
            500 => FailureClass::Unknown, // could be anything
            _ if status >= 400 => FailureClass::Protocol,
            _ => FailureClass::Unknown,
        }
    }

    /// Classify from an error string (for non-HTTP failures).
    pub fn from_error_message(msg: &str) -> Self {
        let lower = msg.to_lowercase();
        if lower.contains("timeout") || lower.contains("timed out") {
            FailureClass::Timeout
        } else if lower.contains("connection") || lower.contains("dns") || lower.contains("tls") {
            FailureClass::Transport
        } else if lower.contains("rate limit") || lower.contains("429") {
            FailureClass::RateLimit
        } else if lower.contains("unauthorized") || lower.contains("401") || lower.contains("403")
        {
            FailureClass::Authentication
        } else if lower.contains("capability")
            || lower.contains("unsupported")
            || lower.contains("not supported")
            || lower.contains("not support")
        {
            FailureClass::Capability
        } else if lower.contains("cancelled") || lower.contains("abort") {
            FailureClass::ClientCancelled
        } else {
            FailureClass::Unknown
        }
    }
}

/// A classified failure with context.
#[derive(Debug, Clone)]
pub struct ClassifiedFailure {
    pub class: FailureClass,
    pub status: Option<u16>,
    pub message: String,
    pub impact: FailureImpact,
}

impl ClassifiedFailure {
    pub fn from_status(status: u16, message: String) -> Self {
        let class = FailureClass::from_status(status);
        let impact = class.impact();
        ClassifiedFailure {
            class,
            status: Some(status),
            message,
            impact,
        }
    }

    pub fn from_error(message: String) -> Self {
        let class = FailureClass::from_error_message(&message);
        let impact = class.impact();
        ClassifiedFailure {
            class,
            status: None,
            message,
            impact,
        }
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
        assert!(!i.retryable, "same request will produce same protocol error");
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
        let f = ClassifiedFailure::from_error("connection refused".into());
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
            FailureClass::Protocol,
            FailureClass::Capability,
            FailureClass::InvalidRequest,
            FailureClass::ClientCancelled,
            FailureClass::Unknown,
        ];
        for class in &classes {
            let impact = class.impact();
            // At least one of retryable or fallbackable should be true for
            // provider-attributable failures, and both false for client errors.
            if impact.provider_fault {
                assert!(
                    impact.retryable || impact.fallbackable,
                    "{:?} is provider_fault but neither retryable nor fallbackable",
                    class
                );
            }
        }
    }
}
