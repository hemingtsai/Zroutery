//! Capturing a NewAPI panel token with a headed browser.
//!
//! # What this is
//!
//! The account page already treats a panel's personal access token as the
//! account credential. Typing it by hand means signing in to the panel, finding
//! the token page, and copying a value the panel usually shows once. This module
//! opens the panel in the account's own isolated browser profile, lets the
//! operator sign in, asks the page for a durable token, verifies it against
//! `GET /api/user/self`, and stores it in the system credential store.
//!
//! # What crosses the boundary
//!
//! Only the minted personal token, from the page to this module to the keychain.
//! The browser's own credential — an in-memory access token or an HttpOnly
//! refresh cookie — never leaves the page. [`TokenCaptureView`] has no field
//! that could carry a secret, and nothing here logs the token.
//!
//! # One browser per account
//!
//! The browser is held by [`crate::checkin::CheckinRuntime`], under a session
//! whose purpose is [`SessionPurpose::TokenCapture`]. That is the same lock a
//! check-in takes, so a capture and a check-in cannot open two Chromes on one
//! profile, shutdown closes it, and the refusal message names which operation is
//! already open.

pub mod script;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::State;

use zroutery_core::account::adapters::newapi::{NewApiAdapter, NewApiAuth, NewApiConfig};

use crate::checkin::cdp;
use crate::checkin::{SessionKey, SessionPurpose};
use crate::commands::{
    account_secret_target, delete_account_secret, repoint_account_key, write_account_secret,
};
use crate::state::Desktop;

/// How often the page is asked whether it has a credential yet.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long the operator has to complete the panel sign-in.
const DEADLINE: Duration = Duration::from_secs(10 * 60);
/// How many consecutive page errors are tolerated before the capture fails.
///
/// A navigation makes `Runtime.evaluate` fail for a moment; a browser that is
/// gone fails every time. The count separates the two without waiting for the
/// whole deadline.
const MAX_TRANSIENT_ERRORS: u32 = 15;

/// Where a capture is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePhase {
    /// No capture has been started for this account.
    Idle,
    /// The browser is opening.
    Opening,
    /// The browser is open and waiting for a sign-in.
    Waiting,
    /// The panel asked for a verification step in the browser.
    NeedsVerification,
    /// The token is verified and stored; the account declaration still has to
    /// be pointed at it.
    Stored,
    /// The declaration points at the stored token.
    Captured,
    /// The capture could not finish.
    Failed,
    /// The operator cancelled it.
    Cancelled,
    /// The panel offers no durable token this capture can use.
    Unsupported,
}

/// The capture's report to the dashboard.
///
/// Deliberately has no token field: the only place the token lives after the
/// page hands it over is the system credential store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenCaptureView {
    pub provider_id: String,
    pub account_id: String,
    pub phase: CapturePhase,
    pub message: Option<String>,
    pub username: Option<String>,
    /// Whether a browser is open for this account right now.
    pub browser_held: bool,
}

impl TokenCaptureView {
    fn new(provider_id: &str, account_id: &str, phase: CapturePhase) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            account_id: account_id.to_string(),
            phase,
            message: None,
            username: None,
            browser_held: false,
        }
    }

    fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// The reports, keyed by account.
///
/// The browser lives in the check-in registry; this only remembers what the
/// capture concluded, so a finished capture's phase survives the session being
/// removed. It is managed by Tauri rather than held on [`Desktop`] because it
/// outlives nothing and holds nothing that needs closing.
#[derive(Clone, Default)]
pub struct TokenCaptureRuntime {
    reports: Arc<Mutex<BTreeMap<(String, String), TokenCaptureView>>>,
}

impl TokenCaptureRuntime {
    fn key(provider_id: &str, account_id: &str) -> (String, String) {
        (provider_id.to_string(), account_id.to_string())
    }

    fn record(&self, view: TokenCaptureView) {
        let mut reports = self.reports.lock().unwrap_or_else(|p| p.into_inner());
        reports.insert(Self::key(&view.provider_id, &view.account_id), view);
    }

