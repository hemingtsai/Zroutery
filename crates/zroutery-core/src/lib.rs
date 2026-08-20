//! Zroutery core: aggregate several LLM providers behind one endpoint that
//! speaks both the Anthropic Messages API and the OpenAI Chat Completions API.
//!
//! Layering:
//!
//! ```text
//! ingress (anthropic | openai)  ->  IR  ->  router  ->  egress (anthropic | openai)
//! ```
//!
//! * [`ir`] is the canonical representation every dialect is translated through.
//! * [`config`] holds providers, the model registry and routing policy.

pub mod config;
pub mod error;
pub mod ir;

pub use config::{
    AppConfig, ConfigIssue, IssueSeverity, MemorySecretStore, ModelClass, ModelEntry,
    ProviderConfig, ProviderKind, RoutingConfig, RoutingStrategy, SecretStore, ServerConfig,
};
pub use error::{Error, Result};
pub use ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, Message, Role, StopReason, StreamEvent, Usage,
};
