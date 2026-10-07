//! Console credential resolution: the stored Console token, refreshed in place.
//!
//! See `docs/development/opencode-zen.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::oauth::{self, Tokens};
use crate::config::SecretStore;
use crate::error::{Error, Result};

/// Refresh an access token this long before it actually expires.
const REFRESH_SKEW: Duration = Duration::from_secs(60);

fn cache() -> &'static Mutex<HashMap<String, Tokens>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Tokens>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fresh_in_cache(key_ref: &str) -> Option<Tokens> {
    cache()
        .lock()
        .unwrap()
        .get(key_ref)
        .filter(|tokens| !tokens.needs_refresh(REFRESH_SKEW))
        .cloned()
}

fn refresh_lock(key_ref: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    locks
        .lock()
        .unwrap()
        .entry(key_ref.to_string())
        .or_default()
        .clone()
}

/// Resolve the access token and billing org for a Console provider.
///
/// The stored value at `key_ref` is the [`Tokens`] blob the device-code login
/// wrote. It is refreshed (in memory) when it is expired or within
/// [`REFRESH_SKEW`], with a single-flight lock per `key_ref` so concurrent
/// requests refresh once. Refreshed tokens are not written back — the secret
/// store is read-only, and this flow's refresh token does not rotate — so a
/// process restart re-refreshes once.
pub async fn resolve(store: &dyn SecretStore, key_ref: &str) -> Result<(String, Option<String>)> {
    let stored = store
        .get(key_ref)
        .ok_or_else(|| Error::MissingApiKey(key_ref.to_string()))?;
    let tokens: Tokens = serde_json::from_str(&stored)
        .map_err(|e| Error::BadUpstreamPayload(format!("opencode console token: {e}")))?;

    if let Some(fresh) = fresh_in_cache(key_ref) {
        return Ok((fresh.access_token, fresh.org_id));
    }
    if !tokens.needs_refresh(REFRESH_SKEW) {
        cache()
            .lock()
            .unwrap()
            .insert(key_ref.to_string(), tokens.clone());
        return Ok((tokens.access_token, tokens.org_id));
    }

    let lock = refresh_lock(key_ref);
    let _single_flight = lock.lock().await;
    // Another task may have refreshed while this one waited.
    if let Some(fresh) = fresh_in_cache(key_ref) {
        return Ok((fresh.access_token, fresh.org_id));
    }
    if tokens.refresh_token.trim().is_empty() {
        return Err(Error::invalid(
            "opencode console token expired and has no refresh token; log in again",
        ));
    }
    let server = tokens.server.as_deref().unwrap_or(oauth::DEFAULT_SERVER);
    let refreshed =
        oauth::refresh_tokens(server, &tokens.refresh_token, tokens.org_id.clone()).await?;
    cache()
        .lock()
        .unwrap()
        .insert(key_ref.to_string(), refreshed.clone());
    Ok((refreshed.access_token, refreshed.org_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MemorySecretStore;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use tokio::net::TcpListener;

    fn seeded(key_ref: &str, tokens: &Tokens) -> MemorySecretStore {
        MemorySecretStore::new().with(key_ref, serde_json::to_string(tokens).unwrap())
    }

    fn tokens(access: &str, expires_in_ms: i64, server: Option<String>, org: Option<&str>) -> Tokens {
        Tokens {
            access_token: access.into(),
            refresh_token: "refresh".into(),
            expires_at_ms: chrono::Utc::now().timestamp_millis() + expires_in_ms,
            server,
            org_id: org.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn an_unexpired_token_is_used_as_is() {
        let secrets = seeded(
            "provider:console-live",
            &tokens("live", 3_600_000, None, Some("wrk")),
        );
        let (access, org) = resolve(&secrets, "provider:console-live").await.unwrap();
        assert_eq!(access, "live");
        assert_eq!(org.as_deref(), Some("wrk"));
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_against_the_console() {
        async fn token(Json(_body): Json<Value>) -> Json<Value> {
            Json(json!({"access_token": "fresh", "refresh_token": "refresh", "expires_in": 3600}))
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/auth/device/token", post(token));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let secrets = seeded(
            "provider:console-expired",
            &tokens("stale", -1000, Some(format!("http://{addr}")), Some("wrk")),
        );
        let (access, org) = resolve(&secrets, "provider:console-expired").await.unwrap();
        assert_eq!(access, "fresh");
        assert_eq!(org.as_deref(), Some("wrk"));
    }

    #[tokio::test]
    async fn a_missing_secret_is_an_error() {
        let secrets = MemorySecretStore::new();
        assert!(resolve(&secrets, "provider:console-absent").await.is_err());
    }
}
