//! Check-in on NewAPI: what the operation did, and what was observed afterwards.
//!
//! # The split this module is built around
//!
//! Check-in has two halves that fail independently, and Zroutery needs both.
//! The **operation** either reached the panel and was accepted, refused, or
//! intercepted. The **observation** either shows a reward landed or does not.
//! Upstream is explicit that these are separate facts: `DoCheckin` writes the
//! reward into `users.quota` inside a transaction *before* it calls
//! `RecordLog(userId, model.LogTypeSystem, ...)`, and the log row it writes
//! leaves `quota` at zero.
//!
//! That ordering is the whole argument against reading a log row for the reward
//! amount. The amount is in `POST /api/user/checkin`'s own response
//! (`quota_awarded`), and in the `checkins` table the response echoes back. The
//! log row is the weakest of three sources for the same fact, and this module
//! uses it last.
//!
//! # Why the observation half is separately callable
//!
//! Because the final check-in has to run in a real browser (a headless client
//! cannot solve the Turnstile challenge some instances require), and a browser
//! the desktop layer drives is not something this adapter can call. So
//! [`NewApiAdapter::checkin_observed`] performs the whole observation half —
//! before/after status, balance, and the system-log window — and can be called
//! around an operation this adapter did not perform.
//!
//! It is what makes the four-layer split honest: the **browser** does the
//! operation, this module does the observation, the **resource** layer explains
//! what the observation means, and the **ML** layer never sees any of it.

use serde::Deserialize;

use crate::account::checkin::{
    CheckinConfirmation, CheckinFailure, CheckinPhase, CheckinReport, CheckinReward, RewardSource,
};
use crate::account::resource::ObservedResourceEvent;
use crate::account::resource::ResourceAmount;
use crate::account::resource::ResourceEventKind;
use crate::account::types::AccountId;

use super::logs::{read_system_signal, LogContext, NewApiLogItem, META_SYSTEM_REWARD_SHAPED};

/// `data` of `GET /api/user/checkin`.
///
/// Transcribed from `GetCheckinStatus` in `controller/checkin.go`, which returns
/// `enabled`, `min_quota`, `max_quota` and `stats`. Every field is optional
/// because older instances answer a narrower shape, and a missing field must not
/// become a false reading of zero.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiCheckinStatus {
    pub enabled: Option<bool>,
    pub min_quota: Option<i64>,
    pub max_quota: Option<i64>,
    pub stats: Option<NewApiCheckinStats>,
}

/// `stats` from [`NewApiCheckinStatus`], from `model.GetUserCheckinStats`.
///
/// This is the structured answer to "has this account checked in today, and what
/// did it get" — which is why it, and not the log text, is the primary
/// confirmation surface.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiCheckinStats {
    /// Lifetime quota awarded by check-ins, in credit units.
    pub total_quota: Option<i64>,
    pub total_checkins: Option<i64>,
    /// Check-ins in the requested month.
    pub checkin_count: Option<i64>,
    /// Whether today is already checked in. The decisive field.
    pub checked_in_today: Option<bool>,
    /// This month's records, newest first.
    pub records: Vec<NewApiCheckinRecord>,
}

/// One entry of `checkin_date` plus the credit units it awarded.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiCheckinRecord {
    /// `YYYY-MM-DD`, in the instance's own timezone.
    pub checkin_date: Option<String>,
    pub quota_awarded: Option<i64>,
}

/// `data` of a successful `POST /api/user/checkin`.
///
/// The structured reward amount, straight from the operation that granted it.
/// [`NewApiDoCheckin::quota_awarded`] is the highest-trust reward source
/// available: it is the grant itself rather than a report about it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiDoCheckin {
    /// Credit units awarded by this check-in.
    pub quota_awarded: Option<i64>,
    /// `YYYY-MM-DD` of the check-in performed.
    pub checkin_date: Option<String>,
}

