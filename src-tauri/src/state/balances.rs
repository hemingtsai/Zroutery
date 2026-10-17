//! Balances, fetched per provider and broken down per account only where the total hides something.

use std::collections::BTreeMap;

use chrono::Utc;

use super::helpers::lock;
use super::views::*;
use super::Desktop;

impl Desktop {
    pub fn balances(&self) -> BTreeMap<String, BalanceStatus> {
        lock(&self.balances).clone()
    }

    /// Ask one provider how much credit is left and remember the answer.
    ///
    /// A failure is stored rather than thrown away: "asked and refused" is more
    /// useful on screen than a blank.
    pub async fn refresh_balance(&self, provider_id: &str) -> Result<(), String> {
        let config = self.core.config();
        let provider = config
            .provider(provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let probe = provider
            .balance
            .probe(provider.base_depth())
            .ok_or_else(|| format!("{} does not publish a balance", provider.name))?;

        let key = self.core.api_key(provider).map_err(|e| e.to_string())?;
        let outcome = self
            .core
            .upstream()
            .fetch_balance(provider, key.as_deref(), &probe)
            .await;

        let status = match outcome {
            Ok(balance) => BalanceStatus {
                checked_at: Utc::now(),
                balance: Some(balance),
                error: None,
            },
            Err(e) => BalanceStatus {
                checked_at: Utc::now(),
                balance: None,
                error: Some(e.to_string()),
            },
        };
        let failed = status.error.clone();
        lock(&self.balances).insert(provider_id.to_string(), status);
        match failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Refresh every provider that publishes a balance, one after another.
    ///
    /// Sequential on purpose: a handful of providers, and hammering them in
    /// parallel is a good way to get rate limited.
    pub async fn refresh_all_balances(&self) -> Vec<String> {
        let ids: Vec<String> = self
            .core
            .config()
            .providers
            .iter()
            .filter(|p| p.enabled && p.balance.is_supported(p.base_depth()))
            .map(|p| p.id.clone())
            .collect();

        let mut problems = Vec::new();
        for id in ids {
            if let Err(e) = self.refresh_balance(&id).await {
                problems.push(format!("{id}: {e}"));
            }
        }
        problems
    }

    /// One account's state, as a panel receives it.
    ///
    /// A projection rather than the core type, for the reason the check-in phase is a
    /// named string: the webview gets a vocabulary that a `#[serde(rename_all)]`
    /// change cannot move underneath it.
    #[cfg(feature = "newapi")]
    pub fn account_balance_views(&self) -> Vec<AccountBalanceView> {
        use zroutery_core::account::AccountStatus;
        let store = self.core.accounts();
        let mut views = Vec::new();
        for provider in &self.core.config().providers {
            // **Only where the provider-level figure is not the whole truth.** With
            // one account the two levels say the same thing, and showing both is
            // noise; with several, the provider-level number is a total that hides
            // which account is empty and which is suspended.
            if provider.accounts.len() < 2 {
                continue;
            }
            for declared in &provider.accounts {
                let Some(runtime) = store.get(
                    &provider.id,
                    &zroutery_core::account::AccountId(declared.account_id.clone()),
                ) else {
                    // Declared but never probed. Rendered as absent rather than as a
                    // zero balance, because "we do not know" and "it is empty" are
                    // different facts and a zero would claim the second.
                    views.push(AccountBalanceView {
                        provider_id: provider.id.clone(),
                        account_id: declared.account_id.clone(),
                        status: "unprobed".into(),
                        remaining: None,
                        total: None,
                        used: None,
                        unit: None,
                        last_sync: None,
                        last_failure: None,
                    });
                    continue;
                };
                let quota = runtime.quota.as_ref();
                views.push(AccountBalanceView {
                    provider_id: provider.id.clone(),
                    account_id: declared.account_id.clone(),
                    status: match runtime.status {
                        AccountStatus::Active => "active",
                        AccountStatus::Suspended => "suspended",
                        AccountStatus::QuotaExhausted => "quota_exhausted",
                        AccountStatus::RateLimited => "rate_limited",
                        AccountStatus::AuthenticationExpired => "authentication_expired",
                        // `Unknown` is carried through as itself rather than as a
                        // failure: the reconciler publishes Unknown for "did not
                        // answer", which is not evidence that anything is wrong.
                        AccountStatus::Unknown => "unknown",
                    }
                    .to_string(),
                    remaining: quota.map(|q| q.remaining),
                    total: quota.map(|q| q.total),
                    used: quota.map(|q| q.used),
                    unit: quota.map(|q| q.unit.clone()),
                    last_sync: runtime.last_sync,
                    last_failure: runtime.last_failure,
                });
            }
        }
        views
    }
}
