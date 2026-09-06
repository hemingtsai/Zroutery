//! Fixed-size feature vector for ML routing models.
//!
//! Defines a 32-dimensional feature vector (`RoutingFeatures`) that encodes
//! task characteristics, candidate capabilities, runtime observations, and
//! account state into a normalized `[0.0, 1.0]` (or `-1.0` for unknown)
//! representation consumed by ML scoring models.
//!
//! Feature order is deterministic and must not change within a schema version.
//! Bump [`FEATURE_SCHEMA_VERSION`] when features are added, removed, or
//! reordered.

use serde::{Deserialize, Serialize};

use crate::config::{ModelCapabilities, ModelTier};
use crate::failure::FailureClass;
use crate::observation::ObservationFreshness;
use crate::policy::{Complexity, TaskProfile};
use crate::stats_ext::ProviderModelStats;

// ---------------------------------------------------------------------------
// Schema metadata
// ---------------------------------------------------------------------------

/// Schema version for the feature vector. Increment when features change.
pub const FEATURE_SCHEMA_VERSION: u32 = 1;

/// Total number of features in the vector.
pub const FEATURE_DIMENSION: usize = 32;

/// Sentinel for unknown/unavailable features.
pub const UNKNOWN: f32 = -1.0;

// ---------------------------------------------------------------------------
// Feature indices — order must not change within a schema version.
// ---------------------------------------------------------------------------

// --- Task features (0..=7) ---

/// 1.0 = streaming, 0.0 = buffered.
pub const F_STREAMING: usize = 0;
/// 1.0 = request has tools defined.
pub const F_HAS_TOOLS: usize = 1;
/// 1.0 = request has image/vision content.
pub const F_HAS_VISION: usize = 2;
/// Normalized context size: `log2(tokens) / 20.0`.
pub const F_CONTEXT_TOKENS: usize = 3;
/// Normalized estimated output size: `log2(tokens) / 20.0`.
pub const F_EST_OUTPUT_TOKENS: usize = 4;
/// Task complexity: Simple=0.0, Standard=0.33, Complex=0.66, Frontier=1.0.
pub const F_COMPLEXITY: usize = 5;
/// Task type: Chat=0.0, Code=0.2, Vision=0.4, ToolUse=0.6, Analysis=0.8, Creative=1.0.
pub const F_TASK_TYPE: usize = 6;
/// Normalized message count: `min(count, 100) / 100.0`.
pub const F_MESSAGE_COUNT: usize = 7;

// --- Candidate features (8..=16) ---

/// Model tier: Fast=0.0, Standard=0.33, Reasoning=0.66, Frontier=1.0.
pub const F_TIER: usize = 8;
/// 1.0 = candidate has vision capability.
pub const F_CAP_VISION: usize = 9;
/// 1.0 = candidate has tool-use capability.
pub const F_CAP_TOOLS: usize = 10;
/// 1.0 = candidate has thinking/extended-thinking capability.
pub const F_CAP_THINKING: usize = 11;
/// 1.0 = candidate has structured output capability.
pub const F_CAP_STRUCTURED: usize = 12;
/// 1.0 = candidate has audio capability.
pub const F_CAP_AUDIO: usize = 13;
/// 1.0 = candidate has video capability.
pub const F_CAP_VIDEO: usize = 14;
/// 1.0 = candidate has file capability.
pub const F_CAP_FILES: usize = 15;
/// Normalized priority: `1.0 - (priority / 100.0)` clamped to [0, 1].
pub const F_PRIORITY: usize = 16;

// --- Runtime observation features (17..=23) ---

/// Health observation score [0, 1].
pub const F_OBS_HEALTH: usize = 17;
/// Inverse-normalized EWMA latency: `1.0 - (ewma_ms / 5000.0)`.
pub const F_OBS_LATENCY_EWMA: usize = 18;
/// Inverse-normalized P50 latency: `1.0 - (p50 / 5000.0)`.
pub const F_OBS_LATENCY_P50: usize = 19;
/// Inverse-normalized P95 latency: `1.0 - (p95 / 10000.0)`.
pub const F_OBS_LATENCY_P95: usize = 20;
/// Inverse-normalized TTFT EWMA: `1.0 - (ewma / 2000.0)`.
pub const F_OBS_TTFT_EWMA: usize = 21;
/// Success rate [0, 1].
pub const F_OBS_SUCCESS_RATE: usize = 22;
/// Observation freshness: Fresh=1.0, Recent=0.75, Stale=0.5, Unknown=0.25.
pub const F_OBS_FRESHNESS: usize = 23;

