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
//! * [`registry`] resolves a client model id (including `*-class` virtual ids).
//! * [`router`] picks candidates, tracks health and drives failover.

pub mod config;
pub mod error;
pub mod ir;
pub mod registry;
pub mod router;

pub use config::{
    AppConfig, ConfigIssue, IssueSeverity, MemorySecretStore, ModelClass, ModelEntry,
    ProviderConfig, ProviderKind, RoutingConfig, RoutingStrategy, SecretStore, ServerConfig,
};
pub use error::{Error, Result};
pub use ir::{
    ChatRequest, ChatResponse, ContentBlock, Dialect, Message, Role, StopReason, StreamEvent, Usage,
};
pub use registry::{Registry, Resolution};
pub use router::{Candidate, Router};
