//! The local HTTP server.
//!
//! Exposes one endpoint per dialect (`POST /v1/messages` and
//! `POST /v1/chat/completions`) plus a merged `GET /v1/models` listing that
//! satisfies both Anthropic and OpenAI clients.
//!
//! Security: it binds loopback by default and requires a local token. Anything
//! that can reach this port can spend the configured API keys.

mod pipeline;
mod projection_log;
mod shadow_candidate;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Local;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::billing::{Cost, Pricing};
use crate::budget::{self, BudgetScope, Ledger, Verdict};
use crate::config::{AppConfig, ModelTier, ProviderConfig, SecretStore, ServerConfig};
use crate::election::{self, Election, Measurement};
use crate::error::{Error, Result};
use crate::ir::{Dialect, ResponseStore};
use crate::protocol;
use crate::registry::Registry;
use crate::router::Router;
use crate::stats::{OutcomeLog, Stats};
use crate::upstream::Upstream;

use pipeline::handle_chat;
use projection_log::ProjectionLog;

pub use shadow_candidate::ShadowAttachment;

#[cfg(feature = "ml")]
pub use shadow_candidate::{
    ShadowCandidate, ShadowCandidateArtifact, ShadowCandidateError, SHADOW_CANDIDATE_ARTIFACT,
    SHADOW_CANDIDATE_ENVELOPE_VERSION,
};

/// Everything a request handler needs.
///
/// Fields are private so nothing outside can leave the cached registry out of
/// step with the configuration it was built from.
pub struct AppState {
    /// Configuration plus its precomputed lookup tables, swapped together.
    registry: RwLock<Arc<Registry>>,
    router: Arc<Router>,
    stats: Arc<Stats>,
    upstream: Upstream,
    secrets: Arc<dyn SecretStore>,
    /// Spend so far, which the budgets are checked against. It lives here rather
    /// than in `Stats` because the request log is deliberately memory only, and a
    /// guardrail that forgets on restart is not a guardrail.
    ledger: RwLock<Ledger>,
    /// Set when the ledger has moved since it was last written out, so the desktop
    /// layer can flush on a timer instead of writing a file per request.
    ///
    /// It is cleared only after a write *succeeded*, so a failed flush leaves the
    /// pending spend in place for the next attempt instead of silently dropping
    /// it — a ledger that forgets a request is a budget that lets it through
    /// again after a restart.
    ledger_dirty: AtomicBool,
    /// Bumped by every ledger mutation. A flush snapshots it before writing and
    /// compares it afterwards: a charge that landed while the file was being
    /// written keeps the dirty flag set, so the newer numbers are written next
    /// time instead of being swallowed by the snapshot that just succeeded.
    ledger_generation: AtomicU64,
    /// Held across snapshot/write/confirm, so a timer flush and a shutdown flush
    /// cannot interleave into writing an older snapshot over a newer one.
    ledger_flush: Mutex<()>,
    /// One admission gate per budgeted scope.
    ///
    /// A request takes a gate for every scope with a configured limit it could
    /// spend against, before the budget check, and gives it back only once the
    /// request has been settled and charged. That is what makes the documented
    /// overshoot bound real: without it, N concurrent requests all read the same
    /// historical total and all pass. A scope with no configured limit has no
    /// gate and never blocks.
    admission: AdmissionGates,
    /// Shadow decision engine (Stage 7E-1): records what the ML routing stack
    /// would have done for policy-routed main traffic. Record-only by
    /// construction — nothing in the request pipeline reads its verdicts.
    #[cfg(feature = "ml")]
    shadow: crate::ml::ShadowEngine,
    /// Terminal outcomes, one per request, as the lifecycle built them. A
    /// diagnostic view of the same single accounting the request log records;
    /// nothing routes on it and no training consumes it yet.
    outcomes: OutcomeLog,
    /// Canonical training samples, ingested once per eligible request from the
    /// request's retained decision-time input and its own terminal Outcome.
    ///
    /// Collection only: nothing trains on it, reads a verdict from it, or lets
    /// it influence a response. Whether a request contributes a sample depends
    /// entirely on whether a decision-time record was retained for it, so
    /// `config.shadow.enabled` is the effective switch and there is deliberately
    /// no second, independent dataset switch.
    #[cfg(feature = "ml")]
    dataset: crate::ml::DatasetStore,
    /// The learned model's role in serving, when `ml_routing.enabled` is on.
    ///
    /// Separate from `shadow`: the shadow records what the model would have
    /// done and its verdict is discarded, while this re-orders the executable
    /// plan. Both are off by default.
    #[cfg(feature = "ml")]
    ml_routing: crate::ml::MlRouter,
    /// The durable routing trace log. Appended from the same terminal
    /// transition that produces the sample, so the history a future training
    /// run reads is the history this process actually served.
    ///
    /// `None` when no state directory is configured, which is the default: no
    /// durable history at all, rather than history written somewhere implicit.
    #[cfg(feature = "ml")]
    traces: Option<crate::ml::TraceLog>,
    /// The durable active-model pointer, when a state directory could be
    /// opened. `None` means no model can serve, which is the state a fresh
    /// installation is in and the state a rollback returns to.
    #[cfg(feature = "ml")]
    active_models: Option<crate::ml::ActiveModelStore>,
    /// The verified model commit the shadow evaluates against, if one is
    /// attached. `None` is the state this node started in and the state a
    /// rollback returns to; it is a real state with a real behaviour, not a
    /// disabled code path, so before/during/after is an ordinary comparison.
    ///
    /// Behind a lock for one reason: withdrawal is a runtime operation, and a
    /// rollback that needs a rebuild to demonstrate has not been demonstrated.
    /// There is deliberately no setter that can *attach* — the candidate is a
    /// program constant — so this lock can only ever move from attached to
    /// withdrawn.
    shadow_attachment: RwLock<ShadowAttachment>,
    /// One terminal outcome per request, paired with the routing decision that
    /// produced it, held so the observability projection is reachable from the
    /// serving path rather than only from tests.
    projections: ProjectionLog,
    pub response_store: ResponseStore,
}

/// The admission gates, one per budgeted scope.
///
/// Each gate is a one-permit semaphore, so whoever holds it is the only request
/// in flight for that scope. Gates are created on first use and deliberately
/// never removed: a request holds its permit through the `Arc` it acquired, so a
/// budget removed (or re-added) while requests are in flight cannot strand a
/// permit, and the next request for that scope waits on the same gate the
/// outstanding request is about to release. The map only ever grows to the set
/// of scopes a configuration has named, which is bounded by the configuration.
#[derive(Default)]
struct AdmissionGates {
    gates: Mutex<BTreeMap<BudgetScope, Arc<Semaphore>>>,
}

/// The admission permits one request holds until it has been settled.
///
/// Opaque on purpose: dropping it is the only way to release, so no terminal
/// path can charge the ledger without handing its scope to the next request.
/// An empty guard is the honest representation of "no scope this request
/// occupies has a configured limit", which is the case that must stay fully
/// concurrent.
struct AdmissionGuard {
    _permits: Vec<OwnedSemaphorePermit>,
}

