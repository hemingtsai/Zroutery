//! The account maintenance commands: start, resume, cancel and report.
//!
//! # What these commands do and do not do
//!
//! They **drive** a check-in. They do not decide whether one is due, do not
//! price it, and do not treat an HTTP answer as a result — the decision logic
//! lives in `zroutery_core::account`, and a command that reimplemented it would
//! be a second implementation that drifts.
//!
//! # Why start and resume are different commands
//!
//! They are different operations, not two names for one. `start` launches a
//! browser; `resume` continues a browser that is already open and already
//! authenticated, and the difference is the whole point of the pause design. If
//! resume were implemented as "start again", every challenge would cost the user
//! a fresh sign-in and the feature would be worse than useless behind a WAF.
//! [`checkin::state`] makes the distinction unrepresentable at the state level,
//! and these two commands are the only places the boundary is crossed.
//!
//! # No secret crosses this boundary
//!
//! A command takes ids and returns a [`CheckinView`]. The browser holds the
//! session, in its own profile directory; no cookie, token, password or page DOM
//! is accepted as an argument or returned as a result. That is why no argument
//! here is a credential.

#![cfg(feature = "account-maint")]

use std::sync::Arc;

use tauri::State;
use zroutery_core::account::adapters::newapi::checkin::{CheckinAttempt, CheckinContext};
use zroutery_core::account::{CheckinFailure, CheckinPhase, CheckinReport, RewardSource};

use crate::checkin::cdp::{self, BrowserSession};
use crate::checkin::state::{Event, State as Phase};
use crate::checkin::waf;
use crate::checkin::SessionKey;
use crate::state::{CheckinView, Desktop};

type Cmd<T> = Result<T, String>;

/// How long the page is given to settle before the attempt is called a failure.
const SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How often the page is re-read while it settles.
///
/// The interesting transition — a challenge appearing, or clearing — is a
/// function of time rather than of anything Zroutery sends, so this polls.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Start a browser check-in for one account.
///
/// Asynchronous on purpose: a check-in opens a visible browser and may pause on
/// a challenge, so blocking this call would freeze the window the user has to
/// interact with. The command returns once the browser is up and the page has
/// been loaded; the panel polls [`get_checkin_status`] for the rest.
#[tauri::command]
pub async fn start_checkin(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
) -> Cmd<CheckinView> {
    let key = SessionKey::new(&provider_id, &account_id);
    let Some(maintenance) = desktop.maintenance_for(&provider_id, &account_id) else {
        return Err(format!("no account {account_id} on provider {provider_id}"));
    };
    if !maintenance.is_browser_checkin_configured() {
        // Said as configuration rather than as a runtime failure, because the fix
        // is a line in the config file and not anything the user retries.
        return Err(
            "check-in is not configured for this account: set maintenance.checkin_enabled \
             and maintenance.checkin_path"
                .to_string(),
        );
    }
    let Some(path) = maintenance
        .checkin_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return Err("this account has no check-in path configured".to_string());
    };
    let base = desktop
        .core
        .config()
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .map(|p| p.base_url.clone())
        .ok_or_else(|| format!("no provider {provider_id}"))?;

    desktop
        .checkin
        .begin(key.clone())
        .await
        .map_err(|e| e.to_string())?;

    let executable = cdp::find_browser(&maintenance.browser_executable)
        .ok_or_else(|| cdp::BrowserError::NotFound.to_string())?;
    let profile_dir = desktop.checkin.profile_dir(&key);
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let url = if url.starts_with("http") {
        url
    } else {
        format!("http://{url}")
    };

    let mut browser = cdp::launch(&executable, &profile_dir, &url)
        .await
        .map_err(|e| {
            // The session is released so a failed start does not leave the key
            // occupied: otherwise the user's next attempt would be refused as a
            // duplicate of an attempt that never ran.
            release(desktop.checkin.clone(), &key);
            e.to_string()
        })?;

    let binding = match browser.open_page(&url).await {
        Ok(binding) => binding,
        Err(e) => {
            let _ = browser.shutdown().await;
            release(desktop.checkin.clone(), &key);
            return Err(e.to_string());
        }
    };
    desktop.checkin.attach(&key, browser).await.map_err(|e| {
        release(desktop.checkin.clone(), &key);
        e.to_string()
    })?;

    let runtime = desktop.checkin.clone();
    let task_key = key.clone();
    let desktop_for_task = Arc::clone(&desktop);
    tauri::async_runtime::spawn(async move {
        drive(runtime, desktop_for_task, task_key, binding).await;
    });

    desktop
        .checkin_view(&provider_id, &account_id)
        .await
        .ok_or_else(|| format!("no account {account_id} on provider {provider_id}"))
}

