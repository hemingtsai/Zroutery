//! NewAPI account adapter.
//!
//! [NewAPI] is a multi-provider LLM relay that also ships a dashboard HTTP API
//! for accounts: balance, consumption logs, check-in and a public status
//! endpoint. This adapter maps that API onto the provider-agnostic
//! [`AccountProvider`] trait. Everything NewAPI-specific — endpoint paths,
//! quota units, error codes, credential shapes — stays inside this module; the
//! rest of the core only sees [`AccountRuntime`], [`AccountQuota`] and friends.
//!
//! [NewAPI]: https://github.com/Calcium-Ion/new-api
//!
//! # Endpoints
//!
//! Verified against `Calcium-Ion/new-api` at commit `c2b7a9a` (2026-09-25):
//!
//! | Endpoint | Auth | Used for |
//! |---|---|---|
//! | `GET  /api/status` | none | reachability, `quota_per_unit`, check-in and Turnstile flags |
//! | `GET  /api/user/self` | bearer | quota, used quota, request count, account status |
//! | `GET  /api/log/self/stat` | bearer | consumed quota for a window, plus `rpm`/`tpm` |
//! | `GET  /api/log/self` | bearer | paged consume logs, summed for token counts |
//! | `POST /api/user/auth/refresh` | refresh cookie | rotate a dashboard access token |
//! | `GET/POST /api/user/checkin` | bearer | check-in status and daily check-in |
//! | `GET  /api/subscription/self` | bearer | whether a zero wallet is still funded by a plan |
//!
//! # Credentials
//!
//! Modern NewAPI dropped cookie sessions and the `New-Api-User` header: the
//! dashboard authenticates with `Authorization: Bearer <token>`, where the
//! token is either a personal access token (`GET /api/user/token`) or a
//! short-lived login JWT. A login JWT can be renewed with the `new_api_refresh`
//! cookie through `POST /api/user/auth/refresh`; this adapter performs that
//! renewal once when a call comes back `401`, and stores the rotated cookie.
//!
//! # Quota units
//!
//! NewAPI keeps a synthetic credit where `quota_per_unit` units equal one US
//! dollar (`common.QuotaPerUnit`, default `500_000`); the panel's own
//! `logger.LogQuota` converts with `usd = quota / quota_per_unit`. This adapter
//! reports every money value in USD, reads the live `quota_per_unit` from
//! `GET /api/status`, and keeps the raw credit numbers in
//! `AccountRuntime::metadata` under `newapi.*` keys.
//!
//! # What is deliberately not supported
//!
//! * Check-in on an instance that requires a Turnstile challenge and no
//!   challenge response is supplied — a headless client cannot solve it, so
//!   [`NewApiAdapter::checkin_with_turnstile`] reports "not supported" instead
//!   of burning a doomed request.
//! * Rate-limit *limits*: NewAPI exposes current `rpm`/`tpm` usage but no
//!   per-account ceiling, so [`RateLimitState`] carries usage with unknown
//!   limits (its `pressure()` stays `0.0`).

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use super::super::provider::*;
use super::super::types::*;
use crate::error::{Error, Result};

/// `provider_id()` reported by this adapter.
const PROVIDER_ID: &str = "newapi";
/// Refresh cookie name (`service.RefreshCookieName`).
const REFRESH_COOKIE_NAME: &str = "new_api_refresh";
/// Quota units per US dollar, as a fallback when `/api/status` cannot tell us
/// (`common.QuotaPerUnit` in NewAPI).
const DEFAULT_QUOTA_PER_UNIT: f64 = 500_000.0;
/// `common.UserStatusEnabled`.
const USER_STATUS_ENABLED: i64 = 1;
/// `common.UserStatusDisabled`.
const USER_STATUS_DISABLED: i64 = 2;
/// `model.LogTypeConsume` — consumption entries, which is what balance and
/// token statistics are about (`topup`/`system`/`error` entries are not).
const LOG_TYPE_CONSUME: i64 = 2;
/// NewAPI clamps `page_size` to 100 (`common.GetPageQuery`).
const LOG_PAGE_SIZE: u32 = 100;
/// Default usage window for [`AccountProvider::fetch_usage`].
const DEFAULT_USAGE_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
/// `/api/status` is public and changes rarely; caching it keeps `refresh()`
/// down to the calls that actually carry account state.
const STATUS_CACHE_TTL: Duration = Duration::from_secs(300);

/// Authentication method for NewAPI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NewApiAuth {
    /// Refresh cookie (`new_api_refresh`) from a dashboard login. Exchanged for
    /// a short-lived access token, and kept so expired tokens can be renewed.
    Cookie { session_cookie: String },
    /// Personal access token from `GET /api/user/token`, sent as a bearer.
    ApiKey { key: String },
    /// A dashboard access token (login JWT) plus, optionally, the refresh
    /// cookie value that can renew it.
    OAuth2 {
        access_token: String,
        refresh_token: Option<String>,
    },
}

/// An authenticated session with NewAPI.
#[derive(Debug, Clone)]
pub struct AuthenticatedSession {
    pub auth: NewApiAuth,
    /// NewAPI user id, once the credential has been accepted.
    pub user_id: Option<String>,
    /// Login name the instance reports for that credential.
    pub username: Option<String>,
    pub authenticated_at: i64,
    /// Expiry of the access token, when the instance reports one. Personal
    /// access tokens do not expire.
    pub expires_at: Option<i64>,
}

/// NewAPI-specific configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct NewApiConfig {
    /// Panel root (`https://newapi.example.com`). A trailing `/api` or `/v1`
    /// segment is tolerated because that is what operators paste from the relay
    /// endpoint; see [`NewApiConfig::validate`].
    pub base_url: String,
    /// Credential used before [`NewApiAdapter::authenticate`] is called: a
    /// personal access token, or a login JWT. Verified lazily by the first
    /// authenticated call.
    pub api_key: String,
    /// Which operations this NewAPI instance supports.
    pub capabilities: AccountCapabilities,
    /// Override the `quota_per_unit` reported by `GET /api/status`.
    pub quota_per_unit: Option<f64>,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    /// Upper bound on log pages walked per usage report (`page_size` is 100, so
    /// the bound is `usage_page_limit * 100` log entries).
    pub usage_page_limit: u32,
}

impl Default for NewApiConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            capabilities: AccountCapabilities {
                supports_usage: true,
                supports_quota: true,
                supports_refresh: true,
                supports_checkin: true,
                supports_health_check: true,
            },
            quota_per_unit: None,
            connect_timeout_secs: 10,
            request_timeout_secs: 20,
            usage_page_limit: 10,
        }
    }
}

impl NewApiConfig {
    /// Validate the configuration before any request is attempted.
    pub fn validate(&self) -> std::result::Result<(), String> {
        normalize_base_url(&self.base_url)?;
        if self.connect_timeout_secs == 0 {
            return Err("connect_timeout_secs must be greater than zero".into());
        }
        if self.request_timeout_secs == 0 {
            return Err("request_timeout_secs must be greater than zero".into());
        }
        if self.usage_page_limit == 0 {
            return Err("usage_page_limit must be at least 1".into());
        }
        if let Some(unit) = self.quota_per_unit {
            if !unit.is_finite() || unit <= 0.0 {
                return Err("quota_per_unit must be a finite positive number".into());
            }
        }
        if !self.api_key.trim().is_empty() {
            validate_secret("api_key", &self.api_key)?;
        }
        Ok(())
    }

    /// The API root, with any `/api` or `/v1` suffix removed.
    fn api_root(&self) -> std::result::Result<String, String> {
        normalize_base_url(&self.base_url)
    }
}

/// The public `/api/status` payload, reduced to the fields this adapter uses.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiStatus {
    pub version: Option<String>,
    pub system_name: Option<String>,
    /// Quota units per US dollar, as the panel reports it.
    pub quota_per_unit: Option<f64>,
    pub checkin_enabled: Option<bool>,
    /// When set, `POST /api/user/checkin` needs a `turnstile` query parameter.
    pub turnstile_check: Option<bool>,
    /// Self-use mode ignores wallet balances for pricing decisions.
    pub self_use_mode_enabled: Option<bool>,
    pub display_in_currency: Option<bool>,
    pub usd_exchange_rate: Option<f64>,
}

impl NewApiStatus {
    /// `quota_per_unit` with NewAPI's own default substituted when the instance
    /// did not report a usable value.
    pub fn quota_per_unit_or_default(&self) -> f64 {
        match self.quota_per_unit {
            Some(unit) if unit.is_finite() && unit > 0.0 => unit,
            _ => DEFAULT_QUOTA_PER_UNIT,
        }
    }
}

/// Usage over one window, with the raw details NewAPI can give us.
#[derive(Debug, Clone, PartialEq)]
pub struct NewApiUsageReport {
    pub period_start: i64,
    pub period_end: i64,
    /// Consumed quota (credit units) inside the window.
    pub quota_used: i64,
    /// `quota_used` converted with the instance's `quota_per_unit`.
    pub cost_usd: f64,
    /// Consumption entries in the window, as counted by NewAPI.
    pub total_requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Requests in the last 60 seconds, when the instance reports it.
    pub rpm_used: Option<u32>,
    /// Tokens in the last 60 seconds, when the instance reports it.
    pub tpm_used: Option<u64>,
    /// True when the token totals only cover the first
    /// `usage_page_limit * 100` log entries of the window.
    pub truncated: bool,
}

