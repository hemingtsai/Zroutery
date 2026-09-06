//! Machine learning infrastructure for adaptive routing.
//!
//! Provides the fixed-size feature vector that ML models consume for
//! scoring and ranking routing candidates.

pub mod features;
pub use features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
    UNKNOWN,
};
