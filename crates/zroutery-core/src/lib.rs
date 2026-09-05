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
//! * [`budget`] stops spending once a limit is reached, and remembers across restarts.
//! * [`classifier`] detects Auto Mode classifier side queries and reads their verdicts.
//! * [`election`] picks a tier's primary from measured latency and price.
//! * [`config`] holds providers, the model registry and routing policy.
//! * [`query`] says what a request is *for* (main vs side query), as opposed to
//!   [`registry`], which says which model it wants.
//! * [`registry`] resolves a client model id (including `*-class` virtual ids).
//! * [`router`] picks candidates, tracks health and drives failover.
//! * [`protocol`] contains the two decoders and two encoders plus SSE handling.
//! * [`upstream`] talks HTTP to providers.
//! * [`server`] exposes the axum app used by the desktop shell.

pub mod billing;
pub mod budget;
pub mod circuit_breaker;
pub mod classifier;
pub mod config;
pub mod election;
pub mod error;
pub mod ir;
pub mod media;
pub mod observation;
pub mod policy;
pub mod protocol;
pub mod query;
pub mod rectifier;
pub mod registry;
pub mod router;
pub mod server;
pub mod stats;
mod sync;
pub mod upstream;

pub use billing::{
    Balance, BalanceConfig, BalancePreset, BalanceProbe, BaseDepth, Cost, CostTotals, Pricing,
};
pub use budget::{Budget, BudgetPeriod, BudgetScope, Ledger, OnExceeded, Verdict};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use classifier::{
    ClassifierSignature, ClassifierVerdict, Detection as ClassifierDetection, DetectionConfig,
};
pub use config::{
    AppConfig, ClassifierCandidate, ClassifierConfig, ConfigIssue, IssueSeverity,
    MemorySecretStore, ModelCapabilities, ModelEntry, ModelTier, NamingStyle, ProviderConfig,
    ProviderKind, RectifierConfig, RoutingConfig, RoutingStrategy, SecretStore, ServerConfig,
    VisionConfig, WindowBehavior,
};
pub use election::{TierElection, Election, Measurement, Ranked, ScoringConfig};
pub use error::{Error, Result};
pub use ir::{
    Capability, ChatRequest, ChatResponse, ContentBlock, Dialect, Message, Role, StopReason,
    StreamEvent, SystemPart, ToolChoice, Usage,
};
pub use ir::response::{ResponseStatus, ResponseStore, StoredResponse};
pub use observation::{
    HealthState, LatencyObservation, ObservationFreshness, ObservationStore, RuntimeObservation,
    Signal,
};
pub use policy::{
    ClientContext, ClientMatcher, ClientProfile, EligibilityCheck, PolicyConfig, PolicyFallback,
    PolicyMatcher, PolicyPreference, PolicyRequirements, RejectionReason, RoutingPolicy,
    resolve_client,
};
pub use query::{RequestKind, SideQueryKind};
pub use registry::{Registry, Resolution};
pub use router::{Candidate, Router};
pub use server::{build_app, AppState, ServerHandle};
pub use stats::{RequestRecord, Stats};
pub use upstream::{DiscoveredModel, Upstream};