    fn report(&self, provider_id: &str, account_id: &str) -> Option<TokenCaptureView> {
        self.reports
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&Self::key(provider_id, account_id))
            .cloned()
    }

    /// Whether a capture is in a phase that still owns the browser.
    fn is_live(&self, provider_id: &str, account_id: &str) -> bool {
        matches!(
            self.report(provider_id, account_id).map(|view| view.phase),
            Some(CapturePhase::Opening | CapturePhase::Waiting | CapturePhase::NeedsVerification)
        )
    }
}

/// Start a capture for one account in a headed browser.
///
/// Returns as soon as the browser is up; the panel polls [`get_token_capture_status`].
#[tauri::command]
pub async fn start_token_capture(
    desktop: State<'_, Arc<Desktop>>,
    runtime: State<'_, TokenCaptureRuntime>,
    provider_id: String,
    account_id: String,
) -> Result<TokenCaptureView, String> {
    if runtime.is_live(&provider_id, &account_id) {
        return Err(format!(
            "a token capture is already open for account `{account_id}`; finish or cancel it first"
        ));
    }
    // The dashboard root: the account's own override when its panel lives on a
    // different host than the relay endpoint, the endpoint-derived root when it
    // does not. Every step below — the page the browser opens, the origin the
    // probe checks against, the verification the stored token must pass — is
    // anchored to this one root, so a relay like AgentRouter (relay on
    // ps.air-outer.com, sign-in on agentrouter.org) is one declaration away
    // from working rather than one assumption from failing.
    let (panel_root, login_path, browser_executable) = {
        let config = desktop.core.config();
        let provider = config
            .provider(&provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let account = provider
            .accounts
            .iter()
            .find(|account| account.account_id == account_id)
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        let base_url = provider.base_url.clone();
        let panel_root = account
            .maintenance
            .panel_override()
            .map(str::to_string)
            .unwrap_or_else(|| script::panel_root(&base_url));
        (
            panel_root,
            account.maintenance.login_path.clone(),
            account.maintenance.browser_executable.clone(),
        )
    };

    let url = script::login_url_at(&panel_root, login_path.as_deref())?;
    let origin = script::origin_of(&url)
        .ok_or_else(|| format!("`{url}` has no origin a page could be checked against"))?;
    let Some(executable) = cdp::find_browser(&browser_executable) else {
        return Err(
            "no Chromium browser was found; set one in the account's maintenance settings".into(),
        );
    };

    let key = SessionKey::new(provider_id.as_str(), account_id.as_str());
    // The same lock a check-in takes. A failure here is the refusal message.
    desktop
        .checkin
        .begin_for(key.clone(), SessionPurpose::TokenCapture)
        .await
        .map_err(|error| error.to_string())?;
    runtime.record(TokenCaptureView::new(
        &provider_id,
        &account_id,
        CapturePhase::Opening,
    ));

    let profile = desktop.checkin.profile_dir(&key);
    let mut browser = match cdp::launch(&executable, &profile, &url).await {
        Ok(browser) => browser,
        Err(error) => {
            finish_session(&desktop, &key).await;
            let message = error.to_string();
            runtime.record(
                TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
                    .with_message(message.clone()),
            );
            return Err(message);
        }
    };
    let page = match browser.open_page(&url).await {
        Ok(page) => page,
        Err(error) => {
            let _ = browser.shutdown().await;
            finish_session(&desktop, &key).await;
            let message = error.to_string();
            runtime.record(
                TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
                    .with_message(message.clone()),
            );
            return Err(message);
        }
    };
    if let Err(error) = desktop.checkin.attach(&key, browser).await {
        finish_session(&desktop, &key).await;
        return Err(error.to_string());
    }

    runtime.record(TokenCaptureView::new(
        &provider_id,
        &account_id,
        CapturePhase::Waiting,
    ));
    let runtime_task = runtime.inner().clone();
    let desktop_task = Arc::clone(desktop.inner());
    tauri::async_runtime::spawn(async move {
        drive(desktop_task, runtime_task, key, page, origin, panel_root).await;
    });

    Ok(status_view(&desktop, &runtime, &provider_id, &account_id).await)
}

/// What the capture for one account concluded, if anything.
#[tauri::command]
pub async fn get_token_capture_status(
    desktop: State<'_, Arc<Desktop>>,
    runtime: State<'_, TokenCaptureRuntime>,
    provider_id: String,
    account_id: String,
) -> Result<TokenCaptureView, String> {
    Ok(status_view(&desktop, &runtime, &provider_id, &account_id).await)
}

/// Ask the driver to close the browser and stop.
#[tauri::command]
pub async fn cancel_token_capture(
    desktop: State<'_, Arc<Desktop>>,
    runtime: State<'_, TokenCaptureRuntime>,
    provider_id: String,
    account_id: String,
) -> Result<TokenCaptureView, String> {
    let key = SessionKey::new(provider_id.as_str(), account_id.as_str());
    // `finish` closes a browser the driver is not using, and records a cancel
    // for one that is. The driver observes it between polls.
    desktop.checkin.finish(&key).await;
    if !runtime.is_live(&provider_id, &account_id) {
        return Ok(status_view(&desktop, &runtime, &provider_id, &account_id).await);
    }
    runtime.record(
        TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Cancelled)
            .with_message("the capture was cancelled"),
    );
    Ok(status_view(&desktop, &runtime, &provider_id, &account_id).await)
}

