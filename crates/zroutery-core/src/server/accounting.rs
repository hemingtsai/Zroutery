//! Durable settlement composition. IO failures never replay an upstream send.
//! Lock order: ledger_flush → accounting → SQLite → ledger/stats projection.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use super::AppState;
use crate::accounting::{AccountingStatus, AttemptFact, PersistenceState, SettlementFact};
use crate::budget::Ledger;
use crate::error::{Error, Result};
use crate::runtime_store::RuntimeStore;

#[derive(Default)]
pub(super) struct AccountingRuntime {
    store: Option<Arc<RuntimeStore>>,
    pending_attempts: BTreeMap<String, AttemptFact>,
    pending_settlements: BTreeMap<String, SettlementFact>,
    /// In memory-only Core mode this also prevents duplicate charges.
    charged: std::collections::HashSet<String>,
    settled: std::collections::HashSet<String>,
    status: AccountingStatus,
}

impl AppState {
    /// Desktop must explicitly require storage before attachment. A failure
    /// then blocks serving, rather than silently becoming volatile Core mode.
    pub fn require_accounting(&self) {
        let mut runtime = crate::sync::lock(&self.accounting);
        runtime.status.required = true;
        runtime.status.available = runtime.store.is_some();
    }

    pub fn accounting_failed(&self, reason: String) {
        let mut runtime = crate::sync::lock(&self.accounting);
        runtime.status.error = Some(reason);
        runtime.status.available = false;
    }

    pub fn attach_accounting(
        &self,
        store: Arc<RuntimeStore>,
        opening: &Ledger,
        source: &str,
    ) -> std::result::Result<(), String> {
        let _flush = crate::sync::lock(&self.ledger_flush);
        let mut runtime = crate::sync::lock(&self.accounting);
        if runtime.store.is_some() {
            return Err("durable accounting is already attached".into());
        }
        let ledger = store.initialize_accounting(opening, source)?;
        let unresolved = store.unresolved_attempts()?.len();
        self.stats.attach_store(Arc::clone(&store))?;
        self.publish_accounting_ledger(ledger);
        runtime.store = Some(store);
        runtime.status.durable = true;
        runtime.status.available = true;
        runtime.status.unreconciled_attempts = unresolved;
        runtime.status.error = None;
        Ok(())
    }

    pub fn accounting_status(&self) -> AccountingStatus {
        let runtime = crate::sync::lock(&self.accounting);
        let mut status = runtime.status.clone();
        status.pending_settlements = runtime.pending_settlements.len();
        status.pending_attempts = runtime.pending_attempts.len();
        if !status.required && runtime.store.is_none() {
            status.available = true;
        }
        status
    }

    /// This is a local prerequisite, never a provider health or fallback event.
    pub fn accounting_ready(&self) -> Result<()> {
        let status = self.accounting_status();
        if !status.available || status.pending_attempts != 0 || status.pending_settlements != 0 {
            return Err(Error::Persistence);
        }
        Ok(())
    }

    pub(super) fn persist_attempt(&self, attempt: &AttemptFact) -> Result<()> {
        let _flush = crate::sync::lock(&self.ledger_flush);
        let mut runtime = crate::sync::lock(&self.accounting);
        if let Some(store) = &runtime.store {
            let started = Instant::now();
            match store.record_attempt(attempt) {
                Ok(ledger) => {
                    self.publish_accounting_ledger(ledger);
                    runtime.pending_attempts.remove(&attempt.id);
                    runtime.status.commits += 1;
                    runtime.status.last_commit_wait_ms =
                        Some(started.elapsed().as_secs_f64() * 1000.0);
                    return Ok(());
                }
                Err(error) => {
                    tracing::error!(attempt_id = %attempt.id, %error, "attempt evidence could not be persisted; no upstream replay");
                    runtime
                        .pending_attempts
                        .insert(attempt.id.clone(), attempt.clone());
                    runtime.status.error = Some(error);
                    runtime.status.available = false;
                    return Err(Error::Persistence);
                }
            }
        }
        if runtime.status.required {
            return Err(Error::Persistence);
        }
        if let Some(cost) = &attempt.cost {
            if runtime.charged.insert(attempt.id.clone()) {
                crate::sync::write(&self.ledger).charge_windows(
                    &attempt.budget_day,
                    &attempt.budget_month,
                    &attempt.execution.provider_id,
                    attempt.execution.tier,
                    cost,
                );
            }
        }
        Ok(())
    }

