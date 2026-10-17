//! Ranking the eligible candidates, and the weights a default policy starts from.

use super::policy::{PolicyFallback, PolicyRequirements};
use super::policy::{PolicyPreference, RoutingPolicy};
use super::profile::TaskProfile;
use serde::{Deserialize, Serialize};

use crate::config::ModelTier;

// ---------------------------------------------------------- Scoring

/// Individual dimension scores for a candidate, each in [0.0, 1.0].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    /// 1.0 = perfectly healthy, 0.0 = circuit open.
    pub health: f64,
    /// Inverse-normalized latency (1000ms baseline).
    pub latency: f64,
    /// Inverse-normalized cost ($10/MTok baseline).
    pub cost: f64,
    /// Lower priority number = higher score.
    pub priority: f64,
    /// Closer to preferred tier = higher score.
    pub tier: f64,
}

/// A candidate annotated with its computed policy score.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub exposed_id: String,
    pub total_score: f64,
    pub breakdown: ScoreBreakdown,
}

/// Context for scoring a single candidate against a policy's preferences.
///
/// The caller supplies runtime signals (health, latency) that the policy
/// engine itself does not own.
pub struct ScoringContext<'a> {
    /// Health score 0.0-1.0, from the router's circuit breaker state.
    pub health: f64,
    /// EWMA latency in milliseconds, from the router.
    pub avg_latency_ms: f64,
    /// Input price per million tokens. `None` when pricing is not configured.
    pub input_per_mtok: Option<f64>,
    /// Output price per million tokens. `None` when pricing is not configured.
    pub output_per_mtok: Option<f64>,
    /// Lower value = higher priority (from ModelEntry).
    pub priority: i32,
    /// The candidate's tier, if assigned.
    pub tier: Option<ModelTier>,
    /// The task profile for request-aware cost estimation.
    pub task: Option<&'a TaskProfile>,
}

const LATENCY_BASELINE_MS: f64 = 1000.0;
const COST_BASELINE_MTOK: f64 = 10.0;
/// Maximum tier distance (Fast=0 .. Frontier=3, so max gap is 3).
const MAX_TIER_DISTANCE: f64 = 3.0;
/// Cap for priority normalization. Priority values are user-configured i32;
/// anything above this is treated as equally low-priority.
const PRIORITY_CAP: f64 = 100.0;

/// Compute a weighted policy score for a single candidate.
///
/// Each dimension produces a value in [0.0, 1.0]. The final score is the
/// weighted sum, also in [0.0, 1.0] (weights are normalized internally so
/// they do not need to sum to 1.0).
pub fn score_candidate<'a>(pref: &PolicyPreference, ctx: &ScoringContext<'a>) -> ScoredCandidate {
    let health = ctx.health.clamp(0.0, 1.0);

    // Latency: lower is better. Inverse-normalized to 1000ms baseline.
    let latency = if ctx.avg_latency_ms <= 0.0 {
        1.0 // unmeasured = benefit of the doubt
    } else {
        (LATENCY_BASELINE_MS / ctx.avg_latency_ms).min(1.0)
    };

    // Cost: request-aware estimation when pricing and task profile are available.
    // estimated_cost = context_tokens * input_per_mtok / 1M + output_tokens * output_per_mtok / 1M
    // Normalize against $1.0 reference: lower cost → higher score.
    let cost = match (ctx.input_per_mtok, ctx.output_per_mtok, ctx.task) {
        (Some(inp), Some(out), Some(task)) => {
            let estimated_cost = task.context_tokens as f64 * inp / 1_000_000.0
                + task.estimated_output_tokens as f64 * out / 1_000_000.0;
            (1.0 / (1.0 + estimated_cost)).min(1.0)
        }
        (Some(inp), Some(out), None) => {
            // No task profile: fall back to average cost with baseline normalization.
            let avg = (inp + out) / 2.0;
            if avg > 0.0 {
                (COST_BASELINE_MTOK / avg).min(1.0)
            } else {
                0.5
            }
        }
        _ => 0.5, // unknown cost = neutral
    };

    // Priority: lower number = higher score. 0 → 1.0, cap → 0.0.
    let priority = if ctx.priority <= 0 {
        1.0
    } else {
        (1.0 - (ctx.priority as f64) / PRIORITY_CAP).clamp(0.0, 1.0)
    };

    // Tier: closer to preferred = higher score. Distance 0 → 1.0, max → 0.0.
    let tier = match (pref.preferred_tier, ctx.tier) {
        (Some(preferred), Some(candidate)) => {
            let dist = tier_distance(preferred, candidate);
            1.0 - (dist / MAX_TIER_DISTANCE)
        }
        _ => 0.5, // no preference or no tier = neutral
    };

    let breakdown = ScoreBreakdown {
        health,
        latency,
        cost,
        priority,
        tier,
    };

    // Weighted sum with normalized weights.
    let w_sum = pref.health_weight
        + pref.latency_weight
        + pref.cost_weight
        + pref.priority_weight
        + pref.tier_weight;

    let total_score = if w_sum > 0.0 {
        (health * pref.health_weight
            + latency * pref.latency_weight
            + cost * pref.cost_weight
            + priority * pref.priority_weight
            + tier * pref.tier_weight)
            / w_sum
    } else {
        0.0
    };

    ScoredCandidate {
        exposed_id: String::new(), // filled by caller
        total_score,
        breakdown,
    }
}

/// Absolute distance between two tiers (0-3).
pub(super) fn tier_distance(a: ModelTier, b: ModelTier) -> f64 {
    (tier_ord(a) as f64 - tier_ord(b) as f64).abs()
}

fn tier_ord(t: ModelTier) -> u8 {
    match t {
        ModelTier::Fast => 0,
        ModelTier::Standard => 1,
        ModelTier::Reasoning => 2,
        ModelTier::Frontier => 3,
    }
}

// ----------------------------------------------------------- Default

/// The default policy: standard tier, escalate on failure.
pub fn default_policy() -> RoutingPolicy {
    RoutingPolicy {
        id: "default".into(),
        name: "Default".into(),
        enabled: true,
        matchers: Vec::new(),
        requirements: PolicyRequirements::default(),
        preference: PolicyPreference {
            preferred_tier: Some(ModelTier::Standard),
            ..PolicyPreference::default()
        },
        fallback: PolicyFallback::Escalate {
            enabled: true,
            max_steps: 2,
        },
    }
}
