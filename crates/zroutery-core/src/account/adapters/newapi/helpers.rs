//! The small pure functions the calls share: query building, error classification, quota conversion, credential validation.

use super::checkin;
use super::checkin::*;
use super::logs::*;
use super::wire::*;
use super::{PROVIDER_ID, REFRESH_COOKIE_NAME, USER_STATUS_DISABLED, USER_STATUS_ENABLED};

use serde_json::Value;

use crate::account::types::*;
use crate::error::{Error, Result};

/// `query` for the log endpoints, which filter on `[start, end]` unix seconds
/// and on a single log type.
///
/// Every log read passes an explicit type. `GetUserLogs` reads
/// `LogTypeUnknown` as *no type filter*, so omitting `type` returns every kind
/// mixed together — which is how a consumption row ends up inside a window meant
/// for system events, and how a check-in confirmation starts "explaining" itself
/// with unrelated spend. The caller names the type it means.
pub(super) fn log_query(log_type: i64, start: i64, end: i64) -> Vec<(&'static str, String)> {
    vec![
        ("type", log_type.to_string()),
        ("start_timestamp", start.to_string()),
        ("end_timestamp", end.to_string()),
    ]
}

/// The log query for consumption rows, which is what balance and token
/// statistics are about.
pub(super) fn window_query(start: i64, end: i64) -> Vec<(&'static str, String)> {
    log_query(LOG_TYPE_CONSUME, start, end)
}

/// Classify a panel failure into a check-in attempt outcome.
///
/// Split out because the mapping is the part of check-in that decides what a
/// user is told, and because it is easy to get wrong in the direction that
/// retries: an `Unauthorized` is a credential problem, and reporting it as a
/// plain failure would have a scheduler retrying forever against a credential
/// that cannot work.
pub(super) fn classify_failure(error: &Error) -> CheckinAttempt {
    match error {
        Error::Unauthorized | Error::MissingApiKey(_) => CheckinAttempt::AuthenticationExpired,
        Error::Transport { .. } => CheckinAttempt::NetworkFailure {
            detail: error.to_string(),
        },
        Error::Upstream { body, .. } => {
            // The panel answers business failures with HTTP 200 and
            // `success: false`, so a refusal arrives as `Upstream` with the
            // message in the body. `UserCheckin` returns `今日已签到` for a
            // second check-in in one day, so that wording is recognised — but
            // only as a corroborating signal, and only here where the
            // structured `checked_in_today` has already been considered by the
            // caller.
            let (code, message) = split_upstream_body(body);
            if checkin::message_indicates_already_completed(&message) {
                CheckinAttempt::AlreadyCompleted { reward_quota: None }
            } else {
                CheckinAttempt::Rejected { code, message }
            }
        }
        Error::BadUpstreamPayload(message) => CheckinAttempt::Unknown {
            detail: message.clone(),
        },
        other => CheckinAttempt::Unknown {
            detail: other.to_string(),
        },
    }
}

/// Split an `Error::Upstream` body back into its `code` and `message` halves.
///
/// `attempt` joins them as `code: message`, so recovering the two is a matter of
/// the same convention read backwards. A body with no leading code is kept whole
/// as the message, because the panel's wording is the only part a user can act on.
pub(super) fn split_upstream_body(body: &str) -> (Option<String>, String) {
    match body.split_once(": ") {
        Some((code, message)) if !code.is_empty() && !code.contains(' ') => {
            (Some(code.to_string()), message.to_string())
        }
        _ => (None, body.to_string()),
    }
}

pub(super) fn map_user_status(status: i64) -> AccountStatus {
    match status {
        USER_STATUS_ENABLED => AccountStatus::Active,
        USER_STATUS_DISABLED => AccountStatus::Suspended,
        _ => AccountStatus::Unknown,
    }
}

pub(super) fn user_id_of(user: &SelfData) -> Option<String> {
    (user.id > 0).then(|| user.id.to_string())
}

