//! Desktop application state: owns the core proxy state, the keychain, the
//! config file location and the running server handle.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;
use zroutery_core::billing::{Balance, Cost};
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope};
#[cfg(test)]
use zroutery_core::config::SecretStore;
use zroutery_core::config::{AppConfig, ConfigIssue, IssueSeverity, RoutingStrategy, ServerConfig};
use zroutery_core::election::Election;
use zroutery_core::router::ModelHealth;
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::{RequestRecord, StatsSummary};

use crate::secrets::KeychainSecrets;
use crate::store;

pub struct Desktop {
    pub(crate) core: Arc<AppState>,
    pub(crate) secrets: Arc<KeychainSecrets>,
    pub(crate) config_dir: PathBuf,
    server: AsyncMutex<Option<ServerHandle>>,
    /// Serialises configuration changes and listener migration, so a save that
    /// has to rebind cannot interleave with a concurrent stop or save.
    migration: AsyncMutex<()>,
    /// Decides whether the listener can claim an address. Real in production,
    /// injectable so tests can simulate a port that cannot be bound.
    prober: Box<dyn ListenerProber + Send + Sync>,
    /// Startup problem worth surfacing once in the UI.
    warning: Mutex<Option<String>>,
    /// Last answer from each provider's balance endpoint. Never fetched on a
    /// timer: it costs a request and some vendors rate limit it.
    balances: Mutex<BTreeMap<String, BalanceStatus>>,
    /// The window layer's live view of the lifecycle settings: the close
    /// handler reads this synchronously, `apply_config` keeps it current.
    pub(crate) window_rules: Mutex<WindowRules>,
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

/// How the close button behaves, held where the window layer can read it
/// without going through the whole config document.
#[derive(Debug, Clone, Default)]
pub struct WindowRules {
    pub keep_in_tray: bool,
}

/// What the last balance check found, per provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceStatus {
    pub checked_at: DateTime<Utc>,
    pub balance: Option<Balance>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub address: Option<String>,
    pub base_url: Option<String>,
    pub host: String,
    pub port: u16,
    pub require_auth: bool,
    /// Enough of the token to recognise which one is in play, never the whole
    /// thing: snapshots are handed to the webview on every poll. Use the
    /// `reveal_token` or `copy_token` commands for the real value.
    pub token_hint: String,
    pub exposed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub config: AppConfig,
    /// The exposed id of every entry in `config.models`, in the same order.
    ///
    /// The dashboard renders these instead of deriving ids itself, so the
    /// `<provider>-<model>` rule has exactly one implementation.
    pub exposed_ids: Vec<String>,
    pub issues: Vec<ConfigIssue>,
    pub blocking: bool,
    pub server: ServerStatus,
    /// provider id -> whether an API key is stored for it.
    pub keys: std::collections::BTreeMap<String, bool>,
    pub health: Vec<ModelHealth>,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
    pub warning: Option<String>,
    pub config_path: String,
    pub version: String,
    /// provider id -> last balance check, for the providers that were asked.
    pub balances: BTreeMap<String, BalanceStatus>,
    /// The last election, when one has been held this run.
    pub election: Option<Election>,
    /// Every budget with what has been spent against it, for the dashboard.
    pub budgets: Vec<BudgetStatus>,
    /// Whether this build contains a learning stack at all.
    ///
    /// Declared here so the dashboard can ask before it calls, rather than
    /// discovering the absence from an error. A command that was never
    /// registered and a command that failed are the same event to the webview,
    /// and conflating them is how an operator ends up reading "no learning
    /// stack" out of what is actually a broken bridge.
    pub ml_available: bool,
}

/// One budget and how much of it is gone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub budget: Budget,
    pub spent: Cost,
    /// 0.0 to 1.0, and beyond: over 1.0 means the limit has been passed, which
    /// happens because a request is only charged once it has finished.
    pub used: f64,
}

/// The subset the Activity tab polls for.
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub health: Vec<ModelHealth>,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
}

/// `zr-1234abcd…` -> `zr-…abcd`: enough to tell two tokens apart, useless on its
/// own.
pub fn token_hint(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("zr-…{tail}")
}

impl Desktop {
    pub fn new(config_dir: PathBuf, config: AppConfig, secrets: Arc<KeychainSecrets>) -> Self {
        let config = Self::with_ml_state_dir(config_dir.clone(), config);
        let core = Arc::new(AppState::new(config, secrets.clone() as Arc<_>));
        // Spend is carried over from previous runs, because a budget that starts from
        // zero on every launch protects nothing.
        core.set_ledger(store::load_ledger(&config_dir));
        let window_rules = WindowRules {
            keep_in_tray: core.config().window.keep_in_tray,
        };
        Desktop {
            core,
            secrets,
            config_dir,
            server: AsyncMutex::new(None),
            migration: AsyncMutex::new(()),
            prober: Box::new(TcpListenerProber),
            warning: Mutex::new(None),
            balances: Mutex::new(BTreeMap::new()),
            window_rules: Mutex::new(window_rules),
        }
    }

    /// Point the ML durable state at the application's own configuration
    /// directory, unless the document already names one.
    ///
    /// Durable ML state is opt-in and its default is *off*, deliberately: an
    /// implicit directory meant every process on the machine shared one. The
    /// desktop app is the one place that legitimately owns a directory, so it
    /// is the right place to say so — and it says so per installation rather
    /// than per user.
    ///
    /// The document's own value always wins, so an operator who wants the
    /// history somewhere else still gets that.
    #[cfg(feature = "ml")]
    fn with_ml_state_dir(config_dir: PathBuf, mut config: AppConfig) -> AppConfig {
        if !config.ml_routing.has_state_dir() {
            config.ml_routing.state_dir = config_dir.join("ml").display().to_string();
        }
        config
    }

    #[cfg(not(feature = "ml"))]
    fn with_ml_state_dir(_config_dir: PathBuf, config: AppConfig) -> AppConfig {
        config
    }