impl AdmissionGates {
    /// Take the permit for every scope in `scopes`.
    ///
    /// The caller passes the scopes in canonical ascending order and every
    /// caller does, so a request only ever accumulates gates in that one order
    /// and two requests that want overlapping sets cannot each hold what the
    /// other is waiting for. Blocking here is the point: the second request
    /// waits, then re-reads a ledger that already contains what the first one
    /// spent.
    async fn admit(&self, scopes: &[BudgetScope]) -> AdmissionGuard {
        let mut permits = Vec::with_capacity(scopes.len());
        for scope in scopes {
            let gate = {
                let mut gates = crate::sync::lock(&self.gates);
                Arc::clone(
                    gates
                        .entry(scope.clone())
                        .or_insert_with(|| Arc::new(Semaphore::new(1))),
                )
            };
            // `acquire_owned` fails only for a closed semaphore, and nothing
            // here ever closes one; a missing permit would silently unbind the
            // request, so it is treated as "hold nothing" rather than a panic.
            if let Ok(permit) = gate.acquire_owned().await {
                permits.push(permit);
            }
        }
        AdmissionGuard { _permits: permits }
    }
}

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
    fn build_without_ml(
        config: AppConfig,
        secrets: Arc<dyn SecretStore>,
        attachment: ShadowAttachment,
    ) -> Self {
        let log_limit = config.server.log_limit;
        Self {
            registry: RwLock::new(Arc::new(Registry::new(Arc::new(config)))),
            router: Arc::new(Router::new()),
            stats: Arc::new(Stats::new(log_limit)),
            outcomes: OutcomeLog::new(log_limit),
            upstream: Upstream::new(false, 15),
            secrets,
            ledger: RwLock::new(Ledger::new()),
            ledger_dirty: AtomicBool::new(false),
            ledger_generation: AtomicU64::new(0),
            ledger_flush: Mutex::new(()),
            admission: AdmissionGates::default(),
            shadow_attachment: RwLock::new(attachment),
            projections: ProjectionLog::new(log_limit),
            response_store: ResponseStore::default(),
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
    fn build(
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
        AppState {
            registry: RwLock::new(Arc::new(Registry::new(Arc::new(config)))),
            router: Arc::new(Router::new()),
            stats,
            outcomes: OutcomeLog::new(log_limit),
            upstream: Upstream::new(bypass_proxy, connect_timeout_secs),
            secrets,
            ledger: RwLock::new(Ledger::new()),
            ledger_dirty: AtomicBool::new(false),
            ledger_generation: AtomicU64::new(0),
            ledger_flush: Mutex::new(()),
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
            shadow_attachment: RwLock::new(attachment),
            projections: ProjectionLog::new(log_limit),
            response_store: ResponseStore::default(),
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
    fn dirty_ledger_snapshot(&self) -> Option<(Ledger, u64)> {
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
    fn ledger_saved(&self, generation: u64) {
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
    async fn admit(&self, scopes: &[BudgetScope]) -> AdmissionGuard {
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

    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// The terminal outcomes the request lifecycle built, newest first.
    pub fn outcomes(&self) -> &OutcomeLog {
        &self.outcomes
    }

    pub fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    /// The shadow decision engine (record-only; never influences routing).
    #[cfg(feature = "ml")]
    pub fn shadow(&self) -> &crate::ml::ShadowEngine {
        &self.shadow
    }

    /// The canonical training dataset.
    ///
    /// Read by the offline half of the loop — training, comparison, promotion —
    /// and written from the request path. Nothing on the request path reads it
    /// to make a decision.
    #[cfg(feature = "ml")]
    pub fn dataset(&self) -> &crate::ml::DatasetStore {
        &self.dataset
    }

    /// The durable routing trace log.
    #[cfg(feature = "ml")]
    pub fn traces(&self) -> Option<&crate::ml::TraceLog> {
        self.traces.as_ref()
    }

    /// The learned model's role in serving.
    ///
    /// The request path may *rank* through this and may not obtain a predictor
    /// from it. Ranking needs nothing but the plan and the model's opinion;
    /// handing out the predictor would put the whole artifact one call away from
    /// a response, which is a different and much larger surface than the one a
    /// routing strategy needs.
    #[cfg(feature = "ml")]
    pub fn ml_routing(&self) -> &crate::ml::MlRouter {
        &self.ml_routing
    }

    /// The durable active-model pointer, when a state directory was available.
    #[cfg(feature = "ml")]
    pub fn active_models(&self) -> Option<&crate::ml::ActiveModelStore> {
        self.active_models.as_ref()
    }

    /// Bring the serving model back in line with the durable pointer.
    ///
    /// Returns whether a model is attached afterwards.
    ///
    /// This exists because a rollback that only rewrites the pointer is a lie:
    /// the router would keep ranking with the model the operator just withdrew,
    /// and every status document would then report a state the process is not
    /// in. The same seam is used after a promotion for the same reason.
    ///
    /// A pointer that cannot be read is *not* treated as "no model" — it leaves
    /// whatever is attached alone and reports the fault, because the one thing an
    /// operator must not lose by a transient read error is a model they chose.
    #[cfg(feature = "ml")]
    pub fn reload_active_model(&self) -> crate::ml::ReloadOutcome {
        let store = match self.active_models.as_ref() {
            Some(store) => store,
            None => {
                return crate::ml::ReloadOutcome::fault(
                    self.ml_routing.is_attached(),
                    "no durable state directory is configured, so no model can be attached",
                )
            }
        };
        let outcome = match store.active() {
            Ok(Some(predictor)) => {
                let commit = predictor.commit_id().as_str().to_string();
                if self.config().ml_routing.enabled {
                    self.ml_routing.attach(predictor);
                } else {
                    // Installed but deliberately inert. Withdrawing rather than
                    // leaving a stale predictor in place is what makes a
                    // configuration change take effect without a restart.
                    self.ml_routing.withdraw();
                }
                tracing::info!(commit_id = commit, "the active model pointer was reloaded");
                crate::ml::ReloadOutcome::ok(self.ml_routing.is_attached(), Some(commit))
            }
            Ok(None) => {
                self.ml_routing.withdraw();
                tracing::info!("no model is promoted; routing is deterministic");
                crate::ml::ReloadOutcome::ok(false, None)
            }
            Err(error) => {
                crate::ml::ReloadOutcome::fault(self.ml_routing.is_attached(), error.to_string())
            }
        };
        outcome
    }

    /// Withdraw the current model and return to the previously promoted one.
    ///
    /// The operator's kill switch, and the reason the promotion gate is
    /// trustworthy: a model that turns out to be wrong can be removed without
    /// stopping the proxy, without editing a file by hand, and without waiting
    /// for a restart.
    ///
    /// Two things happen and both are required. The durable pointer moves, so
    /// the change survives a restart; and the router is reloaded from that
    /// pointer, so the process stops using the withdrawn model *now*. Doing only
    /// the first is the cosmetic rollback this module was written to prevent.
    ///
    /// Reports `false` when there is no previous model, which is a genuine "there
    /// was nothing to roll back to" rather than a failure — a fresh installation
    /// has a history of length zero and saying so is more useful than an error.
    #[cfg(feature = "ml")]
    pub fn rollback_active_model(&self) -> crate::ml::ReloadOutcome {
        let store = match self.active_models.as_ref() {
            Some(store) => store,
            None => {
                return crate::ml::ReloadOutcome::fault(
                    self.ml_routing.is_attached(),
                    "no durable state directory is configured, so nothing can be rolled back",
                )
            }
        };
        match store.rollback() {
            Ok(true) => {
                tracing::info!("the active model was rolled back by an operator");
                self.reload_active_model()
            }
            Ok(false) => crate::ml::ReloadOutcome::fault(
                self.ml_routing.is_attached(),
                "there is no earlier promoted model to roll back to",
            ),
            Err(error) => crate::ml::ReloadOutcome::fault(
                self.ml_routing.is_attached(),
                format!("the rollback was refused: {error}"),
            ),
        }
    }

    /// A test of the loaded model against real request history.
    ///
    /// This is the shadow analysis from the outside: the same `analyse` the
    /// offline gate uses, fed by the operator's own traffic rather than a
    /// fixture, and run against the weights actually attached to the router.
    ///
    /// Bounded on purpose. `limit` caps how many records are read, because this
    /// is callable from a desktop button and an unbounded read would take the
    /// app down rather than answer a question.
    ///
    /// Run one promotion round over the durable history, and optionally install
    /// what the gate authorised.
    ///
    /// This is the product entry point the loop was missing. It is in-process rather
    /// than a separate command because promotion has to reach the *live* router:
    /// `reload_active_model` attaches on this `AppState`, and nothing watches the
    /// pointer file, so a model installed by another process would sit unread until
    /// a restart. That is a fact about the design rather than a preference, which is
    /// why the entry point is here and not in a CLI.
    ///
    /// # `install` is not implied
    ///
    /// Judging and installing are separate acts, and the default is to judge only.
    /// A promotion changes what every subsequent request is served by, so it should
    /// be something an operator asked for, not something that happened because
    /// someone polled an endpoint. A caller that wants the model live asks for it.
    ///
    /// That makes the third of the three open product decisions — *when* a round
    /// happens — a property of the caller rather than of this code. Nothing here
    /// schedules anything, retries anything, or holds a cadence; an operator's own
    /// scheduler decides, and it decides by choosing when to call.
    ///
    /// The gate is still what judges. `install` only obeys a `PROMOTED` verdict; it
    /// cannot install a refusal.
    #[cfg(feature = "ml")]
    pub fn ml_run_promotion_round(
        &self,
        config: crate::ml::RoundConfig,
        install: bool,
        revision: Option<String>,
    ) -> crate::ml::status::PromotionRoundStatus {
        if self.active_models.is_none() {
            return crate::ml::status::PromotionRoundStatus::unavailable(
                "no durable state directory is configured, so there is no history to learn from",
            );
        }
        let round = match crate::ml::run_promotion_round(&self.ml_state_dir(), &config, revision) {
            Ok(round) => round,
            Err(error) => {
                return crate::ml::status::PromotionRoundStatus::unavailable(error.to_string())
            }
        };

        let current_state = || {
            // Nothing was installed, so the pointer is unchanged and there is nothing
            // to re-read. Reporting the current state keeps the response honest about
            // whether a model is serving.
            crate::ml::ReloadOutcome::ok(
                self.ml_routing.is_attached(),
                self.ml_routing.attached_commit(),
            )
        };

        let installed = if install {
            let store = self.active_models.as_ref().expect("checked above");
            match round.install(store) {
                Ok(installed) => installed,
                Err(error) => {
                    // The round judged fine and the store refused. Reporting this as
                    // an unavailable round would throw away the verdict, which is the
                    // part an operator asked for, so the decision travels back with
                    // the fault attached. No reload: the install failed, so the
                    // pointer is whatever it was before this call.
                    let mut status = crate::ml::status::PromotionRoundStatus::from_round(
                        &round,
                        current_state(),
                    );
                    status.error = Some(format!(
                        "the gate authorised this model but it could not be installed: {error}"
                    ));
                    return status;
                }
            }
        } else {
            None
        };

        let reloaded = if installed.is_some() {
            self.reload_active_model()
        } else {
            current_state()
        };
        crate::ml::status::PromotionRoundStatus::from_round(&round, reloaded)
    }

    /// The durable state directory, derived the same way `new` derived it when it
    /// opened the log and the store. Taken from the configuration rather than from
    /// the open `TraceLog` so that no accessor is added to that type for this one
    /// caller.
    #[cfg(feature = "ml")]
    fn ml_state_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.config().ml_routing.state_dir)
    }

    /// Reports *why* it could not run instead of returning an empty analysis,
    /// because "no model is attached" and "the model disagreed with production on
    /// nothing" are different answers and only the first is a problem.
    #[cfg(feature = "ml")]
    pub fn ml_shadow_analysis(&self, limit: usize) -> crate::ml::ShadowAnalysisStatus {
        const MAX_RECORDS: usize = 50_000;
        let limit = limit.min(MAX_RECORDS);
        let traces =
            match self.traces.as_ref() {
                None => return crate::ml::ShadowAnalysisStatus::unavailable(
                    0,
                    "no durable state directory is configured, so there is no history to replay",
                ),
                Some(log) => match log.tail(limit) {
                    Ok(traces) => traces,
                    Err(error) => {
                        return crate::ml::ShadowAnalysisStatus::unavailable(0, error.to_string())
                    }
                },
            };
        let read = traces.len();
        if read == 0 {
            return crate::ml::ShadowAnalysisStatus::unavailable(
                0,
                "no request history has been recorded yet",
            );
        }
        // Replay the attached model, not whatever was promoted most recently.
        let (ensemble, commit_id) = match (
            self.ml_routing.attached_ensemble(),
            self.ml_routing.attached_commit(),
        ) {
            (Some(ensemble), Some(commit_id)) => (ensemble, commit_id),
            _ => {
                return crate::ml::ShadowAnalysisStatus::unavailable(
                    read,
                    "no model is attached to the router, so there is nothing to replay; \
                     promote a model and enable ml_routing",
                )
            }
        };
        let reward_policy = self.config().ml_routing_reward_policy();
        let policy = crate::ml::MlPolicy::from_ensemble(ensemble, reward_policy.clone());
        let evidence = crate::ml::ShadowEvidence::from_policy(&traces, &policy, BTreeMap::new());
        let analysis = crate::ml::analyse(&traces, &evidence, &reward_policy);
        crate::ml::ShadowAnalysisStatus {
            traces_read: read,
            commit_id: Some(commit_id),
            reason: None,
            analysis: Some(analysis),
        }
    }

    /// Read the operator-facing view of the learned model.
    ///
    /// Everything here is read from the live components; nothing is recomputed.
    /// A store that cannot be read reports itself as unreadable rather than as
    /// empty, because "there is no model" and "I could not tell whether there is
    /// a model" are different answers and an operator acting on the second one
    /// as if it were the first is how a broken deployment keeps routing.
    #[cfg(feature = "ml")]
    pub fn ml_status(&self) -> crate::ml::MlStatus {
        let store = self.active_models.as_ref();
        let (active, active_decision, history) = match store {
            None => (None, None, Vec::new()),
            Some(store) => {
                let pointer = store.read_pointer().ok().flatten();
                let current = pointer.map(|pointer| pointer.current);
                let active = current
                    .as_ref()
                    .map(|model| crate::ml::PromotedModelStatus {
                        model_id: model.model_id.clone(),
                        commit_id: model.commit_id.clone(),
                        verdict: model.promotion.verdict,
                        dataset_fingerprint: model.promotion.dataset_fingerprint.to_string(),
                        fitted_partition_fingerprint: model
                            .promotion
                            .fitted_partition_fingerprint
                            .to_string(),
                        gate_config_identity: model.promotion.gate_config_identity.clone(),
                        required_baseline: model.promotion.baseline.clone(),
                        paired_requests: model.promotion.paired_requests,
                        holdout_loss: model.promotion.holdout_loss,
                        promoted_at: model.promoted_at,
                    });
                let decision = current.map(|model| model.promotion.clone());
                let history = store
                    .audit()
                    .map(|entries| {
                        entries
                            .iter()
                            .map(crate::ml::PromotionHistoryEntry::from)
                            .collect()
                    })
                    .unwrap_or_default();
                (active, decision, history)
            }
        };

        // What the configuration names, narrowed to the candidates that could
        // actually be routed to. This is the classifier's own view — an enabled
        // provider with an enabled model entry — and it is what makes the blind
        // spot list a statement about the configuration the operator just edited
        // rather than a second, drifting copy of what is callable.
        let configured: Vec<(String, String)> = {
            let config = self.config();
            config
                .models
                .iter()
                .filter(|entry| entry.enabled)
                .filter(|entry| {
                    config
                        .providers
                        .iter()
                        .any(|provider| provider.enabled && provider.id == entry.provider_id)
                })
                .map(|entry| (entry.exposed_id(), entry.provider_id.clone()))
                .collect()
        };
        let observations = self.router().observations();
        let blind_candidates = crate::ml::BlindCandidate::unobserved(
            configured
                .iter()
                .map(|(model_id, provider_id)| (model_id.as_str(), provider_id.as_str())),
            observations,
        );

        crate::ml::MlStatus {
            routing_enabled: self.config().ml_routing.enabled,
            durable_state: self.config().ml_routing.has_state_dir(),
            traces_open: self.traces.is_some(),
            model_store_open: store.is_some(),
            active,
            active_decision,
            history,
            routing: self.ml_routing.counts(),
            exploration_probability: self.config().ml_routing.exploration_probability,
            exploration_seed: self.config().ml_routing.exploration_seed,
            blind_candidates,
            dataset: self.dataset.counters(),
            traces: self.traces.as_ref().map(crate::ml::TraceLog::counters),
            shadow: crate::ml::ShadowStatus {
                enabled: self.shadow().enabled(),
                decisions_recorded: self.shadow().store().len(),
                faults: self.shadow().fault_count(),
            },
            read_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Evaluate one request's shadow counterfactual, against whatever is attached.
    ///
    /// The predictor does not leave this struct. That is a deliberate boundary,
    /// not an encapsulation convenience: an accepted gate asserts that
    /// `AppState` never names a learning seam, and a method that handed out the
    /// ensemble predictor would put one in reach of this module and of every
    /// caller downstream of it. Exposing only the *evaluation* means the request
    /// path cannot obtain the predictor at all, so the only thing it can do with
    /// a candidate is record what it would have decided — which is the whole of
    /// what a shadow is.
    ///
    /// `None` when the candidate is withdrawn, in which case the engine's own
    /// cold-start predictor runs and the record names the engine's commit. That
    /// is the behaviour this state had before a candidate existed, and it is an
    /// ordinary outcome rather than an error.
    #[cfg(feature = "ml")]
    pub fn shadow_evaluated(
        &self,
        request_id: &str,
        input: &crate::ml::ShadowInput,
    ) -> Option<crate::ml::ShadowDecision> {
        let shadow = &self.shadow;
        let predictor = crate::sync::read(&self.shadow_attachment).predictor();
        match predictor {
            Some(predictor) => shadow.evaluate_with(request_id, input, predictor.as_ref()),
            None => shadow.evaluate(request_id, input),
        }
    }

    /// The commit id of the attached candidate, or `None` when withdrawn.
    #[cfg(feature = "ml")]
    pub fn shadow_candidate_commit_id(&self) -> Option<String> {
        crate::sync::read(&self.shadow_attachment)
            .attached_commit_id()
            .map(str::to_string)
    }

    /// Whether a named candidate is currently attached.
    ///
    /// Answerable in every build. Without the ML feature it is always `false`,
    /// and that is the honest answer rather than a missing method: the desktop
    /// application compiles no shadow and therefore has no candidate.
    pub fn shadow_candidate_attached(&self) -> bool {
        crate::sync::read(&self.shadow_attachment).is_attached()
    }

    /// Withdraw the shadow candidate.
    ///
    /// This is the rollback lever and it is deliberately one-way: there is no
    /// counterpart that can attach a candidate, because the candidate is a
    /// program constant verified at construction and re-attaching it at runtime
    /// would be a swap rather than a rollback. After this returns, every
    /// subsequent evaluation names the engine's own cold-start predictor, which
    /// is the behaviour this node started from.
    ///
    /// Returns whether anything was withdrawn, so a caller can tell a real
    /// rollback from a no-op rather than assuming one.
    #[cfg(feature = "ml")]
    pub fn withdraw_shadow_candidate(&self) -> bool {
        let mut attachment = crate::sync::write(&self.shadow_attachment);
        if !attachment.is_attached() {
            return false;
        }
        *attachment = ShadowAttachment::withdrawn();
        tracing::warn!(
            "the shadow model candidate was withdrawn; the shadow is back on its own predictor"
        );
        true
    }

    /// The retained outcomes paired with their routing decisions, so the
    /// observability projection can be read back.
    ///
    /// Not a routing input and not a training source: a diagnostic view of the
    /// same single accounting the request log records.
    pub fn projections(&self) -> &ProjectionLog {
        &self.projections
    }

    /// Rebuild the upstream HTTP client (e.g. when bypass_proxy changes).
    pub fn rebuild_upstream(&self, bypass_proxy: bool) {
        self.upstream.rebuild_client(bypass_proxy);
    }

    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    pub fn config(&self) -> Arc<AppConfig> {
        self.registry().snapshot()
    }

    /// Swap in a new configuration, rebuilding the lookup tables once here
    /// instead of per request. In-flight requests keep their snapshot.
    pub fn set_config(&self, config: AppConfig) {
        self.stats.set_limit(config.server.log_limit);
        // The shadow switch and retention are a running engine's configuration,
        // so a swap has to reach the engine instead of only the next process.
        #[cfg(feature = "ml")]
        self.shadow.reconfigure(
            config.shadow.enabled,
            config.shadow.max_decisions,
            config.shadow.max_age_secs,
        );
        let known: std::collections::HashSet<String> =
            config.models.iter().map(|m| m.exposed_id()).collect();
        // Health rows for models that no longer exist would otherwise linger
        // forever and keep appearing in the GUI snapshot.
        self.router.retain_models(|id| known.contains(id));
        let registry = Arc::new(Registry::new(Arc::new(config)));
        *crate::sync::write(&self.registry) = registry;
    }

    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&crate::sync::read(&self.registry))
    }

    /// Resolve a provider's secret, failing loudly when one is expected but absent.
    pub fn api_key(&self, provider: &ProviderConfig) -> Result<Option<String>> {
        if provider.key_ref.is_empty() {
            return Ok(None);
        }
        match self.secrets.get(&provider.key_ref) {
            Some(k) if !k.is_empty() => Ok(Some(k)),
            _ => Err(Error::MissingApiKey(provider.name.clone())),
        }
    }

    /// Probe every tier member, rank the results, and pin the outcome.
    ///
    /// The runner lives here rather than in [`crate::election`] because it needs the
    /// registry, the HTTP client and the secret store, and keeping those out of the
    /// scoring module is what lets the scoring be a pure function with tests that
    /// never open a socket.
    ///
    /// Probes run one at a time on purpose: fired together they would queue behind
    /// each other on the same uplink and measure the queue instead of the providers.
    pub async fn hold_election(&self) -> Election {
        let registry = self.registry();
        let scoring = registry.config().routing.scoring.clone();
        let mut election = Election::new(scoring.clone());

        for tier in ModelTier::ALL {
            let members = registry.tier_members(tier);
            if members.is_empty() {
                continue;
            }

            let mut measurements = Vec::with_capacity(members.len());
            for entry in members {
                let exposed = entry.exposed_id();
                let mut measurement = Measurement::new(&exposed);
                measurement.priority = entry.priority;
                measurement.price = entry.pricing.as_ref().map(|p| scoring.reference_cost(p));

                measurement = match registry
                    .provider_of(entry)
                    .and_then(|provider| Ok((provider, self.api_key(provider)?)))
                {
                    Ok((provider, key)) => {
                        match self
                            .upstream
                            .probe(provider, key.as_deref(), &entry.upstream_model)
                            .await
                        {
                            Ok(latency) => {
                                // A probe is a real call, so it is also the freshest
                                // health signal there is.
                                let routing = &registry.config().routing;
                                self.router.report_success(&exposed, latency, routing);
                                measurement.answered(latency)
                            }
                            Err(e) => {
                                let routing = &registry.config().routing;
                                self.router.report_failure(&exposed, &e, routing);
                                measurement.failed(e.to_string())
                            }
                        }
                    }
                    Err(e) => measurement.failed(e.to_string()),
                };
                measurements.push(measurement);
            }

            election
                .tiers
                .insert(tier, election::rank(tier, &measurements, &scoring));
        }

        self.router.set_election(election.clone());
        election
    }
}

/// Build the axum application.
/// Every path the proxy answers on, in the order the docs list them.
///
/// Both prefixes are served. OpenAI clients are usually configured with a base
/// URL that already ends in `/v1` and append `/models` themselves, but plenty of
/// tools take the host on its own and do the same, so serving only one prefix
/// turns a working configuration into a bare 404.
const ENDPOINTS: &[&str] = &[
    "/v1/messages",
    "/v1/messages/count_tokens",
    "/v1/chat/completions",
    "/v1/responses",
    "/v1/responses/{response_id}",
    "/v1/responses/{response_id}/cancel",
    "/v1/generateContent",
    "/v1/models",
    "/v1/models/{id}",
    "/v1/status",
    "/health",
];

pub fn build_app(state: Arc<AppState>) -> AxumRouter {
    let cfg = state.config();
    let mut api = AxumRouter::new();
    // `/x` and `/v1/x` reach the same handler, so a base URL with or without the
    // version prefix both work.
    for prefix in ["", "/v1"] {
        api = api
            .route(&format!("{prefix}/messages"), post(anthropic_messages))
            .route(
                &format!("{prefix}/messages/count_tokens"),
                post(count_tokens),
            )
            .route(&format!("{prefix}/chat/completions"), post(openai_chat))
            .route(&format!("{prefix}/responses"), post(responses_chat))
            .route(
                &format!("{prefix}/responses/{{response_id}}"),
                get(get_response).delete(delete_response),
            )
            .route(
                &format!("{prefix}/responses/{{response_id}}/cancel"),
                post(cancel_response),
            )
            .route(&format!("{prefix}/generateContent"), post(gemini_generate))
            .route(&format!("{prefix}/models"), get(list_models))
            .route(&format!("{prefix}/models/{{id}}"), get(get_model))
            .route(&format!("{prefix}/status"), get(status))
            .route(&format!("{prefix}/ml/status"), get(ml_status))
            .route(&format!("{prefix}/ml/shadow"), get(ml_shadow))
            .route(&format!("{prefix}/ml/rollback"), post(ml_rollback))
            .route(&format!("{prefix}/ml/promote"), post(ml_promote))
            .route(
                &format!("{prefix}/ml/traces"),
                get(ml_traces).delete(ml_traces_clear),
            );
    }

    let mut app = api
        // Prompts with inline images are large, runaway bodies are not.
        .layer(DefaultBodyLimit::max(cfg.server.max_body_bytes()))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_layer,
        ))
        // Liveness only, and deliberately unauthenticated: it says nothing about
        // the configuration. `/v1/status` behind the token has the detail.
        .route("/health", get(health))
        // An unmatched path used to answer with an empty 404, which tells a user
        // nothing about whether the proxy is even running. A wrong method on a
        // real path is worth spelling out too.
        .fallback(unknown_route)
        .method_not_allowed_fallback(wrong_method)
        .with_state(Arc::clone(&state));

    if let Some(cors) = cors_layer(&cfg.server) {
        app = app.layer(cors);
    }
    app
}

/// Answer an unknown path with something a human can act on.
///
/// Only route names are listed, never configuration: this runs before
/// authentication, because an unmatched path never reaches the auth layer.
async fn unknown_route(method: Method, uri: Uri) -> Response {
    let path = uri.path().to_string();
    let error = Error::UnknownRoute(format!("{method} {path}"));
    // Anthropic clients parse a different error envelope, so answer in the shape
    // the caller is most likely to understand.
    let dialect = if path.contains("messages") || path.contains("count_tokens") {
        Dialect::Anthropic
    } else if path.contains("responses") {
        Dialect::OpenAIResponses
    } else if path.contains("generateContent") {
        Dialect::Gemini
    } else {
        Dialect::OpenAI
    };

    let mut hint = serde_json::Map::new();
    hint.insert("endpoints".into(), json!(ENDPOINTS));
    // The usual cause: a base URL that already ends in /v1 while the client
    // appends /v1 as well, which the Anthropic SDKs do.
    if let Some(rest) = path.strip_prefix("/v1/v1/") {
        hint.insert(
            "likely_cause".into(),
            json!(format!(
                "the base URL already ends in /v1 and the client added another; \
                 drop the /v1 from the base URL and this becomes /v1/{rest}"
            )),
        );
    }
    let mut body = error.to_wire(dialect);
    if let Some(object) = body.as_object_mut() {
        object.insert("zroutery".into(), Value::Object(hint));
    }
    (error.status(), Json(body)).into_response()
}

/// The path exists but not for this verb. Say which one it wants, since an empty
/// 405 reads exactly like a broken proxy.
///
/// Like the 404 handler this runs before authentication, so it names routes and
/// nothing else. That a well known path exists is already public.
async fn wrong_method(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    let wanted = if path.ends_with("/models") || path.ends_with("/status") {
        "GET"
    } else {
        "POST"
    };
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": {
                "message": format!("{path} is a {wanted} endpoint, not {method}"),
                "type": "invalid_request_error",
                "code": "method_not_allowed",
            },
            "zroutery": {"endpoints": ENDPOINTS},
        })),
    )
        .into_response()
}

