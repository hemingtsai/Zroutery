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
        match self
            .attempt(
                method.clone(),
                path,
                query,
                Some(&session.session.bearer),
                None,
            )
            .await
        {
            Ok(attempt) => Ok(attempt.data),
            Err(NewApiError::Unauthorized { .. }) if session.session.refresh_cookie.is_some() => {
                tracing::debug!("newapi access token rejected; renewing from the refresh cookie");
                let renewed = self.refresh_session(&session).await?;
                self.attempt(method, path, query, Some(&renewed.session.bearer), None)
                    .await
                    .map(|attempt| attempt.data)
            }
            Err(e) => Err(e),
        }
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
