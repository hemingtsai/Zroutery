//! The scheduler: deciding which accounts are due, unattended.
//!
//! # What this is for
//!
//! [`decide`] in the core says whether one account is due. This module is the
//! thing that walks the declared accounts, asks it about each, and starts the ones
//! that come back [`MaintenanceDecision::Due`]. It runs on a timer, which is the
//! entire reason it needs its own module and its own tests: a check-in opened by
//! hand has a person watching it, and a check-in opened by a timer does not.
//!
//! # The three ways this can go wrong, and the guard for each
//!
//! **Starting a browser for an account that already has one.** A scan overlaps an
//! attempt in flight by construction: the attempt runs for up to a minute and the
//! scan runs every tick. [`CheckinRuntime::begin`] already refuses a second
//! session for a key, so this is refused rather than doubled, and the refusal is
//! counted rather than logged per account — at a one-minute tick a per-account log
//! line is a flood.
//!
//! **Restarting an account that is paused on a challenge.** This is the serious
//! one. A WAF pause holds its session *deliberately*, waiting for a human, and it
//! may wait for hours. A scheduler that treated "not terminal" as "ready to start
//! again" would open a fresh browser every tick for as long as the human took,
//! each one needing the same verification — turning one challenge into an
//! unbounded pile of windows and hammering the very endpoint that is already
//! suspicious of us. So the scan skips any account with a live session, and it
//! does not care whether that session is healthy. This is the same conclusion the
//! state machine reaches from the other direction: see `checkin::state`, which has
//! no transition out of `WaitingForUser` that starts anything.
//!
//! **Reporting success for something it did not do.** A scan that counted a
//! launch as a check-in would inflate every reward figure. It records nothing
//! itself: the settlement timestamp is written by the attempt's own observation
//! step, which is the only place that knows what actually happened.
//!
//! # Why the scan does not decide what a check-in is worth
//!
//! It cannot, and the separation is not incidental. The scan knows a check-in was
//! *due*; only the provider's structured observation establishes that a reward was
//! *granted*, and that is the same execution-versus-observation discipline the
//! check-in itself is built on. A scheduler that inferred an award from the fact
//! that it fired would fabricate money in the direction an operator most wants to
//! believe.

use std::collections::BTreeMap;
use std::sync::Arc;

use zroutery_core::account::checkin::{decide, MaintenanceDecision};

use crate::checkin::SessionKey;
use crate::state::Desktop;

/// How often the scheduler looks for accounts that are due.
///
/// A minute, and the number is a floor on how *often we ask* rather than on how
/// often anything runs: [`decide`] is a comparison against a stored timestamp, so
/// the cost of a tick is a walk over the configured accounts and nothing else. The
/// alternative — waking exactly when each account is due — would need a timer per
/// account and would make a configuration edit restart them all.
pub const SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// What one pass decided, before anything was opened.
///
/// **Splitting this from [`ScanOutcome`] is what makes the scheduler testable, and
/// the reason it was split is a defect this module caused.** Deciding and launching
/// were one function, so a test that asserted only on the decision still ran the
/// launch: `find_browser` succeeds on a developer machine, so asserting "this
/// account is due" opened a real Chrome window thirty times across a suite run and
/// left thirty profile directories behind in the temporary directory. A test that
/// cannot be run hermetically is a test whose cost is invisible until it is paid,
/// and this one was paid in disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanPlan {
    /// Accounts the scan looked at.
    pub considered: usize,
    /// The accounts a decision called due, in configuration order.
    pub due: Vec<SessionKey>,
}