/// What one check-in attempt did, before any observation.
///
/// Mirrors `CheckinPhase` and [`CheckinFailure`] rather than collapsing into
/// [`super::super::provider::AccountOpResult`], because that type cannot say
/// "the browser is still open on a challenge" or "the panel accepted it and we
/// are still waiting to see the reward" — and both of those are states an
/// operator has to act on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "attempt", rename_all = "snake_case")]
pub enum CheckinAttempt {
    /// The panel accepted the check-in and reported what it awarded.
    Accepted {
        /// Credit units the panel says it awarded.
        quota_awarded: Option<i64>,
        /// `YYYY-MM-DD`, as the panel recorded it.
        checkin_date: Option<String>,
    },
    /// The account had already checked in for this period.
    ///
    /// Not a failure. No new reward is the correct outcome of a second attempt,
    /// so nothing downstream should look for one.
    AlreadyCompleted {
        /// What the earlier check-in awarded, when the panel reports it.
        reward_quota: Option<i64>,
    },
    /// This instance does not offer check-in at all.
    NotSupported,
    /// The instance offers it and has it switched off.
    Disabled,
    /// A WAF or human-verification challenge intercepted the operation.
    ///
    /// Not a failure. The browser is held open, so the next step is a resume of
    /// *this* session rather than a new one.
    WafBlocked,
    /// The credential is no longer usable.
    AuthenticationExpired,
    /// The provider answered and refused.
    Rejected {
        code: Option<String>,
        message: String,
    },
    /// The request never completed.
    NetworkFailure { detail: String },
    /// The operation was cancelled before the panel answered.
    Cancelled,
    /// The attempt failed and no more specific reason was established.
    Unknown { detail: String },
}

impl CheckinAttempt {
    /// Whether the attempt reached the panel and was accepted.
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }
}

/// A reading of an account's check-in state at one moment.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewApiCheckinSnapshot {
    pub enabled: Option<bool>,
    pub checked_in_today: Option<bool>,
    pub total_checkins: Option<i64>,
    /// Lifetime credit units awarded by check-ins.
    pub total_quota: Option<i64>,
    pub records: Vec<NewApiCheckinRecord>,
    /// When this was read (unix seconds).
    pub read_at: i64,
}

impl NewApiCheckinSnapshot {
    /// Build a snapshot from the status payload, or `None` when the instance
    /// answered nothing usable.
    ///
    /// `None` rather than an empty snapshot, so "this instance has no check-in
    /// statistics" is distinguishable from "this account has checked in zero
    /// times" — the same distinction `AccountQuota: Option` exists to make.
    pub fn from_status(status: &NewApiCheckinStatus, read_at: i64) -> Option<Self> {
        let stats = status.stats.as_ref()?;
        Some(Self {
            enabled: status.enabled,
            checked_in_today: stats.checked_in_today,
            total_checkins: stats.total_checkins,
            total_quota: stats.total_quota,
            records: stats.records.clone(),
            read_at,
        })
    }

    /// The record for `date` (`YYYY-MM-DD`), if the instance reports one.
    pub fn record_for(&self, date: &str) -> Option<&NewApiCheckinRecord> {
        self.records
            .iter()
            .find(|record| record.checkin_date.as_deref() == Some(date))
    }
}

/// What was observed after a check-in attempt.
///
/// Every field is optional and each one is evidence, not a requirement. The
/// point of collecting them separately is that any subset can reach a
/// [`CheckinPhase`], and the phase says how much was actually established.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckinObservation {
    /// Check-in status read after the attempt.
    pub after: Option<NewApiCheckinSnapshot>,
    /// Check-in status read before the attempt, when one was taken.
    pub before: Option<NewApiCheckinSnapshot>,
    /// Remaining wallet quota after the attempt, in credit units.
    pub quota_after: Option<i64>,
    /// Remaining wallet quota before the attempt, in credit units.
    pub quota_before: Option<i64>,
    /// `type=4` rows observed in the window around the attempt.
    pub system_events: Vec<ObservedResourceEvent>,
    /// When the observation was made (unix seconds).
    pub observed_at: i64,
}

