//! Desktop application state: owns the core proxy state, the keychain, the
//! config file location and the running server handle.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`views`] | the plain data the window layer reads: serialised across the Tauri boundary, carrying no behaviour |
//! | [`snapshot`] | the one call the dashboard polls |
//! | [`accounts`] | declared accounts, their maintenance and check-in state, and the browsers held open |
//! | [`balances`] | balances per provider, broken down per account only where the total hides something |
//! | [`ml`] | the learned model: status, promotion, the durable trace log |
//! | [`budget`] | activity counters, elections, and the ledger carried across runs |
//! | [`server`] | binding, rebinding and unbinding the listener, and saving the document around it |
//! | [`lifecycle`] | building a `Desktop`, and the warnings and window rules it holds |
//!
//! Seven of the nine children carry methods of `Desktop`. Rust merges split
//! inherent `impl` blocks, so this is legal rather than a workaround, but it
//! does make the table above the only map from a method name to a file.
//!
//! The `#[cfg]` gates here mirror the ones the items already carried. A flat
//! re-export list would not compile under the feature combinations the items
//! themselves support.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use zroutery_core::server::{AppState, ServerHandle};

use crate::secrets::KeychainSecrets;

pub struct Desktop {
    pub(crate) core: Arc<AppState>,
    pub(crate) secrets: Arc<KeychainSecrets>,
    pub(crate) config_dir: PathBuf,
    pub(super) server: AsyncMutex<Option<ServerHandle>>,
    /// Serialises configuration changes and listener migration, so a save that
    /// has to rebind cannot interleave with a concurrent stop or save.
    pub(super) migration: AsyncMutex<()>,
    /// Decides whether the listener can claim an address. Real in production,
    /// injectable so tests can simulate a port that cannot be bound.
    pub(super) prober: Box<dyn ListenerProber + Send + Sync>,
    /// Startup problem worth surfacing once in the UI.
    pub(super) warning: Mutex<Option<String>>,
    /// Last answer from each provider's balance endpoint. Never fetched on a
    /// timer: it costs a request and some vendors rate limit it.
    pub(super) balances: Mutex<BTreeMap<String, BalanceStatus>>,
    /// The window layer's live view of the lifecycle settings: the close
    /// handler reads this synchronously, `apply_config` keeps it current.
    pub(crate) window_rules: Mutex<WindowRules>,
    /// Browser check-in runtime, holding any live browser open across a pause.
    ///
    /// Owned here rather than in [`Snapshot`] for the same reason the server
    /// handle is: a browser is a process, not a reading. Nothing about it can be
    /// serialised into a document handed to the webview, and a snapshot that
    /// *tried* would be the place a session leaked. The snapshot reports whether
    /// a browser is held, which is the only thing a panel needs to decide what to
    /// offer.
    #[cfg(feature = "account-maint")]
    pub(crate) checkin: crate::checkin::CheckinRuntime,
    /// Last known check-in outcome per account, for the snapshot.
    ///
    /// Observations rather than the live session: they survive a browser closing,
    /// and they are the part a panel renders between runs. The live session is
    /// asked separately, because "is a browser open right now" and "what
    /// happened last time" are different questions and a panel that conflates them
    /// will offer to resume an attempt that finished hours ago.
    #[cfg(feature = "account-maint")]
    pub(super) checkin_reports: Mutex<BTreeMap<(String, String), CheckinView>>,
    /// When each account's last check-in settled, across restarts.
    ///
    /// Deliberately a *different* thing from `checkin_reports`, and the
    /// distinction is the reason the scheduler works at all. `checkin_reports`
    /// answers "what happened last time" and starts empty on every launch, which is
    /// correct for a panel: an account with no history should read as untried. The
    /// scheduler instead needs "when did this account last settle", and reading
    /// that from an in-memory map would make every account look untried after every
    /// restart — so one visible browser window per account, seconds after launch.
    #[cfg(feature = "account-maint")]
    pub(crate) checkin_schedule: crate::checkin::schedule::Schedule,
    /// What has actually been observed happening to accounts' resources.
    ///
    /// Bounded and in memory, on purpose: this is a record of what the providers
    /// said, kept so an operator can see why a balance moved and how strongly that
    /// was established. It is deliberately not the source of a balance — the
    /// provider is — and a running sum of partially-observed events drifts.
    ///
    /// It exists because the observation layer was written, tested and then held by
    /// nothing. Holding it here is what makes the events reachable at all.
    #[cfg(feature = "account-maint")]
    pub(crate) account_events: zroutery_core::account::AccountEventLog,
    /// Per-agent takeover ownership stores, opened lazily against the durable
    /// manifests under `<config_dir>/takeover/`.
    ///
    /// Held here rather than reopened per command so one long-lived process
    /// keeps one view of each manifest; the manifests themselves are what
    /// survive a restart.
    takeover_stores: Mutex<
        BTreeMap<
            zroutery_core::agent_takeover::AgentType,
            Arc<zroutery_core::agent_takeover::TakeoverStore>,
        >,
    >,
    /// The cutover record and its rollback snapshot, opened on first use.
    ///
    /// Durable by construction: the three states that mean a cutover started
    /// and did not finish are the ones a restart has to be able to recover
    /// from, and a record that lives only in this struct reports `Detected` for
    /// all three once the process is gone. The manifest and the snapshot are
    /// what survive; this holds the one open handle to them.
    ///
    /// Named `cutover` and not `migration` because `Desktop::migration` above
    /// already exists and is a different thing entirely -- the lock that
    /// serialises listener rebinds against saves.
    cutover: Mutex<Option<Arc<zroutery_core::migration::MigrationExecutor>>>,
}

