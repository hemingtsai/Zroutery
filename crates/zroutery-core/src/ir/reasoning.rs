//! Native effort is a request parameter, not a usage tier or token budget.
//! ThinkingConfig remains a compatible projection for legacy callers.

use serde::{Deserialize, Serialize};

use super::ThinkingConfig;

pub const REASONING_SCHEMA_VERSION: u32 = 1;
pub const NATIVE_EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReasoningRequest {
    Disabled,
    NativeEffort { value: String },
    TokenBudget { tokens: u32 },
}

impl ReasoningRequest {
    pub fn native(value: &str) -> Result<Self, String> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(
                "reasoning effort must be a non-empty native identifier of at most 64 bytes".into(),
            );
        }
        Ok(Self::NativeEffort {
            value: value.to_string(),
        })
    }

    pub fn effort(&self) -> Option<&str> {
        match self {
            Self::NativeEffort { value } => Some(value),
            Self::Disabled => Some("none"),
            _ => None,
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            Self::Disabled => false,
            Self::NativeEffort { value } => value != "none",
            Self::TokenBudget { tokens } => *tokens > 0,
        }
    }

    pub fn legacy(&self) -> ThinkingConfig {
        match self {
            Self::Disabled => ThinkingConfig {
                enabled: false,
                budget_tokens: None,
            },
            Self::TokenBudget { tokens } => ThinkingConfig {
                enabled: *tokens > 0,
                budget_tokens: Some(*tokens),
            },
            Self::NativeEffort { value } => crate::protocol::reasoning_bridge::decode_effort(value)
                .unwrap_or(ThinkingConfig {
                    enabled: value != "none",
                    budget_tokens: None,
                }),
        }
    }
}