// --- Statistics features (24..=27) ---

/// Normalized total request count: `log2(count + 1) / 20.0`.
pub const F_STAT_TOTAL_REQUESTS: usize = 24;
/// Failure rate: `failures / total` [0, 1].
pub const F_STAT_FAILURE_RATE: usize = 25;
/// Timeout rate: `timeouts / total` [0, 1].
pub const F_STAT_TIMEOUT_RATE: usize = 26;
/// Rate-limit rate: `rate_limits / total` [0, 1].
pub const F_STAT_RATELIMIT_RATE: usize = 27;

// --- Account features (28..=31) ---

/// Remaining quota ratio: `remaining / total` [0, 1].
pub const F_ACCT_QUOTA_REMAINING: usize = 28;
/// Rate limit pressure from account: `RateLimitState.pressure()` [0, 1].
pub const F_ACCT_RATE_PRESSURE: usize = 29;
/// Account health: Active=1.0, Degraded=0.5, etc.
pub const F_ACCT_HEALTH: usize = 30;
/// Reserved for future use.
pub const F_RESERVE_31: usize = 31;

// ---------------------------------------------------------------------------
// RoutingFeatures
// ---------------------------------------------------------------------------

/// Fixed-size feature vector for ML models.
///
/// All features are normalized to `[0.0, 1.0]` or use [`UNKNOWN`] (`-1.0`)
/// for unavailable data. Feature order is deterministic and must not change
/// within a schema version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingFeatures {
    /// The feature values.
    pub values: [f32; FEATURE_DIMENSION],
    /// Schema version that produced this vector.
    pub schema_version: u32,
}

impl Default for RoutingFeatures {
    fn default() -> Self {
        RoutingFeatures {
            values: [UNKNOWN; FEATURE_DIMENSION],
            schema_version: FEATURE_SCHEMA_VERSION,
        }
    }
}

// ---------------------------------------------------------------------------
// FeatureContext — input for extraction
// ---------------------------------------------------------------------------

/// Input data for feature extraction.
///
/// Each field is optional; missing fields leave the corresponding features at
/// [`UNKNOWN`].
pub struct FeatureContext<'a> {
    /// Task profile derived from the request.
    pub task: Option<&'a TaskProfile>,
    /// Number of messages in the conversation (for F_MESSAGE_COUNT).
    pub message_count: Option<usize>,
    /// Candidate model tier.
    pub tier: Option<ModelTier>,
    /// Candidate model capabilities.
    pub capabilities: Option<&'a ModelCapabilities>,
    /// Candidate priority (lower = higher priority).
    pub priority: i32,
    /// Runtime observation for the candidate.
    pub observation: Option<&'a crate::observation::RuntimeObservation>,
    /// Streaming statistics for the candidate.
    pub stats: Option<&'a ProviderModelStats>,
    /// Account runtime state (feature-gated).
    #[cfg(feature = "account")]
    pub account: Option<&'a crate::account::AccountRuntime>,
}

// ---------------------------------------------------------------------------
// extract_features
// ---------------------------------------------------------------------------

