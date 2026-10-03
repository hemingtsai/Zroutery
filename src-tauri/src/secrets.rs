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

/// What a credential store did.
///
/// The backend's own error type is not carried through: the one thing the
/// caller has to distinguish is whether an entry was there at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// There is no entry under this reference. Deleting one is a success.
    NoEntry,
    /// Anything else the platform reported: access denied, a locked store, a
    /// storage failure.
    Backend(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NoEntry => write!(f, "no such entry"),
            StoreError::Backend(message) => write!(f, "{message}"),
        }
    }
}

/// The platform credential store.
///
/// A trait so the desktop can be exercised against a store that refuses,
/// fails or has nothing, without touching the user's real keychain.
pub trait CredentialBackend: Send + Sync {
    fn get(&self, key_ref: &str) -> Result<String, StoreError>;
    fn set(&self, key_ref: &str, secret: &str) -> Result<(), StoreError>;
    fn delete(&self, key_ref: &str) -> Result<(), StoreError>;
}

/// The real backend: the macOS keychain or the Windows Credential Manager,
/// whichever the `keyring` crate was built against on this target.
#[derive(Debug, Default)]
pub struct KeyringBackend {
    service: String,
}

impl KeyringBackend {
    pub fn new(service: impl Into<String>) -> Self {
        KeyringBackend {
            service: service.into(),
        }
    }

    fn entry(&self, key_ref: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, key_ref).map_err(backend_error)
    }
}

impl CredentialBackend for KeyringBackend {
    fn get(&self, key_ref: &str) -> Result<String, StoreError> {
        self.entry(key_ref)?.get_password().map_err(backend_error)
    }

    fn set(&self, key_ref: &str, secret: &str) -> Result<(), StoreError> {
        self.entry(key_ref)?
            .set_password(secret)
            .map_err(backend_error)
    }

    fn delete(&self, key_ref: &str) -> Result<(), StoreError> {
        self.entry(key_ref)?
            .delete_credential()
            .map_err(backend_error)
    }
}

/// How the `keyring` crate reports a missing entry.
fn backend_error(error: keyring::Error) -> StoreError {
    match error {
        keyring::Error::NoEntry => StoreError::NoEntry,
        other => StoreError::Backend(other.to_string()),
    }
}

pub struct KeychainSecrets {
    backend: Box<dyn CredentialBackend>,
    fallback: Option<Fallback>,
    cache: RwLock<HashMap<String, Option<String>>>,
}

impl KeychainSecrets {
    /// Keychain only. This is what the desktop app uses.
    pub fn new(service: impl Into<String>) -> Self {
        let service = service.into();
        Self::with_backend(Box::new(KeyringBackend::new(service)))
    }

    /// Keychain first, then `ZROUTERY_KEY_<REF>` from the environment.
    pub fn with_env_fallback(service: impl Into<String>) -> Self {
        Self::with_fallback(
            service,
            Arc::new(|key_ref: &str| std::env::var(KeychainSecrets::env_name(key_ref)).ok()),
        )
    }

