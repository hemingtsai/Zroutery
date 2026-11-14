use crate::ml::features::RoutingFeatures;

use super::base::{ModelState, Prediction, RoutingModel};

// ---------------------------------------------------------------------------
// LatencyModel — Linear regression for total latency
// ---------------------------------------------------------------------------

/// Predicts expected total latency in ms.
/// Uses online linear regression with EWMA for the residual.
#[derive(Debug, Clone)]
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
            return Err(format!("expected latency_linear, got {}", state.algorithm));
        }
        // Expected: 2 scalars (bias, residual_ewma) + n weights = n+2 (minimum 2)
        if state.parameters.len() < 2 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = state.parameters.len() - 2; // bias + residual_ewma + weights
        if n != crate::ml::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                crate::ml::features::FEATURE_DIMENSION,
                n
            ));
        }
        let bias = state.parameters[0];
        let residual_ewma = state.parameters[1];
        // The residual EWMA averages absolute errors, so it cannot be negative.
        if residual_ewma < 0.0 {
            return Err(format!(
                "residual_ewma must be non-negative, got {residual_ewma}"
            ));
        }
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
#[derive(Debug, Clone)]
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
            return Err(format!("expected ttft_linear, got {}", state.algorithm));
        }
        // Expected: 2 scalars (bias, residual_ewma) + n weights = n+2 (minimum 2)
        if state.parameters.len() < 2 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = state.parameters.len() - 2; // bias + residual_ewma + weights
        if n != crate::ml::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                crate::ml::features::FEATURE_DIMENSION,
                n
            ));
        }
        let bias = state.parameters[0];
        let residual_ewma = state.parameters[1];
        // The residual EWMA averages absolute errors, so it cannot be negative.
        if residual_ewma < 0.0 {
            return Err(format!(
                "residual_ewma must be non-negative, got {residual_ewma}"
            ));
        }
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
#[derive(Debug, Clone)]
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
            return Err(format!("expected cost_linear, got {}", state.algorithm));
        }
        // Expected: 2 scalars (bias, residual_ewma) + n weights = n+2 (minimum 2)
        if state.parameters.len() < 2 {
            return Err("too few parameters".into());
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = state.parameters.len() - 2; // bias + residual_ewma + weights
        if n != crate::ml::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                crate::ml::features::FEATURE_DIMENSION,
                n
            ));
        }
        let bias = state.parameters[0];
        let residual_ewma = state.parameters[1];
        // The residual EWMA averages absolute errors, so it cannot be negative.
        if residual_ewma < 0.0 {
            return Err(format!(
                "residual_ewma must be non-negative, got {residual_ewma}"
            ));
        }
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