pub(super) fn username_of(user: &SelfData) -> Option<String> {
    (!user.username.is_empty()).then(|| user.username.clone())
}

/// Credit units to US dollars, using the instance's `quota_per_unit`.
pub(super) fn quota_to_usd(quota: i64, quota_per_unit: f64) -> f64 {
    if quota_per_unit <= 0.0 || !quota_per_unit.is_finite() {
        return 0.0;
    }
    quota as f64 / quota_per_unit
}

/// The panel tracks a remaining balance plus lifetime consumption; the granted
/// total is their sum, which top-ups and redemptions can drift from.
pub(super) fn quota_from_self(user: &SelfData, quota_per_unit: f64) -> AccountQuota {
    let remaining = quota_to_usd(user.quota, quota_per_unit);
    let used = quota_to_usd(user.used_quota, quota_per_unit);
    AccountQuota {
        total: remaining + used,
        used,
        remaining,
        unit: "USD".into(),
        resets_at: None,
    }
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(value: Value, endpoint: &str) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| Error::BadUpstreamPayload(format!("{PROVIDER_ID} {endpoint} payload: {e}")))
}

pub(super) fn header_value(
    raw: &str,
) -> std::result::Result<reqwest::header::HeaderValue, NewApiError> {
    reqwest::header::HeaderValue::from_str(raw).map_err(|_| {
        NewApiError::Invalid(
            "newapi credential contains characters that are not valid in a header".into(),
        )
    })
}

/// Pull the rotated `new_api_refresh` cookie out of a response, if any.
pub(super) fn response_refresh_cookie(response: &reqwest::Response) -> Option<String> {
    let prefix = format!("{REFRESH_COOKIE_NAME}=");
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let rest = value.strip_prefix(&prefix)?;
            let cookie = rest.split(';').next()?.trim();
            (!cookie.is_empty()).then(|| cookie.to_string())
        })
}

/// Trim, drop a pasted `Bearer ` prefix, and check the result can ride in a
/// header.
pub(super) fn strip_bearer_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim()
}

pub(super) fn validate_secret(what: &str, value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err(format!("empty {what}"));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{what} must not contain whitespace or control characters"
        ));
    }
    Ok(())
}

/// Accept both a bare cookie value and a pasted `new_api_refresh=<value>`.
pub(super) fn cookie_value(raw: &str) -> String {
    let trimmed = raw.trim();
    let prefix = format!("{REFRESH_COOKIE_NAME}=");
    trimmed
        .strip_prefix(&prefix)
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

pub(super) fn validate_cookie(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("empty session cookie".into());
    }
    if value.chars().any(|c| c == ';' || c.is_control()) {
        return Err("session cookie must be a single cookie value".into());
    }
    Ok(())
}

/// Normalise a panel address into a bare origin + path prefix.
///
/// Operators paste three shapes: the panel root (`https://host`), the API root
/// (`https://host/api`) and the relay base the provider config uses
/// (`https://host/v1`). All three describe the same instance, so the `/api` and
/// `/v1` suffixes are dropped here and the adapter owns the paths it calls.
pub(super) fn normalize_base_url(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("base_url is empty".into());
    }
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|e| format!("base_url is not a valid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "base_url must start with http:// or https://, not `{other}://`"
            ))
        }
    }
    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "base_url has no host".to_string())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("base_url must not embed credentials".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("base_url must not carry a query string or fragment".into());
    }

    let mut path = parsed.path().trim_end_matches('/').to_string();
    if path.ends_with("/v1") {
        path.truncate(path.len() - "/v1".len());
    }
    if path.ends_with("/api") {
        path.truncate(path.len() - "/api".len());
    }
    let path = path.trim_end_matches('/');

    let mut base = format!("{}://{host}", parsed.scheme());
    if let Some(port) = parsed.port() {
        base.push_str(&format!(":{port}"));
    }
    base.push_str(path);
    Ok(base)
}
