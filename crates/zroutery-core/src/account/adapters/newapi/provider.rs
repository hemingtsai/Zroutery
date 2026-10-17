//! The provider-agnostic surface: what the rest of the core sees.

use crate::account::provider::*;

use crate::account::types::*;
use crate::error::Result;

use super::helpers::*;
use super::wire::*;
use super::NewApiAdapter;
use super::PROVIDER_ID;

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
