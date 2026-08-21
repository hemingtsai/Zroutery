//! Desktop application state: owns the core proxy state, the keychain, the
//! config file location and the running server handle.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use zroutery_core::billing::Balance;
use zroutery_core::config::{AppConfig, ConfigIssue, IssueSeverity, ServerConfig};
use zroutery_core::router::ModelHealth;
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::{RequestRecord, StatsSummary};

use crate::secrets::KeychainSecrets;
use crate::store;

pub struct Desktop {
    pub(crate) core: Arc<AppState>,
    pub(crate) secrets: Arc<KeychainSecrets>,
    pub(crate) config_dir: PathBuf,
    server: AsyncMutex<Option<ServerHandle>>,
    /// Startup problem worth surfacing once in the UI.
    warning: Mutex<Option<String>>,
    /// Last answer from each provider's balance endpoint. Never fetched on a
    /// timer: it costs a request and some vendors rate limit it.
    balances: Mutex<BTreeMap<String, BalanceStatus>>,
}

/// What the last balance check found, per provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceStatus {
    pub checked_at: DateTime<Utc>,
    pub balance: Option<Balance>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub address: Option<String>,
    pub base_url: Option<String>,
    pub host: String,
    pub port: u16,
    pub require_auth: bool,
    /// Enough of the token to recognise which one is in play, never the whole
    /// thing: snapshots are handed to the webview on every poll. Use the
    /// `reveal_token` or `copy_token` commands for the real value.
    pub token_hint: String,
    pub exposed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub config: AppConfig,
    /// The exposed id of every entry in `config.models`, in the same order.
    ///
    /// The dashboard renders these instead of deriving ids itself, so the
    /// `<provider>-<model>` rule has exactly one implementation.
    pub exposed_ids: Vec<String>,
    pub issues: Vec<ConfigIssue>,
    pub blocking: bool,
    pub server: ServerStatus,
    /// provider id -> whether an API key is stored for it.
    pub keys: std::collections::BTreeMap<String, bool>,
    pub health: Vec<ModelHealth>,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
    pub warning: Option<String>,
    pub config_path: String,
    pub version: String,
    /// provider id -> last balance check, for the providers that were asked.
    pub balances: BTreeMap<String, BalanceStatus>,
}

/// The subset the Activity tab polls for.
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub health: Vec<ModelHealth>,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
}

/// `zr-1234abcd…` -> `zr-…abcd`: enough to tell two tokens apart, useless on its
/// own.
pub fn token_hint(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("zr-…{tail}")
}

