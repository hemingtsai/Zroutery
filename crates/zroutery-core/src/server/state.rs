//! The shared state: what every handler can reach, and the admission permits that keep budget scopes exclusive.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::budget::{BudgetScope, Ledger};
use crate::config::SecretStore;
use crate::ir::ResponseStore;
use crate::registry::Registry;
use crate::router::Router;
use crate::stats::{OutcomeLog, Stats};
use crate::upstream::Upstream;

use super::projection_log::ProjectionLog;
use super::shadow_candidate::*;

/// Everything a request handler needs.
///
/// Fields are private so nothing outside can leave the cached registry out of
/// step with the configuration it was built from.
pub struct AppState {
    /// Configuration plus its precomputed lookup tables, swapped together.
    pub(super) registry: RwLock<Arc<Registry>>,
    pub(super) router: Arc<Router>,
    pub(super) stats: Arc<Stats>,
    pub(super) upstream: Upstream,
    pub(super) secrets: Arc<dyn SecretStore>,
    /// Spend so far, which the budgets are checked against. It lives here rather
    /// than in `Stats` because the request log is deliberately memory only, and a
    /// guardrail that forgets on restart is not a guardrail.
    pub(super) ledger: RwLock<Ledger>,
    /// Set when the ledger has moved since it was last written out, so the desktop
    /// layer can flush on a timer instead of writing a file per request.
    ///
    /// It is cleared only after a write *succeeded*, so a failed flush leaves the
    /// pending spend in place for the next attempt instead of silently dropping
    /// it — a ledger that forgets a request is a budget that lets it through
    /// again after a restart.
    pub(super) ledger_dirty: AtomicBool,
    /// Bumped by every ledger mutation. A flush snapshots it before writing and
    /// compares it afterwards: a charge that landed while the file was being
    /// written keeps the dirty flag set, so the newer numbers are written next
    /// time instead of being swallowed by the snapshot that just succeeded.
    pub(super) ledger_generation: AtomicU64,
    /// Held across snapshot/write/confirm, so a timer flush and a shutdown flush
    /// cannot interleave into writing an older snapshot over a newer one.
    pub(super) ledger_flush: Mutex<()>,
    pub(super) accounting: Mutex<super::accounting::AccountingRuntime>,
    /// One admission gate per budgeted scope.
    ///
    /// A request takes a gate for every scope with a configured limit it could
    /// spend against, before the budget check, and gives it back only once the
    /// request has been settled and charged. That is what makes the documented
    /// overshoot bound real: without it, N concurrent requests all read the same
    /// historical total and all pass. A scope with no configured limit has no
    /// gate and never blocks.
    pub(super) admission: AdmissionGates,
    /// Shadow decision engine (Stage 7E-1): records what the ML routing stack
    /// would have done for policy-routed main traffic. Record-only by
    /// construction — nothing in the request pipeline reads its verdicts.
    #[cfg(feature = "ml")]
    pub(super) shadow: crate::ml::ShadowEngine,
    /// Terminal outcomes, one per request, as the lifecycle built them. A
    /// diagnostic view of the same single accounting the request log records;
    /// nothing routes on it and no training consumes it yet.
    pub(super) outcomes: OutcomeLog,
    /// Canonical training samples, ingested once per eligible request from the
    /// request's retained decision-time input and its own terminal Outcome.
    ///
    /// Collection only: nothing trains on it, reads a verdict from it, or lets
    /// it influence a response. Whether a request contributes a sample depends
    /// entirely on whether a decision-time record was retained for it, so
    /// `config.shadow.enabled` is the effective switch and there is deliberately
    /// no second, independent dataset switch.
    #[cfg(feature = "ml")]
    pub(super) dataset: crate::ml::DatasetStore,
    /// The learned model's role in serving, when `ml_routing.enabled` is on.
    ///
    /// Separate from `shadow`: the shadow records what the model would have
    /// done and its verdict is discarded, while this re-orders the executable
    /// plan. Both are off by default.
    #[cfg(feature = "ml")]
    pub(super) ml_routing: crate::ml::MlRouter,
    /// Runtime state for the accounts a provider hosts.
    ///
    /// **Runtime, not configuration.** What an operator declares — which accounts
    /// exist and where each credential lives — is `ProviderConfig::accounts`.
    /// What this holds is what probing *derived*: status, capabilities, quota,
    /// usage, rate limits, and when each last succeeded or failed. Those expire,
    /// so they live as long as the process and are never written to `AppConfig`.
    ///
    /// Owned here for the same reason `shadow` and `dataset` are: the contents
    /// come from serving, so the lifetime should be the process. It also makes the
    /// store a delegate for the desktop layer the way `Snapshot` is, which is how
    /// a GUI will read it.
    ///
    /// The field is gated on its own feature and nothing else. `account` and `ml`
    /// are independent, so turning accounts on does not perturb ML feature order.
    #[cfg(feature = "account")]
    pub(super) accounts: crate::account::AccountStore,
    /// The durable routing trace log. Appended from the same terminal
    /// transition that produces the sample, so the history a future training
    /// run reads is the history this process actually served.
    ///
    /// `None` when no state directory is configured, which is the default: no
    /// durable history at all, rather than history written somewhere implicit.
    #[cfg(feature = "ml")]
    pub(super) traces: Option<crate::ml::TraceLog>,
    /// The durable active-model pointer, when a state directory could be
    /// opened. `None` means no model can serve, which is the state a fresh
    /// installation is in and the state a rollback returns to.
    #[cfg(feature = "ml")]
    pub(super) active_models: Option<crate::ml::ActiveModelStore>,
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
    // The ml state is the only reader. A build without the feature keeps the
    // field because `with_shadow_attachment` is shared by both builds on
    // purpose: one constructor for both is what stops the two from drifting
    // apart, and a field that exists in only one of them would be the drift.
    #[cfg_attr(not(feature = "ml"), allow(dead_code))]
    pub(super) shadow_attachment: RwLock<ShadowAttachment>,
    /// One terminal outcome per request, paired with the routing decision that
    /// produced it, held so the observability projection is reachable from the
    /// serving path rather than only from tests.
    pub(super) projections: ProjectionLog,
    pub response_store: ResponseStore,
    pub(super) upstream_sessions: super::pipeline::session_binding::UpstreamSessions,
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
pub(super) struct AdmissionGates {
    pub(super) gates: Mutex<BTreeMap<BudgetScope, Arc<Semaphore>>>,
}

/// The admission permits one request holds until it has been settled.
///
/// Opaque on purpose: dropping it is the only way to release, so no terminal
/// path can charge the ledger without handing its scope to the next request.
/// An empty guard is the honest representation of "no scope this request
/// occupies has a configured limit", which is the case that must stay fully
/// concurrent.
pub(crate) struct AdmissionGuard {
    pub(super) _permits: Vec<OwnedSemaphorePermit>,
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
    pub(super) async fn admit(&self, scopes: &[BudgetScope]) -> AdmissionGuard {
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
