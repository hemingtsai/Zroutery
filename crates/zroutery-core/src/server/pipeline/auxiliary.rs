//! One auxiliary send keeps its own durable identity, usage and cancellation.
//!
//! [`AuxiliaryAttempt::begin`] persists the InFlight fact *before* the send, so
//! the bound execution snapshot is durable evidence of what was about to be
//! asked; [`AuxiliaryAttempt::finish`] and [`Drop`] complete that same fact.
//! Everything between the two is the send itself: a caller must resolve every
//! local preparation step — request encoding, key resolution, console
//! credential refresh — before `begin`, because `sent` is part of the
//! immutable identity (`AttemptFact::same_execution`) and a preparation
//! failure never reached an upstream, so it must never produce a fact at all.
use super::AppState;
use crate::accounting::{AttemptFact, AttemptState};
use crate::error::Result;
use crate::ir::AttemptObservation;
use crate::router::Candidate;

pub(super) struct AuxiliaryAttempt<'a> {
    state: &'a AppState,
    fact: AttemptFact,
    observation: AttemptObservation,
    finished: bool,
}

impl<'a> AuxiliaryAttempt<'a> {
    /// Bind one durable attempt to one send.
    ///
    /// The InFlight fact carries `sent = true` from here on — that flag is
    /// part of the identity the store refuses to move, so it can only be
    /// written truthfully if `begin` happens after every step that can fail
    /// without reaching an upstream. The encode, the key and the console
    /// credential belong before this call.
    pub fn begin(
        state: &'a AppState,
        candidate: &Candidate,
        observation: AttemptObservation,
    ) -> Result<Self> {
        state.accounting_ready()?;
        let mut fact = AttemptFact::new(
            format!("att_{}", uuid::Uuid::new_v4().simple()),
            None,
            candidate,
            true,
        );
        fact.bind_iteration_pricing(&state.config(), candidate);
        state.persist_attempt(&fact)?;
        Ok(Self {
            state,
            fact,
            observation,
            finished: false,
        })
    }

    /// Complete the attempt with the send's outcome.
    ///
    /// Idempotent: after the first call the fact is terminal, and a second
    /// call rechecks durability without changing the fact. Re-running `capture` on a
    /// meanwhile-arrived transport observation would write a *different*
    /// final fact under the same ID, which the store must refuse — turning a
    /// settled attempt into a false persistence failure and dragging the
    /// whole accounting into pending. The first verdict stands; durability of
    /// a verdict whose persist failed is `retry_accounting`'s job, not a
    /// second finish's.
    pub fn finish(&mut self, result: &Result<crate::ir::ChatResponse>) -> Result<()> {
        if self.finished {
            return self.state.accounting_ready();
        }
        if let Ok(response) = result {
            self.observation
                .observe_decoded(&Default::default(), response.usage);
            if let Some(declaration) = crate::ir::declaration_from_passthrough(response) {
                self.fact
                    .declared
                    .get_or_insert_with(Default::default)
                    .merge(&declaration);
            }
        }
        self.capture();
        self.fact.failure_class = result.as_ref().err().map(|error| error.classified().class);
        self.fact.finish(if result.is_ok() {
            AttemptState::Served
        } else {
            AttemptState::Failed
        });
        self.finished = true;
        self.state.persist_attempt(&self.fact)
    }

    fn capture(&mut self) {
        let observed = self.observation.snapshot();
        self.fact.observe_response(&observed);
    }
}

impl Drop for AuxiliaryAttempt<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.capture();
        self.fact.finish(AttemptState::Cancelled);
        let _ = self.state.persist_attempt(&self.fact);
    }
}

#[cfg(test)]
mod tests {
    //! The helper's own durable contract, against a real file-backed store:
    //! one immutable identity per send, one charge per execution, honest
    //! unknowns, and pending evidence that only a local retry completes.
    use super::*;
    use crate::billing::Pricing;
    use crate::budget::{Budget, BudgetPeriod, BudgetScope};
    use crate::config::{
        AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
    };
    use crate::error::Error;
    use crate::ir::{ChatResponse, ContentBlock, StopReason, Usage};
    use crate::runtime_store::RuntimeStore;
    use std::path::Path;
    use std::sync::Arc;

    const DESCRIPTION_TEXT: &str = "A chart with a rising line, titled \"Revenue\".";

    fn token_pricing() -> Pricing {
        Pricing::new("USD", 100.0, 100.0)
    }

