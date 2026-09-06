//! Thread-safe account store.

use std::collections::HashMap;
use std::sync::Mutex;
use super::types::*;

/// Composite key: provider + account.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccountKey {
    provider_id: String,
    account_id: AccountId,
}

/// Thread-safe store for account runtime states.
#[derive(Debug)]
pub struct AccountStore {
    accounts: Mutex<HashMap<AccountKey, AccountRuntime>>,
}

impl AccountStore {
    pub fn new() -> Self {
        Self { accounts: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, provider_id: &str, account_id: &AccountId) -> Option<AccountRuntime> {
        let key = AccountKey { provider_id: provider_id.into(), account_id: account_id.clone() };
        crate::sync::lock(&self.accounts).get(&key).cloned()
    }

    pub fn upsert(&self, runtime: AccountRuntime) {
        let key = AccountKey {
            provider_id: runtime.provider_id.clone(),
            account_id: runtime.account_id.clone(),
        };
        crate::sync::lock(&self.accounts).insert(key, runtime);
    }

    pub fn remove(&self, provider_id: &str, account_id: &AccountId) -> bool {
        let key = AccountKey { provider_id: provider_id.into(), account_id: account_id.clone() };
        crate::sync::lock(&self.accounts).remove(&key).is_some()
    }

    pub fn list_by_provider(&self, provider_id: &str) -> Vec<AccountRuntime> {
        crate::sync::lock(&self.accounts)
            .iter()
            .filter(|(k, _)| k.provider_id == provider_id)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

impl Default for AccountStore {
    fn default() -> Self { Self::new() }
}
