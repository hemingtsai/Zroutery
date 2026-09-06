//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates, and the training dataset
//! that collects samples for model training.

pub mod dataset;
pub mod features;
pub use dataset::{
    DatasetStore, SampleBuilder, Targets, TrainingSample as DatasetTrainingSample, validate_sample,
};
pub use features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
    UNKNOWN,
};