    /// The same as [`Desktop::new`] with a chosen listener prober.
    ///
    /// Production always uses the real one; tests use it to make a port
    /// unbindable without racing another process for one.
    pub fn with_prober(
        config_dir: PathBuf,
        config: AppConfig,
        secrets: Arc<KeychainSecrets>,
        prober: Box<dyn ListenerProber + Send + Sync>,
    ) -> Self {
        let mut desktop = Desktop::new(config_dir, config, secrets);
        desktop.prober = prober;
        desktop
    }

    pub fn set_warning(&self, warning: Option<String>) {
        *lock(&self.warning) = warning;
    }

    pub fn warning(&self) -> Option<String> {
        lock(&self.warning).clone()
    }

    /// A snapshot of the window-layer rules, for the close handler.
    pub fn window_rules(&self) -> WindowRules {
        lock(&self.window_rules).clone()
    }

    pub async fn snapshot(&self) -> Snapshot {
        let stored = self.core.config();
        let issues = stored.validate();
        let blocking = issues.iter().any(|i| i.severity == IssueSeverity::Error);
        let running_addr = self
            .server
            .lock()
            .await
            .as_ref()
            .map(|s| s.addr.to_string());

        let keys = stored
            .providers
            .iter()
            .map(|p| (p.id.clone(), self.secrets.has(&p.key_ref)))
            .collect();

        // The webview gets everything except the token itself.
        let mut config = (*stored).clone();
        config.server.auth_token = String::new();

        Snapshot {
            server: ServerStatus {
                running: running_addr.is_some(),
                base_url: running_addr.as_ref().map(|a| format!("http://{a}")),
                address: running_addr,
                host: stored.server.host.clone(),
                port: stored.server.port,
                require_auth: stored.server.require_auth,
                token_hint: token_hint(&stored.server.auth_token),
                exposed: stored.server.is_exposed(),
            },
            keys,
            issues,
            blocking,
            health: self.core.router().health_snapshot(),
            summary: self.core.stats().summary(),
            recent: self.core.stats().recent(200),
            warning: self.warning(),
            config_path: self.config_dir.join(store::FILE_NAME).display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            exposed_ids: config.exposed_ids(),
            balances: self.balances(),
            election: self.core.router().election(),
            budgets: self.budget_status(),
            ml_available: cfg!(feature = "ml"),
            config,
        }
    }

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

    /// Only the counters and the log, for the Activity tab's polling.
    ///
    /// A full snapshot clones the whole configuration and asks the keychain about
    /// every provider; this is what the dashboard actually needs twice a second.
    /// The learned model's status, read from the live serving components.
    ///
    /// Delegates to Core rather than re-deriving anything here, so the dashboard
    /// and the HTTP endpoint cannot disagree about what is serving.
    #[cfg(feature = "ml")]
    pub fn ml_status(&self) -> zroutery_core::MlStatus {
        self.core.ml_status()
    }

    /// Replay the serving model over recorded history.
    #[cfg(feature = "ml")]
    pub fn ml_shadow_analysis(&self, limit: usize) -> zroutery_core::ShadowAnalysisStatus {
        self.core.ml_shadow_analysis(limit)
    }

    /// Withdraw the current model and return to the previously promoted one.
    #[cfg(feature = "ml")]
    pub fn rollback_active_model(&self) -> zroutery_core::ReloadOutcome {
        self.core.rollback_active_model()
    }

