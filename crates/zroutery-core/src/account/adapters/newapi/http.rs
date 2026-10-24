//! One authenticated call, the retry that renews an expired token once, and the status cache.

use serde_json::Value;

use crate::error::Result;

use super::helpers::*;
use super::wire::*;
use super::NewApiAdapter;
use super::{PROVIDER_ID, REFRESH_COOKIE_NAME};

impl NewApiAdapter {
    /// Run one authenticated call, renewing the credential once when the panel
    /// says the token expired.
    pub(super) async fn request_value(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> std::result::Result<Value, NewApiError> {
        let session = self
            .session_snapshot()
            .ok_or(NewApiError::NotAuthenticated)?;
        /*
         * `New-Api-User: <id>` rides every call a session already owns. The
         * current new-api middleware enforces it on the log, check-in and
         * subscription surfaces — the header must name the user the token
         * belongs to, and a missing header answers 401 even though the
         * Authorization token is perfectly valid. `/api/user/self` accepts the
         * bearer alone, which is exactly why a freshly captured token
         * verified, stored, and then "failed" the very probe that should have
         * confirmed it.
         *
         * The id is the panel's own answer, not a value anyone types. A session
         * that does not carry one yet — every adapter built straight from a
         * configured key — learns it from `/api/user/self`, but only on the way
         * to a surface that enforces the header: anywhere else the extra call
         * would be traffic the panel never asked for, and a one-shot self
         * answer a test (or a rate limit) was holding would be spent on it.
         */
        let mut user_header = session.session.user_id.clone().filter(|id| !id.is_empty());
        // A configured id already rides every call (see `attempt_with_user`),
        // so there is nothing to learn; the lookup below is for the panels
        // where the header is optional on `/api/user/self` but demanded on the
        // log surfaces.
        let configured_user = self
            .config
            .new_api_user
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty());
        if user_header.is_none() && !configured_user && surface_names_its_user(path) {
            if let Some(id) = self.learn_user_id(&session.session.bearer).await {
                if let Ok(mut guard) = self.session.write() {
                    if let Some(installed) = guard.as_mut() {
                        installed.session.user_id = Some(id.clone());
                    }
                }
                user_header = Some(id);
            }
        }
        match self
            .attempt_with_user(
                method.clone(),
                path,
                query,
                Some(&session.session.bearer),
                None,
                user_header.as_deref(),
            )
            .await
        {
            Ok(attempt) => Ok(attempt.data),
            Err(NewApiError::Unauthorized { .. }) if session.session.refresh_cookie.is_some() => {
                tracing::debug!("newapi access token rejected; renewing from the refresh cookie");
                let renewed = self.refresh_session(&session).await?;
                let renewed_user = renewed
                    .session
                    .user_id
                    .clone()
                    .or(user_header.clone())
                    .filter(|id| !id.is_empty());
                self.attempt_with_user(
                    method,
                    path,
                    query,
                    Some(&renewed.session.bearer),
                    None,
                    renewed_user.as_deref(),
                )
                .await
                .map(|attempt| attempt.data)
            }
            Err(e) => Err(e),
        }
    }

    /// Ask the panel which user a bearer belongs to.
    ///
    /// `/api/user/self` is the one surface that answers the bearer alone, so it
    /// is also the one a sessionless id lookup can use. `None` — unreachable,
    /// rejected, or an answer without an id — means "carry on without the
    /// header", which is exactly the behaviour panels that never wanted it
    /// have always had.
    async fn learn_user_id(&self, bearer: &str) -> Option<String> {
        let attempt = self
            .attempt(
                reqwest::Method::GET,
                "/api/user/self",
                &[],
                Some(bearer),
                None,
            )
            .await
            .ok()?;
        let user: super::wire::SelfData = serde_json::from_value(attempt.data).ok()?;
        let id = user_id_of(&user)?;
        (!id.is_empty()).then_some(id)
    }

    /// [`Self::request_value`], with the panel failure projected onto [`Error`].
    pub(super) async fn request_json(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value> {
        self.request_value(method, path, query)
            .await
            .map_err(NewApiError::into_core)
    }

    /// One authenticated call decoded into `T`.
    pub(super) async fn request_typed<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let value = self
            .request_value(method, path, query)
            .await
            .map_err(NewApiError::into_core)?;
        decode(value, path)
    }

    /// The single place a panel request is built and sent.
    pub(super) async fn attempt(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        bearer: Option<&str>,
        refresh_cookie: Option<&str>,
    ) -> std::result::Result<Attempt, NewApiError> {
        self.attempt_with_user(method, path, query, bearer, refresh_cookie, None)
            .await
    }

