//! Desktop application state: owns the core proxy state, the keychain, the
//! config file location and the running server handle.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use zroutery_core::config::{AppConfig, ConfigIssue, IssueSeverity, ServerConfig};
use zroutery_core::router::ModelHealth;
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::{RequestRecord, StatsSummary};

use crate::secrets::KeychainSecrets;
use crate::store;

pub struct Desktop {
    pub core: Arc<AppState>,
    pub secrets: Arc<KeychainSecrets>,
    pub config_dir: PathBuf,
    pub server: AsyncMutex<Option<ServerHandle>>,
    /// Startup problem worth surfacing once in the UI.
    pub warning: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub address: Option<String>,
    pub base_url: Option<String>,
    pub host: String,
    pub port: u16,
    pub require_auth: bool,
    pub token: String,
    pub exposed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub config: AppConfig,
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
        }
    }

    pub fn set_warning(&self, warning: Option<String>) {
        *self.warning.lock().expect("warning poisoned") = warning;
    }

    pub async fn snapshot(&self) -> Snapshot {
        let config = (*self.core.config()).clone();
        let issues = config.validate();
        let blocking = issues.iter().any(|i| i.severity == IssueSeverity::Error);
        let running_addr = self
            .server
            .lock()
            .await
            .as_ref()
            .map(|s| s.addr.to_string());

        let keys = config
            .providers
            .iter()
            .map(|p| (p.id.clone(), self.secrets.has(&p.key_ref)))
            .collect();

        Snapshot {
            server: ServerStatus {
                running: running_addr.is_some(),
                base_url: running_addr.as_ref().map(|a| format!("http://{a}")),
                address: running_addr,
                host: config.server.host.clone(),
                port: config.server.port,
                require_auth: config.server.require_auth,
                token: config.server.auth_token.clone(),
                exposed: config.server.is_exposed(),
            },
            keys,
            issues,
            blocking,
            health: self.core.router.health_snapshot(),
            summary: self.core.stats.summary(),
            recent: self.core.stats.recent(200),
            warning: self.warning.lock().expect("warning poisoned").clone(),
            config_path: self.config_dir.join(store::FILE_NAME).display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            config,
        }
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
    pub async fn apply_config(&self, next: AppConfig) -> Result<bool, String> {
        let issues = next.validate();
        if let Some(err) = issues.iter().find(|i| i.severity == IssueSeverity::Error) {
            return Err(err.message.clone());
        }
        let previous = self.core.config();
        store::save(&self.config_dir, &next)?;
        let needs_rebind = needs_rebind(&previous.server, &next.server);
        self.core.set_config(next);
        if needs_rebind && self.is_running().await {
            self.restart().await?;
        }
        Ok(needs_rebind)
    }
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
}
