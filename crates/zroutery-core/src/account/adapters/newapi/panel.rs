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
    /// tokens for each group the model is available to.
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
        let published_groups = attempt
            .group_ratio
            .as_ref()
            .or_else(|| data.get("group_ratio"));
        let groups: Vec<(String, f64)> = match published_groups {
            Some(Value::Object(map)) => map
                .iter()
                .filter_map(|(group, ratio)| {
                    number(Some(ratio)).map(|ratio| (group.clone(), ratio))
                })
                .collect(),
            None | Some(Value::Null) => vec![("default".into(), 1.0)],
            Some(_) => {
                return Err(NewApiError::Payload(
                    "newapi /api/pricing group_ratio is not an object".into(),
                )
                .into_core())
            }
        };
        let rows = data
            .as_array()
            .or_else(|| data.get("data").and_then(Value::as_array))
            .ok_or_else(|| {
                NewApiError::Payload("newapi /api/pricing model list is missing".into()).into_core()
            })?;
        let mut prices = Vec::new();
        for row in rows {
            let Some(model) = row
                .get("model_name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
            else {
                continue;
            };
            let quota_type = row
                .get("quota_type")
                .map_or(Some(0.0), |value| number(Some(value)));
            for (group, group_ratio) in &groups {
                if row
                    .get("enable_groups")
                    .and_then(Value::as_array)
                    .is_some_and(|enabled| {
                        !enabled.is_empty()
                            && !enabled
                                .iter()
                                .any(|value| value.as_str() == Some(group.as_str()))
                    })
                {
                    continue;
                }
                let (input, per_request) = match quota_type {
                    Some(0.0) => (
                        number(row.get("model_ratio")).and_then(|ratio| {
                            finite_price(ratio * group_ratio * 1_000_000.0 / unit)
                        }),
                        None,
                    ),
                    Some(1.0) => (
                        None,
                        number(row.get("model_price"))
                            .and_then(|price| finite_price(price * group_ratio)),
                    ),
                    _ => continue,
                };
                if input.is_none() && per_request.is_none() {
                    continue;
                }
                let token_price = |field: &str| {
                    input.and_then(|input| {
                        number(row.get(field)).and_then(|ratio| finite_price(input * ratio))
                    })
                };
                prices.push(NewApiPrice {
                    model: model.to_string(),
                    group: group.clone(),
                    input_per_mtok: input,
                    output_per_mtok: token_price("completion_ratio"),
                    cache_read_per_mtok: token_price("cache_ratio"),
                    cache_write_per_mtok: token_price("cache_creation_ratio"),
                    per_request,
                });
            }
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

fn finite_price(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
        .and_then(finite_price)
}
