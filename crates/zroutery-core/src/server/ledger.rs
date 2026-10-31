//! Construction, the spend ledger, and the budget admission that gates on it.
use super::shadow_candidate::*;
use super::state::AdmissionGates;
use super::state::{AdmissionGuard, AppState};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use chrono::Local;

use crate::billing::Cost;
use crate::budget::{self, BudgetScope, Ledger, Verdict};
use crate::config::{AppConfig, ModelTier, SecretStore};
use crate::ir::ResponseStore;
use crate::registry::Registry;
use crate::router::Router;
use crate::stats::{OutcomeLog, Stats};
use crate::upstream::Upstream;

use super::projection_log::ProjectionLog;

impl AppState {
    pub fn new(config: AppConfig, secrets: Arc<dyn SecretStore>) -> Self {
        #[cfg(feature = "ml")]
        let attachment = match ShadowAttachment::embedded() {
            Ok(attached) => attached,
            Err(error) => {
                // Fail closed to the previous behaviour and say so loudly: the
                // shadow runs with no named candidate, which is exactly what it
                // did before this node existed. It is never a panic and never a
                // partially-attached predictor, because a diagnostic capability
                // that cannot be verified is not a capability.
                tracing::error!(
                    error = %error,
                    "the embedded shadow candidate artifact did not verify; running with the \
                     shadow candidate withdrawn"
                );
                ShadowAttachment::withdrawn()
            }
        };
        // Without the ML feature there is no shadow and no candidate to name, so
        // the only state this build can be in is the withdrawn one. That is a
        // real answer, not a stub: the desktop application compiles no ML stack
        // and therefore has nothing to shadow.
        #[cfg(not(feature = "ml"))]
        let attachment = ShadowAttachment::withdrawn();
        Self::with_shadow_attachment(config, secrets, attachment)
    }