/// Build the CORS layer for the configured origins.
///
/// Browsers are the only reason this exists, so the allowed methods and headers
/// are pinned to what the two APIs actually use instead of `Any`. An empty origin
/// list with CORS enabled means "any origin", which `AppConfig::validate` flags as
/// a warning and the dashboard shows in red.
fn cors_layer(server: &ServerConfig) -> Option<CorsLayer> {
    if !server.allow_cors {
        return None;
    }
    let mut layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            HeaderName::from_static("x-api-key"),
            HeaderName::from_static("anthropic-version"),
            HeaderName::from_static("anthropic-beta"),
        ])
        .max_age(Duration::from_secs(600));

    // Origins split into valid / invalid. An empty *configured* list means
    // "allow any", but if the user configured origins and none of them are
    // usable, deny everything rather than silently widening to `Any` — a
    // typo must never turn into an open CORS policy.
    match origin_policy(&server.cors_origins) {
        OriginPolicy::Any => layer = layer.allow_origin(Any),
        OriginPolicy::List(valid) => {
            let origins: Vec<HeaderValue> = valid
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect();
            layer = layer.allow_origin(AllowOrigin::list(origins));
        }
        OriginPolicy::DenyAll => layer = layer.allow_origin(AllowOrigin::list(Vec::new())),
    }
    Some(layer)
}

