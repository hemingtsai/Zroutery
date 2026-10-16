//! Daily check-in: running it, and observing what it did.

use crate::account::checkin::CheckinReport;
use crate::account::resource::ObservedResourceEvent;
use crate::account::types::*;
use crate::error::Result;

use super::checkin;
use super::checkin::*;
use super::helpers::*;
use super::logs::*;
use super::NewApiAdapter;
use super::{DEFAULT_QUOTA_PER_UNIT, PROVIDER_ID};

impl NewApiAdapter {
    /// Read `GET /api/user/checkin` as a structured snapshot.
    ///
    /// Returns `Ok(None)` when the instance answered a payload with no `stats`
    /// at all, which is different from "this account has checked in zero times":
    /// the first means the instance offers no such statistics, the second is a
    /// measurement. Collapsing them would let an unsupported instance look like
    /// an account that has never checked in.
    pub async fn checkin_snapshot(&self) -> Result<Option<NewApiCheckinSnapshot>> {
        let now = chrono::Utc::now().timestamp();
        let status: NewApiCheckinStatus = self
            .request_typed(reqwest::Method::GET, "/api/user/checkin", &[])
            .await?;
        Ok(NewApiCheckinSnapshot::from_status(&status, now))
    }

    /// The account's remaining wallet quota, in credit units.
    ///
    /// Raw credits rather than the normalised [`AccountQuota`], because a
    /// check-in reconciliation compares two readings of the *same* unit before
    /// and after, and converting to a display currency in between would make the
    /// comparison depend on a rate that can itself be re-read.
    pub async fn wallet_quota(&self) -> Result<i64> {
        Ok(self.self_snapshot().await?.quota)
    }

    /// Read everything an observation needs, in one call.
    ///
    /// Three requests, each optional in the result: check-in status, the wallet
    /// balance, and the `type=4` system rows in `[since, until]`. Each is
    /// reported independently rather than failing the whole observation, because
    /// a partial observation is still evidence and reporting only the parts that
    /// arrived is more useful than reporting nothing.
    pub async fn observe_checkin(
        &self,
        account_id: &AccountId,
        before: Option<&NewApiCheckinSnapshot>,
        quota_before: Option<i64>,
        since: i64,
        until: i64,
    ) -> Result<CheckinObservation> {
        let after = self.checkin_snapshot().await.ok().flatten();
        let quota_after = self.wallet_quota().await.ok();
        let quota_per_unit = self
            .status_cached()
            .await
            .map(|s| self.effective_quota_per_unit(&s))
            .unwrap_or(DEFAULT_QUOTA_PER_UNIT);
        let system_events = self
            .system_events(account_id, since, until, quota_per_unit)
            .await
            .unwrap_or_default();

        Ok(CheckinObservation {
            after,
            before: before.cloned(),
            quota_after,
            quota_before,
            system_events,
            observed_at: chrono::Utc::now().timestamp(),
        })
    }

    /// Read `type=4` rows in `[since, until]` as account events.
    ///
    /// `type=4` specifically rather than "no type filter": an unfiltered read
    /// would return consumption rows too, and mixing them into a check-in window
    /// would put unrelated spend into the evidence for a grant.
    pub async fn system_events(
        &self,
        account_id: &AccountId,
        since: i64,
        until: i64,
        quota_per_unit: f64,
    ) -> Result<Vec<ObservedResourceEvent>> {
        let query = log_query(LOG_TYPE_SYSTEM, since, until);
        let page: NewApiLogPage = self
            .request_typed(reqwest::Method::GET, "/api/log/self", &query)
            .await?;
        Ok(checkin::interpret_system_rows(
            &page.items,
            account_id,
            quota_per_unit,
        ))
    }

    /// Run a check-in over HTTP and reconcile it against what was observed.
    ///
    /// The HTTP path, and deliberately **not** the one the product uses: a
    /// headless client cannot solve the Turnstile challenge some instances
    /// require, so the final check-in has to run in a real browser. This method
    /// exists because it is the only one that works end-to-end with no browser,
    /// which makes it the right thing to test the reconciliation against and the
    /// right thing for an instance that has no challenge enabled.
    ///
    /// For the browser path, the desktop layer performs the operation and then
    /// calls [`NewApiAdapter::observe_checkin`] plus [`checkin::reconcile`] with
    /// a [`CheckinAttempt`] describing what the browser did.
    pub async fn checkin_observed(&self, account_id: &AccountId) -> Result<CheckinReport> {
        let started_at = chrono::Utc::now().timestamp();
        let before = self.checkin_snapshot().await?;
        let quota_before = self.wallet_quota().await.ok();
        let attempt = self.checkin_attempt(&before).await;
        let until = chrono::Utc::now().timestamp();
        let observation = self
            .observe_checkin(account_id, before.as_ref(), quota_before, started_at, until)
            .await?;

        let status = self.status_cached().await.unwrap_or_default();
        let ctx = checkin::CheckinContext::usd(
            PROVIDER_ID,
            account_id,
            self.effective_quota_per_unit(&status),
        );
        Ok(checkin::reconcile(attempt, observation, &ctx, started_at))
    }

    /// Perform one `POST /api/user/checkin` and describe what it did.
    ///
    /// `before` is the status read taken beforehand. It is authoritative for
    /// "already completed" and the panel's own message is only corroboration —
    /// see [`checkin::message_indicates_already_completed`] for why.
    pub(super) async fn checkin_attempt(
        &self,
        before: &Option<NewApiCheckinSnapshot>,
    ) -> CheckinAttempt {
        let status = match self.status_cached().await {
            Ok(status) => status,
            Err(e) => return classify_failure(&e),
        };
        if status.checkin_enabled != Some(true) {
            return CheckinAttempt::NotSupported;
        }
        // With Turnstile on and no challenge response there is nothing to send:
        // a headless client cannot solve it, so this belongs to the browser path.
        if status.turnstile_check.unwrap_or(false) {
            return CheckinAttempt::WafBlocked;
        }
        if before.as_ref().and_then(|b| b.checked_in_today) == Some(true) {
            return CheckinAttempt::AlreadyCompleted {
                reward_quota: before
                    .as_ref()
                    .and_then(|b| b.records.first())
                    .and_then(|record| record.quota_awarded),
            };
        }

        match self
            .request_typed::<NewApiDoCheckin>(reqwest::Method::POST, "/api/user/checkin", &[])
            .await
        {
            Ok(data) => CheckinAttempt::Accepted {
                quota_awarded: checkin::awarded_quota(&data),
                checkin_date: data.checkin_date,
            },
            Err(e) => classify_failure(&e),
        }
    }
}