/// Decide which accounts are due, and open nothing.
///
/// Free of side effects beyond reading the configuration and the durable schedule,
/// which is the whole reason it is separate. Every scheduling rule is decided here
/// and can therefore be asserted without a browser on the machine.
pub async fn scan(desktop: &Arc<Desktop>, now: i64) -> ScanPlan {
    let mut plan = ScanPlan::default();

    // Snapshot the declaration first and work from that. Reading the live config
    // per account would mean an account added halfway through the walk is seen and
    // an account removed halfway through it is half-processed.
    let declared: Vec<(String, String)> = {
        let config = desktop.core.config();
        config
            .providers
            .iter()
            .flat_map(|p| {
                p.accounts
                    .iter()
                    .map(move |a| (p.id.clone(), a.account_id.clone()))
            })
            .collect()
    };

    // Pruning here rather than at load: the configuration is only readable now, and
    // pruning against an empty declaration would erase every history in the file.
    let forgotten = desktop.checkin_schedule.prune_to(&declared);
    if forgotten > 0 {
        tracing::debug!(
            forgotten,
            "check-in schedule dropped accounts that are no longer declared"
        );
    }

    for (provider_id, account_id) in declared {
        plan.considered += 1;
        let Some(maintenance) = desktop.maintenance_for(&provider_id, &account_id) else {
            continue;
        };
        if decide(
            &maintenance,
            desktop
                .checkin_schedule
                .last_settled_at(&provider_id, &account_id),
            now,
        )
        .should_run()
        {
            plan.due.push(SessionKey::new(&provider_id, &account_id));
        }
    }
    plan
}

/// Decide and then act: open a browser for each account a decision called due.
///
/// The only part of this module that opens anything, and it is the only part that
/// needs a browser to exist on the machine.
pub async fn run(desktop: &Arc<Desktop>, now: i64) -> ScanOutcome {
    let plan = scan(desktop, now).await;
    let mut outcome = ScanOutcome {
        considered: plan.considered,
        due: plan.due.len(),
        ..ScanOutcome::default()
    };

    for key in plan.due {
        // The guard that keeps a WAF pause from becoming a spawn loop. Asked about
        // the live runtime, never inferred from a stored report: a report written
        // before a pause must not be read as "nothing is running".
        if desktop.checkin.phase(&key).await.is_some() {
            outcome.skipped_busy += 1;
            continue;
        }
        match crate::checkin_commands::launch_attempt(desktop, &key.provider_id, &key.account_id)
            .await
        {
            Ok(_) => {
                outcome.started += 1;
                tracing::debug!(
                    provider = %key.provider_id,
                    account = %key.account_id,
                    "scheduler started a due check-in"
                );
            }
            Err(e) => {
                // Counted, not asserted. A scan that refused to launch because one
                // account is busy must still try the others, so this is recorded and
                // the walk continues.
                outcome.refused += 1;
                tracing::debug!(
                    provider = %key.provider_id,
                    account = %key.account_id,
                    error = %e,
                    "scheduler could not start a due check-in"
                );
            }
        }
    }
    outcome
}

/// What one pass over the accounts did, for logging and for tests.
///
/// A summary rather than a list of launches, because the interesting figure is
/// usually zero and "the scan ran and did nothing" has to be sayable without
/// reading a per-account log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Accounts the scan looked at.
    pub considered: usize,
    /// Accounts that came back [`MaintenanceDecision::Due`].
    pub due: usize,
    /// Of those, attempts actually opened.
    pub started: usize,
    /// Accounts skipped because a session is live or waiting for a human.
    pub skipped_busy: usize,
    /// Attempts the launch refused, most often because a session appeared first.
    pub refused: usize,
}

/// Record that an account settled, for the scheduler's next decision.
///
/// Called by the observation step rather than by the scan, because that is the
/// only place that knows whether an attempt actually finished. A *failure* counts
/// as a settlement, and deliberately so: an account that cannot check in must not
/// be retried on every tick, which is how one broken account turns a scheduler
/// into a loop. The interval is measured from the attempt, not from a success.
pub(crate) fn record_settlement(desktop: &Desktop, provider_id: &str, account_id: &str, at: i64) {
    if desktop
        .checkin_schedule
        .record_settled(provider_id, account_id, at)
    {
        tracing::debug!(
            provider = %provider_id,
            account = %account_id,
            at,
            "recorded a check-in settlement"
        );
    }
}