/// What to allow, derived from the configured `cors_origins`.
#[derive(Debug, PartialEq, Eq)]
enum OriginPolicy {
    /// No origins configured: allow any.
    Any,
    /// These (validated) origins are allowed.
    List(Vec<String>),
    /// Origins were configured but none are valid: allow none. A typo must
    /// never widen an explicit allow-list into an open policy.
    DenyAll,
}

fn origin_policy(configured: &[String]) -> OriginPolicy {
    let valid: Vec<String> = configured
        .iter()
        .map(|o| o.trim())
        .filter(|o| is_valid_origin(o))
        .map(str::to_string)
        .collect();
    if valid.is_empty() {
        let any_configured = configured.iter().any(|o| !o.trim().is_empty());
        if any_configured {
            tracing::warn!("every configured cors_origin is invalid; no origin will be allowed");
            OriginPolicy::DenyAll
        } else {
            OriginPolicy::Any
        }
    } else {
        if valid.len() != configured.len() {
            tracing::warn!("some cors_origins entries are invalid and were ignored");
        }
        OriginPolicy::List(valid)
    }
}

/// Whether `origin` is a well-formed `scheme://host[:port]` value usable as a
/// CORS allow-origin entry. Rejects paths, queries, fragments, whitespace,
/// userinfo, non-ASCII text and invalid ports — those are config mistakes, and
/// treating them as "no origins" would silently open the proxy to `Any`.
pub(crate) fn is_valid_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    if rest.is_empty() || !rest.is_ascii() {
        return false;
    }
    // No path / query / fragment / whitespace / userinfo after the authority.
    if rest.contains(['/', '?', '#', ' ', '\t', '@']) {
        return false;
    }

    let (host, port) = if let Some(stripped) = rest.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:8787.
        let Some(close) = stripped.find(']') else {
            return false;
        };
        let host = &stripped[..close];
        let after = &stripped[close + 1..];
        if host.is_empty() {
            return false;
        }
        if after.is_empty() {
            (host, None)
        } else if let Some(port) = after.strip_prefix(':') {
            (host, Some(port))
        } else {
            return false;
        }
    } else {
        match rest.rsplit_once(':') {
            None => (rest, None),
            Some((_, "")) => return false, // trailing colon, no port
            Some((host, _)) if host.contains(':') => return false, // bare IPv6 without []
            Some((host, port)) => (host, Some(port)),
        }
    };

    if host.is_empty() {
        return false;
    }
    if let Some(port) = port {
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if port.parse::<u16>().is_err() {
            return false;
        }
    }
    true
}