/// Continue a check-in that is waiting on a human-verification challenge.
///
/// This resumes the **same** browser, on the same page, with the same session.
/// It does not navigate, does not sign in and does not start anything new — the
/// operation picks up where it stopped. A call that arrives while nothing is
/// paused is refused rather than quietly starting a new attempt, because a
/// button labelled "continue" that silently begins a fresh login is the exact
/// behaviour the pause design exists to prevent.
#[tauri::command]
pub async fn resume_checkin(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
) -> Cmd<CheckinView> {
    let key = SessionKey::new(&provider_id, &account_id);
    match desktop.checkin.phase(&key).await {
        Some(Phase::WaitingForUser) | Some(Phase::BlockedByWaf) => {}
        Some(other) => {
            return Err(format!(
                "this check-in is not waiting for you (it is {other:?}); nothing to resume"
            ))
        }
        None => return Err("no check-in is open for this account".to_string()),
    }

    desktop
        .checkin
        .advance(&key, Event::UserResumed)
        .await
        .map_err(|e| e.to_string())?;

    let Some(browser) = desktop.checkin.take_browser(&key).await else {
        return Err("the browser for this check-in is gone; start a new one".to_string());
    };
    // The session id belongs to the browser, not to this layer, so it is asked for
    // again rather than kept: that is what makes the resume talk to the page the
    // user just solved the challenge in, and what makes a resume impossible if
    // that page is gone.
    let (browser, binding) = match rebind(browser).await {
        Some(pair) => pair,
        None => {
            release(desktop.checkin.clone(), &key);
            return Err("the page this check-in was using is gone".to_string());
        }
    };
    desktop
        .checkin
        .attach(&key, browser)
        .await
        .map_err(|e| e.to_string())?;

    let runtime = desktop.checkin.clone();
    let task_key = key.clone();
    let desktop_for_task = Arc::clone(&desktop);
    tauri::async_runtime::spawn(async move {
        drive(runtime, desktop_for_task, task_key, binding).await;
    });

    desktop
        .checkin_view(&provider_id, &account_id)
        .await
        .ok_or_else(|| format!("no account {account_id} on provider {provider_id}"))
}

/// Cancel a check-in, running or paused.
///
/// Legal from every live state, and the browser is closed rather than abandoned:
/// a user who cancels a paused challenge must not be left with a window nothing
/// owns. The session is released so the next attempt is not refused as a
/// duplicate.
#[tauri::command]
pub async fn cancel_checkin(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
) -> Cmd<CheckinView> {
    let key = SessionKey::new(&provider_id, &account_id);
    // The state machine has no terminal state that accepts an event, so this is
    // guarded on being live: cancelling something already finished is a no-op the
    // panel can render as "done", not an error.
    let _ = desktop.checkin.advance(&key, Event::Cancelled).await;
    desktop.checkin.finish(&key).await;

    let mut view = desktop
        .checkin_view(&provider_id, &account_id)
        .await
        .ok_or_else(|| format!("no account {account_id} on provider {provider_id}"))?;
    view.browser_held = false;
    view.awaiting_user = false;
    if view.phase == "running" || view.phase == "waiting_for_user" {
        view.phase = "cancelled".to_string();
    }
    Ok(view)
}

/// What one account's check-in is doing, right now.
///
/// Cheap and safe to poll: it reads the runtime's state and the last recorded
/// report, and never touches the browser. That matters because a panel polls it
/// while the user is mid-challenge, and a status read that had to query the page
/// would compete with the thing it is reporting on.
#[tauri::command]
pub async fn get_checkin_status(
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    account_id: String,
) -> Cmd<CheckinView> {
    desktop
        .checkin_view(&provider_id, &account_id)
        .await
        .ok_or_else(|| format!("no account {account_id} on provider {provider_id}"))
}

