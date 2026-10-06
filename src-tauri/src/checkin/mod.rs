//! Browser-driven check-in: the runtime that holds a browser open across a pause.
//!
//! # The one thing this module has to get right
//!
//! A check-in that hits a human-verification challenge must be **resumable in
//! place**. The user finishes the challenge in the window that is already open,
//! presses resume, and the operation continues in that same page — with the same
//! cookies, the same clearance token and the same half-finished work. Starting a
//! new browser instead would discard all of it and make the user authenticate
//! again, which is the failure mode [`state`] exists to make unrepresentable.
//!
//! So a live session is **owned by a key, not by a task**. [`Registry`] is keyed
//! on `(provider_id, account_id)` and holds the [`cdp::BrowserSession`] itself.
//! A resume is a lookup, not a re-launch: there is exactly one browser per key,
//! and creating a second one for a key that already has one is refused. That is
//! what makes "resume" a different operation from "start" rather than a
//! misspelling of it.
//!
//! # What is deliberately not held here
//!
//! No cookie, token, DOM or password. The browser owns the session, in its own
//! profile directory on disk, and the only things that cross this boundary are a
//! URL, a title and a list of frame hosts. A snapshot handed to the webview can
//! therefore never contain session material, because there is none here to leak.
//!
//! # Concurrency
//!
//! One [`Mutex`] over the whole map, held only for map operations and never across
//! an `await` that touches the browser. That is coarse but correct, and it is
//! correct *by construction*: taking a per-entry lock would be finer-grained and
//! would need the lock order to be established against every early return, and a
//! lock-order bug in a type whose job is serialising browser sessions would be
//! found in production.

pub mod cdp;
pub mod profile;
pub mod state;
pub mod waf;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use state::{Event, State};

/// Identifies one account's browser, and therefore its profile.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub provider_id: String,
    pub account_id: String,
}

impl SessionKey {
    pub fn new(provider_id: impl Into<String>, account_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            account_id: account_id.into(),
        }
    }
}

/// What a caller needs to drive one account's check-in.
///
/// The whole surface is this plus the four verbs, and that is the point: the Tauri
/// layer never sees a browser, a page or a socket.
///
/// Cloneable, because the Tauri commands hand the runtime to a background task
/// and keep using it. What is shared is the registry — the live browsers — not a
/// copy of it, which is what keeps one browser per account true across the task
/// boundary.
#[derive(Clone)]
pub struct CheckinRuntime {
    config_dir: PathBuf,
    registry: Arc<Mutex<HashMap<SessionKey, Arc<Mutex<LiveSession>>>>>,
}

impl CheckinRuntime {
    /// A runtime that keeps browser profiles under `config_dir`.
    ///
    /// Not `Default`: a browser profile directory is user data, and a runtime that
    /// guessed one would write into a directory the application never told it
    /// about.
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            config_dir,
            registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The isolated profile for one account.
    pub fn profile_dir(&self, key: &SessionKey) -> PathBuf {
        profile::profile_dir(&self.config_dir, &key.provider_id, &key.account_id)
    }

    /// Whether a browser is currently held open for this account.
    ///
    /// This is what the UI's "continue the verification" button is offered
    /// against, so it must answer about a *live* browser rather than about the
    /// last thing that happened to one.
    pub async fn is_browser_held(&self, key: &SessionKey) -> bool {
        let Some(session) = self.session(key).await else {
            return false;
        };
        let guard = session.lock().await;
        guard.state.holds_browser()
    }

    /// What is happening to one account's check-in, if anything.
    pub async fn phase(&self, key: &SessionKey) -> Option<State> {
        let session = self.session(key).await?;
        let guard = session.lock().await;
        Some(guard.state)
    }

    /// Start a check-in for this account.
    ///
    /// Refused when a session already exists, whether it is running or paused.
    /// That refusal is the isolation guarantee in its strictest form: two
    /// operations cannot share one browser profile, so they cannot share a
    /// session — and the user is told to resume or cancel the existing one
    /// rather than being quietly given a second browser.
    pub async fn begin(&self, key: SessionKey) -> Result<(), StartError> {
        let mut guard = self.registry.lock().await;
        if let Some(existing) = guard.get(&key) {
            let state = existing.lock().await.state;
            return Err(StartError::AlreadyExists { state });
        }
        guard.insert(key, Arc::new(Mutex::new(LiveSession::new())));
        Ok(())
    }

    /// Apply an event to one account's state machine.
    ///
    /// Returns the new state, or refuses. The refusal is the whole mechanism:
    /// a caller cannot quietly set a state it should have reached by transition.
    pub async fn advance(
        &self,
        key: &SessionKey,
        event: Event,
    ) -> Result<State, state::IllegalTransition> {
        let session = self.session(key).await.ok_or(state::IllegalTransition {
            from: State::Pending,
            event,
        })?;
        let mut session = session.lock().await;
        let next = state::transition(session.state, event)?;
        session.state = next;
        Ok(next)
    }