impl Desktop {
    pub fn new(config_dir: PathBuf, config: AppConfig, secrets: Arc<KeychainSecrets>) -> Self {
        let core = Arc::new(AppState::new(config, secrets.clone() as Arc<_>));
        Desktop {
            core,
            secrets,
            config_dir,
            server: AsyncMutex::new(None),
            warning: Mutex::new(None),
            balances: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn set_warning(&self, warning: Option<String>) {
        *lock(&self.warning) = warning;
    }

    pub fn warning(&self) -> Option<String> {
        lock(&self.warning).clone()
    }

    pub async fn snapshot(&self) -> Snapshot {
        let stored = self.core.config();
        let issues = stored.validate();
        let blocking = issues.iter().any(|i| i.severity == IssueSeverity::Error);
        let running_addr = self
            .server
            .lock()
            .await
            .as_ref()
            .map(|s| s.addr.to_string());

        let keys = stored
            .providers
            .iter()
            .map(|p| (p.id.clone(), self.secrets.has(&p.key_ref)))
            .collect();

        // The webview gets everything except the token itself.
        let mut config = (*stored).clone();
        config.server.auth_token = String::new();

        Snapshot {
            server: ServerStatus {
                running: running_addr.is_some(),
                base_url: running_addr.as_ref().map(|a| format!("http://{a}")),
                address: running_addr,
                host: stored.server.host.clone(),
                port: stored.server.port,
                require_auth: stored.server.require_auth,
                token_hint: token_hint(&stored.server.auth_token),
                exposed: stored.server.is_exposed(),
            },
            keys,
            issues,
            blocking,
            health: self.core.router().health_snapshot(),
            summary: self.core.stats().summary(),
            recent: self.core.stats().recent(200),
            warning: self.warning(),
            config_path: self.config_dir.join(store::FILE_NAME).display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            exposed_ids: config.exposed_ids(),
            balances: self.balances(),
            config,
        }
    }

    pub fn balances(&self) -> BTreeMap<String, BalanceStatus> {
        lock(&self.balances).clone()
    }

    /// Ask one provider how much credit is left and remember the answer.
    ///
    /// A failure is stored rather than thrown away: "asked and refused" is more
    /// useful on screen than a blank.
    pub async fn refresh_balance(&self, provider_id: &str) -> Result<(), String> {
        let config = self.core.config();
        let provider = config
            .provider(provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let probe = provider
            .balance
            .probe(provider.kind)
            .ok_or_else(|| format!("{} does not publish a balance", provider.name))?;

        let key = self.core.api_key(provider).map_err(|e| e.to_string())?;
        let outcome = self
            .core
            .upstream()
            .fetch_balance(provider, key.as_deref(), &probe)
            .await;

        let status = match outcome {
            Ok(balance) => BalanceStatus {
                checked_at: Utc::now(),
                balance: Some(balance),
                error: None,
            },
            Err(e) => BalanceStatus {
                checked_at: Utc::now(),
                balance: None,
                error: Some(e.to_string()),
            },
        };
        let failed = status.error.clone();
        lock(&self.balances).insert(provider_id.to_string(), status);
        match failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Refresh every provider that publishes a balance, one after another.
    ///
    /// Sequential on purpose: a handful of providers, and hammering them in
    /// parallel is a good way to get rate limited.
    pub async fn refresh_all_balances(&self) -> Vec<String> {
        let ids: Vec<String> = self
            .core
            .config()
            .providers
            .iter()
            .filter(|p| p.enabled && p.balance.is_supported(p.kind))
            .map(|p| p.id.clone())
            .collect();

        let mut problems = Vec::new();
        for id in ids {
            if let Err(e) = self.refresh_balance(&id).await {
                problems.push(format!("{id}: {e}"));
            }
        }
        problems
    }

    /// Only the counters and the log, for the Activity tab's polling.
    ///
    /// A full snapshot clones the whole configuration and asks the keychain about
    /// every provider; this is what the dashboard actually needs twice a second.
    pub fn activity(&self) -> Activity {
        Activity {
            health: self.core.router().health_snapshot(),
            summary: self.core.stats().summary(),
            recent: self.core.stats().recent(200),
        }
    }

    /// The real token. Only reached through an explicit user action.
    pub fn auth_token(&self) -> String {
        self.core.config().server.auth_token.clone()
    }

    pub async fn is_running(&self) -> bool {
        self.server.lock().await.is_some()
    }

    pub async fn start(&self) -> Result<(), String> {
        let mut guard = self.server.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let handle = ServerHandle::start(Arc::clone(&self.core))
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!("proxy listening on http://{}", handle.addr);
        *guard = Some(handle);
        Ok(())
    }

    pub async fn stop(&self) {
        let handle = self.server.lock().await.take();
        if let Some(h) = handle {
            h.stop().await;
            tracing::info!("proxy stopped");
        }
    }

    pub async fn restart(&self) -> Result<(), String> {
        self.stop().await;
        self.start().await
    }

    /// Persist and hot swap a new configuration.
    ///
    /// Returns `true` when the listener had to be rebound, which only happens
    /// for host, port or CORS changes.
    pub async fn apply_config(&self, mut next: AppConfig) -> Result<bool, String> {
        // Tidy the alias lists and fold any legacy id the dashboard echoed back.
        next.normalize();
        let previous = self.core.config();
        // The dashboard never receives the token, so an empty one means "keep
        // what you have" rather than "clear it".
        if next.server.auth_token.trim().is_empty() {
            next.server.auth_token = previous.server.auth_token.clone();
        }
        let issues = next.validate();
        if let Some(err) = issues.iter().find(|i| i.severity == IssueSeverity::Error) {
            return Err(err.message.clone());
        }
        store::save(&self.config_dir, &next)?;
        let needs_rebind = needs_rebind(&previous.server, &next.server);
        self.core.set_config(next);
        if needs_rebind && self.is_running().await {
            self.restart().await?;
        }
        Ok(needs_rebind)
    }
}

/// Recovers a poisoned lock instead of taking the app down with it; the worst
/// case is a stale balance or warning.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Only these settings require tearing the listener down.
pub fn needs_rebind(a: &ServerConfig, b: &ServerConfig) -> bool {
    a.host != b.host || a.port != b.port || a.allow_cors != b.allow_cors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebind_only_on_transport_changes() {
        let a = ServerConfig::default();
        let mut b = a.clone();
        b.auth_token = "different".into();
        b.require_auth = false;
        b.log_limit = 10;
        assert!(!needs_rebind(&a, &b));

        b.port = 9999;
        assert!(needs_rebind(&a, &b));

        let mut c = a.clone();
        c.allow_cors = true;
        assert!(needs_rebind(&a, &c));

        let mut d = a.clone();
        d.host = "0.0.0.0".into();
        assert!(needs_rebind(&a, &d));
    }

    fn desktop_with(config: AppConfig) -> (Arc<Desktop>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("zroutery-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let secrets = Arc::new(KeychainSecrets::new(format!(
            "app.zroutery.test.{}",
            uuid::Uuid::new_v4()
        )));
        (Arc::new(Desktop::new(dir.clone(), config, secrets)), dir)
    }

    fn config_with_token(token: &str) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.server.auth_token = token.into();
        cfg.server.port = 0;
        cfg
    }

    #[test]
    fn the_token_hint_keeps_only_the_tail() {
        assert_eq!(token_hint("zr-0123456789abcdef"), "zr-…cdef");
        assert_eq!(token_hint(""), "");
        // A short token still does not reveal itself entirely.
        assert_eq!(token_hint("abcd"), "zr-…abcd");
    }

    #[tokio::test]
    async fn snapshots_never_carry_the_token() {
        let (desktop, dir) = desktop_with(config_with_token("zr-secret-token-1234"));
        let snapshot = desktop.snapshot().await;

        assert_eq!(snapshot.server.token_hint, "zr-…1234");
        assert!(snapshot.config.server.auth_token.is_empty());
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(
            !json.contains("zr-secret-token-1234"),
            "the token reached the payload handed to the webview"
        );
        // The explicit accessor still has it.
        assert_eq!(desktop.auth_token(), "zr-secret-token-1234");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn saving_a_redacted_config_keeps_the_existing_token() {
        let (desktop, dir) = desktop_with(config_with_token("zr-keep-me-9999"));

        // What the dashboard sends back: everything except the token.
        let mut edited = desktop.snapshot().await.config;
        assert!(edited.server.auth_token.is_empty());
        edited.server.log_limit = 42;
        desktop.apply_config(edited).await.unwrap();

        assert_eq!(desktop.auth_token(), "zr-keep-me-9999");
        assert_eq!(desktop.core.config().server.log_limit, 42);

        // An explicit new token is still honoured.
        let mut rotated = desktop.snapshot().await.config;
        rotated.server.auth_token = "zr-brand-new".into();
        desktop.apply_config(rotated).await.unwrap();
        assert_eq!(desktop.auth_token(), "zr-brand-new");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn the_activity_view_matches_the_snapshot_counters() {
        let (desktop, dir) = desktop_with(config_with_token("zr-1"));
        let activity = desktop.activity();
        let snapshot = desktop.snapshot().await;
        assert_eq!(activity.summary.requests, snapshot.summary.requests);
        assert_eq!(activity.recent.len(), snapshot.recent.len());
        assert_eq!(activity.health.len(), snapshot.health.len());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn balances_are_only_fetched_where_they_exist() {
        use zroutery_core::billing::{BalanceConfig, BalancePreset, BalanceProbe};
        use zroutery_core::config::{ProviderConfig, ProviderKind};

        let mut config = config_with_token("zr-1");
        let mut quiet = ProviderConfig::new("quiet", "Quiet Co", ProviderKind::OpenAICompatible);
        quiet.key_ref = String::new();
        let mut probed = quiet.clone();
        probed.id = "probed".into();
        probed.name = "Probed Co".into();
        // Nothing listens here, so the fetch fails without needing a mock server.
        probed.base_url = "http://127.0.0.1:1".into();
        probed.connect_timeout_secs = 1;
        probed.timeout_secs = 2;
        probed.balance = BalanceConfig {
            preset: BalancePreset::Custom,
            custom: Some(BalanceProbe::default()),
        };
        config.providers = vec![quiet, probed];
        let (desktop, dir) = desktop_with(config);

        // A provider with no endpoint is refused up front rather than asked.
        let err = desktop.refresh_balance("quiet").await.unwrap_err();
        assert!(err.contains("does not publish a balance"), "{err}");
        assert!(desktop.balances().is_empty());
        assert!(desktop.refresh_balance("nope").await.is_err());

        // A failure is remembered so the dashboard can show why.
        assert!(desktop.refresh_balance("probed").await.is_err());
        let status = desktop.balances().get("probed").cloned().unwrap();
        assert!(status.balance.is_none());
        assert!(status.error.is_some());
        assert!(desktop.snapshot().await.balances.contains_key("probed"));

        // Refreshing everything skips the provider that cannot answer.
        let problems = desktop.refresh_all_balances().await;
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("probed:"));

        std::fs::remove_dir_all(dir).ok();
    }
}