    fn candidate(pricing: Pricing) -> Candidate {
        let mut model = ModelEntry::for_upstream("v", "eyes-model", Some(ModelTier::Fast));
        model.display_name = Some("Readable eyes".into());
        model.pricing = Some(pricing);
        Candidate {
            exposed_id: model.exposed_id(),
            entry: model,
            provider: ProviderConfig::new("v", "V", ProviderKind::OpenAICompatible),
            degraded: false,
            credential: None,
            rate_quote: None,
        }
    }

    fn usage() -> Usage {
        Usage {
            input_tokens: 10_000,
            output_tokens: 200,
            ..Default::default()
        }
    }

    fn answered(usage: Usage) -> Result<crate::ir::ChatResponse> {
        Ok(ChatResponse {
            id: "resp".into(),
            model: "eyes-model".into(),
            content: vec![ContentBlock::text(DESCRIPTION_TEXT)],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage,
            passthrough: Default::default(),
        })
    }

    fn state_with_store(path: &Path) -> (AppState, Arc<RuntimeStore>) {
        let store = Arc::new(RuntimeStore::open(path).unwrap());
        let state = AppState::new(AppConfig::default(), Arc::new(MemorySecretStore::new()));
        state
            .attach_accounting(
                Arc::clone(&store),
                &crate::budget::Ledger::new(),
                "helper test",
            )
            .unwrap();
        (state, store)
    }

    /// The one fact an auxiliary attempt writes, read back from the disk.
    fn stored_fact(path: &Path, id: &str) -> crate::accounting::AttemptFact {
        let db = rusqlite::Connection::open(path).unwrap();
        let json: String = db
            .query_row(
                "SELECT record FROM attempt_facts WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&json).unwrap()
    }

    fn charge_count(path: &Path) -> usize {
        let db = rusqlite::Connection::open(path).unwrap();
        db.query_row("SELECT count(*) FROM attempt_charges", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap() as usize
    }

    fn spent_today(state: &AppState, scope: BudgetScope) -> f64 {
        state.ledger().spent(
            &Budget::new(scope, BudgetPeriod::Day, "USD", f64::MAX),
            chrono::Local::now(),
        )
    }

    /// One send: the InFlight evidence written before the send, the terminal
    /// fact after it, one charge, and an identity that never moved.
    #[test]
    fn one_send_is_one_immutable_identity_and_one_charge() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, store) = state_with_store(&path);

        let observation = AttemptObservation::default();
        let mut attempt =
            AuxiliaryAttempt::begin(&state, &candidate(token_pricing()), observation.clone())
                .unwrap();
        let fact_id = attempt.fact.id.clone();
        let before = stored_fact(&path, &fact_id);
        assert_eq!(before.state, AttemptState::InFlight);
        assert!(
            before.sent,
            "the pre-send record claims the send it is bound to"
        );
        assert_eq!(before.usage, None);
        assert_eq!(before.cost, None);
        assert_eq!(before.request_id, None);

        // The transport observed the metadata while the send was in flight.
        observation.observe(&Default::default(), Some(usage()));
        attempt.finish(&answered(usage())).unwrap();

        let after = stored_fact(&path, &fact_id);
        assert_eq!(after.state, AttemptState::Served);
        assert_eq!(after.usage, Some(usage()));
        assert_eq!(after.id, before.id);
        assert_eq!(
            after.execution, before.execution,
            "the bound quote never moves"
        );
        assert_eq!(after.started_at, before.started_at);
        assert_eq!(after.sent, before.sent);
        let expected = candidate(token_pricing())
            .settled_pricing()
            .unwrap()
            .cost_of(&usage())
            .amount;
        assert_eq!(after.cost.as_ref().unwrap().amount, expected);
        assert_eq!(charge_count(&path), 1, "one execution, one charge");
        assert_eq!(
            spent_today(&state, BudgetScope::Provider { id: "v".into() }),
            expected
        );

        // A repeated finish is not a second observation and not a second charge.
        attempt.finish(&answered(usage())).unwrap();
        assert_eq!(charge_count(&path), 1);
        assert_eq!(stored_fact(&path, &fact_id), after);

        // Reopening the storage reads the same settled fact and ledger.
        let durable_ledger = store.accounting_ledger().unwrap().unwrap();
        drop(store);
        let reopened = RuntimeStore::open(&path).unwrap();
        assert!(reopened.unresolved_attempts().unwrap().is_empty());
        assert_eq!(
            reopened.accounting_ledger().unwrap().unwrap(),
            durable_ledger
        );
        assert_eq!(
            durable_ledger.spent(
                &Budget::new(
                    BudgetScope::Provider { id: "v".into() },
                    BudgetPeriod::Day,
                    "USD",
                    f64::MAX
                ),
                chrono::Local::now(),
            ),
            expected
        );
    }

