//! Core model abstraction and concrete models for ML routing.
//!
//! Provides the [`RoutingModel`] trait that all routing models implement,
//! along with concrete models:
//!
//! - [`SuccessModel`]: binary classifier for request success prediction (FTRL).
//! - [`LatencyModel`]: online linear regression for total latency.
//! - [`TtftModel`]: online linear regression for time-to-first-token.
//! - [`CostModel`]: online linear regression for cost prediction.

use serde::{Deserialize, Serialize};

use super::features::RoutingFeatures;

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

    pub fn compute_checksum(params: &[f64]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for p in params {
            p.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    pub fn verify_checksum(&self) -> bool {
        Self::compute_checksum(&self.parameters) == self.checksum
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

// ---------------------------------------------------------------------------
// SuccessModel — Logistic regression with FTRL
// ---------------------------------------------------------------------------

/// Binary classifier for request success prediction.
/// Uses online logistic regression with FTRL (Follow The Regularized Leader).
pub struct SuccessModel {
    /// Feature weights.
    weights: Vec<f64>,
    /// Bias term.
    bias: f64,
    /// Per-feature squared gradient accumulators (for AdaGrad-style learning rate).
    grad_sq: Vec<f64>,
    /// Learning rate.
    lr: f64,
    /// Total samples seen.
    samples: u64,
}

impl SuccessModel {
    pub fn new(dimension: usize) -> Self {
        SuccessModel {
            weights: vec![0.0; dimension],
            bias: 0.0,
            grad_sq: vec![1.0; dimension], // initialized to 1 to avoid division by zero
            lr: 0.05,
            samples: 0,
        }
    }

    fn sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x).exp())
    }

    fn raw_predict(&self, features: &[f32]) -> f64 {
        let mut z = self.bias;
        for (w, f) in self.weights.iter().zip(features.iter()) {
            z += w * (*f as f64);
        }
        Self::sigmoid(z)
    }
}

impl RoutingModel for SuccessModel {
    fn name(&self) -> &str {
        "success"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        let p = self.raw_predict(&features.values);
        let confidence = if self.samples < 10 {
            0.1
        } else {
            (p * (1.0 - p) * 4.0).min(1.0)
        };
        Prediction::clamped(p, confidence, self.samples)
    }

    fn update(&mut self, features: &RoutingFeatures, target: f64) {
        self.samples += 1;
        let p = self.raw_predict(&features.values);
        let error = target - p; // target is 0.0 or 1.0

        // FTRL-style update with per-feature adaptive learning rate
        for i in 0..self.weights.len().min(features.values.len()) {
            let g = error * features.values[i] as f64;
            self.grad_sq[i] += g * g;
            self.weights[i] += self.lr * g / self.grad_sq[i].sqrt();
        }
        self.bias += self.lr * error;
    }

    fn sample_count(&self) -> u64 {
        self.samples
    }

    fn save(&self) -> ModelState {
        let mut params = vec![self.bias];
        params.extend_from_slice(&self.weights);
        params.extend_from_slice(&self.grad_sq);
        let mut state = ModelState::new("success_logistic_ftrl", params);
        state.update_count = self.samples;
        state
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        if state.algorithm != "success_logistic_ftrl" {
            return Err(format!(
                "expected success_logistic_ftrl, got {}",
                state.algorithm
            ));
        }
        if state.parameters.len() < 3 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = (state.parameters.len() - 1) / 2;
        let bias = state.parameters[0];
        let weights = state.parameters[1..=n].to_vec();
        let grad_sq = state.parameters[n + 1..].to_vec();
        Ok(SuccessModel {
            weights,
            bias,
            grad_sq,
            lr: 0.05,
            samples: state.update_count,
        })
    }

    fn reset(&mut self) {
        self.weights.fill(0.0);
        self.bias = 0.0;
        self.grad_sq.fill(1.0);
        self.samples = 0;
    }
}

// ---------------------------------------------------------------------------
// LatencyModel — Linear regression for total latency
// ---------------------------------------------------------------------------

/// Predicts expected total latency in ms.
/// Uses online linear regression with EWMA for the residual.
pub struct LatencyModel {
    weights: Vec<f64>,
    bias: f64,
    /// EWMA of absolute residuals for confidence estimation.
    residual_ewma: f64,
    lr: f64,
    samples: u64,
}

