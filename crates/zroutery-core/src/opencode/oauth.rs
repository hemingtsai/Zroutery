//! OpenCode Console device-code OAuth.
//!
//! The authenticated Console tier logs in the way OpenCode's own CLI does: a
//! device-code flow against the Console yields an access token, a refresh token
//! and the billing org id the inference endpoint requires (`x-opencode-org-id`).
//! This module owns only the protocol; token persistence and provider wiring sit
//! above it.
//!
//! See `docs/development/opencode-zen.md`.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::error::{Error, Result};

/// The Console origin the device-code flow runs against.
pub const DEFAULT_SERVER: &str = "https://opencode.ai/console";

const CLIENT_ID: &str = "opencode-cli";
const DEVICE_CODE_PATH: &str = "/auth/device/code";
const DEVICE_TOKEN_PATH: &str = "/auth/device/token";
const PROVIDER_ID: &str = "opencode";

/// Persisted Console OAuth tokens.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Unix epoch milliseconds; `0` means unknown.
    #[serde(default)]
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Billing org id, sent back as `x-opencode-org-id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
}

impl Tokens {
    /// Whether the access token is expired or within `skew` of expiring.
    ///
    /// An unknown expiry (`0`) is never "needs refresh": the token is used until
    /// the gateway rejects it.
    pub fn needs_refresh(&self, skew: Duration) -> bool {
        if self.expires_at_ms <= 0 {
            return false;
        }
        let skew_ms = i64::try_from(skew.as_millis()).unwrap_or(i64::MAX);
        now_ms().saturating_add(skew_ms) >= self.expires_at_ms
    }
}

/// A started device-code login waiting for the user to authorize in a browser.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in_secs: u64,
    pub interval_secs: u64,
}

/// Result of one device-token poll.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// The user has not authorized yet; keep polling.
    Pending,
    /// The server asked us to poll more slowly.
    SlowDown,
    /// Authorization completed and tokens were issued.
    Ready(Tokens),
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn expiry_from_secs(expires_in: Option<i64>) -> i64 {
    expires_in
        .filter(|secs| *secs > 0)
        .map(|secs| now_ms().saturating_add(secs.saturating_mul(1000)))
        .unwrap_or(0)
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::internal(format!("opencode oauth client: {e}")))
}

async fn post_json(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<String> {
    let url = format!("{}{path}", server.trim_end_matches('/'));
    let response = client
        .post(&url)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|source| Error::Transport {
            provider: PROVIDER_ID.into(),
            source,
        })?;
    response.text().await.map_err(|source| Error::Transport {
        provider: PROVIDER_ID.into(),
        source,
    })
}

/// Start a Console device-code login.
pub async fn start_device_code(server: &str) -> Result<DeviceCode> {
    let client = http_client()?;
    let body = post_json(
        &client,
        server,
        DEVICE_CODE_PATH,
        json!({ "client_id": CLIENT_ID }),
    )
    .await?;
    let device: DeviceCodeResponse = serde_json::from_str(&body).map_err(|e| {
        Error::BadUpstreamPayload(format!("opencode device-code payload: {e}: {body}"))
    })?;
    Ok(DeviceCode {
        device_code: device.device_code,
        user_code: device.user_code,
        verification_url: absolute_verification_url(
            server,
            device
                .verification_uri_complete
                .as_deref()
                .or(device.verification_uri.as_deref()),
        ),
        expires_in_secs: device.expires_in.unwrap_or(600),
        interval_secs: device.interval.unwrap_or(5).max(5),
    })
}

/// Poll the device-token endpoint once.
pub async fn poll_device_token(server: &str, device_code: &str) -> Result<Poll> {
    let client = http_client()?;
    let body = post_json(
        &client,
        server,
        DEVICE_TOKEN_PATH,
        json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": device_code,
            "client_id": CLIENT_ID,
        }),
    )
    .await?;
    // The Console answers a still-pending poll with HTTP 400, so the body is
    // always parsed instead of branching on the status code.
    let token: TokenResponse = serde_json::from_str(&body).map_err(|e| {
        Error::BadUpstreamPayload(format!("opencode device-token payload: {e}: {body}"))
    })?;
    if let Some(access_token) = token.access_token {
        return Ok(Poll::Ready(Tokens {
            access_token,
            refresh_token: token.refresh_token.unwrap_or_default(),
            expires_at_ms: expiry_from_secs(token.expires_in),
            server: Some(server.to_string()),
            org_id: None,
        }));
    }
    match token.error.as_deref() {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token") => Err(Error::invalid("opencode device code expired")),
        Some("access_denied") => Err(Error::invalid("opencode device authorization denied")),
        Some(other) => {
            let detail = token
                .error_description
                .as_deref()
                .filter(|detail| !detail.trim().is_empty())
                .unwrap_or(other);
            Err(Error::invalid(format!(
                "opencode device authorization failed: {detail}"
            )))
        }
        None => Err(Error::BadUpstreamPayload(format!(
            "opencode device token returned no token: {body}"
        ))),
    }
}

/// Exchange a refresh token for a fresh access token.
pub async fn refresh_tokens(
    server: &str,
    refresh_token: &str,
    org_id: Option<String>,
) -> Result<Tokens> {
    let client = http_client()?;
    let body = post_json(
        &client,
        server,
        DEVICE_TOKEN_PATH,
        json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }),
    )
    .await?;
    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|e| Error::BadUpstreamPayload(format!("opencode token refresh payload: {e}")))?;
    let access_token = token.access_token.ok_or_else(|| {
        Error::BadUpstreamPayload("opencode refresh returned no access token".into())
    })?;
    Ok(Tokens {
        access_token,
        refresh_token: token
            .refresh_token
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| refresh_token.to_string()),
        expires_at_ms: expiry_from_secs(token.expires_in),
        server: Some(server.to_string()),
        org_id,
    })
}

/// Turn a possibly-relative verification URL into an absolute one.
fn absolute_verification_url(server: &str, value: Option<&str>) -> String {
    let fallback = format!("{}/device", server.trim_end_matches('/'));
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback;
    };
    if value.starts_with("http://") || value.starts_with("https://") {
        return value.to_string();
    }
    match reqwest::Url::parse(server)
        .ok()
        .and_then(|url| url.host_str().map(|host| (url.scheme().to_string(), host.to_string())))
    {
        Some((scheme, host)) => format!("{scheme}://{host}{value}"),
        None => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(expires_at_ms: i64) -> Tokens {
        Tokens {
            access_token: "a".into(),
            refresh_token: String::new(),
            expires_at_ms,
            server: None,
            org_id: None,
        }
    }

    #[test]
    fn expiry_is_milliseconds_and_unknown_is_zero() {
        assert_eq!(expiry_from_secs(None), 0);
        assert_eq!(expiry_from_secs(Some(0)), 0);
        assert!(expiry_from_secs(Some(600)) > now_ms());
    }

    #[test]
    fn needs_refresh_respects_skew_and_unknown_expiry() {
        assert!(!tokens(0).needs_refresh(Duration::from_secs(60)));
        assert!(tokens(now_ms() + 30_000).needs_refresh(Duration::from_secs(60)));
        assert!(!tokens(now_ms() + 3_600_000).needs_refresh(Duration::from_secs(60)));
    }

    #[test]
    fn verification_url_is_made_absolute() {
        assert_eq!(
            absolute_verification_url("https://opencode.ai/console", Some("https://example.com/verify")),
            "https://example.com/verify"
        );
        assert_eq!(
            absolute_verification_url("https://opencode.ai/console", None),
            "https://opencode.ai/console/device"
        );
        assert!(
            absolute_verification_url(
                "https://opencode.ai/console",
                Some("/console/device?user_code=ABCD")
            )
            .ends_with("/console/device?user_code=ABCD")
        );
    }

    #[tokio::test]
    async fn device_code_login_and_refresh_against_a_fake_console() {
        use axum::routing::post;
        use axum::{Json, Router};
        use serde_json::Value;
        use tokio::net::TcpListener;

        async fn device_code() -> Json<Value> {
            Json(json!({
                "device_code": "device-1",
                "user_code": "ABCD",
                "verification_uri": "/console/device?user_code=ABCD",
                "expires_in": 600,
                "interval": 5
            }))
        }
        async fn token(Json(body): Json<Value>) -> Json<Value> {
            if body["grant_type"].as_str() == Some("refresh_token") {
                Json(json!({"access_token": "acc2", "refresh_token": "ref2", "expires_in": 3600}))
            } else if body["device_code"].as_str() == Some("pending-code") {
                Json(json!({"error": "authorization_pending"}))
            } else {
                Json(json!({"access_token": "acc", "refresh_token": "ref", "expires_in": 3600}))
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/auth/device/code", post(device_code))
            .route("/auth/device/token", post(token));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let server = format!("http://{addr}");

        let device = start_device_code(&server).await.expect("start");
        assert_eq!(device.device_code, "device-1");
        assert_eq!(device.user_code, "ABCD");
        assert_eq!(device.interval_secs, 5);
        assert!(device.verification_url.contains("/console/device?user_code=ABCD"));

        assert_eq!(
            poll_device_token(&server, "pending-code").await.expect("poll"),
            Poll::Pending
        );

        let Poll::Ready(ready) = poll_device_token(&server, "device-1").await.expect("poll") else {
            panic!("expected ready");
        };
        assert_eq!(ready.access_token, "acc");
        assert_eq!(ready.refresh_token, "ref");
        assert!(ready.expires_at_ms > now_ms());

        let refreshed = refresh_tokens(&server, "ref", Some("wrk_1".into()))
            .await
            .expect("refresh");
        assert_eq!(refreshed.access_token, "acc2");
        assert_eq!(refreshed.refresh_token, "ref2");
        assert_eq!(refreshed.org_id.as_deref(), Some("wrk_1"));
    }
}
