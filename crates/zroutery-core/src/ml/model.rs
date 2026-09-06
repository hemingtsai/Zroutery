//! Core model abstraction and concrete models for ML routing.
//!
//! Provides the [`RoutingModel`] trait that all routing models implement,
//! along with concrete models:
//!
//! - [`SuccessModel`]: binary classifier for request success prediction (AdaGrad).
//! - [`LatencyModel`]: online linear regression for total latency (SGD + EWMA).
//! - [`TtftModel`]: online linear regression for time-to-first-token (SGD + EWMA).
//! - [`CostModel`]: online linear regression for cost prediction (SGD + EWMA).

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

// ---------------------------------------------------------------------------
// TrainingResult / train_batch — batch training runner
// ---------------------------------------------------------------------------

/// Result of a batch training run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingResult {
    /// Name of the model that was trained.
    pub model_name: String,
    /// Number of samples in the batch.
    pub samples_trained: u64,
    /// Wall-clock duration of the training run in milliseconds.
    pub duration_ms: u64,
    /// Serializable model state after training.
    pub final_state: ModelState,
}

/// Train a model on a batch of (features, target) samples.
///
/// Applies each sample sequentially via [`RoutingModel::update`] and returns
/// a [`TrainingResult`] with metadata and the post-training state.
pub fn train_batch(
    model: &mut dyn RoutingModel,
    samples: &[(RoutingFeatures, f64)],
) -> TrainingResult {
    let start = std::time::Instant::now();
    for (features, target) in samples {
        model.update(features, *target);
    }
    TrainingResult {
        model_name: model.name().to_string(),
        samples_trained: samples.len() as u64,
        duration_ms: start.elapsed().as_millis() as u64,
        final_state: model.save(),
    }
}

// ---------------------------------------------------------------------------
// SuccessModel — Logistic regression with AdaGrad
// ---------------------------------------------------------------------------