    /// Build the state with an explicitly chosen shadow attachment.
    ///
    /// The production constructor is [`Self::new`], which attaches the embedded
    /// candidate where the feature exists and withdraws it where it does not.
    /// This one takes the attachment as a value so a test can state "no
    /// candidate" and compare the served bytes against the attached case, which
    /// is the whole of what proving the shadow changes nothing served means.
    ///
    /// Public without the `ml` feature too. A build with no ML stack still has a
    /// shadow attachment — a withdrawn one — and keeping one constructor for
    /// both builds is what stops the two from drifting apart.
    pub fn with_shadow_attachment(
        config: AppConfig,
        secrets: Arc<dyn SecretStore>,
        attachment: ShadowAttachment,
    ) -> Self {
        #[cfg(feature = "ml")]
        {
            let ml_routing = crate::ml::MlRouter::new(
                crate::ml::DecisionEngine::new(
                    crate::ml::CoordinatorConfig::default(),
                    config.ml_routing_reward_policy(),
                ),
                crate::ml::ExplorationConfig {
                    probability: config.ml_routing.exploration_probability,
                    seed: config.ml_routing.exploration_seed,
                },
            );
            // A state directory that cannot be opened is not fatal: the request
            // path still runs, still records in memory, and still routes
            // deterministically. What is lost is history across restarts and the
            // ability to promote, both of which are absent in a fresh install
            // anyway.
            // An unset or unusable `state_dir` falls back to a process-scoped
            // directory rather than the working directory, so a proxy launched
            // from a source checkout cannot write its history into the tree it
            // was launched from.
            // Durable ML state is opt-in. With no configured state directory
            // there is no trace log and no model store at all, rather than one
            // written somewhere implicit - see `MlRoutingConfig::state_dir` for
            // why an OS-derived default was a mistake.
            let (traces, active_models) = if config.ml_routing.has_state_dir() {
                let state_dir = std::path::PathBuf::from(&config.ml_routing.state_dir);
                let log = match crate::ml::TraceLog::open(&state_dir) {
                    Ok(log) => Some(log),
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            "the routing trace log could not be opened; \
                             traces will not survive this process"
                        );
                        None
                    }
                };
                let store = crate::ml::ActiveModelStore::open(&state_dir)
                    .inspect_err(|error| {
                        tracing::error!(
                            error = %error,
                            "the active model store could not be opened; no model can serve"
                        );
                    })
                    .ok();
                (log, store)
            } else {
                (None, None)
            };
            Self::with_ml_routing(
                config,
                secrets,
                attachment,
                ml_routing,
                traces,
                active_models,
            )
        }
        #[cfg(not(feature = "ml"))]
        {
            Self::build_without_ml(config, secrets, attachment)
        }
    }

    #[cfg(not(feature = "ml"))]
    pub(super) fn build_without_ml(
        config: AppConfig,
        secrets: Arc<dyn SecretStore>,
        attachment: ShadowAttachment,
    ) -> Self {
        let log_limit = config.server.log_limit;
        // OpenCode's free and Console tiers meter by account (or source IP for
        // the anonymous free tier), so a 429 on one model is a 429 on all of
        // them. Other providers keep their model-scoped rate-limit handling.
        let group_limited: Vec<String> = config
            .providers
            .iter()
            .filter(|provider| provider.kind.is_opencode())
            .map(|provider| provider.id.clone())
            .collect();
        let router = Router::new();
        router.set_group_limited_providers(group_limited);
        Self {
            registry: RwLock::new(Arc::new(Registry::new(Arc::new(config)))),
            router: Arc::new(router),
            stats: Arc::new(Stats::new(log_limit)),
            outcomes: OutcomeLog::new(log_limit),
            upstream: Upstream::new(false, 15),
            secrets,
            ledger: RwLock::new(Ledger::new()),
            ledger_dirty: AtomicBool::new(false),
            ledger_generation: AtomicU64::new(0),
            ledger_flush: Mutex::new(()),
            accounting: Mutex::new(Default::default()),
            admission: AdmissionGates::default(),
            shadow_attachment: RwLock::new(attachment),
            projections: ProjectionLog::new(log_limit),
            response_store: ResponseStore::default(),
            codex_filter: Default::default(),
            upstream_sessions: Default::default(),
            // `ml` and `account` are independent features, so this constructor is
            // reached with accounts on or off and the field must satisfy both.
            // Found by compiling `--features account`: gating the field in `build()`
            // alone leaves the no-ml build missing it.
            #[cfg(feature = "account")]
            accounts: crate::account::AccountStore::new(),
        }
    }

    /// Build the state with an explicit ML serving component, trace log and
    /// active-model store.
    ///
    /// The parameters exist so a caller — a test, or an operator tool — can state
    /// exactly which model may serve and which directory history goes to, rather
    /// than inheriting whatever a temporary directory happened to contain.
    #[cfg(feature = "ml")]
    pub fn with_ml_routing(
        config: AppConfig,
        secrets: Arc<dyn SecretStore>,
        attachment: ShadowAttachment,
        ml_routing: crate::ml::MlRouter,
        traces: Option<crate::ml::TraceLog>,
        active_models: Option<crate::ml::ActiveModelStore>,
    ) -> Self {
        if let Some(store) = active_models.as_ref() {
            match store.active() {
                Ok(Some(predictor)) => {
                    if config.ml_routing.enabled {
                        tracing::info!(
                            model_id = predictor.model_id(),
                            commit_id = predictor.commit_id().as_str(),
                            "an ML model is active and may re-order the provider plan"
                        );
                        ml_routing.attach(predictor);
                    } else {
                        tracing::info!(
                            commit_id = predictor.commit_id().as_str(),
                            "an ML model is promoted but ml_routing.enabled is false; \
                             routing stays deterministic"
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "the active model store could not be read; routing stays deterministic"
                    );
                }
            }
        }
        Self::build(
            config,
            secrets,
            attachment,
            ml_routing,
            traces,
            active_models,
        )
    }

    #[cfg(feature = "ml")]
    pub(super) fn build(
        config: AppConfig,
        secrets: Arc<dyn SecretStore>,
        attachment: ShadowAttachment,
        ml_routing: crate::ml::MlRouter,
        traces: Option<crate::ml::TraceLog>,
        active_models: Option<crate::ml::ActiveModelStore>,
    ) -> Self {
        let log_limit = config.server.log_limit;
        let stats = Arc::new(Stats::new(log_limit));
        let bypass_proxy = config.server.bypass_proxy;
        let connect_timeout_secs = config
            .providers
            .first()
            .map(|p| p.connect_timeout_secs)
            .unwrap_or(15);
        // Read before `config` moves into the registry: the shadow switch and
        // retention are configuration the engine carries, not construction
        // constants.
        #[cfg(feature = "ml")]
        let shadow_config = config.shadow.clone();
        let group_limited: Vec<String> = config
            .providers
            .iter()
            .filter(|provider| provider.kind.is_opencode())
            .map(|provider| provider.id.clone())
            .collect();
        let router = Router::new();
        router.set_group_limited_providers(group_limited);
        AppState {
            registry: RwLock::new(Arc::new(Registry::new(Arc::new(config)))),
            router: Arc::new(router),
            stats,
            outcomes: OutcomeLog::new(log_limit),
            upstream: Upstream::new(bypass_proxy, connect_timeout_secs),
            secrets,
            ledger: RwLock::new(Ledger::new()),
            ledger_dirty: AtomicBool::new(false),
            ledger_generation: AtomicU64::new(0),
            ledger_flush: Mutex::new(()),
            accounting: Mutex::new(Default::default()),
            admission: AdmissionGates::default(),
            #[cfg(feature = "ml")]
            shadow: crate::ml::ShadowEngine::with_retention(
                crate::ml::DecisionEngine::new(
                    crate::ml::CoordinatorConfig::default(),
                    crate::ml::RewardPolicy::default(),
                ),
                shadow_config.enabled,
                shadow_config.max_decisions,
                shadow_config.max_age_secs,
            ),
            #[cfg(feature = "ml")]
            dataset: crate::ml::DatasetStore::production(),
            #[cfg(feature = "ml")]
            ml_routing,
            #[cfg(feature = "ml")]
            traces,
            #[cfg(feature = "ml")]
            active_models,
            // Constructed here rather than passed in, unlike the ml pieces: it has
            // no configuration to read and nothing to fail. The declarations live in
            // `ProviderConfig::accounts`; this holds only what probing derived, and
            // starts empty because nothing has been probed yet.
            #[cfg(feature = "account")]
            accounts: crate::account::AccountStore::new(),
            shadow_attachment: RwLock::new(attachment),
            projections: ProjectionLog::new(log_limit),
            response_store: ResponseStore::default(),
            codex_filter: Default::default(),
            upstream_sessions: Default::default(),
        }
    }

    /// Adopt a ledger read from disk.
    ///
    /// Takes the flush lock so adopting a ledger cannot interleave with a write
    /// of the previous one, and bumps the generation so a snapshot taken before
    /// the adoption cannot confirm itself against the new state.
    pub fn set_ledger(&self, ledger: Ledger) {
        let _flush = crate::sync::lock(&self.ledger_flush);
        *crate::sync::write(&self.ledger) = ledger;
        self.ledger_generation.fetch_add(1, Ordering::AcqRel);
        self.ledger_dirty.store(false, Ordering::Release);
    }

    pub fn ledger(&self) -> Ledger {
        crate::sync::read(&self.ledger).clone()
    }

    /// Snapshot the ledger for writing out, with the generation it carries.
    ///
    /// Returns `None` when nothing has changed since the last successful write,
    /// so an idle proxy does not rewrite the same file every few seconds. The
    /// snapshot is pruned so long-running processes do not accumulate stale
    /// day/month buckets in `spend.json` (the in-memory ledger keeps its
    /// buckets; checks ignore stale ones).
    ///
    /// The dirty flag is deliberately *not* cleared here: only a successful
    /// write may do that, through [`Self::ledger_saved`].
    pub(super) fn dirty_ledger_snapshot(&self) -> Option<(Ledger, u64)> {
        if !self.ledger_dirty.load(Ordering::Acquire) {
            return None;
        }
        let generation = self.ledger_generation.load(Ordering::Acquire);
        let mut ledger = self.ledger();
        ledger.prune(Local::now());
        Some((ledger, generation))
    }

    /// Confirm that the snapshot carrying `generation` reached the disk.
    ///
    /// Clears the dirty flag only when nothing has been charged since the
    /// snapshot was taken. A charge that landed mid-write leaves the flag set,
    /// so the next flush writes the newer numbers instead of losing them.
    pub(super) fn ledger_saved(&self, generation: u64) {
        if self.ledger_generation.load(Ordering::Acquire) == generation {
            self.ledger_dirty.store(false, Ordering::Release);
        } else {
            self.ledger_dirty.store(true, Ordering::Release);
        }
    }

    /// Write the ledger out through `save`, if it has moved since the last
    /// attempt.
    ///
    /// The whole snapshot/save/confirm sequence is serialised, so a timer flush
    /// and a shutdown flush cannot interleave into writing an older snapshot
    /// over a newer one. A failed `save` leaves the dirty flag exactly as it
    /// was, so the pending spend is retried by the next flush rather than being
    /// dropped; the error is returned for the caller to log.
    ///
    /// The save itself is a closure because the disk lives in the desktop
    /// shell: the core owns the accounting, the shell owns the file.
    pub fn flush_ledger(
        &self,
        save: impl FnOnce(&Ledger) -> std::result::Result<(), String>,
    ) -> std::result::Result<(), String> {
        let _flush = crate::sync::lock(&self.ledger_flush);
        let Some((ledger, generation)) = self.dirty_ledger_snapshot() else {
            return Ok(());
        };
        save(&ledger)?;
        self.ledger_saved(generation);
        Ok(())
    }

    /// Record what a finished request cost, against every scope that covers it.
    pub fn charge(&self, provider_id: &str, tier: Option<ModelTier>, cost: &Cost) {
        crate::sync::write(&self.ledger).charge(Local::now(), provider_id, tier, cost);
        self.ledger_generation.fetch_add(1, Ordering::AcqRel);
        self.ledger_dirty.store(true, Ordering::Release);
    }

    /// Record what an auxiliary call cost, against the provider that answered.
    ///
    /// The same ledger as [`Self::charge`], with a separate entry point because
    /// the attribution is different: a vision description (or any other side
    /// call made on a request's behalf) is billed to the provider and tier it
    /// actually reached, never folded into the main request's own settlement.
    /// The main request's lifecycle keeps its single charge site.
    pub fn charge_auxiliary(&self, provider_id: &str, tier: Option<ModelTier>, cost: &Cost) {
        self.charge(provider_id, tier, cost);
    }

    /// What the budgets say about a request that is about to be routed.
    pub fn budget_verdict(&self, provider_ids: &[String], tier: Option<ModelTier>) -> Verdict {
        let config = self.config();
        if config.budgets.is_empty() {
            return Verdict::Allow;
        }
        budget::check(
            &config.budgets,
            &crate::sync::read(&self.ledger),
            Local::now(),
            provider_ids,
            tier,
        )
    }

    /// Take the admission permits for a request's budgeted scopes.
    ///
    /// The caller passes every scope with a configured limit that this request
    /// could spend against, sorted, and must call this before the budget check:
    /// the gate is what stops a peer from being charged after the check but
    /// before the request that passed it has finished. The returned guard is
    /// held until the request is settled; dropping it releases every scope.
    pub(super) async fn admit(&self, scopes: &[BudgetScope]) -> AdmissionGuard {
        self.admission.admit(scopes).await
    }

    /// What the budgets say about a classifier side request.
    ///
    /// A classifier request is billed under the model that answers it, so the
    /// global and provider scopes are not exempt: once either is used up, a
    /// side request is real spend like any other and must not be sent. The one
    /// deliberate exception is the *class* (tier) scope — the classifier pool
    /// is chosen by the classifier config rather than by tier policy, so a
    /// class budget neither degrades nor rejects a side request. Filtering the
    /// tier budgets out here is how that single-scope exception is expressed,
    /// instead of skipping the whole check.
    pub fn classifier_budget_verdict(&self, provider_ids: &[String]) -> Verdict {
        let config = self.config();
        let spend_budgets: Vec<crate::budget::Budget> = config
            .budgets
            .iter()
            .filter(|budget| !matches!(budget.scope, BudgetScope::Tier { .. }))
            .cloned()
            .collect();
        if spend_budgets.is_empty() {
            return Verdict::Allow;
        }
        budget::check(
            &spend_budgets,
            &crate::sync::read(&self.ledger),
            Local::now(),
            provider_ids,
            None,
        )
    }
}
