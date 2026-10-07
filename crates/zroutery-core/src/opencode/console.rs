//! Console credential resolution: the stored Console token, refreshed in place.
//!
//! See `docs/development/opencode-zen.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::oauth::{self, Tokens};
use crate::config::{ProviderConfig, SecretStore};
use crate::error::{Error, Result};
use crate::upstream::DiscoveredModel;

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
    let tokens = read_tokens(store, key_ref)?;

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

/// Read the stored [`Tokens`] blob at `key_ref`.
fn read_tokens(store: &dyn SecretStore, key_ref: &str) -> Result<Tokens> {
    let stored = store
        .get(key_ref)
        .ok_or_else(|| Error::MissingApiKey(key_ref.to_string()))?;
    serde_json::from_str(&stored)
        .map_err(|e| Error::BadUpstreamPayload(format!("opencode console token: {e}")))
}

/// Discover the Console model catalog via `GET {server}/api/config`.
///
/// The Console-issued inference endpoint exposes no `/models` route, so the
/// catalog comes from the Console config endpoint the official client also
/// consults. `provider` supplies the `key_ref`; the Console origin is the one the
/// login recorded.
pub async fn discover(
    store: &dyn SecretStore,
    provider: &ProviderConfig,
) -> Result<Vec<DiscoveredModel>> {
    let tokens = read_tokens(store, &provider.key_ref)?;
    let server = tokens.server.as_deref().unwrap_or(oauth::DEFAULT_SERVER);
    let (access, org) = resolve(store, &provider.key_ref).await?;
    fetch_models(server, &access, org.as_deref()).await
}

async fn fetch_models(
    server: &str,
    access_token: &str,
    org_id: Option<&str>,
) -> Result<Vec<DiscoveredModel>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::internal(format!("opencode console client: {e}")))?;
    let url = format!("{}/api/config", server.trim_end_matches('/'));
    let mut request = client
        .get(&url)
        .bearer_auth(access_token)
        .header("accept", "application/json");
    if let Some(org) = org_id {
        request = request.header("x-opencode-org-id", org);
    }
    let response = request.send().await.map_err(|source| Error::Transport {
        provider: super::PROVIDER_ID.into(),
        source,
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Upstream {
            provider: super::PROVIDER_ID.into(),
            status: status.as_u16(),
            body: String::new(),
        });
    }
    let json: Value = response
        .json()
        .await
        .map_err(|e| Error::BadUpstreamPayload(format!("opencode console config decode: {e}")))?;
    Ok(models_from_config(&json))
}

/// Parse the `config.provider.*.models` catalog out of `/api/config`.
///
/// Disabled models are dropped; the capability fields are the same shape the
/// published catalog uses.
pub fn models_from_config(json: &Value) -> Vec<DiscoveredModel> {
    let Some(providers) = json.pointer("/config/provider").and_then(Value::as_object) else {
        return Vec::new();
    };
    let provider = providers
        .get("opencode")
        .or_else(|| providers.values().next());
    let Some(models) = provider
        .and_then(|provider| provider.get("models"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut out: Vec<DiscoveredModel> = models
        .iter()
        .filter(|(_, entry)| entry.get("disabled").and_then(Value::as_bool) != Some(true))
        .map(|(id, entry)| {
            let mapped = super::catalog_model(entry);
            DiscoveredModel {
                id: id.clone(),
                pricing: None,
                context_window: mapped.context_window,
                max_output: mapped.max_output,
                capabilities: Some(mapped.capabilities),
                free: mapped.free,
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
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

    fn tokens(
        access: &str,
        expires_in_ms: i64,
        server: Option<String>,
        org: Option<&str>,
    ) -> Tokens {
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

    #[test]
    fn parses_the_console_config_catalog() {
        let json = serde_json::json!({
            "config": {"provider": {"opencode": {"models": {
                "space-bunny-free": {
                    "attachment": true, "reasoning": true, "tool_call": true,
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "limit": {"context": 1048576, "output": 524288},
                    "cost": {"input": 0, "output": 0}
                },
                "retired": {"disabled": true, "limit": {"context": 1000, "output": 100}}
            }}}}
        });
        let models = models_from_config(&json);
        assert_eq!(models.len(), 1, "disabled models are dropped");
        let model = &models[0];
        assert_eq!(model.id, "space-bunny-free");
        assert!(model.free);
        assert_eq!(model.context_window, Some(1048576));
        assert!(model.capabilities.as_ref().unwrap().vision);
    }

    #[tokio::test]
    async fn discovery_reads_api_config_against_a_fake_console() {
        use crate::config::ProviderKind;
        use axum::routing::get;

        async fn config() -> Json<Value> {
            Json(json!({"config": {"provider": {"opencode": {"models": {
                "space-bunny-free": {
                    "tool_call": true,
                    "limit": {"context": 1048576, "output": 524288},
                    "cost": {"input": 0, "output": 0}
                }
            }}}}}))
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/api/config", get(config));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let provider = ProviderConfig::new("console", "Console", ProviderKind::OpenCodeConsole);
        let secrets = MemorySecretStore::new().with(
            provider.key_ref.clone(),
            serde_json::to_string(&Tokens {
                access_token: "acc".into(),
                refresh_token: "ref".into(),
                expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
                server: Some(format!("http://{addr}")),
                org_id: Some("wrk".into()),
            })
            .unwrap(),
        );
        let models = discover(&secrets, &provider).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "space-bunny-free");
        assert!(models[0].free);
    }
}