/// Point the account declaration at the token the capture stored.
///
/// Split from the driver so a configuration save the operator had in flight
/// cannot be clobbered while a browser is still open: the driver stores the
/// secret and reports `stored`; this reads the configuration as it is now and
/// repoints the declaration only if it still shares the provider's key.
#[tauri::command]
pub async fn complete_token_capture(
    desktop: State<'_, Arc<Desktop>>,
    runtime: State<'_, TokenCaptureRuntime>,
    provider_id: String,
    account_id: String,
) -> Result<TokenCaptureView, String> {
    let report = runtime
        .report(&provider_id, &account_id)
        .ok_or_else(|| format!("no token capture was started for `{account_id}`"))?;
    if report.phase != CapturePhase::Stored {
        return Err(format!(
            "the capture is in phase `{:?}`, so there is no stored token to use",
            report.phase
        ));
    }
    let (key_ref, repoint) =
        account_secret_target(&desktop.core.config(), &provider_id, &account_id)?;
    if repoint {
        if let Err(error) = repoint_account_key(&desktop, &provider_id, &account_id, &key_ref).await
        {
            // The secret was written by the driver; a configuration that refused
            // it must not leave a credential nobody points at. The derived
            // reference is the one this path writes, and the account shared the
            // provider's key before, so there is no earlier value to restore.
            let _ = delete_account_secret(&desktop, &key_ref).await;
            runtime.record(
                TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
                    .with_message(format!(
                        "the account could not be pointed at the token: {error}"
                    )),
            );
            return Err(error);
        }
    }
    runtime.record(TokenCaptureView {
        provider_id: provider_id.clone(),
        account_id: account_id.clone(),
        phase: CapturePhase::Captured,
        message: Some("the panel token is stored as this account's credential".into()),
        username: report.username,
        browser_held: false,
    });
    Ok(status_view(&desktop, &runtime, &provider_id, &account_id).await)
}

async fn status_view(
    desktop: &Desktop,
    runtime: &TokenCaptureRuntime,
    provider_id: &str,
    account_id: &str,
) -> TokenCaptureView {
    let key = SessionKey::new(provider_id, account_id);
    let mut view = runtime
        .report(provider_id, account_id)
        .unwrap_or_else(|| TokenCaptureView::new(provider_id, account_id, CapturePhase::Idle));
    view.browser_held = desktop.checkin.holds_browser(&key).await;
    view
}

async fn finish_session(desktop: &Desktop, key: &SessionKey) {
    desktop.checkin.end_driving(key).await;
    desktop.checkin.finish(key).await;
}

