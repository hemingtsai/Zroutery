//! What Zroutery thinks the task is: its complexity, its kind, and the profile derived from both.

use serde::{Deserialize, Serialize};

use crate::config::ModelTier;
use crate::ir::{Capability, ChatRequest};

// ----------------------------------------------------------- Profile

/// Task complexity level, derived from request characteristics.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Simple,
    #[default]
    Standard,
    Complex,
    Frontier,
}

/// The kind of task the request represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    #[default]
    Chat,
    Code,
    Vision,
    Analysis,
    Creative,
    ToolUse,
}

/// Derived profile of a request, fed into policy matchers.
#[derive(Debug, Clone, Default)]
pub struct TaskProfile {
    pub tier: Option<ModelTier>,
    pub required_capabilities: Vec<Capability>,
    pub context_tokens: u64,
    pub estimated_output_tokens: u64,
    pub streaming: bool,
    pub has_tools: bool,
    pub has_vision: bool,
    pub complexity: Complexity,
    pub task_type: TaskType,
}

impl TaskProfile {
    /// Derive a [`TaskProfile`] from a [`ChatRequest`].
    pub fn from_request(req: &ChatRequest) -> Self {
        let required_capabilities = req.compute_required_capabilities();
        let context_tokens = req.estimate_tokens() as u64;
        let estimated_output_tokens = req.max_tokens.unwrap_or(4096) as u64;
        let has_tools = !req.tools.is_empty();
        let has_vision = required_capabilities.contains(&Capability::Vision);
        let thinking_enabled = req.thinking.as_ref().is_some_and(|t| t.enabled);
        let total_tokens = context_tokens + estimated_output_tokens;

        let complexity = if context_tokens > 200_000 && thinking_enabled && has_tools {
            Complexity::Frontier
        } else if thinking_enabled || total_tokens > 100_000 {
            Complexity::Complex
        } else if total_tokens > 10_000 || has_tools {
            Complexity::Standard
        } else {
            Complexity::Simple
        };

        let task_type = if has_tools {
            TaskType::ToolUse
        } else if has_vision {
            TaskType::Vision
        } else {
            TaskType::Chat
        };

        TaskProfile {
            tier: None,
            required_capabilities,
            context_tokens,
            estimated_output_tokens,
            streaming: req.stream,
            has_tools,
            has_vision,
            complexity,
            task_type,
        }
    }

    /// Suggest a model tier based on the task profile's complexity.
    pub fn suggested_tier(&self) -> ModelTier {
        match self.complexity {
            Complexity::Frontier => ModelTier::Frontier,
            Complexity::Complex => ModelTier::Reasoning,
            Complexity::Standard => ModelTier::Standard,
            Complexity::Simple => ModelTier::Fast,
        }
    }
}
