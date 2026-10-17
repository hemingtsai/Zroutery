//! Building a Desktop, and the warnings and window rules it holds.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use zroutery_core::config::AppConfig;
use zroutery_core::server::AppState;

use super::helpers::lock;
use super::views::*;
use super::Desktop;
use super::{ListenerProber, TcpListenerProber};
use crate::secrets::KeychainSecrets;
use crate::store;

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
        #[cfg(feature = "account-maint")]
        let checkin = crate::checkin::CheckinRuntime::new(config_dir.clone());
        // Read before the struct literal, which moves `config_dir` into the field.
        #[cfg(feature = "account-maint")]
        let checkin_schedule = crate::checkin::schedule::Schedule::load(
            crate::checkin::schedule::Schedule::path_in(&config_dir),
        );
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
            #[cfg(feature = "account-maint")]
            checkin,
            #[cfg(feature = "account-maint")]
            checkin_reports: Mutex::new(BTreeMap::new()),
            #[cfg(feature = "account-maint")]
            account_events: zroutery_core::account::AccountEventLog::new(
                zroutery_core::account::events::DEFAULT_EVENT_CAPACITY,
            ),
            #[cfg(feature = "account-maint")]
            checkin_schedule,
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
}