/// Hold the browser, poll the page, and verify and store what it hands back.
async fn drive(
    desktop: Arc<Desktop>,
    runtime: TokenCaptureRuntime,
    key: SessionKey,
    page: cdp::PageBinding,
    origin: String,
    panel_root: String,
) {
    let provider_id = key.provider_id.clone();
    let account_id = key.account_id.clone();
    // A cancel that arrived between `attach` and the task starting has already
    // removed the browser; there is nothing left to poll.
    let Some(mut browser) = desktop.checkin.take_browser(&key).await else {
        runtime.record(
            TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Cancelled)
                .with_message("the capture was cancelled before it started"),
        );
        return;
    };

    let deadline = Instant::now() + DEADLINE;
    let mut transient = 0u32;
    let mut final_view = None;

    while Instant::now() < deadline {
        if desktop.checkin.is_cancel_requested(&key).await {
            final_view = Some(
                TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Cancelled)
                    .with_message("the capture was cancelled"),
            );
            break;
        }
        let expression = script::capture_expression(&origin);
        match browser.evaluate(&page, &expression).await {
            Ok(value) => match script::parse(&value) {
                script::CaptureResult::Waiting => {
                    transient = 0;
                    runtime.record(TokenCaptureView::new(
                        &provider_id,
                        &account_id,
                        CapturePhase::Waiting,
                    ));
                }
                script::CaptureResult::NeedsVerification { message } => {
                    transient = 0;
                    runtime.record(TokenCaptureView {
                        provider_id: provider_id.clone(),
                        account_id: account_id.clone(),
                        phase: CapturePhase::NeedsVerification,
                        message: message.or_else(|| {
                            Some(
                                "finish the verification in the browser; the capture resumes"
                                    .into(),
                            )
                        }),
                        username: None,
                        browser_held: true,
                    });
                }
                script::CaptureResult::Ready {
                    token,
                    username,
                    user_id,
                    source,
                } => {
                    final_view = Some(
                        store_token(
                            &desktop,
                            &provider_id,
                            &account_id,
                            CapturedToken {
                                token,
                                username,
                                user_id,
                                source,
                                panel_root: &panel_root,
                            },
                        )
                        .await,
                    );
                    break;
                }
                script::CaptureResult::Unsupported { message } => {
                    final_view = Some(
                        TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Unsupported)
                            .with_message(message),
                    );
                    break;
                }
                script::CaptureResult::Error { message } => {
                    final_view = Some(
                        TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
                            .with_message(message),
                    );
                    break;
                }
            },
            Err(cdp::BrowserError::Exited) => {
                final_view = Some(
                    TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
                        .with_message("the browser was closed before a token was captured"),
                );
                break;
            }
            Err(error) => {
                transient += 1;
                if transient > MAX_TRANSIENT_ERRORS {
                    final_view = Some(
                        TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
                            .with_message(format!("the panel page could not be read: {error}")),
                    );
                    break;
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let mut view = final_view.unwrap_or_else(|| {
        TokenCaptureView::new(&provider_id, &account_id, CapturePhase::Failed)
            .with_message("no token was found before the ten-minute limit; the browser was closed")
    });
    view.browser_held = false;
    let _ = browser.shutdown().await;
    desktop.checkin.end_driving(&key).await;
    desktop.checkin.finish(&key).await;
    runtime.record(view);
}

/// Verify the panel's token, then store it.
///
/// The panel root is the capture session's own — the override the page was
/// opened against — because the mint happened there and the verification has
/// to ask the same host what the token is worth. Deriving the root from the
/// relay endpoint would verify a token from one host against another, and a
/// relay whose dashboard lives on a second domain would refuse every token it
/// had just handed out.
/// What a capture session handed back.
///
/// Grouped so the store step names the handful of facts it needs without an
/// eight-argument signature, and so a caller cannot mix up the token with the
/// user id or the source label.
struct CapturedToken<'a> {
    token: String,
    username: Option<String>,
    user_id: Option<String>,
    /// Where the value was read from, for the report the user reads.
    source: String,
    /// The panel root the page was opened against; verification asks the same
    /// host, never the relay endpoint.
    panel_root: &'a str,
}

