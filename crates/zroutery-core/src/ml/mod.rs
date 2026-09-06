//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates, and the training dataset
//! that collects samples for model training.

pub mod coordinator;
pub mod dataset;
pub mod evaluation;
pub mod features;
pub mod model;
pub mod reward;
pub use coordinator::{Coordinator, CoordinatorConfig, RoutingAction, RoutingDecision};
pub use dataset::{
    DatasetStore, SampleBuilder, Targets, TrainingSample as DatasetTrainingSample, validate_sample,
};
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
pub use reward::{
    Action, ActionGuard, AttemptReward, PredictionBundle, RequestReward, RewardComputer,
    RewardPolicy, UtilityBreakdown, compute_utility,
};
