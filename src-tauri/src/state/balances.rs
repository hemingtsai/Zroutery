//! Balances, fetched per provider and broken down per declared account.

use std::collections::BTreeMap;

use chrono::Utc;
use zroutery_core::billing::Balance;

use super::helpers::lock;
use super::views::*;
use super::Desktop;

/// The balance a provider's accounts report, added up.
///
/// The panels that publish a wallet balance publish it per *user*, and one
/// provider here may declare several accounts against the same panel. The
/// provider's figure is therefore the sum of its accounts'.
///
/// Only when they agree on the unit, though. Adding dollars to yuan produces a
/// number, and a number is not a balance: it is a figure that cannot be spent
/// and cannot be read as one. Two accounts reporting different currencies is a
/// fact about the declaration, and it is reported rather than papered over with
/// a total that means nothing.
///
/// Gated on `any(account, newapi)` to match its only caller: the panel path is
/// compiled whenever there is a panel to ask, and the two gates drifting apart
/// is a build that fails in exactly one feature combination — which is the
/// shape of defect E-115 records.
#[cfg(any(feature = "account", feature = "newapi"))]
pub(super) fn sum_account_quotas(
    quotas: &[zroutery_core::account::types::AccountQuota],
) -> Result<Balance, String> {
    let Some(first) = quotas.first() else {
        return Err("no account reported a balance".into());
    };
    let unit = first.unit.clone();
    if let Some(other) = quotas.iter().find(|q| q.unit != unit) {
        return Err(format!(
            "these accounts report different currencies ({} and {}), so their balances cannot be \
             added into one figure",
            unit, other.unit
        ));
    }

    let sum = |pick: fn(&zroutery_core::account::types::AccountQuota) -> f64| -> f64 {
        quotas.iter().map(pick).sum()
    };
    Ok(Balance {
        currency: unit,
        remaining: Some(sum(|q| q.remaining)),
        total: Some(sum(|q| q.total)),
        used: Some(sum(|q| q.used)),
    })
}

/// Whether this provider has a balance to ask for at all.
///
/// Two sources, and a provider may have either or both:
///
/// - it **publishes** a balance of its own, at a relay endpoint the probe
///   names; or
/// - it **declares accounts**, and the panel those accounts are users of holds
///   the wallet. A NewAPI relay serves `/v1` and publishes nothing there, while
///   the panel that owns the balance publishes one — to the user's access
///   token, which is the credential an account carries rather than the relay
///   key the provider carries.
///
/// The accounts half needs the account subsystem to ask, so it is only counted
/// in a build that has one. Without it, the answer is "this provider has no
/// balance", which is true of the relay and does not pretend the panel was
/// asked.
pub(super) fn has_balance_source(provider: &zroutery_core::config::ProviderConfig) -> bool {
    if provider.balance.is_supported(provider.base_depth()) {
        return true;
    }
    #[cfg(any(feature = "account", feature = "newapi"))]
    if provider.accounts.iter().any(|a| a.enabled) {
        return true;
    }
    false
}

impl Desktop {
    pub fn balances(&self) -> BTreeMap<String, BalanceStatus> {
        lock(&self.balances).clone()
    }

