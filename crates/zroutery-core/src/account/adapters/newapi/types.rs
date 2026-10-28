//! What a caller configures and what it gets back: the credential shape, the panel status, and the usage report.

use serde::{Deserialize, Serialize};

use super::helpers::*;
use super::DEFAULT_QUOTA_PER_UNIT;
use crate::account::types::*;

/// Authentication method for NewAPI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NewApiAuth {
    /// Refresh cookie (`new_api_refresh`) from a dashboard login. Exchanged for
    /// a short-lived access token, and kept so expired tokens can be renewed.
    Cookie { session_cookie: String },
    /// Personal access token from `GET /api/user/token`, sent verbatim in the
    /// Authorization header.
    ApiKey { key: String },
    /// A dashboard access token (login JWT) plus, optionally, the refresh
    /// cookie value that can renew it.
    OAuth2 {
        access_token: String,
        refresh_token: Option<String>,
    },
}

/// An authenticated session with NewAPI.
#[derive(Debug, Clone)]
pub struct AuthenticatedSession {
    pub auth: NewApiAuth,
    /// NewAPI user id, once the credential has been accepted.
    pub user_id: Option<String>,
    /// Login name the instance reports for that credential.
    pub username: Option<String>,
    pub authenticated_at: i64,
    /// Expiry of the access token, when the instance reports one. Personal
    /// access tokens do not expire.
    pub expires_at: Option<i64>,
}

/// NewAPI-specific configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct NewApiConfig {
    /// Panel root (`https://newapi.example.com`). A trailing `/api` or `/v1`
    /// segment is tolerated because that is what operators paste from the relay
    /// endpoint; see [`NewApiConfig::validate`].
    pub base_url: String,
    /// Credential used before [`NewApiAdapter::authenticate`] is called: a
    /// personal access token, or a login JWT. Verified lazily by the first
    /// authenticated call. A relay key (`sk-…`) is rejected here: it
    /// authenticates the `/v1` relay endpoints, which no dashboard endpoint
    /// accepts, and sending one would only ever produce a 401 that reads as
    /// "the token expired" when the truth is "this is the wrong kind of
    /// credential".
    pub api_key: String,
    /// Which operations this NewAPI instance supports.
    pub capabilities: AccountCapabilities,
    /// Override the `quota_per_unit` reported by `GET /api/status`.
    pub quota_per_unit: Option<f64>,
    /// The panel user id the `New-Api-User` header must name, when the panel's
    /// middleware demands one.
    ///
    /// The AgentRouter generation enforces the header on `/api/user/self`
    /// itself — a perfectly valid token answers 401 there without it, so the
    /// id cannot be learned by asking (the usual discovery call is the one
    /// being refused). The browser token capture reads it from the logged-in
    /// dashboard and persists it here; a hand-configured account may type it.
    /// Panels that never wanted the header leave it empty and behave exactly
    /// as before.
    pub new_api_user: Option<String>,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    /// Upper bound on log pages walked per usage report (`page_size` is 100, so
    /// the bound is `usage_page_limit * 100` log entries).
    pub usage_page_limit: u32,
}

impl Default for NewApiConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            capabilities: AccountCapabilities {
                supports_usage: true,
                supports_quota: true,
                supports_refresh: true,
                supports_checkin: true,
                supports_health_check: true,
            },
            quota_per_unit: None,
            new_api_user: None,
            connect_timeout_secs: 10,
            request_timeout_secs: 20,
            usage_page_limit: 10,
        }
    }
}

