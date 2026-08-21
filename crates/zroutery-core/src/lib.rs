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
//! * [`billing`] prices a request and reads provider balances.
//! * [`config`] holds providers, the model registry and routing policy.
//! * [`registry`] resolves a client model id (including `*-class` virtual ids).
//! * [`router`] picks candidates, tracks health and drives failover.
//! * [`protocol`] contains the two decoders and two encoders plus SSE handling.
//! * [`upstream`] talks HTTP to providers.
//! * [`server`] exposes the axum app used by the desktop shell.

pub mod billing;
pub mod config;
pub mod error;
pub mod ir;
pub mod protocol;
pub mod registry;
pub mod router;
pub mod server;
pub mod stats;
mod sync;
pub mod upstream;

pub use billing::{Balance, BalanceConfig, BalancePreset, BalanceProbe, Cost, CostTotals, Pricing};
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
pub use server::{build_app, AppState, ServerHandle};
pub use stats::{RequestRecord, Stats};
pub use upstream::{DiscoveredModel, Upstream};