/// A running server plus its shutdown handle.
pub struct ServerHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    /// Bind and serve. Returns as soon as the socket is listening.
    pub async fn start(state: Arc<AppState>) -> Result<ServerHandle> {
        let cfg = state.config();
        let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("cannot bind {addr}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::internal(format!("cannot read local addr: {e}")))?;

        let app = build_app(state);
        let (tx, rx) = oneshot::channel();
        let join = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            if let Err(e) = served {
                tracing::error!("server stopped: {e}");
            }
        });

        Ok(ServerHandle {
            addr,
            shutdown: Some(tx),
            join,
        })
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

// ------------------------------------------------------------------- handlers

/// Liveness probe. Says nothing about the configuration on purpose: it is the
/// only route that does not require the token.
async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

/// The detail that `/health` used to leak, behind authentication.
async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let registry = state.registry();
    let cfg = registry.config();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "models": registry.list().len(),
        "providers": cfg.providers.iter().filter(|p| p.enabled).count(),
        "auth_required": cfg.server.require_auth,
    }))
}

/// The optional bound on a replay, from the query string.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

#[cfg(feature = "ml")]
impl LimitQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_SHADOW_LIMIT)
    }
}

/// Records read by an on-demand replay when the caller does not say.
///
/// Sized to be comfortable to run from a UI click while still covering enough
/// traffic for the agreement and regret numbers to mean something. The caller
/// can ask for more; nobody should want to.
#[cfg(feature = "ml")]
const DEFAULT_SHADOW_LIMIT: usize = 5_000;

/// Read the learned model's status.
///
/// Read-only, and behind the same auth layer as everything else: an operator
/// surface that leaked which model is serving, and on whose authority, over an
/// unauthenticated port would be a disclosure.
///
/// Without the `ml` feature this reports `available: false` rather than 404, so
/// a client can tell "this build has no ML stack" from "this build does and the
/// request went somewhere wrong".
#[cfg(feature = "ml")]
async fn ml_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let status = state.ml_status();
    Json(json!({
        "available": true,
        "routing_with_a_model": status.is_routing_with_a_model(),
        "headline": status.headline(),
        "status": status,
    }))
}

#[cfg(not(feature = "ml"))]
async fn ml_status() -> Json<Value> {
    Json(json!({
        "available": false,
        "routing_with_a_model": false,
        "headline": "this build contains no ML stack; routing is deterministic",
    }))
}

/// Replay the serving model over recorded history, on demand.
///
/// Bounded by the `limit` query parameter, and capped again inside, because this
/// is a button in a desktop app and an unbounded replay is a hang.
#[cfg(feature = "ml")]
async fn ml_shadow(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(limit): axum::extract::Query<LimitQuery>,
) -> Json<Value> {
    Json(json!(state.ml_shadow_analysis(limit.limit())))
}

#[cfg(not(feature = "ml"))]
async fn ml_shadow() -> Json<Value> {
    Json(json!({
        "available": false,
        "traces_read": 0,
        "reason": "this build contains no ML stack",
    }))
}

/// Roll back to the previously promoted model, or to deterministic routing if
/// there was not one.
#[cfg(feature = "ml")]
async fn ml_rollback(State(state): State<Arc<AppState>>) -> Json<Value> {
    let outcome = state.rollback_active_model();
    Json(json!({
        "outcome": outcome,
        "status": state.ml_status(),
    }))
}

#[cfg(not(feature = "ml"))]
async fn ml_rollback() -> Json<Value> {
    Json(json!({
        "error": "this build contains no ML stack, so nothing can be rolled back",
    }))
}

