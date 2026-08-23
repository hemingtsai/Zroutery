//! Error type shared by the whole core, with mapping to both wire dialects.

use axum::http::StatusCode;
use serde_json::{json, Value};
use thiserror::Error;

use crate::ir::Dialect;

#[derive(Debug, Error)]
pub enum Error {
    /// Client sent something we cannot parse or that violates the schema.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Body larger than `server.max_body_mib`.
    #[error("request body is too large (limit {limit_mib} MiB)")]
    TooLarge { limit_mib: usize },

    /// Client model id resolved to nothing.
    #[error("model `{0}` is not available")]
    UnknownModel(String),

    /// Nothing is served at that path.
    #[error("no endpoint at {0}")]
    UnknownRoute(String),

    /// A spending limit the user set has been reached.
    #[error("stopped by a budget: {0}")]
    OverBudget(String),

    /// A `*-class` id has no enabled, healthy member.
    #[error("no model is available for `{0}`")]
    NoCandidate(String),

    #[error("missing or invalid credentials")]
    Unauthorized,

    /// Provider has no API key in the secret store.
    #[error("provider `{0}` has no API key configured")]
    MissingApiKey(String),

    /// Upstream answered with a non 2xx status.
    #[error("upstream {provider} returned {status}: {body}")]
    Upstream {
        provider: String,
        status: u16,
        body: String,
    },

    /// Network level failure talking to the upstream.
    #[error("cannot reach upstream {provider}: {source}")]
    Transport {
        provider: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("upstream returned malformed data: {0}")]
    BadUpstreamPayload(String),

    #[error("request timed out after {0}s")]
    Timeout(u64),

    #[error("{0}")]
    Internal(String),
}

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::InvalidRequest(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Error::Internal(msg.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Error::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Error::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Error::UnknownModel(_) | Error::UnknownRoute(_) => StatusCode::NOT_FOUND,
            Error::NoCandidate(_) => StatusCode::SERVICE_UNAVAILABLE,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::MissingApiKey(_) => StatusCode::PRECONDITION_FAILED,
            // The request is well formed and authorised; what is missing is money.
            Error::OverBudget(_) => StatusCode::PAYMENT_REQUIRED,
            Error::Upstream { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            Error::Transport { .. } => StatusCode::BAD_GATEWAY,
            Error::BadUpstreamPayload(_) => StatusCode::BAD_GATEWAY,
            Error::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Anthropic style error `type` / OpenAI style error `code`.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::InvalidRequest(_) => "invalid_request_error",
            Error::TooLarge { .. } => "request_too_large",
            Error::UnknownModel(_) | Error::UnknownRoute(_) => "not_found_error",
            Error::NoCandidate(_) => "overloaded_error",
            Error::Unauthorized => "authentication_error",
            Error::MissingApiKey(_) => "authentication_error",
            Error::OverBudget(_) => "budget_exceeded",
            Error::Upstream { status, .. } => match *status {
                400 => "invalid_request_error",
                401 | 403 => "authentication_error",
                404 => "not_found_error",
                429 => "rate_limit_error",
                529 => "overloaded_error",
                _ => "api_error",
            },
            Error::Transport { .. } | Error::BadUpstreamPayload(_) => "api_error",
            Error::Timeout(_) => "timeout_error",
            Error::Internal(_) => "api_error",
        }
    }

    /// True when trying another candidate model could plausibly help.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Transport { .. } | Error::Timeout(_) | Error::BadUpstreamPayload(_) => true,
            // The next candidate may belong to a provider whose key does exist.
            Error::MissingApiKey(_) => true,
            Error::Upstream { status, .. } => {
                matches!(*status, 408 | 409 | 425 | 429 | 500..=599)
            }
            _ => false,
        }
    }

    /// True when the failure should count against the model's health.
    pub fn counts_against_health(&self) -> bool {
        !matches!(
            self,
            Error::InvalidRequest(_)
                | Error::UnknownModel(_)
                | Error::UnknownRoute(_)
                | Error::OverBudget(_)
                | Error::TooLarge { .. }
                // An absent key is a configuration problem, not evidence about
                // the model: counting it would cool down every model of the
                // provider until someone fixes the keychain.
                | Error::MissingApiKey(_)
        )
    }

    /// Serialize into the shape the given dialect expects.
    pub fn to_wire(&self, dialect: Dialect) -> Value {
        let msg = self.to_string();
        match dialect {
            Dialect::Anthropic => json!({
                "type": "error",
                "error": { "type": self.kind(), "message": msg }
            }),
            Dialect::OpenAI => json!({
                "error": {
                    "message": msg,
                    "type": self.kind(),
                    "code": self.kind(),
                    "param": Value::Null,
                }
            }),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification() {
        assert!(Error::Upstream {
            provider: "p".into(),
            status: 429,
            body: String::new()
        }
        .is_retryable());
        assert!(Error::Upstream {
            provider: "p".into(),
            status: 503,
            body: String::new()
        }
        .is_retryable());
        assert!(!Error::Upstream {
            provider: "p".into(),
            status: 400,
            body: String::new()
        }
        .is_retryable());
        assert!(!Error::invalid("bad").is_retryable());
        assert!(Error::Timeout(30).is_retryable());
    }

    #[test]
    fn wire_shapes() {
        let e = Error::UnknownModel("x".into());
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        let a = e.to_wire(Dialect::Anthropic);
        assert_eq!(a["type"], "error");
        assert_eq!(a["error"]["type"], "not_found_error");
        let o = e.to_wire(Dialect::OpenAI);
        assert_eq!(o["error"]["code"], "not_found_error");
        assert!(o["error"]["message"].as_str().unwrap().contains('x'));
    }

    #[test]
    fn a_budget_stop_is_payment_required_and_never_retried() {
        let e = Error::OverBudget("the today limit for everything is used up".into());
        assert_eq!(e.status(), StatusCode::PAYMENT_REQUIRED);
        // Retrying or failing over would spend the money the budget just refused.
        assert!(!e.is_retryable());
        assert!(!e.counts_against_health());
        assert_eq!(
            e.to_wire(Dialect::OpenAI)["error"]["code"],
            "budget_exceeded"
        );
        assert_eq!(
            e.to_wire(Dialect::Anthropic)["error"]["type"],
            "budget_exceeded"
        );
    }

    #[test]
    fn health_accounting_ignores_client_errors() {
        assert!(!Error::invalid("nope").counts_against_health());
        assert!(!Error::TooLarge { limit_mib: 32 }.counts_against_health());
        assert!(Error::Timeout(1).counts_against_health());
    }

    #[test]
    fn a_missing_key_fails_over_without_poisoning_health() {
        let e = Error::MissingApiKey("p".into());
        // Another provider's key may well exist, so keep trying candidates.
        assert!(e.is_retryable());
        // But the models themselves did nothing wrong.
        assert!(!e.counts_against_health());
        assert_eq!(e.status(), StatusCode::PRECONDITION_FAILED);
    }

    #[test]
    fn oversized_bodies_report_413_in_both_dialects() {
        let e = Error::TooLarge { limit_mib: 32 };
        assert_eq!(e.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!e.is_retryable());
        assert_eq!(
            e.to_wire(Dialect::Anthropic)["error"]["type"],
            "request_too_large"
        );
        assert_eq!(
            e.to_wire(Dialect::OpenAI)["error"]["code"],
            "request_too_large"
        );
        assert!(e.to_string().contains("32 MiB"));
    }
}
