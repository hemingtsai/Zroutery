//! Desktop/headless composition: plugin broker + existing Core/keyring services.
//! No dynamic code executes in the webview or in Core.
#[cfg(feature = "plugin")]
pub(crate) mod bindings;
pub mod commands;
pub mod credentials;
pub(crate) mod options;
#[cfg(feature = "plugin")]
pub(crate) mod scoped_secrets;
pub mod views;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use zroutery_plugin_api::{PluginError, Result};
use zroutery_plugin_host::supervisor::Supervisor;
use zroutery_plugin_host::{install::Package, lifecycle::Manager};

/// Action intents are single-use, short-lived and bounded — the same budget
/// the host `Actions` table applies. Stale tokens cannot accumulate across
/// renders, and a render flood cannot grow the table without end.
pub(crate) const ACTION_INTENT_TTL: Duration = Duration::from_secs(30);
pub(crate) const ACTION_INTENT_LIMIT: usize = 256;

#[derive(Clone)]
pub(crate) struct PendingUiAction {
    pub installation_id: String,
    pub contribution_id: String,
    pub action_id: String,
    pub revision: String,
    pub grant_epoch: String,
    pub issued_at: Instant,
}

/// Drops expired intents and reports whether room remains for a new one.
pub(crate) fn prune_action_intents(intents: &mut BTreeMap<String, PendingUiAction>) -> bool {
    intents.retain(|_, intent| intent.issued_at.elapsed() < ACTION_INTENT_TTL);
    intents.len() < ACTION_INTENT_LIMIT
}

/// Revocation drops that installation's pending intents immediately; the
/// epoch check at consume time stays as the second line of defence.
pub(crate) fn purge_action_intents(
    intents: &mut BTreeMap<String, PendingUiAction>,
    installation_id: &str,
) {
    intents.retain(|_, intent| intent.installation_id != installation_id);
}

/// Kills the worker job at the declared wall-clock deadline. A wedged worker
/// cannot hold a broker thread past its budget: the terminated job closes the
/// private pipe, the pending read fails, and the caller reports a timeout
/// instead of hanging. The terminate closure is the job handle duplicate from
/// `RestrictedProcess::watchdog`, so firing is independent of the read path.
pub(crate) struct Watchdog {
    fired: Arc<std::sync::atomic::AtomicBool>,
    stand_down: Option<mpsc::Sender<()>>,
    joined: Option<std::thread::JoinHandle<()>>,
}
impl Watchdog {
    pub(crate) fn spawn(deadline: Duration, terminate: impl FnOnce() + Send + 'static) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (signal, received) = mpsc::channel::<()>();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let joined = std::thread::spawn(move || {
            if received.recv_timeout(deadline) == Err(mpsc::RecvTimeoutError::Timeout) {
                flag.store(true, Ordering::Release);
                terminate();
            }
        });
        Self {
            fired,
            stand_down: Some(signal),
            joined: Some(joined),
        }
    }
    /// Retire the watchdog and report whether it fired. Joins the thread so a
    /// late terminate can never race the next worker launch.
    pub(crate) fn stand_down(mut self) -> bool {
        use std::sync::atomic::Ordering;
        self.stand_down.take();
        let fired = self.fired.load(Ordering::Acquire);
        if let Some(joined) = self.joined.take() {
            let _ = joined.join();
        }
        fired
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stand_down.take();
        if let Some(joined) = self.joined.take() {
            let _ = joined.join();
        }
    }
}