/// Inspect the durable trace log, and let an operator discard it.
///
/// `GET` reports size and record count without reading the file into memory —
/// `count` streams, and it is the same number the promotion gate uses to decide
/// whether a body is trainable.
///
/// `DELETE` exists because the log is the operator's evidence and only the
/// operator should decide to destroy it. There is no automatic retention: the
/// promotion round trains from the whole log, so a retention policy that fired on
/// its own would change what the next model learns from without anyone asking.
/// The response says what was removed *and* what it affects, because the second
/// is the part that is not obvious.
///
/// It is a separate route from `status` rather than a parameter on it: this is
/// destructive and there is no reason it should be reachable by a typo'd query
/// string on a read.
#[cfg(feature = "ml")]
async fn ml_traces(State(state): State<Arc<AppState>>) -> Json<Value> {
    // The instance `AppState` holds, not a freshly opened one. Two `TraceLog`s on
    // one path would be two independent handles, and reporting on a second one
    // would describe a file nobody is appending to.
    let Some(log) = state.traces() else {
        return Json(json!({
            "error": "no durable state directory is configured, so there is no trace log",
        }));
    };
    match log.count() {
        Ok(records) => Json(json!({
            "records": records,
            "bytes_on_disk": std::fs::metadata(log.path()).map(|m| m.len()).unwrap_or(0),
            "path": log.path().display().to_string(),
            "counters": log.counters(),
        })),
        Err(error) => Json(json!({ "error": error.to_string() })),
    }
}

/// The no-ML twin. Present because the route is registered unconditionally, and
/// because a 404 on a read would be a worse answer than "this build keeps no
/// trace log" -- the caller asked a fair question of a build that genuinely has
/// nothing to report.
#[cfg(not(feature = "ml"))]
async fn ml_traces() -> Json<Value> {
    Json(json!({
        "available": false,
        "error": "this build contains no ML stack, so it keeps no trace log",
    }))
}

#[cfg(feature = "ml")]
async fn ml_traces_clear(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Through the live log, so `clear` drops the append handle this process is
    // actually holding. Clearing a second handle would truncate the file and then
    // leave the serving path holding a handle to a file it had already written.
    let outcome = match state.traces() {
        None => json!({
            "error": "no durable state directory is configured, so there is no trace log",
        }),
        Some(log) => match log.clear() {
            Ok(cleared) => json!({
                "cleared": cleared.cleared,
                "removed_bytes": cleared.removed_bytes,
                "note": "the durable trace log is now empty; the in-memory dataset still \
                         holds recent samples, and the next promotion round trains only \
                         on records written from here on",
            }),
            Err(error) => json!({ "error": error.to_string() }),
        },
    };
    Json(outcome)
}

#[cfg(not(feature = "ml"))]
async fn ml_traces_clear() -> Json<Value> {
    Json(json!({
        "error": "this build contains no ML stack, so it keeps no trace log to clear",
    }))
}

/// Run one promotion round over the durable history.
///
/// `install` is a query parameter rather than the default, because judging a model
/// and installing it are different acts and only the second changes what every
/// later request is served by. `POST /v1/ml/promote` with no `install` reports what
/// the gate would decide and changes nothing, which makes the endpoint safe to poll
/// and safe to point a dashboard at. `?install=true` asks for the model to go live,
/// and then it is on the live router before the response is written, because
/// `reload_active_model` attaches in-process rather than waiting for a restart.
///
/// Nothing here schedules. When a round happens is the caller's decision, which is
/// why the endpoint has no timer and no cadence of its own.
#[cfg(feature = "ml")]
async fn ml_promote(
    State(state): State<Arc<AppState>>,
    // Fully qualified, like `ml_shadow` above. The bare `Query` would need an
    // import that is only used from ml-gated handlers, so a `--no-default-features`
    // build would warn about an unused import on a row CI actually runs.
    axum::extract::Query(request): axum::extract::Query<PromoteRequest>,
) -> Json<Value> {
    // Only the baseline is the caller's to choose. The evidence floors — minimum
    // paired requests, required utility delta, permitted regressions — stay at their
    // shipped values, because relaxing those is not a statement about *what* to hold
    // a model to, it is a decision to stop requiring evidence at all.
    let mut gate = crate::ml::PromotionConfig::default();
    if let Some(baseline) = request.baseline.as_deref() {
        gate.required_baseline = baseline.to_string();
    }
    let config = crate::ml::RoundConfig {
        gate,
        ..crate::ml::RoundConfig::default()
    };
    let outcome = state.ml_run_promotion_round(config, request.install, request.revision);
    Json(json!({
        "round": outcome,
        "status": state.ml_status(),
    }))
}

#[cfg(not(feature = "ml"))]
async fn ml_promote() -> Json<Value> {
    Json(json!({
        "error": "this build contains no ML stack, so there is nothing to promote",
    }))
}

/// What to do about the round a `POST /v1/ml/promote` just ran.
///
/// Gated with the handler that deserialises it. Ungated it is dead code in a
/// `--no-default-features` build, and that is a row CI runs.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct PromoteRequest {
    /// Install the model if — and only if — the gate authorised it.
    ///
    /// Defaults to false. A promotion is the most consequential thing this process
    /// does on request, so it is asked for rather than inferred.
    #[serde(default)]
    install: bool,
    /// Recorded on the gate decision, so a promotion or refusal can be tied back to
    /// the round that produced it.
    #[serde(default)]
    revision: Option<String>,
    /// Which baseline the model must beat.
    ///
    /// "Beats a baseline" is a claim about a *specific* baseline, so the endpoint
    /// names one rather than letting the promotion quietly pick whichever it
    /// happened to win against. The shipped default is `baseline.lowest_latency`;
    /// `baseline.priority` is the strategy Zroutery routes by, and is what most
    /// operators mean. This changes only which comparison is required — the evidence
    /// floors are not caller-controlled.
    #[serde(default)]
    baseline: Option<String>,
}

async fn auth_layer(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let cfg = state.config();
    if !cfg.server.require_auth {
        return next.run(request).await;
    }
    let expected = cfg.server.auth_token.as_bytes();
    let presented = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
        });

    let ok = match presented {
        Some(token) if !expected.is_empty() => constant_time_eq(token.as_bytes(), expected),
        _ => false,
    };
    if !ok {
        let path = request.uri().path();
        let dialect = if path.contains("messages") || path.contains("count_tokens") {
            Dialect::Anthropic
        } else if path.contains("responses") {
            Dialect::OpenAIResponses
        } else if path.contains("generateContent") {
            Dialect::Gemini
        } else {
            Dialect::OpenAI
        };
        return error_response(dialect, &Error::Unauthorized);
    }
    next.run(request).await
}

/// Length-independent comparison so timing does not leak how much of a
/// presented token was right, or how long the expected one is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    // Walk the longer side; zipping alone would stop at the shorter and give
    // length information back through runtime.
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

type JsonBody = std::result::Result<Json<Value>, JsonRejection>;

fn unwrap_body(state: &AppState, body: JsonBody) -> Result<Value> {
    match body {
        Ok(Json(v)) => Ok(v),
        // A body over the limit is a different problem from malformed JSON, and
        // clients back off differently for 413 than for 400.
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(Error::TooLarge {
            limit_mib: state.config().server.max_body_mib,
        }),
        Err(e) => Err(Error::invalid(e.body_text())),
    }
}

async fn anthropic_messages(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };
    handle_chat(state, Dialect::Anthropic, headers, body).await
}

async fn openai_chat(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::OpenAI, &e),
    };
    handle_chat(state, Dialect::OpenAI, headers, body).await
}

async fn responses_chat(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::OpenAIResponses, &e),
    };
    handle_chat(state, Dialect::OpenAIResponses, headers, body).await
}

async fn gemini_generate(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Gemini, &e),
    };
    handle_chat(state, Dialect::Gemini, headers, body).await
}

async fn count_tokens(State(state): State<Arc<AppState>>, body: JsonBody) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(v) => v,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };
    let req = match protocol::decode_request(Dialect::Anthropic, body) {
        Ok(req) => req,
        Err(e) => return error_response(Dialect::Anthropic, &e),
    };

    // Best effort: providers do not expose a shared tokenizer, so this is an
    // estimate, and the price beside it covers the prompt only.
    let estimate = req.estimate_tokens();
    let mut extra = serde_json::Map::new();
    extra.insert("estimated".into(), json!(true));
    if let Some((model_id, pricing)) = first_candidate_pricing(&state, &req.model) {
        extra.insert("model".into(), json!(model_id));
        if let Some(pricing) = pricing {
            let cost = pricing.estimate_input(estimate);
            extra.insert(
                "estimated_input_cost".into(),
                json!({"currency": cost.currency, "amount": cost.amount}),
            );
            extra.insert("input_per_mtok".into(), json!(pricing.input_per_mtok));
            extra.insert("output_per_mtok".into(), json!(pricing.output_per_mtok));
        }
    }

    Json(json!({
        "input_tokens": estimate,
        // Namespaced, so Anthropic clients that only read `input_tokens` ignore it.
        "zroutery": Value::Object(extra),
    }))
    .into_response()
}

/// Which model would answer this request, and what it charges.
fn first_candidate_pricing(state: &AppState, requested: &str) -> Option<(String, Option<Pricing>)> {
    let registry = state.registry();
    let resolution = registry.resolve(requested).ok()?;
    let plan = state.router().plan(&registry, &resolution, &[]).ok()?;
    let candidate = plan.into_iter().next()?;
    Some((candidate.exposed_id, candidate.entry.pricing))
}