    /// An upstream rejection is still an execution whose observed usage and
    /// computed cost survive as a Failed attempt, charged once.
    #[test]
    fn a_failed_send_keeps_its_observed_usage_and_charges_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, _store) = state_with_store(&path);

        let observation = AttemptObservation::default();
        let mut attempt =
            AuxiliaryAttempt::begin(&state, &candidate(token_pricing()), observation.clone())
                .unwrap();
        let fact_id = attempt.fact.id.clone();
        observation.observe(&Default::default(), Some(usage()));
        attempt
            .finish(&Err(Error::Upstream {
                provider: "V".into(),
                status: 500,
                body: "vision model exploded".into(),
                retry_after: None,
            }))
            .unwrap();

        let fact = stored_fact(&path, &fact_id);
        assert_eq!(fact.state, AttemptState::Failed);
        assert!(fact.sent);
        assert_eq!(fact.usage, Some(usage()));
        assert!(
            fact.cost.is_some(),
            "a known cost of a failed attempt is kept"
        );
        assert!(fact.failure_class.is_some());
        assert_eq!(charge_count(&path), 1);
        assert!(spent_today(&state, BudgetScope::Provider { id: "v".into() }) > 0.0);
    }

    /// A send that reported no usage stays unknown: no tokens, no charge, no
    /// invented zero. The ledger is untouched.
    #[test]
    fn an_unreported_usage_stays_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, _store) = state_with_store(&path);

        let mut attempt = AuxiliaryAttempt::begin(
            &state,
            &candidate(token_pricing()),
            AttemptObservation::default(),
        )
        .unwrap();
        let fact_id = attempt.fact.id.clone();
        attempt.finish(&answered(Usage::default())).unwrap();

        let fact = stored_fact(&path, &fact_id);
        assert_eq!(fact.state, AttemptState::Served);
        assert_eq!(
            fact.usage, None,
            "an all-zero report is not a measured zero"
        );
        assert_eq!(fact.cost, None);
        assert_eq!(charge_count(&path), 0);
        assert_eq!(
            spent_today(&state, BudgetScope::Provider { id: "v".into() }),
            0.0
        );
    }

    /// A per-request tariff charges what its quote names for a completed send
    /// even when the upstream reported no token usage — the completed response
    /// is the evidence of the execution the tariff prices. A cancellation
    /// without any usage evidence stays unknown: the drop must not infer a
    /// remote charge it never observed.
    #[test]
    fn a_per_request_tariff_charges_a_completed_send_but_never_an_unobserved_cancel() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, _store) = state_with_store(&path);

        let mut pricing = token_pricing();
        pricing.per_request = Some(0.5);
        let priced = candidate(pricing);

        let mut attempt =
            AuxiliaryAttempt::begin(&state, &priced, AttemptObservation::default()).unwrap();
        let served_id = attempt.fact.id.clone();
        attempt.finish(&answered(Usage::default())).unwrap();
        let served = stored_fact(&path, &served_id);
        assert_eq!(served.cost.as_ref().unwrap().amount, 0.5);
        assert_eq!(charge_count(&path), 1);

        let cancelled =
            AuxiliaryAttempt::begin(&state, &priced, AttemptObservation::default()).unwrap();
        let cancelled_id = cancelled.fact.id.clone();
        drop(cancelled);
        let dropped = stored_fact(&path, &cancelled_id);
        assert_eq!(dropped.state, AttemptState::Cancelled);
        assert_eq!(dropped.usage, None);
        assert_eq!(
            dropped.cost, None,
            "no observed evidence, no inferred charge"
        );
        assert_eq!(charge_count(&path), 1, "the cancellation added no charge");
    }

    /// Cancelling after the transport observed metadata preserves the known
    /// cost of the work the upstream may already have done, exactly once.
    #[test]
    fn a_cancel_after_observed_metadata_keeps_its_known_cost() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, _store) = state_with_store(&path);

        let observation = AttemptObservation::default();
        let attempt =
            AuxiliaryAttempt::begin(&state, &candidate(token_pricing()), observation.clone())
                .unwrap();
        let fact_id = attempt.fact.id.clone();
        observation.observe(&Default::default(), Some(usage()));
        drop(attempt);

        let fact = stored_fact(&path, &fact_id);
        assert_eq!(fact.state, AttemptState::Cancelled);
        assert_eq!(fact.usage, Some(usage()));
        assert!(fact.cost.is_some());
        assert_eq!(charge_count(&path), 1);
        let charged = fact.cost.unwrap().amount;
        assert_eq!(
            spent_today(&state, BudgetScope::Provider { id: "v".into() }),
            charged
        );
    }

    /// A local persistence failure retains its pending evidence and is
    /// completed only by a local retry; a `finish` whose persist failed is
    /// never overwritten by the `Drop` that follows it.
    ///
    /// The failure is a real SQLite one: a second connection holds the write
    /// lock past the store's busy timeout, the same contention a busy disk
    /// produces. The store then waits out its five second budget twice, once
    /// per window.
    #[test]
    fn a_local_persistence_failure_is_retried_locally_and_never_replays_the_send() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, store) = state_with_store(&path);

        // Window 1: `begin` cannot write its pre-send evidence.
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(matches!(
            AuxiliaryAttempt::begin(
                &state,
                &candidate(token_pricing()),
                AttemptObservation::default()
            ),
            Err(Error::Persistence)
        ));
        let status = state.accounting_status();
        assert!(!status.available);
        assert_eq!(status.pending_attempts, 1);
        blocker.execute_batch("COMMIT").unwrap();

        state.retry_accounting().unwrap();
        let unresolved = store.unresolved_attempts().unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(
            unresolved[0].state,
            AttemptState::InFlight,
            "recovery completes nothing the send never did"
        );
        assert!(state.accounting_status().available);

        // Window 2: the finish of a real send cannot commit. The helper is
        // then dropped — and the Drop must not cancel what already finished.
        let observation = AttemptObservation::default();
        let mut attempt =
            AuxiliaryAttempt::begin(&state, &candidate(token_pricing()), observation.clone())
                .unwrap();
        let fact_id = attempt.fact.id.clone();
        observation.observe(&Default::default(), Some(usage()));
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(matches!(
            attempt.finish(&answered(usage())),
            Err(Error::Persistence)
        ));
        assert!(
            matches!(
                attempt.finish(&answered(Usage::default())),
                Err(Error::Persistence)
            ),
            "a repeated finish cannot turn pending durability into success"
        );
        drop(attempt);
        assert_eq!(state.accounting_status().pending_attempts, 1);
        blocker.execute_batch("COMMIT").unwrap();

        state.retry_accounting().unwrap();
        let fact = stored_fact(&path, &fact_id);
        assert_eq!(
            fact.state,
            AttemptState::Served,
            "the drop after a failed finish must not turn the fact into a cancellation"
        );
        assert_eq!(fact.usage, Some(usage()));
        assert_eq!(charge_count(&path), 1, "one retry, one charge");
        let expected = candidate(token_pricing())
            .settled_pricing()
            .unwrap()
            .cost_of(&usage())
            .amount;
        assert_eq!(
            spent_today(&state, BudgetScope::Provider { id: "v".into() }),
            expected
        );
        assert_eq!(
            store.unresolved_attempts().unwrap().len(),
            1,
            "only the orphaned InFlight attempt from window 1 remains"
        );

        // A retry that runs again on the same pending set adds nothing, and
        // reopening reads exactly what the retry committed.
        state.retry_accounting().unwrap();
        assert_eq!(charge_count(&path), 1, "the second retry adds nothing");
        let durable_ledger = store.accounting_ledger().unwrap().unwrap();
        let reopened = RuntimeStore::open(&path).unwrap();
        assert_eq!(
            reopened.accounting_ledger().unwrap().unwrap(),
            durable_ledger
        );
        assert_eq!(
            reopened
                .unresolved_attempts()
                .unwrap()
                .iter()
                .filter(|fact| fact.state == AttemptState::InFlight)
                .count(),
            1
        );
    }

    /// Without durable accounting the helper refuses to begin at all: no
    /// fact, no charge, no send recorded as if it were settled. Explicit
    /// memory-only Core mode stays a legitimate mode and begins normally.
    #[test]
    fn begin_refuses_when_accounting_cannot_persist() {
        let state = AppState::new(AppConfig::default(), Arc::new(MemorySecretStore::new()));
        state.require_accounting();
        assert!(matches!(
            AuxiliaryAttempt::begin(
                &state,
                &candidate(token_pricing()),
                AttemptObservation::default()
            ),
            Err(Error::Persistence)
        ));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite3");
        let (state, _store) = state_with_store(&path);
        state.accounting_failed("injected storage failure".into());
        assert!(matches!(
            AuxiliaryAttempt::begin(
                &state,
                &candidate(token_pricing()),
                AttemptObservation::default()
            ),
            Err(Error::Persistence)
        ));
        let db = rusqlite::Connection::open(&path).unwrap();
        let facts: i64 = db
            .query_row("SELECT count(*) FROM attempt_facts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(facts, 0);
    }
}