/// Ask the runtime for the current page's session again.
///
/// The session id is not stored by the desktop layer — it belongs to the browser
/// — so a resumed operation re-derives it from the browser's own target list.
/// Returning `None` means the page the challenge was being solved on is gone,
/// which is a real answer rather than a reason to navigate somewhere.
async fn rebind(mut browser: BrowserSession) -> Option<(BrowserSession, cdp::PageBinding)> {
    let listed = browser
        .call("Target.getTargets", serde_json::json!({}))
        .await
        .ok()?;
    let info = listed
        .get("targetInfos")?
        .as_array()?
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))?;
    let target_id = info.get("targetId")?.as_str()?.to_string();
    let session_id = browser
        .call(
            "Target.attachToTarget",
            serde_json::json!({ "targetId": target_id, "flatten": true }),
        )
        .await
        .ok()?
        .get("sessionId")?
        .as_str()?
        .to_string();
    let binding = cdp::PageBinding {
        target_id,
        session_id,
    };
    browser
        .call_page(&binding, "Runtime.enable", serde_json::json!({}))
        .await
        .ok()?;
    Some((browser, binding))
}

/// Release a session key whose attempt could not start.
fn release(runtime: crate::checkin::CheckinRuntime, key: &SessionKey) {
    let key = key.clone();
    tauri::async_runtime::spawn(async move {
        runtime.finish(&key).await;
    });
}

/// Watch the page and record what the attempt did.
///
/// # The policy, in full
///
/// Read the page; classify it; act on the verdict:
///
/// * [`waf::Verdict::Clear`] — the panel's own document is on screen, so the
///   browser has got through and the operation proceeds.
/// * [`waf::Verdict::Challenge`] — pause, keep the browser, ask the user.
/// * [`waf::Verdict::Indeterminate`] — **keep watching**. Neither reading is
///   established, and the two wrong choices are both bad: pausing on an
///   undetermined page asks the user to solve a challenge that is not there, and
///   proceeding reports a check-in that may not have run. So the loop continues
///   until the deadline, and a timeout is reported as `failed` rather than as
///   success.
///
/// # What it deliberately does not decide
///
/// Anything about *what the check-in did to the account*. That is the adapter's
/// job and it is reached over HTTP, not from here — so the view this records
/// carries no reward. A figure could only be produced by the observation half,
/// and inventing one in a function that has never spoken to the panel would be
/// exactly the fabrication the rest of the design refuses.
async fn drive(
    runtime: crate::checkin::CheckinRuntime,
    desktop: Arc<Desktop>,
    key: SessionKey,
    binding: cdp::PageBinding,
) {
    let started_at = chrono::Utc::now().timestamp();
    if runtime.advance(&key, Event::Start).await.is_err()
        || runtime.advance(&key, Event::BrowserStarted).await.is_err()
        || runtime.advance(&key, Event::Navigated).await.is_err()
    {
        return;
    }

    let Some(mut browser) = runtime.take_browser(&key).await else {
        return;
    };
    runtime.advance(&key, Event::OperationRunning).await.ok();

    let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
    let mut paused = false;
    let mut clear = false;

    while std::time::Instant::now() < deadline && !paused && !clear {
        tokio::time::sleep(POLL_INTERVAL).await;
        match browser.page_evidence(&binding).await {
            Ok(probe) => match waf::classify(&probe) {
                waf::Verdict::Challenge => paused = true,
                waf::Verdict::Clear => clear = true,
                waf::Verdict::Indeterminate => {
                    tracing::debug!(
                        provider = %key.provider_id,
                        account = %key.account_id,
                        url = %probe.url,
                        "check-in page is not settled; watching"
                    );
                }
            },
            Err(e) => {
                tracing::debug!(?e, "check-in could not read the page");
                break;
            }
        }
    }

    let (phase, failure) = if paused {
        let _ = runtime.advance(&key, Event::WafBlocked).await;
        let _ = runtime.advance(&key, Event::AwaitingUser).await;
        ("waiting_for_user", None)
    } else if clear {
        // The panel's own page is up. Whether the check-in was performed and what
        // it awarded is not established here; the observation half does that over
        // HTTP. What is established is that the browser got through, which is a
        // real and reportable fact — and the phase is `running` rather than
        // `succeeded` precisely because no reward has been observed.
        ("running", None)
    } else {
        let _ = runtime.advance(&key, Event::Failed).await;
        (
            "failed",
            Some("the panel did not load before the check-in timed out".to_string()),
        )
    };

    // The browser goes back either way. A pause leaves it open; the caller closes
    // it when the attempt is cancelled or observed to have finished.
    let _ = runtime.attach(&key, browser).await;

    let settled = phase.to_string();
    desktop.record_checkin_view(CheckinView {
        provider_id: key.provider_id.clone(),
        account_id: key.account_id.clone(),
        phase: settled,
        browser_held: true,
        awaiting_user: paused,
        failure,
        reward_amount: None,
        reward_unit: None,
        reward_source: None,
        last_completed_at: None,
    });

    // Only when the browser got through. A paused attempt has not happened yet, so
    // asking the provider what it granted would read the *previous* day's reward
    // and render it as this one's.
    if clear {
        let account = zroutery_core::account::AccountId(key.account_id.clone());
        observe(&desktop, &key, &account, started_at).await;
    }
}

