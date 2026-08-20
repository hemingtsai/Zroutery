//! API key storage backed by the macOS keychain.
//!
//! Keys never touch the configuration file. Reads are cached in memory because
//! the keychain is consulted on every proxied request.

use std::collections::HashMap;
use std::sync::RwLock;

use zroutery_core::config::SecretStore;

pub struct KeychainSecrets {
    service: String,
    cache: RwLock<HashMap<String, Option<String>>>,
}

impl KeychainSecrets {
    pub fn new(service: impl Into<String>) -> Self {
        KeychainSecrets {
            service: service.into(),
            cache: RwLock::new(HashMap::new()),
        }
    }

    fn entry(&self, key_ref: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, key_ref).map_err(|e| e.to_string())
    }

    /// `provider:deepseek` -> `ZROUTERY_KEY_PROVIDER_DEEPSEEK`
    fn env_name(key_ref: &str) -> String {
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
                // Not found is normal; anything else is worth a log line.
                if !err.contains("No matching entry") && !err.contains("not found") {
                    tracing::debug!("keychain read for {key_ref} failed: {err}");
                }
                // Useful for CI and headless runs.
                std::env::var(Self::env_name(key_ref)).ok()
            }
        }
    }

    pub fn set(&self, key_ref: &str, secret: &str) -> Result<(), String> {
        self.entry(key_ref)?
            .set_password(secret)
            .map_err(|e| format!("cannot store key in keychain: {e}"))?;
        self.cache
            .write()
            .expect("secret cache poisoned")
            .insert(key_ref.to_string(), Some(secret.to_string()));
        Ok(())
    }

    pub fn delete(&self, key_ref: &str) -> Result<(), String> {
        // Deleting a key that is not there is not an error.
        if let Ok(entry) = self.entry(key_ref) {
            let _ = entry.delete_credential();
        }
        self.cache
            .write()
            .expect("secret cache poisoned")
            .insert(key_ref.to_string(), None);
        Ok(())
    }

    pub fn has(&self, key_ref: &str) -> bool {
        self.get(key_ref).is_some_and(|k| !k.is_empty())
    }

    pub fn forget_cached(&self, key_ref: &str) {
        self.cache
            .write()
            .expect("secret cache poisoned")
            .remove(key_ref);
    }
}

impl SecretStore for KeychainSecrets {
    fn get(&self, key_ref: &str) -> Option<String> {
        if let Some(hit) = self
            .cache
            .read()
            .expect("secret cache poisoned")
            .get(key_ref)
        {
            return hit.clone();
        }
        let value = self.read_backend(key_ref);
        self.cache
            .write()
            .expect("secret cache poisoned")
            .insert(key_ref.to_string(), value.clone());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let key_ref = format!("provider:{}", uuid::Uuid::new_v4());
        assert!(store.get(&key_ref).is_none());
        assert!(!store.has(&key_ref));
        // second read comes from the cache
        assert!(store.get(&key_ref).is_none());
    }

    #[test]
    fn env_fallback_is_used_when_the_keychain_has_nothing() {
        let key_ref = format!("provider:{}", uuid::Uuid::new_v4().simple());
        let store = KeychainSecrets::new("app.zroutery.test.env");
        // SAFETY: single-threaded test setup for a process-local variable.
        unsafe { std::env::set_var(KeychainSecrets::env_name(&key_ref), "sk-from-env") };
        assert_eq!(store.get(&key_ref).as_deref(), Some("sk-from-env"));
        unsafe { std::env::remove_var(KeychainSecrets::env_name(&key_ref)) };
    }
}
