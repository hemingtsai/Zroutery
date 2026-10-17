//! The local server and the shadow-routing knobs, as configured rather than as running.

use serde::{Deserialize, Serialize};

use super::provider::default_true;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Loopback by default. Changing this exposes your API keys to the network.
    #[serde(default = "ServerConfig::default_host")]
    pub host: String,
    #[serde(default = "ServerConfig::default_port")]
    pub port: u16,
    /// Require `Authorization: Bearer <token>` or `x-api-key: <token>`.
    #[serde(default = "default_true")]
    pub require_auth: bool,
    /// Local access token. Generated on first run.
    #[serde(default)]
    pub auth_token: String,
    /// Start the proxy as soon as the app launches.
    #[serde(default = "default_true")]
    pub autostart: bool,
    /// Allow browser origins to call the proxy.
    #[serde(default)]
    pub allow_cors: bool,
    /// Origins allowed when `allow_cors` is on. Empty means every origin, which
    /// `validate` reports as a warning.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Largest accepted request body, in mebibytes. Inline images make prompts
    /// big, but not unbounded.
    #[serde(default = "ServerConfig::default_body_limit_mib")]
    pub max_body_mib: usize,
    /// Number of request records kept in memory for the GUI.
    #[serde(default = "ServerConfig::default_log_limit")]
    pub log_limit: usize,
    /// Bypass the system proxy for upstream requests. Some proxies strip
    /// non-standard headers (x-app, x-stainless-*) needed for gateway
    /// fingerprint checks.
    #[serde(default)]
    pub bypass_proxy: bool,
}

impl ServerConfig {
    fn default_host() -> String {
        "127.0.0.1".to_string()
    }
    fn default_port() -> u16 {
        8787
    }
    fn default_log_limit() -> usize {
        500
    }
    fn default_body_limit_mib() -> usize {
        32
    }

    /// True when the server is reachable from outside this machine.
    pub fn is_exposed(&self) -> bool {
        !matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// Body limit in bytes, clamped to something a proxy can actually buffer.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_mib.clamp(1, 512) * 1024 * 1024
    }

    /// True when CORS is on without an origin list, i.e. any site may call in.
    pub fn cors_is_wide_open(&self) -> bool {
        self.allow_cors && self.cors_origins.iter().all(|o| o.trim().is_empty())
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            host: Self::default_host(),
            port: Self::default_port(),
            require_auth: true,
            auth_token: String::new(),
            autostart: true,
            allow_cors: false,
            cors_origins: Vec::new(),
            max_body_mib: Self::default_body_limit_mib(),
            log_limit: Self::default_log_limit(),
            bypass_proxy: false,
        }
    }
}

/// Shadow decision evaluation (Stage 7E-1): record what the ML routing stack
/// would have done with policy-routed main traffic, without influencing
/// production routing. Off by default — the records are diagnostic evidence,
/// never routing input.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowConfig {
    /// Master switch. Off: no snapshots are taken and nothing is recorded.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum shadow decisions held in memory before the oldest is evicted.
    #[serde(default = "ShadowConfig::default_max_decisions")]
    pub max_decisions: usize,
    /// Retention window for shadow decisions, in seconds.
    #[serde(default = "ShadowConfig::default_max_age_secs")]
    pub max_age_secs: i64,
}

#[cfg(feature = "ml")]
impl ShadowConfig {
    fn default_max_decisions() -> usize {
        10_000
    }
    fn default_max_age_secs() -> i64 {
        7 * 24 * 3600
    }
}

#[cfg(feature = "ml")]
impl Default for ShadowConfig {
    fn default() -> Self {
        ShadowConfig {
            enabled: false,
            max_decisions: Self::default_max_decisions(),
            max_age_secs: Self::default_max_age_secs(),
        }
    }
}
