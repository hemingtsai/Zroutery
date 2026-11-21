//! AgentRouter's login-settled check-in. Tickets and browser credentials stay
//! inside the page; only a fresh, identity-checked login result leaves it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use zroutery_core::account::{
    AccountId, CheckinConfirmation, CheckinPhase, CheckinReport, CheckinReward, ResourceAmount,
    RewardSource,
};

pub const ORIGIN: &str = "https://agentrouter.org";
/// How long the automatic half of the flow (sign-out, page load, callback
/// detection) may take before it is reported instead of waiting forever.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a person may take on GitHub. The password, 2FA and consent are
/// human steps, so this is deliberately much longer than [`SETTLE_TIMEOUT`]; a
/// timeout here ends the attempt and closes the window instead of parking it
/// indefinitely.
pub const HUMAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// AgentRouter reports quota in NewAPI units: 500,000 quota per USD. The archive
/// record measured the same ratio on the page and the API.
const QUOTA_PER_USD: f64 = 500_000.0;

#[derive(Clone, Copy)]
struct DriveLimits {
    timeout: Duration,
    human: Duration,
    poll: Duration,
}

const LIVE_LIMITS: DriveLimits = DriveLimits {
    timeout: SETTLE_TIMEOUT,
    human: HUMAN_TIMEOUT,
    poll: POLL_INTERVAL,
};

use super::cdp::{BrowserSession, PageBinding};
use super::state::Event;
use super::{CheckinRuntime, SessionKey};
use crate::state::{CheckinView, Desktop};

/// Never infer this flow from an account, provider or model name.
pub fn is_panel(root: &str) -> bool {
    url::Url::parse(root).is_ok_and(|url| {
        url.origin().ascii_serialization() == ORIGIN
            && url.username().is_empty()
            && url.password().is_none()
    })
}

/// Non-secret, in-memory progress retained across a human-verification pause.
/// There is deliberately no serde implementation or OAuth state/code field.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub started_at: i64,
    pub binding_version: String,
    pub user_id: String,
    pub prepared: bool,
    pub oauth_started: bool,
    pub popup_target: Option<String>,
    pub login_target: Option<String>,
    pub previous_targets: Vec<String>,
    /// The panel's own quota reading taken just before sign-out, when a session
    /// was still usable. Only a threshold for the post-login observation; it
    /// never leaves the application and carries no identity.
    pub quota_before: Option<i64>,
}

impl Attempt {
    pub fn new(started_at: i64, binding_version: String, user_id: String) -> Self {
        Self {
            started_at,
            binding_version,
            user_id,
            prepared: false,
            oauth_started: false,
            popup_target: None,
            login_target: None,
            previous_targets: Vec::new(),
            quota_before: None,
        }
    }
}

/// What the panel's own surfaces established about the completed login.
///
/// Only amounts are carried; the access token, cookie and password stay in the
/// page. `log_quota` is a structured field of the panel's log row, and
/// `quota_after` is the structured `quota` of `/api/user/self`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    pub checked_in: bool,
    pub quota_after: Option<i64>,
    pub log_quota: Option<i64>,
    pub log_at: Option<i64>,
}

