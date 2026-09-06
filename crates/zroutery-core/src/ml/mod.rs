//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates, and the training dataset
//! that collects samples for model training.

pub mod dataset;
pub mod evaluation;
pub mod features;
pub mod model;
pub use dataset::{
    DatasetStore, SampleBuilder, Targets, TrainingSample as DatasetTrainingSample, validate_sample,
};
pub use evaluation::{
    ComparisonReport, Evaluator, PredictionMetrics, Recommendation, RoutingDeltas, RoutingMetrics,
};
pub use features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
    UNKNOWN,
};
pub use model::{
    CostModel, LatencyModel, ModelState, Prediction, RoutingModel, SuccessModel, TtftModel,
};