impl CheckinObservation {
    /// The account balance difference implied by the before/after readings.
    ///
    /// Returned as evidence and never as a confirmation on its own: any traffic
    /// during the window lands in the same difference, so a busy account would
    /// attribute spend to a check-in. [`RewardSource::BalanceDelta`] records
    /// that limitation, and [`reconcile`] refuses to confirm from it.
    pub fn balance_delta_credits(&self) -> Option<i64> {
        match (self.quota_before, self.quota_after) {
            (Some(before), Some(after)) => Some(after - before),
            _ => None,
        }
    }

    /// The first system event that looks like a check-in reward.
    ///
    /// Reads the recorded metadata rather than re-deriving it from the text, so
    /// the attribution is the same decision [`super::logs::interpret`] made
    /// rather than a second, possibly different, one.
    pub fn reward_event(&self) -> Option<&ObservedResourceEvent> {
        self.system_events.iter().find(|event| {
            event.event_kind == ResourceEventKind::SystemGrant
                && event.metadata_value(META_SYSTEM_REWARD_SHAPED) == Some("true")
                && read_system_signal(&event.description).indicates_checkin_reward()
        })
    }
}

/// Everything needed to read an observation, beyond the observation itself.
pub struct CheckinContext<'a> {
    pub provider_id: &'a str,
    pub account_id: &'a AccountId,
    /// Credit units per US dollar.
    pub quota_per_unit: f64,
    /// The unit normalised amounts are expressed in.
    pub unit: &'a str,
}

impl<'a> CheckinContext<'a> {
    /// A context reporting money in US dollars.
    pub fn usd(provider_id: &'a str, account_id: &'a AccountId, quota_per_unit: f64) -> Self {
        Self {
            provider_id,
            account_id,
            quota_per_unit,
            unit: "USD",
        }
    }

    fn to_unit(&self, credits: i64) -> f64 {
        if self.quota_per_unit <= 0.0 || !self.quota_per_unit.is_finite() {
            return 0.0;
        }
        credits as f64 / self.quota_per_unit
    }

    fn reward(&self, credits: i64, source: RewardSource) -> CheckinReward {
        CheckinReward {
            amount: ResourceAmount::normalised(self.to_unit(credits), self.unit)
                .with_provider_value(credits as f64),
            source,
        }
    }
}

/// Reconcile one attempt with what was observed into a report.
///
/// This is where "the provider said yes" and "the reward landed" are decided,
/// and the order of preference is the substance of the design:
///
/// 1. **The operation's own `quota_awarded`.** The grant itself.
/// 2. **A record in the post-attempt status for today's date.** Structured, and
///    it works even when the check-in ran in a browser this adapter never called.
/// 3. **A system log event in the window.** Corroboration only — upstream log
///    text is localised and reworded by forks, and its `quota` column is zero.
/// 4. **A balance difference.** Not confirmation on its own, for the reason in
///    [`CheckinObservation::balance_delta_credits`].
///
/// If none of those establish anything, the result is
/// [`CheckinPhase::ProviderAccepted`] followed by
/// [`CheckinPhase::ObservationPending`] — never [`CheckinPhase::Confirmed`]. An
/// accepted check-in on an instance that reports nothing further is a real and
/// common outcome, and reporting it as an unconfirmed success is the honest
/// answer; reporting it as a failure would push an operator to retry a check-in
/// that already worked.
pub fn reconcile(
    attempt: CheckinAttempt,
    observation: CheckinObservation,
    ctx: &CheckinContext<'_>,
    started_at: i64,
) -> CheckinReport {
    let report = CheckinReport {
        observed_events: observation.system_events.clone(),
        reward_event: observation.reward_event().cloned(),
        ..CheckinReport::requested(ctx.provider_id, ctx.account_id.clone(), started_at)
    };
    let observed_at = observation.observed_at;

    match attempt {
        CheckinAttempt::Accepted {
            quota_awarded,
            checkin_date,
        } => confirm_accepted(
            report,
            observation,
            ctx,
            quota_awarded,
            checkin_date,
            observed_at,
        ),
        other => settle_unaccepted(report, other, ctx, observed_at),
    }
}