impl LatencyModel {
    pub fn new(dimension: usize) -> Self {
        LatencyModel {
            weights: vec![0.0; dimension],
            bias: 500.0, // default 500ms estimate
            residual_ewma: 500.0,
            lr: 0.01,
            samples: 0,
        }
    }
}

impl RoutingModel for LatencyModel {
    fn name(&self) -> &str {
        "latency"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        let mut z = self.bias;
        for (w, f) in self.weights.iter().zip(features.values.iter()) {
            z += w * (*f as f64);
        }
        let value = z.max(0.0); // latency can't be negative
        let confidence = if self.samples < 20 {
            0.1
        } else {
            (1.0 - (self.residual_ewma / value.max(1.0))).clamp(0.1, 0.99)
        };
        Prediction::trained(value, confidence, self.samples)
    }

    fn update(&mut self, features: &RoutingFeatures, target: f64) {
        self.samples += 1;
        let mut predicted = self.bias;
        for (w, f) in self.weights.iter().zip(features.values.iter()) {
            predicted += w * (*f as f64);
        }
        let error = target - predicted;
        self.residual_ewma = 0.3 * error.abs() + 0.7 * self.residual_ewma;
        for i in 0..self.weights.len().min(features.values.len()) {
            self.weights[i] += self.lr * error * features.values[i] as f64;
        }
        self.bias += self.lr * error;
    }

    fn sample_count(&self) -> u64 {
        self.samples
    }