/// An authenticated NewAPI credential, as held in memory by the adapter.
#[derive(Debug, Clone)]
struct Session {
    bearer: String,
    /// `new_api_refresh` cookie value; only present for credentials that can be
    /// renewed (`Cookie` and `OAuth2` with a refresh token).
    refresh_cookie: Option<String>,
    user_id: Option<String>,
    username: Option<String>,
    expires_at: Option<i64>,
}

/// One HTTP exchange with the panel.
#[derive(Debug)]
struct Attempt {
    data: Value,
    /// A rotated `new_api_refresh` cookie, when the response set one.
    refresh_cookie: Option<String>,
}

/// Failure from one panel call, before it is projected onto [`Error`].
#[derive(Debug)]
enum NewApiError {
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
    fn into_core(self) -> Error {
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

fn join_code_message(code: Option<String>, message: String) -> String {
    match (code, message.is_empty()) {
        (Some(code), false) => format!("{code}: {message}"),
        (Some(code), true) => code,
        (None, _) => message,
    }
}

/// `{success, message, code, data}` envelope every dashboard endpoint uses.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

/// `data` of `GET /api/user/self`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SelfData {
    id: i64,
    username: String,
    display_name: Option<String>,
    /// 1 = enabled, 2 = disabled.
    status: i64,
    /// Remaining wallet quota, in credit units.
    quota: i64,
    /// Lifetime consumption, in credit units.
    used_quota: i64,
    request_count: i64,
    group: Option<String>,
}

/// `data` of `GET /api/log/self/stat`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LogStatData {
    quota: i64,
    /// Requests in the last 60 seconds.
    rpm: i64,
    /// Tokens in the last 60 seconds.
    tpm: i64,
}

/// `data` of `GET /api/log/self`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LogPage {
    total: i64,
    items: Vec<LogItem>,
}

/// One consumption log entry.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LogItem {
    prompt_tokens: i64,
    completion_tokens: i64,
}

/// `data` of `POST /api/user/auth/refresh` and of a successful login.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LoginData {
    access_token: String,
    access_expires_at: Option<i64>,
    user: Option<SelfData>,
}

/// NewAPI account adapter.
pub struct NewApiAdapter {
    config: NewApiConfig,
    client: reqwest::Client,
    /// API root, already normalised by [`normalize_base_url`].
    base: String,
    session: RwLock<Option<Session>>,
    status_cache: RwLock<Option<(Instant, NewApiStatus)>>,
}

impl NewApiAdapter {
    /// Build an adapter for one NewAPI instance.
    ///
    /// A non-empty [`NewApiConfig::api_key`] is installed as the initial
    /// credential; it is verified by the first authenticated call.
    pub fn new(config: NewApiConfig) -> Result<Self> {
        config.validate().map_err(Error::invalid)?;
        let base = config.api_root().map_err(Error::invalid)?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            // A redirect would hand the bearer to whatever host the panel
            // names, so the panel must answer where it was asked.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("zroutery/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::internal(format!("cannot build the newapi HTTP client: {e}")))?;

        let session = if config.api_key.trim().is_empty() {
            None
        } else {
            Some(Session {
                bearer: config.api_key.trim().to_string(),
                refresh_cookie: None,
                user_id: None,
                username: None,
                expires_at: None,
            })
        };

        Ok(Self {
            config,
            client,
            base,
            session: RwLock::new(session),
            status_cache: RwLock::new(None),
        })
    }

    /// The configuration this adapter was built with.
    pub fn config(&self) -> &NewApiConfig {
        &self.config
    }

    /// Whether a credential is available, authenticated or not.
    pub fn is_authenticated(&self) -> bool {
        self.session_snapshot().is_some()
    }

    /// Authenticate with NewAPI using the provided auth method, replacing any
    /// credential configured in [`NewApiConfig::api_key`].
    ///
    /// `ApiKey` and `OAuth2` are verified against `GET /api/user/self` before
    /// the session is installed, so a rejected credential leaves the adapter
    /// exactly as unauthenticated as it was.
    pub async fn authenticate(&self, auth: NewApiAuth) -> Result<AuthenticatedSession> {
        let session = match auth.clone() {
            NewApiAuth::Cookie { session_cookie } => {
                self.session_from_cookie(&session_cookie).await?
            }
            NewApiAuth::ApiKey { key } => {
                let key = strip_bearer_prefix(&key);
                validate_secret("API key", key).map_err(Error::invalid)?;
                self.verified_session(key, None).await?
            }
            NewApiAuth::OAuth2 {
                access_token,
                refresh_token,
            } => {
                let token = strip_bearer_prefix(&access_token);
                validate_secret("access token", token).map_err(Error::invalid)?;
                if let Some(cookie) = refresh_token.as_deref() {
                    let cookie = cookie_value(cookie);
                    validate_cookie(&cookie).map_err(Error::invalid)?;
                    self.verified_session(token, Some(cookie)).await?
                } else {
                    self.verified_session(token, None).await?
                }
            }
        };

        let authenticated_at = chrono::Utc::now().timestamp();
        let session = self.install_session(session);
        Ok(AuthenticatedSession {
            auth,
            user_id: session.user_id,
            username: session.username,
            authenticated_at,
            expires_at: session.expires_at,
        })
    }

    /// Read `GET /api/status` without any credential, for a "test connection"
    /// action. Not cached, so it always reflects the instance right now.
    pub async fn probe_status(&self) -> Result<NewApiStatus> {
        self.fetch_status().await
    }

    /// Usage over `[start, end]` (unix seconds; `None` means "last 30 days
    /// ending now").
    pub async fn usage_report(
        &self,
        start: Option<i64>,
        end: Option<i64>,
    ) -> Result<NewApiUsageReport> {
        let end = end.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let start = start.unwrap_or(end - DEFAULT_USAGE_WINDOW_SECS);
        if start > end {
            return Err(Error::invalid("usage window starts after it ends"));
        }

        let status = self.status_cached().await?;
        let unit = self.effective_quota_per_unit(&status);

        let stat: LogStatData = self
            .request_typed(
                reqwest::Method::GET,
                "/api/log/self/stat",
                &window_query(start, end),
            )
            .await?;

        let mut page = 1u32;
        let mut total_requests = 0u64;
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut truncated = false;

        loop {
            let mut query = window_query(start, end);
            query.push(("p", page.to_string()));
            query.push(("page_size", LOG_PAGE_SIZE.to_string()));

            let logs: LogPage = self
                .request_typed(reqwest::Method::GET, "/api/log/self", &query)
                .await?;

            if page == 1 {
                total_requests = u64::try_from(logs.total).unwrap_or(0);
            }
            for item in &logs.items {
                prompt_tokens += u64::try_from(item.prompt_tokens.max(0)).unwrap_or(0);
                completion_tokens += u64::try_from(item.completion_tokens.max(0)).unwrap_or(0);
            }

            if logs.items.is_empty() {
                break;
            }
            let fetched = u64::from(page) * u64::from(LOG_PAGE_SIZE);
            if u64::try_from(logs.total).unwrap_or(0) <= fetched {
                break;
            }
            if page >= self.config.usage_page_limit {
                truncated = true;
                break;
            }
            page += 1;
        }

        Ok(NewApiUsageReport {
            period_start: start,
            period_end: end,
            quota_used: stat.quota,
            cost_usd: quota_to_usd(stat.quota, unit),
            total_requests,
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            rpm_used: u32::try_from(stat.rpm).ok().filter(|v| *v > 0),
            tpm_used: u64::try_from(stat.tpm).ok().filter(|v| *v > 0),
            truncated,
        })
    }

    /// Daily check-in, with an optional Turnstile challenge response.
    ///
    /// Returns [`AccountOpResult::NotSupported`] when the instance has check-in
    /// disabled, or when it requires a Turnstile response that the caller did
    /// not supply. Signing in twice in one day is reported as
    /// [`AccountOpResult::Success`] — the day's reward is already granted.
    pub async fn checkin_with_turnstile(&self, turnstile: Option<&str>) -> Result<AccountOpResult> {
        let status = self.status_cached().await?;
        if !status.checkin_enabled.unwrap_or(false) {
            return Ok(AccountOpResult::NotSupported);
        }
        let challenge = turnstile.map(str::trim).filter(|t| !t.is_empty());
        if status.turnstile_check.unwrap_or(false) && challenge.is_none() {
            return Ok(AccountOpResult::NotSupported);
        }

        if let Ok(data) = self
            .request_json(reqwest::Method::GET, "/api/user/checkin", &[])
            .await
        {
            let already = data
                .pointer("/stats/checked_in_today")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if already {
                return Ok(AccountOpResult::Success);
            }
        }

        let query = match challenge {
            Some(token) => vec![("turnstile", token.to_string())],
            None => Vec::new(),
        };
        match self
            .request_json(reqwest::Method::POST, "/api/user/checkin", &query)
            .await
        {
            Ok(_) => Ok(AccountOpResult::Success),
            // The instance answers business failures with HTTP 200 and
            // `success: false`; the message is the only useful part.
            Err(Error::Upstream { body, .. }) => Ok(AccountOpResult::Failed(body)),
            Err(e) => Err(e),
        }
    }

    // ── credentials ──────────────────────────────────────────────────────