/// Settle an attempt that never reached a state where a reward could be pending.
///
/// Split out from [`reconcile`] so the "the operation did not succeed" table and
/// the "the operation succeeded, now what did we observe" ladder are two
/// separate functions rather than one match with two different jobs.
fn settle_unaccepted(
    mut report: CheckinReport,
    attempt: CheckinAttempt,
    ctx: &CheckinContext<'_>,
    observed_at: i64,
) -> CheckinReport {
    match attempt {
        // Nothing was attempted and nothing needs watching.
        CheckinAttempt::NotSupported => report.settle(CheckinPhase::NotSupported, observed_at),
        // Same phase, a reason attached: a scheduler has to be able to tell "this
        // provider has no check-in" from "this provider has it switched off".
        CheckinAttempt::Disabled => {
            report.failure = Some(CheckinFailure::Disabled);
            report.settle(CheckinPhase::NotSupported, observed_at)
        }
        CheckinAttempt::Cancelled => {
            report.failure = Some(CheckinFailure::Cancelled);
            report.settle(CheckinPhase::Cancelled, observed_at)
        }
        // Not a failure and not terminal: the browser is held open, so the next
        // step is resuming *this* session.
        CheckinAttempt::WafBlocked => {
            report.failure = Some(CheckinFailure::WafBlocked);
            report.settle(CheckinPhase::BlockedByWaf, observed_at)
        }
        CheckinAttempt::AuthenticationExpired => {
            report.failed(CheckinFailure::AuthenticationExpired, observed_at)
        }
        CheckinAttempt::NetworkFailure { detail } => {
            // The provider's transport detail is not a user-facing message and can
            // name internals, so it goes to the log and the variant carries the
            // category alone.
            tracing::debug!(
                provider = ctx.provider_id,
                account = %ctx.account_id,
                detail = %detail,
                "newapi checkin transport failed"
            );
            report.failed(CheckinFailure::NetworkFailure, observed_at)
        }
        CheckinAttempt::Rejected { code, message } => report.failed(
            CheckinFailure::ProviderRejected { code, message },
            observed_at,
        ),
        CheckinAttempt::Unknown { detail } => {
            report.failed(CheckinFailure::Unknown { detail }, observed_at)
        }
        // Already done. No new reward is expected, and its absence is not a
        // missing observation — so this is a success and never `Confirmed`,
        // because this operation caused no credit.
        CheckinAttempt::AlreadyCompleted { reward_quota } => {
            report.confirmation = Some(CheckinConfirmation::AlreadyCompleted {
                reward: reward_quota.map(|quota| ctx.reward(quota, RewardSource::ProviderRecord)),
            });
            report.settle(CheckinPhase::AlreadyCompleted, observed_at)
        }
        // Handled by `reconcile`; listed so a new variant cannot be added without
        // this match failing to compile.
        CheckinAttempt::Accepted { .. } => report.failed(
            CheckinFailure::Unknown {
                detail: "accepted attempt reached the unaccepted path".into(),
            },
            observed_at,
        ),
    }
}

