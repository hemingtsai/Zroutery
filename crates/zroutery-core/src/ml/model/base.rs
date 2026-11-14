use serde::{Deserialize, Serialize};

use crate::ml::features::RoutingFeatures;

// ---------------------------------------------------------------------------
// Prediction — model output
// ---------------------------------------------------------------------------

/// A prediction from a routing model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    /// The predicted value (probability for classification, magnitude for regression).
    pub value: f64,
    /// Confidence in the prediction [0.0, 1.0].
    pub confidence: f64,
    /// Number of training samples the model has seen.
    pub sample_count: u64,
    /// Whether this is a cold-start default (no real training data).
    pub cold: bool,
}

impl Prediction {
    pub fn cold(value: f64) -> Self {
        Prediction {
            value,
            confidence: 0.1,
            sample_count: 0,
            cold: true,
        }
    }

    pub fn trained(value: f64, confidence: f64, sample_count: u64) -> Self {
        Prediction {
            value,
            confidence,
            sample_count,
            cold: false,
        }
    }

    /// Clamp value to [0, 1] for probability predictions.
    pub fn clamped(value: f64, confidence: f64, sample_count: u64) -> Self {
        Prediction {
            value: value.clamp(0.0, 1.0),
            confidence: confidence.clamp(0.0, 1.0),
            sample_count,
            cold: false,
        }
    }
}

// ---------------------------------------------------------------------------
// ModelState — serializable model state for persistence
// ---------------------------------------------------------------------------

/// Serializable model state for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelState {
    /// Schema version for migration.
    pub schema_version: u32,
    /// Algorithm identifier.
    pub algorithm: String,
    /// Number of training updates applied.
    pub update_count: u64,
    /// Serialized model weights/parameters.
    pub parameters: Vec<f64>,
    /// Checksum of parameters for integrity verification.
    pub checksum: u64,
}

impl ModelState {
    pub fn new(algorithm: &str, parameters: Vec<f64>) -> Self {
        let checksum = Self::compute_checksum(&parameters);
        ModelState {
            schema_version: 1,
            algorithm: algorithm.to_string(),
            update_count: 0,
            parameters,
            checksum,
        }
    }

    /// FNV-1a checksum of parameter bits for cross-version stability.
    pub fn compute_checksum(params: &[f64]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
        for p in params {
            for b in p.to_bits().to_le_bytes() {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3); // FNV prime
            }
        }
        hash
    }

    pub fn verify_checksum(&self) -> bool {
        Self::compute_checksum(&self.parameters) == self.checksum
    }

    /// Validate schema version and that all parameters are finite (no NaN/Inf).
    pub fn validate_basics(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported schema version {} (expected 1)",
                self.schema_version
            ));
        }
        for (i, p) in self.parameters.iter().enumerate() {
            if !p.is_finite() {
                return Err(format!("parameter[{i}] is not finite: {p}"));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RoutingModel — the core trait
// ---------------------------------------------------------------------------

/// Core trait for all routing models.
pub trait RoutingModel: Send + Sync {
    /// Model name for diagnostics.
    fn name(&self) -> &str;

    /// Predict from features. Must never return NaN/Inf.
    fn predict(&self, features: &RoutingFeatures) -> Prediction;

    /// Update model with a single training sample.
    /// (features, target_value) — target is already extracted from TrainingSample.
    fn update(&mut self, features: &RoutingFeatures, target: f64);

    /// Number of training samples seen.
    fn sample_count(&self) -> u64;

    /// Serialize model state.
    fn save(&self) -> ModelState;

    /// Load model state. Returns error if incompatible.
    fn load(state: &ModelState) -> Result<Self, String>
    where
        Self: Sized;

    /// Reset to cold-start state.
    fn reset(&mut self);
}
