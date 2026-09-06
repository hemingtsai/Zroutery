//! NewAPI account adapter.
//!
//! Maps NewAPI provider endpoints to the generic AccountProvider interface.
//! NewAPI is one of many possible account sources — this adapter must not
//! leak NewAPI-specific semantics into the core.

use super::super::types::*;
use super::super::provider::*;
use crate::error::Result;

/// Authentication method for NewAPI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NewApiAuth {
    /// Cookie-based session (existing browser session).
    Cookie { session_cookie: String },
    /// API key / password.
    ApiKey { key: String },
    /// OAuth2 token.
    OAuth2 {
        access_token: String,
        refresh_token: Option<String>,
    },
}

/// An authenticated session with NewAPI.
#[derive(Debug, Clone)]
pub struct AuthenticatedSession {
    pub auth: NewApiAuth,
    pub user_id: Option<String>,
    pub authenticated_at: i64,
    pub expires_at: Option<i64>,
}

/// NewAPI-specific configuration.
#[derive(Debug, Clone)]
pub struct NewApiConfig {
    pub base_url: String,
    pub api_key: String,
    /// Which operations this NewAPI instance supports.
    pub capabilities: AccountCapabilities,
}

impl NewApiConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.base_url.is_empty() {
            return Err("base_url is empty".into());
        }
        if !self.base_url.starts_with("http") {
            return Err("base_url must start with http:// or https://".into());
        }
        Ok(())
    }
}

/// NewAPI account adapter.
pub struct NewApiAdapter {
    config: NewApiConfig,
}

impl NewApiAdapter {
    pub fn new(config: NewApiConfig) -> Self {
        Self { config }
    }

    /// Authenticate with NewAPI using the provided auth method.
    pub async fn authenticate(&self, auth: NewApiAuth) -> Result<AuthenticatedSession> {
        // For now: validate that auth is non-empty
        match &auth {
            NewApiAuth::Cookie { session_cookie } if session_cookie.is_empty() => {
                return Err(crate::Error::invalid("empty session cookie"));
            }
            NewApiAuth::ApiKey { key } if key.is_empty() => {
                return Err(crate::Error::invalid("empty API key"));
            }
            NewApiAuth::OAuth2 {
                access_token, ..
            } if access_token.is_empty() => {
                return Err(crate::Error::invalid("empty access token"));
            }
            _ => {}
        }
        Ok(AuthenticatedSession {
            auth,
            user_id: None,
            authenticated_at: chrono::Utc::now().timestamp(),
            expires_at: None,
        })
    }
}

impl AccountProvider for NewApiAdapter {
    fn provider_id(&self) -> &str {
        "newapi"
    }

    fn capabilities(&self) -> AccountCapabilities {
        self.config.capabilities.clone()
    }

    async fn refresh(&self, account_id: &AccountId) -> Result<AccountRuntime> {
        // In real impl: GET /api/account/info
        // For now: return a basic runtime
        Ok(AccountRuntime {
            account_id: account_id.clone(),
            provider_id: "newapi".into(),
            status: AccountStatus::Active,
            capabilities: self.config.capabilities.clone(),
            ..Default::default()
        })
    }

    async fn fetch_usage(&self, _account_id: &AccountId) -> Result<AccountUsage> {
        // In real impl: GET /api/usage
        Err(crate::Error::internal("NewAPI usage fetch not yet implemented"))
    }

    async fn fetch_quota(&self, _account_id: &AccountId) -> Result<AccountQuota> {
        // In real impl: GET /api/quota or balance endpoint
        Err(crate::Error::internal("NewAPI quota fetch not yet implemented"))
    }

    async fn health_check(&self, _account_id: &AccountId) -> Result<AccountStatus> {
        // In real impl: ping NewAPI health endpoint
        Ok(AccountStatus::Active)
    }

