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

/// How long the panel is given to act on an activated control before the account
/// is read.
///
/// The click and the grant are separate events: the panel's handler has to run
/// before a reward exists to observe. Without this grace an observation can land
/// in the gap and report `ObservationPending` for a check-in that then succeeds,
/// which is the one wrong answer this design is supposed to make hard to reach.
const POST_TRIGGER_GRACE: std::time::Duration = std::time::Duration::from_millis(1_500);

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
    launch_attempt(&desktop, &provider_id, &account_id).await
}

/// Open a browser for one account and drive one attempt.
///
/// Shared by the command and the scheduler, and the reason it is a plain function
/// rather than living inside the command: a second copy of this sequence would
/// drift, and the drift would be invisible — the manual path would keep working
/// while the automatic one quietly stopped doing what the manual one does. Both
/// callers therefore get the same configuration checks, the same duplicate guard
/// and the same failure messages.
///
/// Returns the view as soon as the browser is up and the page has loaded. The
/// attempt itself continues on a background task.
pub(crate) async fn launch_attempt(
    desktop: &Arc<Desktop>,
    provider_id: &str,
    account_id: &str,
) -> Cmd<CheckinView> {
    let key = SessionKey::new(provider_id, account_id);
    let Some(maintenance) = desktop.maintenance_for(provider_id, account_id) else {
        return Err(format!("no account {account_id} on provider {provider_id}"));
    };
    if !maintenance.is_browser_checkin_configured() {
        // Said as configuration rather than as a runtime failure, because the fix
        // is a line in the config file and not anything the user retries. All
        // three settings are named because "not configured" without saying which
        // one is missing sends the reader hunting through the whole block.
        return Err(format!(
            "check-in is not actionable for {account_id}: set maintenance.checkin_enabled, \
             maintenance.checkin_path, and maintenance.checkin_trigger_selector \
             (got enabled={}, path={}, selector={})",
            maintenance.checkin_enabled,
            maintenance.checkin_path.as_deref().unwrap_or("unset"),
            maintenance
                .checkin_trigger_selector
                .as_deref()
                .unwrap_or("unset"),
        ));
    }
    let path = maintenance
        .checkin_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .expect("is_browser_checkin_configured requires a non-empty path");
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
    let desktop_for_task = Arc::clone(desktop);
    // Resolved before the browser opens, so a missing selector is a configuration
    // error returned to the caller rather than something a background task
    // discovers after a window has already appeared.
    let Some(selector) = maintenance.checkin_selector().map(str::to_string) else {
        release(desktop.checkin.clone(), &key);
        return Err(
            "checkin_trigger_selector is not set for this account, so there is no control \
             to activate"
                .to_string(),
        );
    };
    tauri::async_runtime::spawn(async move {
        drive(runtime, desktop_for_task, task_key, binding, selector).await;
    });

    desktop
        .checkin_view(provider_id, account_id)
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
    // Taken from the live configuration rather than from whatever was declared
    // when the session opened, so an operator who corrected the selector while the
    // challenge was pending gets the corrected one.
    let Some(selector) = desktop
        .maintenance_for(&provider_id, &account_id)
        .and_then(|m| m.checkin_selector().map(str::to_string))
    else {
        return Err(
            "checkin_trigger_selector is not set for this account, so there is no control \
             to activate"
                .to_string(),
        );
    };
    tauri::async_runtime::spawn(async move {
        drive(runtime, desktop_for_task, task_key, binding, selector).await;
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
    // Whether a browser was closed *here* is asked rather than assumed. A cancel
    // that lands while the `drive` task has the browser out cannot close it — that
    // task owns it — so it is recorded and closed when the driver puts it down.
    // Reporting "cancelled" either way would be the same class of lie as a panel
    // claiming a reward nobody observed: the operator would be told the window is
    // gone while it is still on their desktop.
    let outcome = desktop.checkin.finish(&key).await;

    let mut view = desktop
        .checkin_view(&provider_id, &account_id)
        .await
        .ok_or_else(|| format!("no account {account_id} on provider {provider_id}"))?;
    if matches!(outcome, crate::checkin::FinishOutcome::DeferredToDriver) {
        view.browser_held = true;
        view.awaiting_user = false;
        view.phase = "cancelling".to_string();
        return Ok(view);
    }
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
    selector: String,
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

    // Activate the panel's own control, once the page has settled.
    //
    // This is the execution the feature exists for, and it happens here and
    // nowhere else. It is deliberately *not* a request Zroutery sends itself: the
    // page holds the session and the challenge token, so a request built outside
    // it would be the "sign in, take the cookies, post over HTTP" path this
    // design rules out.
    let mut activated = false;
    let mut control_problem: Option<String> = None;
    if clear {
        match browser.click(&binding, &selector).await {
            Ok(cdp::ClickOutcome::Clicked { text }) => {
                activated = true;
                tracing::debug!(
                    provider = %key.provider_id,
                    account = %key.account_id,
                    control = %text,
                    "check-in control activated"
                );
                // The panel needs a moment to act before the account is read: the
                // click and the grant are separate events, and an observation
                // landing in the gap would report `ObservationPending` for a
                // check-in that then succeeded.
                tokio::time::sleep(POST_TRIGGER_GRACE).await;
            }
            Ok(cdp::ClickOutcome::Disabled { text }) => {
                // Neither a failure nor a success. A disabled control is the usual
                // shape of "already done today", and only the observation can turn
                // that into `already_completed` — or into a genuine refusal.
                control_problem = Some(format!(
                    "the check-in control is not active ({text}); the panel may already show \
                     today as checked in"
                ));
            }
            Ok(cdp::ClickOutcome::NotFound) => {
                control_problem = Some(format!(
                    "no check-in control matched {selector:?} on the page; check \
                     maintenance.checkin_trigger_selector"
                ));
            }
            Err(e) => {
                control_problem = Some(format!("the check-in control could not be activated: {e}"));
            }
        }
    }

    let (phase, failure) = if paused {
        let _ = runtime.advance(&key, Event::WafBlocked).await;
        let _ = runtime.advance(&key, Event::AwaitingUser).await;
        ("waiting_for_user", None)
    } else if !clear {
        let _ = runtime.advance(&key, Event::Failed).await;
        (
            "failed",
            Some("the panel did not load before the check-in timed out".to_string()),
        )
    } else if let Some(problem) = control_problem {
        // A control that cannot be activated is something the operator can see and
        // fix, and the message names the setting — except when it merely was
        // inactive, where the observation may still resolve it into "already
        // checked in", which is why the observation runs either way below.
        let _ = runtime.advance(&key, Event::OperationRunning).await;
        let _ = runtime.advance(&key, Event::Failed).await;
        ("failed", Some(problem))
    } else {
        // The control was activated. That is the execution; what it awarded is
        // still unobserved, so the phase stays `running` until the observation
        // half says otherwise.
        ("running", None)
    };

    // **Only a pause keeps the browser.** This used to put it back "either way",
    // on the understanding that the caller would close it once the attempt was
    // cancelled or observed to have finished. There is no such caller: the only
    // paths that closed a browser were an explicit cancel and application
    // shutdown. So every succeeded, failed or already-completed check-in left a
    // Chrome running, and that Chrome holds a lock on this account's profile
    // directory. The next attempt then launched a second `chrome.exe` against a
    // profile already in use, which Chrome answers by handing the command line to
    // the running instance — a new window in the old browser — or by damaging the
    // profile, whichever the timing produced. Both were reported as a corrupted
    // profile or as windows that would not reuse, and neither named the cause.
    //
    // So the disposition is decided here, where the outcome is known, rather than
    // left to a caller that does not exist.
    // Give the key back before deciding where the browser goes, so a cancel that
    // arrived while it was being driven is visible here rather than lost.
    let cancelled = runtime.end_driving(&key).await;

    let browser_held = match browser_disposition(paused && !cancelled) {
        BrowserDisposition::KeepForUser => {
            let _ = runtime.attach(&key, browser).await;
            true
        }
        BrowserDisposition::Close => {
            if let Err(e) = browser.shutdown().await {
                // Surfaced rather than swallowed: a browser that will not close is
                // what corrupts the next attempt's profile, and a silent failure here
                // is exactly how that happened unnoticed.
                tracing::debug!(
                    %e,
                    provider = %key.provider_id,
                    account = %key.account_id,
                    "the finished check-in's browser did not close"
                );
            }
            // The session is released too, so the account is startable again. Leaving
            // it registered would make `begin` refuse the next attempt as a
            // duplicate of an attempt that is over.
            runtime.finish(&key).await;
            false
        }
    };

    let settled = phase.to_string();
    desktop.record_checkin_view(CheckinView {
        provider_id: key.provider_id.clone(),
        account_id: key.account_id.clone(),
        phase: settled,
        browser_held,
        awaiting_user: paused && !cancelled,
        failure,
        reward_amount: None,
        reward_unit: None,
        reward_source: None,
        last_completed_at: None,
        // Overwritten on every read; see `CheckinScheduleView::unknown`.
        schedule: crate::state::CheckinScheduleView::unknown(),
    });

    // Observe whenever the browser reached the panel, whether or not a control was
    // activated. An inactive control is exactly the case where the provider's
    // `checked_in_today` is the only thing that can say what happened, so
    // skipping the observation there would leave the common "already done" outcome
    // unreported. A *paused* attempt is excluded: it has not happened yet, and
    // asking now would read the previous day's reward as this one's.
    let observed = if clear {
        let account = zroutery_core::account::AccountId(key.account_id.clone());
        observe(&desktop, &key, &account, started_at).await
    } else {
        None
    };

    // Record the settlement for the scheduler, here and nowhere else.
    //
    // The rule is "an attempt ended", not "a check-in succeeded", and the
    // difference is what keeps the scheduler from becoming a loop: an account whose
    // check-in fails, or whose observation could not be made, must not be due again
    // on the next tick. A *pause* records nothing, because nothing ended — the human
    // still has a session to resume or cancel, and recording now would let the
    // scheduler treat an unfinished attempt as a finished one.
    //
    // `observed` being `None` after a control was activated means the attempt
    // happened but nothing could be learned about it. That counts as ended: the
    // operation was performed against the panel, and retrying it a minute later
    // would hammer the same endpoint for the same unreadable answer.
    let ended = counts_as_settlement(observed, paused);
    if ended {
        crate::checkin::scheduler::record_settlement(
            &desktop,
            &key.provider_id,
            &key.account_id,
            chrono::Utc::now().timestamp(),
        );
    }

    tracing::debug!(
        provider = %key.provider_id,
        account = %key.account_id,
        activated,
        paused,
        ended,
        "check-in browser phase finished"
    );
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
) -> Option<CheckinPhase> {
    let Some((adapter, quota_per_unit)) = desktop.panel_adapter(&key.provider_id, &key.account_id)
    else {
        tracing::debug!(
            provider = %key.provider_id,
            account = %key.account_id,
            "no stored credential for this account; the browser record stands unconfirmed"
        );
        return None;
    };
    let until = chrono::Utc::now().timestamp();
    let observation = match adapter
        .observe_checkin(account_id, None, None, started_at, until)
        .await
    {
        Ok(observation) => observation,
        Err(e) => {
            tracing::debug!(?e, "check-in observation failed; the browser record stands");
            return None;
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
    let phase = report.phase;

    // The one place a check-in becomes a resource event. It lives here rather than
    // in the scheduler because this is the only place that holds both a finished
    // report and the account it belongs to — and because the conversion is the
    // whole discipline in one call: only a confirmed reward may claim the balance
    // moved, and the weaker answers say so rather than borrowing the strong one.
    //
    // An event that nobody constructs is a type, not a feature, so this line is
    // what makes the observation layer reachable at all.
    if let Some(event) = zroutery_core::account::checkin::as_resource_event(&report) {
        desktop.account_events.record(event);
    }

    desktop.record_checkin_view(view_from_report(&key.provider_id, &key.account_id, &report));
    Some(phase)
}

/// What happens to the browser once an attempt has stopped driving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserDisposition {
    /// A human still needs this window; it must outlive the attempt.
    KeepForUser,
    /// Nothing is driving it any more, so it must be closed and the key released.
    Close,
}

/// Whether the browser stays open after an attempt.
///
/// One bit, and it is extracted for the same reason [`counts_as_settlement`] is: it
/// decides whether a visible window on somebody's desktop is left behind, and
/// getting it wrong is invisible until a profile is damaged. Pure, so it is tested
/// rather than asserted.
fn browser_disposition(paused: bool) -> BrowserDisposition {
    if paused {
        BrowserDisposition::KeepForUser
    } else {
        BrowserDisposition::Close
    }
}

/// Whether an attempt that ended this way counts as settled for the scheduler.
///
/// The rule is "an attempt ended", not "a check-in succeeded", and the difference
/// is what stops the scheduler becoming a loop: an account whose check-in failed, or
/// whose observation could not be made, must not come due again on the next tick.
///
/// A **pause** records nothing, because nothing ended — the human still holds a
/// session to resume or cancel, and recording now would let the scheduler treat an
/// unfinished attempt as a finished one.
///
/// `observed` being `None` after a control was activated means the attempt happened
/// and nothing could be learned about it. That still counts as ended: the operation
/// was performed against the panel, and retrying it a minute later would hammer the
/// same endpoint for the same unreadable answer.
///
/// Pure, and extracted rather than left inline in [`drive`] so it can be tested
/// without a browser. This is a scheduling rule that decides whether a timer opens a
/// window on somebody's machine, which is not a thing to leave asserted only by
/// inspection.
fn counts_as_settlement(observed: Option<CheckinPhase>, paused: bool) -> bool {
    match observed {
        Some(phase) => phase.is_terminal(),
        None => !paused,
    }
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
        // Overwritten on every read; see `CheckinScheduleView::unknown`.
        schedule: crate::state::CheckinScheduleView::unknown(),
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
    #[test]
    fn a_pause_settles_nothing_because_nothing_ended() {
        // The one case that must not record. A pause holds a session for the human;
        // recording now would let the scheduler treat an unfinished attempt as a
        // finished one and leave the operator with a browser nobody is watching.
        assert!(
            !counts_as_settlement(None, true),
            "a paused attempt is not a settlement"
        );
    }

    #[test]
    fn an_attempt_with_nothing_to_learn_from_still_counts_as_ended() {
        // The operation was performed against the panel. Retrying it a minute later
        // hammers the same endpoint for the same unreadable answer, so this counts.
        assert!(counts_as_settlement(None, false));
    }

    #[test]
    fn a_terminal_phase_settles_and_a_waiting_one_does_not() {
        for phase in [
            CheckinPhase::Confirmed,
            CheckinPhase::AlreadyCompleted,
            CheckinPhase::NotSupported,
            CheckinPhase::Failed,
            CheckinPhase::Cancelled,
        ] {
            assert!(
                counts_as_settlement(Some(phase), false),
                "{phase:?} ended the attempt"
            );
        }
        // Neither of these may settle, whatever the pause flag says: an observation
        // that has not concluded has not finished, and a terminal phase is the only
        // thing that overrides the browser's own opinion.
        for phase in [CheckinPhase::BlockedByWaf, CheckinPhase::NeedsUserAction] {
            assert!(
                !counts_as_settlement(Some(phase), false),
                "{phase:?} is waiting on a person, not ended"
            );
            assert!(
                !counts_as_settlement(Some(phase), true),
                "{phase:?} is waiting on a person, not ended"
            );
        }
    }

    #[test]
    fn an_attempt_still_running_has_not_settled() {
        for phase in [
            CheckinPhase::Requested,
            CheckinPhase::Executing,
            CheckinPhase::ProviderAccepted,
            CheckinPhase::ObservationPending,
        ] {
            assert!(
                !counts_as_settlement(Some(phase), false),
                "{phase:?} has not concluded"
            );
        }
    }
    #[test]
    fn only_a_pause_keeps_the_browser_open() {
        // The defect this encodes. Leaving the window open after a finished attempt
        // is what let a stale Chrome hold a profile directory and turn the next
        // attempt into a corrupted profile, so the rule is stated rather than left
        // implicit in the branch that happens to handle a pause.
        assert_eq!(
            browser_disposition(true),
            BrowserDisposition::KeepForUser,
            "a pause exists for the human looking at that window"
        );
        assert_eq!(
            browser_disposition(false),
            BrowserDisposition::Close,
            "a finished attempt has nobody looking at its window"
        );
    }
}
