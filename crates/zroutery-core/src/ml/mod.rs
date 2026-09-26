//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates, and the training dataset
//! that collects samples for model training.

pub mod coordinator;
pub mod dataset;
pub mod decision_contract;
pub mod decision_engine;
pub mod evaluation;
pub mod features;
pub mod model;
pub mod model_identity;
pub mod reward;
pub mod shadow;
pub use coordinator::{Coordinator, CoordinatorConfig, RoutingAction, RoutingDecision};
pub use dataset::{
    canonical_samples_from_outcome, outcome_to_dataset_sample, sample_from_outcome, samples_from_outcome,
    try_outcome_sample, try_samples_from_outcome, try_samples_from_outcome_with_feedback,
    validate_outcome_sample, CanonicalTrainingSample, DatasetStore, OutcomeDatasetSample,
    OutcomeTrainingSample, SampleBuilder, SampleScope, Targets, TrainingSample as DatasetTrainingSample,
    validate_sample,
};
pub use decision_contract::{
    CandidateEligibility, CandidatePredictions, CandidateRoles, CandidateScore,
    DecisionCandidate, DecisionContractError, DecisionDimension, DecisionDistribution,
    DecisionIdentities, DecisionModel, DecisionModelStates, DecisionPhase, DecisionState,
    ModelInput, OpenStep, SettledStep, DISTRIBUTION_NORMALIZATION_TOLERANCE,
};
pub use decision_engine::{DecisionEngine, EngineCandidate, EngineInput, EngineOutput};
pub use evaluation::{
    ComparisonReport, Evaluator, FrozenHoldout, PredictionMetrics, Recommendation, RoutingDeltas,
    RoutingMetrics, temporal_split,
};
pub use features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
    UNKNOWN,
};
pub use model::{
    CostModel, LatencyModel, ModelState, Prediction, RoutingModel, SuccessModel, TtftModel,
};
pub use model_identity::{
    CommitId, CommitInfo, LearningEvent, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId,
    ModelRef, ModelStore, ReplayEngine, ReplayError,
};
pub use reward::{
    Action, ActionGuard, AttemptReward, PredictionBundle, RequestReward, RewardComputer,
    RewardPolicy, UtilityBreakdown, compute_utility,
};
pub use shadow::{
    EnsemblePredictor, ModelEnsemblePredictor, ProductionDecisionRef, ShadowCandidate,
    ShadowCandidateInput, ShadowDecision, ShadowEngine, ShadowInput, ShadowObservation, ShadowScope,
    ShadowStore, ShadowVerdict,
};
