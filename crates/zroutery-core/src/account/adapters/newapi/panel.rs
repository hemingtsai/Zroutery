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

    /// Model prices as the panel publishes them, converted to USD per million
    /// tokens for the panel's default group.
    ///
    /// `GET /api/pricing` is public; it needs no credential. The formula is the
    /// panel's own: `ratio * group_ratio * 1_000_000 / quota_per_unit`, with the
    /// completion side multiplied by `completion_ratio`.
    pub async fn probe_pricing(&self) -> Result<Vec<NewApiPrice>> {
        let status = self.fetch_status().await?;
        let unit = self.effective_quota_per_unit(&status);
        let attempt = self
            .attempt(reqwest::Method::GET, "/api/pricing", &[], None, None)
            .await
            .map_err(NewApiError::into_core)?;
        let data = attempt.data;
        let number = |value: Option<&Value>| -> Option<f64> {
            let value = value?;
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
                .filter(|n| n.is_finite())
        };
        let groups = data.get("group_ratio").and_then(Value::as_object);
        let (group, group_ratio) = groups
            .and_then(|map| {
                map.get("default")
                    .or_else(|| map.values().next())
                    .and_then(|value| value.as_f64())
                    .map(|ratio| ("default".to_string(), ratio))
            })
            .unwrap_or_else(|| ("default".into(), 1.0));
        let rows = data
            .as_array()
            .or_else(|| data.get("data").and_then(Value::as_array))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut prices = Vec::new();
        for row in rows {
            let Some(model) = row
                .get("model_name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
            else {
                continue;
            };
            let quota_type = number(row.get("quota_type")).unwrap_or(0.0);
            let (input, output, per_request) = if quota_type == 1.0 {
                (
                    None,
                    None,
                    number(row.get("model_price")).map(|price| price * group_ratio),
                )
            } else {
                let input = number(row.get("model_ratio"))
                    .map(|ratio| ratio * group_ratio * 1_000_000.0 / unit)
                    .filter(|value| value.is_finite() && *value >= 0.0);
                let output = input.and_then(|input| {
                    number(row.get("completion_ratio"))
                        .map(|ratio| input * ratio)
                        .filter(|value| value.is_finite() && *value >= 0.0)
                });
                (input, output, None)
            };
            if input.is_none() && per_request.is_none() {
                continue;
            }
            prices.push(NewApiPrice {
                model: model.to_string(),
                group: group.clone(),
                input_per_mtok: input,
                output_per_mtok: output,
                per_request,
            });
        }
        Ok(prices)
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
