//! Activity counters, elections, and the budget ledger this process carries across runs.

use zroutery_core::billing::Cost;
use zroutery_core::budget::{BudgetPeriod, BudgetScope};
use zroutery_core::config::RoutingStrategy;
use zroutery_core::election::Election;

use super::views::*;
use super::Desktop;
use crate::store;

impl Desktop {
    pub fn activity(&self) -> Activity {
        Activity {
            health: self.core.router().health_snapshot(),
            summary: self.core.stats().summary(),
            recent: self.core.stats().recent(200),
            accounting: self.core.accounting_status(),
        }
    }

    /// Hold an election and pin the result. Costs one tiny request per model, which
    /// is why it happens on demand or at startup and never on a timer.
    pub async fn hold_election(&self) -> Election {
        self.core.hold_election().await
    }

    /// Elect at startup, when the user asked for the balanced strategy.
    ///
    /// Skipped for every other strategy: probing costs money, and an order nobody
    /// reads is not worth paying for.
    pub async fn elect_if_configured(&self) {
        let routing = self.core.config().routing.clone();
        if routing.strategy != RoutingStrategy::Balanced || !routing.elect_on_start {
            return;
        }
        for (tier, outcome) in &self.hold_election().await.tiers {
            tracing::info!(
                "{} elected for {}{}",
                outcome.winner().unwrap_or("nothing"),
                tier.virtual_id(),
                outcome
                    .note
                    .as_ref()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default()
            );
        }
    }

    /// Every budget with the spend counted against it right now.
    pub fn budget_status(&self) -> Vec<BudgetStatus> {
        let config = self.core.config();
        let ledger = self.core.ledger();
        let now = chrono::Local::now();
        config
            .budgets
            .iter()
            .map(|budget| {
                let spent = ledger.spent(budget, now);
                BudgetStatus {
                    used: if budget.limit.amount > 0.0 {
                        spent / budget.limit.amount
                    } else {
                        0.0
                    },
                    spent: Cost {
                        currency: budget.limit.currency.clone(),
                        amount: spent,
                    },
                    budget: budget.clone(),
                }
            })
            .collect()
    }

    /// What has been spent on one scope in the current windows, budget or not.
    pub fn spend_on(&self, scope: &BudgetScope) -> Vec<(BudgetPeriod, Cost)> {
        self.core.ledger().totals_for(scope, chrono::Local::now())
    }

    /// Write the ledger out if it has moved since the last time.
    ///
    /// Request settlement commits to SQLite before completion. This timer only
    /// retries local pending facts and writes legacy spend.json as a compatibility
    /// projection; it is not the authoritative accounting acknowledgement.
    pub fn flush_ledger(&self) {
        if let Err(error) = self.core.retry_accounting() {
            tracing::error!(%error, "durable accounting retry failed; no upstream replay");
            return;
        }
        if let Err(error) = self.core.stats().flush() {
            tracing::warn!(%error, "usage history flush failed; it will be retried");
        }
        let result = self
            .core
            .flush_ledger(|ledger| store::save_ledger(&self.config_dir, ledger));
        if let Err(e) = result {
            tracing::warn!("cannot write the spend ledger, will retry: {e}");
        }
    }

    /// The real token. Only reached through an explicit user action.
    pub fn auth_token(&self) -> String {
        self.core.config().server.auth_token.clone()
    }
}