/// Ask the provider what the attempt actually did, and record that instead.
///
/// # Why this is a separate step from [`drive`]
///
/// Because it is a different kind of fact. `drive` learned whether the *browser*
/// got through; this asks the *provider* what it granted, over its own API,
/// through the structured surfaces a log row cannot provide. Splitting them is the
/// execution-versus-observation discipline in its concrete form: a browser that
/// loaded the panel has not thereby established a reward, and a provider that
/// reports a reward has not thereby required a browser.
///
/// When no observation can be made — no credential stored, an instance without
/// the check-in endpoints, a network failure — the browser layer's record stands
/// unchanged. Overwriting it with a failure would turn "the check-in happened and
/// we could not confirm it" into "the check-in did not happen", which is the wrong
/// way round and would push a user to retry a check-in that already worked.
async fn observe(
    desktop: &Arc<Desktop>,
    key: &SessionKey,
    account_id: &zroutery_core::account::AccountId,
    started_at: i64,
) {
    let Some((adapter, quota_per_unit)) = desktop.panel_adapter(&key.provider_id, &key.account_id)
    else {
        tracing::debug!(
            provider = %key.provider_id,
            account = %key.account_id,
            "no stored credential for this account; the browser record stands unconfirmed"
        );
        return;
    };
    let until = chrono::Utc::now().timestamp();
    let observation = match adapter
        .observe_checkin(account_id, None, None, started_at, until)
        .await
    {
        Ok(observation) => observation,
        Err(e) => {
            tracing::debug!(?e, "check-in observation failed; the browser record stands");
            return;
        }
    };
    let ctx = CheckinContext::usd(&key.provider_id, account_id, quota_per_unit);
    let report = zroutery_core::account::adapters::newapi::checkin::reconcile(
        // The browser performed the operation, so there is no operation response
        // to read a figure from. `None` here is what makes the reconciliation fall
        // through to the provider's own check-in record rather than inventing one.
        CheckinAttempt::Accepted {
            quota_awarded: None,
            checkin_date: None,
        },
        observation,
        &ctx,
        started_at,
    );
    desktop.record_checkin_view(view_from_report(&key.provider_id, &key.account_id, &report));
}