    /// [`Self::attempt`], with the `New-Api-User` id attached when the call
    /// has one. `None` means "not resolved for this call": a configured id
    /// (see `NewApiConfig::new_api_user`) still rides, because a panel that
    /// demands the header demands it on every call, including the
    /// session-bootstrapping one — and on those panels the id cannot be
    /// learned by asking, since the asking call is the one being refused.
    pub(super) async fn attempt_with_user(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        bearer: Option<&str>,
        refresh_cookie: Option<&str>,
        new_api_user: Option<&str>,
    ) -> std::result::Result<Attempt, NewApiError> {
        let new_api_user = new_api_user.or_else(|| {
            self.config
                .new_api_user
                .as_deref()
                .filter(|id| !id.trim().is_empty())
        });
        let url = format!("{}{}", self.base, path);
        let mut request = self.client.request(method, &url);
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(token) = bearer {
            // The credential rides in the Authorization header **as-is**, with no
            // `Bearer ` prefix. This is a dashboard credential (a personal access
            // token or a login token), and NewAPI's dashboard middleware compares
            // the header value against the stored token verbatim — a `Bearer `
            // prefix becomes part of the compared string and the panel answers
            // 401 no matter how correct the token is. Newer forks that do strip
            // the prefix accept the bare value too, so the bare form is the one
            // form every generation of the panel accepts.
            let value = header_value(token)?;
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        if let Some(cookie) = refresh_cookie {
            let value = header_value(&format!("{REFRESH_COOKIE_NAME}={cookie}"))?;
            request = request.header(reqwest::header::COOKIE, value);
        }
        if let Some(id) = new_api_user {
            let value = header_value(id).map_err(|_| {
                NewApiError::Invalid("the panel user id contains invalid characters".into())
            })?;
            request = request.header("new-api-user", value);
        }

        let response = request.send().await.map_err(NewApiError::Transport)?;
        let status = response.status();
        let refresh_cookie = response_refresh_cookie(&response);
        let body = response.text().await.map_err(NewApiError::Transport)?;

        let envelope: Option<Envelope> = serde_json::from_str(&body).ok();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            let envelope = envelope.unwrap_or(Envelope {
                success: false,
                message: None,
                code: None,
                data: None,
            });
            // The panel's own words for the refusal: a dead token, a missing
            // New-Api-User header and a WAF interstitial each name themselves,
            // need different fixes, and the account status collapses all three
            // into one "rejected" — so the body goes to the log.
            tracing::warn!(
                path,
                body = %body.chars().take(200).collect::<String>(),
                "the panel answered 401"
            );
            return Err(NewApiError::Unauthorized {
                code: envelope.code,
                message: envelope.message.unwrap_or_default(),
            });
        }

        // A non-2xx answer whose body is not the dashboard envelope is an HTTP
        // failure (a reverse proxy page, a 404 from an older instance), not a
        // malformed success payload. Either way the body itself is dropped.
        let Some(envelope) = envelope else {
            return Err(if status.is_success() {
                NewApiError::Payload(format!(
                    "{PROVIDER_ID} answered HTTP {} with a body that is not the dashboard JSON envelope",
                    status.as_u16()
                ))
            } else {
                NewApiError::Upstream {
                    status: status.as_u16(),
                    code: None,
                    message: "response body was not the dashboard JSON envelope".into(),
                }
            });
        };

        if !status.is_success() {
            return Err(NewApiError::Upstream {
                status: status.as_u16(),
                code: envelope.code,
                message: envelope.message.unwrap_or_default(),
            });
        }
        if !envelope.success {
            return Err(NewApiError::Upstream {
                status: status.as_u16(),
                code: envelope.code,
                message: envelope.message.unwrap_or_default(),
            });
        }

        Ok(Attempt {
            data: envelope.data.unwrap_or(Value::Null),
            refresh_cookie,
        })
    }
}

/// Whether this path belongs to a surface that names its user.
///
/// The list is the current new-api middleware's own: the log endpoints, the
/// check-in endpoints and the subscription endpoints all resolve the bearer
/// and then compare the New-Api-User header against it. Everything else —
/// /api/user/self above all — authenticates on the bearer alone.
fn surface_names_its_user(path: &str) -> bool {
    path.starts_with("/api/log")
        || path.starts_with("/api/user/checkin")
        || path.starts_with("/api/subscription")
}