impl NewApiConfig {
    /// Validate the configuration before any request is attempted.
    pub fn validate(&self) -> std::result::Result<(), String> {
        normalize_base_url(&self.base_url)?;
        if self.connect_timeout_secs == 0 {
            return Err("connect_timeout_secs must be greater than zero".into());
        }
        if self.request_timeout_secs == 0 {
            return Err("request_timeout_secs must be greater than zero".into());
        }
        if self.usage_page_limit == 0 {
            return Err("usage_page_limit must be at least 1".into());
        }
        if let Some(unit) = self.quota_per_unit {
            if !unit.is_finite() || unit <= 0.0 {
                return Err("quota_per_unit must be a finite positive number".into());
            }
        }
        if !self.api_key.trim().is_empty() {
            let key = strip_bearer_prefix(&self.api_key);
            validate_secret("api_key", key)?;
            if key.starts_with("sk-") {
                return Err(
                    "the stored credential is a relay API key (`sk-…`), but the dashboard \
                     authenticates with the personal access token from the panel's personal \
                     settings page — store that token as the account credential instead"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// The API root, with any `/api` or `/v1` suffix removed.
    pub(super) fn api_root(&self) -> std::result::Result<String, String> {
        normalize_base_url(&self.base_url)
    }
}

/// The public `/api/status` payload, reduced to the fields this adapter uses.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiStatus {
    pub version: Option<String>,
    pub system_name: Option<String>,
    /// Quota units per US dollar, as the panel reports it.
    pub quota_per_unit: Option<f64>,
    pub checkin_enabled: Option<bool>,
    /// When set, `POST /api/user/checkin` needs a `turnstile` query parameter.
    pub turnstile_check: Option<bool>,
    /// Self-use mode ignores wallet balances for pricing decisions.
    pub self_use_mode_enabled: Option<bool>,
    pub display_in_currency: Option<bool>,
    pub usd_exchange_rate: Option<f64>,
    /// Operator announcements shown on the panel's landing page.
    pub announcements_enabled: Option<bool>,
    #[serde(deserialize_with = "nullable_announcements")]
    pub announcements: Vec<NewApiAnnouncement>,
}

fn nullable_announcements<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<NewApiAnnouncement>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<NewApiAnnouncement>>::deserialize(deserializer)?.unwrap_or_default())
}

/// One operator announcement from `/api/status`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewApiAnnouncement {
    pub id: Option<i64>,
    pub title: Option<String>,
    pub content: Option<String>,
    #[serde(alias = "publishDate")]
    pub publish_date: Option<serde_json::Value>,
    /// NewAPI uses this field for both numeric kinds and the strings
    /// `default`/`ongoing`, so it is kept opaque rather than guessed at.
    #[serde(rename = "type")]
    pub kind: Option<serde_json::Value>,
}

/// One model price as the panel publishes it, already converted to USD per
/// million tokens. `None` means the panel did not give enough information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewApiPrice {
    pub model: String,
    pub group: String,
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// Fixed price per request when the panel prices the model per call.
    pub per_request: Option<f64>,
}

impl NewApiStatus {
    /// `quota_per_unit` with NewAPI's own default substituted when the instance
    /// did not report a usable value.
    pub fn quota_per_unit_or_default(&self) -> f64 {
        match self.quota_per_unit {
            Some(unit) if unit.is_finite() && unit > 0.0 => unit,
            _ => DEFAULT_QUOTA_PER_UNIT,
        }
    }
}

/// Usage over one window, with the raw details NewAPI can give us.
#[derive(Debug, Clone, PartialEq)]
pub struct NewApiUsageReport {
    pub period_start: i64,
    pub period_end: i64,
    /// Consumed quota (credit units) inside the window.
    pub quota_used: i64,
    /// `quota_used` converted with the instance's `quota_per_unit`.
    pub cost_usd: f64,
    /// Consumption entries in the window, as counted by NewAPI.
    pub total_requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Requests in the last 60 seconds, when the instance reports it.
    pub rpm_used: Option<u32>,
    /// Tokens in the last 60 seconds, when the instance reports it.
    pub tpm_used: Option<u64>,
    /// True when the token totals only cover the first
    /// `usage_page_limit * 100` log entries of the window.
    pub truncated: bool,
}