/// Turn an adapter report into the view a panel renders.
///
/// Kept next to the commands rather than in the core so the core keeps no
/// knowledge of the wire vocabulary, and kept as one function so there is exactly
/// one place where a [`CheckinPhase`] becomes a string.
pub fn view_from_report(
    provider_id: &str,
    account_id: &str,
    report: &CheckinReport,
) -> CheckinView {
    let phase = match report.phase {
        CheckinPhase::Requested => "running",
        CheckinPhase::Executing => "running",
        CheckinPhase::ProviderAccepted => "running",
        CheckinPhase::ObservationPending => "running",
        CheckinPhase::Confirmed => "succeeded",
        CheckinPhase::AlreadyCompleted => "already_completed",
        CheckinPhase::NotSupported => "not_supported",
        CheckinPhase::BlockedByWaf => "waiting_for_user",
        CheckinPhase::NeedsUserAction => "waiting_for_user",
        CheckinPhase::Failed => "failed",
        CheckinPhase::Cancelled => "cancelled",
    };
    let reward = report.reward();
    CheckinView {
        provider_id: provider_id.to_string(),
        account_id: account_id.to_string(),
        phase: phase.to_string(),
        browser_held: report.phase.is_waiting_for_user(),
        awaiting_user: report.phase.is_waiting_for_user(),
        failure: report.failure.as_ref().map(describe_failure),
        reward_amount: reward.map(|r| r.amount.value),
        reward_unit: reward.map(|r| r.amount.unit.clone()),
        reward_source: reward.map(|r| reward_source(r.source).to_string()),
        last_completed_at: report.completed_at,
    }
}

/// The vocabulary a panel can branch on.
fn reward_source(source: RewardSource) -> &'static str {
    match source {
        RewardSource::ProviderResponse => "provider_response",
        RewardSource::ProviderRecord => "provider_record",
        RewardSource::EventContent => "event_content",
        RewardSource::BalanceDelta => "balance_delta",
    }
}