/// Build a [`RoutingFeatures`] vector from the given [`FeatureContext`].
///
/// Features for missing inputs are set to [`UNKNOWN`]. All populated features
/// are clamped to `[-1.0, 1.0]`.
pub fn extract_features(ctx: &FeatureContext) -> RoutingFeatures {
    let mut f = RoutingFeatures::default();

    // --- Task features ---
    if let Some(task) = ctx.task {
        f.values[F_STREAMING] = if task.streaming { 1.0 } else { 0.0 };
        f.values[F_HAS_TOOLS] = if task.has_tools { 1.0 } else { 0.0 };
        f.values[F_HAS_VISION] = if task.has_vision { 1.0 } else { 0.0 };
        f.values[F_CONTEXT_TOKENS] = normalize_log2(task.context_tokens);
        f.values[F_EST_OUTPUT_TOKENS] = normalize_log2(task.estimated_output_tokens);
        f.values[F_COMPLEXITY] = complexity_to_f32(task.complexity);
        f.values[F_TASK_TYPE] = task_type_to_f32(task.task_type);
    }
    if let Some(count) = ctx.message_count {
        f.values[F_MESSAGE_COUNT] = (count.min(100) as f32) / 100.0;
    }

    // --- Candidate features ---
    if let Some(tier) = ctx.tier {
        f.values[F_TIER] = tier_to_f32(tier);
    }
    if let Some(caps) = ctx.capabilities {
        f.values[F_CAP_VISION] = if caps.vision { 1.0 } else { 0.0 };
        f.values[F_CAP_TOOLS] = if caps.tools { 1.0 } else { 0.0 };
        f.values[F_CAP_THINKING] = if caps.thinking { 1.0 } else { 0.0 };
        f.values[F_CAP_STRUCTURED] = if caps.structured_output { 1.0 } else { 0.0 };
        f.values[F_CAP_AUDIO] = if caps.audio { 1.0 } else { 0.0 };
        f.values[F_CAP_VIDEO] = if caps.video { 1.0 } else { 0.0 };
        f.values[F_CAP_FILES] = if caps.files { 1.0 } else { 0.0 };
    }
    f.values[F_PRIORITY] = (1.0 - (ctx.priority as f32 / 100.0)).clamp(0.0, 1.0);

    // --- Runtime observation features ---
    if let Some(obs) = ctx.observation {
        f.values[F_OBS_HEALTH] = obs.health.score() as f32;
        if let Some(ewma) = obs.latency.total_ms.value {
            f.values[F_OBS_LATENCY_EWMA] = inverse_normalize(ewma, 5000.0);
        }
        // P50 and P95 from the observation's total_ms signal — the observation
        // layer tracks a single point estimate (EWMA), so we use it for both
        // when the detailed stats are unavailable.
        if let Some(total) = obs.latency.total_ms.value {
            f.values[F_OBS_LATENCY_P50] = inverse_normalize(total, 5000.0);
            f.values[F_OBS_LATENCY_P95] = inverse_normalize(total, 10000.0);
        }
        if let Some(ttft) = obs.latency.ttft_ms.value {
            f.values[F_OBS_TTFT_EWMA] = inverse_normalize(ttft, 2000.0);
        }
        if let Some(rate) = obs.health.success_rate.value {
            f.values[F_OBS_SUCCESS_RATE] = rate as f32;
        }
        f.values[F_OBS_FRESHNESS] = freshness_to_f32(obs.freshness);
    }

    // --- Statistics features ---
    if let Some(stats) = ctx.stats {
        f.values[F_STAT_TOTAL_REQUESTS] = normalize_log2(stats.total_requests);
        if stats.total_requests > 0 {
            let total = stats.total_requests as f32;
            f.values[F_STAT_FAILURE_RATE] =
                (stats.total_failures as f32 / total).clamp(0.0, 1.0);
            f.values[F_STAT_TIMEOUT_RATE] =
                (stats.failures.count(FailureClass::Timeout) as f32 / total).clamp(0.0, 1.0);
            f.values[F_STAT_RATELIMIT_RATE] =
                (stats.failures.count(FailureClass::RateLimit) as f32 / total).clamp(0.0, 1.0);
        }
    }

    // --- Account features ---
    #[cfg(feature = "account")]
    if let Some(acct) = ctx.account {
        f.values[F_ACCT_QUOTA_REMAINING] = acct
            .quota
            .as_ref()
            .map(|q| {
                if q.total > 0.0 {
                    (q.remaining / q.total).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                }
            })
            .unwrap_or(UNKNOWN);
        f.values[F_ACCT_RATE_PRESSURE] = acct
            .rate_limit
            .as_ref()
            .map(|rl| rl.pressure() as f32)
            .unwrap_or(UNKNOWN);
        f.values[F_ACCT_HEALTH] = account_status_to_f32(acct.status);
    }

    f
}

// ---------------------------------------------------------------------------
// Normalization helpers
// ---------------------------------------------------------------------------

/// Normalize a u64 value using log2 scaling into [0.0, 1.0].
/// Returns 0.0 for zero input.
pub(crate) fn normalize_log2(value: u64) -> f32 {
    if value == 0 {
        return 0.0;
    }
    ((value as f32).log2() / 20.0).clamp(0.0, 1.0)
}