    pub(super) fn settle_request(&self, fact: &mut SettlementFact) -> Result<()> {
        let _flush = crate::sync::lock(&self.ledger_flush);
        let mut runtime = crate::sync::lock(&self.accounting);
        if let Some(store) = &runtime.store {
            if let Some(accounting) = &mut fact.request.accounting {
                accounting.persistence = PersistenceState::Durable;
            }
            let started = Instant::now();
            match store.settle(fact) {
                Ok(commit) => {
                    self.publish_accounting_ledger(commit.ledger);
                    self.stats.record_committed(
                        fact.request.clone(),
                        commit.usage_before,
                        commit.usage_after,
                        commit.inserted,
                    );
                    runtime.pending_settlements.remove(&fact.request.id);
                    for attempt in &fact.attempts {
                        runtime.pending_attempts.remove(&attempt.id);
                    }
                    runtime.status.commits += 1;
                    runtime.status.last_commit_wait_ms =
                        Some(started.elapsed().as_secs_f64() * 1000.0);
                    if runtime.pending_attempts.is_empty() && runtime.pending_settlements.is_empty()
                    {
                        runtime.status.available = true;
                        runtime.status.error = None;
                    }
                    return Ok(());
                }
                Err(error) => {
                    tracing::error!(request_id = %fact.request.id, %error, "terminal settlement did not commit; upstream will not be replayed");
                    // Preserve the exact intended transaction for uncertain-commit
                    // retries. The UI's local failure is not a new upstream fact.
                    runtime
                        .pending_settlements
                        .insert(fact.request.id.clone(), fact.clone());
                    if let Some(accounting) = &mut fact.request.accounting {
                        accounting.persistence = PersistenceState::Pending;
                    }
                    fact.request.ok = false;
                    fact.request.status = Error::Persistence.status().as_u16();
                    fact.request.error = Some(Error::Persistence.safe_message());
                    runtime.status.error = Some(error);
                    runtime.status.available = false;
                    self.stats.record_settlement_pending(fact.request.clone());
                    return Err(Error::Persistence);
                }
            }
        }
        if runtime.status.required {
            return Err(Error::Persistence);
        }
        if runtime.settled.insert(fact.request.id.clone()) {
            for attempt in &fact.attempts {
                if let Some(cost) = &attempt.cost {
                    if runtime.charged.insert(attempt.id.clone()) {
                        crate::sync::write(&self.ledger).charge_windows(
                            &attempt.budget_day,
                            &attempt.budget_month,
                            &attempt.execution.provider_id,
                            attempt.execution.tier,
                            cost,
                        );
                    }
                }
            }
            self.stats.record(fact.request.clone());
        }
        Ok(())
    }

    /// Retry only local facts. No provider call, token lookup, training or health
    /// update is made. Stable IDs make an uncertain commit safe to retry.
    pub fn retry_accounting(&self) -> Result<()> {
        let (attempts, settlements) = {
            let runtime = crate::sync::lock(&self.accounting);
            (
                runtime
                    .pending_attempts
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
                runtime
                    .pending_settlements
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        for attempt in attempts {
            self.persist_attempt(&attempt)?;
        }
        for mut settlement in settlements {
            self.settle_request(&mut settlement)?;
        }
        let mut runtime = crate::sync::lock(&self.accounting);
        if runtime.store.is_some()
            && runtime.pending_attempts.is_empty()
            && runtime.pending_settlements.is_empty()
        {
            runtime.status.available = true;
            runtime.status.error = None;
        }
        drop(runtime);
        self.accounting_ready()
    }

    /// Legacy spend.json is only a projection, but it must advance for an
    /// explicitly controlled development handoff to a compatible fallback.
    fn publish_accounting_ledger(&self, next: Ledger) {
        let mut ledger = crate::sync::write(&self.ledger);
        if *ledger != next {
            *ledger = next;
            self.ledger_generation.fetch_add(1, Ordering::AcqRel);
            self.ledger_dirty.store(true, Ordering::Release);
        }
    }
}
