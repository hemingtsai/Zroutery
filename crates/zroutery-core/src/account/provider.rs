//! Account provider abstraction.
//!
//! Each provider adapter implements a subset of account operations.
//! Not all operations are required — the trait uses default implementations
//! that return "not supported".

use super::types::*;
use crate::error::Result;
use std::future::Future;

/// Result of an account operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountOpResult {
    Success,
    /// Operation not supported by this provider.
    NotSupported,
    /// Operation failed.
    Failed(String),
}

/// Provider-agnostic account operations.
///
/// Each async method has a default "not supported" implementation.
/// Adapters override only the operations they support.
///
/// Uses native async fn in trait (stable since Rust 1.75).
/// For dynamic dispatch (`dyn AccountProvider`), boxing via `async_trait`
/// or similar will be needed when the use-case arises.
///
/// That use-case has arrived — `account::reconcile` dispatches over a provider it
/// was handed — so the object-safe shim lives in [`super::reconcile`] and this
/// trait's methods are declared as `-> impl Future + Send` rather than `async fn`.
/// The `+ Send` is the load-bearing part: a bare `async fn` desugars to a future
/// with no `Send` guarantee, so a shim boxing it as `dyn Future + Send` would not
/// compile, and dropping `Send` from the shim instead would make every caller that
/// needs to move the future across a thread unable to. Implementations still write
/// `async fn`, which satisfies this signature unchanged.
#[allow(async_fn_in_trait)]
pub trait AccountProvider: Send + Sync {
    /// Provider identifier.
    fn provider_id(&self) -> &str;

    /// What operations this provider supports.
    fn capabilities(&self) -> AccountCapabilities;

    /// Refresh account state from the provider.
    fn refresh(
        &self,
        _account_id: &AccountId,
    ) -> impl Future<Output = Result<AccountRuntime>> + Send {
        std::future::ready(Err(crate::Error::internal("refresh not supported")))
    }

    /// Fetch current usage.
    fn fetch_usage(
        &self,
        _account_id: &AccountId,
    ) -> impl Future<Output = Result<AccountUsage>> + Send {
        std::future::ready(Err(crate::Error::internal("fetch_usage not supported")))
    }

    /// Fetch current quota.
    fn fetch_quota(
        &self,
        _account_id: &AccountId,
    ) -> impl Future<Output = Result<AccountQuota>> + Send {
        std::future::ready(Err(crate::Error::internal("fetch_quota not supported")))
    }

    /// Check account health.
    fn health_check(
        &self,
        _account_id: &AccountId,
    ) -> impl Future<Output = Result<AccountStatus>> + Send {
        std::future::ready(Err(crate::Error::internal("health_check not supported")))
    }

    /// Check in / ping the account (keep-alive, token refresh).
    async fn checkin(&self, _account_id: &AccountId) -> Result<AccountOpResult> {
        Ok(AccountOpResult::NotSupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal provider that uses all defaults.
    struct StubProvider;

    impl AccountProvider for StubProvider {
        fn provider_id(&self) -> &str {
            "stub"
        }

        fn capabilities(&self) -> AccountCapabilities {
            AccountCapabilities::default()
        }
    }

    /// A provider that overrides refresh and health_check only.
    struct PartialProvider;

    impl AccountProvider for PartialProvider {
        fn provider_id(&self) -> &str {
            "partial"
        }

        fn capabilities(&self) -> AccountCapabilities {
            AccountCapabilities {
                supports_refresh: true,
                supports_health_check: true,
                ..Default::default()
            }
        }

        async fn refresh(&self, account_id: &AccountId) -> Result<AccountRuntime> {
            Ok(AccountRuntime {
                account_id: account_id.clone(),
                provider_id: "partial".into(),
                status: AccountStatus::Active,
                ..Default::default()
            })
        }

        async fn health_check(&self, _account_id: &AccountId) -> Result<AccountStatus> {
            Ok(AccountStatus::Active)
        }
    }

    #[test]
    fn stub_provider_id_and_capabilities() {
        let p = StubProvider;
        assert_eq!(p.provider_id(), "stub");
        let caps = p.capabilities();
        assert!(!caps.supports_usage);
        assert!(!caps.supports_quota);
        assert!(!caps.supports_refresh);
        assert!(!caps.supports_checkin);
        assert!(!caps.supports_health_check);
    }

    #[tokio::test]
    async fn stub_defaults_return_not_supported() {
        let p = StubProvider;
        let id = AccountId("test".into());

        // refresh returns internal error
        let err = p.refresh(&id).await.unwrap_err();
        assert!(err.to_string().contains("refresh not supported"));

        // fetch_usage returns internal error
        let err = p.fetch_usage(&id).await.unwrap_err();
        assert!(err.to_string().contains("fetch_usage not supported"));

        // fetch_quota returns internal error
        let err = p.fetch_quota(&id).await.unwrap_err();
        assert!(err.to_string().contains("fetch_quota not supported"));

        // health_check returns internal error
        let err = p.health_check(&id).await.unwrap_err();
        assert!(err.to_string().contains("health_check not supported"));

        // checkin returns NotSupported (not an error)
        let result = p.checkin(&id).await.unwrap();
        assert_eq!(result, AccountOpResult::NotSupported);
    }

    #[tokio::test]
    async fn partial_provider_overrides_work() {
        let p = PartialProvider;
        let id = AccountId("acct-1".into());

        // refresh is overridden — returns a runtime
        let rt = p.refresh(&id).await.unwrap();
        assert_eq!(rt.account_id, AccountId("acct-1".into()));
        assert_eq!(rt.provider_id, "partial");
        assert_eq!(rt.status, AccountStatus::Active);

        // health_check is overridden — returns Active
        let status = p.health_check(&id).await.unwrap();
        assert_eq!(status, AccountStatus::Active);
    }

    #[tokio::test]
    async fn partial_provider_unsupported_still_defaults() {
        let p = PartialProvider;
        let id = AccountId("acct-1".into());

        // fetch_usage is NOT overridden — falls back to default
        let err = p.fetch_usage(&id).await.unwrap_err();
        assert!(err.to_string().contains("fetch_usage not supported"));

        // fetch_quota is NOT overridden — falls back to default
        let err = p.fetch_quota(&id).await.unwrap_err();
        assert!(err.to_string().contains("fetch_quota not supported"));

        // checkin is NOT overridden — falls back to default NotSupported
        let result = p.checkin(&id).await.unwrap();
        assert_eq!(result, AccountOpResult::NotSupported);
    }

    #[test]
    fn account_op_result_equality() {
        assert_eq!(AccountOpResult::Success, AccountOpResult::Success);
        assert_eq!(AccountOpResult::NotSupported, AccountOpResult::NotSupported);
        assert_eq!(
            AccountOpResult::Failed("x".into()),
            AccountOpResult::Failed("x".into())
        );
        assert_ne!(AccountOpResult::Success, AccountOpResult::NotSupported);
        assert_ne!(
            AccountOpResult::Failed("a".into()),
            AccountOpResult::Failed("b".into())
        );
    }
}
