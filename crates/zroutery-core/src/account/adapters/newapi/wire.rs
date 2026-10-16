//! The shapes NewAPI actually puts on the wire, and the session the adapter holds in memory. None of these leave the module.

use serde::Deserialize;
use serde_json::Value;

use super::PROVIDER_ID;
use crate::error::Error;

/// An authenticated NewAPI credential, as held in memory by the adapter.
#[derive(Debug, Clone)]
pub(super) struct Session {
    pub(super) bearer: String,
    /// `new_api_refresh` cookie value; only present for credentials that can be
    /// renewed (`Cookie` and `OAuth2` with a refresh token).
    pub(super) refresh_cookie: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) username: Option<String>,
    pub(super) expires_at: Option<i64>,
}

/// One HTTP exchange with the panel.
#[derive(Debug)]
pub(super) struct Attempt {
    pub(super) data: Value,
    /// A rotated `new_api_refresh` cookie, when the response set one.
    pub(super) refresh_cookie: Option<String>,
}

/// Failure from one panel call, before it is projected onto [`Error`].
#[derive(Debug)]
pub(super) enum NewApiError {
    /// No credential has been configured or authenticated.
    NotAuthenticated,
    /// The request never left the process, or the answer never arrived.
    Transport(reqwest::Error),
    /// The panel rejected the request because the credential is unusable.
    Unauthorized {
        code: Option<String>,
        message: String,
    },
    /// A 2xx answer with `success: false`, or a non-2xx answer.
    Upstream {
        status: u16,
        code: Option<String>,
        message: String,
    },
    /// The answer was not the JSON this adapter expects.
    Payload(String),
    /// The caller asked for something this adapter cannot express.
    Invalid(String),
}

impl NewApiError {
    pub(super) fn into_core(self) -> Error {
        match self {
            NewApiError::NotAuthenticated => Error::MissingApiKey(PROVIDER_ID.into()),
            NewApiError::Invalid(message) => Error::InvalidRequest(message),
            NewApiError::Unauthorized { code, message } => {
                // The credential is the problem; the panel's own wording adds
                // nothing a caller can act on, and may name internal detail.
                tracing::debug!(
                    code = code.as_deref().unwrap_or(""),
                    message = %message,
                    "newapi rejected the credential"
                );
                Error::Unauthorized
            }
            NewApiError::Upstream {
                status,
                code,
                message,
            } => Error::Upstream {
                provider: PROVIDER_ID.into(),
                status,
                body: join_code_message(code, message),
            },
            NewApiError::Transport(source) => Error::Transport {
                provider: PROVIDER_ID.into(),
                source,
            },
            NewApiError::Payload(message) => Error::BadUpstreamPayload(message),
        }
    }
}

pub(super) fn join_code_message(code: Option<String>, message: String) -> String {
    match (code, message.is_empty()) {
        (Some(code), false) => format!("{code}: {message}"),
        (Some(code), true) => code,
        (None, _) => message,
    }
}

/// `{success, message, code, data}` envelope every dashboard endpoint uses.
#[derive(Debug, Deserialize)]
pub(super) struct Envelope {
    #[serde(default)]
    pub(super) success: bool,
    #[serde(default)]
    pub(super) message: Option<String>,
    #[serde(default)]
    pub(super) code: Option<String>,
    #[serde(default)]
    pub(super) data: Option<Value>,
}

/// `data` of `GET /api/user/self`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct SelfData {
    pub(super) id: i64,
    pub(super) username: String,
    pub(super) display_name: Option<String>,
    /// 1 = enabled, 2 = disabled.
    pub(super) status: i64,
    /// Remaining wallet quota, in credit units.
    pub(super) quota: i64,
    /// Lifetime consumption, in credit units.
    pub(super) used_quota: i64,
    pub(super) request_count: i64,
    pub(super) group: Option<String>,
}

/// `data` of `GET /api/log/self/stat`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct LogStatData {
    pub(super) quota: i64,
    /// Requests in the last 60 seconds.
    pub(super) rpm: i64,
    /// Tokens in the last 60 seconds.
    pub(super) tpm: i64,
}

/// `data` of `POST /api/user/auth/refresh` and of a successful login.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct LoginData {
    pub(super) access_token: String,
    pub(super) access_expires_at: Option<i64>,
    pub(super) user: Option<SelfData>,
}

/// The credential installed in the adapter, and the generation it belongs to.
///
/// The generation is what makes a refresh safe under concurrency: a task that
/// snapshotted generation *n* can tell whether it is still redeeming the current
/// credential or has been overtaken by another refresh or a fresh login.
#[derive(Debug, Clone)]
pub(super) struct InstalledSession {
    pub(super) session: Session,
    pub(super) generation: u64,
}
