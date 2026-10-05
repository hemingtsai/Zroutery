//! Zroutery core: aggregate several LLM providers behind one endpoint that
//! speaks the Anthropic Messages API, the OpenAI Chat Completions and Responses
//! APIs, and Gemini's `generateContent`.
//!
//! Layering:
//!
//! ```text
//! ingress (anthropic | openai | responses | gemini)
//!     -> IR -> router -> egress (anthropic | openai)
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
//! * [`protocol`] contains one decoder and encoder per dialect, plus SSE handling.
//! * [`upstream`] talks HTTP to providers.
//! * [`server`] exposes the axum app used by the desktop shell.

pub mod agent_takeover;
pub mod billing;
pub mod budget;
pub mod circuit_breaker;
pub mod classifier;
pub mod config;
pub mod election;
pub mod error;
pub mod failure;
pub mod feedback;
pub mod ir;
pub mod media;
pub mod migration;
#[cfg(feature = "ml")]
pub mod ml;
pub mod observability;
pub mod observation;
pub mod outcome;
pub mod policy;
pub mod protocol;
pub mod query;
pub mod rectifier;
pub mod registry;
pub mod router;
pub mod server;
pub mod session;
pub mod stats;
pub mod stats_ext;
mod sync;
pub mod upstream;

#[cfg(feature = "account")]
pub mod account;

pub use agent_takeover::{ExternalModification, OwnershipManifest, OwnershipState, TakeoverStore};
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
pub use election::{Election, Measurement, Ranked, ScoringConfig, TierElection};
pub use error::{Error, Result};
pub use failure::{ClassifiedFailure, FailureClass, FailureImpact};
pub use feedback::{
    feedback_from_outcome, outcome_to_feedback, try_feedback_from_outcome, DataOrigin, Feedback,
    FeedbackSignal, FeedbackSource, OutcomeSummary, TrainingSample as FeedbackTrainingSample,
};
pub use ir::response::{ResponseStatus, ResponseStore, StoredResponse};
pub use ir::{
    Capability, ChatRequest, ChatResponse, ContentBlock, Dialect, Message, Role, StopReason,
    StreamEvent, SystemPart, ToolChoice, Usage,
};
pub use migration::{
    MigrationAction, MigrationPlan, MigrationResult, MigrationState, MigrationStep, MigrationStore,
};
#[cfg(feature = "ml")]
pub use ml::{
    extract_features, outcome_to_dataset_sample, samples_from_outcome, temporal_split,
    validate_outcome_sample, validate_sample, Action, ActionGuard, ActiveModel, ActiveModelStore,
    AttemptReward, CanonicalTrainingSample, ComparisonReport, DatasetStore, DatasetTrainingSample,
    Evaluator, ExplorationConfig, FeatureContext, FrozenHoldout, MlRouter, MlStatus,
    OutcomeDatasetSample, OutcomeTrainingSample, PredictionMetrics, PromotedModelStatus,
    PromotionDecision, PromotionGate, PromotionVerdict, Recommendation, ReloadOutcome,
    RequestReward, RewardComputer, RewardPolicy, RoutingDeltas, RoutingFeatures, RoutingMetrics,
    SampleBuilder, SampleScope, ShadowAnalysisStatus, Targets, FEATURE_DIMENSION,
    FEATURE_SCHEMA_VERSION,
};
pub use observation::{
    HealthState, LatencyObservation, ObservationFreshness, ObservationStore, RuntimeObservation,
    Signal,
};
pub use outcome::{
    failure_class_wire_name, Attempt, CandidateIdentity, ErrorFacts, FailureFacts, FinalStatus,
    Outcome, OutcomeBuilder, OutcomeIdentity,
};
pub use policy::{
    resolve_client, ClientContext, ClientMatcher, ClientProfile, EligibilityCheck, PolicyConfig,
    PolicyFallback, PolicyMatcher, PolicyPreference, PolicyRequirements, RejectionReason,
    RoutingPolicy,
};
pub use query::{RequestKind, SideQueryKind};
pub use registry::{Registry, Resolution};
pub use router::{Candidate, Router};
pub use server::{build_app, AppState, ServerHandle};
pub use session::{SessionRoutingMode, SessionState, SessionStore};
pub use stats::{RequestRecord, Stats};
pub use stats_ext::{
    Ewma, FailureStats, LatencyStats, PercentileEstimator, ProviderModelStats, StatsStore,
};
pub use upstream::{DiscoveredModel, Upstream};

#[cfg(feature = "account")]
pub use account::{AccountId, AccountRuntime, AccountStatus, AccountStore};