    /// Exchange a `new_api_refresh` cookie for a dashboard access token.
    async fn session_from_cookie(&self, raw: &str) -> Result<Session> {
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
            expires_at: login.access_expires_at,
        })
    }

    /// Verify a bearer credential against `GET /api/user/self`.
    async fn verified_session(
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
            expires_at: None,
        })
    }

    /// Renew an expired access token from the stored refresh cookie.
    async fn renew_session(&self, session: &Session) -> std::result::Result<Session, NewApiError> {
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
            expires_at: login.access_expires_at,
        })
    }

    fn install_session(&self, session: Session) -> Session {
        *self
            .session
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(session.clone());
        session
    }

    fn session_snapshot(&self) -> Option<Session> {
        self.session
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    // ── HTTP ─────────────────────────────────────────────────────────────

    /// Run one authenticated call, renewing the credential once when the panel
    /// says the token expired.
    async fn request_value(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> std::result::Result<Value, NewApiError> {
        let session = self
            .session_snapshot()
            .ok_or(NewApiError::NotAuthenticated)?;
        match self
            .attempt(method.clone(), path, query, Some(&session.bearer), None)
            .await
        {
            Ok(attempt) => Ok(attempt.data),
            Err(NewApiError::Unauthorized { .. }) if session.refresh_cookie.is_some() => {
                tracing::debug!("newapi access token rejected; renewing from the refresh cookie");
                let renewed = self.renew_session(&session).await?;
                let renewed = self.install_session(renewed);
                self.attempt(method, path, query, Some(&renewed.bearer), None)
                    .await
                    .map(|attempt| attempt.data)
            }
            Err(e) => Err(e),
        }
    }

    /// [`Self::request_value`], with the panel failure projected onto [`Error`].
    async fn request_json(
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
    async fn request_typed<T: serde::de::DeserializeOwned>(
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
    async fn attempt(
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
            let value = header_value(&format!("Bearer {token}"))?;
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

    // ── panel reads ──────────────────────────────────────────────────────

    /// `GET /api/status`, always fresh.
    async fn fetch_status(&self) -> Result<NewApiStatus> {
        let attempt = self
            .attempt(reqwest::Method::GET, "/api/status", &[], None, None)
            .await
            .map_err(NewApiError::into_core)?;
        decode(attempt.data, "/api/status")
    }

    /// `GET /api/status`, memoised for [`STATUS_CACHE_TTL`].
    async fn status_cached(&self) -> Result<NewApiStatus> {
        if let Some((at, status)) = self
            .status_cache
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
        {
            if at.elapsed() < STATUS_CACHE_TTL {
                return Ok(status);
            }
        }
        let status = self.fetch_status().await?;
        *self
            .status_cache
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some((Instant::now(), status.clone()));
        Ok(status)
    }

    /// `GET /api/user/self`, keeping the panel's error code so callers can tell
    /// "this user is disabled" from "this token expired".
    async fn self_probe(&self) -> std::result::Result<SelfData, NewApiError> {
        let value = self
            .request_value(reqwest::Method::GET, "/api/user/self", &[])
            .await?;
        serde_json::from_value(value)
            .map_err(|e| NewApiError::Payload(format!("{PROVIDER_ID} /api/user/self payload: {e}")))
    }

    /// `GET /api/user/self`, with the panel failure projected onto [`Error`].
    async fn self_snapshot(&self) -> Result<SelfData> {
        self.self_probe().await.map_err(NewApiError::into_core)
    }

    /// Whether a zero wallet is still funded by an active subscription plan.
    ///
    /// Instances old enough to lack `/api/subscription/self` answer 404, which
    /// this reports as "no subscription": the wallet is then the only funding
    /// source, which is what the caller is trying to decide.
    async fn has_active_subscription(&self) -> Result<bool> {
        let data = self
            .request_json(reqwest::Method::GET, "/api/subscription/self", &[])
            .await?;
        let active = data
            .get("subscriptions")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        Ok(active > 0)
    }

    /// `GET /api/log/self/stat` over the last minute, for `rpm`/`tpm` usage.
    async fn rate_limit_probe(&self) -> Result<RateLimitState> {
        let end = chrono::Utc::now().timestamp();
        let query = window_query(end - 60, end);
        let stat: LogStatData = self
            .request_typed(reqwest::Method::GET, "/api/log/self/stat", &query)
            .await?;
        Ok(RateLimitState {
            rpm_used: u32::try_from(stat.rpm).ok().filter(|v| *v > 0),
            tpm_used: u64::try_from(stat.tpm).ok().filter(|v| *v > 0),
            ..Default::default()
        })
    }

    /// Map the panel's user record onto an [`AccountStatus`].
    ///
    /// A zero wallet is only "quota exhausted" when nothing else funds the
    /// account: self-use mode ignores balances, and a subscription can serve a
    /// wallet that is empty.
    async fn effective_status(&self, status: &NewApiStatus, user: &SelfData) -> AccountStatus {
        match user.status {
            USER_STATUS_ENABLED => {
                if user.quota > 0 || status.self_use_mode_enabled.unwrap_or(false) {
                    return AccountStatus::Active;
                }
                if self.has_active_subscription().await.unwrap_or(false) {
                    AccountStatus::Active
                } else {
                    AccountStatus::QuotaExhausted
                }
            }
            USER_STATUS_DISABLED => AccountStatus::Suspended,
            _ => AccountStatus::Unknown,
        }
    }

    fn effective_quota_per_unit(&self, status: &NewApiStatus) -> f64 {
        self.config
            .quota_per_unit
            .unwrap_or_else(|| status.quota_per_unit_or_default())
    }

    /// The `newapi.*` metadata a caller can rely on alongside the typed fields.
    fn metadata(
        &self,
        status: &NewApiStatus,
        user: &SelfData,
        quota_per_unit: f64,
    ) -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        if user.id > 0 {
            metadata.insert("newapi.user_id".into(), user.id.to_string());
        }
        if !user.username.is_empty() {
            metadata.insert("newapi.username".into(), user.username.clone());
        }
        if let Some(name) = user.display_name.as_deref().filter(|n| !n.is_empty()) {
            metadata.insert("newapi.display_name".into(), name.to_string());
        }
        if let Some(group) = user.group.as_deref().filter(|g| !g.is_empty()) {
            metadata.insert("newapi.group".into(), group.to_string());
        }
        metadata.insert("newapi.quota_raw".into(), user.quota.to_string());
        metadata.insert("newapi.used_quota_raw".into(), user.used_quota.to_string());
        metadata.insert(
            "newapi.request_count".into(),
            user.request_count.to_string(),
        );
        metadata.insert("newapi.quota_per_unit".into(), quota_per_unit.to_string());
        if let Some(version) = &status.version {
            metadata.insert("newapi.version".into(), version.clone());
        }
        if let Some(name) = &status.system_name {
            metadata.insert("newapi.system_name".into(), name.clone());
        }
        if let Some(enabled) = status.checkin_enabled {
            metadata.insert("newapi.checkin_enabled".into(), enabled.to_string());
        }
        if let Some(required) = status.turnstile_check {
            metadata.insert("newapi.turnstile_check".into(), required.to_string());
        }
        if let Some(self_use) = status.self_use_mode_enabled {
            metadata.insert("newapi.self_use_mode".into(), self_use.to_string());
        }
        if let Some(currency) = status.display_in_currency {
            metadata.insert("newapi.display_in_currency".into(), currency.to_string());
        }
        metadata
    }
}

impl AccountProvider for NewApiAdapter {
    fn provider_id(&self) -> &str {
        PROVIDER_ID
    }

    fn capabilities(&self) -> AccountCapabilities {
        self.config.capabilities.clone()
    }

    /// `GET /api/status` + `GET /api/user/self`, plus a one-minute rate probe.
    async fn refresh(&self, account_id: &AccountId) -> Result<AccountRuntime> {
        let status = self.status_cached().await?;
        let user = self.self_snapshot().await?;
        let quota_per_unit = self.effective_quota_per_unit(&status);
        let account_status = self.effective_status(&status, &user).await;

        // Usage is what `fetch_usage` is for: it costs a log sweep, which a
        // periodic refresh should not pay.
        let rate_limit = match self.rate_limit_probe().await {
            Ok(state) => Some(state),
            Err(e) => {
                tracing::debug!(error = %e, "newapi rate probe failed; leaving rate_limit empty");
                None
            }
        };

        let now = chrono::Utc::now().timestamp();
        Ok(AccountRuntime {
            account_id: account_id.clone(),
            provider_id: PROVIDER_ID.into(),
            status: account_status,
            capabilities: self.config.capabilities.clone(),
            quota: Some(quota_from_self(&user, quota_per_unit)),
            usage: None,
            rate_limit,
            last_success: Some(now),
            last_failure: None,
            last_sync: Some(now),
            metadata: self.metadata(&status, &user, quota_per_unit),
        })
    }

    /// Sums `GET /api/log/self` over the last 30 days.
    ///
    /// NewAPI counts tokens per log entry and has no aggregate endpoint for
    /// them, so the sweep is bounded by [`NewApiConfig::usage_page_limit`]; use
    /// [`NewApiAdapter::usage_report`] when the truncation flag matters.
    async fn fetch_usage(&self, _account_id: &AccountId) -> Result<AccountUsage> {
        let report = self.usage_report(None, None).await?;
        Ok(AccountUsage {
            total_requests: report.total_requests,
            total_tokens: report.total_tokens,
            total_cost: report.cost_usd,
            currency: "USD".into(),
            period_start: Some(report.period_start),
            period_end: Some(report.period_end),
        })
    }

    async fn fetch_quota(&self, _account_id: &AccountId) -> Result<AccountQuota> {
        let status = self.status_cached().await?;
        let user = self.self_snapshot().await?;
        Ok(quota_from_self(
            &user,
            self.effective_quota_per_unit(&status),
        ))
    }

    /// A disabled user is [`AccountStatus::Suspended`]; a rejected credential
    /// is [`AccountStatus::AuthenticationExpired`]. Without a credential the
    /// public status endpoint still proves the instance is reachable.
    async fn health_check(&self, _account_id: &AccountId) -> Result<AccountStatus> {
        if self.session_snapshot().is_none() {
            self.fetch_status().await?;
            return Ok(AccountStatus::Active);
        }
        match self.self_probe().await {
            Ok(user) => Ok(map_user_status(user.status)),
            Err(NewApiError::Unauthorized { code, .. })
                if code.as_deref() == Some("AUTH_USER_DISABLED") =>
            {
                Ok(AccountStatus::Suspended)
            }
            Err(NewApiError::Unauthorized { .. }) => Ok(AccountStatus::AuthenticationExpired),
            Err(e) => Err(e.into_core()),
        }
    }

    /// Daily check-in; see [`NewApiAdapter::checkin_with_turnstile`].
    async fn checkin(&self, _account_id: &AccountId) -> Result<AccountOpResult> {
        self.checkin_with_turnstile(None).await
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

/// `query` for the log endpoints, which filter on `[start, end]` unix seconds
/// and on the `consume` log type.
fn window_query(start: i64, end: i64) -> Vec<(&'static str, String)> {
    vec![
        ("type", LOG_TYPE_CONSUME.to_string()),
        ("start_timestamp", start.to_string()),
        ("end_timestamp", end.to_string()),
    ]
}

fn map_user_status(status: i64) -> AccountStatus {
    match status {
        USER_STATUS_ENABLED => AccountStatus::Active,
        USER_STATUS_DISABLED => AccountStatus::Suspended,
        _ => AccountStatus::Unknown,
    }
}

fn user_id_of(user: &SelfData) -> Option<String> {
    (user.id > 0).then(|| user.id.to_string())
}

fn username_of(user: &SelfData) -> Option<String> {
    (!user.username.is_empty()).then(|| user.username.clone())
}

/// Credit units to US dollars, using the instance's `quota_per_unit`.
fn quota_to_usd(quota: i64, quota_per_unit: f64) -> f64 {
    if quota_per_unit <= 0.0 || !quota_per_unit.is_finite() {
        return 0.0;
    }
    quota as f64 / quota_per_unit
}

/// The panel tracks a remaining balance plus lifetime consumption; the granted
/// total is their sum, which top-ups and redemptions can drift from.
fn quota_from_self(user: &SelfData, quota_per_unit: f64) -> AccountQuota {
    let remaining = quota_to_usd(user.quota, quota_per_unit);
    let used = quota_to_usd(user.used_quota, quota_per_unit);
    AccountQuota {
        total: remaining + used,
        used,
        remaining,
        unit: "USD".into(),
        resets_at: None,
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, endpoint: &str) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| Error::BadUpstreamPayload(format!("{PROVIDER_ID} {endpoint} payload: {e}")))
}

fn header_value(raw: &str) -> std::result::Result<reqwest::header::HeaderValue, NewApiError> {
    reqwest::header::HeaderValue::from_str(raw).map_err(|_| {
        NewApiError::Invalid(
            "newapi credential contains characters that are not valid in a header".into(),
        )
    })
}

/// Pull the rotated `new_api_refresh` cookie out of a response, if any.
fn response_refresh_cookie(response: &reqwest::Response) -> Option<String> {
    let prefix = format!("{REFRESH_COOKIE_NAME}=");
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let rest = value.strip_prefix(&prefix)?;
            let cookie = rest.split(';').next()?.trim();
            (!cookie.is_empty()).then(|| cookie.to_string())
        })
}

/// Trim, drop a pasted `Bearer ` prefix, and check the result can ride in a
/// header.
fn strip_bearer_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim()
}

fn validate_secret(what: &str, value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err(format!("empty {what}"));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{what} must not contain whitespace or control characters"
        ));
    }
    Ok(())
}