async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let registry = state.registry();
    let items: Vec<Value> = registry.list().iter().map(model_json).collect();
    let first = items.first().and_then(|m| m["id"].as_str()).unwrap_or("");
    let last = items.last().and_then(|m| m["id"].as_str()).unwrap_or("");
    Json(json!({
        "object": "list",
        "data": items,
        "has_more": false,
        "first_id": first,
        "last_id": last,
    }))
    .into_response()
}

async fn get_model(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let registry = state.registry();
    match registry.list().iter().find(|m| m.id == id) {
        Some(info) => Json(model_json(info)).into_response(),
        None => error_response(Dialect::OpenAI, &Error::UnknownModel(id)),
    }
}

/// One listing entry, carrying both the Anthropic and the OpenAI field names.
fn model_json(info: &crate::registry::ModelInfo) -> Value {
    let now = chrono::Utc::now();
    json!({
        "id": info.id,
        "object": "model",
        "type": "model",
        "created": now.timestamp(),
        "created_at": now.to_rfc3339(),
        "display_name": info.display_name,
        "owned_by": info.provider_name.clone().unwrap_or_else(|| "zroutery".into()),
        "zroutery": {
            "tier": info.tier,
            "virtual": info.virtual_model,
            "member_count": info.member_count,
            "provider": info.provider_name,
            "supports_tools": info.capabilities.tools,
            "supports_vision": info.capabilities.vision,
            "supports_thinking": info.capabilities.thinking,
            "capabilities": info.capabilities,
        }
    })
}

async fn get_response(
    State(state): State<Arc<AppState>>,
    Path(response_id): Path<String>,
) -> Response {
    match state.response_store.get(&response_id) {
        Some(resp) => Json(serde_json::to_value(&resp).unwrap()).into_response(),
        None => error_response(
            Dialect::OpenAIResponses,
            &Error::invalid("response not found"),
        ),
    }
}

async fn delete_response(
    State(state): State<Arc<AppState>>,
    Path(response_id): Path<String>,
) -> Response {
    if state.response_store.delete(&response_id) {
        Json(json!({"id": response_id, "object": "response", "deleted": true})).into_response()
    } else {
        error_response(
            Dialect::OpenAIResponses,
            &Error::invalid("response not found"),
        )
    }
}

async fn cancel_response(
    State(state): State<Arc<AppState>>,
    Path(response_id): Path<String>,
) -> Response {
    // Cancellation stores a content-free placeholder itself, but only when the
    // request asked for retention; a `store: false` client gets the status
    // without anything being kept.
    if state.response_store.cancel(&response_id, "unknown") {
        // Return the cancelled response.
        match state.response_store.get(&response_id) {
            Some(resp) => Json(serde_json::to_value(&resp).unwrap()).into_response(),
            None => Json(json!({
                "id": response_id, "object": "response", "status": "cancelled"
            }))
            .into_response(),
        }
    } else {
        // Not in-flight — check if it is a completed response.
        match state.response_store.get(&response_id) {
            Some(resp) => Json(serde_json::to_value(&resp).unwrap()).into_response(),
            None => error_response(
                Dialect::OpenAIResponses,
                &Error::invalid("response not found"),
            ),
        }
    }
}