    /// Ask one provider how much credit is left and remember the answer.
    ///
    /// Two sources, and which one is used is decided by what the provider
    /// publishes rather than by what is convenient:
    ///
    /// - A **relay** probe — `GET /user/balance` and its relatives — is asked
    ///   when the provider is configured with one. That is the provider's own
    ///   HTTP endpoint, authenticated with the provider's key.
    /// - Otherwise, if the provider **declares accounts**, the balance comes
    ///   from the panel those accounts are users of. A NewAPI relay publishes
    ///   no balance of its own; its wallet is the panel's, and the panel
    ///   answers only to the user's access token — the credential an account
    ///   carries, not the relay key the provider carries. There was no way to
    ///   ask for that figure before, so a panel-backed provider showed no
    ///   balance at all and its "check balance" button was disabled.
    ///
    /// A failure is stored rather than thrown away: "asked and refused" is more
    /// useful on screen than a blank.
    pub async fn refresh_balance(&self, provider_id: &str) -> Result<(), String> {
        let config = self.core.config();
        let provider = config
            .provider(provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;

        // Refused up front when there is nothing to ask. A provider that
        // publishes no balance and declares no account is not asked at all,
        // and nothing is recorded against it either: "not asked" and "asked
        // and refused" are different facts, and recording the first as the
        // second would put a failure on a provider that was never contacted.
        if !has_balance_source(provider) {
            return Err(format!("{} does not publish a balance", provider.name));
        }

        let outcome = match provider.balance.probe(provider.base_depth()) {
            Some(probe) => {
                // The relay publishes a balance of its own. Ask it, with the
                // provider's key, the way it has always been asked.
                let key = self.core.api_key(provider).map_err(|e| e.to_string())?;
                self.core
                    .upstream()
                    .fetch_balance(provider, key.as_deref(), &probe)
                    .await
                    .map_err(|e| e.to_string())
            }
            None => self.panel_balance(provider).await,
        };

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

    /// The balance a provider's panel reports for the accounts it declares.
    ///
    /// The panel path, reached only when the provider publishes no relay
    /// balance of its own. A NewAPI relay is the case that matters: it serves
    /// `/v1` and publishes no balance there, while the panel that owns the
    /// wallet does publish one — to the **user's access token**, which is the
    /// credential an account carries, not the relay key the provider carries.
    ///
    /// So the figure is the sum of what the provider's accounts report, and
    /// getting it means asking the panel once per account. That is a request
    /// the user asked for by pressing the button, not a poll: nothing here
    /// runs on a timer.
    ///
    /// Gated on `any(account, newapi)` because the reconciler lives in core
    /// behind core's `account` feature, which `newapi` implies — so a build
    /// with only `newapi` can still ask, and a build with neither has no
    /// panel to ask.
    #[cfg(any(feature = "account", feature = "newapi"))]
    async fn panel_balance(
        &self,
        provider: &zroutery_core::config::ProviderConfig,
    ) -> Result<Balance, String> {
        use zroutery_core::account::AccountId;

        let declared: Vec<String> = provider
            .accounts
            .iter()
            .filter(|a| a.enabled)
            .map(|a| a.account_id.clone())
            .collect();
        if declared.is_empty() {
            return Err(format!("{} does not publish a balance", provider.name));
        }

        // Asked per provider, not per fleet: a panel that asks for one
        // provider's balance must not pay for a request to every other.
        let report = self.core.reconcile_provider_accounts(&provider.id).await;
        let failures: Vec<String> = report
            .outcomes
            .iter()
            .filter_map(|(_, account_id, outcome)| match outcome {
                zroutery_core::account::reconcile::AccountSyncOutcome::Failed { reason } => {
                    Some(format!("{account_id}: {reason}"))
                }
                _ => None,
            })
            .collect();

        let store = self.core.accounts();
        let quotas: Vec<zroutery_core::account::types::AccountQuota> = declared
            .iter()
            .filter_map(|account_id| {
                store
                    .get(&provider.id, &AccountId(account_id.clone()))
                    .and_then(|runtime| runtime.quota)
            })
            .collect();

        if quotas.is_empty() {
            // Say *why* rather than "no balance": the usual cause is a
            // credential the panel will not accept, and that is a thing the
            // user can fix.
            //
            // When the probe said why, that reason is the message. It is
            // specific — "the stored credential is a relay API key, and the
            // dashboard authenticates with the panel's access token" — and
            // wrapping it in a sentence about *no account reporting a
            // balance* would bury the one line the user can act on behind a
            // summary of what did not happen.
            return Err(if failures.is_empty() {
                format!(
                    "no account of {} reported a balance. Declare an account and store the \
                     panel's user access token as its credential — the relay's own key \
                     authenticates `/v1` and no dashboard endpoint accepts it.",
                    provider.name
                )
            } else {
                failures.join("; ")
            });
        }

        sum_account_quotas(&quotas)
    }

    /// The provider's balance, with no panel to ask.
    ///
    /// A build without the account subsystem has no way to read a panel's
    /// wallet, and saying so is better than reporting a zero that reads as
    /// "this account is empty".
    #[cfg(not(any(feature = "account", feature = "newapi")))]
    async fn panel_balance(
        &self,
        provider: &zroutery_core::config::ProviderConfig,
    ) -> Result<Balance, String> {
        Err(format!(
            "{} does not publish a balance, and this build has no account subsystem to ask its \
             panel",
            provider.name
        ))
    }

    /// Refresh every provider that has a balance to ask for, one after another.
    ///
    /// Sequential on purpose: a handful of providers, and hammering them in
    /// parallel is a good way to get rate limited.
    ///
    /// "Has a balance to ask for" is not the same as "publishes a balance of
    /// its own". A panel-backed provider publishes none and still has one, in
    /// the panel — so the sweep asks it too, through the same
    /// [`Self::refresh_balance`] the per-provider button uses, and the two
    /// cannot drift.
    pub async fn refresh_all_balances(&self) -> Vec<String> {
        let ids: Vec<String> = self
            .core
            .config()
            .providers
            .iter()
            .filter(|p| p.enabled && has_balance_source(p))
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
            // Every declared account of an enabled provider is emitted, including
            // the single-account case. The old rule — views only from two accounts
            // up, on the grounds that one account's provider-level figure is the
            // whole truth — hid the only reading a single-account operator has:
            // the probe runs, the store fills, and no panel anywhere shows it,
            // because the provider-level balance is a relay-endpoint figure that
            // most deployments do not publish. Whether one account's row is noise
            // is a layout question for the panel that renders these, not a reason
            // for the data layer to drop it.
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