/// Whether a listener can claim an address.
///
/// Checking before the old listener is stopped is what keeps a change of port
/// transactional: an address somebody else already holds is refused while the
/// running gateway and the saved configuration are both left alone.
pub trait ListenerProber: Send + Sync {
    /// `Ok(())` when the address could be claimed and released again.
    fn probe(&self, addr: &str) -> Result<(), String>;
}

/// The production prober: bind the address, then let it go.
///
/// There is a small window between this probe and the real bind; a port taken
/// in that window is caught by the listener rollback in
/// [`Desktop::migrate_listener`].
pub struct TcpListenerProber;

impl ListenerProber for TcpListenerProber {
    fn probe(&self, addr: &str) -> Result<(), String> {
        TcpListener::bind(addr)
            .map(drop)
            .map_err(|e| format!("cannot bind {addr}: {e}"))
    }
}

mod accounts;
mod balances;
mod budget;
mod helpers;
mod lifecycle;
mod migration;
#[cfg(test)]
mod migration_tests;
// `any`, not `ml`: `ml.rs` is the learned model, but it also holds
// `reconcile_accounts`, which is gated on `account` alone. Gating the module
// on `ml` took the reconciler with it, so every combination carrying `newapi`
// without `account` failed to build --features newapi is legal on the command
// line and the feature's own manifest comment describes it.
#[cfg(any(feature = "ml", feature = "account"))]
mod ml;
mod server;
mod snapshot;
mod takeover;
mod views;

#[cfg(test)]
mod tests;

pub use helpers::{needs_rebind, server_addr};
// The gates below mirror the items in `views`. A flat re-export would not
// compile under a feature combination the items themselves support.
#[cfg(feature = "newapi")]
pub use views::AccountBalanceView;
pub use views::{
    token_hint, AccountEventView, Activity, BalanceStatus, BudgetStatus, CheckinScheduleView,
    ServerStatus, Snapshot, WindowRules,
};
#[cfg(feature = "account-maint")]
pub use views::{CheckinView, ACCOUNT_EVENT_SNAPSHOT_LIMIT};
#[cfg(feature = "ml")]
pub use views::{MlTraceClear, MlTraceInfo};

// Seven of the nine children carry methods of Desktop. Rust merges split inherent
// impl blocks, so this is legal rather than a workaround, but it does make the
// table above the only map from a method name to a file.