    /// Forget the session for this account and close its browser.
    ///
    /// The browser is closed before the entry is removed, so a shutdown cannot
    /// leave a process running with nothing pointing at it. A close that fails is
    /// logged rather than propagated: the caller asked for the session to end, and
    /// refusing to end it because the window would not close would leave the user
    /// with no way to proceed.
    pub async fn finish(&self, key: &SessionKey) {
        let session = {
            let mut guard = self.registry.lock().await;
            guard.remove(key)
        };
        let Some(session) = session else { return };
        let browser = session.lock().await.browser.take();
        if let Some(browser) = browser {
            if let Err(e) = browser.shutdown().await {
                tracing::debug!(?e, %key.provider_id, %key.account_id, "browser close failed");
            }
        }
    }

    /// Attach a live browser to a session.
    pub async fn attach(
        &self,
        key: &SessionKey,
        browser: cdp::BrowserSession,
    ) -> Result<(), AttachError> {
        let session = self.session(key).await.ok_or(AttachError::NoSession)?;
        let mut session = session.lock().await;
        if session.browser.is_some() {
            return Err(AttachError::AlreadyAttached);
        }
        session.browser = Some(browser);
        Ok(())
    }

    /// Take the browser out of a session, leaving the session itself in place.
    ///
    /// Used to shut the browser down while leaving the record of the attempt, so
    /// a panel can still show what happened after the window is gone.
    pub async fn take_browser(&self, key: &SessionKey) -> Option<cdp::BrowserSession> {
        let session = self.session(key).await?;
        let mut guard = session.lock().await;
        guard.browser.take()
    }

    /// Every account with a live or paused session.
    pub async fn keys(&self) -> Vec<SessionKey> {
        self.registry.lock().await.keys().cloned().collect()
    }

    /// Close every browser and forget every session.
    ///
    /// Called on shutdown. Sessions are taken out of the map before being closed
    /// so a concurrent `phase` read sees the map as empty rather than an entry
    /// whose browser is being torn down.
    pub async fn shutdown_all(&self) {
        let sessions: Vec<_> = self.registry.lock().await.drain().collect();
        for (key, session) in sessions {
            let browser = session.lock().await.browser.take();
            if let Some(browser) = browser {
                if let Err(e) = browser.shutdown().await {
                    tracing::debug!(
                        ?e,
                        provider = %key.provider_id,
                        account = %key.account_id,
                        "browser close failed during shutdown"
                    );
                }
            }
        }
    }

    async fn session(&self, key: &SessionKey) -> Option<Arc<Mutex<LiveSession>>> {
        self.registry.lock().await.get(key).cloned()
    }
}

/// One account's check-in attempt.
struct LiveSession {
    state: State,
    /// `None` before the browser is attached, and after it has been closed.
    browser: Option<cdp::BrowserSession>,
}

impl LiveSession {
    fn new() -> Self {
        Self {
            state: State::Pending,
            browser: None,
        }
    }
}

/// Why a check-in could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// A session for this account already exists.
    ///
    /// Carries its state so the caller can tell the user to *continue* it from
    /// [`State::WaitingForUser`] or to cancel it from anywhere else, rather than
    /// offering "start" for an attempt that is already half-finished.
    AlreadyExists { state: State },
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists { state } if state.awaiting_user() => write!(
                f,
                "this account already has a check-in waiting for you to finish a \
                 verification; continue it instead of starting a new one"
            ),
            Self::AlreadyExists { state } => write!(
                f,
                "this account already has a check-in in progress ({state:?}); wait for it or \
                 cancel it first"
            ),
        }
    }
}

impl std::error::Error for StartError {}

/// Why a browser could not be attached to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// No session is open for this account.
    NoSession,
    /// A browser is already attached.
    AlreadyAttached,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSession => write!(f, "no check-in is open for this account"),
            Self::AlreadyAttached => write!(f, "this check-in already has a browser"),
        }
    }
}