/// Inverse-normalize: `1.0 - (value / baseline)`, clamped to [0.0, 1.0].
/// Higher raw values produce lower feature values (latency penalty).
fn inverse_normalize(value: f64, baseline: f64) -> f32 {
    (1.0 - (value / baseline) as f32).clamp(0.0, 1.0)
}

/// Map a [`ModelTier`] to a float.
fn tier_to_f32(tier: ModelTier) -> f32 {
    match tier {
        ModelTier::Fast => 0.0,
        ModelTier::Standard => 0.33,
        ModelTier::Reasoning => 0.66,
        ModelTier::Frontier => 1.0,
    }
}

/// Map a [`Complexity`] to a float.
fn complexity_to_f32(c: Complexity) -> f32 {
    match c {
        Complexity::Simple => 0.0,
        Complexity::Standard => 0.33,
        Complexity::Complex => 0.66,
        Complexity::Frontier => 1.0,
    }
}

/// Map a [`TaskType`] to a float.
fn task_type_to_f32(t: crate::policy::TaskType) -> f32 {
    match t {
        crate::policy::TaskType::Chat => 0.0,
        crate::policy::TaskType::Code => 0.2,
        crate::policy::TaskType::Vision => 0.4,
        crate::policy::TaskType::ToolUse => 0.6,
        crate::policy::TaskType::Analysis => 0.8,
        crate::policy::TaskType::Creative => 1.0,
    }
}

/// Map [`ObservationFreshness`] to a float.
fn freshness_to_f32(f: ObservationFreshness) -> f32 {
    match f {
        ObservationFreshness::Fresh => 1.0,
        ObservationFreshness::Recent => 0.75,
        ObservationFreshness::Stale => 0.5,
        ObservationFreshness::Unknown => 0.25,
    }
}

