//! Account core types.
//!
//! Account represents a provider account with its identity, runtime state,
//! quota, usage, and rate-limit information. Account is always optional in
//! routing — Provider+Model works without it.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Stable account identifier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId(pub String);

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Account lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Suspended,
    QuotaExhausted,
    RateLimited,
    AuthenticationExpired,
    Unknown,
}

impl Default for AccountStatus {
    fn default() -> Self { Self::Unknown }
}

/// What operations an account provider supports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountCapabilities {
    pub supports_usage: bool,
    pub supports_quota: bool,
    pub supports_refresh: bool,
    pub supports_checkin: bool,
    pub supports_health_check: bool,
}

/// Quota information for an account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountQuota {
    /// Total quota (e.g. total credits, total tokens).
    pub total: f64,
    /// Used quota.
    pub used: f64,
    /// Remaining quota.
    pub remaining: f64,
    /// Unit (e.g. "credits", "tokens", "usd").
    pub unit: String,
    /// When the quota resets (unix timestamp, None if no reset).
    pub resets_at: Option<i64>,
}

impl AccountQuota {
    pub fn utilization(&self) -> f64 {
        if self.total <= 0.0 { 0.0 } else { self.used / self.total }
    }
}

/// Usage statistics for an account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountUsage {
    pub total_requests: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub currency: String,
    /// Period start (unix timestamp).
    pub period_start: Option<i64>,
    /// Period end (unix timestamp).
    pub period_end: Option<i64>,
}

/// Rate limit state for an account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitState {
    /// Requests per minute limit.
    pub rpm_limit: Option<u32>,
    /// Requests per minute used in current window.
    pub rpm_used: Option<u32>,
    /// Tokens per minute limit.
    pub tpm_limit: Option<u64>,
    /// Tokens per minute used in current window.
    pub tpm_used: Option<u64>,
    /// Concurrent request limit.
    pub concurrency_limit: Option<u32>,
    /// Active concurrent requests.
    pub concurrency_used: Option<u32>,
    /// When the current rate limit window resets (unix timestamp).
    pub resets_at: Option<i64>,
}

impl RateLimitState {
    /// How much rate limit pressure (0.0 = empty, 1.0 = at limit).
    pub fn pressure(&self) -> f64 {
        let rpm_pressure = match (self.rpm_used, self.rpm_limit) {
            (Some(used), Some(limit)) if limit > 0 => used as f64 / limit as f64,
            _ => 0.0,
        };
        let tpm_pressure = match (self.tpm_used, self.tpm_limit) {
            (Some(used), Some(limit)) if limit > 0 => used as f64 / limit as f64,
            _ => 0.0,
        };
        rpm_pressure.max(tpm_pressure)
    }
}

/// Complete runtime state of an account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountRuntime {
    pub account_id: AccountId,
    pub provider_id: String,
    pub status: AccountStatus,
    pub capabilities: AccountCapabilities,
    pub quota: Option<AccountQuota>,
    pub usage: Option<AccountUsage>,
    pub rate_limit: Option<RateLimitState>,
    pub last_success: Option<i64>,
    pub last_failure: Option<i64>,
    pub last_sync: Option<i64>,
    /// Opaque metadata from the provider adapter.
    pub metadata: HashMap<String, String>,
}