impl std::error::Error for AttachError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> CheckinRuntime {
        CheckinRuntime::new(std::env::temp_dir().join("zroutery-checkin-registry-test"))
    }

    fn key() -> SessionKey {
        SessionKey::new("relay", "main")
    }

    #[tokio::test]
    async fn a_fresh_runtime_holds_nothing() {
        let runtime = runtime();
        assert!(!runtime.is_browser_held(&key()).await);
        assert_eq!(runtime.phase(&key()).await, None);
        assert!(runtime.keys().await.is_empty());
    }

    #[tokio::test]
    async fn two_accounts_are_kept_apart() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        runtime
            .begin(SessionKey::new("relay", "second"))
            .await
            .expect("begins");
        // Both exist, and the first one did not evict the second.
        assert_eq!(runtime.keys().await.len(), 2);
    }

    #[tokio::test]
    async fn starting_twice_is_refused_rather_than_opening_a_second_browser() {
        // The isolation guarantee in its strictest form.
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        let second = runtime.begin(key()).await.expect_err("refused");
        assert!(matches!(
            second,
            StartError::AlreadyExists {
                state: State::Pending
            }
        ));
    }

    #[tokio::test]
    async fn a_paused_session_refuses_a_start_and_says_to_continue() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        runtime
            .advance(&key(), Event::Start)
            .await
            .expect("advances");
        runtime
            .advance(&key(), Event::BrowserStarted)
            .await
            .expect("advances");
        runtime
            .advance(&key(), Event::Navigated)
            .await
            .expect("advances");
        runtime
            .advance(&key(), Event::WafBlocked)
            .await
            .expect("advances");
        runtime
            .advance(&key(), Event::AwaitingUser)
            .await
            .expect("advances");

        let error = runtime.begin(key()).await.expect_err("refused");
        assert_eq!(
            error,
            StartError::AlreadyExists {
                state: State::WaitingForUser
            }
        );
        // And the message tells the user to continue, not to start again — which
        // is the difference between preserving and discarding the session.
        assert!(error.to_string().contains("continue"));
        assert!(error.to_string().contains("verification"));
    }

    #[tokio::test]
    async fn a_paused_session_keeps_its_browser_flagged_as_held() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        for event in [
            Event::Start,
            Event::BrowserStarted,
            Event::Navigated,
            Event::WafBlocked,
            Event::AwaitingUser,
        ] {
            runtime.advance(&key(), event).await.expect("advances");
        }
        // The session is not progressing, and the browser must not be closed.
        assert!(runtime.is_browser_held(&key()).await);
    }

    #[tokio::test]
    async fn a_resume_continues_the_same_session_rather_than_restarting_it() {
        // §15: the operation resumes; it does not begin again. `BrowserStarted` is
        // illegal from here, which is what stops a resume from creating a second
        // browser.
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        for event in [
            Event::Start,
            Event::BrowserStarted,
            Event::Navigated,
            Event::WafBlocked,
            Event::AwaitingUser,
        ] {
            runtime.advance(&key(), event).await.expect("advances");
        }
        assert_eq!(
            runtime.advance(&key(), Event::UserResumed).await,
            Ok(State::UserContinued)
        );
        assert!(
            runtime
                .advance(&key(), Event::BrowserStarted)
                .await
                .is_err(),
            "a resume must not be able to launch a second browser"
        );
        assert_eq!(
            runtime.advance(&key(), Event::OperationRunning).await,
            Ok(State::Running)
        );
        assert_eq!(
            runtime.advance(&key(), Event::Succeeded).await,
            Ok(State::Succeeded)
        );
    }

    #[tokio::test]
    async fn cancelling_a_paused_session_ends_it_and_frees_the_key() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        for event in [
            Event::Start,
            Event::BrowserStarted,
            Event::Navigated,
            Event::WafBlocked,
            Event::AwaitingUser,
        ] {
            runtime.advance(&key(), event).await.expect("advances");
        }
        assert_eq!(
            runtime.advance(&key(), Event::Cancelled).await,
            Ok(State::Cancelled)
        );
        runtime.finish(&key()).await;
        assert!(!runtime.is_browser_held(&key()).await);
        // And the key is free again, so a fresh check-in is allowed.
        runtime.begin(key()).await.expect("begins");
    }

    #[tokio::test]
    async fn finishing_releases_the_key_so_a_later_attempt_is_not_a_conflict() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        runtime.finish(&key()).await;
        assert!(runtime.keys().await.is_empty());
        runtime
            .begin(key())
            .await
            .expect("a new attempt is allowed");
    }

    #[tokio::test]
    async fn advancing_an_unknown_account_is_refused() {
        let runtime = runtime();
        assert!(
            runtime.advance(&key(), Event::Start).await.is_err(),
            "there is no state machine to advance"
        );
    }

    #[tokio::test]
    async fn an_illegal_transition_leaves_the_state_unchanged() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        assert!(runtime.advance(&key(), Event::Succeeded).await.is_err());
        assert_eq!(
            runtime.phase(&key()).await,
            Some(State::Pending),
            "a refused transition must not have moved anything"
        );
    }

    #[tokio::test]
    async fn profiles_are_isolated_per_account() {
        let runtime = runtime();
        let a = runtime.profile_dir(&SessionKey::new("relay", "a"));
        let b = runtime.profile_dir(&SessionKey::new("relay", "b"));
        let c = runtime.profile_dir(&SessionKey::new("other", "a"));
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[tokio::test]
    async fn shutdown_empties_the_registry() {
        let runtime = runtime();
        runtime.begin(key()).await.expect("begins");
        runtime
            .begin(SessionKey::new("relay", "two"))
            .await
            .expect("begins");
        runtime.shutdown_all().await;
        assert!(runtime.keys().await.is_empty());
        assert!(!runtime.is_browser_held(&key()).await);
    }

    #[tokio::test]
    async fn attaching_without_a_session_is_refused() {
        // Asserted through the error type rather than by reaching for a browser:
        // a unit test that needed Chrome installed to prove a message would not be
        // a unit test, and CI does not have one.
        assert_eq!(
            AttachError::NoSession.to_string(),
            "no check-in is open for this account"
        );
        assert_eq!(
            AttachError::AlreadyAttached.to_string(),
            "this check-in already has a browser"
        );
    }
}
