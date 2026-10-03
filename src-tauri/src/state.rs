//! Desktop application state: owns the core proxy state, the keychain, the
//! config file location and the running server handle.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use zroutery_core::billing::{Balance, Cost};
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope};
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
            .map(|listener| drop(listener))
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

    fn config_on(port: u16, token: &str) -> AppConfig {
        let mut config = config_with_token(token);
        config.server.port = port;
        config
    }

    /// The port the configuration on disk was saved with, when a document was
    /// written at all.
    fn persisted_port(dir: &PathBuf) -> Option<u16> {
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

    /// The port the running gateway actually serves.
    async fn running_port(desktop: &Desktop) -> u16 {
        desktop
            .bound_addr()
            .await
            .expect("gateway is running")
            .port()
    }

    async fn observe(desktop: &Desktop, dir: &PathBuf) -> Observed {
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
        let old = advertised_port();
        let (desktop, dir) = desktop_with(config_on(old, "zr-occupied"));
        desktop.start().await.unwrap();

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
    }

    /// An address no listener can claim is refused the same way, with the
    /// reason attached to the state that actually resulted.
    #[tokio::test]
    async fn an_unbindable_address_is_refused_with_the_resulting_state() {
        let old = advertised_port();
        let (desktop, dir) = desktop_with_prober(
            config_on(old, "zr-invalid"),
            Box::new(PolicyProber { running: false }),
        );
        desktop.start().await.unwrap();

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
        let old = advertised_port();
        let (desktop, dir) =
            desktop_with_prober(config_on(old, "zr-rollback"), Box::new(PermissiveProber));
        desktop.start().await.unwrap();

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
    }

    /// A successful migration commits the document and moves the listener.
    #[tokio::test]
    async fn a_successful_rebind_commits_the_document_and_moves_the_listener() {
        let old = advertised_port();
        let (desktop, dir) =
            desktop_with_prober(config_on(old, "zr-move"), Box::new(PermissiveProber));
        desktop.start().await.unwrap();

        let target = advertised_port();
        let mut next = (*desktop.core.config()).clone();
        next.server.port = target;
        assert!(desktop.apply_config(next).await.unwrap());

        let seen = observe(&desktop, &dir).await;
        seen.agrees(target);
        seen.serves(target);

        desktop.stop().await;
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
