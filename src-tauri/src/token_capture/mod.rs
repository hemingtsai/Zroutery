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
    let (base_url, login_path, browser_executable) = {
        let config = desktop.core.config();
        let provider = config
            .provider(&provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        let account = provider
            .accounts
            .iter()
            .find(|account| account.account_id == account_id)
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        (
            provider.base_url.clone(),
            account.maintenance.login_path.clone(),
            account.maintenance.browser_executable.clone(),
        )
    };

    let url = script::login_url(&base_url, login_path.as_deref())?;
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
        drive(desktop_task, runtime_task, key, page, origin).await;
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
                    source,
                } => {
                    final_view = Some(
                        store_token(
                            &desktop,
                            &provider_id,
                            &account_id,
                            &token,
                            username,
                            &source,
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
async fn store_token(
    desktop: &Desktop,
    provider_id: &str,
    account_id: &str,
    token: &str,
    username: Option<String>,
    source: &str,
) -> TokenCaptureView {
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
    let base_url = match desktop.core.config().provider(provider_id) {
        Some(provider) => provider.base_url.clone(),
        None => {
            return view(
                CapturePhase::Failed,
                Some(format!("unknown provider `{provider_id}`")),
                None,
            )
        }
    };
    let verified = match verify_token(&base_url, token).await {
        Ok(username) => username,
        Err(error) => {
            return view(
                CapturePhase::Failed,
                Some(format!("the panel rejected the captured token: {error}")),
                None,
            )
        }
    };
    if let Err(error) = write_account_secret(desktop, &key_ref, token).await {
        return view(CapturePhase::Failed, Some(error), None);
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
async fn verify_token(base_url: &str, token: &str) -> Result<Option<String>, String> {
    let base = script::panel_root(base_url);
    let adapter = NewApiAdapter::new(NewApiConfig {
        base_url: base,
        api_key: token.to_string(),
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