    fn save(&self) -> ModelState {
        let mut params = vec![self.bias, self.residual_ewma];
        params.extend_from_slice(&self.weights);
        let mut state = ModelState::new("latency_linear", params);
        state.update_count = self.samples;
        state
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        if state.algorithm != "latency_linear" {
            return Err("wrong algorithm".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let bias = state.parameters[0];
        let residual_ewma = state.parameters[1];
        let weights = state.parameters[2..].to_vec();
        Ok(LatencyModel {
            weights,
            bias,
            residual_ewma,
            lr: 0.01,
            samples: state.update_count,
        })
    }

    fn reset(&mut self) {
        self.weights.fill(0.0);
        self.bias = 500.0;
        self.residual_ewma = 500.0;
        self.samples = 0;
    }
}

// ---------------------------------------------------------------------------
// TtftModel — Linear regression for time-to-first-token
// ---------------------------------------------------------------------------

/// Predicts expected TTFT in ms.
/// Uses online linear regression with EWMA for the residual.
pub struct TtftModel {
    weights: Vec<f64>,
    bias: f64,
    residual_ewma: f64,
    lr: f64,
    samples: u64,
}

impl TtftModel {
    pub fn new(dimension: usize) -> Self {
        TtftModel {
            weights: vec![0.0; dimension],
            bias: 200.0,
            residual_ewma: 200.0,
            lr: 0.01,
            samples: 0,
        }
    }
}

impl RoutingModel for TtftModel {
    fn name(&self) -> &str {
        "ttft"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        let mut z = self.bias;
        for (w, f) in self.weights.iter().zip(features.values.iter()) {
            z += w * (*f as f64);
        }
        let value = z.max(0.0); // ttft can't be negative
        let confidence = if self.samples < 20 {
            0.1
        } else {
            (1.0 - (self.residual_ewma / value.max(1.0))).clamp(0.1, 0.99)
        };
        Prediction::trained(value, confidence, self.samples)
    }

    fn update(&mut self, features: &RoutingFeatures, target: f64) {
        self.samples += 1;
        let mut predicted = self.bias;
        for (w, f) in self.weights.iter().zip(features.values.iter()) {
            predicted += w * (*f as f64);
        }
        let error = target - predicted;
        self.residual_ewma = 0.3 * error.abs() + 0.7 * self.residual_ewma;
        for i in 0..self.weights.len().min(features.values.len()) {
            self.weights[i] += self.lr * error * features.values[i] as f64;
        }
        self.bias += self.lr * error;
    }

    fn sample_count(&self) -> u64 {
        self.samples
    }

    fn save(&self) -> ModelState {
        let mut params = vec![self.bias, self.residual_ewma];
        params.extend_from_slice(&self.weights);
        let mut state = ModelState::new("ttft_linear", params);
        state.update_count = self.samples;
        state
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        if state.algorithm != "ttft_linear" {
            return Err("wrong algorithm".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let bias = state.parameters[0];
        let residual_ewma = state.parameters[1];
        let weights = state.parameters[2..].to_vec();
        Ok(TtftModel {
            weights,
            bias,
            residual_ewma,
            lr: 0.01,
            samples: state.update_count,
        })
    }

    fn reset(&mut self) {
        self.weights.fill(0.0);
        self.bias = 200.0;
        self.residual_ewma = 200.0;
        self.samples = 0;
    }
}

// ---------------------------------------------------------------------------
// CostModel — Linear regression for cost
// ---------------------------------------------------------------------------

/// Predicts expected cost.
/// Uses online linear regression with EWMA for the residual.
pub struct CostModel {
    weights: Vec<f64>,
    bias: f64,
    residual_ewma: f64,
    lr: f64,
    samples: u64,
}

impl CostModel {
    pub fn new(dimension: usize) -> Self {
        CostModel {
            weights: vec![0.0; dimension],
            bias: 0.01,
            residual_ewma: 0.01,
            lr: 0.01,
            samples: 0,
        }
    }
}

impl RoutingModel for CostModel {
    fn name(&self) -> &str {
        "cost"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        let mut z = self.bias;
        for (w, f) in self.weights.iter().zip(features.values.iter()) {
            z += w * (*f as f64);
        }
        let value = z.max(0.0); // cost can't be negative
        let confidence = if self.samples < 20 {
            0.1
        } else {
            (1.0 - (self.residual_ewma / value.max(0.001))).clamp(0.1, 0.99)
        };
        Prediction::trained(value, confidence, self.samples)
    }

    fn update(&mut self, features: &RoutingFeatures, target: f64) {
        self.samples += 1;
        let mut predicted = self.bias;
        for (w, f) in self.weights.iter().zip(features.values.iter()) {
            predicted += w * (*f as f64);
        }
        let error = target - predicted;
        self.residual_ewma = 0.3 * error.abs() + 0.7 * self.residual_ewma;
        for i in 0..self.weights.len().min(features.values.len()) {
            self.weights[i] += self.lr * error * features.values[i] as f64;
        }
        self.bias += self.lr * error;
    }

    fn sample_count(&self) -> u64 {
        self.samples
    }

    fn save(&self) -> ModelState {
        let mut params = vec![self.bias, self.residual_ewma];
        params.extend_from_slice(&self.weights);
        let mut state = ModelState::new("cost_linear", params);
        state.update_count = self.samples;
        state
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        if state.algorithm != "cost_linear" {
            return Err("wrong algorithm".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let bias = state.parameters[0];
        let residual_ewma = state.parameters[1];
        let weights = state.parameters[2..].to_vec();
        Ok(CostModel {
            weights,
            bias,
            residual_ewma,
            lr: 0.01,
            samples: state.update_count,
        })
    }

    fn reset(&mut self) {
        self.weights.fill(0.0);
        self.bias = 0.01;
        self.residual_ewma = 0.01;
        self.samples = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION};

    // -- helpers --

    fn zero_features() -> RoutingFeatures {
        RoutingFeatures {
            values: [0.0; FEATURE_DIMENSION],
            schema_version: 1,
        }
    }

    fn random_like_features(seed: u64) -> RoutingFeatures {
        let mut f = RoutingFeatures::default();
        for i in 0..FEATURE_DIMENSION {
            // Simple deterministic pseudo-random in [0, 1]
            f.values[i] = ((seed.wrapping_mul(i as u64 + 1) % 1000) as f32) / 1000.0;
        }
        f
    }

    // -- 1. SuccessModel cold start -> Prediction.cold is false (it uses clamped) --
    // Actually: predict with 0 samples -> samples < 10, so confidence = 0.1.
    // The SuccessModel doesn't produce "cold" predictions because it uses clamped,
    // but we verify it starts with low confidence.

    #[test]
    fn success_model_cold_start_low_confidence() {
        let model = SuccessModel::new(FEATURE_DIMENSION);
        let features = zero_features();
        let pred = model.predict(&features);
        // With 0 samples, confidence should be 0.1
        assert_eq!(pred.confidence, 0.1);
        assert_eq!(pred.sample_count, 0);
        // Prediction::clamped sets cold=false, but sample_count=0 indicates cold start
        assert!(!pred.cold);
    }

    // -- 2. SuccessModel sigmoid correctness --

    #[test]
    fn success_model_sigmoid_correctness() {
        // sigmoid(0) = 0.5
        assert!((SuccessModel::sigmoid(0.0) - 0.5).abs() < 1e-10);
        // sigmoid(large positive) -> ~1.0
        assert!((SuccessModel::sigmoid(100.0) - 1.0).abs() < 1e-10);
        // sigmoid(large negative) -> ~0.0
        assert!(SuccessModel::sigmoid(-100.0) < 1e-10);
        // sigmoid is monotonically increasing
        assert!(SuccessModel::sigmoid(1.0) > SuccessModel::sigmoid(0.0));
        assert!(SuccessModel::sigmoid(0.0) > SuccessModel::sigmoid(-1.0));
    }

    // -- 3. SuccessModel update moves prediction toward target --

    #[test]
    fn success_model_update_moves_toward_target() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            // Set some features to non-zero so weights can take effect
            for i in 0..FEATURE_DIMENSION {
                f.values[i] = 0.5;
            }
            f
        };

        let before = model.predict(&features).value;

        // Train toward success (target=1.0) multiple times
        for _ in 0..20 {
            model.update(&features, 1.0);
        }

        let after = model.predict(&features).value;
        assert!(
            after > before,
            "prediction should increase toward 1.0: before={before}, after={after}"
        );
    }

    // -- 4. SuccessModel 100 updates -> prediction converges --

    #[test]
    fn success_model_convergence() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        // Train toward success 100 times
        for _ in 0..100 {
            model.update(&features, 1.0);
        }

        let pred = model.predict(&features);
        assert!(
            pred.value > 0.7,
            "after 100 success samples, prediction should be >0.7, got {}",
            pred.value
        );
        assert_eq!(pred.sample_count, 100);
    }

    // -- 5. SuccessModel prediction always in [0, 1] --

    #[test]
    fn success_model_prediction_always_in_range() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);

        // Train with extreme features
        for i in 0..200 {
            let features = random_like_features(i);
            let target = if i % 2 == 0 { 1.0 } else { 0.0 };
            model.update(&features, target);
            let pred = model.predict(&features);
            assert!(
                pred.value >= 0.0 && pred.value <= 1.0,
                "prediction out of [0,1]: {}",
                pred.value
            );
            assert!(
                pred.confidence >= 0.0 && pred.confidence <= 1.0,
                "confidence out of [0,1]: {}",
                pred.confidence
            );
        }
    }

    // -- 6. LatencyModel prediction always >= 0 --

    #[test]
    fn latency_model_prediction_always_nonnegative() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);

