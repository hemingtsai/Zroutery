//! The small helpers the rest share: poison recovery, the address a config would bind, and when a rebind is needed.

use std::sync::Mutex;

use zroutery_core::config::ServerConfig;

/// Recovers a poisoned lock instead of taking the app down with it; the worst
/// case is a stale balance or warning.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("recovered from poisoned mutex");
        poisoned.into_inner()
    })
}

/// The address a server configuration would listen on.
pub fn server_addr(server: &ServerConfig) -> String {
    format!("{}:{}", server.host, server.port)
}

/// Only these settings require tearing the listener down.
pub fn needs_rebind(a: &ServerConfig, b: &ServerConfig) -> bool {
    a.host != b.host
        || a.port != b.port
        || a.allow_cors != b.allow_cors
        // CORS origins and the body limit are baked into the router at bind
        // time; changing them needs a listener rebuild to take effect.
        || a.cors_origins != b.cors_origins
        || a.max_body_mib != b.max_body_mib
}
