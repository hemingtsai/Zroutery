//! Request rectifiers: attempt to repair a provider-rejected request before
//! failing over to another provider.
//!
//! Rectifier retries deliberately do not count against the circuit breaker:
//! they are the same provider and the same model, and the failure was caused by
//! a fixable request shape, not by provider health.

pub mod media_fallback;
pub mod thinking_budget;
pub mod thinking_signature;

use serde_json::Value;

use crate::config::RectifierConfig;
use crate::error::Error;

/// Result of applying a rectifier to a request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RectifyResult {
    pub applied: bool,
    pub details: String,
}

/// A stateless request transformer keyed by provider error signatures.
pub trait Rectifier: Send + Sync {
    /// Check whether this error matches the rectifier's repair condition.
    fn should_apply(&self, error: &Error, body: &Value) -> bool;

    /// Modify the request body in place.
    fn rectify(&self, body: &mut Value) -> RectifyResult;

    /// Rectifier name for logs and the tried-set.
    fn name(&self) -> &'static str;
}

/// Build the enabled rectifiers from routing configuration.
pub fn from_config(config: &RectifierConfig) -> Vec<Box<dyn Rectifier>> {
    let mut out: Vec<Box<dyn Rectifier>> = Vec::new();
    if !config.enabled {
        return out;
    }
    if config.thinking_signature {
        out.push(Box::new(thinking_signature::ThinkingSignatureRectifier));
    }
    if config.media_fallback {
        out.push(Box::new(media_fallback::MediaFallbackRectifier));
    }
    if config.thinking_budget {
        out.push(Box::new(thinking_budget::ThinkingBudgetRectifier));
    }
    out
}

pub(crate) fn error_text(error: &Error) -> String {
    error.to_string().to_lowercase()
}

pub(crate) fn contains_all(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().all(|n| haystack.contains(n))
}

pub(crate) fn messages_mut(body: &mut Value) -> Option<&mut Vec<Value>> {
    body.get_mut("messages")?.as_array_mut()
}