fn error_response(dialect: Dialect, err: &Error) -> Response {
    (err.status(), Json(err.to_wire(dialect))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderKind;

    /// Read a handler's JSON body back out.
    async fn body_json(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[test]
    fn token_comparison_is_length_safe_and_exact() {
        assert!(constant_time_eq(b"zr-abc", b"zr-abc"));
        assert!(!constant_time_eq(b"zr-abc", b"zr-abd"));
        // A prefix must not pass, which is what a naive loop would allow.
        assert!(!constant_time_eq(b"zr-ab", b"zr-abc"));
        assert!(!constant_time_eq(b"", b"zr-abc"));
        assert!(constant_time_eq(b"", b""));
        // Length differences that alias through a narrow accumulator must not
        // sneak past either.
        let zeros = vec![0u8; 256];
        assert!(!constant_time_eq(b"", &zeros));
        assert!(constant_time_eq(&zeros, &zeros));
    }

    #[tokio::test]
    async fn an_unknown_path_answers_in_the_likely_dialect() {
        // A models path is an OpenAI client's, so use its envelope.
        let response = unknown_route(Method::GET, "/v2/models".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "not_found_error");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("GET /v2/models"));
        assert!(body["zroutery"]["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "/v1/models"));

        // Anything that mentions messages is an Anthropic client's.
        let response = unknown_route(Method::POST, "/v1/messages/typo".parse().unwrap()).await;
        let body = body_json(response).await;
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "not_found_error");
    }

    #[tokio::test]
    async fn a_doubled_version_prefix_is_named_as_the_cause() {
        let response = unknown_route(Method::POST, "/v1/v1/messages".parse().unwrap()).await;
        let body = body_json(response).await;
        let cause = body["zroutery"]["likely_cause"].as_str().unwrap();
        assert!(cause.contains("base URL already ends in /v1"), "{cause}");
        assert!(cause.contains("/v1/messages"), "{cause}");

        // A single prefix is normal and gets no lecture.
        let response = unknown_route(Method::POST, "/v1/nope".parse().unwrap()).await;
        assert!(body_json(response).await["zroutery"]["likely_cause"].is_null());
    }

    #[tokio::test]
    async fn a_wrong_verb_names_the_verb_it_wants() {
        let response = wrong_method(Method::GET, "/v1/chat/completions".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let message = body_json(response).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("POST endpoint"), "{message}");
        assert!(message.contains("not GET"), "{message}");

        // Listings are the other way round.
        let response = wrong_method(Method::POST, "/v1/models".parse().unwrap()).await;
        let message = body_json(response).await["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("GET endpoint"), "{message}");
    }

    #[test]
    fn cors_is_off_unless_asked_for() {
        let mut server = ServerConfig::default();
        assert!(cors_layer(&server).is_none());
        server.allow_cors = true;
        assert!(cors_layer(&server).is_some());
        // An empty list still builds a layer, and `validate` is what complains.
        server.cors_origins = vec!["http://localhost:3000".into()];
        assert!(cors_layer(&server).is_some());
        // A malformed origin is dropped rather than rejected: the rest still works.
        server.cors_origins = vec!["not a header value\n".into()];
        assert!(cors_layer(&server).is_some());
    }

    #[test]
    fn is_valid_origin_rejects_paths_scheme_less_and_bad_ports() {
        assert!(is_valid_origin("http://localhost:3000"));
        assert!(is_valid_origin("https://app.example.com"));
        assert!(is_valid_origin("tauri://localhost"));
        assert!(is_valid_origin("http://[::1]:8787"));
        assert!(is_valid_origin("http://[::1]"));
        assert!(!is_valid_origin("https://app.example.com/path"));
        assert!(!is_valid_origin("https://app.example.com?q=1"));
        assert!(!is_valid_origin("app.example.com"));
        assert!(!is_valid_origin(""));
        assert!(!is_valid_origin("has space://x"));
        assert!(!is_valid_origin("https://app.example.com:99999"));
        assert!(!is_valid_origin("https://app.example.com:abc"));
        assert!(!is_valid_origin("https://app.example.com:"));
        assert!(!is_valid_origin("https://user@example.com"));
        assert!(!is_valid_origin("https://例子.测试"));
    }

    #[test]
    fn origin_policy_never_widens_on_invalid_entries() {
        use OriginPolicy::*;
        // Nothing configured: documented Any.
        assert_eq!(origin_policy(&[]), Any);
        // Valid entries: a list.
        assert_eq!(
            origin_policy(&["http://a".into(), "https://b".into()]),
            List(vec!["http://a".to_string(), "https://b".to_string()])
        );
        // Mixed: invalid entries are dropped, valid ones survive.
        assert_eq!(
            origin_policy(&["https://ok".into(), "garbage".into()]),
            List(vec!["https://ok".into()])
        );
        // All invalid (or whitespace-only): deny — never widen to Any.
        assert_eq!(origin_policy(&["garbage".into()]), DenyAll);
        assert_eq!(origin_policy(&["   ".into()]), Any);
    }

    #[test]
    fn a_missing_key_is_a_precondition_not_a_silent_none() {
        let secrets =
            Arc::new(crate::config::MemorySecretStore::new().with("provider:has", "sk-1"));
        let state = AppState::new(AppConfig::default(), secrets);

        let mut provider = ProviderConfig::new("has", "Has", ProviderKind::OpenAICompatible);
        provider.key_ref = "provider:has".into();
        assert_eq!(state.api_key(&provider).unwrap().as_deref(), Some("sk-1"));

        provider.key_ref = "provider:missing".into();
        assert!(matches!(
            state.api_key(&provider),
            Err(Error::MissingApiKey(_))
        ));

        // A provider that needs no credential says so explicitly.
        provider.key_ref = String::new();
        assert!(state.api_key(&provider).unwrap().is_none());
    }

    #[test]
    fn swapping_the_config_rebuilds_the_registry_with_it() {
        let secrets = Arc::new(crate::config::MemorySecretStore::new());
        let state = AppState::new(AppConfig::default(), secrets);
        assert!(state.registry().list().is_empty());

        let mut next = AppConfig::default();
        next.providers.push(ProviderConfig::new(
            "p",
            "P",
            ProviderKind::OpenAICompatible,
        ));
        next.models.push(crate::config::ModelEntry::for_upstream(
            "p",
            "m",
            Some(crate::config::ModelTier::Standard),
        ));
        state.set_config(next);

        // The cached index has to move with the document, not lag behind it.
        assert_eq!(state.config().models.len(), 1);
        assert!(state.registry().list().iter().any(|m| m.id == "p-m"));
        assert!(state
            .registry()
            .resolve("standard-class")
            .is_ok_and(|r| matches!(r, crate::registry::Resolution::Tier(_))));
    }

    /// The shadow switch and retention are configuration the running engine
    /// carries: the documented limits arrive at construction, and `set_config`
    /// applies a changed switch and smaller limits without a restart.
    #[cfg(feature = "ml")]
    #[test]
    fn swapping_the_config_applies_shadow_settings_to_the_running_engine() {
        let secrets = Arc::new(crate::config::MemorySecretStore::new());
        let mut initial = AppConfig::default();
        initial.shadow.enabled = true;
        initial.shadow.max_decisions = 5;
        initial.shadow.max_age_secs = 60;
        let state = AppState::new(initial, secrets);
        assert!(state.shadow().enabled());
        assert_eq!(
            state.shadow().store().limits(),
            (5, 60),
            "the configured retention must reach the store"
        );

        let mut next = state.config().as_ref().clone();
        next.shadow.enabled = false;
        next.shadow.max_decisions = 2;
        next.shadow.max_age_secs = 30;
        state.set_config(next);
        assert!(
            !state.shadow().enabled(),
            "turning shadow off must reach the running engine"
        );
        assert_eq!(
            state.shadow().store().limits(),
            (2, 30),
            "the new retention must reach the running store"
        );

        let mut back_on = state.config().as_ref().clone();
        back_on.shadow.enabled = true;
        state.set_config(back_on);
        assert!(
            state.shadow().enabled(),
            "turning shadow back on must reach the running engine"
        );
    }

    /// The global day spend the ledger currently holds.
    fn global_day_spend(state: &AppState) -> f64 {
        state
            .ledger()
            .totals_for(&crate::budget::BudgetScope::Global, Local::now())
            .into_iter()
            .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
            .map(|(_, cost)| cost.amount)
            .unwrap_or(0.0)
    }

    fn state_with_charge(amount: f64) -> AppState {
        let state = AppState::new(
            AppConfig::default(),
            Arc::new(crate::config::MemorySecretStore::new()),
        );
        state.charge(
            "provider",
            Some(ModelTier::Standard),
            &Cost {
                currency: "USD".into(),
                amount,
            },
        );
        state
    }

    #[test]
    fn a_failed_flush_keeps_the_pending_spend_and_the_next_flush_retries() {
        let state = state_with_charge(1.0);
        let mut failed_attempts = 0;

        let result = state.flush_ledger(|_| {
            failed_attempts += 1;
            Err("disk is not writable".to_string())
        });
        assert_eq!(result, Err("disk is not writable".to_string()));
        assert_eq!(failed_attempts, 1);
        assert_eq!(
            global_day_spend(&state),
            1.0,
            "a failed write must not drop the spend it could not persist"
        );

        // The disk recovers: the very next flush must still see the pending
        // spend and write it, not report "nothing changed".
        let mut written: Option<f64> = None;
        state
            .flush_ledger(|ledger| {
                written = Some(
                    ledger
                        .totals_for(&crate::budget::BudgetScope::Global, Local::now())
                        .into_iter()
                        .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
                        .map(|(_, cost)| cost.amount)
                        .unwrap_or(0.0),
                );
                Ok(())
            })
            .expect("the retry succeeds once the destination is writable again");
        assert_eq!(written, Some(1.0));

        // Nothing is pending after a successful write, so a flush on an idle
        // proxy does not rewrite the file.
        let mut writes_after_success = 0;
        state
            .flush_ledger(|_| {
                writes_after_success += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(writes_after_success, 0);
    }

    #[test]
    fn a_charge_that_lands_during_a_write_survives_the_confirmation() {
        let state = state_with_charge(1.0);

        // The charge arrives while the snapshot is on its way to disk. Clearing
        // the dirty flag unconditionally afterwards would lose it.
        state
            .flush_ledger(|_| {
                state.charge(
                    "provider",
                    Some(ModelTier::Standard),
                    &Cost {
                        currency: "USD".into(),
                        amount: 0.5,
                    },
                );
                Ok(())
            })
            .expect("the first write succeeds");

        let mut written: Option<f64> = None;
        state
            .flush_ledger(|ledger| {
                written = Some(
                    ledger
                        .totals_for(&crate::budget::BudgetScope::Global, Local::now())
                        .into_iter()
                        .find(|(period, _)| *period == crate::budget::BudgetPeriod::Day)
                        .map(|(_, cost)| cost.amount)
                        .unwrap_or(0.0),
                );
                Ok(())
            })
            .expect("the mid-write charge is flushed next");
        assert_eq!(written, Some(1.5));
    }

    #[test]
    fn concurrent_flushes_never_run_their_writes_at_the_same_time() {
        let state = Arc::new(state_with_charge(1.0));
        let inside = Arc::new(AtomicBool::new(false));
        let overlap = Arc::new(AtomicBool::new(false));

        let mut threads = Vec::new();
        for _ in 0..8 {
            let state = Arc::clone(&state);
            let inside = Arc::clone(&inside);
            let overlap = Arc::clone(&overlap);
            threads.push(std::thread::spawn(move || {
                state.flush_ledger(|_| {
                    if inside.swap(true, Ordering::AcqRel) {
                        overlap.store(true, Ordering::Release);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    inside.store(false, Ordering::Release);
                    Ok(())
                })
            }));
        }
        for thread in threads {
            thread.join().expect("no flush panicked").unwrap();
        }
        assert!(
            !overlap.load(Ordering::Acquire),
            "two flushes wrote at once, so an older snapshot could overwrite a newer one"
        );
    }

    fn budget_scope_state() -> AppState {
        AppState::new(
            AppConfig::default(),
            Arc::new(crate::config::MemorySecretStore::new()),
        )
    }

    /// One permit per scope, released by the drop and never by an explicit call.
    #[tokio::test]
    async fn an_admitted_scope_is_exclusive_until_the_guard_drops() {
        let state = budget_scope_state();
        let scope = BudgetScope::Global;

        let held = state.admit(std::slice::from_ref(&scope)).await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                state.admit(std::slice::from_ref(&scope))
            )
            .await
            .is_err(),
            "a second request for a held scope must wait, not run against the same total"
        );
        drop(held);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(500),
                state.admit(std::slice::from_ref(&scope))
            )
            .await
            .is_ok(),
            "dropping the guard must release the scope"
        );
    }

    /// A request that occupies no budgeted scope is never gated, and two
    /// different scopes do not block each other.
    #[tokio::test]
    async fn scopes_without_a_budget_stay_concurrent() {
        let state = budget_scope_state();
        let provider = BudgetScope::Provider {
            id: "deepseek".into(),
        };

        let held = state.admit(std::slice::from_ref(&provider)).await;
        // Nothing budgeted at all: admitted immediately, no waiting.
        assert!(
            tokio::time::timeout(Duration::from_millis(500), state.admit(&[]))
                .await
                .is_ok(),
            "an unbudgeted request must not wait on anyone"
        );
        // A different scope is a different gate.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(500),
                state.admit(&[BudgetScope::Tier {
                    tier: ModelTier::Standard
                }])
            )
            .await
            .is_ok(),
            "one held scope must not serialise another"
        );
        drop(held);
    }

    /// Removing a scope's budget while a request holds its permit must not
    /// strand the permit: the gate outlives the configuration that made it.
    #[tokio::test]
    async fn removing_a_budget_does_not_leak_an_in_flight_permit() {
        let state = budget_scope_state();
        let scope = BudgetScope::Global;
        let held = state.admit(std::slice::from_ref(&scope)).await;

        // The budget disappears while the request is still in flight.
        state.set_config(AppConfig::default());
        drop(held);

        assert!(
            tokio::time::timeout(
                Duration::from_millis(500),
                state.admit(std::slice::from_ref(&scope))
            )
            .await
            .is_ok(),
            "the released permit must still be the one the gate hands out"
        );
    }
}
