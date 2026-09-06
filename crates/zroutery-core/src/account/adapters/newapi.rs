//! NewAPI account adapter.
//!
//! Maps NewAPI provider endpoints to the generic AccountProvider interface.
//! NewAPI is one of many possible account sources — this adapter must not
//! leak NewAPI-specific semantics into the core.

use super::super::types::*;
use super::super::provider::*;
use crate::error::Result;

/// NewAPI-specific configuration.
#[derive(Debug, Clone)]
pub struct NewApiConfig {
    pub base_url: String,
    pub api_key: String,
    /// Which operations this NewAPI instance supports.
    pub capabilities: AccountCapabilities,
}

/// NewAPI account adapter.
pub struct NewApiAdapter {
    config: NewApiConfig,
}

impl NewApiAdapter {
    pub fn new(config: NewApiConfig) -> Self {
        Self { config }
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
}
