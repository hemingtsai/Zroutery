//! Constructing an adapter and turning a credential into a verified session.

use crate::account::provider::*;
use std::sync::atomic::AtomicU64;
use std::sync::RwLock;
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};

use super::helpers::*;
use super::logs::*;
use super::types::*;
use super::wire::*;
use super::NewApiAdapter;
use super::{DEFAULT_USAGE_WINDOW_SECS, LOG_PAGE_SIZE};

impl NewApiAdapter {
    /// Identity-only verification does not sweep logs, sign in, mint a token or
    /// infer account ownership from an unverified JWT/username.
    pub async fn verify_identity(&self, token: &str) -> Result<crate::config::RemoteIdentity> {
        let session = self
            .authenticate(NewApiAuth::ApiKey {
                key: token.to_string(),
            })
            .await?;
        let user_id = session
            .user_id
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                Error::BadUpstreamPayload(
                    "verified panel response contains no remote user id".into(),
                )
            })?;
        if self
            .config
            .new_api_user
            .as_deref()
            .is_some_and(|expected| expected != user_id)
        {
            return Err(Error::invalid(
                "the verified remote identity does not match New-Api-User",
            ));
        }
        Ok(crate::config::RemoteIdentity {
            panel_root: self.base.clone(),
            user_id,
            username: session.username,
            group: session.group,
            verified_at: session.authenticated_at,
            source: "GET /api/user/self (Rust HTTP, accepted credential)".into(),
        })
    }
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
                group: None,
                expires_at: None,
            })
        };

        Ok(Self {
            config,
            client,
            base,
            session: RwLock::new(session.map(|session| InstalledSession {
                session,
                generation: 0,
            })),
            session_generation: AtomicU64::new(0),
            refresh: tokio::sync::Mutex::new(()),
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
        let installed = self.install_session(session);
        Ok(AuthenticatedSession {
            auth,
            user_id: installed.session.user_id,
            username: installed.session.username,
            group: installed.session.group,
            authenticated_at,
            expires_at: installed.session.expires_at,
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

            let logs: NewApiLogPage = self
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
}
