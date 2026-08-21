//! API key storage backed by the macOS keychain.
//!
//! Keys never touch the configuration file. Reads are cached in memory because
//! the keychain would otherwise be consulted on every proxied request.
//!
//! The desktop app is keychain only. Environment variables are a convenient
//! fallback for headless runs and CI, but they are readable by every process of
//! the same user and end up in crash reports and CI logs, so the GUI never
//! consults them: only [`KeychainSecrets::with_env_fallback`] enables that path.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use zroutery_core::config::SecretStore;

/// Looks a secret up outside the keychain. Kept behind a trait object so tests
/// can supply their own without touching the process environment.
pub type Fallback = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct KeychainSecrets {
    service: String,
    fallback: Option<Fallback>,
    cache: RwLock<HashMap<String, Option<String>>>,
}

impl KeychainSecrets {
    /// Keychain only. This is what the desktop app uses.
    pub fn new(service: impl Into<String>) -> Self {
        KeychainSecrets {
            service: service.into(),
            fallback: None,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Keychain first, then `ZROUTERY_KEY_<REF>` from the environment.
    pub fn with_env_fallback(service: impl Into<String>) -> Self {
        Self::with_fallback(
            service,
            Arc::new(|key_ref: &str| std::env::var(KeychainSecrets::env_name(key_ref)).ok()),
        )
    }

    pub fn with_fallback(service: impl Into<String>, fallback: Fallback) -> Self {
        KeychainSecrets {
            service: service.into(),
            fallback: Some(fallback),
            cache: RwLock::new(HashMap::new()),
        }
    }

    fn entry(&self, key_ref: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, key_ref).map_err(|e| e.to_string())
    }

    /// `provider:deepseek` -> `ZROUTERY_KEY_PROVIDER_DEEPSEEK`
    pub fn env_name(key_ref: &str) -> String {
        let sanitized: String = key_ref
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("ZROUTERY_KEY_{sanitized}")
    }

    fn read_backend(&self, key_ref: &str) -> Option<String> {
        match self
            .entry(key_ref)
            .and_then(|e| e.get_password().map_err(|e| e.to_string()))
        {
            Ok(secret) => Some(secret),
            Err(err) => {
                // Not found is the normal case; anything else is worth a line.
                if !err.contains("No matching entry") && !err.contains("not found") {
                    tracing::debug!("keychain read for {key_ref} failed: {err}");
                }
                self.fallback.as_ref().and_then(|f| f(key_ref))
            }
        }
    }

    pub fn set(&self, key_ref: &str, secret: &str) -> Result<(), String> {
        self.entry(key_ref)?
            .set_password(secret)
            .map_err(|e| format!("cannot store key in keychain: {e}"))?;
        self.cached(key_ref, Some(secret.to_string()));
        Ok(())
    }

    pub fn delete(&self, key_ref: &str) -> Result<(), String> {
        // Deleting a key that is not there is not an error.
        if let Ok(entry) = self.entry(key_ref) {
            let _ = entry.delete_credential();
        }
        self.cached(key_ref, None);
        Ok(())
    }

    pub fn has(&self, key_ref: &str) -> bool {
        self.get(key_ref).is_some_and(|k| !k.is_empty())
    }

    fn cached(&self, key_ref: &str, value: Option<String>) {
        write(&self.cache).insert(key_ref.to_string(), value);
    }
}

impl SecretStore for KeychainSecrets {
    fn get(&self, key_ref: &str) -> Option<String> {
        if let Some(hit) = read(&self.cache).get(key_ref) {
            return hit.clone();
        }
        let value = self.read_backend(key_ref);
        self.cached(key_ref, value.clone());
        value
    }
}

/// A poisoned cache is not a reason to take the proxy down; the worst case is one
/// extra keychain read.
fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|p| p.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_ref() -> String {
        format!("provider:{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn env_names_are_derived_predictably() {
        assert_eq!(
            KeychainSecrets::env_name("provider:deepseek"),
            "ZROUTERY_KEY_PROVIDER_DEEPSEEK"
        );
        assert_eq!(
            KeychainSecrets::env_name("provider:my-thing.1"),
            "ZROUTERY_KEY_PROVIDER_MY_THING_1"
        );
    }

    #[test]
    fn missing_keys_are_none_and_get_cached() {
        let store = KeychainSecrets::new("app.zroutery.test.missing");
        let key = key_ref();
        assert!(store.get(&key).is_none());
        assert!(!store.has(&key));
        // second read comes from the cache
        assert!(store.get(&key).is_none());
    }

    #[test]
    fn the_gui_store_ignores_the_environment() {
        // No fallback installed, so even a set variable is invisible.
        let store = KeychainSecrets::new("app.zroutery.test.no-fallback");
        assert!(store.fallback.is_none());
        assert!(store.get(&key_ref()).is_none());
    }

    #[test]
    fn a_fallback_is_consulted_when_the_keychain_has_nothing() {
        let key = key_ref();
        let wanted = key.clone();
        let store = KeychainSecrets::with_fallback(
            "app.zroutery.test.fallback",
            Arc::new(move |asked: &str| (asked == wanted).then(|| "sk-from-fallback".to_string())),
        );
        assert_eq!(store.get(&key).as_deref(), Some("sk-from-fallback"));
        assert!(store.has(&key));
        assert!(store.get("provider:something-else").is_none());
    }

    #[test]
    fn deleting_updates_the_cache_without_touching_the_keychain_again() {
        let key = key_ref();
        let store = KeychainSecrets::with_fallback(
            "app.zroutery.test.delete",
            Arc::new(|_| Some("sk-x".to_string())),
        );
        assert!(store.has(&key));
        store.delete(&key).unwrap();
        assert!(!store.has(&key), "the deletion must win over the fallback");
    }
}
