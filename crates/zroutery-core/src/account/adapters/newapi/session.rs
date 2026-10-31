//! The rotating refresh cookie, and the single-flight guard that keeps a login from being overwritten by a refresh that started earlier.

use std::sync::atomic::Ordering;

use crate::error::{Error, Result};

use super::helpers::*;
use super::wire::*;
use super::NewApiAdapter;
use super::PROVIDER_ID;

impl NewApiAdapter {
    /// Exchange a `new_api_refresh` cookie for a dashboard access token.
    pub(super) async fn session_from_cookie(&self, raw: &str) -> Result<Session> {
        let cookie = cookie_value(raw);
        validate_cookie(&cookie).map_err(Error::invalid)?;

        let attempt = self
            .attempt(
                reqwest::Method::POST,
                "/api/user/auth/refresh",
                &[],
                None,
                Some(&cookie),
            )
            .await
            .map_err(NewApiError::into_core)?;
        let login: LoginData = serde_json::from_value(attempt.data).map_err(|e| {
            Error::BadUpstreamPayload(format!("{PROVIDER_ID} /api/user/auth/refresh payload: {e}"))
        })?;
        if login.access_token.trim().is_empty() {
            return Err(Error::BadUpstreamPayload(
                "newapi auth refresh returned no access_token".into(),
            ));
        }

        let user = login.user.unwrap_or_default();
        Ok(Session {
            bearer: login.access_token,
            // NewAPI rotates the refresh cookie on every refresh; keep the new
            // one so the next renewal still works.
            refresh_cookie: Some(attempt.refresh_cookie.unwrap_or(cookie)),
            user_id: user_id_of(&user),
            username: username_of(&user),
            group: user.group.clone(),
            expires_at: login.access_expires_at,
        })
    }

    /// Verify a bearer credential against `GET /api/user/self`.
    pub(super) async fn verified_session(
        &self,
        bearer: &str,
        refresh_cookie: Option<String>,
    ) -> Result<Session> {
        let attempt = self
            .attempt(
                reqwest::Method::GET,
                "/api/user/self",
                &[],
                Some(bearer),
                None,
            )
            .await
            .map_err(NewApiError::into_core)?;
        let user: SelfData = serde_json::from_value(attempt.data).map_err(|e| {
            Error::BadUpstreamPayload(format!("{PROVIDER_ID} /api/user/self payload: {e}"))
        })?;

        Ok(Session {
            bearer: bearer.to_string(),
            refresh_cookie,
            user_id: user_id_of(&user),
            username: username_of(&user),
            group: user.group.clone(),
            expires_at: None,
        })
    }

    /// Renew an expired access token from the stored refresh cookie.
    pub(super) async fn renew_session(
        &self,
        session: &Session,
    ) -> std::result::Result<Session, NewApiError> {
        let cookie = session
            .refresh_cookie
            .as_deref()
            .ok_or(NewApiError::NotAuthenticated)?;
        let attempt = self
            .attempt(
                reqwest::Method::POST,
                "/api/user/auth/refresh",
                &[],
                None,
                Some(cookie),
            )
            .await?;
        let login: LoginData = serde_json::from_value(attempt.data).map_err(|e| {
            NewApiError::Payload(format!("{PROVIDER_ID} /api/user/auth/refresh payload: {e}"))
        })?;
        if login.access_token.trim().is_empty() {
            return Err(NewApiError::Payload(
                "newapi auth refresh returned no access_token".into(),
            ));
        }
        let user = login.user.unwrap_or_default();
        Ok(Session {
            bearer: login.access_token,
            refresh_cookie: Some(attempt.refresh_cookie.unwrap_or_else(|| cookie.to_string())),
            user_id: user_id_of(&user).or_else(|| session.user_id.clone()),
            username: username_of(&user).or_else(|| session.username.clone()),
            group: user.group.clone().or_else(|| session.group.clone()),
            expires_at: login.access_expires_at,
        })
    }

    /// Install `session` as the current credential, at a new generation.
    pub(super) fn install_session(&self, session: Session) -> InstalledSession {
        let mut guard = self
            .session
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let generation = self.session_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let installed = InstalledSession {
            session,
            generation,
        };
        *guard = Some(installed.clone());
        installed
    }

    /// Install a refreshed `session`, but only if the credential is still the
    /// one the refresh started from.
    ///
    /// A refresh takes time, and during it another task may have installed a
    /// newer session — a second refresh, or an explicit `authenticate` with a
    /// credential the user just supplied. Overwriting that with the older
    /// refresh result would log the user back out, so the newer session wins and
    /// the stale result is dropped. Returns whatever is current afterwards.
    pub(super) fn install_session_if_current(
        &self,
        expected: u64,
        session: Session,
    ) -> InstalledSession {
        let mut guard = self
            .session
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(current) = guard.as_ref() {
            if current.generation != expected {
                return current.clone();
            }
        }
        let generation = self.session_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let installed = InstalledSession {
            session,
            generation,
        };
        *guard = Some(installed.clone());
        installed
    }

    pub(super) fn session_snapshot(&self) -> Option<InstalledSession> {
        self.session
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Redeem the rotating refresh cookie once, whichever task asks first.
    ///
    /// NewAPI invalidates a refresh cookie as it is used, so two tasks that both
    /// saw a 401 must not each redeem the same cookie: the second exchange would
    /// fail while the credential the first installed is already good. The lock
    /// serialises the exchange, and the generation check after acquiring it
    /// sends a task that was overtaken straight to the session the winner
    /// installed instead of redeeming a cookie that is no longer current.
    pub(super) async fn refresh_session(
        &self,
        stale: &InstalledSession,
    ) -> std::result::Result<InstalledSession, NewApiError> {
        let _single_flight = self.refresh.lock().await;
        let Some(current) = self.session_snapshot() else {
            return Err(NewApiError::NotAuthenticated);
        };
        if current.generation != stale.generation {
            tracing::debug!("newapi credential was already renewed; reusing the newer session");
            return Ok(current);
        }
        let renewed = self.renew_session(&current.session).await?;
        Ok(self.install_session_if_current(stale.generation, renewed))
    }
}
