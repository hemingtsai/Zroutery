//! The desktop fixtures: rebinding, provider removal, the ledger, and every view the window reads.

use super::*;
use crate::store;
use zroutery_core::billing::Cost;

use std::path::{Path, PathBuf};

use zroutery_core::budget::{BudgetPeriod, BudgetScope};
use zroutery_core::config::SecretStore;
use zroutery_core::config::{AppConfig, ServerConfig};

use crate::secrets::KeychainSecrets;

/// The two account flags answer different questions and must not collapse.
///
/// Asserted on a built `Snapshot` rather than on `cfg!` compared with itself,
/// which is what the first draft of this gate did: it restated the feature
/// table and could not fail on anything but a broken feature graph.
#[tokio::test]
async fn account_availability_is_a_separate_answer_from_the_other_availability_flags() {
    let (desktop, dir) = desktop_with(AppConfig::default());
    let snapshot = desktop.snapshot().await;

    if snapshot.account_maintenance_available {
        assert!(
            snapshot.account_available,
            "account-maint is declared as depending on newapi, which depends on account; a \
             build with maintenance and no subsystem is a broken feature graph"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The snapshot the dashboard reads must carry both account flags, with the
/// values this build actually has.
///
/// Built from a real `Desktop` rather than from `cfg!` compared to itself,
/// which is what the first draft of this gate did and which could not fail.
#[tokio::test]
async fn the_snapshot_reports_this_build_s_account_availability() {
    let (desktop, dir) = desktop_with(AppConfig::default());
    let snapshot = desktop.snapshot().await;

    assert_eq!(
        snapshot.account_available,
        cfg!(feature = "account"),
        "the snapshot must report the account subsystem exactly as this build has it"
    );
    assert_eq!(
        snapshot.account_maintenance_available,
        cfg!(feature = "account-maint")
    );
    assert_eq!(
        snapshot.opencode_available,
        cfg!(feature = "opencode"),
        "the snapshot must report the OpenCode stack exactly as this build has it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The shipped app carries every capability the dashboard has a flag for.
///
/// The defect this exists to prevent is the one E-155 found: the desktop's
/// default feature list was `["ml"]`, so the account subsystem, the browser
/// check-in and the OpenCode stack were absent from the *installed* app while
/// the dashboard offered all three — and the account page told the operator
/// that this build contains no account subsystem.
///
/// The flag is the dashboard's contract and the default list is the build's
/// decision, so this checks the two against each other and against the
/// manifest, rather than against `cfg!` — which is what the first draft of
/// this gate did, and which can only fail on a broken feature graph.
///
/// Two properties, and the second is the one that bites:
///
/// 1. Every feature an availability flag names is a feature the package
///    actually declares. A flag that spells the name with an underscore where
///    the manifest has a hyphen is false forever, and the dashboard would hide
///    a capability the build has.
/// 2. Every capability the dashboard has a flag for is in the shipped default
///    list. Dropping one is a decision, and a decision belongs in this file
///    rather than in an edit nobody notices.
#[test]
fn the_shipped_app_carries_every_capability_the_dashboard_has_a_flag_for() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("the desktop manifest is readable");

    let (declared, defaults) = feature_table(&manifest);

    // The capability features, and the flag that tells the dashboard about
    // each. `newapi` is not here: it has no flag of its own, because the
    // panel adapter is reached through the account subsystem and the HTTP
    // check-in, and a flag for it would be a third answer to a question two
    // flags already answer.
    let capabilities = ["ml", "account", "account-maint", "opencode"];

    for capability in capabilities {
        assert!(
            declared.iter().any(|feature| feature == capability),
            "the desktop package does not declare the feature `{capability}`, so the \
             flag that reports it can never be true"
        );
        assert!(
            defaults.iter().any(|feature| feature == capability),
            "the shipped app does not carry the `{capability}` capability, while the \
             dashboard has a flag for it — so the shipped app would offer a control it \
             cannot serve. The default list is {defaults:?}."
        );
    }

    // And the default list names only features that exist, because a default
    // that names a feature nobody declared is a build that cannot be made.
    for feature in &defaults {
        assert!(
            declared.iter().any(|declared| declared == feature),
            "the default feature list names `{feature}`, which the package does not \
             declare"
        );
    }
}

/// The package's declared features and its default list, read from the manifest.
///
/// A small reader rather than a TOML parser, because the manifest is the
/// project's own file and the shape of the thing being read is fixed: a
/// `[features]` section, one `name = ["a", "b"]` per line.
fn feature_table(manifest: &str) -> (Vec<String>, Vec<String>) {
    let mut declared: Vec<String> = Vec::new();
    let mut defaults: Vec<String> = Vec::new();
    let mut in_features = false;

    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_features = line == "[features]";
            continue;
        }
        if !in_features || line.is_empty() {
            continue;
        }
        // A trailing comment is stripped before the line is read, because a
        // comment after the closing bracket would otherwise be read as part of
        // the last value — and a manifest line with a comment on it is
        // perfectly ordinary.
        let line = match line.split_once('#') {
            Some((before, _)) => before.trim(),
            None => line,
        };
        if line.is_empty() {
            continue;
        }
        let Some((name, values)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let values = values.trim();
        if !values.starts_with('[') {
            continue;
        }
        let list: Vec<String> = values
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|value| value.trim().trim_matches('"').to_string())
            .filter(|value| !value.is_empty())
            .collect();
        if name == "default" {
            defaults = list;
        } else {
            declared.push(name.to_string());
        }
    }

    (declared, defaults)
}

/// And the two must be able to disagree, or declaring them separately is
/// decoration.
///
/// In a build with neither, both are `false`; in one with only the subsystem,
/// they differ. What must never happen is the maintenance flag standing in for
/// the subsystem, because `account-maint` implies `account` and a dashboard
/// reading one for the other would offer a probe control the build cannot
/// serve.
#[tokio::test]
async fn maintenance_availability_never_stands_in_for_the_account_subsystem() {
    let (desktop, dir) = desktop_with(AppConfig::default());
    let snapshot = desktop.snapshot().await;

    if snapshot.account_maintenance_available {
        assert!(
            snapshot.account_available,
            "a build with account-maint must have the account subsystem it depends on"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

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
///
/// `pub(super)` so the panel-balance suite can store a credential too: it
/// needs a secret in the store to probe with, and a second store would be a
/// second set of rules about what a reference resolves to.
#[derive(Default)]
pub(super) struct FakeSecrets {
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
/// A provider declaring the given account ids, for the account-projection tests.
// `any`, not one of them: this fixture declares accounts, and the two tests
// that use it sit behind different features -- `account_balance_views` is a
// newapi surface, `reconcile_accounts` is an account one. Gating the fixture on
// either alone compiles one family and fails the other.
#[cfg(any(feature = "newapi", feature = "account"))]
fn multi_provider(account_ids: &[&str]) -> AppConfig {
    let accounts: Vec<serde_json::Value> = account_ids
        .iter()
        .map(|id| serde_json::json!({ "account_id": id }))
        .collect();
    let document = serde_json::json!({
        "providers": [{
            "id": "prov",
            "name": "prov",
            "kind": "openai_compatible",
            "base_url": "https://example.invalid",
            "accounts": accounts,
        }]
    });
    serde_json::from_value(document).expect("the fixture is a valid configuration")
}

/// A configuration built through its own deserialiser, for the schedule tests.
///
/// JSON rather than a struct literal because `ProviderConfig` names a private
/// type and cannot be built that way from outside the core crate at all.
#[cfg(feature = "account-maint")]
fn config_with_account(maintenance: zroutery_core::config::MaintenanceConfig) -> AppConfig {
    let document = serde_json::json!({
        "providers": [{
            "id": "prov",
            "name": "prov",
            "kind": "openai_compatible",
            "base_url": "https://example.invalid",
            "accounts": [{ "account_id": "a", "maintenance": maintenance }],
        }]
    });
    serde_json::from_value(document).expect("the fixture is a valid configuration")
}

#[cfg(feature = "account-maint")]
fn daily(interval: Option<u64>) -> zroutery_core::config::MaintenanceConfig {
    zroutery_core::config::MaintenanceConfig {
        checkin_enabled: true,
        checkin_interval_secs: interval,
        checkin_path: Some("/console/personal".into()),
        checkin_trigger_selector: Some("button.checkin".into()),
        ..Default::default()
    }
}

#[test]
#[cfg(feature = "account-maint")]
fn the_schedule_view_names_the_reason_nothing_will_run() {
    let (desktop, _dir) = desktop_with(config_with_account(daily(Some(3_600))));
    // Nothing has ever run, so there is no future time to name.
    let view = desktop.schedule_view("prov", "a");
    assert_eq!(view.decision, "needs_first_observation");
    assert_eq!(
        view.next_due_at, None,
        "there is no due time for an account that has never run"
    );
    assert_eq!(view.last_settled_at, None);
}

#[test]
#[cfg(feature = "account-maint")]
fn an_undeclared_account_is_unknown_rather_than_decided() {
    let (desktop, _dir) = desktop_with(config_with_account(daily(Some(3_600))));
    // Deciding against whatever declaration happens to be at hand would answer
    // for an account that does not exist, and a panel would render that answer.
    assert_eq!(desktop.schedule_view("prov", "nope").decision, "unknown");
    assert_eq!(desktop.schedule_view("nope", "a").decision, "unknown");
}

#[test]
#[cfg(feature = "account-maint")]
fn the_schedule_view_is_derived_and_never_a_cached_copy() {
    let (desktop, _dir) = desktop_with(config_with_account(daily(Some(3_600))));
    desktop
        .checkin_schedule
        .record_settled("prov", "a", chrono::Utc::now().timestamp() - 60);

    let view = desktop.schedule_view("prov", "a");
    assert_eq!(view.decision, "not_due");
    assert!(
        view.next_due_at.expect("a not-due decision names a time") > chrono::Utc::now().timestamp(),
        "the named time must be in the future, not the last settlement"
    );

    // A deadline that has already passed is due rather than a time in the past
    // being rendered as a schedule. Verified by moving the recorded settlement
    // rather than by waiting an hour.
    assert!(desktop.checkin_schedule.record_settled(
        "prov",
        "a",
        chrono::Utc::now().timestamp() - 7_200
    ));
    assert_eq!(desktop.schedule_view("prov", "a").decision, "due");
}
/// One observed event, so the projection can be exercised without a provider.
#[cfg(feature = "account-maint")]
fn observed(
    account: &str,
    amount: Option<f64>,
    effect: zroutery_core::account::resource::ResourceEffect,
) -> zroutery_core::account::resource::ObservedResourceEvent {
    use zroutery_core::account::resource::{
        ObservedResourceEvent, ResourceEventKind, ResourceEventSource,
    };
    ObservedResourceEvent {
        event_id: None,
        provider_id: "prov".into(),
        account_id: zroutery_core::account::AccountId(account.into()),
        observed_at: 1_000,
        event_kind: ResourceEventKind::SystemGrant,
        raw_type: None,
        description: "check-in".into(),
        resource_effect: effect,
        source: ResourceEventSource::AccountState,
        request_id: None,
        channel_id: None,
        model: None,
        accounting: None,
        metadata: Default::default(),
    }
    .tap_amount(amount)
}

#[cfg(feature = "account-maint")]
trait TapAmount {
    fn tap_amount(self, amount: Option<f64>) -> Self;
}

#[cfg(feature = "account-maint")]
impl TapAmount for zroutery_core::account::resource::ObservedResourceEvent {
    fn tap_amount(mut self, amount: Option<f64>) -> Self {
        if let (
            Some(value),
            zroutery_core::account::resource::ResourceEffect::Credited { amount },
        ) = (amount, &mut self.resource_effect)
        {
            amount.value = value;
        }
        self
    }
}

#[test]
#[cfg(feature = "account-maint")]
fn an_observed_event_reaches_the_snapshot_with_its_effect_and_source() {
    use zroutery_core::account::resource::{ResourceAmount, ResourceEffect};

    let (desktop, _dir) = desktop_with(AppConfig::default());
    assert!(
        desktop.account_event_views().is_empty(),
        "a fresh application has observed nothing, and must not invent an entry"
    );

    desktop.account_events.record(observed(
        "a",
        Some(25.0),
        ResourceEffect::Credited {
            amount: ResourceAmount::normalised(0.0, "USD"),
        },
    ));
    desktop
        .account_events
        .record(observed("b", None, ResourceEffect::NoChange));

    let views = desktop.account_event_views();
    assert_eq!(views.len(), 2, "both accounts reach the snapshot");
    let credited = views
        .iter()
        .find(|v| v.account_id == "a")
        .expect("the credited event");
    assert_eq!(credited.effect, "credited");
    assert_eq!(credited.amount, Some(25.0));
    assert_eq!(credited.unit.as_deref(), Some("USD"));
    assert_eq!(credited.source, "account_state");
    assert_eq!(credited.kind, "system_grant");

    // The distinction a panel most needs: "we looked and nothing moved" and
    // "we could not find out" are different sentences.
    let unchanged = views
        .iter()
        .find(|v| v.account_id == "b")
        .expect("the unchanged event");
    assert_eq!(unchanged.effect, "no_change");
    assert_eq!(
        unchanged.amount, None,
        "an event that moved nothing must not carry a figure"
    );
}

#[test]
#[cfg(feature = "account-maint")]
fn the_snapshot_carries_at_most_the_documented_number_of_events() {
    use zroutery_core::account::resource::{ResourceAmount, ResourceEffect};

    let (desktop, _dir) = desktop_with(AppConfig::default());
    for i in 0..(ACCOUNT_EVENT_SNAPSHOT_LIMIT + 10) {
        desktop.account_events.record(observed(
            "a",
            Some(i as f64),
            ResourceEffect::Credited {
                amount: ResourceAmount::normalised(0.0, "USD"),
            },
        ));
    }
    assert_eq!(
        desktop.account_event_views().len(),
        ACCOUNT_EVENT_SNAPSHOT_LIMIT,
        "a long-running application must not grow its snapshot without limit"
    );
}
#[test]
#[cfg(feature = "newapi")]
fn account_balances_appear_for_every_declared_account() {
    // The rule under test: every declared account of an enabled provider is
    // emitted, including the single-account case. The old rule — views only from
    // two accounts up — hid the only reading a single-account operator has, which
    // is the "probed but shown nowhere" report that motivated the change; whether
    // one row is noise is the rendering panel's decision, not the data layer's.
    let (one, _d1) = desktop_with(multi_provider(&["only"]));
    let views = one.account_balance_views();
    assert_eq!(
        views.len(),
        1,
        "a single account still produces its own view"
    );
    assert_eq!(views[0].account_id, "only");
    assert_eq!(
        views[0].status, "unprobed",
        "an account that was never probed says so"
    );

    let (several, _d2) = desktop_with(multi_provider(&["a", "b", "c"]));
    let views = several.account_balance_views();
    assert_eq!(views.len(), 3, "every declared account appears");
    assert!(views
        .iter()
        .all(|v| v.provider_id == "prov" && v.status == "unprobed"));
    assert!(
        views.iter().all(|v| v.remaining.is_none()),
        "an account that was never probed must not be rendered as a balance"
    );
}

#[tokio::test]
// `account`, not `newapi`, for the reason given on the caller in commands.rs:
// the method under test is gated on `account`, and `newapi` does not enable
// that in this crate even though it does in zroutery-core.
#[cfg(feature = "account")]
async fn an_account_with_no_credential_is_reported_rather_than_silently_absent() {
    // The single most likely reason an operator sees an account missing is a
    // credential that will not resolve, and a report that omitted it would leave
    // them with nothing to act on. The caller delegates to Core, which answers
    // this by putting one entry in the report for every declared, enabled
    // account — present with an outcome, never absent — so a panel can tell an
    // account that was probed and established nothing from one that was not
    // declared at all.
    let (desktop, _dir) = desktop_with(multi_provider(&["a", "b"]));
    let report = desktop.reconcile_accounts().await;
    assert_eq!(
        report.outcomes.len(),
        2,
        "both accounts are reported, not one and not none"
    );
    let mut ids: Vec<&str> = Vec::new();
    for (provider_id, account_id, _) in &report.outcomes {
        assert_eq!(
            provider_id, "prov",
            "the provider id is carried in the report, not looked up later"
        );
        ids.push(account_id.0.as_str());
    }
    ids.sort_unstable();
    assert_eq!(ids, ["a", "b"], "both account ids survive into the report");
}

// ---------------------------------------------------------------- takeover

/// The gateway target is exactly what the proxy can serve right now: the
/// bound address, the live token, and one class per tier that has an enabled
/// model — not one per declared tier, and not a default chosen on the user's
/// behalf.
#[test]
fn the_gateway_target_offers_exactly_the_tiers_that_have_enabled_models() {
    let config: AppConfig = serde_json::from_value(serde_json::json!({
        "server": {"auth_token": "gw-token"},
        "providers": [{
            "id": "p", "name": "p", "kind": "openai_compatible",
            "base_url": "https://example.invalid"
        }],
        "models": [
            {"provider_id": "p", "upstream_model": "m-fast", "tier": "fast", "enabled": true},
            {"provider_id": "p", "upstream_model": "m-std", "tier": "standard", "enabled": true},
            {"provider_id": "p", "upstream_model": "m-off", "tier": "reasoning", "enabled": false}
        ]
    }))
    .expect("the fixture is a valid configuration");
    let (desktop, _dir) = desktop_with(config);

    let target = desktop
        .gateway_target()
        .expect("a token and two tiered models make a servable target");
    assert_eq!(target.token, "gw-token");
    assert!(target.base_url.starts_with("http://127.0.0.1:"));
    assert_eq!(
        target
            .classes
            .iter()
            .map(|c| c.model.as_str())
            .collect::<Vec<_>>(),
        ["fast-class", "standard-class"],
        "the disabled reasoning model offers no class, and the order follows ModelTier::ALL"
    );
    assert!(target.default_class.is_none());

    // No token: nothing to authenticate a client with, so no target at all.
    let mut config = AppConfig::default();
    config.server.auth_token.clear();
    let (desktop, _dir) = desktop_with(config);
    assert!(desktop.gateway_target().is_none());
}

#[test]
fn the_gateway_target_publishes_only_the_effort_levels_every_member_accepts() {
    let config: AppConfig = serde_json::from_value(serde_json::json!({
        "server": {"auth_token": "gw-token"},
        "providers": [
            {"id": "p", "name": "p", "kind": "openai_compatible",
             "base_url": "https://example.invalid"},
            {"id": "off", "name": "off", "kind": "openai_compatible",
             "base_url": "https://example.invalid", "enabled": false}
        ],
        "models": [
            {"provider_id": "p", "upstream_model": "m-a", "tier": "fast",
             "effort_levels": ["low", "high"]},
            {"provider_id": "p", "upstream_model": "m-b", "tier": "fast",
             "effort_levels": ["high", "max", "ultra"]},
            {"provider_id": "off", "upstream_model": "m-c", "tier": "fast",
             "effort_levels": ["medium"]}
        ]
    }))
    .expect("the fixture is a valid configuration");
    let (desktop, _dir) = desktop_with(config);

    let target = desktop.gateway_target().expect("the target is servable");
    let fast = target
        .classes
        .iter()
        .find(|class| class.model == "fast-class")
        .expect("the fast class");
    assert_eq!(
        fast.effort_levels,
        ["high"],
        "only the level both enabled members accept; `max` is m-b's alone and would be \
         refused by m-a, and the disabled provider does not contribute"
    );
}

/// Each agent's ownership store is durable from first use: the manifest lives
/// under the application's own config directory, so a crash cannot orphan a
/// client config that still points at this proxy.
#[test]
fn takeover_stores_are_durable_under_the_config_directory() {
    let (desktop, dir) = desktop_with(AppConfig::default());
    let store = desktop
        .takeover_store(zroutery_core::agent_takeover::AgentType::Claude)
        .expect("a fresh store opens cleanly");
    assert!(store.is_durable());
    assert!(
        dir.join("takeover").join("claude.json").exists() || store.manifest().is_none(),
        "the manifest path lives under <config_dir>/takeover/"
    );
    // The same store comes back for the same agent; ownership state is not
    // re-derived per command.
    let again = desktop
        .takeover_store(zroutery_core::agent_takeover::AgentType::Claude)
        .expect("the second open reuses the first");
    assert!(Arc::ptr_eq(&store, &again));
}