async fn store_token(
    desktop: &Desktop,
    provider_id: &str,
    account_id: &str,
    captured: CapturedToken<'_>,
) -> TokenCaptureView {
    let CapturedToken {
        token,
        username,
        user_id,
        source,
        panel_root,
    } = captured;
    let view = |phase, message: Option<String>, username| TokenCaptureView {
        provider_id: provider_id.to_string(),
        account_id: account_id.to_string(),
        phase,
        message,
        username,
        browser_held: true,
    };

    let (key_ref, _repoint) =
        match account_secret_target(&desktop.core.config(), provider_id, account_id) {
            Ok(target) => target,
            Err(error) => return view(CapturePhase::Failed, Some(error), None),
        };
    // The provider still has to exist — the capture holds no other reason to
    // touch the relay endpoint, so the check is about the config having moved
    // under the session rather than about a url nobody reads.
    if desktop.core.config().provider(provider_id).is_none() {
        return view(
            CapturePhase::Failed,
            Some(format!("unknown provider `{provider_id}`")),
            None,
        );
    }
    let verified = match verify_token(panel_root, &token, user_id.as_deref()).await {
        Ok(username) => username,
        Err(error) => {
            /*
             * A *network* failure to re-verify is not a rejection. The probe
             * verified this token inside the browser minutes ago — same panel,
             * same header, a 200 the panel itself issued — and the browser
             * reaches the panel through the operator's proxy, which the
             * desktop process may not share (AgentRouter is only reachable
             * through one here). Refusing to store a token the panel just
             * confirmed, because this process cannot dial the panel, would
             * make the capture unusable behind every proxy setup that splits
             * browser and app traffic. An *authorization* failure is still a
             * refusal: that is the panel speaking, not the network.
             */
            let unreachable = error.contains("cannot reach upstream");
            if !unreachable {
                return view(
                    CapturePhase::Failed,
                    Some(format!("the panel rejected the captured token: {error}")),
                    None,
                );
            }
            tracing::warn!(
                provider = provider_id,
                "the panel is unreachable from this process; storing the token the browser verified"
            );
            None
        }
    };
    if let Err(error) = write_account_secret(desktop, &key_ref, &token).await {
        return view(CapturePhase::Failed, Some(error), None);
    }
    /*
     * Remember the panel user id beside the credential it names. Every later
     * probe, check-in and usage read goes through the same middleware that
     * just demanded the header; without this the capture succeeds once and
     * the very next probe reports the credential it stored as rejected. A
     * failure to persist is logged and survived — the token itself is stored,
     * and the id can be typed into the maintenance settings by hand.
     */
    if let Some(id) = user_id.as_deref().filter(|id| !id.trim().is_empty()) {
        let mut config = (*desktop.core.config()).clone();
        let declaration = config
            .providers
            .iter_mut()
            .find(|p| p.id == provider_id)
            .and_then(|p| p.accounts.iter_mut().find(|a| a.account_id == account_id));
        if let Some(account) = declaration {
            if account.maintenance.new_api_user.as_deref() != Some(id) {
                account.maintenance.new_api_user = Some(id.to_string());
                if let Err(error) = desktop.apply_config(config).await {
                    tracing::warn!(
                        provider = provider_id,
                        account = account_id,
                        error = %error,
                        "the panel user id could not be persisted; set it in the maintenance settings"
                    );
                }
            }
        }
    }
    view(
        CapturePhase::Stored,
        Some(format!(
            "the token from `{source}` is verified and stored; finish to use it"
        )),
        verified.or(username),
    )
}

/// Ask the panel who the token belongs to.
async fn verify_token(
    base_url: &str,
    token: &str,
    user_id: Option<&str>,
) -> Result<Option<String>, String> {
    let base = script::panel_root(base_url);
    let adapter = NewApiAdapter::new(NewApiConfig {
        base_url: base,
        api_key: token.to_string(),
        new_api_user: user_id.map(str::to_string),
        ..NewApiConfig::default()
    })
    .map_err(|error| error.to_string())?;
    let session = adapter
        .authenticate(NewApiAuth::ApiKey {
            key: token.to_string(),
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(session.username)
}

#[cfg(test)]
mod tests;