    pub fn activity(&self) -> Activity {
        Activity {
            health: self.core.router().health_snapshot(),
            summary: self.core.stats().summary(),
            recent: self.core.stats().recent(200),
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
    /// Called on a timer and at shutdown rather than per request: six numbers change
    /// on every call, and rewriting the file each time buys nothing. The cost of that
    /// choice is losing the last few seconds of spend to a hard kill, which is a fair
    /// trade against constant disk writes.
    ///
    /// A write that fails leaves the spend pending, so the next timer tick (or the
    /// shutdown flush) retries it once the disk is writable again, instead of the
    /// old ledger surviving until the process restarts.
    pub fn flush_ledger(&self) {
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

    pub async fn is_running(&self) -> bool {
        self.server.lock().await.is_some()
    }

    /// The address the listener actually bound, when one is running. A
    /// configuration of port 0 resolves to a real port here.
    pub async fn bound_addr(&self) -> Option<std::net::SocketAddr> {
        self.server.lock().await.as_ref().map(|s| s.addr)
    }

    pub async fn start(&self) -> Result<(), String> {
        let mut guard = self.server.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let handle = ServerHandle::start(Arc::clone(&self.core))
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!("proxy listening on http://{}", handle.addr);
        *guard = Some(handle);
        Ok(())
    }

    pub async fn stop(&self) {
        self.stop_listener().await;
        // Whatever was spent while it ran must outlive the process.
        self.flush_ledger();
    }

    /// Drop the running listener without touching the ledger.
    ///
    /// The migration uses this so the listener handover and the ledger flush
    /// stay separate steps.
    async fn stop_listener(&self) {
        let handle = self.server.lock().await.take();
        if let Some(h) = handle {
            h.stop().await;
            tracing::info!("proxy stopped");
        }
    }

    pub async fn restart(&self) -> Result<(), String> {
        self.stop().await;
        self.start().await
    }

    /// Remove a provider, its models and the credential only it used.
    ///
    /// The real `key_ref` is captured before the configuration changes, so a
    /// custom reference is cleared instead of a guessed `provider:{id}`. A
    /// reference another provider still holds is left alone, and a delete that
    /// failed comes back as a warning on the next snapshot rather than a
    /// silent success.
    ///
    /// Returns the warning when the credential could not be removed.
    pub async fn remove_provider(&self, provider_id: &str) -> Result<Option<String>, String> {
        let _migration = self.migration.lock().await;
        let previous = self.core.config();
        let provider = previous
            .provider(provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let removed_key_ref = provider.key_ref.clone();

        let mut next = (*previous).clone();
        next.providers.retain(|p| p.id != provider_id);
        next.models.retain(|m| m.provider_id != provider_id);

        let orphaned = if removed_key_ref.trim().is_empty() {
            // No credential to look after.
            None
        } else if next.providers.iter().any(|p| p.key_ref == removed_key_ref) {
            // A reference another provider still holds is not ours to delete:
            // it may be deliberate sharing, and removing it would break the
            // provider that stays.
            tracing::info!(
                "provider `{provider_id}` removed; `{removed_key_ref}` is still used by another provider"
            );
            None
        } else {
            match self.secrets.delete(&removed_key_ref) {
                Ok(()) => None,
                // The provider is gone either way; the key that could not be
                // removed is reported so the user can clear it by hand.
                Err(e) => {
                    tracing::warn!(
                        "provider `{provider_id}` removed, but its stored key could not be removed: {e}"
                    );
                    Some(format!(
                        "provider `{provider_id}` was removed, but its stored key `{removed_key_ref}` could not be removed ({e}); remove it in the credential manager"
                    ))
                }
            }
        };

        store::save(&self.config_dir, &next)?;
        *lock(&self.window_rules) = WindowRules {
            keep_in_tray: next.window.keep_in_tray,
        };
        self.core.set_config(next);
        if orphaned.is_some() {
            self.set_warning(orphaned.clone());
        }
        Ok(orphaned)
    }

    /// Persist and hot swap a new configuration.
    ///
    /// Returns `true` when the listener had to be rebound, which only happens
    /// for host, port or CORS changes.
    ///
    /// The configuration change and the listener migration are one step. When
    /// the address changes the target has to be bindable *before* the running
    /// gateway is touched, so a save that fails leaves the old configuration —
    /// on disk as well as in memory — and the old listener in place. If the
    /// new listener cannot be started after the old one stopped, the previous
    /// configuration and listener are restored, so what the caller is told and
    /// what the app is doing cannot disagree.
    pub async fn apply_config(&self, mut next: AppConfig) -> Result<bool, String> {
        // Serialise with other saves and with any listener stop.
        let _migration = self.migration.lock().await;

        // Tidy the alias lists and fold any legacy id the dashboard echoed back.
        next.normalize();
        let previous = self.core.config();
        // The dashboard never receives the token, so an empty one means "keep
        // what you have" rather than "clear it".
        if next.server.auth_token.trim().is_empty() {
            next.server.auth_token = previous.server.auth_token.clone();
        }
        // Warn when the server is about to become exposed to the network.
        if !previous.server.is_exposed() && next.server.is_exposed() {
            tracing::warn!(
                "server host is changing from {} to {} — the proxy will be \
                 reachable from the network",
                previous.server.host,
                next.server.host,
            );
        }
        let issues = next.validate();
        if let Some(err) = issues.iter().find(|i| i.severity == IssueSeverity::Error) {
            return Err(err.message.clone());
        }
        let needs_rebind = needs_rebind(&previous.server, &next.server);

        // Refuse an address the gateway could not claim while nothing has been
        // written yet: a running listener keeps serving the saved settings and
        // a stopped one stays stopped. Port 0 is excluded: the OS picks a free
        // port, so there is nothing to conflict with and probing the resolved
        // port would be a false clash.
        let running = self.is_running().await;
        if needs_rebind && next.server.port != 0 {
            self.prober
                .probe(&server_addr(&next.server))
                .map_err(|e| self.refused_rebind_error(&previous.server, running, &e))?;
        }

        store::save(&self.config_dir, &next)?;
        // Rebuild the upstream HTTP client if proxy bypass setting changed.
        if previous.server.bypass_proxy != next.server.bypass_proxy {
            self.core.rebuild_upstream(next.server.bypass_proxy);
        }
        // The close button reads the rules synchronously, so the window layer
        // is told directly rather than discovering the change later.
        *lock(&self.window_rules) = WindowRules {
            keep_in_tray: next.window.keep_in_tray,
        };
        self.core.set_config(next.clone());

        if let Err(e) = self.migrate_listener(&previous, &next, needs_rebind).await {
            // The change did not take: put the document back the way it was, so
            // a dashboard that re-reads the state sees the truth.
            if let Err(restore) = store::save(&self.config_dir, &previous) {
                tracing::error!("cannot restore the previous configuration: {restore}");
            }
            self.core.set_config((*previous).clone());
            *lock(&self.window_rules) = WindowRules {
                keep_in_tray: previous.window.keep_in_tray,
            };
            let note = format!(
                "the change to {} was refused and the previous configuration and gateway are still in place ({e})",
                server_addr(&next.server),
            );
            self.set_warning(Some(note.clone()));
            return Err(note);
        }

        self.set_warning(None);
        Ok(needs_rebind)
    }

    /// What to say when the target address could not be claimed at all.
    ///
    /// The caller is told what actually happened: nothing was saved and the old
    /// listener is untouched. That is the state the dashboard has to re-read.
    fn refused_rebind_error(&self, previous: &ServerConfig, running: bool, cause: &str) -> String {
        let note = if running {
            format!(
                "cannot use the new address: {cause}; the gateway is still listening on {} and the configuration was not changed",
                server_addr(previous),
            )
        } else {
            format!("cannot use the new address: {cause}; the configuration was not changed",)
        };
        self.set_warning(Some(note.clone()));
        note
    }

    /// Move the running listener onto `next`, or stop it when the change only
    /// needs a new port that nothing is serving.
    async fn migrate_listener(
        &self,
        previous: &AppConfig,
        next: &AppConfig,
        needs_rebind: bool,
    ) -> Result<(), String> {
        if !needs_rebind {
            return Ok(());
        }
        if self.is_running().await {
            // Remember the address that was actually served, not the one in the
            // document: a configuration of port 0 has a real port here.
            let served = self.bound_addr().await;
            self.stop_listener().await;
            if let Err(e) = self.start().await {
                // The old listener is gone and the new one would not come up:
                // restore the previous listener so the user is left with a
                // working gateway rather than none.
                self.core.set_config((*previous).clone());
                let mut restored = self.start().await;
                if restored.is_err() {
                    if let Some(addr) = served {
                        // The document said port 0, so bring the listener back
                        // on the port it was really serving.
                        let mut pinned = (*previous).clone();
                        pinned.server.port = addr.port();
                        self.core.set_config(pinned);
                        restored = self.start().await;
                    }
                }
                return match restored {
                    Ok(()) => Err(format!(
                        "cannot listen on {}: {e}; the previous gateway was restored",
                        server_addr(&next.server),
                    )),
                    Err(restore) => Err(format!(
                        "cannot listen on {}: {e}; the previous gateway could not be restored either: {restore}",
                        server_addr(&next.server),
                    )),
                };
            }
        }
        Ok(())
    }
}

/// Recovers a poisoned lock instead of taking the app down with it; the worst
/// case is a stale balance or warning.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("recovered from poisoned mutex");
        poisoned.into_inner()
    })
}

/// The address a server configuration would listen on.
pub fn server_addr(server: &ServerConfig) -> String {
    format!("{}:{}", server.host, server.port)
}

/// Only these settings require tearing the listener down.
pub fn needs_rebind(a: &ServerConfig, b: &ServerConfig) -> bool {
    a.host != b.host
        || a.port != b.port
        || a.allow_cors != b.allow_cors
        // CORS origins and the body limit are baked into the router at bind
        // time; changing them needs a listener rebuild to take effect.
        || a.cors_origins != b.cors_origins
        || a.max_body_mib != b.max_body_mib
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebind_only_on_transport_changes() {
        let a = ServerConfig::default();
        let mut b = a.clone();
        b.auth_token = "different".into();
        b.require_auth = false;
        b.log_limit = 10;
        assert!(!needs_rebind(&a, &b));

        b.port = 9999;
        assert!(needs_rebind(&a, &b));

        let mut c = a.clone();
        c.allow_cors = true;
        assert!(needs_rebind(&a, &c));

        let mut d = a.clone();
        d.host = "0.0.0.0".into();
        assert!(needs_rebind(&a, &d));

        // Origins and body limit are baked into the router: they need a rebind.
        let mut e = a.clone();
        e.allow_cors = true;
        e.cors_origins = vec!["http://localhost:3000".into()];
        assert!(needs_rebind(&a, &e));

        let mut f = a.clone();
        f.max_body_mib += 1;
        assert!(needs_rebind(&a, &f));
    }

    fn desktop_with(config: AppConfig) -> (Arc<Desktop>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("zroutery-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let secrets = Arc::new(KeychainSecrets::new(format!(
            "app.zroutery.test.{}",
            uuid::Uuid::new_v4()
        )));
        (Arc::new(Desktop::new(dir.clone(), config, secrets)), dir)
    }

    /// A desktop whose listener prober is driven by the test instead of by
    /// whether a real port happens to be free.
    fn desktop_with_prober(
        config: AppConfig,
        prober: Box<dyn ListenerProber + Send + Sync>,
    ) -> (Arc<Desktop>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("zroutery-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let secrets = Arc::new(KeychainSecrets::new(format!(
            "app.zroutery.test.{}",
            uuid::Uuid::new_v4()
        )));
        (
            Arc::new(Desktop::with_prober(dir.clone(), config, secrets, prober)),
            dir,
        )
    }

    /// The desktop app is the one place that legitimately owns a directory, so
    /// it is the one place that says where durable ML state lives.
    ///
    /// Previously nothing set `ml_routing.state_dir` in the product, which meant
    /// the desktop app had a trace log, a model store and a promotion gate that
    /// could never be used. A state directory alone changes nothing an operator
    /// can observe — `ml_routing.enabled` still defaults to false and no model is
    /// promoted — but it is what makes history survivable, and without it the
    /// whole loop stops at the request.
    #[cfg(feature = "ml")]
    #[test]
    fn the_desktop_app_points_durable_ml_state_at_its_own_directory() {
        let (desktop, dir) = desktop_with(AppConfig::default());
        let configured = &desktop.core.config().ml_routing.state_dir;
        assert_eq!(
            configured,
            &dir.join("ml").display().to_string(),
            "the desktop app did not claim a state directory under its own config dir"
        );
        assert!(desktop.core.config().ml_routing.has_state_dir());
        // And the switch that actually lets a model rank is still off, so
        // claiming the directory changed nothing about how this install routes.
        assert!(!desktop.core.config().ml_routing.enabled);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An operator who names a directory gets that directory.
    ///
    /// The default is for the app's own use; it is not a policy that overrides
    /// what was asked for. Getting this backwards would silently relocate
    /// someone's routing history.
    #[cfg(feature = "ml")]
    #[test]
    fn a_configured_state_directory_is_never_overwritten() {
        let (desktop, dir) = desktop_with({
            let mut config = AppConfig::default();
            config.ml_routing.state_dir = std::env::temp_dir()
                .join("zroutery-explicit-ml")
                .display()
                .to_string();
            config
        });
        assert_eq!(
            desktop.core.config().ml_routing.state_dir,
            std::env::temp_dir()
                .join("zroutery-explicit-ml")
                .display()
                .to_string()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The dashboard is told whether this build has a learning stack, rather
    /// than finding out by calling a command that is not there.
    #[tokio::test]
    async fn the_snapshot_declares_whether_the_build_has_a_learning_stack() {
        let (desktop, dir) = desktop_with(AppConfig::default());
        assert_eq!(
            desktop.snapshot().await.ml_available,
            cfg!(feature = "ml"),
            "the snapshot's capability flag does not match what was compiled in"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A prober that answers from a policy instead of touching a socket.
    struct PolicyProber {
        running: bool,
    }

    impl ListenerProber for PolicyProber {
        fn probe(&self, addr: &str) -> Result<(), String> {
            if self.running {
                Ok(())
            } else {
                Err(format!("cannot bind {addr}: simulated"))
            }
        }
    }

    /// A prober that always succeeds. It is used where the test needs the
    /// migration to reach the real `ServerHandle::start` call, whose own bind
    /// then decides the outcome.
    struct PermissiveProber;

    impl ListenerProber for PermissiveProber {
        fn probe(&self, _addr: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    /// A port that is free now, for a configuration that names one explicitly.
    ///
    /// A fixed port is what makes the rebind assertions exact: the committed
    /// configuration has to match the port the listener really serves, and a
    /// configuration of port 0 cannot say that.
    fn advertised_port() -> u16 {
        for _ in 0..64 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            if port >= 30_000 {
                return port;
            }
        }
        panic!("no ephemeral port above 30000 was offered");
    }

    /// Whether an error is the gateway losing a race for a port, rather than the
    /// behaviour a test is about.
    ///
    /// `ServerHandle::start` is the only place that binds, and it reports
    /// `cannot bind {addr}: {os error}`. That string reaches both callers verbatim:
    /// `Desktop::start` returns it directly, and `migrate_listener` embeds it in
    /// the note about the previous gateway being restored. So this matches one
    /// string from one place rather than guessing at several.
    fn lost_a_port_race(error: &str) -> bool {
        error.contains("cannot bind")
    }

    /// Run `body`, retrying while it fails because a probed port was taken first.
    ///
    /// `advertised_port` binds `:0`, reads the number and drops the socket, so the
    /// port genuinely is free when probed and may be gone by the time the gateway
    /// binds it. Four tests here start on an explicitly probed port and they run
    /// concurrently, so the window is taken often enough to present as an
    /// intermittent failure rather than as a broken test.
    ///
    /// Retrying rather than sleeping, and retrying rather than using a fixed port:
    /// a fixed port collides with anything else on the machine and cannot be
    /// reasoned about. Configuring port 0 would remove the race too, but these
    /// tests exist to check that the committed document names a port the listener
    /// really serves, and a document saying 0 does not check that.
    ///
    /// `body` returns `Err` only for a bind the test did *not* intend to fail. The
    /// failures these tests are about are asserted inside the body, so they never
    /// reach the retry.
    async fn with_ports_that_survive<F, Fut>(what: &str, mut body: F) -> Result<(), String>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        const TRIES: u32 = 8;
        let mut last = String::new();
        for attempt in 1..=TRIES {
            match body().await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let raced = lost_a_port_race(&error);
                    last = error;
                    if attempt == TRIES || !raced {
                        return Err(format!("{what}: {last}"));
                    }
                    tracing::debug!("{what}: lost a race for a port ({last}); retry {attempt}");
                }
            }
        }
        Err(format!("{what}: {last}"))
    }

    fn config_on(port: u16, token: &str) -> AppConfig {
        let mut config = config_with_token(token);
        config.server.port = port;
        config
    }

    /// The port the configuration on disk was saved with, when a document was
    /// written at all.
    fn persisted_port(dir: &Path) -> Option<u16> {
        if !dir.join(store::FILE_NAME).exists() {
            return None;
        }
        let (config, warning) = store::load(dir);
        assert!(
            warning.is_none(),
            "the saved configuration must load cleanly: {warning:?}"
        );
        Some(config.server.port)
    }

    /// Everything the dashboard can see, read the way it reads it.
    struct Observed {
        persisted: Option<u16>,
        memory: u16,
        running: bool,
        running_port: Option<u16>,
    }

    impl Observed {
        /// The saved configuration, the in-memory configuration and the
        /// running listener agree about the port.
        fn agrees(&self, expected: u16) {
            assert_eq!(
                self.persisted,
                Some(expected),
                "on-disk config disagrees with the in-memory one"
            );
            assert_eq!(self.memory, expected, "in-memory config");
            assert_eq!(self.running, self.running_port.is_some(), "running flag");
        }

        /// The listener is serving exactly the configured port on the loopback
        /// interface, so it is the gateway this configuration describes.
        fn serves(&self, expected: u16) {
            assert_eq!(
                self.running_port,
                Some(expected),
                "the running listener is on the committed port"
            );
        }
    }

    async fn observe(desktop: &Desktop, dir: &Path) -> Observed {
        Observed {
            persisted: persisted_port(dir),
            memory: desktop.core.config().server.port,
            running: desktop.is_running().await,
            running_port: desktop.bound_addr().await.map(|a| a.port()),
        }
    }

    /// A port another process already holds is refused before anything moves:
    /// the running listener and the saved configuration keep the old address.
    #[tokio::test]
    async fn an_occupied_target_port_leaves_the_config_and_listener_untouched() {
        with_ports_that_survive("an_occupied_target_port", || async {
            let old = advertised_port();
            let (desktop, dir) = desktop_with(config_on(old, "zr-occupied"));
            desktop.start().await?;

            let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let busy = squatter.local_addr().unwrap().port();

            let mut next = (*desktop.core.config()).clone();
            next.server.port = busy;
            let err = desktop.apply_config(next).await.unwrap_err();
            assert!(err.contains("cannot bind"), "{err}");

            let seen = observe(&desktop, &dir).await;
            assert!(seen.running);
            // A refused save writes nothing: the in-memory configuration is the
            // one the process started with, and the dashboard re-reads that.
            assert_eq!(seen.persisted, None, "a refused save must not write");
            assert_eq!(seen.memory, old);
            seen.serves(old);
            assert_eq!(desktop.snapshot().await.config.server.port, old);

            drop(squatter);
            desktop.stop().await;
            std::fs::remove_dir_all(dir).ok();
            Ok(())
        })
        .await
        .expect("the gateway must come up on a port nobody took first");
    }

    /// An address no listener can claim is refused the same way, with the
    /// reason attached to the state that actually resulted.
    #[tokio::test]
    async fn an_unbindable_address_is_refused_with_the_resulting_state() {
        with_ports_that_survive("an_unbindable_address", || async {
            let old = advertised_port();
            let (desktop, dir) = desktop_with_prober(
                config_on(old, "zr-invalid"),
                Box::new(PolicyProber { running: false }),
            );
            desktop.start().await?;

            // A host name that cannot resolve is not bindable, exactly like an
            // occupied port: the address itself is the problem.
            let mut next = (*desktop.core.config()).clone();
            next.server.host = "host.invalid".into();
            let err = desktop.apply_config(next).await.unwrap_err();
            assert!(err.contains("cannot bind"), "{err}");

            let warning = desktop.warning().unwrap();
            assert!(warning.contains("was not changed"), "{warning}");

            let seen = observe(&desktop, &dir).await;
            assert!(seen.running);
            assert_eq!(seen.memory, old, "the in-memory configuration must be kept");
            seen.serves(old);

            desktop.stop().await;
            std::fs::remove_dir_all(dir).ok();
            Ok(())
        })
        .await
        .expect("the gateway must come up on a port nobody took first");
    }

    /// With the gateway stopped nothing is listening, so a configuration that
    /// cannot be bound is refused without saving it.
    #[tokio::test]
    async fn a_refused_address_is_not_persisted_while_stopped() {
        let (desktop, dir) = desktop_with_prober(
            config_with_token("zr-stopped"),
            Box::new(PolicyProber { running: false }),
        );
        let mut next = (*desktop.core.config()).clone();
        next.server.host = "host.invalid".into();
        next.server.port = advertised_port();
        desktop.apply_config(next).await.unwrap_err();

        assert!(!desktop.is_running().await);
        let seen = observe(&desktop, &dir).await;
        assert_eq!(seen.persisted, None, "nothing may be written for a refusal");
        assert_eq!(seen.memory, 0, "the in-memory configuration must be kept");
        assert!(!seen.running);

        std::fs::remove_dir_all(dir).ok();
    }

    /// The retry the other tests here depend on, proven rather than assumed.
    ///
    /// Without this, "no more intermittent failures" would be an observation over a
    /// handful of runs and the helper could be doing nothing at all. Three cases:
    /// it retries a lost race, it stops retrying once the port holds, and it does
    /// not retry a failure that is not a port race — which matters because two of
    /// the tests above assert on a bind failure that is the *point*, and a helper
    /// that looped on those would hang rather than fail.
    #[tokio::test]
    async fn the_port_race_retry_retries_only_port_races() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A port lost twice, then held: two retries, then success.
        let attempts = AtomicUsize::new(0);
        with_ports_that_survive("recovers", || async {
            if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                Err("cannot bind 127.0.0.1:51234: address already in use".to_string())
            } else {
                Ok(())
            }
        })
        .await
        .expect("a port race is recoverable");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "the body should run once per attempt: twice lost, once held"
        );

        // A failure that is not a port race is reported immediately, not retried.
        let unrelated = AtomicUsize::new(0);
        let error = with_ports_that_survive("unrelated", || async {
            unrelated.fetch_add(1, Ordering::SeqCst);
            Err::<(), String>("the change was refused and the gateway is still up".to_string())
        })
        .await
        .expect_err("an unrelated failure must not be swallowed");
        assert_eq!(
            unrelated.load(Ordering::SeqCst),
            1,
            "a refusal that is the behaviour under test must not be retried"
        );
        assert!(
            error.contains("was refused"),
            "the caller's failure should survive verbatim: {error}"
        );
    }

    /// A prober that reports the probe through a channel, so a test can act
    /// while the migration lock is held.
    struct SignallingProber {
        probed: tokio::sync::mpsc::UnboundedSender<()>,
    }

    impl ListenerProber for SignallingProber {
        fn probe(&self, _addr: &str) -> Result<(), String> {
            let _ = self.probed.send(());
            Ok(())
        }
    }

    /// A save and a stop racing during the migration must not leave the saved
    /// configuration, the in-memory configuration and the listener disagreeing.
    #[tokio::test]
    async fn a_save_racing_a_stop_leaves_one_consistent_state() {
        let (probed_tx, mut probed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (desktop, dir) = desktop_with_prober(
            config_with_token("zr-race"),
            Box::new(SignallingProber { probed: probed_tx }),
        );
        desktop.start().await.unwrap();

        let target = free_port();
        let mut next = (*desktop.core.config()).clone();
        next.server.port = target;

        let saving = tokio::spawn({
            let desktop = Arc::clone(&desktop);
            async move { desktop.apply_config(next).await }
        });
        // The probe runs under the migration lock, so the stop queued now has to
        // wait for the whole migration instead of interleaving with it.
        probed_rx.recv().await.expect("the probe was reached");
        let stopping = tokio::spawn({
            let desktop = Arc::clone(&desktop);
            async move { desktop.stop().await }
        });

        let saved = saving.await.unwrap();
        stopping.await.unwrap();
        // The saved document, the in-memory configuration and the listener
        // still agree, whatever order the two operations ended up in.
        let seen = observe(&desktop, &dir).await;
        let committed = seen.persisted.expect("a migration always saves a document");
        seen.agrees(committed);
        if saved.is_ok() {
            assert_eq!(committed, target, "a successful save must be on disk");
        } else {
            assert_ne!(committed, target, "a refused save must not be on disk");
        }

        std::fs::remove_dir_all(dir).ok();
    }

    /// A new listener that cannot come up — somebody took the port inside the
    /// probe window — restores the previous configuration and listener.
    #[tokio::test]
    async fn a_failed_listener_start_restores_the_previous_configuration() {
        with_ports_that_survive("a_failed_listener_start", || async {
            let old = advertised_port();
            let (desktop, dir) =
                desktop_with_prober(config_on(old, "zr-rollback"), Box::new(PermissiveProber));
            desktop.start().await?;

            let target = advertised_port();
            let mut next = (*desktop.core.config()).clone();
            next.server.port = target;
            // The probe is permissive, so the real bind is what fails: the squatter
            // plays the process that took the port first.
            let squatter = std::net::TcpListener::bind(("127.0.0.1", target)).unwrap();
            let err = desktop.apply_config(next).await.unwrap_err();
            drop(squatter);
            assert!(
                err.contains("previous gateway was restored"),
                "the message must say the old listener came back: {err}"
            );

            let seen = observe(&desktop, &dir).await;
            assert!(seen.running, "the previous gateway must be running again");
            seen.agrees(old);
            seen.serves(old);

            desktop.stop().await;
            std::fs::remove_dir_all(dir).ok();
            Ok(())
        })
        .await
        .expect("the gateway must come up on a port nobody took first");
    }

    /// A successful migration commits the document and moves the listener.
    #[tokio::test]
    async fn a_successful_rebind_commits_the_document_and_moves_the_listener() {
        with_ports_that_survive("a_successful_rebind", || async {
            let old = advertised_port();
            let (desktop, dir) =
                desktop_with_prober(config_on(old, "zr-move"), Box::new(PermissiveProber));
            desktop.start().await?;

            let target = advertised_port();
            let mut next = (*desktop.core.config()).clone();
            next.server.port = target;
            // Both ports are probed, so this one can be lost too. `apply_config`
            // puts the document back before returning, so a retry starts clean.
            assert!(
                desktop.apply_config(next).await?,
                "a changed port must be reported as a rebind"
            );

            let seen = observe(&desktop, &dir).await;
            seen.agrees(target);
            seen.serves(target);

            desktop.stop().await;
            std::fs::remove_dir_all(dir).ok();
            Ok(())
        })
        .await
        .expect("both the original and the target port must be bindable");
    }

    /// A credential store that keeps secrets in memory and can be told to
    /// refuse a deletion, so provider removal is exercised without the real
    /// keychain.
    #[derive(Default)]
    struct FakeSecrets {
        entries: Mutex<std::collections::HashMap<String, String>>,
        refuse_delete: bool,
    }

    impl crate::secrets::CredentialBackend for FakeSecrets {
        fn get(&self, key_ref: &str) -> Result<String, crate::secrets::StoreError> {
            self.entries
                .lock()
                .unwrap()
                .get(key_ref)
                .cloned()
                .ok_or(crate::secrets::StoreError::NoEntry)
        }

        fn set(&self, key_ref: &str, secret: &str) -> Result<(), crate::secrets::StoreError> {
            self.entries
                .lock()
                .unwrap()
                .insert(key_ref.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, key_ref: &str) -> Result<(), crate::secrets::StoreError> {
            if self.refuse_delete {
                return Err(crate::secrets::StoreError::Backend(
                    "access denied by the user".into(),
                ));
            }
            self.entries.lock().unwrap().remove(key_ref);
            Ok(())
        }
    }

    /// A provider using a reference a user typed by hand.
    fn custom_provider(id: &str, key_ref: &str) -> zroutery_core::config::ProviderConfig {
        use zroutery_core::config::{ProviderConfig, ProviderKind};

        let mut provider = ProviderConfig::new(id, id, ProviderKind::Anthropic);
        provider.key_ref = key_ref.to_string();
        provider
    }

    /// A desktop whose secrets live in memory and can be inspected afterwards.
    fn desktop_with_secrets(
        config: AppConfig,
        secrets: FakeSecrets,
    ) -> (Arc<Desktop>, Arc<KeychainSecrets>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("zroutery-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(KeychainSecrets::with_backend(Box::new(secrets)));
        (
            Arc::new(Desktop::new(dir.clone(), config, Arc::clone(&store))),
            store,
            dir,
        )
    }

    /// A provider whose `key_ref` is not the name-derived default has its real
    /// credential removed, not a guessed `provider:{id}`.
    #[tokio::test]
    async fn removing_a_provider_clears_its_custom_key_reference() {
        let mut config = config_with_token("zr-remove");
        config
            .providers
            .push(custom_provider("relay", "vault:relay-prod"));

        let (desktop, secrets, dir) = desktop_with_secrets(config, FakeSecrets::default());
        secrets.set("vault:relay-prod", "sk-real").unwrap();
        assert!(secrets.has("vault:relay-prod"));

        let warning = desktop.remove_provider("relay").await.unwrap();
        assert!(warning.is_none(), "a clean removal: {warning:?}");
        assert!(
            secrets.get("vault:relay-prod").is_none(),
            "the real key reference must be the one removed"
        );
        assert!(desktop.core.config().provider("relay").is_none());
        // The document on disk agrees.
        let (persisted, warning) = store::load(&dir);
        assert!(warning.is_none());
        assert!(persisted.provider("relay").is_none());

        std::fs::remove_dir_all(dir).ok();
    }

    /// A reference another provider still holds is never deleted: the provider
    /// that stays would lose its key.
    #[tokio::test]
    async fn removing_a_provider_keeps_a_shared_credential() {
        let mut config = config_with_token("zr-shared");
        config
            .providers
            .push(custom_provider("one", "shared:team-key"));
        config
            .providers
            .push(custom_provider("two", "shared:team-key"));

        let (desktop, secrets, dir) = desktop_with_secrets(config, FakeSecrets::default());
        secrets.set("shared:team-key", "sk-shared").unwrap();

        assert!(desktop.remove_provider("one").await.unwrap().is_none());
        assert_eq!(
            secrets.get("shared:team-key").as_deref(),
            Some("sk-shared"),
            "a shared credential is not the removed provider's to delete"
        );
        assert!(desktop.core.config().provider("two").is_some());

        std::fs::remove_dir_all(dir).ok();
    }

    /// A credential store that refuses must not turn a removal into a silent
    /// orphan: the caller is told, and the provider is still gone.
    #[tokio::test]
    async fn a_refused_credential_delete_is_reported_after_the_removal() {
        let mut config = config_with_token("zr-refused");
        config
            .providers
            .push(custom_provider("relay", "vault:relay-prod"));

        let (desktop, secrets, dir) = desktop_with_secrets(
            config,
            FakeSecrets {
                entries: Mutex::new(std::collections::HashMap::from([(
                    "vault:relay-prod".to_string(),
                    "sk-kept".to_string(),
                )])),
                refuse_delete: true,
            },
        );

        let warning = desktop.remove_provider("relay").await.unwrap().unwrap();
        assert!(warning.contains("could not be removed"), "{warning}");
        assert!(warning.contains("vault:relay-prod"), "{warning}");
        assert!(desktop.core.config().provider("relay").is_none());
        // The dashboard sees the same warning on its next snapshot.
        assert_eq!(
            desktop.snapshot().await.warning.as_deref(),
            Some(warning.as_str())
        );
        assert_eq!(secrets.get("vault:relay-prod").as_deref(), Some("sk-kept"));

        std::fs::remove_dir_all(dir).ok();
    }

    /// A provider with no credential at all is removed without touching the
    /// store.
    #[tokio::test]
    async fn removing_a_provider_without_a_credential_is_clean() {
        let mut config = config_with_token("zr-nocred");
        config.providers.push(custom_provider("local", ""));
        let (desktop, _secrets, dir) = desktop_with_secrets(config, FakeSecrets::default());
        assert!(desktop.remove_provider("local").await.unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    /// Removing something that is not there is an error, not a silent success.
    #[tokio::test]
    async fn removing_an_unknown_provider_is_refused() {
        let (desktop, _secrets, dir) =
            desktop_with_secrets(config_with_token("zr-unknown"), FakeSecrets::default());
        let err = desktop.remove_provider("nope").await.unwrap_err();
        assert!(err.contains("unknown provider"), "{err}");
        std::fs::remove_dir_all(dir).ok();
    }

    fn config_with_token(token: &str) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.server.auth_token = token.into();
        cfg.server.port = 0;
        cfg
    }

    #[test]
    fn the_token_hint_keeps_only_the_tail() {
        assert_eq!(token_hint("zr-0123456789abcdef"), "zr-…cdef");
        assert_eq!(token_hint(""), "");
        // A short token still does not reveal itself entirely.
        assert_eq!(token_hint("abcd"), "zr-…abcd");
    }

    #[tokio::test]
    async fn snapshots_never_carry_the_token() {
        let (desktop, dir) = desktop_with(config_with_token("zr-secret-token-1234"));
        let snapshot = desktop.snapshot().await;

        assert_eq!(snapshot.server.token_hint, "zr-…1234");
        assert!(snapshot.config.server.auth_token.is_empty());
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(
            !json.contains("zr-secret-token-1234"),
            "the token reached the payload handed to the webview"
        );
        // The explicit accessor still has it.
        assert_eq!(desktop.auth_token(), "zr-secret-token-1234");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn saving_a_redacted_config_keeps_the_existing_token() {
        let (desktop, dir) = desktop_with(config_with_token("zr-keep-me-9999"));

        // What the dashboard sends back: everything except the token.
        let mut edited = desktop.snapshot().await.config;
        assert!(edited.server.auth_token.is_empty());
        edited.server.log_limit = 42;
        desktop.apply_config(edited).await.unwrap();

        assert_eq!(desktop.auth_token(), "zr-keep-me-9999");
        assert_eq!(desktop.core.config().server.log_limit, 42);

        // An explicit new token is still honoured.
        let mut rotated = desktop.snapshot().await.config;
        rotated.server.auth_token = "zr-brand-new".into();
        desktop.apply_config(rotated).await.unwrap();
        assert_eq!(desktop.auth_token(), "zr-brand-new");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn the_activity_view_matches_the_snapshot_counters() {
        let (desktop, dir) = desktop_with(config_with_token("zr-1"));
        let activity = desktop.activity();
        let snapshot = desktop.snapshot().await;
        assert_eq!(activity.summary.requests, snapshot.summary.requests);
        assert_eq!(activity.recent.len(), snapshot.recent.len());
        assert_eq!(activity.health.len(), snapshot.health.len());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn balances_are_only_fetched_where_they_exist() {
        use zroutery_core::billing::{BalanceConfig, BalancePreset, BalanceProbe};
        use zroutery_core::config::{ProviderConfig, ProviderKind};

        let mut config = config_with_token("zr-1");
        let mut quiet = ProviderConfig::new("quiet", "Quiet Co", ProviderKind::OpenAICompatible);
        quiet.key_ref = String::new();
        let mut probed = quiet.clone();
        probed.id = "probed".into();
        probed.name = "Probed Co".into();
        // Nothing listens here, so the fetch fails without needing a mock server.
        probed.base_url = "http://127.0.0.1:1".into();
        probed.connect_timeout_secs = 1;
        probed.timeout_secs = 2;
        probed.balance = BalanceConfig {
            preset: BalancePreset::Custom,
            custom: Some(BalanceProbe::default()),
        };
        config.providers = vec![quiet, probed];
        let (desktop, dir) = desktop_with(config);

        // A provider with no endpoint is refused up front rather than asked.
        let err = desktop.refresh_balance("quiet").await.unwrap_err();
        assert!(err.contains("does not publish a balance"), "{err}");
        assert!(desktop.balances().is_empty());
        assert!(desktop.refresh_balance("nope").await.is_err());

        // A failure is remembered so the dashboard can show why.
        assert!(desktop.refresh_balance("probed").await.is_err());
        let status = desktop.balances().get("probed").cloned().unwrap();
        assert!(status.balance.is_none());
        assert!(status.error.is_some());
        assert!(desktop.snapshot().await.balances.contains_key("probed"));

        // Refreshing everything skips the provider that cannot answer.
        let problems = desktop.refresh_all_balances().await;
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("probed:"));

        std::fs::remove_dir_all(dir).ok();
    }

    /// A failed ledger write must not lose the spend: once the destination is
    /// writable again the next flush has to persist it.
    #[test]
    fn a_failed_ledger_write_is_retried_after_the_disk_recovers() {
        use zroutery_core::budget::Ledger;
        use zroutery_core::config::ModelTier;

        let global_day = |ledger: &Ledger| {
            ledger
                .totals_for(&BudgetScope::Global, chrono::Local::now())
                .into_iter()
                .find(|(period, _)| *period == BudgetPeriod::Day)
                .map(|(_, cost)| cost.amount)
                .unwrap_or(0.0)
        };

        let (desktop, dir) = desktop_with(config_with_token("zr-ledger"));
        desktop.core.charge(
            "provider",
            Some(ModelTier::Standard),
            &Cost {
                currency: "USD".into(),
                amount: 1.09,
            },
        );

        // A directory where the temporary file goes makes the write fail the way
        // a full or read-only destination does, without needing root.
        let blocked = dir.join(format!("{}.tmp", store::LEDGER_FILE));
        std::fs::create_dir(&blocked).unwrap();

        desktop.flush_ledger();
        assert!(
            !dir.join(store::LEDGER_FILE).exists(),
            "the blocked destination cannot have produced a ledger"
        );
        assert_eq!(
            global_day(&store::load_ledger(&dir)),
            0.0,
            "nothing was written while the destination was blocked"
        );

        // The disk recovers. The pending spend must still be there to write.
        std::fs::remove_dir(&blocked).unwrap();
        desktop.flush_ledger();

        assert_eq!(
            global_day(&store::load_ledger(&dir)),
            1.09,
            "the retry after recovery must persist the spend charged before the failure"
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