impl Settlement {
    pub fn signed_in(checked_in: bool) -> Self {
        Self {
            checked_in,
            quota_after: None,
            log_quota: None,
            log_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Pending,
    Prepared { quota: Option<i64> },
    Started,
    NeedsUser,
    Unconfirmed,
    SignedIn(Settlement),
    Failed(&'static str),
}

/// Ignore all unrecognised fields, including any accidentally returned secret.
/// A false/missing boolean or an old timestamp must not become a new reward.
pub fn parse(value: &Value, attempt: &Attempt) -> Probe {
    let Some(text) = value.as_str() else {
        return Probe::Failed("the login probe returned no result");
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Probe::Failed("the login probe returned an invalid result");
    };
    match value.get("state").and_then(Value::as_str) {
        Some("pending") => Probe::Pending,
        Some("prepared") => Probe::Prepared {
            quota: value.get("quota").and_then(Value::as_i64),
        },
        Some("started") => Probe::Started,
        Some("needs_user") => Probe::NeedsUser,
        Some("unconfirmed") => Probe::Unconfirmed,
        Some("identity_mismatch") => Probe::Failed(
            "the browser is signed in to a different account; verify the account binding first",
        ),
        Some("logout_failed") => {
            Probe::Failed("the panel did not confirm sign-out; the old login result was not reused")
        }
        Some("oauth_failed") => Probe::Failed(
            "the panel could not start GitHub sign-in; no authorization ticket was replayed",
        ),
        Some("signed_in") => {
            if value.get("user_id").and_then(Value::as_str) != Some(attempt.user_id.as_str()) {
                return Probe::Failed("the completed login belongs to a different account");
            }
            let fresh = value
                .get("last_login_time")
                .and_then(Value::as_i64)
                .is_some_and(|at| at >= attempt.started_at);
            if !attempt.prepared || !attempt.oauth_started || !fresh {
                return Probe::Pending;
            }
            let Some(checked_in) = value.get("checked_in").and_then(Value::as_bool) else {
                return Probe::Pending;
            };
            Probe::SignedIn(Settlement {
                checked_in,
                quota_after: value.get("quota").and_then(Value::as_i64),
                log_quota: value.get("log_quota").and_then(Value::as_i64),
                log_at: value.get("log_at").and_then(Value::as_i64),
            })
        }
        _ => Probe::Failed("the login probe returned an unknown result"),
    }
}

fn expression(script: &str, attempt: &Attempt) -> String {
    script
        .replace(
            "__ORIGIN__",
            &serde_json::to_string(ORIGIN).expect("string"),
        )
        .replace(
            "__USER_ID__",
            &serde_json::to_string(&attempt.user_id).expect("string"),
        )
        .replace("__STARTED_AT__", &attempt.started_at.to_string())
}

pub fn prepare_expression(attempt: &Attempt) -> String {
    expression(include_str!("agentrouter/prepare.js"), attempt)
}

pub fn oauth_expression(attempt: &Attempt) -> String {
    expression(include_str!("agentrouter/oauth.js"), attempt)
}

pub fn observe_expression(attempt: &Attempt) -> String {
    expression(include_str!("agentrouter/observe.js"), attempt)
}

fn page(info: &Value) -> Option<(&str, &str)> {
    (info.get("type")?.as_str()? == "page")
        .then_some((info.get("targetId")?.as_str()?, info.get("url")?.as_str()?))
}

/// Only a new child target of the page that initiated this OAuth may be adopted.
/// A matching URL elsewhere in the browser is not evidence of ownership.
fn popup_target(infos: &[Value], opener: &str, attempt: &Attempt) -> Option<String> {
    if let Some(owned) = attempt.popup_target.as_deref() {
        return infos
            .iter()
            .filter_map(page)
            .any(|(id, _)| id == owned)
            .then(|| owned.to_string());
    }
    let mut candidates = infos.iter().filter(|info| {
        let Some((id, location)) = page(info) else {
            return false;
        };
        if attempt
            .previous_targets
            .iter()
            .any(|previous| previous == id)
            || info.get("openerId").and_then(Value::as_str) != Some(opener)
        {
            return false;
        }
        url::Url::parse(location).is_ok_and(|url| {
            let origin = url.origin().ascii_serialization();
            origin == ORIGIN || origin == "https://github.com"
        })
    });
    let target = candidates
        .next()
        .and_then(page)
        .map(|(id, _)| id.to_string())?;
    candidates.next().is_none().then_some(target)
}

fn on_panel(infos: &[Value], target: &str) -> bool {
    infos.iter().filter_map(page).any(|(id, location)| {
        id == target
            && url::Url::parse(location)
                .is_ok_and(|url| url.origin().ascii_serialization() == ORIGIN)
    })
}

/// Drive exactly one login. A pause retains preparation, popup ownership and
/// the original start time, so resume cannot sign out or launch OAuth again.
pub(crate) async fn drive(
    runtime: CheckinRuntime,
    desktop: Arc<Desktop>,
    key: SessionKey,
    browser: BrowserSession,
    binding: PageBinding,
) {
    drive_with_limits(runtime, desktop, key, browser, binding, LIVE_LIMITS).await;
}

async fn drive_with_limits(
    runtime: CheckinRuntime,
    desktop: Arc<Desktop>,
    key: SessionKey,
    mut browser: BrowserSession,
    mut binding: PageBinding,
    limits: DriveLimits,
) {
    let mut attempt = browser
        .login_attempt
        .take()
        .expect("login driver requires attempt");
    attempt
        .login_target
        .get_or_insert_with(|| binding.target_id.clone());
    let started = Instant::now();
    let deadline = started + limits.timeout;
    // The person's half of the flow gets its own, longer window: GitHub's
    // password, 2FA and consent cannot be completed inside the automatic
    // deadline, and treating that as a stall is what left a browser parked
    // forever.
    let human_deadline = started + limits.human;
    let mut completed = None;
    let mut problem = None;
    let mut cancelled = false;
    let mut pause = false;
    let mut transient_errors = 0;
    let mut unconfirmed = false;

    while Instant::now()
        < if attempt.oauth_started {
            human_deadline
        } else {
            deadline
        }
    {
        if runtime.is_cancel_requested(&key).await
            || runtime.purpose(&key).await != Some(super::SessionPurpose::Checkin)
        {
            cancelled = true;
            break;
        }
        if !browser.is_running().await {
            problem = Some("the check-in browser was closed before login was confirmed".into());
            break;
        }
        if crate::checkin_commands::checkin_binding_version(
            &desktop,
            &key.provider_id,
            &key.account_id,
        )
        .ok()
        .as_deref()
            != Some(attempt.binding_version.as_str())
        {
            problem = Some("the account binding changed while check-in was open".to_string());
            break;
        }
        let result = if !attempt.prepared {
            match browser.page_evidence(&binding).await {
                Ok(evidence)
                    if super::waf::classify(&evidence) == super::waf::Verdict::Challenge =>
                {
                    pause = true;
                    break;
                }
                Ok(evidence) if super::waf::classify(&evidence) == super::waf::Verdict::Clear => {
                    browser
                        .evaluate(&binding, &prepare_expression(&attempt))
                        .await
                }
                _ => {
                    tokio::time::sleep(limits.poll).await;
                    continue;
                }
            }
        } else if !attempt.oauth_started {
            let ready_expression = format!(
                "location.origin === {} && document.readyState === 'complete'",
                serde_json::to_string(ORIGIN).expect("origin string")
            );
            let ready = browser
                .evaluate(&binding, &ready_expression)
                .await
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !ready {
                tokio::time::sleep(limits.poll).await;
                continue;
            }
            let infos = match browser.target_infos().await {
                Ok(infos) => infos,
                Err(_) => {
                    problem = Some("the browser could not identify this login's popup".into());
                    break;
                }
            };
            attempt.previous_targets = infos
                .iter()
                .filter_map(page)
                .map(|(id, _)| id.to_string())
                .collect();
            // Record before the side effect: a protocol timeout may lose the
            // reply after the page already opened OAuth. Resume must only watch.
            attempt.oauth_started = true;
            browser
                .evaluate_user_action(&binding, &oauth_expression(&attempt))
                .await
        } else {
            let infos = match browser.target_infos().await {
                Ok(infos) => infos,
                Err(_) => {
                    problem = Some("the browser could not verify the callback page; the login result is unconfirmed".into());
                    break;
                }
            };
            if attempt
                .popup_target
                .as_deref()
                .is_some_and(|owned| !infos.iter().filter_map(page).any(|(id, _)| id == owned))
            {
                // The last owned callback is gone. Do not replace that evidence
                // with the opener's shared storage or silently start OAuth again.
                problem = Some("the OAuth callback page closed before its check-in result was confirmed; verify the account on the panel before retrying".into());
                break;
            }
            let opener = attempt
                .login_target
                .as_deref()
                .unwrap_or(&binding.target_id);
            if let Some(target) = popup_target(&infos, opener, &attempt) {
                attempt.popup_target = Some(target.clone());
                if on_panel(&infos, &target) && binding.target_id != target {
                    match browser.bind_page(target).await {
                        Ok(page) => binding = page,
                        Err(_) => {
                            tokio::time::sleep(limits.poll).await;
                            continue;
                        }
                    }
                }
            }
            // Shared localStorage is not callback ownership. An unrelated or
            // stale callback tab must not finish this attempt from the opener.
            if attempt.popup_target.as_deref() != Some(binding.target_id.as_str())
                || !on_panel(&infos, &binding.target_id)
            {
                tokio::time::sleep(limits.poll).await;
                continue;
            }
            browser
                .evaluate(&binding, &observe_expression(&attempt))
                .await
        };
        let probe = match result {
            Ok(value) => {
                transient_errors = 0;
                parse(&value, &attempt)
            }
            Err(_) => {
                transient_errors += 1;
                if transient_errors >= 5 {
                    problem = Some("the browser could not read the login result".into());
                    break;
                }
                tokio::time::sleep(limits.poll).await;
                continue;
            }
        };
        match probe {
            Probe::Prepared { quota } => {
                attempt.prepared = true;
                if quota.is_some() {
                    attempt.quota_before = quota;
                }
                if browser
                    .navigate(&binding, &format!("{ORIGIN}/login"))
                    .await
                    .is_err()
                {
                    problem = Some("the browser could not open the fresh login page".into());
                    break;
                }
            }
            Probe::Started => attempt.oauth_started = true,
            Probe::SignedIn(settlement) => {
                completed = Some(settlement);
                break;
            }
            Probe::NeedsUser => {
                // The OAuth request may have succeeded even if popup creation
                // was blocked. Never blindly fetch another state on resume.
                if attempt.prepared {
                    attempt.oauth_started = true;
                }
                pause = true;
                break;
            }
            Probe::Failed(message) => {
                problem = Some(message.to_string());
                break;
            }
            Probe::Unconfirmed => unconfirmed = true,
            Probe::Pending => {}
        }
        tokio::time::sleep(limits.poll).await;
    }

    if completed.is_none() && problem.is_none() && !cancelled {
        if unconfirmed {
            problem = Some("the panel did not report a fresh, matching login time and check-in flag; the result remains unconfirmed, so verify it before retrying".into());
        } else if attempt.oauth_started && attempt.popup_target.is_some() {
            // The person is still on GitHub. Keep this same browser for a
            // resume, but the scheduler bounds how long any login pause may
            // hold a profile (see `scheduler::expire_login_pauses`).
            pause = true;
        } else if attempt.oauth_started {
            problem = Some(
                "the GitHub sign-in window was blocked or closed before the callback arrived; the check-in browser was closed".into(),
            );
        } else {
            problem = Some(
                "the panel's sign-in page did not load before the check-in timed out; the check-in browser was closed".into(),
            );
        }
    }
    if pause && !cancelled {
        let _ = runtime.advance(&key, Event::WafBlocked).await;
        let _ = runtime.advance(&key, Event::AwaitingUser).await;
        browser.login_attempt = Some(attempt.clone());
        let mut view = CheckinView::idle(&key.provider_id, &key.account_id);
        view.phase = "waiting_for_user".into();
        view.browser_held = true;
        view.awaiting_user = true;
        view.failure = Some(
            "finish GitHub sign-in or verification in the open browser, then resume this check-in; an unattended pause is closed automatically"
                .into(),
        );
        desktop.record_checkin_view(view);
        match runtime.park_browser(&key, browser).await {
            Ok(()) => return,
            Err(returned) => {
                browser = *returned;
                cancelled = true;
            }
        }
    }

    let mut cleanup_notice = None;
    // The browser on this profile was launched by this flow, so a terminal
    // attempt closes it entirely. Closing only "owned" tabs used to leave the
    // window (and the profile lock) behind whenever the browser had any other
    // tab, which made the next attempt fail with a profile-in-use refusal. The
    // attempt is handed back so the page close can still tidy the flow's tabs
    // before the window goes.
    browser.login_attempt = Some(attempt.clone());
    if browser.close_checkin().await.is_err() {
        cleanup_notice = Some(
            "post-check-in cleanup: the browser did not close; close that window before another attempt"
                .to_string(),
        );
    }
    let outcome = if cancelled {
        Event::Cancelled
    } else if let Some(settlement) = completed.as_ref() {
        if settlement.checked_in {
            Event::Succeeded
        } else {
            Event::AlreadyCompleted
        }
    } else {
        Event::Failed
    };
    // Linearize completion with cancel while the driver still owns the key.
    // A cancel that arrived after the response but before this point wins.
    cancelled |= runtime.conclude_driver(&key, outcome).await;
    let settled_at = chrono::Utc::now().timestamp();
    let report = completed.as_ref().filter(|_| !cancelled).map(|settlement| {
        report(
            &key.provider_id,
            &key.account_id,
            &attempt,
            settlement,
            settled_at,
        )
    });
    let mut view = match (&report, &completed) {
        (Some(report), Some(settlement)) => {
            let mut view = crate::checkin_commands::view_from_report(
                &key.provider_id,
                &key.account_id,
                report,
            );
            apply_observed_delta(&mut view, &attempt, settlement);
            view
        }
        _ => {
            let mut view = CheckinView::idle(&key.provider_id, &key.account_id);
            view.phase = if cancelled { "cancelled" } else { "failed" }.into();
            view.failure = problem;
            view
        }
    };
    if let Some(notice) = cleanup_notice {
        view.failure = Some(match view.failure {
            Some(failure) => format!("{failure}; {notice}"),
            None => notice,
        });
    }
    view.browser_held = false;
    view.awaiting_user = false;
    let still_bound = crate::checkin_commands::checkin_binding_version(
        &desktop,
        &key.provider_id,
        &key.account_id,
    )
    .ok()
    .as_deref()
        == Some(attempt.binding_version.as_str());
    if still_bound {
        if let Some(report) = report.as_ref() {
            if let Some(event) = zroutery_core::account::checkin::as_resource_event(report) {
                desktop.account_events.record(event);
            }
        }
        desktop.record_checkin_view(view);
        super::scheduler::record_settlement(
            &desktop,
            &key.provider_id,
            &key.account_id,
            chrono::Utc::now().timestamp(),
        );
    } else {
        desktop.forget_checkin_view(&key.provider_id, &key.account_id);
    }
    runtime.end_driving(&key).await;
    runtime.finish(&key).await;
    if still_bound && completed.is_some() && !cancelled {
        // Use the existing versioned balance refresh. It asks the panel with
        // the stored personal token, not with an exported browser cookie.
        let _ = desktop.refresh_balance(&key.provider_id).await;
    }
}

#[cfg(test)]
#[path = "agentrouter/driver_tests.rs"]
mod driver_tests;

/// Turn the panel's own records into the report. A fresh structured quota in
/// the panel's log row is a measured amount (`ProviderRecord`); without one the
/// login still establishes completion, but no amount is invented. A before/after
/// quota difference is weaker evidence and is only ever shown in the view as an
/// inferred observation, never as a confirmed reward.
pub fn report(
    provider_id: &str,
    account_id: &str,
    attempt: &Attempt,
    settlement: &Settlement,
    observed_at: i64,
) -> CheckinReport {
    let mut report = CheckinReport::requested(
        provider_id,
        AccountId(account_id.to_string()),
        attempt.started_at,
    );
    let reward = log_reward(settlement);
    if settlement.checked_in {
        report.confirmation = Some(match reward.clone() {
            Some(reward) => CheckinConfirmation::Confirmed { reward },
            None => CheckinConfirmation::Completed {
                source: RewardSource::ProviderRecord,
            },
        });
        report.settle(
            if reward.is_some() {
                CheckinPhase::Confirmed
            } else {
                CheckinPhase::Completed
            },
            observed_at,
        )
    } else {
        report.confirmation = Some(CheckinConfirmation::AlreadyCompleted { reward: None });
        report.settle(CheckinPhase::AlreadyCompleted, observed_at)
    }
}

/// The reward a fresh, structured log row establishes. `None` when the row is
/// absent, non-positive or older than this attempt: an old check-in's grant is
/// not this check-in's reward.
fn log_reward(settlement: &Settlement) -> Option<CheckinReward> {
    if !settlement.checked_in {
        return None;
    }
    let quota = settlement.log_quota.filter(|quota| *quota > 0)?;
    Some(CheckinReward {
        amount: ResourceAmount::normalised(quota as f64 / QUOTA_PER_USD, "USD"),
        source: RewardSource::ProviderRecord,
    })
}

/// The quota difference between the pre-sign-out reading and the post-login
/// reading, in USD. This is arithmetic over two observations, so the view labels
/// it as inferred; it is never used to claim a confirmed reward.
fn observed_delta(attempt: &Attempt, settlement: &Settlement) -> Option<f64> {
    let before = attempt.quota_before?;
    let after = settlement.quota_after?;
    let delta = after.checked_sub(before)?;
    (delta > 0).then_some(delta as f64 / QUOTA_PER_USD)
}

fn apply_observed_delta(view: &mut CheckinView, attempt: &Attempt, settlement: &Settlement) {
    if view.reward_amount.is_some() || !settlement.checked_in {
        return;
    }
    let Some(delta) = observed_delta(attempt, settlement) else {
        return;
    };
    tracing::info!(
        provider = %view.provider_id,
        account = %view.account_id,
        delta_usd = delta,
        quota_before = attempt.quota_before,
        quota_after = settlement.quota_after,
        log_quota = settlement.log_quota,
        log_at = settlement.log_at,
        "AgentRouter check-in quota observed"
    );
    view.reward_amount = Some((delta * 100.0).round() / 100.0);
    view.reward_unit = Some("USD".into());
    view.reward_source = Some("balance_delta".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attempt() -> Attempt {
        let mut attempt = Attempt::new(100, "binding:1".into(), "7".into());
        attempt.prepared = true;
        attempt.oauth_started = true;
        attempt
    }

    fn login(at: i64, checked_in: Value, user_id: &str) -> Value {
        Value::String(
            json!({"state":"signed_in", "user_id":user_id,
                "last_login_time":at, "checked_in":checked_in})
            .to_string(),
        )
    }

    #[test]
    fn only_the_exact_https_panel_selects_login_settlement() {
        assert!(is_panel(ORIGIN));
        assert!(is_panel("https://agentrouter.org/"));
        for root in [
            "http://agentrouter.org",
            "https://agentrouter.org.evil.test",
            "https://agentrouter.org:8443",
            "https://user@agentrouter.org",
            "https://other.example/agentrouter.org",
        ] {
            assert!(!is_panel(root), "{root}");
        }
    }

    fn target(id: &str, location: &str, opener: Option<&str>) -> Value {
        json!({"targetId":id, "type":"page", "url":location, "openerId":opener})
    }

    #[test]
    fn only_a_new_child_popup_can_become_the_callback() {
        let mut attempt = attempt();
        attempt.previous_targets = vec!["stale".into()];
        let infos = vec![
            target(
                "stale",
                "https://agentrouter.org/oauth/github?code=old",
                Some("login"),
            ),
            target(
                "unrelated",
                "https://agentrouter.org/console",
                Some("other"),
            ),
            target(
                "lookalike",
                "https://agentrouter.org.evil.test/console",
                Some("login"),
            ),
            target(
                "new",
                "https://agentrouter.org/console/token",
                Some("login"),
            ),
        ];
        assert_eq!(popup_target(&infos, "login", &attempt), Some("new".into()));
        attempt.popup_target = Some("new".into());
        assert!(on_panel(&infos, "new"));
        assert!(!on_panel(&infos, "lookalike"));
    }

    #[test]
    fn ambiguous_popups_are_not_selected_by_target_order() {
        let infos = vec![
            target(
                "a",
                "https://github.com/login/oauth/authorize",
                Some("login"),
            ),
            target("b", "https://agentrouter.org/oauth/github", Some("login")),
        ];
        assert_eq!(popup_target(&infos, "login", &attempt()), None);
    }

    #[test]
    fn an_old_checked_in_flag_never_confirms_this_attempt() {
        assert_eq!(
            parse(&login(99, json!(true), "7"), &attempt()),
            Probe::Pending
        );
        for checked_in in [Value::Null, json!("true"), json!(1)] {
            assert_eq!(
                parse(&login(101, checked_in, "7"), &attempt()),
                Probe::Pending
            );
        }
        let mut old = attempt();
        old.prepared = false;
        assert_eq!(parse(&login(101, json!(true), "7"), &old), Probe::Pending);
        old.prepared = true;
        old.oauth_started = false;
        assert_eq!(parse(&login(101, json!(true), "7"), &old), Probe::Pending);
    }

    #[test]
    fn a_fresh_false_flag_is_already_completed_not_a_new_reward() {
        assert_eq!(
            parse(&login(101, json!(false), "7"), &attempt()),
            Probe::SignedIn(Settlement::signed_in(false))
        );
        let report = report("p", "a", &attempt(), &Settlement::signed_in(false), 102);
        assert_eq!(report.phase, CheckinPhase::AlreadyCompleted);
        assert!(report.reward().is_none());
    }

    #[test]
    fn completion_without_a_fresh_log_row_does_not_invent_a_reward() {
        let settlement = Settlement {
            checked_in: true,
            quota_after: Some(50_000_000),
            log_quota: None,
            log_at: None,
        };
        let report = report("p", "a", &attempt(), &settlement, 102);
        assert_eq!(report.phase, CheckinPhase::Completed);
        assert!(report.reward().is_none());
        let event = zroutery_core::account::checkin::as_resource_event(&report).unwrap();
        assert!(event.resource_effect.amount().is_none());
    }

    #[test]
    fn a_fresh_log_row_is_a_measured_reward_in_usd() {
        let settlement = Settlement {
            checked_in: true,
            quota_after: Some(62_500_000),
            log_quota: Some(12_500_000),
            log_at: Some(101),
        };
        let report = report("p", "a", &attempt(), &settlement, 102);
        assert_eq!(report.phase, CheckinPhase::Confirmed);
        let reward = report.reward().expect("a structured log row is evidence");
        assert_eq!(reward.source, RewardSource::ProviderRecord);
        assert_eq!(reward.amount.value, 25.0);
        assert_eq!(reward.amount.unit, "USD");
        let event = zroutery_core::account::checkin::as_resource_event(&report).unwrap();
        assert_eq!(event.resource_effect.amount().map(|a| a.value), Some(25.0));
    }

    #[test]
    fn an_old_or_nonpositive_log_row_is_not_this_attempts_reward() {
        for log_quota in [None, Some(0), Some(-12_500_000)] {
            let settlement = Settlement {
                checked_in: true,
                quota_after: Some(50_000_000),
                log_quota,
                log_at: Some(99),
            };
            assert!(log_reward(&settlement).is_none(), "{log_quota:?}");
        }
    }

    #[test]
    fn a_quota_difference_is_shown_as_inferred_not_as_a_confirmed_reward() {
        let mut attempt = attempt();
        attempt.quota_before = Some(37_500_000);
        let settlement = Settlement {
            checked_in: true,
            quota_after: Some(50_000_000),
            log_quota: None,
            log_at: None,
        };
        let report = report("p", "a", &attempt, &settlement, 102);
        assert_eq!(report.phase, CheckinPhase::Completed);
        assert!(report.reward().is_none(), "a delta alone cannot confirm");

        let mut view = crate::checkin_commands::view_from_report("p", "a", &report);
        apply_observed_delta(&mut view, &attempt, &settlement);
        assert_eq!(view.reward_amount, Some(25.0));
        assert_eq!(view.reward_unit.as_deref(), Some("USD"));
        assert_eq!(view.reward_source.as_deref(), Some("balance_delta"));
    }

    #[test]
    fn a_structured_log_reward_is_not_overwritten_by_the_quota_difference() {
        let mut attempt = attempt();
        attempt.quota_before = Some(37_000_000);
        let settlement = Settlement {
            checked_in: true,
            quota_after: Some(50_000_000),
            log_quota: Some(12_500_000),
            log_at: Some(101),
        };
        let report = report("p", "a", &attempt, &settlement, 102);
        let mut view = crate::checkin_commands::view_from_report("p", "a", &report);
        apply_observed_delta(&mut view, &attempt, &settlement);
        assert_eq!(view.reward_amount, Some(25.0));
        assert_eq!(view.reward_source.as_deref(), Some("provider_record"));
    }

    #[test]
    fn prepare_records_the_quota_threshold_without_requiring_one() {
        let value = Value::String(json!({"state":"prepared", "quota":37500000}).to_string());
        assert_eq!(
            parse(&value, &attempt()),
            Probe::Prepared {
                quota: Some(37_500_000)
            }
        );
        let value = Value::String(json!({"state":"prepared"}).to_string());
        assert_eq!(parse(&value, &attempt()), Probe::Prepared { quota: None });
    }

    #[test]
    fn another_identity_cannot_complete_the_local_account() {
        assert!(matches!(
            parse(&login(101, json!(true), "8"), &attempt()),
            Probe::Failed(_)
        ));
    }

    #[test]
    fn untrusted_probe_errors_are_not_echoed_into_reports() {
        let bad = json!(r#"{"state":"error","message":"password=do-not-log"}"#);
        let Probe::Failed(message) = parse(&bad, &attempt()) else {
            panic!("a bad result must fail");
        };
        assert!(!message.contains("password"));
    }
}