        for i in 0..200 {
            let features = random_like_features(i);
            model.update(&features, 500.0 + (i as f64));
            let pred = model.predict(&features);
            assert!(
                pred.value >= 0.0,
                "latency prediction must be >= 0, got {}",
                pred.value
            );
        }
    }

    // -- 7. LatencyModel update corrects toward target --

    #[test]
    fn latency_model_update_corrects_toward_target() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let before = model.predict(&features).value; // should be ~500

        // Train toward low latency (100ms)
        for _ in 0..100 {
            model.update(&features, 100.0);
        }

        let after = model.predict(&features).value;
        assert!(
            after < before,
            "latency should decrease toward 100ms: before={before}, after={after}"
        );
    }

    // -- 8. TtftModel cold start bias --

    #[test]
    fn ttft_model_cold_start_bias() {
        let model = TtftModel::new(FEATURE_DIMENSION);
        let features = zero_features();
        let pred = model.predict(&features);
        // Default bias is 200ms
        assert!(
            (pred.value - 200.0).abs() < 1.0,
            "expected ~200ms, got {}",
            pred.value
        );
        assert_eq!(pred.sample_count, 0);
    }

    // -- 9. CostModel cold start bias --

    #[test]
    fn cost_model_cold_start_bias() {
        let model = CostModel::new(FEATURE_DIMENSION);
        let features = zero_features();
        let pred = model.predict(&features);
        // Default bias is 0.01
        assert!(
            (pred.value - 0.01).abs() < 0.001,
            "expected ~0.01, got {}",
            pred.value
        );
        assert_eq!(pred.sample_count, 0);
    }

    // -- 10. ModelState checksum computation --

    #[test]
    fn model_state_checksum_computation() {
        let params = vec![1.0, 2.0, 3.0];
        let checksum = ModelState::compute_checksum(&params);
        // Checksum should be deterministic
        assert_eq!(checksum, ModelState::compute_checksum(&params));
        // Different params should produce different checksums
        let params2 = vec![1.0, 2.0, 4.0];
        assert_ne!(checksum, ModelState::compute_checksum(&params2));
    }

    // -- 11. ModelState checksum verification --

    #[test]
    fn model_state_checksum_verification() {
        let state = ModelState::new("test", vec![1.0, 2.0, 3.0]);
        assert!(state.verify_checksum());

        // Corrupt the state
        let mut corrupted = state.clone();
        corrupted.parameters[1] = 999.0;
        assert!(!corrupted.verify_checksum());
    }

    // -- 12. SuccessModel save/load round-trip --

    #[test]
    fn success_model_save_load_round_trip() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = random_like_features(42);

        // Train some samples
        for i in 0..50 {
            model.update(&features, if i % 3 == 0 { 1.0 } else { 0.0 });
        }

        let state = model.save();
        assert_eq!(state.algorithm, "success_logistic_ftrl");
        assert_eq!(state.update_count, 50);

        let loaded = SuccessModel::load(&state).unwrap();
        assert_eq!(loaded.sample_count(), 50);

        // Predictions should match
        let pred_orig = model.predict(&features);
        let pred_loaded = loaded.predict(&features);
        assert!(
            (pred_orig.value - pred_loaded.value).abs() < 1e-10,
            "predictions differ: orig={}, loaded={}",
            pred_orig.value,
            pred_loaded.value
        );
    }

    // -- 13. LatencyModel save/load round-trip --

    #[test]
    fn latency_model_save_load_round_trip() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = random_like_features(42);

        for i in 0..50 {
            model.update(&features, 200.0 + i as f64);
        }

        let state = model.save();
        assert_eq!(state.algorithm, "latency_linear");

        let loaded = LatencyModel::load(&state).unwrap();
        assert_eq!(loaded.sample_count(), 50);

        let pred_orig = model.predict(&features);
        let pred_loaded = loaded.predict(&features);
        assert!(
            (pred_orig.value - pred_loaded.value).abs() < 1e-10,
            "predictions differ: orig={}, loaded={}",
            pred_orig.value,
            pred_loaded.value
        );
    }

    // -- 14. Load with wrong algorithm -> error --

    #[test]
    fn load_wrong_algorithm_errors() {
        let state = ModelState::new("wrong_algo", vec![0.0, 1.0, 2.0]);
        assert!(SuccessModel::load(&state).is_err());
        assert!(LatencyModel::load(&state).is_err());
        assert!(TtftModel::load(&state).is_err());
        assert!(CostModel::load(&state).is_err());
    }

    // -- 15. Load with corrupted checksum -> error --

    #[test]
    fn load_corrupted_checksum_errors() {
        let mut state = ModelState::new("success_logistic_ftrl", vec![0.0, 1.0, 2.0]);
        state.checksum = 999; // corrupt
        assert!(SuccessModel::load(&state).is_err());
    }

    // -- 16. Same state -> same prediction (determinism) --

    #[test]
    fn same_state_same_prediction() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = random_like_features(77);
        for i in 0..30 {
            model.update(&features, if i % 2 == 0 { 1.0 } else { 0.0 });
        }

        let state = model.save();
        let loaded1 = SuccessModel::load(&state).unwrap();
        let loaded2 = SuccessModel::load(&state).unwrap();

        let p1 = loaded1.predict(&features);
        let p2 = loaded2.predict(&features);
        assert!(
            (p1.value - p2.value).abs() < 1e-15,
            "same state must produce same prediction"
        );
    }

    // -- 17. NaN/Inf cannot appear in predictions --

    #[test]
    fn no_nan_or_inf_in_predictions() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for (i, v) in f.values.iter_mut().enumerate() {
                *v = (i as f32) / (FEATURE_DIMENSION as f32);
            }
            f
        };

        // Train with extreme values
        for i in 0..500 {
            model.update(&features, if i % 2 == 0 { 1.0 } else { 0.0 });
            let pred = model.predict(&features);
            assert!(pred.value.is_finite(), "value is not finite: {}", pred.value);
            assert!(
                pred.confidence.is_finite(),
                "confidence is not finite: {}",
                pred.confidence
            );
        }

        // Also test latency model
        let mut latency = LatencyModel::new(FEATURE_DIMENSION);
        for i in 0..500 {
            latency.update(&features, 100.0 + i as f64);
            let pred = latency.predict(&features);
            assert!(pred.value.is_finite(), "latency value not finite: {}", pred.value);
            assert!(
                pred.confidence.is_finite(),
                "latency confidence not finite: {}",
                pred.confidence
            );
        }
    }

    // -- 18. Reset returns to cold start --

    #[test]
    fn reset_returns_to_cold_start() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = random_like_features(42);

        // Train
        for _ in 0..100 {
            model.update(&features, 1.0);
        }
        assert_eq!(model.sample_count(), 100);

        let cold_pred = {
            let m = SuccessModel::new(FEATURE_DIMENSION);
            m.predict(&features)
        };

        model.reset();
        assert_eq!(model.sample_count(), 0);

        let after_reset = model.predict(&features);
        assert!(
            (after_reset.value - cold_pred.value).abs() < 1e-10,
            "after reset should match fresh model: reset={}, fresh={}",
            after_reset.value,
            cold_pred.value
        );
    }

    // -- 19. Prediction serde round-trip --

    #[test]
    fn prediction_serde_round_trip() {
        let pred = Prediction {
            value: 0.85,
            confidence: 0.72,
            sample_count: 42,
            cold: false,
        };
        let json = serde_json::to_string(&pred).unwrap();
        let restored: Prediction = serde_json::from_str(&json).unwrap();
        assert!((restored.value - pred.value).abs() < 1e-10);
        assert!((restored.confidence - pred.confidence).abs() < 1e-10);
        assert_eq!(restored.sample_count, pred.sample_count);
        assert_eq!(restored.cold, pred.cold);
    }

    // -- 20. Training: 1000 samples -> prediction moves in correct direction --

    #[test]
    fn training_1000_samples_correct_direction() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        // Train toward failure (target=0.0)
        for _ in 0..1000 {
            model.update(&features, 0.0);
        }
        let pred_failure = model.predict(&features);

        // Reset and train toward success (target=1.0)
        model.reset();
        for _ in 0..1000 {
            model.update(&features, 1.0);
        }
        let pred_success = model.predict(&features);

        assert!(
            pred_success.value > pred_failure.value,
            "success training should yield higher prediction: success={}, failure={}",
            pred_success.value,
            pred_failure.value
        );
        assert!(
            pred_success.value > 0.8,
            "1000 success samples should push prediction >0.8, got {}",
            pred_success.value
        );
        assert!(
            pred_failure.value < 0.2,
            "1000 failure samples should push prediction <0.2, got {}",
            pred_failure.value
        );
    }

    // -- Additional: TtftModel and CostModel save/load round-trips --

    #[test]
    fn ttft_model_save_load_round_trip() {
        let mut model = TtftModel::new(FEATURE_DIMENSION);
        let features = random_like_features(42);
        for i in 0..50 {
            model.update(&features, 100.0 + i as f64);
        }

        let state = model.save();
        assert_eq!(state.algorithm, "ttft_linear");

        let loaded = TtftModel::load(&state).unwrap();
        assert_eq!(loaded.sample_count(), 50);

        let p1 = model.predict(&features);
        let p2 = loaded.predict(&features);
        assert!((p1.value - p2.value).abs() < 1e-10);
    }

    #[test]
    fn cost_model_save_load_round_trip() {
        let mut model = CostModel::new(FEATURE_DIMENSION);
        let features = random_like_features(42);
        for i in 0..50 {
            model.update(&features, 0.01 + (i as f64) * 0.001);
        }

        let state = model.save();
        assert_eq!(state.algorithm, "cost_linear");

        let loaded = CostModel::load(&state).unwrap();
        assert_eq!(loaded.sample_count(), 50);

        let p1 = model.predict(&features);
        let p2 = loaded.predict(&features);
        assert!((p1.value - p2.value).abs() < 1e-10);
    }

    // -- Additional: Prediction::cold constructor --

    #[test]
    fn prediction_cold_constructor() {
        let pred = Prediction::cold(0.5);
        assert_eq!(pred.value, 0.5);
        assert_eq!(pred.confidence, 0.1);
        assert_eq!(pred.sample_count, 0);
        assert!(pred.cold);
    }

    // -- Additional: Prediction::trained constructor --

    #[test]
    fn prediction_trained_constructor() {
        let pred = Prediction::trained(0.8, 0.9, 100);
        assert_eq!(pred.value, 0.8);
        assert_eq!(pred.confidence, 0.9);
        assert_eq!(pred.sample_count, 100);
        assert!(!pred.cold);
    }

    // -- Additional: model names --

    #[test]
    fn model_names() {
        assert_eq!(SuccessModel::new(4).name(), "success");
        assert_eq!(LatencyModel::new(4).name(), "latency");
        assert_eq!(TtftModel::new(4).name(), "ttft");
        assert_eq!(CostModel::new(4).name(), "cost");
    }
}
