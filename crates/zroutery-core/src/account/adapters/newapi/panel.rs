//! Reading the panel: the account itself, the rate limits, and the metadata the runtime carries forward.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::Value;

use crate::account::types::*;
use crate::error::Result;

use super::helpers::*;
use super::types::*;
use super::wire::*;
use super::NewApiAdapter;
use super::{PROVIDER_ID, STATUS_CACHE_TTL, USER_STATUS_DISABLED, USER_STATUS_ENABLED};

impl NewApiAdapter {
    /// `GET /api/status`, always fresh.
    pub(super) async fn fetch_status(&self) -> Result<NewApiStatus> {
        let attempt = self
            .attempt(reqwest::Method::GET, "/api/status", &[], None, None)
            .await
            .map_err(NewApiError::into_core)?;
        decode(attempt.data, "/api/status")
    }

    /// `GET /api/status`, memoised for [`STATUS_CACHE_TTL`].
    pub(super) async fn status_cached(&self) -> Result<NewApiStatus> {
        if let Some((at, status)) = self
            .status_cache
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
        {
            if at.elapsed() < STATUS_CACHE_TTL {
                return Ok(status);
            }
        }
        let status = self.fetch_status().await?;
        *self
            .status_cache
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some((Instant::now(), status.clone()));
        Ok(status)
    }

    /// `GET /api/user/self`, keeping the panel's error code so callers can tell
    /// "this user is disabled" from "this token expired".
    pub(super) async fn self_probe(&self) -> std::result::Result<SelfData, NewApiError> {
        let value = self
            .request_value(reqwest::Method::GET, "/api/user/self", &[])
            .await?;
        serde_json::from_value(value)
            .map_err(|e| NewApiError::Payload(format!("{PROVIDER_ID} /api/user/self payload: {e}")))
    }

    /// `GET /api/user/self`, with the panel failure projected onto [`Error`].
    pub(super) async fn self_snapshot(&self) -> Result<SelfData> {
        self.self_probe().await.map_err(NewApiError::into_core)
    }

    /// Whether a zero wallet is still funded by an active subscription plan.
    ///
    /// Instances old enough to lack `/api/subscription/self` answer 404, which
    /// this reports as "no subscription": the wallet is then the only funding
    /// source, which is what the caller is trying to decide.
    pub(super) async fn has_active_subscription(&self) -> Result<bool> {
        let data = self
            .request_json(reqwest::Method::GET, "/api/subscription/self", &[])
            .await?;
        let active = data
            .get("subscriptions")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        Ok(active > 0)
    }

    /// `GET /api/log/self/stat` over the last minute, for `rpm`/`tpm` usage.
    pub(super) async fn rate_limit_probe(&self) -> Result<RateLimitState> {
        let end = chrono::Utc::now().timestamp();
        let query = window_query(end - 60, end);
        let stat: LogStatData = self
            .request_typed(reqwest::Method::GET, "/api/log/self/stat", &query)
            .await?;
        Ok(RateLimitState {
            rpm_used: u32::try_from(stat.rpm).ok().filter(|v| *v > 0),
            tpm_used: u64::try_from(stat.tpm).ok().filter(|v| *v > 0),
            ..Default::default()
        })
    }

    /// Map the panel's user record onto an [`AccountStatus`].
    ///
    /// A zero wallet is only "quota exhausted" when nothing else funds the
    /// account: self-use mode ignores balances, and a subscription can serve a
    /// wallet that is empty.
    pub(super) async fn effective_status(
        &self,
        status: &NewApiStatus,
        user: &SelfData,
    ) -> AccountStatus {
        match user.status {
            USER_STATUS_ENABLED => {
                if user.quota > 0 || status.self_use_mode_enabled.unwrap_or(false) {
                    return AccountStatus::Active;
                }
                if self.has_active_subscription().await.unwrap_or(false) {
                    AccountStatus::Active
                } else {
                    AccountStatus::QuotaExhausted
                }
            }
            USER_STATUS_DISABLED => AccountStatus::Suspended,
            _ => AccountStatus::Unknown,
        }
    }

    pub(super) fn effective_quota_per_unit(&self, status: &NewApiStatus) -> f64 {
        self.config
            .quota_per_unit
            .unwrap_or_else(|| status.quota_per_unit_or_default())
    }

    /// The `newapi.*` metadata a caller can rely on alongside the typed fields.
    pub(super) fn metadata(
        &self,
        status: &NewApiStatus,
        user: &SelfData,
        quota_per_unit: f64,
    ) -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        if user.id > 0 {
            metadata.insert("newapi.user_id".into(), user.id.to_string());
        }
        if !user.username.is_empty() {
            metadata.insert("newapi.username".into(), user.username.clone());
        }
        if let Some(name) = user.display_name.as_deref().filter(|n| !n.is_empty()) {
            metadata.insert("newapi.display_name".into(), name.to_string());
        }
        if let Some(group) = user.group.as_deref().filter(|g| !g.is_empty()) {
            metadata.insert("newapi.group".into(), group.to_string());
        }
        metadata.insert("newapi.quota_raw".into(), user.quota.to_string());
        metadata.insert("newapi.used_quota_raw".into(), user.used_quota.to_string());
        metadata.insert(
            "newapi.request_count".into(),
            user.request_count.to_string(),
        );
        metadata.insert("newapi.quota_per_unit".into(), quota_per_unit.to_string());
        if let Some(version) = &status.version {
            metadata.insert("newapi.version".into(), version.clone());
        }
        if let Some(name) = &status.system_name {
            metadata.insert("newapi.system_name".into(), name.clone());
        }
        if let Some(enabled) = status.checkin_enabled {
            metadata.insert("newapi.checkin_enabled".into(), enabled.to_string());
        }
        if let Some(required) = status.turnstile_check {
            metadata.insert("newapi.turnstile_check".into(), required.to_string());
        }
        if let Some(self_use) = status.self_use_mode_enabled {
            metadata.insert("newapi.self_use_mode".into(), self_use.to_string());
        }
        if let Some(currency) = status.display_in_currency {
            metadata.insert("newapi.display_in_currency".into(), currency.to_string());
        }
        metadata
    }
}