/// Binary classifier for request success prediction.
/// Uses online logistic regression with per-feature adaptive learning rate (AdaGrad).
#[derive(Debug)]
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
        if !target.is_finite() {
            return; // silently skip NaN/Inf targets
        }
        let target = target.clamp(0.0, 1.0);
        self.samples += 1;
        let p = self.raw_predict(&features.values);
        let error = target - p; // target is 0.0 or 1.0

        // AdaGrad-style update with per-feature adaptive learning rate
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
        let mut state = ModelState::new("success_logistic_adagrad", params);
        state.update_count = self.samples;
        state
    }

    fn load(state: &ModelState) -> Result<Self, String> {
        state.validate_basics()?;
        if state.algorithm != "success_logistic_adagrad" {
            return Err(format!(
                "expected success_logistic_adagrad, got {}",
                state.algorithm
            ));
        }
        // Expected: 1 bias + n weights + n grad_sq = 2n+1 (minimum 3)
        if state.parameters.len() < 3 {
            return Err("too few parameters".into());
        }
        if state.parameters.len() % 2 == 0 {
            return Err(format!(
                "parameter count must be odd (2n+1), got {}",
                state.parameters.len()
            ));
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = (state.parameters.len() - 1) / 2;
        if n != super::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                super::features::FEATURE_DIMENSION,
                n
            ));
        }
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
        if !target.is_finite() {
            return; // silently skip NaN/Inf targets
        }
        if target < 0.0 {
            return; // reject negative targets
        }
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
        state.validate_basics()?;
        if state.algorithm != "latency_linear" {
            return Err(format!(
                "expected latency_linear, got {}",
                state.algorithm
            ));
        }
        // Expected: 2 scalars (bias, residual_ewma) + n weights = n+2 (minimum 2)
        if state.parameters.len() < 2 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = state.parameters.len() - 2; // bias + residual_ewma + weights
        if n != super::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                super::features::FEATURE_DIMENSION,
                n
            ));
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
        if !target.is_finite() {
            return; // silently skip NaN/Inf targets
        }
        if target < 0.0 {
            return; // reject negative targets
        }
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
        state.validate_basics()?;
        if state.algorithm != "ttft_linear" {
            return Err(format!(
                "expected ttft_linear, got {}",
                state.algorithm
            ));
        }
        // Expected: 2 scalars (bias, residual_ewma) + n weights = n+2 (minimum 2)
        if state.parameters.len() < 2 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = state.parameters.len() - 2; // bias + residual_ewma + weights
        if n != super::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                super::features::FEATURE_DIMENSION,
                n
            ));
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
        if !target.is_finite() {
            return; // silently skip NaN/Inf targets
        }
        if target < 0.0 {
            return; // reject negative targets
        }
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
        state.validate_basics()?;
        if state.algorithm != "cost_linear" {
            return Err(format!(
                "expected cost_linear, got {}",
                state.algorithm
            ));
        }
        // Expected: 2 scalars (bias, residual_ewma) + n weights = n+2 (minimum 2)
        if state.parameters.len() < 2 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = state.parameters.len() - 2; // bias + residual_ewma + weights
        if n != super::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                super::features::FEATURE_DIMENSION,
                n
            ));
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
        assert_eq!(state.algorithm, "success_logistic_adagrad");
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
        let mut state = ModelState::new("success_logistic_adagrad", vec![0.0, 1.0, 2.0]);
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

    // -- T7C-H02: ModelState hardening tests --

    #[test]
    fn load_rejects_wrong_schema_version() {
        let mut state = ModelState::new("success_logistic_adagrad", vec![0.0, 1.0, 2.0]);
        state.schema_version = 2;
        let err = SuccessModel::load(&state).err().unwrap();
        assert!(err.contains("schema version"), "unexpected error: {err}");
    }

    #[test]
    fn load_rejects_nan_parameters() {
        let params = vec![0.0, f64::NAN, 2.0];
        let mut state = ModelState::new("success_logistic_adagrad", params);
        state.checksum = ModelState::compute_checksum(&state.parameters); // fix checksum for NAN bits
        let err = SuccessModel::load(&state).err().unwrap();
        assert!(err.contains("not finite"), "unexpected error: {err}");
    }

    #[test]
    fn load_rejects_inf_parameters() {
        let params = vec![0.0, f64::INFINITY, 2.0];
        let state = ModelState::new("success_logistic_adagrad", params);
        let err = SuccessModel::load(&state).err().unwrap();
        assert!(err.contains("not finite"), "unexpected error: {err}");
    }

    #[test]
    fn load_rejects_wrong_parameter_count_success() {
        // SuccessModel expects odd count (2n+1), give even
        let state = ModelState::new("success_logistic_adagrad", vec![0.0, 1.0, 2.0, 3.0]);
        let err = SuccessModel::load(&state).err().unwrap();
        assert!(err.contains("odd"), "unexpected error: {err}");
    }

    // -- T7C-H03: FNV-1a checksum stability test --

    #[test]
    fn fnv1a_checksum_known_vector() {
        // Verify our FNV-1a produces a deterministic value for a known input.
        let params = vec![1.0f64, 2.0, 3.0];
        let h1 = ModelState::compute_checksum(&params);
        let h2 = ModelState::compute_checksum(&params);
        assert_eq!(h1, h2, "checksum must be deterministic");
        // Different params -> different hash
        let params2 = vec![1.0, 2.0, 3.1];
        assert_ne!(h1, ModelState::compute_checksum(&params2));
        // Single bit difference should change hash
        let params3 = vec![1.0, 2.0, f64::from_bits(3.0f64.to_bits() ^ 1)];
        assert_ne!(h1, ModelState::compute_checksum(&params3));
    }

    // -- T7C-H04: Cold/warm semantics test --
    // Train a model, save it, create a fresh instance, load the saved state,
    // and verify that actual learned behavior is preserved (not just serde round-trip).

    #[test]
    fn cold_warm_success_model_preserves_learned_behavior() {
        // 1. Create and train a model on a clear pattern
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let good_features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.8; // high feature values -> success
            }
            f
        };
        let bad_features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.2; // low feature values -> failure
            }
            f
        };

        // Train: high features -> success, low features -> failure
        for _ in 0..200 {
            model.update(&good_features, 1.0);
            model.update(&bad_features, 0.0);
        }

        // Record learned predictions from the live model
        let live_good = model.predict(&good_features).value;
        let live_bad = model.predict(&bad_features).value;
        assert!(live_good > 0.6, "model should predict high success for good features, got {live_good}");
        assert!(live_bad < 0.4, "model should predict low success for bad features, got {live_bad}");

        // 2. Save to ModelState (simulates persistence to disk/database)
        let state = model.save();

        // 3. Drop the original model entirely
        drop(model);

        // 4. Create a brand new (cold) model instance
        let cold_model = SuccessModel::new(FEATURE_DIMENSION);
        let cold_good = cold_model.predict(&good_features).value;
        let cold_bad = cold_model.predict(&bad_features).value;
        // Cold model should NOT have learned the pattern
        assert!(
            (cold_good - cold_bad).abs() < 0.1,
            "cold model predictions should be similar, got good={cold_good}, bad={cold_bad}"
        );

        // 5. Load from saved state (simulates restart from persisted state)
        let warm_model = SuccessModel::load(&state).unwrap();

        // 6. Verify warm model reproduces learned behavior exactly
        let warm_good = warm_model.predict(&good_features).value;
        let warm_bad = warm_model.predict(&bad_features).value;

        assert!(
            (warm_good - live_good).abs() < 1e-10,
            "warm model must match live predictions for good features: warm={warm_good}, live={live_good}"
        );
        assert!(
            (warm_bad - live_bad).abs() < 1e-10,
            "warm model must match live predictions for bad features: warm={warm_bad}, live={live_bad}"
        );

        // 7. Verify the pattern is actually preserved (discrimination)
        assert!(
            warm_good > warm_bad + 0.2,
            "warm model should still discriminate: good={warm_good}, bad={warm_bad}"
        );
        assert_eq!(warm_model.sample_count(), 400);
    }

    #[test]
    fn cold_warm_latency_model_preserves_learned_behavior() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        // Train toward 200ms latency
        for _ in 0..100 {
            model.update(&features, 200.0);
        }

        let live_pred = model.predict(&features).value;
        assert!(live_pred < 400.0, "should have learned lower latency, got {live_pred}");

        let state = model.save();
        drop(model);

        // Cold model starts at 500ms default
        let cold = LatencyModel::new(FEATURE_DIMENSION);
        let cold_pred = cold.predict(&features).value;
        assert!((cold_pred - 500.0).abs() < 1.0, "cold should be ~500ms, got {cold_pred}");

        // Warm model preserves learned behavior
        let warm = LatencyModel::load(&state).unwrap();
        let warm_pred = warm.predict(&features).value;
        assert!(
            (warm_pred - live_pred).abs() < 1e-10,
            "warm latency must match live: warm={warm_pred}, live={live_pred}"
        );
        assert_eq!(warm.sample_count(), 100);
    }

    #[test]
    fn cold_warm_cost_model_preserves_learned_behavior() {
        let mut model = CostModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        for _ in 0..100 {
            model.update(&features, 0.005);
        }

        let live_pred = model.predict(&features).value;
        let state = model.save();
        drop(model);

        let warm = CostModel::load(&state).unwrap();
        let warm_pred = warm.predict(&features).value;
        assert!(
            (warm_pred - live_pred).abs() < 1e-10,
            "warm cost must match live: warm={warm_pred}, live={live_pred}"
        );
        assert_eq!(warm.sample_count(), 100);
    }

    #[test]
    fn cold_warm_ttft_model_preserves_learned_behavior() {
        let mut model = TtftModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        for _ in 0..100 {
            model.update(&features, 80.0);
        }

        let live_pred = model.predict(&features).value;
        let state = model.save();
        drop(model);

        let warm = TtftModel::load(&state).unwrap();
        let warm_pred = warm.predict(&features).value;
        assert!(
            (warm_pred - live_pred).abs() < 1e-10,
            "warm ttft must match live: warm={warm_pred}, live={live_pred}"
        );
        assert_eq!(warm.sample_count(), 100);
    }

    // -- T7C-H05: train_batch / TrainingResult tests --

    #[test]
    fn train_batch_success_model() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let samples: Vec<_> = (0..50)
            .map(|_| (features.clone(), 1.0))
            .collect();

        let result = train_batch(&mut model, &samples);
        assert_eq!(result.model_name, "success");
        assert_eq!(result.samples_trained, 50);
        assert_eq!(result.final_state.algorithm, "success_logistic_adagrad");
        assert_eq!(result.final_state.update_count, 50);
        // Model should still be usable after train_batch
        assert_eq!(model.sample_count(), 50);
    }

    #[test]
    fn train_batch_zero_duration_for_small_batch() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = zero_features();
        let samples: Vec<_> = (0..10)
            .map(|_| (features.clone(), 200.0))
            .collect();

        let result = train_batch(&mut model, &samples);
        // duration_ms may be 0 for very fast batches; just verify the field exists
        assert_eq!(result.samples_trained, 10);
        assert_eq!(result.model_name, "latency");
    }

    #[test]
    fn train_batch_empty_samples() {
        let mut model = CostModel::new(FEATURE_DIMENSION);
        let samples: Vec<(RoutingFeatures, f64)> = Vec::new();

        let result = train_batch(&mut model, &samples);
        assert_eq!(result.samples_trained, 0);
        assert_eq!(model.sample_count(), 0);
    }

    #[test]
    fn training_result_serde_round_trip() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = zero_features();
        let samples: Vec<_> = (0..5).map(|_| (features.clone(), 1.0)).collect();

        let result = train_batch(&mut model, &samples);
        let json = serde_json::to_string(&result).unwrap();
        let restored: TrainingResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.model_name, result.model_name);
        assert_eq!(restored.samples_trained, result.samples_trained);
        assert_eq!(restored.duration_ms, result.duration_ms);
        assert_eq!(
            restored.final_state.algorithm,
            result.final_state.algorithm
        );
    }

    // -- T7C-H06: Learning direction tests --
    // Each test proves that after training with a known pattern,
    // predictions move in the expected direction.

    #[test]
    fn success_model_learning_direction() {
        // Train with target=1.0 -> prediction should exceed 0.5 after enough samples
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let samples: Vec<_> = (0..200)
            .map(|_| (features.clone(), 1.0))
            .collect();
        train_batch(&mut model, &samples);

        let pred = model.predict(&features);
        assert!(
            pred.value > 0.5,
            "SuccessModel: after 200 target=1.0 samples, prediction should be >0.5, got {}",
            pred.value
        );
    }

    #[test]
    fn latency_model_learning_direction() {
        // Train with 100ms -> prediction should be <300ms after 200 samples
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let samples: Vec<_> = (0..200)
            .map(|_| (features.clone(), 100.0))
            .collect();
        train_batch(&mut model, &samples);

        let pred = model.predict(&features);
        assert!(
            pred.value < 300.0,
            "LatencyModel: after 200 samples at 100ms, prediction should be <300ms, got {}",
            pred.value
        );
    }

    #[test]
    fn ttft_model_learning_direction() {
        // Train with 50ms -> prediction should be <150ms after 200 samples
        let mut model = TtftModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let samples: Vec<_> = (0..200)
            .map(|_| (features.clone(), 50.0))
            .collect();
        train_batch(&mut model, &samples);

        let pred = model.predict(&features);
        assert!(
            pred.value < 150.0,
            "TtftModel: after 200 samples at 50ms, prediction should be <150ms, got {}",
            pred.value
        );
    }

    #[test]
    fn cost_model_learning_direction() {
        // Train with 0.01 -> prediction should be <0.05 after 200 samples
        let mut model = CostModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let samples: Vec<_> = (0..200)
            .map(|_| (features.clone(), 0.01))
            .collect();
        train_batch(&mut model, &samples);

        let pred = model.predict(&features);
        assert!(
            pred.value < 0.05,
            "CostModel: after 200 samples at 0.01, prediction should be <0.05, got {}",
            pred.value
        );
    }

    // -- T7C-H07: Training benchmark --

    #[test]
    fn training_benchmark_100k() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        let samples: Vec<_> = (0..100_000)
            .map(|i| (features.clone(), if i % 2 == 0 { 1.0 } else { 0.0 }))
            .collect();

        let start = std::time::Instant::now();
        let result = train_batch(&mut model, &samples);
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 30,
            "training 100k samples took {}s, expected <30s",
            elapsed.as_secs()
        );
        assert_eq!(result.samples_trained, 100_000);
        assert_eq!(model.sample_count(), 100_000);

        // Verify model state JSON size < 2MB
        let json = serde_json::to_string(&result.final_state).expect("state should serialize");
        assert!(
            json.len() < 2 * 1024 * 1024,
            "model state JSON size {} bytes, expected <2MB",
            json.len()
        );
    }

    // -- T7C-H08: Prediction/update benchmark --

    #[test]
    fn prediction_update_benchmark() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = {
            let mut f = RoutingFeatures::default();
            for v in f.values.iter_mut() {
                *v = 0.5;
            }
            f
        };

        // Train with 1000 samples first so predictions are meaningful
        for _ in 0..1000 {
            model.update(&features, 1.0);
        }

        // Benchmark 10,000 predictions
        let start = std::time::Instant::now();
        let iterations = 10_000u32;
        for _ in 0..iterations {
            let _ = std::hint::black_box(model.predict(&features));
        }
        let predict_elapsed = start.elapsed();
        let per_prediction_ns = predict_elapsed.as_nanos() / iterations as u128;
        assert!(
            per_prediction_ns < 1_000_000, // < 1ms = 1,000,000 ns
            "per-prediction {}ns ({}us), expected <1ms",
            per_prediction_ns,
            per_prediction_ns / 1_000
        );

        // Benchmark 10,000 updates
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            model.update(&features, 1.0);
        }
        let update_elapsed = start.elapsed();
        let per_update_ns = update_elapsed.as_nanos() / iterations as u128;
        assert!(
            per_update_ns < 1_000_000, // < 1ms
            "per-update {}ns ({}us), expected <1ms",
            per_update_ns,
            per_update_ns / 1_000
        );
    }

    // -- T7C-H09: Model matrix test --
    // Tests all 4 model types through the full lifecycle: cold predict, update,
    // save/load round-trip, and reset.

    fn assert_model_lifecycle<M: RoutingModel>(
        make: impl Fn() -> M,
        name: &str,
        expected_algorithm: &str,
        target_fn: impl Fn(u32) -> f64,
    ) {
        let mut model = make();
        let features = random_like_features(42);

        // 1. Cold predict
        let cold_pred = model.predict(&features);
        assert!(
            cold_pred.value.is_finite(),
            "{name}: cold prediction value not finite"
        );
        assert!(
            cold_pred.confidence >= 0.0 && cold_pred.confidence <= 1.0,
            "{name}: cold confidence out of range: {}",
            cold_pred.confidence
        );
        assert_eq!(
            cold_pred.sample_count, 0,
            "{name}: cold predict should have 0 samples"
        );
        assert_eq!(
            model.sample_count(),
            0,
            "{name}: cold sample_count should be 0"
        );

        // 2. Update — train 50 samples
        for i in 0..50u32 {
            model.update(&features, target_fn(i));
        }
        assert_eq!(
            model.sample_count(),
            50,
            "{name}: sample_count should be 50 after training"
        );

        // Post-update prediction should be finite
        let warm_pred = model.predict(&features);
        assert!(
            warm_pred.value.is_finite(),
            "{name}: post-update prediction not finite"
        );
        assert!(
            warm_pred.confidence.is_finite(),
            "{name}: post-update confidence not finite"
        );
        assert_eq!(warm_pred.sample_count, 50);

        // 3. Save/load round-trip
        let state = model.save();
        assert_eq!(
            state.algorithm, expected_algorithm,
            "{name}: save() produced wrong algorithm"
        );
        assert_eq!(state.update_count, 50, "{name}: update_count mismatch");
        assert!(state.verify_checksum(), "{name}: checksum mismatch");

        let pre_load_pred = model.predict(&features);
        let loaded = M::load(&state).unwrap();
        assert_eq!(
            loaded.sample_count(),
            50,
            "{name}: loaded sample_count wrong"
        );
        let loaded_pred = loaded.predict(&features);
        assert!(
            (pre_load_pred.value - loaded_pred.value).abs() < 1e-10,
            "{name}: predictions differ after save/load: pre={}, loaded={}",
            pre_load_pred.value,
            loaded_pred.value
        );

        // 4. Reset
        model.reset();
        assert_eq!(
            model.sample_count(),
            0,
            "{name}: sample_count should be 0 after reset"
        );
        let reset_pred = model.predict(&features);
        assert!(
            reset_pred.value.is_finite(),
            "{name}: prediction after reset not finite"
        );
        assert_eq!(
            reset_pred.sample_count, 0,
            "{name}: prediction sample_count should be 0 after reset"
        );
    }

    #[test]
    fn model_matrix_all_models() {
        assert_model_lifecycle(
            || SuccessModel::new(FEATURE_DIMENSION),
            "success",
            "success_logistic_adagrad",
            |i| if i % 2 == 0 { 1.0 } else { 0.0 },
        );
        assert_model_lifecycle(
            || LatencyModel::new(FEATURE_DIMENSION),
            "latency",
            "latency_linear",
            |i| 100.0 + i as f64,
        );
        assert_model_lifecycle(
            || TtftModel::new(FEATURE_DIMENSION),
            "ttft",
            "ttft_linear",
            |i| 50.0 + i as f64,
        );
        assert_model_lifecycle(
            || CostModel::new(FEATURE_DIMENSION),
            "cost",
            "cost_linear",
            |i| 0.01 + i as f64 * 0.001,
        );
    }

    // -- Fix 5: Training benchmark for all 4 models --

    #[test]
    fn training_benchmark_all_models_100k() {
        let models: Vec<Box<dyn RoutingModel>> = vec![
            Box::new(SuccessModel::new(FEATURE_DIMENSION)),
            Box::new(LatencyModel::new(FEATURE_DIMENSION)),
            Box::new(TtftModel::new(FEATURE_DIMENSION)),
            Box::new(CostModel::new(FEATURE_DIMENSION)),
        ];
        let features = RoutingFeatures::default();
        for mut model in models {
            let start = std::time::Instant::now();
            for i in 0..100_000 {
                model.update(&features, if i % 2 == 0 { 1.0 } else { 0.0 });
            }
            let elapsed = start.elapsed();
            assert!(
                elapsed.as_secs() < 30,
                "{}: 100k took {}s",
                model.name(),
                elapsed.as_secs()
            );
            let state = model.save();
            let size = serde_json::to_vec(&state).unwrap().len();
            assert!(
                size < 2 * 1024 * 1024,
                "{}: state is {} bytes",
                model.name(),
                size
            );
        }
    }

    // -- Fix 6: P95/P99 prediction benchmark --

    #[test]
    fn prediction_p95_p99_benchmark() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        for i in 0..1000 {
            model.update(&features, (i % 2) as f64);
        }

        let mut times = Vec::new();
        for _ in 0..10_000 {
            let start = std::time::Instant::now();
            let _ = model.predict(&features);
            times.push(start.elapsed().as_micros() as f64);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95 = times[(times.len() as f64 * 0.95) as usize];
        let p99 = times[(times.len() as f64 * 0.99) as usize];
        assert!(p95 < 1000.0, "P95 = {}us", p95); // < 1ms
        assert!(p99 < 3000.0, "P99 = {}us", p99); // < 3ms
    }

    // -- Fix 7: Target validation tests --

    #[test]
    fn update_rejects_nan_target() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, f64::NAN);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "NaN target should be rejected"
        );
    }

    #[test]
    fn update_rejects_inf_target() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, f64::INFINITY);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "Inf target should be rejected"
        );
    }

    #[test]
    fn update_rejects_neg_inf_target() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, f64::NEG_INFINITY);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "-Inf target should be rejected"
        );
    }

    #[test]
    fn success_model_clamps_target_to_0_1() {
        let mut model = SuccessModel::new(FEATURE_DIMENSION);
        let mut features = RoutingFeatures::default();
        for v in features.values.iter_mut() {
            *v = 0.5;
        }
        // Train with target=1.0 to push weights up
        for _ in 0..100 {
            model.update(&features, 1.0);
        }
        let pred_high = model.predict(&features).value;

        // Reset and train with target=2.0 (should be clamped to 1.0, same as above)
        model.reset();
        for _ in 0..100 {
            model.update(&features, 2.0);
        }
        let pred_clamped = model.predict(&features).value;

        assert!(
            (pred_high - pred_clamped).abs() < 1e-10,
            "target=2.0 should be clamped to 1.0: high={}, clamped={}",
            pred_high,
            pred_clamped
        );
    }

    #[test]
    fn latency_model_rejects_nan_target() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, f64::NAN);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "LatencyModel: NaN target should be rejected"
        );
    }

    #[test]
    fn latency_model_rejects_negative_target() {
        let mut model = LatencyModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, -100.0);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "LatencyModel: negative target should be rejected"
        );
    }

    #[test]
    fn ttft_model_rejects_nan_target() {
        let mut model = TtftModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, f64::NAN);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "TtftModel: NaN target should be rejected"
        );
    }

    #[test]
    fn ttft_model_rejects_negative_target() {
        let mut model = TtftModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, -50.0);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "TtftModel: negative target should be rejected"
        );
    }

    #[test]
    fn cost_model_rejects_nan_target() {
        let mut model = CostModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, f64::NAN);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "CostModel: NaN target should be rejected"
        );
    }

    #[test]
    fn cost_model_rejects_negative_target() {
        let mut model = CostModel::new(FEATURE_DIMENSION);
        let features = RoutingFeatures::default();
        let before = model.predict(&features).value;
        model.update(&features, -0.01);
        let after = model.predict(&features).value;
        assert!(
            (before - after).abs() < 1e-10,
            "CostModel: negative target should be rejected"
        );
    }
}
