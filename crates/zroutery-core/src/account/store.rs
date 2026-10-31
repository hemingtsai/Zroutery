//! Thread-safe account store.

use super::types::*;
use std::collections::HashMap;
use std::sync::Mutex;

/// Composite key: provider + account.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccountKey {
    provider_id: String,
    account_id: AccountId,
}

/// Thread-safe store for account runtime states.
#[derive(Debug)]
pub struct AccountStore {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    accounts: HashMap<AccountKey, AccountRuntime>,
    versions: HashMap<AccountKey, u64>,
    sequence: u64,
    declarations: HashMap<AccountKey, String>,
}

impl AccountStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn get(&self, provider_id: &str, account_id: &AccountId) -> Option<AccountRuntime> {
        let key = AccountKey {
            provider_id: provider_id.into(),
            account_id: account_id.clone(),
        };
        crate::sync::lock(&self.inner).accounts.get(&key).cloned()
    }

    pub fn upsert(&self, runtime: AccountRuntime) {
        let key = AccountKey {
            provider_id: runtime.provider_id.clone(),
            account_id: runtime.account_id.clone(),
        };
        crate::sync::lock(&self.inner).accounts.insert(key, runtime);
    }

    pub fn remove(&self, provider_id: &str, account_id: &AccountId) -> bool {
        let key = AccountKey {
            provider_id: provider_id.into(),
            account_id: account_id.clone(),
        };
        let mut inner = crate::sync::lock(&self.inner);
        inner.sequence += 1;
        let sequence = inner.sequence;
        inner.versions.insert(key.clone(), sequence);
        inner.declarations.remove(&key);
        inner.accounts.remove(&key).is_some()
    }

    pub fn list_by_provider(&self, provider_id: &str) -> Vec<AccountRuntime> {
        crate::sync::lock(&self.inner)
            .accounts
            .iter()
            .filter(|(k, _)| k.provider_id == provider_id)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Capture a publication token. Two overlapping refreshes cannot publish in
    /// reverse order, and a delete/recreate invalidates every previous token.
    pub fn begin_probe(&self, provider_id: &str, account_id: &AccountId) -> u64 {
        let key = AccountKey {
            provider_id: provider_id.into(),
            account_id: account_id.clone(),
        };
        let mut inner = crate::sync::lock(&self.inner);
        inner.sequence += 1;
        let version = inner.sequence;
        inner.versions.insert(key, version);
        version
    }

    pub fn begin_declared_probe(
        &self,
        provider: &crate::config::ProviderConfig,
        account: &crate::config::AccountConfig,
    ) -> Option<u64> {
        let key = AccountKey {
            provider_id: provider.id.clone(),
            account_id: AccountId(account.account_id.clone()),
        };
        let fingerprint = declaration_fingerprint(provider, account);
        let mut inner = crate::sync::lock(&self.inner);
        if inner.declarations.get(&key) != Some(&fingerprint) {
            return None;
        }
        inner.sequence += 1;
        let version = inner.sequence;
        inner.versions.insert(key, version);
        Some(version)
    }

    pub fn publish_if_current(&self, runtime: AccountRuntime, expected: u64) -> bool {
        let key = AccountKey {
            provider_id: runtime.provider_id.clone(),
            account_id: runtime.account_id.clone(),
        };
        let mut inner = crate::sync::lock(&self.inner);
        if inner.versions.get(&key) != Some(&expected) {
            return false;
        }
        inner.accounts.insert(key, runtime);
        true
    }

    /// Called at configuration publication, not by a late result. A changed
    /// binding or panel invalidates cached observations before it becomes live.
    pub fn sync_declarations(&self, config: &crate::config::AppConfig) {
        let next: HashMap<_, _> = config
            .providers
            .iter()
            .flat_map(|provider| {
                provider.accounts.iter().map(move |account| {
                    let key = AccountKey {
                        provider_id: provider.id.clone(),
                        account_id: AccountId(account.account_id.clone()),
                    };
                    let fingerprint = declaration_fingerprint(provider, account);
                    (key, fingerprint)
                })
            })
            .collect();
        let mut inner = crate::sync::lock(&self.inner);
        let changed: Vec<_> = inner
            .declarations
            .iter()
            .filter(|(key, previous)| next.get(*key) != Some(*previous))
            .map(|(key, _)| key.clone())
            .collect();
        for key in changed {
            inner.sequence += 1;
            let sequence = inner.sequence;
            inner.versions.insert(key.clone(), sequence);
            inner.accounts.remove(&key);
        }
        inner.declarations = next;
    }
}

fn declaration_fingerprint(
    provider: &crate::config::ProviderConfig,
    account: &crate::config::AccountConfig,
) -> String {
    serde_json::to_string(&(
        account,
        &provider.base_url,
        &provider.key_ref,
        provider.enabled,
    ))
    .expect("account declarations contain no non-finite data")
}

impl Default for AccountStore {
    fn default() -> Self {
        Self::new()
    }
}