/// Map [`AccountStatus`] to a float.
#[cfg(feature = "account")]
fn account_status_to_f32(status: crate::account::AccountStatus) -> f32 {
    match status {
        crate::account::AccountStatus::Active => 1.0,
        crate::account::AccountStatus::RateLimited => 0.5,
        crate::account::AccountStatus::QuotaExhausted => 0.25,
        crate::account::AccountStatus::Suspended => 0.0,
        crate::account::AccountStatus::AuthenticationExpired => 0.0,
        crate::account::AccountStatus::Unknown => UNKNOWN,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{HealthState, ObservationFreshness, Signal};
    use crate::policy::{Complexity, TaskProfile, TaskType};
    use crate::stats_ext::ProviderModelStats;

    // -- Determinism: same input -> bit-exact same features --

    #[test]
    fn determinism_same_input_same_output() {
        let task = TaskProfile {
            streaming: true,
            has_tools: true,
            has_vision: false,
            context_tokens: 50_000,
            estimated_output_tokens: 4096,
            complexity: Complexity::Complex,
            task_type: TaskType::ToolUse,
            ..Default::default()
        };
        let ctx = FeatureContext {
            task: Some(&task),
            message_count: Some(10),
            tier: Some(ModelTier::Standard),
            capabilities: Some(&ModelCapabilities {
                tools: true,
                thinking: true,
                ..Default::default()
            }),
            priority: 50,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };

        let a = extract_features(&ctx);
        let b = extract_features(&ctx);
        assert_eq!(a, b, "same input must produce bit-exact same features");
    }

    #[test]
    fn determinism_multiple_extractions() {
        let task = TaskProfile::default();
        let ctx = FeatureContext {
            task: Some(&task),
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };

        let first = extract_features(&ctx);
        for _ in 0..100 {
            assert_eq!(extract_features(&ctx), first);
        }
    }

    // -- Missing data: no observation -> UNKNOWN values, no NaN/Inf --

    #[test]
    fn missing_data_all_none() {
        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        // All task/candidate/observation/stats features should be UNKNOWN
        // except F_PRIORITY which is always computed.
        for i in 0..FEATURE_DIMENSION {
            if i == F_PRIORITY {
                // priority=0 -> 1.0 - 0/100 = 1.0
                assert_eq!(f.values[i], 1.0);
            } else {
                assert_eq!(f.values[i], UNKNOWN, "index {i} should be UNKNOWN");
            }
        }
    }

    #[test]
    fn no_nan_or_inf_in_any_feature() {
        let task = TaskProfile {
            streaming: true,
            has_tools: true,
            has_vision: true,
            context_tokens: u64::MAX,
            estimated_output_tokens: u64::MAX,
            complexity: Complexity::Frontier,
            task_type: TaskType::Creative,
            ..Default::default()
        };
        let mut stats = ProviderModelStats::new("m".into(), "p".into());
        stats.record_success(999_999.0, Some(999_999.0));
        stats.record_failure(FailureClass::Timeout);
        stats.record_failure(FailureClass::RateLimit);

        let ctx = FeatureContext {
            task: Some(&task),
            message_count: Some(1000),
            tier: Some(ModelTier::Frontier),
            capabilities: Some(&ModelCapabilities {
                vision: true,
                tools: true,
                thinking: true,
                structured_output: true,
                audio: true,
                video: true,
                files: true,
            }),
            priority: -50,
            observation: None,
            stats: Some(&stats),
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        for (i, &v) in f.values.iter().enumerate() {
            assert!(v.is_finite(), "feature {i} is not finite: {v}");
            assert!(v >= -1.0, "feature {i} below -1.0: {v}");
            assert!(v <= 1.0, "feature {i} above 1.0: {v}");
        }
    }

    // -- Dimension --

    #[test]
    fn feature_dimension_is_32() {
        assert_eq!(FEATURE_DIMENSION, 32);
    }

    #[test]
    fn default_values_len_equals_dimension() {
        let f = RoutingFeatures::default();
        assert_eq!(f.values.len(), FEATURE_DIMENSION);
    }

    // -- Normalization: all values in [-1.0, 1.0] --

    #[test]
    fn all_values_in_valid_range() {
        let task = TaskProfile {
            streaming: true,
            has_tools: true,
            has_vision: true,
            context_tokens: 100_000,
            estimated_output_tokens: 50_000,
            complexity: Complexity::Complex,
            task_type: TaskType::Analysis,
            ..Default::default()
        };
        let mut obs = crate::observation::RuntimeObservation::default();
        obs.record_success(500.0, Some(100.0));

        let mut stats = ProviderModelStats::new("m".into(), "p".into());
        for _ in 0..80 {
            stats.record_success(200.0, Some(50.0));
        }
        for _ in 0..20 {
            stats.record_failure(FailureClass::Timeout);
        }

        let ctx = FeatureContext {
            task: Some(&task),
            message_count: Some(25),
            tier: Some(ModelTier::Reasoning),
            capabilities: Some(&ModelCapabilities {
                vision: true,
                tools: true,
                thinking: true,
                structured_output: false,
                audio: false,
                video: false,
                files: true,
            }),
            priority: 10,
            observation: Some(&obs),
            stats: Some(&stats),
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        for (i, &v) in f.values.iter().enumerate() {
            assert!(
                (-1.0..=1.0).contains(&v),
                "feature {i} out of range: {v}"
            );
        }
    }

    // -- Serde round-trip --

    #[test]
    fn serde_round_trip() {
        let f = RoutingFeatures {
            values: {
                let mut v = [0.5f32; FEATURE_DIMENSION];
                v[0] = 1.0;
                v[1] = 0.0;
                v[FEATURE_DIMENSION - 1] = UNKNOWN;
                v
            },
            schema_version: FEATURE_SCHEMA_VERSION,
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: RoutingFeatures = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn serde_preserves_unknown_sentinel() {
        let f = RoutingFeatures::default();
        let json = serde_json::to_string(&f).unwrap();
        let back: RoutingFeatures = serde_json::from_str(&json).unwrap();
        for &v in &back.values {
            assert_eq!(v, UNKNOWN);
        }
    }

    // -- Schema version --

    #[test]
    fn schema_version_matches_constant() {
        let f = RoutingFeatures::default();
        assert_eq!(f.schema_version, FEATURE_SCHEMA_VERSION);
    }

    #[test]
    fn schema_version_is_one() {
        assert_eq!(FEATURE_SCHEMA_VERSION, 1);
    }

    // -- Performance: 10,000 extractions in <100ms --

    #[test]
    fn performance_10k_extractions() {
        let task = TaskProfile {
            streaming: true,
            has_tools: true,
            has_vision: false,
            context_tokens: 50_000,
            estimated_output_tokens: 4096,
            complexity: Complexity::Standard,
            task_type: TaskType::ToolUse,
            ..Default::default()
        };
        let caps = ModelCapabilities {
            tools: true,
            thinking: true,
            ..Default::default()
        };
        let mut stats = ProviderModelStats::new("m".into(), "p".into());
        stats.record_success(200.0, Some(50.0));

        let ctx = FeatureContext {
            task: Some(&task),
            message_count: Some(10),
            tier: Some(ModelTier::Standard),
            capabilities: Some(&caps),
            priority: 50,
            observation: None,
            stats: Some(&stats),
            #[cfg(feature = "account")]
            account: None,
        };

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            std::hint::black_box(extract_features(&ctx));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "10,000 extractions took {}ms (limit: 100ms)",
            elapsed.as_millis()
        );
    }

    // -- Individual feature correctness --

    #[test]
    fn streaming_feature_true() {
        let task = TaskProfile {
            streaming: true,
            ..Default::default()
        };
        let ctx = minimal_ctx_with_task(&task);
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_STREAMING], 1.0);
    }

    #[test]
    fn streaming_feature_false() {
        let task = TaskProfile {
            streaming: false,
            ..Default::default()
        };
        let ctx = minimal_ctx_with_task(&task);
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_STREAMING], 0.0);
    }

    #[test]
    fn tools_feature() {
        let task = TaskProfile {
            has_tools: true,
            ..Default::default()
        };
        let ctx = minimal_ctx_with_task(&task);
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_HAS_TOOLS], 1.0);
    }

    #[test]
    fn vision_feature() {
        let task = TaskProfile {
            has_vision: true,
            ..Default::default()
        };
        let ctx = minimal_ctx_with_task(&task);
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_HAS_VISION], 1.0);
    }

    #[test]
    fn context_tokens_normalization() {
        let task = TaskProfile {
            context_tokens: 1024, // 2^10 -> 10/20 = 0.5
            ..Default::default()
        };
        let ctx = minimal_ctx_with_task(&task);
        let f = extract_features(&ctx);
        assert!((f.values[F_CONTEXT_TOKENS] - 0.5).abs() < 0.01);
    }

    #[test]
    fn context_tokens_zero() {
        let task = TaskProfile {
            context_tokens: 0,
            ..Default::default()
        };
        let ctx = minimal_ctx_with_task(&task);
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_CONTEXT_TOKENS], 0.0);
    }

    #[test]
    fn complexity_all_variants() {
        for (c, expected) in [
            (Complexity::Simple, 0.0),
            (Complexity::Standard, 0.33),
            (Complexity::Complex, 0.66),
            (Complexity::Frontier, 1.0),
        ] {
            let task = TaskProfile {
                complexity: c,
                ..Default::default()
            };
            let ctx = minimal_ctx_with_task(&task);
            let f = extract_features(&ctx);
            assert!(
                (f.values[F_COMPLEXITY] - expected).abs() < 0.01,
                "complexity {c:?}: expected {expected}, got {}",
                f.values[F_COMPLEXITY]
            );
        }
    }

    #[test]
    fn task_type_all_variants() {
        for (tt, expected) in [
            (TaskType::Chat, 0.0),
            (TaskType::Code, 0.2),
            (TaskType::Vision, 0.4),
            (TaskType::ToolUse, 0.6),
            (TaskType::Analysis, 0.8),
            (TaskType::Creative, 1.0),
        ] {
            let task = TaskProfile {
                task_type: tt,
                ..Default::default()
            };
            let ctx = minimal_ctx_with_task(&task);
            let f = extract_features(&ctx);
            assert!(
                (f.values[F_TASK_TYPE] - expected).abs() < 0.01,
                "task_type {tt:?}: expected {expected}, got {}",
                f.values[F_TASK_TYPE]
            );
        }
    }

    #[test]
    fn message_count_normalization() {
        let task = TaskProfile::default();
        let ctx = FeatureContext {
            task: Some(&task),
            message_count: Some(50),
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        assert!((f.values[F_MESSAGE_COUNT] - 0.5).abs() < 0.001);
    }

    #[test]
    fn message_count_capped_at_100() {
        let task = TaskProfile::default();
        let ctx = FeatureContext {
            task: Some(&task),
            message_count: Some(500),
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        assert!((f.values[F_MESSAGE_COUNT] - 1.0).abs() < 0.001);
    }

    #[test]
    fn tier_all_variants() {
        for (tier, expected) in [
            (ModelTier::Fast, 0.0),
            (ModelTier::Standard, 0.33),
            (ModelTier::Reasoning, 0.66),
            (ModelTier::Frontier, 1.0),
        ] {
            let ctx = FeatureContext {
                task: None,
                message_count: None,
                tier: Some(tier),
                capabilities: None,
                priority: 0,
                observation: None,
                stats: None,
                #[cfg(feature = "account")]
                account: None,
            };
            let f = extract_features(&ctx);
            assert!(
                (f.values[F_TIER] - expected).abs() < 0.01,
                "tier {tier:?}: expected {expected}, got {}",
                f.values[F_TIER]
            );
        }
    }

    #[test]
    fn capabilities_mapping() {
        let caps = ModelCapabilities {
            vision: true,
            tools: false,
            thinking: true,
            structured_output: false,
            audio: true,
            video: false,
            files: true,
        };
        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: Some(&caps),
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_CAP_VISION], 1.0);
        assert_eq!(f.values[F_CAP_TOOLS], 0.0);
        assert_eq!(f.values[F_CAP_THINKING], 1.0);
        assert_eq!(f.values[F_CAP_STRUCTURED], 0.0);
        assert_eq!(f.values[F_CAP_AUDIO], 1.0);
        assert_eq!(f.values[F_CAP_VIDEO], 0.0);
        assert_eq!(f.values[F_CAP_FILES], 1.0);
    }

    #[test]
    fn priority_normalization() {
        // priority=0 -> 1.0 - 0 = 1.0
        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_PRIORITY], 1.0);

        // priority=50 -> 1.0 - 0.5 = 0.5
        let ctx = FeatureContext {
            priority: 50,
            ..minimal_ctx()
        };
        let f = extract_features(&ctx);
        assert!((f.values[F_PRIORITY] - 0.5).abs() < 0.01);

        // priority=100 -> 1.0 - 1.0 = 0.0
        let ctx = FeatureContext {
            priority: 100,
            ..minimal_ctx()
        };
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_PRIORITY], 0.0);
    }

    #[test]
    fn observation_health_score() {
        let mut obs = crate::observation::RuntimeObservation::default();
        obs.health.state = HealthState::Healthy;
        obs.health.success_rate = Signal::new(0.95);

        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: Some(&obs),
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        assert!((f.values[F_OBS_HEALTH] - 0.95).abs() < 0.01);
    }

    #[test]
    fn observation_latency_features() {
        let mut obs = crate::observation::RuntimeObservation::default();
        obs.record_success(1000.0, Some(200.0));

        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: Some(&obs),
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        // 1000ms / 5000ms baseline = 0.2, inverse = 0.8
        assert!((f.values[F_OBS_LATENCY_EWMA] - 0.8).abs() < 0.01);
        // TTFT: 200ms / 2000ms = 0.1, inverse = 0.9
        assert!((f.values[F_OBS_TTFT_EWMA] - 0.9).abs() < 0.01);
    }

    #[test]
    fn observation_freshness_all_variants() {
        for (fresh, expected) in [
            (ObservationFreshness::Fresh, 1.0),
            (ObservationFreshness::Recent, 0.75),
            (ObservationFreshness::Stale, 0.5),
            (ObservationFreshness::Unknown, 0.25),
        ] {
            let mut obs = crate::observation::RuntimeObservation::default();
            obs.freshness = fresh;

            let ctx = FeatureContext {
                task: None,
                message_count: None,
                tier: None,
                capabilities: None,
                priority: 0,
                observation: Some(&obs),
                stats: None,
                #[cfg(feature = "account")]
                account: None,
            };
            let f = extract_features(&ctx);
            assert!(
                (f.values[F_OBS_FRESHNESS] - expected).abs() < 0.01,
                "freshness {fresh:?}: expected {expected}, got {}",
                f.values[F_OBS_FRESHNESS]
            );
        }
    }

    #[test]
    fn stats_features() {
        let mut stats = ProviderModelStats::new("m".into(), "p".into());
        // 8 successes, 2 timeouts, 1 ratelimit = 11 total
        for _ in 0..8 {
            stats.record_success(200.0, Some(50.0));
        }
        stats.record_failure(FailureClass::Timeout);
        stats.record_failure(FailureClass::Timeout);
        stats.record_failure(FailureClass::RateLimit);

        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: Some(&stats),
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);

        // total_requests = 11, log2(11+1) / 20 = log2(12) / 20 ~ 3.58 / 20 ~ 0.179
        assert!(f.values[F_STAT_TOTAL_REQUESTS] > 0.15);
        assert!(f.values[F_STAT_TOTAL_REQUESTS] < 0.20);

        // failure_rate = 3/11 ~ 0.273
        assert!((f.values[F_STAT_FAILURE_RATE] - 3.0 / 11.0).abs() < 0.01);

        // timeout_rate = 2/11 ~ 0.182
        assert!((f.values[F_STAT_TIMEOUT_RATE] - 2.0 / 11.0).abs() < 0.01);

        // ratelimit_rate = 1/11 ~ 0.091
        assert!((f.values[F_STAT_RATELIMIT_RATE] - 1.0 / 11.0).abs() < 0.01);
    }

    #[test]
    fn stats_zero_requests_stays_unknown() {
        let stats = ProviderModelStats::new("m".into(), "p".into());
        let ctx = FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: Some(&stats),
            #[cfg(feature = "account")]
            account: None,
        };
        let f = extract_features(&ctx);
        assert_eq!(f.values[F_STAT_TOTAL_REQUESTS], 0.0);
        assert_eq!(f.values[F_STAT_FAILURE_RATE], UNKNOWN);
        assert_eq!(f.values[F_STAT_TIMEOUT_RATE], UNKNOWN);
        assert_eq!(f.values[F_STAT_RATELIMIT_RATE], UNKNOWN);
    }

    #[test]
    fn normalize_log2_values() {
        assert_eq!(normalize_log2(0), 0.0);
        // 2^10 = 1024 -> 10/20 = 0.5
        assert!((normalize_log2(1024) - 0.5).abs() < 0.01);
        // 2^20 = 1048576 -> 20/20 = 1.0 (clamped)
        assert!((normalize_log2(1_048_576) - 1.0).abs() < 0.01);
        // Very large value clamps to 1.0
        assert_eq!(normalize_log2(u64::MAX), 1.0);
    }

    #[test]
    fn feature_indices_are_sequential_0_to_31() {
        // Verify no index collisions by collecting all public const indices.
        let indices = [
            F_STREAMING,
            F_HAS_TOOLS,
            F_HAS_VISION,
            F_CONTEXT_TOKENS,
            F_EST_OUTPUT_TOKENS,
            F_COMPLEXITY,
            F_TASK_TYPE,
            F_MESSAGE_COUNT,
            F_TIER,
            F_CAP_VISION,
            F_CAP_TOOLS,
            F_CAP_THINKING,
            F_CAP_STRUCTURED,
            F_CAP_AUDIO,
            F_CAP_VIDEO,
            F_CAP_FILES,
            F_PRIORITY,
            F_OBS_HEALTH,
            F_OBS_LATENCY_EWMA,
            F_OBS_LATENCY_P50,
            F_OBS_LATENCY_P95,
            F_OBS_TTFT_EWMA,
            F_OBS_SUCCESS_RATE,
            F_OBS_FRESHNESS,
            F_STAT_TOTAL_REQUESTS,
            F_STAT_FAILURE_RATE,
            F_STAT_TIMEOUT_RATE,
            F_STAT_RATELIMIT_RATE,
            F_ACCT_QUOTA_REMAINING,
            F_ACCT_RATE_PRESSURE,
            F_ACCT_HEALTH,
            F_RESERVE_31,
        ];
        let mut sorted = indices.to_vec();
        sorted.sort();
        let expected: Vec<usize> = (0..32).collect();
        assert_eq!(sorted, expected, "feature indices must cover 0..31 exactly once");
    }

    // -- Helpers --

    fn minimal_ctx<'a>() -> FeatureContext<'a> {
        FeatureContext {
            task: None,
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        }
    }

    fn minimal_ctx_with_task<'a>(task: &'a TaskProfile) -> FeatureContext<'a> {
        FeatureContext {
            task: Some(task),
            message_count: None,
            tier: None,
            capabilities: None,
            priority: 0,
            observation: None,
            stats: None,
            #[cfg(feature = "account")]
            account: None,
        }
    }
}