/// Group the accounts by why the scheduler did or did not act, for a panel.
///
/// Exists because the six answers are six different messages for the user and a
/// count per kind is what makes them renderable at all. Two reasons sharing a
/// group would send an operator to fix the wrong thing.
pub fn decisions_by_kind(
    desktop: &Desktop,
    now: i64,
) -> BTreeMap<&'static str, Vec<(String, String)>> {
    let mut grouped: BTreeMap<&'static str, Vec<(String, String)>> = BTreeMap::new();
    for provider in &desktop.core.config().providers {
        for account in &provider.accounts {
            let Some(maintenance) = desktop.maintenance_for(&provider.id, &account.account_id)
            else {
                continue;
            };
            let decision = decide(
                &maintenance,
                desktop
                    .checkin_schedule
                    .last_settled_at(&provider.id, &account.account_id),
                now,
            );
            let kind = match decision {
                MaintenanceDecision::NotApplicable => "not_applicable",
                MaintenanceDecision::Disabled => "disabled",
                MaintenanceDecision::NeedsFirstObservation => "needs_first_observation",
                MaintenanceDecision::NoPolicy => "no_policy",
                MaintenanceDecision::NotDue { .. } => "not_due",
                MaintenanceDecision::Due { .. } => "due",
            };
            grouped
                .entry(kind)
                .or_default()
                .push((provider.id.clone(), account.account_id.clone()));
        }
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    use zroutery_core::config::{AppConfig, MaintenanceConfig};

    fn account(id: &str, maintenance: &MaintenanceConfig) -> serde_json::Value {
        serde_json::json!({ "account_id": id, "maintenance": maintenance })
    }

    /// A configuration built through its own deserialiser.
    ///
    /// Not a struct literal, for a reason worth keeping: `ProviderConfig` names a
    /// private type in one of its fields, so it cannot be built that way from out
    /// here at all. Going through the document an operator writes is also a better
    /// fixture — it exercises the real defaults rather than asserting against
    /// whichever fields this test happened to remember.
    fn config_with(accounts: Vec<serde_json::Value>) -> AppConfig {
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

    /// A desktop over a scratch directory that cleans itself up when it is dropped.
    ///
    /// The cleanup is not tidiness. Before it existed, one suite run left a hundred
    /// and seventy megabytes of temporary directories behind, thirty of them
    /// containing a real browser profile, because a test that asserted only on a
    /// decision still executed the launch. A fixture whose leak is invisible is a
    /// fixture that gets paid for in somebody else's disk.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("zroutery-sched-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn secrets() -> Arc<crate::secrets::KeychainSecrets> {
        Arc::new(crate::secrets::KeychainSecrets::new(format!(
            "app.zroutery.test.{}",
            uuid::Uuid::new_v4()
        )))
    }

    fn desktop(config: AppConfig) -> (Arc<Desktop>, Scratch) {
        let dir = scratch("run");
        let desktop = Arc::new(Desktop::new(dir.0.clone(), config, secrets()));
        (desktop, dir)
    }

    /// A declaration that can be acted on, so each fixture varies one field.
    fn daily(interval: Option<u64>) -> MaintenanceConfig {
        MaintenanceConfig {
            checkin_enabled: true,
            checkin_interval_secs: interval,
            checkin_path: Some("/console/personal".into()),
            checkin_trigger_selector: Some("button.checkin".into()),
            ..Default::default()
        }
    }

    fn one_account(interval: Option<u64>) -> (Arc<Desktop>, Scratch) {
        desktop(config_with(vec![account("a", &daily(interval))]))
    }

    #[tokio::test]
    async fn an_account_with_no_history_is_not_due() {
        // The conservative branch, and the one that stops every configured account
        // being opened within seconds of launch.
        let (desktop, _dir) = one_account(Some(60));
        let plan = scan(&desktop, 1_000).await;
        assert_eq!(plan.considered, 1);
        assert_eq!(
            plan.due.len(),
            0,
            "no history means no evidence about when to act"
        );
    }

    #[tokio::test]
    async fn an_account_that_settled_long_enough_ago_is_due() {
        let (desktop, _dir) = one_account(Some(3_600));
        record_settlement(&desktop, "prov", "a", 1_000);
        assert_eq!(
            scan(&desktop, 1_999).await.due.len(),
            0,
            "still inside the interval"
        );
        assert_eq!(
            scan(&desktop, 1_000 + 3_600).await.due.len(),
            1,
            "the interval elapsed"
        );
    }

    #[tokio::test]
    async fn deciding_never_opens_a_browser() {
        // **The regression that keeps this module honest.** Deciding and launching
        // were one function, so asserting "this account is due" ran the launch too:
        // `find_browser` succeeds on a developer machine, and a suite run opened
        // thirty real browsers and left thirty profiles in the temporary directory.
        // This asserts the split rather than the scheduling rules, because the rules
        // are all tested above and this is the property that makes them testable.
        let (desktop, dir) = one_account(Some(1));
        record_settlement(&desktop, "prov", "a", 0);
        assert_eq!(
            scan(&desktop, 1_000_000).await.due.len(),
            1,
            "the account is due"
        );
        assert!(
            !dir.0.join("browser-profiles").exists(),
            "deciding must not create a browser profile, so this suite cannot open \
             windows or leave state behind"
        );
    }

    #[tokio::test]
    async fn a_settlement_is_survived_by_a_new_desktop_over_the_same_file() {
        // What a relaunch looks like. Without the durable schedule every account
        // would come back with no history, and the launch would open a browser for
        // every one of them.
        let dir = scratch("restart");
        let config = config_with(vec![account("a", &daily(Some(3_600)))]);
        let first = Arc::new(Desktop::new(dir.0.clone(), config.clone(), secrets()));
        record_settlement(&first, "prov", "a", 1_000);

        let second = Arc::new(Desktop::new(dir.0.clone(), config, secrets()));
        assert_eq!(
            scan(&second, 1_000 + 3_599).await.due.len(),
            0,
            "a relaunched application must not treat a settled account as untried"
        );
    }

    #[tokio::test]
    async fn a_declaration_that_cannot_be_acted_on_is_never_due() {
        let not_configured = MaintenanceConfig {
            checkin_enabled: true,
            checkin_interval_secs: Some(60),
            checkin_path: Some("/console/personal".into()),
            ..Default::default()
        };
        let (desktop, _dir) = desktop(config_with(vec![account("a", &not_configured)]));
        record_settlement(&desktop, "prov", "a", 0);
        assert_eq!(scan(&desktop, 1_000_000).await.due.len(), 0);
    }

    #[tokio::test]
    async fn a_disabled_account_is_never_due() {
        let off = MaintenanceConfig {
            checkin_enabled: false,
            ..daily(Some(60))
        };
        let (desktop, _dir) = desktop(config_with(vec![account("a", &off)]));
        record_settlement(&desktop, "prov", "a", 0);
        assert_eq!(scan(&desktop, 1_000_000).await.due.len(), 0);
    }

    #[tokio::test]
    async fn a_live_session_is_skipped_by_the_launch_not_by_the_decision() {
        // The serious one: a WAF pause holds its session for as long as the human
        // takes, and a launch that restarted it would open a fresh browser every
        // tick for the same unverified endpoint. The decision still says due — that
        // is correct — and the guard is the launch's job.
        let (desktop, _dir) = one_account(Some(1));
        record_settlement(&desktop, "prov", "a", 0);
        let key = SessionKey::new("prov", "a");
        desktop
            .checkin
            .begin(key.clone())
            .await
            .expect("occupy the key");

        assert_eq!(scan(&desktop, 1_000_000).await.due.len(), 1);

        let outcome = run(&desktop, 1_000_000).await;
        assert_eq!(outcome.due, 1);
        assert_eq!(
            outcome.skipped_busy, 1,
            "a live session must be skipped, not restarted"
        );
        assert_eq!(outcome.started, 0);
        desktop.checkin.finish(&key).await;
    }

    #[tokio::test]
    async fn a_failed_attempt_still_counts_as_a_settlement() {
        // Otherwise an account that cannot check in is due on every single tick,
        // and one broken account turns the scheduler into a loop.
        let (desktop, _dir) = one_account(Some(3_600));
        record_settlement(&desktop, "prov", "a", 1_000);
        assert_eq!(scan(&desktop, 1_000 + 10).await.due.len(), 0);
        assert_eq!(scan(&desktop, 1_000 + 3_601).await.due.len(), 1);
    }

    #[tokio::test]
    async fn every_declining_reason_reaches_the_panel_as_its_own_group() {
        // A panel renders these; two reasons sharing a group would send an operator
        // to fix the wrong thing. Each account carries a different declaration so
        // each reason comes from configuration rather than from timing.
        let unconfigured = MaintenanceConfig {
            checkin_enabled: true,
            checkin_interval_secs: Some(3_600),
            checkin_path: Some("/console/personal".into()),
            ..Default::default()
        };
        let (desktop, _dir) = desktop(config_with(vec![
            account(
                "off",
                &MaintenanceConfig {
                    checkin_enabled: false,
                    ..daily(Some(3_600))
                },
            ),
            account("unconfigured", &unconfigured),
            account("nointerval", &daily(None)),
            account("fresh", &daily(Some(3_600))),
            account("settled", &daily(Some(3_600))),
        ]));
        record_settlement(&desktop, "prov", "nointerval", 0);
        record_settlement(&desktop, "prov", "settled", 0);

        let grouped = decisions_by_kind(&desktop, 1_000);
        assert_eq!(grouped.get("disabled").map(Vec::len), Some(1));
        assert_eq!(grouped.get("not_applicable").map(Vec::len), Some(1));
        assert_eq!(
            grouped.get("needs_first_observation").map(Vec::len),
            Some(1),
            "an account that has never run reports its own reason, not 'no policy'"
        );
        assert_eq!(
            grouped.get("no_policy").map(Vec::len),
            Some(1),
            "a settled account with no interval says the interval is missing"
        );
        assert_eq!(
            grouped.get("not_due").map(Vec::len),
            Some(1),
            "the only account with both a policy and a history, not yet due"
        );
        assert!(
            !grouped.contains_key("due"),
            "nothing is due an hour after settling against a one-hour interval"
        );
    }

    #[tokio::test]
    async fn the_scan_considers_every_declared_account_exactly_once() {
        let (desktop, _dir) = desktop(config_with(vec![
            account("a", &daily(Some(3_600))),
            account("b", &daily(Some(3_600))),
            account("c", &daily(Some(3_600))),
        ]));
        let plan = scan(&desktop, 1_000).await;
        assert_eq!(plan.considered, 3);
        assert_eq!(plan.due.len(), 0);
    }

    #[tokio::test]
    async fn a_forgotten_account_leaves_nothing_behind_in_the_schedule() {
        // An account removed from the configuration must stop being scheduled, and
        // its history must not sit in the file forever.
        let mut config = config_with(vec![
            account("kept", &daily(Some(3_600))),
            account("removed", &daily(Some(3_600))),
        ]);
        let dir = scratch("prune");

        let first = Arc::new(Desktop::new(dir.0.clone(), config.clone(), secrets()));
        record_settlement(&first, "prov", "kept", 10);
        record_settlement(&first, "prov", "removed", 20);
        drop(first);

        config.providers[0]
            .accounts
            .retain(|a| a.account_id == "kept");
        let second = Arc::new(Desktop::new(dir.0.clone(), config, secrets()));
        scan(&second, 100).await;
        assert_eq!(
            second.checkin_schedule.last_settled_at("prov", "removed"),
            None,
            "an undeclared account must not keep a history"
        );
        assert_eq!(
            second.checkin_schedule.last_settled_at("prov", "kept"),
            Some(10)
        );
    }
}
