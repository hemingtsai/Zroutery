use crate::ml::features::RoutingFeatures;

use super::base::{ModelState, Prediction, RoutingModel};

// ---------------------------------------------------------------------------
// SuccessModel — Logistic regression with AdaGrad
// ---------------------------------------------------------------------------

/// Binary classifier for request success prediction.
/// Uses online logistic regression with per-feature adaptive learning rate (AdaGrad).
///
/// `Clone` is for the operator surface: analysing the model that is serving
/// needs the same weights traffic is being ranked by, taken as a snapshot
/// rather than reloaded or re-trained.
#[derive(Debug, Clone)]
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

    pub(super) fn sigmoid(x: f64) -> f64 {
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

/// Samples below which a success head is still cold.
const SUCCESS_COLD_SAMPLES: u64 = 10;

/// The evidence weight of a success head's own estimate.
///
/// A head that has seen only a handful of samples beyond the cold floor cannot
/// be as sure of its output as one backed by hundreds, so the margin below is
/// discounted by `samples / (samples + prior)`. The prior is small so the
/// ten-sample cold floor already counts as real evidence.
const SUCCESS_CONFIDENCE_PRIOR_SAMPLES: f64 = 2.0;

/// How much a success head trusts its own prediction.
///
/// The predicted success probability and the model's confidence in it are
/// different quantities, and the guard, the utility uncertainty penalty and
/// the Sticky threshold all read the second. `4p(1-p)` is the Bernoulli
/// variance of the outcome - maximal at `p = 0.5` and zero at the extremes -
/// so reading it as confidence let the *least* decisive prediction clear the
/// switch gate while blocking the most decisive one. Confidence here is the
/// head's margin from a coin flip, `|2p - 1|`, discounted by how much evidence
/// stands behind the estimate, so it grows as the prediction becomes both
/// decisive and well sampled.
fn success_confidence(probability: f64, samples: u64) -> f64 {
    if samples < SUCCESS_COLD_SAMPLES {
        return 0.1;
    }
    let decisiveness = (2.0 * probability - 1.0).abs();
    let evidence = samples as f64 / (samples as f64 + SUCCESS_CONFIDENCE_PRIOR_SAMPLES);
    (decisiveness * evidence).clamp(0.0, 1.0)
}

impl RoutingModel for SuccessModel {
    fn name(&self) -> &str {
        "success"
    }

    fn predict(&self, features: &RoutingFeatures) -> Prediction {
        let p = self.raw_predict(&features.values);
        Prediction::clamped(p, success_confidence(p, self.samples), self.samples)
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
        if state.parameters.len().is_multiple_of(2) {
            return Err(format!(
                "parameter count must be odd (2n+1), got {}",
                state.parameters.len()
            ));
        }
        if !state.verify_checksum() {
            return Err("checksum mismatch".into());
        }
        let n = (state.parameters.len() - 1) / 2;
        if n != crate::ml::features::FEATURE_DIMENSION {
            return Err(format!(
                "expected {} weights, got {}",
                crate::ml::features::FEATURE_DIMENSION,
                n
            ));
        }
        let bias = state.parameters[0];
        let weights = state.parameters[1..=n].to_vec();
        let grad_sq = state.parameters[n + 1..].to_vec();
        // The AdaGrad accumulator is a sum of squared gradients seeded at 1, so
        // its domain is strictly positive and a value outside it is corrupt
        // state, not a finite parameter. Finiteness alone accepts a negative or
        // zero accumulator, and the next update's `sqrt` then turns a validated
        // checkpoint into NaN predictions.
        if let Some((index, value)) = grad_sq
            .iter()
            .enumerate()
            .find(|(_, value)| (**value).partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
        {
            return Err(format!(
                "AdaGrad accumulator grad_sq[{index}] must be positive, got {value}"
            ));
        }
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