/// The ladder that decides whether an accepted check-in actually granted anything.
///
/// Ordered by source strength, and each step says what it could and could not
/// establish. See [`reconcile`] for why the order is what it is.
fn confirm_accepted(
    mut report: CheckinReport,
    observation: CheckinObservation,
    ctx: &CheckinContext<'_>,
    quota_awarded: Option<i64>,
    checkin_date: Option<String>,
    observed_at: i64,
) -> CheckinReport {
    // The panel accepted it. Whether a reward landed is still open, so the phase
    // stays non-terminal until one of the sources below settles it.
    report.phase = CheckinPhase::ProviderAccepted;

    // 1. The grant itself.
    if let Some(credits) = quota_awarded {
        report.confirmation = Some(CheckinConfirmation::Confirmed {
            reward: ctx.reward(credits, RewardSource::ProviderResponse),
        });
        return report.settle(CheckinPhase::Confirmed, observed_at);
    }

    // 2. The structured record the panel wrote. This is the path a browser-driven
    // check-in takes, because that path has no response figure to read.
    let date = checkin_date.or_else(|| {
        observation
            .after
            .as_ref()
            .and_then(|after| after.records.first())
            .and_then(|record| record.checkin_date.clone())
    });
    let recorded = date
        .as_deref()
        .and_then(|date| observation.after.as_ref().and_then(|a| a.record_for(date)))
        .and_then(|record| record.quota_awarded);
    if let Some(credits) = recorded {
        report.confirmation = Some(CheckinConfirmation::Confirmed {
            reward: ctx.reward(credits, RewardSource::ProviderRecord),
        });
        return report.settle(CheckinPhase::Confirmed, observed_at);
    }

    // 3. The status transition, with no amount attached. Before says not today and
    // after says today: that is more than nothing, and it is structured.
    let transitioned = observation
        .before
        .as_ref()
        .and_then(|before| before.checked_in_today)
        == Some(false)
        && observation
            .after
            .as_ref()
            .and_then(|after| after.checked_in_today)
            == Some(true);

    // 4. A system event in the window. Real corroboration, and its amount stays
    // metadata: the log row's own `quota` is zero and its text is in the
    // operator's display currency, so it cannot establish a balance figure.
    let corroborated = report.reward_event.is_some();

    // 5. A balance that grew. The weakest evidence available and recorded without
    // confirming, because concurrent spend would produce the same reading on a
    // busy account.
    let grew = matches!(observation.balance_delta_credits(), Some(delta) if delta > 0);

    report.confirmation = Some(CheckinConfirmation::ObservationPending);
    if transitioned || corroborated || grew {
        tracing::debug!(
            provider = ctx.provider_id,
            account = %ctx.account_id,
            transitioned,
            corroborated,
            grew,
            "newapi checkin accepted; no reward amount established"
        );
    }
    report.settle(CheckinPhase::ObservationPending, observed_at)
}

/// Read `checked_in_today` out of a status payload.
pub fn checked_in_today(status: &NewApiCheckinStatus) -> Option<bool> {
    status.stats.as_ref().and_then(|s| s.checked_in_today)
}

/// Read the credit units a successful check-in awarded.
pub fn awarded_quota(data: &NewApiDoCheckin) -> Option<i64> {
    data.quota_awarded
}

/// Whether a panel business-failure message reads as "already checked in".
///
/// A **corroborating** signal only. `model.UserCheckin` returns `今日已签到`, but
/// that text is localised and forks reword it, so this is consulted only after
/// the structured `stats.checked_in_today` has been considered, and never as the
/// sole reason to call a check-in complete. Matching the upstream string alone
/// would make every unrecognised wording into a hard failure on an account that
/// had in fact already checked in.
pub fn message_indicates_already_completed(message: &str) -> bool {
    let message = message.trim();
    if message.is_empty() {
        return false;
    }
    let lowered = message.to_lowercase();
    [
        "已签到",
        "already checked in",
        "already checked-in",
        "already signed",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

/// Build a `LogContext` for rows read while observing one account.
pub fn log_context<'a>(
    account_id: &'a AccountId,
    quota_per_unit: f64,
    unit: &'a str,
) -> LogContext<'a> {
    LogContext {
        account_id,
        quota_per_unit,
        unit,
    }
}

/// Read a page of system rows as events for one account.
///
/// Thin on purpose: the per-row work is [`super::logs::interpret`], and this
/// exists only so a caller reading a `type=4` window cannot accidentally apply a
/// `LogContext` built with the wrong conversion rate — the difference between a
/// reward that reads as `25.00` and one that reads as `500000.00`.
pub fn interpret_system_rows(
    rows: &[NewApiLogItem],
    account_id: &AccountId,
    quota_per_unit: f64,
) -> Vec<ObservedResourceEvent> {
    let ctx = LogContext::usd(account_id, quota_per_unit);
    rows.iter()
        .filter_map(|row| super::logs::interpret(row, &ctx))
        .collect()
}