/// Accept both a bare cookie value and a pasted `new_api_refresh=<value>`.
fn cookie_value(raw: &str) -> String {
    let trimmed = raw.trim();
    let prefix = format!("{REFRESH_COOKIE_NAME}=");
    trimmed
        .strip_prefix(&prefix)
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn validate_cookie(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("empty session cookie".into());
    }
    if value.chars().any(|c| c == ';' || c.is_control()) {
        return Err("session cookie must be a single cookie value".into());
    }
    Ok(())
}

/// Normalise a panel address into a bare origin + path prefix.
///
/// Operators paste three shapes: the panel root (`https://host`), the API root
/// (`https://host/api`) and the relay base the provider config uses
/// (`https://host/v1`). All three describe the same instance, so the `/api` and
/// `/v1` suffixes are dropped here and the adapter owns the paths it calls.
fn normalize_base_url(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("base_url is empty".into());
    }
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|e| format!("base_url is not a valid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "base_url must start with http:// or https://, not `{other}://`"
            ))
        }
    }
    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "base_url has no host".to_string())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("base_url must not embed credentials".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("base_url must not carry a query string or fragment".into());
    }

    let mut path = parsed.path().trim_end_matches('/').to_string();
    if path.ends_with("/v1") {
        path.truncate(path.len() - "/v1".len());
    }
    if path.ends_with("/api") {
        path.truncate(path.len() - "/api".len());
    }
    let path = path.trim_end_matches('/');

    let mut base = format!("{}://{host}", parsed.scheme());
    if let Some(port) = parsed.port() {
        base.push_str(&format!(":{port}"));
    }
    base.push_str(path);
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
    use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::Router;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    const BEARER: &str = "pat-123";

    // ── fake panel ───────────────────────────────────────────────────────

    fn envelope(data: Value) -> Value {
        json!({ "success": true, "message": "", "data": data })
    }

    fn failure(code: &str, message: &str) -> Value {
        json!({ "success": false, "code": code, "message": message })
    }

    fn self_payload() -> Value {
        envelope(json!({
            "id": 7,
            "username": "alice",
            "display_name": "Alice",
            "status": 1,
            "group": "default",
            "quota": 1_500_000,
            "used_quota": 500_000,
            "request_count": 42,
        }))
    }

    fn status_payload() -> Value {
        envelope(json!({
            "version": "v0.9.0",
            "system_name": "New API",
            "quota_per_unit": 500_000.0,
            "checkin_enabled": true,
            "turnstile_check": false,
            "self_use_mode_enabled": false,
            "display_in_currency": true,
            "usd_exchange_rate": 7.3,
        }))
    }

    #[derive(Debug, Clone)]
    struct Recorded {
        method: String,
        path: String,
        query: String,
        bearer: Option<String>,
        cookie: Option<String>,
    }

    struct MockState {
        required_bearer: Mutex<String>,
        requests: Mutex<Vec<Recorded>>,
        status_body: Mutex<Value>,
        /// When set, `/api/status` answers with this raw body instead of JSON.
        status_raw: Mutex<Option<String>>,
        status_hits: AtomicU64,
        self_body: Mutex<Value>,
        self_http_status: Mutex<u16>,
        /// Number of upcoming `/api/user/self` calls to reject with an expired
        /// token before answering normally.
        self_expired: AtomicI64,
        stat_body: Mutex<Value>,
        stat_http_status: Mutex<u16>,
        logs: Mutex<Vec<Value>>,
        log_total_override: Mutex<Option<i64>>,
        refresh_body: Mutex<Value>,
        refresh_status: Mutex<u16>,
        subscription_body: Mutex<Value>,
        subscription_status: Mutex<u16>,
        checkin_body: Mutex<Value>,
        checkin_post_body: Mutex<Value>,
    }

    impl Default for MockState {
        fn default() -> Self {
            Self {
                required_bearer: Mutex::new(BEARER.into()),
                requests: Mutex::new(Vec::new()),
                status_body: Mutex::new(status_payload()),
                status_raw: Mutex::new(None),
                status_hits: AtomicU64::new(0),
                self_body: Mutex::new(self_payload()),
                self_http_status: Mutex::new(200),
                self_expired: AtomicI64::new(0),
                stat_body: Mutex::new(envelope(json!({ "quota": 0, "rpm": 0, "tpm": 0 }))),
                stat_http_status: Mutex::new(200),
                logs: Mutex::new(Vec::new()),
                log_total_override: Mutex::new(None),
                refresh_body: Mutex::new(envelope(json!({
                    "access_token": "jwt-from-cookie",
                    "token_type": "Bearer",
                    "access_expires_at": 1_800_000_000,
                    "user": {
                        "id": 7,
                        "username": "alice",
                        "display_name": "Alice",
                        "status": 1,
                        "quota": 1_500_000,
                        "used_quota": 500_000,
                        "request_count": 42,
                    },
                }))),
                refresh_status: Mutex::new(200),
                subscription_body: Mutex::new(envelope(json!({
                    "billing_preference": "wallet",
                    "subscriptions": [],
                }))),
                subscription_status: Mutex::new(200),
                checkin_body: Mutex::new(envelope(json!({
                    "enabled": true,
                    "min_quota": 1000,
                    "max_quota": 10000,
                    "stats": { "checked_in_today": false, "checkin_count": 0, "total_checkins": 0, "total_quota": 0 },
                }))),
                checkin_post_body: Mutex::new(envelope(json!({
                    "quota_awarded": 5000,
                    "checkin_date": "2026-09-25",
                }))),
            }
        }
    }

    impl MockState {
        fn record(&self, method: &Method, uri: &Uri, headers: &HeaderMap) {
            let field = |name: HeaderName| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            };
            self.requests.lock().unwrap().push(Recorded {
                method: method.to_string(),
                path: uri.path().to_string(),
                query: uri.query().unwrap_or("").to_string(),
                bearer: field(AUTHORIZATION),
                cookie: field(COOKIE),
            });
        }

        fn default_body(&self, slot: &Mutex<Value>) -> Response {
            axum::Json(slot.lock().unwrap().clone()).into_response()
        }

        fn last(&self) -> Recorded {
            self.requests
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("the mock panel recorded no request")
        }

        fn hits(&self, path: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.path == path)
                .count()
        }
    }

    /// Reject the request unless it carries the expected bearer.
    fn authorize(state: &MockState, headers: &HeaderMap) -> Option<Response> {
        let raw = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let token = strip_bearer_prefix(raw);
        let expected = state.required_bearer.lock().unwrap().clone();
        if token.is_empty() || (!expected.is_empty() && token != expected) {
            return Some(
                (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(failure("AUTH_UNAUTHORIZED", "unauthorized")),
                )
                    .into_response(),
            );
        }
        None
    }

    fn query_param(query: &str, name: &str) -> Option<String> {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| value.to_string())
        })
    }

    async fn mock_status(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        state.status_hits.fetch_add(1, Ordering::SeqCst);
        if let Some(raw) = state.status_raw.lock().unwrap().clone() {
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(raw))
                .unwrap();
        }
        state.default_body(&state.status_body)
    }

    async fn mock_self(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        if let Some(response) = authorize(&state, &headers) {
            return response;
        }
        if state.self_expired.load(Ordering::SeqCst) > 0 {
            state.self_expired.fetch_sub(1, Ordering::SeqCst);
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(failure("AUTH_TOKEN_EXPIRED", "登录已过期")),
            )
                .into_response();
        }
        let status = StatusCode::from_u16(*state.self_http_status.lock().unwrap()).unwrap();
        let body = state.self_body.lock().unwrap().clone();
        (status, axum::Json(body)).into_response()
    }

    async fn mock_refresh(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        let status = *state.refresh_status.lock().unwrap();
        if status != 200 {
            return (
                StatusCode::from_u16(status).unwrap(),
                axum::Json(failure("AUTH_UNAUTHORIZED", "refresh token invalid")),
            )
                .into_response();
        }
        let mut response = state.default_body(&state.refresh_body);
        // NewAPI rotates the refresh cookie on every refresh.
        response.headers_mut().insert(
            SET_COOKIE,
            HeaderValue::from_static(
                "new_api_refresh=rotated-cookie; Path=/api/user/auth; HttpOnly",
            ),
        );
        response
    }

    async fn mock_stat(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        if let Some(response) = authorize(&state, &headers) {
            return response;
        }
        let status = *state.stat_http_status.lock().unwrap();
        if status != 200 {
            return (
                StatusCode::from_u16(status).unwrap(),
                axum::Json(failure("INTERNAL_ERROR", "stat unavailable")),
            )
                .into_response();
        }
        state.default_body(&state.stat_body)
    }

    async fn mock_logs(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        if let Some(response) = authorize(&state, &headers) {
            return response;
        }
        let query = uri.query().unwrap_or_default();
        let page: u32 = query_param(query, "p")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1)
            .max(1);
        let page_size: u32 = query_param(query, "page_size")
            .and_then(|value| value.parse().ok())
            .unwrap_or(20)
            .min(LOG_PAGE_SIZE);
        let logs = state.logs.lock().unwrap().clone();
        let total = state
            .log_total_override
            .lock()
            .unwrap()
            .unwrap_or(logs.len() as i64);
        let skip = usize::try_from((page - 1) * page_size).unwrap_or(usize::MAX);
        let items: Vec<Value> = logs
            .into_iter()
            .skip(skip)
            .take(page_size as usize)
            .collect();
        axum::Json(json!({
            "success": true,
            "message": "",
            "data": { "page": page, "page_size": page_size, "total": total, "items": items },
        }))
        .into_response()
    }

    async fn mock_subscription(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        if let Some(response) = authorize(&state, &headers) {
            return response;
        }
        let status = *state.subscription_status.lock().unwrap();
        if status != 200 {
            // Older instances have no such route; gin answers plain text.
            return Response::builder()
                .status(StatusCode::from_u16(status).unwrap())
                .header("content-type", "text/plain")
                .body(Body::from("404 page not found"))
                .unwrap();
        }
        state.default_body(&state.subscription_body)
    }

    async fn mock_checkin(
        State(state): State<Arc<MockState>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
    ) -> Response {
        state.record(&method, &uri, &headers);
        if let Some(response) = authorize(&state, &headers) {
            return response;
        }
        match method {
            Method::POST => state.default_body(&state.checkin_post_body),
            _ => state.default_body(&state.checkin_body),
        }
    }

    struct MockPanel {
        base_url: String,
        state: Arc<MockState>,
        task: JoinHandle<()>,
    }

    impl MockPanel {
        async fn start() -> Self {
            Self::start_with(MockState::default()).await
        }

        async fn start_with(state: MockState) -> Self {
            let state = Arc::new(state);
            let app = Router::new()
                .route("/api/status", get(mock_status))
                .route("/api/user/self", get(mock_self))
                .route("/api/user/auth/refresh", post(mock_refresh))
                .route("/api/log/self/stat", get(mock_stat))
                .route("/api/log/self", get(mock_logs))
                .route("/api/subscription/self", get(mock_subscription))
                .route("/api/user/checkin", get(mock_checkin).post(mock_checkin))
                .fallback(|| async {
                    (
                        StatusCode::NOT_FOUND,
                        axum::Json(failure("NOT_FOUND", "no such endpoint")),
                    )
                        .into_response()
                })
                .with_state(state.clone());

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Self {
                base_url: format!("http://{addr}"),
                state,
                task,
            }
        }

        /// An adapter carrying the panel's personal access token.
        fn adapter(&self) -> NewApiAdapter {
            self.adapter_with(|_| {})
        }

        /// An adapter with one knob changed; callers that want to start
        /// unauthenticated clear `api_key` themselves.
        fn adapter_with(&self, tweak: impl FnOnce(&mut NewApiConfig)) -> NewApiAdapter {
            let mut config = NewApiConfig {
                base_url: self.base_url.clone(),
                api_key: BEARER.into(),
                ..Default::default()
            };
            tweak(&mut config);
            NewApiAdapter::new(config).expect("adapter builds")
        }
    }

    impl Drop for MockPanel {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn account() -> AccountId {
        AccountId("acct-1".into())
    }

    // ── configuration ────────────────────────────────────────────────────

    #[test]
    fn provider_id_is_newapi() {
        let mock_base = NewApiConfig {
            base_url: "https://newapi.example.com".into(),
            ..Default::default()
        };
        let adapter = NewApiAdapter::new(mock_base).unwrap();
        assert_eq!(adapter.provider_id(), "newapi");
    }

    #[test]
    fn default_capabilities_cover_every_operation_the_adapter_implements() {
        let config = NewApiConfig {
            base_url: "https://newapi.example.com".into(),
            ..Default::default()
        };
        let adapter = NewApiAdapter::new(config).unwrap();
        let caps = adapter.capabilities();
        assert!(caps.supports_usage);
        assert!(caps.supports_quota);
        assert!(caps.supports_refresh);
        assert!(caps.supports_checkin);
        assert!(caps.supports_health_check);
    }

    #[test]
    fn configured_capabilities_win() {
        let config = NewApiConfig {
            base_url: "https://newapi.example.com".into(),
            capabilities: AccountCapabilities {
                supports_checkin: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let adapter = NewApiAdapter::new(config).unwrap();
        assert!(!adapter.capabilities().supports_checkin);
        assert!(!adapter.capabilities().supports_usage);
    }

    #[test]
    fn config_validate_accepts_panel_and_relay_shapes() {
        for base in [
            "https://newapi.example.com",
            "https://newapi.example.com/",
            "https://newapi.example.com/api",
            "https://newapi.example.com/api/",
            "https://newapi.example.com/v1",
            "http://127.0.0.1:3000",
        ] {
            let config = NewApiConfig {
                base_url: base.into(),
                ..Default::default()
            };
            assert!(config.validate().is_ok(), "rejected {base}");
        }
    }

    #[test]
    fn config_validate_rejects_unusable_base_urls() {
        let cases = [
            ("", "base_url is empty"),
            ("   ", "base_url is empty"),
            ("newapi.example.com", "base_url is not a valid URL"),
            ("ftp://newapi.example.com", "must start with http://"),
            ("file:///etc/passwd", "must start with http://"),
            (
                "https://user:secret@newapi.example.com",
                "must not embed credentials",
            ),
            ("https://newapi.example.com?key=1", "must not carry a query"),
            ("https://newapi.example.com#frag", "must not carry a query"),
        ];
        for (base, expected) in cases {
            let config = NewApiConfig {
                base_url: base.into(),
                ..Default::default()
            };
            let err = config.validate().unwrap_err();
            assert!(
                err.contains(expected),
                "for `{base}` expected `{expected}`, got `{err}`"
            );
        }
    }

    #[test]
    fn config_validate_rejects_bad_tuning_values() {
        let base = "https://newapi.example.com";
        let cases: [(NewApiConfig, &str); 4] = [
            (
                NewApiConfig {
                    connect_timeout_secs: 0,
                    ..Default::default()
                },
                "connect_timeout_secs",
            ),
            (
                NewApiConfig {
                    request_timeout_secs: 0,
                    ..Default::default()
                },
                "request_timeout_secs",
            ),
            (
                NewApiConfig {
                    usage_page_limit: 0,
                    ..Default::default()
                },
                "usage_page_limit",
            ),
            (
                NewApiConfig {
                    quota_per_unit: Some(0.0),
                    ..Default::default()
                },
                "quota_per_unit",
            ),
        ];
        for (mut config, expected) in cases {
            config.base_url = base.into();
            let err = config.validate().unwrap_err();
            assert!(err.contains(expected), "expected `{expected}`, got `{err}`");
        }
    }

    #[test]
    fn config_validate_rejects_secrets_that_cannot_ride_in_a_header() {
        let config = NewApiConfig {
            base_url: "https://newapi.example.com".into(),
            api_key: "pat-123\r\nX-Injected: 1".into(),
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("control characters"), "got `{err}`");
    }

    #[test]
    fn normalize_base_url_strips_panel_and_relay_suffixes() {
        let cases = [
            ("https://host", "https://host"),
            ("https://host/", "https://host"),
            ("https://host/api", "https://host"),
            ("https://host/api/", "https://host"),
            ("https://host/v1", "https://host"),
            ("https://host/api/v1", "https://host"),
            ("https://host:8443/", "https://host:8443"),
            ("https://host/panel", "https://host/panel"),
            ("  https://host/v1  ", "https://host"),
        ];
        for (raw, expected) in cases {
            assert_eq!(normalize_base_url(raw).unwrap(), expected, "for `{raw}`");
        }
    }

    #[test]
    fn new_rejects_an_invalid_base_url() {
        let config = NewApiConfig {
            base_url: "newapi.example.com".into(),
            ..Default::default()
        };
        match NewApiAdapter::new(config) {
            Ok(_) => panic!("an invalid base_url must be rejected"),
            Err(err) => assert!(matches!(err, Error::InvalidRequest(_)), "got {err:?}"),
        }
    }

    // ── authentication ───────────────────────────────────────────────────

    #[tokio::test]
    async fn authenticate_with_api_key_verifies_and_installs_the_session() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        assert!(!adapter.is_authenticated());

        let session = adapter
            .authenticate(NewApiAuth::ApiKey { key: BEARER.into() })
            .await
            .unwrap();

        assert_eq!(session.user_id.as_deref(), Some("7"));
        assert_eq!(session.username.as_deref(), Some("alice"));
        assert!(session.authenticated_at > 0);
        assert!(
            session.expires_at.is_none(),
            "personal tokens do not expire"
        );
        assert!(adapter.is_authenticated());

        let probe = panel.state.last();
        assert_eq!(probe.path, "/api/user/self");
        assert_eq!(
            probe.bearer.as_deref(),
            Some(&format!("Bearer {BEARER}")[..])
        );
    }

    #[tokio::test]
    async fn authenticate_strips_a_pasted_bearer_prefix() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        let session = adapter
            .authenticate(NewApiAuth::ApiKey {
                key: format!("Bearer {BEARER} "),
            })
            .await
            .unwrap();
        assert_eq!(session.username.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn authenticate_with_a_rejected_key_leaves_the_adapter_unauthenticated() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());

        let err = adapter
            .authenticate(NewApiAuth::ApiKey {
                key: "pat-wrong".into(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "got {err:?}");
        assert!(!adapter.is_authenticated());
    }

    #[tokio::test]
    async fn authenticate_with_cookie_exchanges_it_for_an_access_token() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        // The exchanged token, not the cookie, is what the panel will see next.
        *panel.state.required_bearer.lock().unwrap() = "jwt-from-cookie".into();

        let session = adapter
            .authenticate(NewApiAuth::Cookie {
                session_cookie: "raw-refresh-cookie".into(),
            })
            .await
            .unwrap();

        assert_eq!(session.user_id.as_deref(), Some("7"));
        assert_eq!(session.username.as_deref(), Some("alice"));
        assert_eq!(session.expires_at, Some(1_800_000_000));

        let refresh = panel.state.last();
        assert_eq!(refresh.path, "/api/user/auth/refresh");
        assert_eq!(refresh.method, "POST");
        assert_eq!(
            refresh.cookie.as_deref(),
            Some("new_api_refresh=raw-refresh-cookie")
        );
        assert!(refresh.bearer.is_none(), "refresh carries no bearer");

        // Later calls use the exchanged token, not the cookie.
        let quota = adapter.fetch_quota(&account()).await.unwrap();
        assert!(quota.remaining > 0.0);
        assert_eq!(
            panel.state.last().bearer.as_deref(),
            Some("Bearer jwt-from-cookie")
        );
    }

    #[tokio::test]
    async fn authenticate_with_cookie_accepts_a_pasted_cookie_pair() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        adapter
            .authenticate(NewApiAuth::Cookie {
                session_cookie: " new_api_refresh=raw-refresh-cookie ".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            panel.state.last().cookie.as_deref(),
            Some("new_api_refresh=raw-refresh-cookie")
        );
    }

    #[tokio::test]
    async fn authenticate_with_oauth2_uses_the_access_token_as_the_bearer() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        *panel.state.required_bearer.lock().unwrap() = "bearer-jwt".into();

        let session = adapter
            .authenticate(NewApiAuth::OAuth2 {
                access_token: "bearer-jwt".into(),
                refresh_token: Some("new_api_refresh=cookie-jwt".into()),
            })
            .await
            .unwrap();

        assert_eq!(session.username.as_deref(), Some("alice"));
        let probe = panel.state.last();
        assert_eq!(probe.bearer.as_deref(), Some("Bearer bearer-jwt"));
    }

    #[tokio::test]
    async fn authenticate_rejects_empty_credentials() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();

        let cases = [
            (
                NewApiAuth::Cookie {
                    session_cookie: String::new(),
                },
                "empty session cookie",
            ),
            (NewApiAuth::ApiKey { key: "  ".into() }, "empty API key"),
            (
                NewApiAuth::OAuth2 {
                    access_token: String::new(),
                    refresh_token: None,
                },
                "empty access token",
            ),
        ];
        for (auth, expected) in cases {
            let err = adapter.authenticate(auth).await.unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected `{expected}`, got `{err}`"
            );
        }
    }

    #[tokio::test]
    async fn authenticate_rejects_a_cookie_that_tries_to_add_another_one() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        let err = adapter
            .authenticate(NewApiAuth::Cookie {
                session_cookie: "good; admin=1".into(),
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("single cookie value"),
            "got `{err}`"
        );
    }

    #[tokio::test]
    async fn calls_without_a_credential_fail_before_any_request() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        let err = adapter.fetch_quota(&account()).await.unwrap_err();
        assert!(matches!(err, Error::MissingApiKey(_)), "got {err:?}");
        assert_eq!(panel.state.hits("/api/user/self"), 0);
    }

    #[tokio::test]
    async fn an_expired_access_token_is_renewed_once_and_the_call_retried() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        // Accept the stale token for authentication itself, then expire it.
        panel.state.required_bearer.lock().unwrap().clear();
        adapter
            .authenticate(NewApiAuth::OAuth2 {
                access_token: "stale-jwt".into(),
                refresh_token: Some("cookie-jwt".into()),
            })
            .await
            .unwrap();
        panel.state.self_expired.store(1, Ordering::SeqCst);

        let quota = adapter.fetch_quota(&account()).await.unwrap();

        assert!((quota.remaining - 3.0).abs() < 1e-9);
        assert_eq!(panel.state.hits("/api/user/auth/refresh"), 1);
        assert_eq!(
            panel.state.last().bearer.as_deref(),
            Some("Bearer jwt-from-cookie"),
            "the retry uses the renewed token"
        );
    }

    #[tokio::test]
    async fn a_personal_token_is_never_renewed_because_it_cannot_be() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();
        panel.state.self_expired.store(1, Ordering::SeqCst);

        let err = adapter.fetch_quota(&account()).await.unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "got {err:?}");
        assert_eq!(panel.state.hits("/api/user/auth/refresh"), 0);
    }

    // ── status and quota ─────────────────────────────────────────────────

    #[tokio::test]
    async fn probe_status_reads_the_public_endpoint_without_a_bearer() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());

        let status = adapter.probe_status().await.unwrap();

        assert_eq!(status.version.as_deref(), Some("v0.9.0"));
        assert_eq!(status.system_name.as_deref(), Some("New API"));
        assert_eq!(status.quota_per_unit_or_default(), 500_000.0);
        assert_eq!(status.checkin_enabled, Some(true));
        assert!(panel.state.last().bearer.is_none());
    }

    #[tokio::test]
    async fn refresh_maps_a_healthy_account_into_usd() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.account_id, account());
        assert_eq!(runtime.provider_id, "newapi");
        assert_eq!(runtime.status, AccountStatus::Active);
        let quota = runtime.quota.unwrap();
        assert!((quota.remaining - 3.0).abs() < 1e-9, "{quota:?}");
        assert!((quota.used - 1.0).abs() < 1e-9, "{quota:?}");
        assert!((quota.total - 4.0).abs() < 1e-9, "{quota:?}");
        assert_eq!(quota.unit, "USD");
        assert!(
            runtime.usage.is_none(),
            "usage costs a log sweep; refresh skips it"
        );
        assert!(runtime.last_sync.unwrap() > 0);

        assert_eq!(runtime.metadata.get("newapi.username").unwrap(), "alice");
        assert_eq!(runtime.metadata.get("newapi.quota_raw").unwrap(), "1500000");
        assert_eq!(
            runtime.metadata.get("newapi.used_quota_raw").unwrap(),
            "500000"
        );
        assert_eq!(runtime.metadata.get("newapi.request_count").unwrap(), "42");
        assert_eq!(runtime.metadata.get("newapi.group").unwrap(), "default");
        assert_eq!(runtime.metadata.get("newapi.version").unwrap(), "v0.9.0");
    }

    #[tokio::test]
    async fn refresh_fills_rpm_and_tpm_from_the_rate_probe() {
        let state = MockState::default();
        *state.stat_body.lock().unwrap() =
            envelope(json!({ "quota": 1234, "rpm": 12, "tpm": 3456 }));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        let rate = runtime.rate_limit.unwrap();
        assert_eq!(rate.rpm_used, Some(12));
        assert_eq!(rate.tpm_used, Some(3456));
        assert_eq!(
            rate.rpm_limit, None,
            "newapi exposes no per-account ceiling"
        );
        assert_eq!(rate.pressure(), 0.0);
        let probe = panel.state.last();
        assert_eq!(probe.path, "/api/log/self/stat");
        assert!(probe.query.contains("type=2"));
    }

    #[tokio::test]
    async fn refresh_survives_a_failing_rate_probe() {
        let state = MockState::default();
        // The account state is readable; only the optional rate probe breaks.
        *state.stat_http_status.lock().unwrap() = 500;
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.status, AccountStatus::Active);
        assert!(runtime.rate_limit.is_none());
        assert_eq!(panel.state.hits("/api/user/self"), 1);
    }

    #[tokio::test]
    async fn a_disabled_user_maps_to_suspended() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = envelope(json!({
            "id": 7, "username": "alice", "status": 2, "quota": 0, "used_quota": 0, "request_count": 0,
        }));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.status, AccountStatus::Suspended);
        assert_eq!(panel.state.hits("/api/subscription/self"), 0);
    }

    #[tokio::test]
    async fn an_empty_wallet_without_a_subscription_maps_to_quota_exhausted() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = envelope(json!({
            "id": 7, "username": "alice", "status": 1, "quota": 0, "used_quota": 900_000, "request_count": 9,
        }));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.status, AccountStatus::QuotaExhausted);
        assert_eq!(panel.state.hits("/api/subscription/self"), 1);
        assert!((runtime.quota.unwrap().remaining).abs() < 1e-9);
    }

    #[tokio::test]
    async fn an_empty_wallet_funded_by_a_subscription_stays_active() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = envelope(json!({
            "id": 7, "username": "alice", "status": 1, "quota": 0, "used_quota": 900_000, "request_count": 9,
        }));
        *state.subscription_body.lock().unwrap() = envelope(json!({
            "billing_preference": "subscription",
            "subscriptions": [{ "id": 3, "plan_title": "Pro", "amount_total": 100 }],
        }));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.status, AccountStatus::Active);
    }

    #[tokio::test]
    async fn an_instance_without_the_subscription_api_treats_the_wallet_as_the_only_funding() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = envelope(json!({
            "id": 7, "username": "alice", "status": 1, "quota": 0, "used_quota": 1, "request_count": 1,
        }));
        // An older panel answers the unknown route with plain-text 404, which
        // is not the dashboard envelope at all.
        *state.subscription_status.lock().unwrap() = 404;
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.status, AccountStatus::QuotaExhausted);
        assert_eq!(panel.state.hits("/api/subscription/self"), 1);
    }

    #[tokio::test]
    async fn self_use_mode_does_not_report_an_empty_wallet_as_exhausted() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = envelope(json!({
            "id": 7, "username": "alice", "status": 1, "quota": -5, "used_quota": 900_000, "request_count": 9,
        }));
        let mut status = status_payload();
        status["data"]["self_use_mode_enabled"] = json!(true);
        *state.status_body.lock().unwrap() = status;
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let runtime = adapter.refresh(&account()).await.unwrap();

        assert_eq!(runtime.status, AccountStatus::Active);
        assert_eq!(panel.state.hits("/api/subscription/self"), 0);
    }

    #[tokio::test]
    async fn quota_per_unit_can_be_overridden_by_configuration() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.quota_per_unit = Some(1_000_000.0));

        let quota = adapter.fetch_quota(&account()).await.unwrap();

        assert!((quota.remaining - 1.5).abs() < 1e-9, "{quota:?}");
        assert!((quota.total - 2.0).abs() < 1e-9, "{quota:?}");
        assert_eq!(
            adapter
                .refresh(&account())
                .await
                .unwrap()
                .metadata
                .get("newapi.quota_per_unit")
                .unwrap(),
            "1000000"
        );
    }

    #[tokio::test]
    async fn status_is_cached_across_refreshes() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();

        adapter.refresh(&account()).await.unwrap();
        adapter.refresh(&account()).await.unwrap();

        assert_eq!(panel.state.hits("/api/status"), 1);
        assert_eq!(panel.state.hits("/api/user/self"), 2);
    }

    #[tokio::test]
    async fn a_status_endpoint_without_quota_per_unit_falls_back_to_the_newapi_default() {
        let state = MockState::default();
        *state.status_body.lock().unwrap() = envelope(json!({ "version": "v0.8.0" }));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let quota = adapter.fetch_quota(&account()).await.unwrap();

        assert!((quota.remaining - 3.0).abs() < 1e-9, "{quota:?}");
    }

    // ── usage ────────────────────────────────────────────────────────────

    fn consume_log(tokens: i64) -> Value {
        json!({
            "prompt_tokens": tokens,
            "completion_tokens": 1,
            "quota": 100,
            "model_name": "gpt-4o",
            "created_at": 1_760_000_000,
            "type": 2,
        })
    }

    #[tokio::test]
    async fn fetch_usage_sums_every_page_of_consumption_logs() {
        let state = MockState::default();
        *state.stat_body.lock().unwrap() =
            envelope(json!({ "quota": 250_000, "rpm": 3, "tpm": 777 }));
        *state.logs.lock().unwrap() = (0..150).map(|_| consume_log(9)).collect();
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let usage = adapter.fetch_usage(&account()).await.unwrap();

        assert_eq!(usage.total_requests, 150);
        assert_eq!(usage.total_tokens, 150 * 10);
        assert!((usage.total_cost - 0.5).abs() < 1e-9, "{usage:?}");
        assert_eq!(usage.currency, "USD");
        let start = usage.period_start.unwrap();
        let end = usage.period_end.unwrap();
        assert_eq!(end - start, DEFAULT_USAGE_WINDOW_SECS);
        assert_eq!(
            panel.state.hits("/api/log/self"),
            2,
            "150 entries span 2 pages"
        );
    }

    #[tokio::test]
    async fn usage_report_reports_truncation_instead_of_guessing_totals() {
        let state = MockState::default();
        state
            .logs
            .lock()
            .unwrap()
            .extend((0..250).map(|_| consume_log(9)));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter_with(|config| config.usage_page_limit = 2);

        let report = adapter
            .usage_report(Some(1_000), Some(2_000))
            .await
            .unwrap();

        assert!(report.truncated);
        assert_eq!(report.total_requests, 250, "the count comes from the panel");
        assert_eq!(
            report.total_tokens,
            200 * 10,
            "tokens cover the pages fetched"
        );
        assert_eq!(report.period_start, 1_000);
        assert_eq!(report.period_end, 2_000);
        assert_eq!(report.rpm_used, None, "a zero rate is reported as unknown");
        assert_eq!(panel.state.hits("/api/log/self"), 2);
    }

    #[tokio::test]
    async fn usage_report_rejects_an_inverted_window() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();
        let err = adapter
            .usage_report(Some(2_000), Some(1_000))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("starts after it ends"),
            "got `{err}`"
        );
    }

    #[tokio::test]
    async fn usage_report_sends_the_consume_filter_and_the_window() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();

        adapter.usage_report(Some(100), Some(200)).await.unwrap();

        for request in panel.state.requests.lock().unwrap().iter() {
            if request.path.starts_with("/api/log/self") {
                assert!(request.query.contains("type=2"), "{request:?}");
                assert!(request.query.contains("start_timestamp=100"), "{request:?}");
                assert!(request.query.contains("end_timestamp=200"), "{request:?}");
            }
        }
    }

    #[tokio::test]
    async fn the_page_cap_bounds_a_window_with_more_logs_than_the_cap() {
        let state = MockState::default();
        // 300 logs while the caller only allows two pages: the sweep must stop
        // at the cap and admit that the token total is partial.
        *state.logs.lock().unwrap() = (0..300).map(|_| consume_log(9)).collect();
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter_with(|config| config.usage_page_limit = 2);

        let report = adapter.usage_report(None, None).await.unwrap();

        assert!(report.truncated);
        assert_eq!(report.total_requests, 300);
        assert_eq!(report.total_tokens, 200 * 10);
        assert_eq!(
            panel.state.hits("/api/log/self"),
            2,
            "the page cap bounds the sweep"
        );
    }

    #[tokio::test]
    async fn a_short_page_ends_the_sweep_even_when_the_total_looks_larger() {
        let state = MockState::default();
        *state.logs.lock().unwrap() = vec![consume_log(1)];
        *state.log_total_override.lock().unwrap() = Some(9_999);
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter_with(|config| config.usage_page_limit = 5);

        let report = adapter.usage_report(None, None).await.unwrap();

        assert_eq!(report.total_requests, 9_999, "the panel owns the count");
        assert_eq!(report.total_tokens, 2);
        assert!(
            !report.truncated,
            "an exhausted page list is not truncation"
        );
        assert_eq!(panel.state.hits("/api/log/self"), 2);
    }

    // ── health ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_check_without_a_credential_pings_the_public_status() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());

        let status = adapter.health_check(&account()).await.unwrap();

        assert_eq!(status, AccountStatus::Active);
        assert_eq!(panel.state.hits("/api/status"), 1);
        assert_eq!(panel.state.hits("/api/user/self"), 0);
    }

    #[tokio::test]
    async fn health_check_maps_tokens_and_users_to_account_states() {
        // Disabled user: the panel rejects the call with AUTH_USER_DISABLED.
        let state = MockState::default();
        state.required_bearer.lock().unwrap().clear();
        *state.self_http_status.lock().unwrap() = 401;
        *state.self_body.lock().unwrap() = failure("AUTH_USER_DISABLED", "banned");
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();
        assert_eq!(
            adapter.health_check(&account()).await.unwrap(),
            AccountStatus::Suspended
        );

        // Expired token: any other 401 means the credential needs renewing.
        let state = MockState::default();
        state.required_bearer.lock().unwrap().clear();
        *state.self_http_status.lock().unwrap() = 401;
        *state.self_body.lock().unwrap() = failure("AUTH_TOKEN_EXPIRED", "expired");
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();
        assert_eq!(
            adapter.health_check(&account()).await.unwrap(),
            AccountStatus::AuthenticationExpired
        );
    }

    #[tokio::test]
    async fn health_check_reports_the_panel_user_status() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter();
        assert_eq!(
            adapter.health_check(&account()).await.unwrap(),
            AccountStatus::Active
        );
        assert_eq!(panel.state.hits("/api/user/self"), 1);
    }

    // ── check-in ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn checkin_is_not_supported_when_the_instance_disables_it() {
        let state = MockState::default();
        let mut status = status_payload();
        status["data"]["checkin_enabled"] = json!(false);
        *state.status_body.lock().unwrap() = status;
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let result = adapter.checkin(&account()).await.unwrap();

        assert_eq!(result, AccountOpResult::NotSupported);
        assert_eq!(panel.state.hits("/api/user/checkin"), 0);
    }

    #[tokio::test]
    async fn checkin_is_not_supported_when_a_turnstile_challenge_is_required() {
        let state = MockState::default();
        let mut status = status_payload();
        status["data"]["turnstile_check"] = json!(true);
        *state.status_body.lock().unwrap() = status;
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        assert_eq!(
            adapter.checkin(&account()).await.unwrap(),
            AccountOpResult::NotSupported
        );
        assert_eq!(panel.state.hits("/api/user/checkin"), 0);
    }

    #[tokio::test]
    async fn checkin_passes_a_turnstile_response_when_the_caller_has_one() {
        let state = MockState::default();
        let mut status = status_payload();
        status["data"]["turnstile_check"] = json!(true);
        *state.status_body.lock().unwrap() = status;
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let result = adapter
            .checkin_with_turnstile(Some("challenge-response"))
            .await
            .unwrap();

        assert_eq!(result, AccountOpResult::Success);
        let post = panel.state.last();
        assert_eq!(post.method, "POST");
        assert_eq!(post.path, "/api/user/checkin");
        assert!(
            post.query.contains("turnstile=challenge-response"),
            "{post:?}"
        );
    }

    #[tokio::test]
    async fn checkin_is_a_no_op_when_today_was_already_claimed() {
        let state = MockState::default();
        *state.checkin_body.lock().unwrap() = envelope(json!({
            "enabled": true,
            "stats": { "checked_in_today": true },
        }));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let result = adapter.checkin(&account()).await.unwrap();

        assert_eq!(result, AccountOpResult::Success);
        assert_eq!(
            panel.state.hits("/api/user/checkin"),
            1,
            "only the status read"
        );
        assert_eq!(panel.state.last().method, "GET");
    }

    #[tokio::test]
    async fn checkin_reports_the_panel_message_when_it_refuses() {
        let state = MockState::default();
        *state.checkin_post_body.lock().unwrap() = failure("CHECKIN_FAILED", "今日已签到");
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let result = adapter.checkin(&account()).await.unwrap();

        assert_eq!(
            result,
            AccountOpResult::Failed("CHECKIN_FAILED: 今日已签到".into())
        );
    }

    // ── failure handling ─────────────────────────────────────────────────

    #[tokio::test]
    async fn transport_failures_surface_as_transport_errors() {
        // Bind and drop a port so nothing is listening on it.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let config = NewApiConfig {
            base_url: format!("http://{addr}"),
            api_key: BEARER.into(),
            connect_timeout_secs: 2,
            ..Default::default()
        };
        let adapter = NewApiAdapter::new(config).unwrap();

        let err = adapter.probe_status().await.unwrap_err();
        assert!(matches!(err, Error::Transport { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn a_body_that_is_not_the_dashboard_envelope_is_rejected() {
        let state = MockState::default();
        *state.status_raw.lock().unwrap() = Some("<html>502 Bad Gateway</html>".into());
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let err = adapter.probe_status().await.unwrap_err();

        assert!(matches!(err, Error::BadUpstreamPayload(_)), "got {err:?}");
        assert!(
            !err.to_string().contains("Bad Gateway"),
            "body must not leak"
        );
    }

    #[tokio::test]
    async fn a_data_field_of_the_wrong_shape_is_rejected() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = envelope(json!("not an object"));
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let err = adapter.fetch_quota(&account()).await.unwrap_err();

        assert!(matches!(err, Error::BadUpstreamPayload(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_business_failure_keeps_its_code_and_message_for_the_log() {
        let state = MockState::default();
        *state.self_body.lock().unwrap() = failure("AUTH_USER_DISABLED", "用户已被封禁");
        let panel = MockPanel::start_with(state).await;
        let adapter = panel.adapter();

        let err = adapter.fetch_quota(&account()).await.unwrap_err();

        match &err {
            Error::Upstream {
                provider,
                status,
                body,
            } => {
                assert_eq!(provider, "newapi");
                assert_eq!(*status, 200);
                assert_eq!(body, "AUTH_USER_DISABLED: 用户已被封禁");
            }
            other => panic!("expected an upstream error, got {other:?}"),
        }
        assert_eq!(
            err.safe_message(),
            "upstream newapi returned 200",
            "the client-facing view stays redacted"
        );
    }

    #[tokio::test]
    async fn a_rejected_refresh_cookie_reads_as_unauthorized() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        *panel.state.refresh_status.lock().unwrap() = 401;

        let err = adapter
            .authenticate(NewApiAuth::Cookie {
                session_cookie: "stale".into(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "got {err:?}");
        assert!(!adapter.is_authenticated());
    }

    #[tokio::test]
    async fn a_rotated_refresh_cookie_is_kept_for_the_next_renewal() {
        let panel = MockPanel::start().await;
        let adapter = panel.adapter_with(|config| config.api_key = String::new());
        panel.state.required_bearer.lock().unwrap().clear();
        adapter
            .authenticate(NewApiAuth::OAuth2 {
                access_token: "stale-jwt".into(),
                refresh_token: Some("first-cookie".into()),
            })
            .await
            .unwrap();

        panel.state.self_expired.store(1, Ordering::SeqCst);
        adapter.fetch_quota(&account()).await.unwrap();

        // The panel answered the refresh with a rotated cookie; the next
        // renewal must present that one, not the original.
        panel.state.self_expired.store(1, Ordering::SeqCst);
        adapter.fetch_quota(&account()).await.unwrap();

        let refreshes: Vec<Recorded> = panel
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path == "/api/user/auth/refresh")
            .cloned()
            .collect();
        assert_eq!(refreshes.len(), 2);
        assert_eq!(
            refreshes[0].cookie.as_deref(),
            Some("new_api_refresh=first-cookie")
        );
        assert_eq!(
            refreshes[1].cookie.as_deref(),
            Some("new_api_refresh=rotated-cookie")
        );
    }

    // ── public helpers ───────────────────────────────────────────────────

    #[test]
    fn quota_conversion_never_divides_by_zero() {
        assert_eq!(quota_to_usd(1_000, 0.0), 0.0);
        assert_eq!(quota_to_usd(1_000, f64::NAN), 0.0);
        assert!((quota_to_usd(1_000_000, 500_000.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn newapi_auth_serialises_with_a_type_tag() {
        let value = serde_json::to_value(NewApiAuth::ApiKey { key: "k".into() }).unwrap();
        assert_eq!(value["type"], "api_key");
        assert_eq!(value["key"], "k");
        let parsed: NewApiAuth = serde_json::from_value(value).unwrap();
        assert!(matches!(parsed, NewApiAuth::ApiKey { .. }));
    }

    #[test]
    fn newapi_status_defaults_quota_per_unit_when_absent() {
        let status = NewApiStatus::default();
        assert_eq!(status.quota_per_unit_or_default(), DEFAULT_QUOTA_PER_UNIT);
        let status = NewApiStatus {
            quota_per_unit: Some(-1.0),
            ..Default::default()
        };
        assert_eq!(status.quota_per_unit_or_default(), DEFAULT_QUOTA_PER_UNIT);
    }
}
