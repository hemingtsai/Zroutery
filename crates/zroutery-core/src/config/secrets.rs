//! Where provider credentials actually come from. Never stored in the configuration.

use std::collections::BTreeMap;

/// Resolves provider secrets. Implemented by the desktop app with the macOS
/// keychain, and by [`MemorySecretStore`] in tests and headless mode.
pub trait SecretStore: Send + Sync + 'static {
    fn get(&self, key_ref: &str) -> Option<String>;
}

/// In-memory store, also used for `ZROUTERY_KEY_<REF>` environment fallbacks.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    keys: BTreeMap<String, String>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key_ref: impl Into<String>, secret: impl Into<String>) -> Self {
        self.keys.insert(key_ref.into(), secret.into());
        self
    }

    pub fn insert(&mut self, key_ref: impl Into<String>, secret: impl Into<String>) {
        self.keys.insert(key_ref.into(), secret.into());
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key_ref: &str) -> Option<String> {
        self.keys.get(key_ref).cloned()
    }
}
