//! Empty wallets are latched, not transient circuit cooldowns. Neither a
//! success nor a balance refresh releases them; only an operator does.
//!
//! The latch is persisted because a restart, a completed cooldown, or a second
//! instance must not turn an empty wallet back into spendable traffic. The
//! database is shared by the desktop and headless instances, so the map is
//! reconciled with it at a small interval: a release pressed in one instance
//! reaches the other without a restart.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::Router;
use crate::runtime_store::RuntimeStore;

/// How often the in-memory latch map is reconciled with the shared database.
const SYNC_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletHold {
    pub provider_id: String,
    pub account_id: Option<String>,
    pub key_id: Option<String>,
    pub held_at: i64,
    pub reason: String,
}

fn scope(provider: &str, account: Option<&str>, key: Option<&str>) -> String {
    serde_json::to_string(&(provider, account, key)).expect("string tuple is serializable")
}

impl Router {
    pub fn attach_runtime_store(&self, store: Arc<RuntimeStore>) -> Result<(), String> {
        let holds = store.wallet_holds()?;
        let mut current = crate::sync::lock(&self.wallet_holds);
        for json in holds {
            let hold: WalletHold = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            current.insert(
                scope(
                    &hold.provider_id,
                    hold.account_id.as_deref(),
                    hold.key_id.as_deref(),
                ),
                hold,
            );
        }
        *crate::sync::lock(&self.runtime_store) = Some(store);
        *crate::sync::lock(&self.wallet_synced) = Some(Instant::now());
        Ok(())
    }

    /// Re-read the shared latch table when the cached copy is old enough.
    ///
    /// Called on the request path, so it must stay cheap when nothing changed:
    /// one timestamp comparison until the interval elapses.
    fn reconcile_wallets(&self, holds: &mut BTreeMap<String, WalletHold>) {
        let Some(store) = crate::sync::lock(&self.runtime_store).clone() else {
            return;
        };
        let mut synced = crate::sync::lock(&self.wallet_synced);
        if synced.is_some_and(|at| at.elapsed() < SYNC_EVERY) {
            return;
        }
        match store.wallet_holds() {
            Ok(rows) => {
                holds.clear();
                for json in rows {
                    match serde_json::from_str::<WalletHold>(&json) {
                        Ok(hold) => {
                            holds.insert(
                                scope(
                                    &hold.provider_id,
                                    hold.account_id.as_deref(),
                                    hold.key_id.as_deref(),
                                ),
                                hold,
                            );
                        }
                        Err(error) => {
                            tracing::warn!(%error, "an unreadable wallet hold was ignored");
                        }
                    }
                }
            }
            Err(error) => {
                // A latch whose state cannot be read stays enforced from the
                // last copy this process has; fail closed, never open.
                tracing::warn!(%error, "could not reconcile wallet holds");
            }
        }
        *synced = Some(Instant::now());
    }

    pub fn hold_empty_wallet(&self, provider: &str, account: Option<&str>, key: Option<&str>) {
        let hold = WalletHold {
            provider_id: provider.into(),
            account_id: account.map(str::to_string),
            key_id: key.map(str::to_string),
            held_at: chrono::Utc::now().timestamp(),
            reason: "balance_exhausted".into(),
        };
        let id = scope(provider, account, key);
        // Serialize all mutations so a concurrent manual release cannot race
        // an older INSERT back onto disk.
        let mut holds = crate::sync::lock(&self.wallet_holds);
        if let Some(store) = crate::sync::lock(&self.runtime_store).as_ref() {
            if let Err(error) = store.hold_wallet(
                &id,
                &serde_json::to_string(&hold).expect("wallet hold is serializable"),
            ) {
                tracing::error!(%error, "empty wallet is blocked in memory but could not be persisted");
            }
        }
        holds.entry(id).or_insert(hold);
        *crate::sync::lock(&self.wallet_synced) = Some(Instant::now());
    }

    pub fn release_wallet(
        &self,
        provider: &str,
        account: Option<&str>,
        key: Option<&str>,
    ) -> Result<(), String> {
        let id = scope(provider, account, key);
        let mut holds = crate::sync::lock(&self.wallet_holds);
        if let Some(store) = crate::sync::lock(&self.runtime_store).as_ref() {
            store.release_wallet(&id)?;
        }
        holds.remove(&id);
        *crate::sync::lock(&self.wallet_synced) = Some(Instant::now());
        Ok(())
    }

    pub(super) fn wallet_held(
        &self,
        provider: &str,
        account: Option<&str>,
        key: Option<&str>,
    ) -> bool {
        let id = scope(provider, account, key);
        let mut holds = crate::sync::lock(&self.wallet_holds);
        self.reconcile_wallets(&mut holds);
        holds.contains_key(&id)
    }

    pub fn wallet_holds(&self) -> Vec<WalletHold> {
        let mut holds = crate::sync::lock(&self.wallet_holds);
        self.reconcile_wallets(&mut holds);
        holds.values().cloned().collect()
    }

    /// Drop latches belonging to providers that no longer exist.
    ///
    /// Deleting a provider is not a release of its wallet; it is the removal of
    /// the thing the wallet belonged to. Keeping the row would block a newly
    /// added provider that happened to reuse the id.
    pub(crate) fn retain_wallet_providers(&self, known: impl Fn(&str) -> bool) {
        let mut holds = crate::sync::lock(&self.wallet_holds);
        let stale: Vec<(String, String)> = holds
            .iter()
            .filter(|(_, hold)| !known(&hold.provider_id))
            .map(|(id, hold)| (id.clone(), hold.provider_id.clone()))
            .collect();
        if stale.is_empty() {
            return;
        }
        holds.retain(|_, hold| known(&hold.provider_id));
        if let Some(store) = crate::sync::lock(&self.runtime_store).as_ref() {
            for (id, provider) in &stale {
                if let Err(error) = store.release_wallet(id) {
                    tracing::warn!(
                        %error,
                        provider,
                        "a removed provider's wallet latch could not be deleted"
                    );
                }
            }
        }
        *crate::sync::lock(&self.wallet_synced) = Some(Instant::now());
    }
}