pub struct PluginState {
    pub manager: Result<Manager>,
    stages: Mutex<BTreeMap<String, Package>>,
    pub(crate) actions: Mutex<BTreeMap<String, PendingUiAction>>,
    pub(crate) worker_path: Mutex<Option<PathBuf>>,
    /// Global admission for worker launches: a render flood or a burst of
    /// actions cannot exceed the supervisor's worker ceiling.
    pub(crate) supervisor: Supervisor,
    #[cfg(feature = "plugin")]
    pub(crate) ownership: scoped_secrets::Ownership,
    pub safe_mode: std::sync::atomic::AtomicBool,
}
impl PluginState {
    pub fn new(root: &Path) -> Self {
        Self {
            manager: Manager::open(root.join("plugins")),
            stages: Mutex::new(BTreeMap::new()),
            actions: Mutex::new(BTreeMap::new()),
            worker_path: Mutex::new(None),
            supervisor: Supervisor::default(),
            safe_mode: std::sync::atomic::AtomicBool::new(
                std::env::args().any(|arg| arg == "--disable-plugins"),
            ),
            #[cfg(feature = "plugin")]
            ownership: scoped_secrets::Ownership::open(root.join("plugins")),
        }
    }
    pub fn stage(&self, package: Package) -> Result<String> {
        let mut stages = self.stages.lock().map_err(|_| PluginError::PluginBusy)?;
        if stages.len() >= 8 {
            return Err(PluginError::PluginBusy);
        }
        let id = uuid::Uuid::new_v4().to_string();
        stages.insert(id.clone(), package);
        Ok(id)
    }
    pub fn consume(&self, stage_id: &str, digest: &str) -> Result<Package> {
        let mut stages = self
            .stages
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        if stages.get(stage_id).is_none_or(|p| p.digest != digest) {
            return Err(PluginError::StaleRevision);
        }
        stages.remove(stage_id).ok_or(PluginError::StaleRevision)
    }
    /// Drop every pending UI intent of one installation. In-memory revocation
    /// already invalidated them by epoch; this keeps the table honest too.
    pub(crate) fn purge_actions(&self, installation_id: &str) {
        if let Ok(mut intents) = self.actions.lock() {
            purge_action_intents(&mut intents, installation_id);
        }
    }
}

pub fn bundled_themes() -> Result<Vec<Package>> {
    let bytes = [
        (
            include_bytes!("../../../plugins/theme-md3/zroutery.plugin.json").as_slice(),
            include_bytes!("../../../plugins/theme-md3/theme.json").as_slice(),
        ),
        (
            include_bytes!("../../../plugins/theme-fluent/zroutery.plugin.json").as_slice(),
            include_bytes!("../../../plugins/theme-fluent/theme.json").as_slice(),
        ),
    ];
    bytes
        .into_iter()
        .map(|(manifest, theme)| {
            Package::from_files(
                BTreeMap::from([
                    ("zroutery.plugin.json".into(), manifest.to_vec()),
                    ("theme.json".into(), theme.to_vec()),
                ]),
                true,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn intent(installation: &str, issued_at: Instant) -> PendingUiAction {
        PendingUiAction {
            installation_id: installation.into(),
            contribution_id: "ui".into(),
            action_id: "refresh".into(),
            revision: "r1".into(),
            grant_epoch: "1".into(),
            issued_at,
        }
    }

    #[test]
    fn expired_intents_are_pruned_and_the_table_is_bounded() {
        let mut intents = BTreeMap::new();
        intents.insert(
            "stale".into(),
            intent("a", Instant::now() - ACTION_INTENT_TTL),
        );
        intents.insert("fresh".into(), intent("a", Instant::now()));
        assert!(prune_action_intents(&mut intents));
        assert!(!intents.contains_key("stale"));
        assert!(intents.contains_key("fresh"));
        let mut full: BTreeMap<String, PendingUiAction> = (0..ACTION_INTENT_LIMIT)
            .map(|n| (n.to_string(), intent("a", Instant::now())))
            .collect();
        assert!(!prune_action_intents(&mut full));
    }

    #[test]
    fn purging_drops_only_the_revoked_installation() {
        let mut intents = BTreeMap::new();
        intents.insert("one".into(), intent("a", Instant::now()));
        intents.insert("two".into(), intent("b", Instant::now()));
        purge_action_intents(&mut intents, "a");
        assert!(!intents.contains_key("one"));
        assert!(intents.contains_key("two"));
    }

    #[test]
    fn the_watchdog_terminates_only_when_the_deadline_expires() {
        let terminated = Arc::new(AtomicUsize::new(0));
        let flag = Arc::clone(&terminated);
        let watchdog = Watchdog::spawn(Duration::from_millis(20), move || {
            flag.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(80));
        assert!(watchdog.stand_down());
        assert_eq!(terminated.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn standing_down_before_the_deadline_never_terminates() {
        let terminated = Arc::new(AtomicUsize::new(0));
        let flag = Arc::clone(&terminated);
        let watchdog = Watchdog::spawn(Duration::from_secs(30), move || {
            flag.fetch_add(1, Ordering::SeqCst);
        });
        assert!(!watchdog.stand_down());
        assert_eq!(terminated.load(Ordering::SeqCst), 0);
    }
}