    async fn checkin(&self, _account_id: &AccountId) -> Result<AccountOpResult> {
        Ok(AccountOpResult::NotSupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> NewApiAdapter {
        NewApiAdapter::new(NewApiConfig {
            base_url: "https://newapi.example.com".into(),
            api_key: "test-key".into(),
            capabilities: AccountCapabilities {
                supports_usage: true,
                supports_quota: true,
                supports_refresh: true,
                supports_checkin: false,
                supports_health_check: true,
            },
        })
    }

    #[test]
    fn provider_id_returns_newapi() {
        let adapter = make_adapter();
        assert_eq!(adapter.provider_id(), "newapi");
    }

    #[test]
    fn capabilities_returns_configured() {
        let adapter = make_adapter();
        let caps = adapter.capabilities();
        assert!(caps.supports_usage);
        assert!(caps.supports_quota);
        assert!(caps.supports_refresh);
        assert!(!caps.supports_checkin);
        assert!(caps.supports_health_check);
    }

    #[test]
    fn newapi_config_construction() {
        let config = NewApiConfig {
            base_url: "https://example.com".into(),
            api_key: "key".into(),
            capabilities: AccountCapabilities::default(),
        };
        assert_eq!(config.base_url, "https://example.com");
        assert_eq!(config.api_key, "key");
    }

    #[tokio::test]
    async fn refresh_returns_ok_with_correct_account_id() {
        let adapter = make_adapter();
        let id = AccountId("acct-42".into());
        let rt = adapter.refresh(&id).await.unwrap();
        assert_eq!(rt.account_id, AccountId("acct-42".into()));
        assert_eq!(rt.provider_id, "newapi");
        assert_eq!(rt.status, AccountStatus::Active);
    }

    #[tokio::test]
    async fn health_check_returns_active() {
        let adapter = make_adapter();
        let id = AccountId("acct-1".into());
        let status = adapter.health_check(&id).await.unwrap();
        assert_eq!(status, AccountStatus::Active);
    }

    #[tokio::test]
    async fn checkin_returns_not_supported() {
        let adapter = make_adapter();
        let id = AccountId("acct-1".into());
        let result = adapter.checkin(&id).await.unwrap();
        assert_eq!(result, AccountOpResult::NotSupported);
    }

    #[tokio::test]
    async fn fetch_usage_returns_error() {
        let adapter = make_adapter();
        let id = AccountId("acct-1".into());
        let err = adapter.fetch_usage(&id).await.unwrap_err();
        assert!(err.to_string().contains("NewAPI usage fetch not yet implemented"));
    }

    #[tokio::test]
    async fn fetch_quota_returns_error() {
        let adapter = make_adapter();
        let id = AccountId("acct-1".into());
        let err = adapter.fetch_quota(&id).await.unwrap_err();
        assert!(err.to_string().contains("NewAPI quota fetch not yet implemented"));
    }

    // ── authenticate ──

    #[tokio::test]
    async fn authenticate_with_cookie_ok() {
        let adapter = make_adapter();
        let session = adapter
            .authenticate(NewApiAuth::Cookie {
                session_cookie: "valid-session-id".into(),
            })
            .await
            .unwrap();
        assert!(session.user_id.is_none());
        assert!(session.expires_at.is_none());
        assert!(session.authenticated_at > 0);
    }

    #[tokio::test]
    async fn authenticate_with_api_key_ok() {
        let adapter = make_adapter();
        let session = adapter
            .authenticate(NewApiAuth::ApiKey {
                key: "sk-valid-key".into(),
            })
            .await
            .unwrap();
        assert!(session.user_id.is_none());
        assert!(session.authenticated_at > 0);
    }

    #[tokio::test]
    async fn authenticate_with_oauth2_ok() {
        let adapter = make_adapter();
        let session = adapter
            .authenticate(NewApiAuth::OAuth2 {
                access_token: "valid-token".into(),
                refresh_token: Some("refresh-me".into()),
            })
            .await
            .unwrap();
        assert!(session.user_id.is_none());
        assert!(session.authenticated_at > 0);
    }

    #[tokio::test]
    async fn authenticate_with_empty_cookie_err() {
        let adapter = make_adapter();
        let err = adapter
            .authenticate(NewApiAuth::Cookie {
                session_cookie: String::new(),
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty session cookie"));
    }

    #[tokio::test]
    async fn authenticate_with_empty_key_err() {
        let adapter = make_adapter();
        let err = adapter
            .authenticate(NewApiAuth::ApiKey {
                key: String::new(),
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty API key"));
    }

    #[tokio::test]
    async fn authenticate_with_empty_access_token_err() {
        let adapter = make_adapter();
        let err = adapter
            .authenticate(NewApiAuth::OAuth2 {
                access_token: String::new(),
                refresh_token: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty access token"));
    }

    #[tokio::test]
    async fn authenticated_session_has_correct_timestamp() {
        let before = chrono::Utc::now().timestamp();
        let adapter = make_adapter();
        let session = adapter
            .authenticate(NewApiAuth::ApiKey {
                key: "key".into(),
            })
            .await
            .unwrap();
        let after = chrono::Utc::now().timestamp();
        assert!(session.authenticated_at >= before);
        assert!(session.authenticated_at <= after);
    }

    // ── NewApiConfig::validate ──

    #[test]
    fn config_validate_valid() {
        let config = NewApiConfig {
            base_url: "https://newapi.example.com".into(),
            api_key: "key".into(),
            capabilities: AccountCapabilities::default(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_validate_empty_url_err() {
        let config = NewApiConfig {
            base_url: String::new(),
            api_key: "key".into(),
            capabilities: AccountCapabilities::default(),
        };
        let err = config.validate().unwrap_err();
        assert_eq!(err, "base_url is empty");
    }

    #[test]
    fn config_validate_no_scheme_err() {
        let config = NewApiConfig {
            base_url: "newapi.example.com".into(),
            api_key: "key".into(),
            capabilities: AccountCapabilities::default(),
        };
        let err = config.validate().unwrap_err();
        assert_eq!(err, "base_url must start with http:// or https://");
    }
}