/// Say what went wrong, in the words a user can act on.
///
/// Each variant names the action, because "failed" tells a user nothing they can
/// do and is the reason this classification exists in the first place.
fn describe_failure(failure: &CheckinFailure) -> String {
    match failure {
        CheckinFailure::NotSupported => "this provider does not offer check-in".into(),
        CheckinFailure::Disabled => "check-in is switched off on this provider".into(),
        CheckinFailure::AuthenticationExpired => {
            "the credential is no longer accepted; sign in again".into()
        }
        CheckinFailure::WafBlocked => "a security challenge is in the way".into(),
        CheckinFailure::UserActionRequired => {
            "this check-in needs you to finish something in the browser".into()
        }
        CheckinFailure::NetworkFailure => "the provider could not be reached".into(),
        CheckinFailure::ProviderRejected { message, .. } if !message.is_empty() => {
            format!("the provider refused: {message}")
        }
        CheckinFailure::ProviderRejected { .. } => "the provider refused the check-in".into(),
        CheckinFailure::ObservationTimeout => {
            "the check-in may have succeeded but its result was not confirmed; not retrying \
             automatically to avoid granting a second reward"
                .into()
        }
        CheckinFailure::Cancelled => "cancelled".into(),
        CheckinFailure::Unknown { detail } if detail.is_empty() => "check-in failed".into(),
        CheckinFailure::Unknown { detail } => format!("check-in failed: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::account::{CheckinConfirmation, CheckinReward, ResourceAmount};

    fn report(phase: CheckinPhase) -> CheckinReport {
        let mut report =
            CheckinReport::requested("relay", zroutery_core::account::AccountId("main".into()), 1);
        report.phase = phase;
        report
    }

    #[test]
    fn every_phase_renders_as_a_name_the_panel_can_branch_on() {
        for phase in [
            CheckinPhase::Requested,
            CheckinPhase::Executing,
            CheckinPhase::ProviderAccepted,
            CheckinPhase::ObservationPending,
            CheckinPhase::Confirmed,
            CheckinPhase::AlreadyCompleted,
            CheckinPhase::NotSupported,
            CheckinPhase::BlockedByWaf,
            CheckinPhase::NeedsUserAction,
            CheckinPhase::Failed,
            CheckinPhase::Cancelled,
        ] {
            let view = view_from_report("relay", "main", &report(phase));
            assert!(
                !view.phase.is_empty(),
                "{phase:?} rendered as an empty phase"
            );
            assert!(
                matches!(
                    view.phase.as_str(),
                    "running"
                        | "succeeded"
                        | "already_completed"
                        | "not_supported"
                        | "waiting_for_user"
                        | "failed"
                        | "cancelled"
                ),
                "{phase:?} became the unknown phase {}",
                view.phase
            );
        }
    }

    #[test]
    fn an_unfinished_checkin_never_renders_as_a_reward() {
        for phase in [
            CheckinPhase::Requested,
            CheckinPhase::Executing,
            CheckinPhase::ProviderAccepted,
            CheckinPhase::ObservationPending,
            CheckinPhase::Failed,
            CheckinPhase::Cancelled,
            CheckinPhase::NotSupported,
            CheckinPhase::BlockedByWaf,
        ] {
            let view = view_from_report("relay", "main", &report(phase));
            assert!(
                view.reward_amount.is_none(),
                "{phase:?} rendered a reward: {:?}",
                view.reward_amount
            );
        }
    }

    #[test]
    fn a_accepted_but_unconfirmed_checkin_is_not_rendered_as_success() {
        // §23: the provider said yes and nothing observed establishes the effect.
        // Rendering that as `succeeded` with no figure would claim the check-in
        // was completed *and* leave the user with nothing to act on.
        for phase in [
            CheckinPhase::ProviderAccepted,
            CheckinPhase::ObservationPending,
        ] {
            let view = view_from_report("relay", "main", &report(phase));
            assert_eq!(view.phase, "running", "{phase:?}");
            assert_eq!(view.reward_amount, None);
        }
    }

    #[test]
    fn a_confirmed_checkin_carries_the_figure_and_says_where_it_came_from() {
        let mut report = report(CheckinPhase::Confirmed);
        report.confirmation = Some(CheckinConfirmation::Confirmed {
            reward: CheckinReward {
                amount: ResourceAmount::normalised(25.0, "USD"),
                source: RewardSource::ProviderResponse,
            },
        });
        let view = view_from_report("relay", "main", &report);
        assert_eq!(view.phase, "succeeded");
        assert_eq!(view.reward_amount, Some(25.0));
        assert_eq!(view.reward_unit.as_deref(), Some("USD"));
        assert_eq!(view.reward_source.as_deref(), Some("provider_response"));
    }

    #[test]
    fn a_pause_is_rendered_as_waiting_with_a_browser_held() {
        for phase in [CheckinPhase::BlockedByWaf, CheckinPhase::NeedsUserAction] {
            let view = view_from_report("relay", "main", &report(phase));
            assert_eq!(view.phase, "waiting_for_user");
            assert!(view.browser_held, "a pause keeps the browser open");
            assert!(view.awaiting_user);
        }
    }

    #[test]
    fn every_failure_class_names_something_a_user_can_do() {
        for failure in [
            CheckinFailure::NotSupported,
            CheckinFailure::Disabled,
            CheckinFailure::AuthenticationExpired,
            CheckinFailure::WafBlocked,
            CheckinFailure::UserActionRequired,
            CheckinFailure::NetworkFailure,
            CheckinFailure::ProviderRejected {
                code: None,
                message: "no".into(),
            },
            CheckinFailure::ObservationTimeout,
            CheckinFailure::Cancelled,
            CheckinFailure::Unknown {
                detail: String::new(),
            },
        ] {
            let text = describe_failure(&failure);
            assert!(!text.is_empty(), "{failure:?} renders as nothing");
            assert!(
                !text.eq_ignore_ascii_case("failed"),
                "{failure:?} renders as bare 'failed'"
            );
        }
    }

    #[test]
    fn an_observation_timeout_explains_why_it_is_not_retried() {
        // A timeout is the one failure a user is most likely to "fix" by retrying,
        // which is exactly what must not happen silently.
        let text = describe_failure(&CheckinFailure::ObservationTimeout);
        assert!(text.contains("not retrying"), "{text}");
    }

    #[test]
    fn a_provider_refusal_keeps_the_providers_own_wording() {
        let text = describe_failure(&CheckinFailure::ProviderRejected {
            code: Some("CHECKIN_OFF".into()),
            message: "check-in is closed for maintenance".into(),
        });
        assert!(
            text.contains("check-in is closed for maintenance"),
            "{text}"
        );
    }
}