    pub fn with_fallback(service: impl Into<String>, fallback: Fallback) -> Self {
        let service = service.into();
        KeychainSecrets {
            backend: Box::new(KeyringBackend::new(service)),
            fallback: Some(fallback),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// A store backed by something other than the platform keychain.
    pub fn with_backend(backend: Box<dyn CredentialBackend>) -> Self {
        KeychainSecrets {
            backend,
            fallback: None,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// `provider:deepseek` -> `ZROUTERY_KEY_PROVIDER_DEEPSEEK`
    ///
    /// Hyphens are kept as-is (valid in env var names on most platforms);
    /// only truly invalid characters like `.`, `:`, and spaces are mapped to `_`.
    /// This avoids collisions where `provider:a.b` and `provider:a-b` would
    /// otherwise produce the same env var name.
    pub fn env_name(key_ref: &str) -> String {
        let sanitized: String = key_ref
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("ZROUTERY_KEY_{sanitized}")
    }

    fn read_backend(&self, key_ref: &str) -> Option<String> {
        match self.backend.get(key_ref) {
            Ok(secret) => Some(secret),
            Err(err) => {
                // Not found is the normal case; anything else is worth a line,
                // because it means the answer came from somewhere else.
                if err != StoreError::NoEntry {
                    tracing::debug!("keychain read for {key_ref} failed: {err}");
                }
                self.fallback.as_ref().and_then(|f| f(key_ref))
            }
        }
    }

    pub fn set(&self, key_ref: &str, secret: &str) -> Result<(), String> {
        self.backend
            .set(key_ref, secret)
            .map_err(|e| format!("cannot store key in keychain: {e}"))?;
        self.cached(key_ref, Some(secret.to_string()));
        Ok(())
    }

    /// Remove the credential. A missing entry is not an error; every other
    /// failure is, and it must not look like the key was removed.
    pub fn delete(&self, key_ref: &str) -> Result<(), String> {
        match self.backend.delete(key_ref) {
            Ok(()) | Err(StoreError::NoEntry) => {
                // Only a confirmed deletion (or a confirmed absence) may mark
                // the cache as deleted. A still-present credential would
                // otherwise reappear after a restart.
                self.cached(key_ref, None);
                Ok(())
            }
            Err(StoreError::Backend(message)) => Err(format!(
                "cannot remove the stored key from the keychain: {message}"
            )),
        }
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
    use std::sync::Mutex;

    fn key_ref() -> String {
        format!("provider:{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn env_names_are_derived_predictably() {
        assert_eq!(
            KeychainSecrets::env_name("provider:deepseek"),
            "ZROUTERY_KEY_PROVIDER_DEEPSEEK"
        );
        // Hyphens are kept; dots, colons, and spaces become underscores.
        assert_eq!(
            KeychainSecrets::env_name("provider:my-thing.1"),
            "ZROUTERY_KEY_PROVIDER_MY-THING_1"
        );
        // Two previously-colliding names now map differently.
        assert_ne!(
            KeychainSecrets::env_name("provider:a.b"),
            KeychainSecrets::env_name("provider:a-b"),
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

    /// A store that answers from memory and can be told to fail.
    ///
    /// It records what it was asked to do, so a test can prove that a failed
    /// delete did not invent a cache state.
    #[derive(Default)]
    struct FakeStore {
        entries: Mutex<HashMap<String, String>>,
        delete_error: Mutex<Option<StoreError>>,
        get_error: Mutex<Option<StoreError>>,
    }

    impl FakeStore {
        fn with_entry(key_ref: &str, secret: &str) -> Self {
            let store = FakeStore::default();
            store
                .entries
                .lock()
                .unwrap()
                .insert(key_ref.to_string(), secret.to_string());
            store
        }
    }

    impl CredentialBackend for FakeStore {
        fn get(&self, key_ref: &str) -> Result<String, StoreError> {
            if let Some(error) = self.get_error.lock().unwrap().clone() {
                return Err(error);
            }
            self.entries
                .lock()
                .unwrap()
                .get(key_ref)
                .cloned()
                .ok_or(StoreError::NoEntry)
        }

        fn set(&self, key_ref: &str, secret: &str) -> Result<(), StoreError> {
            self.entries
                .lock()
                .unwrap()
                .insert(key_ref.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, key_ref: &str) -> Result<(), StoreError> {
            if let Some(error) = self.delete_error.lock().unwrap().clone() {
                return Err(error);
            }
            self.entries.lock().unwrap().remove(key_ref);
            Ok(())
        }
    }

    fn store_with_fake(fake: FakeStore) -> KeychainSecrets {
        KeychainSecrets::with_backend(Box::new(fake))
    }

    /// A refusal is returned to the caller and the cache still says the key is
    /// there, because it is.
    #[test]
    fn a_refused_delete_is_an_error_and_leaves_the_cache_alone() {
        struct Refusing {
            inner: FakeStore,
        }
        impl CredentialBackend for Refusing {
            fn get(&self, key_ref: &str) -> Result<String, StoreError> {
                self.inner.get(key_ref)
            }
            fn set(&self, key_ref: &str, secret: &str) -> Result<(), StoreError> {
                self.inner.set(key_ref, secret)
            }
            fn delete(&self, _key_ref: &str) -> Result<(), StoreError> {
                Err(StoreError::Backend("access denied by the user".into()))
            }
        }

        let key = key_ref();
        let inner = FakeStore::with_entry(&key, "sk-still-there");
        let store = KeychainSecrets::with_backend(Box::new(Refusing { inner }));

        assert!(store.has(&key));
        let err = store.delete(&key).unwrap_err();
        assert!(err.contains("access denied"), "{err}");
        assert!(
            store.has(&key),
            "a refused delete must not make the key look removed"
        );
    }

    #[test]
    fn a_storage_failure_during_delete_is_returned_and_cached_value_kept() {
        let key = key_ref();
        let failing = FakeStore::default();
        failing
            .entries
            .lock()
            .unwrap()
            .insert(key.clone(), "sk-locked".into());
        *failing.delete_error.lock().unwrap() =
            Some(StoreError::Backend("the store is locked".into()));
        let store = store_with_fake(failing);

        assert_eq!(store.get(&key).as_deref(), Some("sk-locked"));
        let err = store.delete(&key).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(
            store.get(&key).as_deref(),
            Some("sk-locked"),
            "the cached value must survive a failed delete"
        );
    }

    /// A confirmed absence is success: the caller wanted the key gone and it is.
    #[test]
    fn deleting_a_missing_entry_succeeds_and_caches_the_absence() {
        let store = store_with_fake(FakeStore::default());
        let key = key_ref();
        assert!(!store.has(&key));
        store.delete(&key).expect("a missing entry is not an error");
        assert!(!store.has(&key));
    }

    /// A confirmed deletion clears the cache and the entry.
    #[test]
    fn deleting_a_present_entry_removes_it_and_caches_the_absence() {
        let key = key_ref();
        let store = store_with_fake(FakeStore::with_entry(&key, "sk-gone"));
        assert!(store.has(&key));
        store.delete(&key).unwrap();
        assert!(!store.has(&key));
        // A second delete is the missing-entry case, and still a success.
        store.delete(&key).unwrap();
    }

    /// A real round trip through the platform's native credential store —
    /// the Windows Credential Manager via the `windows-native` keyring
    /// backend. Proves the target-specific feature actually wires up, which
    /// compiling alone does not.
    #[test]
    #[cfg(target_os = "windows")]
    fn the_native_credential_store_round_trips() {
        let store = KeychainSecrets::new("app.zroutery.test.credential-manager");
        let key = format!("provider:{}", uuid::Uuid::new_v4().simple());

        store.set(&key, "sk-roundtrip").unwrap();
        assert_eq!(store.get(&key).as_deref(), Some("sk-roundtrip"));

        // A second instance reads the same entry: the store is the OS's, not
        // ours (ours only caches).
        let fresh = KeychainSecrets::new("app.zroutery.test.credential-manager");
        assert_eq!(fresh.get(&key).as_deref(), Some("sk-roundtrip"));

        store.delete(&key).unwrap();
        assert!(store.get(&key).is_none());
        // Each instance caches reads independently, so a brand new one is the
        // honest witness that the deletion reached the OS store.
        let later = KeychainSecrets::new("app.zroutery.test.credential-manager");
        assert!(later.get(&key).is_none());
    }
}
