//! The check-in lifecycle: what was attempted, what the provider said, and what
//! was actually observed.
//!
//! # Why execution and observation are separate
//!
//! A check-in that returns HTTP 200 has not established that the account
//! received anything. The panel accepted a request; whether a reward was granted
//! is a separate fact, and on several code paths it is carried in a different
//! endpoint than the one that answered.
//!
//! Collapsing the two produces the specific failure this module is built to
//! prevent: a panel that shows `+25.00` because an operation returned 200. That
//! number would be fabricated, and it would be fabricated in the direction an
//! operator most wants to believe.
//!
//! So a check-in produces two independent records. [`CheckinReport::phase`] says
//! what happened to the *operation*. [`CheckinReward`] says what an *observation*
//! established, and carries the [`RewardSource`] that established it, because
//! a figure read out of the operation's own response and a figure inferred from
//! two balance readings are not equally trustworthy and must not render the same.
//!
//! # The states that matter
//!
//! Three distinctions carry the whole design and each exists because collapsing
//! it produces a lie:
//!
//! * [`CheckinPhase::ProviderAccepted`] vs [`CheckinPhase::ObservationPending`] vs
//!   [`CheckinPhase::Confirmed`]. "The provider said yes" and "the reward landed"
//!   are different claims, and only the third is a resource observation.
//! * [`CheckinPhase::AlreadyCompleted`] is not a failure. A second check-in on
//!   the same day is the system working: the day's reward is already granted and
//!   its absence is expected, not a defect.
//! * [`CheckinPhase::NeedsUserAction`] is not a failure either. A WAF challenge
//!   that is waiting on a human is a paused operation with a live browser
//!   attached, not a dead one — see the desktop layer, which must be able to
//!   resume *that* session rather than start a new one.

use serde::{Deserialize, Serialize};

use super::resource::ObservedResourceEvent;
use super::resource::ResourceAmount;
use super::types::AccountId;

/// Where a check-in has got to.
///
/// Not a boolean and not a message: a panel has to be able to tell the user what
/// to do next, and that requires distinguishing "retry later", "press this
/// button", "nothing is wrong", and "this account is not eligible at all".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckinPhase {
    /// Accepted for execution; nothing has been sent to the provider yet.
    Requested,
    /// The operation is running.
    Executing,
    /// The provider accepted the operation.
    ///
    /// Says nothing about whether a resource effect was observed. The gap between
    /// this and [`CheckinPhase::Confirmed`] is the entire reason the two are
    /// separate states.
    ProviderAccepted,
    /// Accepted, and an observation is still outstanding.
    ///
    /// Reached when there is no observation surface to wait on, or when the
    /// provider's own eventual consistency has not caught up. Never a claim
    /// that anything was or was not granted.
    #[default]
    ObservationPending,
    /// A resource effect was observed and attributed to this check-in.
    Confirmed,
    /// The period's check-in was already done.
    ///
    /// A success state, not an error: no new reward is the correct outcome of
    /// checking in twice, so a report in this phase must not imply a lost reward.
    AlreadyCompleted,
    /// The account cannot check in: the provider does not offer it, or has it
    /// switched off.
    NotSupported,
    /// Blocked by a WAF or human-verification challenge, with the browser held
    /// open for the user to finish it.
    ///
    /// Not a failure. The operation is paused and resumable, which is why
    /// [`CheckinPhase::NeedsUserAction`] is reached from here.
    BlockedByWaf,
    /// Paused, waiting for the user to complete a challenge in the live browser.
    NeedsUserAction,
    /// The operation failed; [`CheckinReport::failure`] says how.
    Failed,
    /// Cancelled before it completed.
    Cancelled,
}

impl CheckinPhase {
    /// Whether no further progress will happen without something external.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Confirmed
                | Self::AlreadyCompleted
                | Self::NotSupported
                | Self::Failed
                | Self::Cancelled
        )
    }

    /// Whether the operation is paused awaiting the user rather than finished.
    ///
    /// Drives whether a panel offers "resume" or "try again", and those are
    /// different buttons: resuming continues the same live session, while
    /// retrying would start a new one and lose whatever the challenge produced.
    pub fn is_waiting_for_user(self) -> bool {
        matches!(self, Self::BlockedByWaf | Self::NeedsUserAction)
    }

    /// Whether a resource effect was established.
    ///
    /// True only for [`CheckinPhase::Confirmed`].
    /// [`CheckinPhase::AlreadyCompleted`] is deliberately excluded: it reports
    /// that the day was already handled, which is a different fact from this
    /// operation having caused a credit.
    pub fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }

    /// Whether repeating the operation could still succeed.
    pub fn is_retryable(self) -> bool {
        !self.is_terminal() && !self.is_waiting_for_user()
    }
}

/// Why a check-in did not complete.
///
/// Wider than [`super::provider::AccountOpResult`] on purpose. That type is
/// three cases and is the right shape for a trait default; this one is a
/// classification an operator can act on, and collapsing "sign in again" into
/// "failed" is what makes a check-in that cannot fix itself look like a bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CheckinFailure {
    /// The provider does not offer check-in at all.
    NotSupported,
    /// The provider offers check-in but has it switched off.
    Disabled,
    /// The credential is no longer usable.
    AuthenticationExpired,
    /// A WAF or human-verification challenge intercepted the operation.
    WafBlocked,
    /// The operation cannot continue until the user acts in the live browser.
    UserActionRequired,
    /// The request never completed.
    NetworkFailure,
    /// The provider answered, and refused.
    ProviderRejected {
        code: Option<String>,
        message: String,
    },
    /// The provider accepted but no observation arrived in time.
    ///
    /// Its own category rather than folded into [`Self::NetworkFailure`]: the
    /// check-in may well have succeeded, and retrying it would risk a second
    /// one, so this must not be presented as a safe retry.
    ObservationTimeout,
    /// Cancelled before completion.
    Cancelled,
    /// Not classified.
    Unknown { detail: String },
}

impl CheckinFailure {
    /// Whether retrying the same operation unchanged could plausibly work.
    ///
    /// False for everything that needs the user or the credential to change
    /// first. A scheduler that ignores this would re-attempt a check-in that
    /// cannot succeed and report the same failure forever.
    ///
    /// [`CheckinFailure::ObservationTimeout`] is deliberately **not** here. The
    /// check-in may well have succeeded and only its observation be missing, so
    /// retrying risks granting a second reward — and an automatic retry that
    /// duplicates a user's benefit is worse than one that does nothing.
    pub fn is_retryable_without_user(&self) -> bool {
        matches!(self, Self::NetworkFailure | Self::Unknown { .. })
    }

    /// Whether this needs a human before anything else can happen.
    pub fn needs_user(&self) -> bool {
        matches!(
            self,
            Self::WafBlocked | Self::UserActionRequired | Self::AuthenticationExpired
        )
    }
}

/// Where an observed reward amount came from.
///
/// Ordered by how much it can be trusted, and kept distinct because two of these
/// produce the same number and mean very different things. A figure read from a
/// structured provider field is a fact; one inferred from two balance readings is
/// arithmetic over a moving target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardSource {
    /// A structured field in the operation's own response.
    ProviderResponse,
    /// A structured field in an account-state endpoint read afterwards.
    ProviderRecord,
    /// Parsed from the text of an observed provider event.
    ///
    /// The weakest source, and deliberately so: provider log text is localised
    /// and forks reword it, so a number recovered from it is a fallback, never
    /// the primary confirmation.
    EventContent,
    /// Inferred from a balance reading taken before and after.
    ///
    /// Confounded by concurrent traffic: anything spent between the two readings
    /// lands in the difference. Never sufficient on its own to confirm.
    BalanceDelta,
}

impl RewardSource {
    /// Whether this source can establish a reward by itself.
    ///
    /// Only the structured ones can. The other two exist to corroborate, which
    /// is why a report built from them alone stays
    /// [`CheckinPhase::ObservationPending`].
    pub fn is_standalone_evidence(self) -> bool {
        matches!(self, Self::ProviderResponse | Self::ProviderRecord)
    }
}

/// A reward amount that some observation established.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckinReward {
    pub amount: ResourceAmount,
    pub source: RewardSource,
}

/// What a completed check-in established.
///
/// Separate from [`CheckinPhase`] because "confirmed" is a claim about the world
/// while the phase is a claim about the operation, and a check-in can reach a
/// terminal phase without this ever being filled in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "confirmation", rename_all = "snake_case")]
pub enum CheckinConfirmation {
    /// A reward was observed and attributed to this check-in.
    Confirmed { reward: CheckinReward },
    /// The provider accepted the operation and no observation contradicts it,
    /// but nothing observed establishes the effect yet.
    ///
    /// The honest state for a provider with no observation surface, and the one
    /// a panel must be able to render without implying a missing reward.
    ObservationPending,
    /// The period's check-in was already done.
    ///
    /// `reward` is the figure the provider reports for the *earlier* check-in
    /// when it reports one at all. Its absence is expected: no new reward was
    /// granted by this operation, and its absence here says nothing about
    /// whether the earlier one landed.
    AlreadyCompleted { reward: Option<CheckinReward> },
}

impl CheckinConfirmation {
    /// The reward this confirmation established, if any.
    pub fn reward(&self) -> Option<&CheckinReward> {
        match self {
            Self::Confirmed { reward } => Some(reward),
            Self::AlreadyCompleted { reward } => reward.as_ref(),
            Self::ObservationPending => None,
        }
    }
}

/// One check-in, from request through to whatever was observed.
///
/// Carries the observed events alongside the outcome rather than leaving them
/// in the provider, because the useful question after a check-in is "what did
/// the panel actually record", and an answer that requires a second round trip
/// to a log endpoint that may have since rotated is not an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckinReport {
    pub provider_id: String,
    pub account_id: AccountId,
    pub phase: CheckinPhase,
    pub failure: Option<CheckinFailure>,
    pub confirmation: Option<CheckinConfirmation>,
    /// Observed provider events from the window around this check-in.
    ///
    /// Not filtered to check-in-shaped events: a consumption that landed in the
    /// same window is evidence about the account, and discarding it here would
    /// lose the ability to explain a balance that moved for two reasons.
    #[serde(default)]
    pub observed_events: Vec<ObservedResourceEvent>,
    /// The specific event attributed to this check-in, when one was.
    #[serde(default)]
    pub reward_event: Option<ObservedResourceEvent>,
    /// When the operation was started (unix seconds).
    pub started_at: i64,
    /// When it reached a terminal phase (unix seconds).
    #[serde(default)]
    pub completed_at: Option<i64>,
}

impl CheckinReport {
    /// A report for an operation that has not been attempted.
    pub fn requested(provider_id: impl Into<String>, account_id: AccountId, at: i64) -> Self {
        Self {
            provider_id: provider_id.into(),
            account_id,
            phase: CheckinPhase::Requested,
            failure: None,
            confirmation: None,
            observed_events: Vec::new(),
            reward_event: None,
            started_at: at,
            completed_at: None,
        }
    }

    /// Set the terminal failure and stamp completion.
    pub fn failed(mut self, failure: CheckinFailure, at: i64) -> Self {
        self.phase = CheckinPhase::Failed;
        self.failure = Some(failure);
        self.completed_at = Some(at);
        self
    }

    /// Set the phase and stamp completion.
    pub fn settle(mut self, phase: CheckinPhase, at: i64) -> Self {
        self.phase = phase;
        self.completed_at = Some(at);
        self
    }

    /// The reward this report established, if any.
    pub fn reward(&self) -> Option<&CheckinReward> {
        self.confirmation
            .as_ref()
            .and_then(CheckinConfirmation::reward)
    }
}

/// What a panel needs to show and what it must be able to do.
///
/// Built from a report plus whether a live browser is being held open, because
/// those are the two questions the UI has: what happened, and is there something
/// waiting for me. Nothing secret is in here, and nothing that could identify a
/// session: a browser is referred to by whether it exists, never by its cookies
/// or its profile contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckinStatus {
    pub provider_id: String,
    pub account_id: AccountId,
    pub phase: CheckinPhase,
    pub failure: Option<CheckinFailure>,
    pub reward: Option<CheckinReward>,
    /// Whether a browser is being held open for this account.
    pub browser_held: bool,
    /// Whether the user is being asked to finish a challenge in it.
    pub awaiting_user: bool,
    /// The last time a check-in reached a terminal phase (unix seconds).
    pub last_completed_at: Option<i64>,
    /// The next time one is due, when a policy says so (unix seconds).
    pub next_due_at: Option<i64>,
    pub started_at: Option<i64>,
}

/// Whether an account's check-in is due.
///
/// A scheduler decision, not a timestamp. The reason it is an enum is that the
/// interesting cases are the ones where *no* check-in should happen, and a
/// `next_due_at: Option<i64>` cannot distinguish "not due", "never checked in,
/// and we do not know the interval" and "this account cannot check in" — all of
/// which are `None` and all of which mean something different.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum MaintenanceDecision {
    /// The account cannot check in, so maintenance is not applicable.
    NotApplicable,
    /// Check-in is switched off for this account.
    Disabled,
    /// Nothing has been observed yet and no interval is declared.
    ///
    /// Distinct from [`MaintenanceDecision::Due`]: with no interval and no
    /// history there is no evidence about when to act, and guessing "now" would
    /// make every freshly configured account check in immediately.
    NeedsFirstObservation,
    /// Due now, having been `since_last_secs` since the last completed attempt.
    Due { since_last_secs: i64 },
    /// Not due until `next_due_at` (unix seconds).
    NotDue { next_due_at: i64 },
}

impl MaintenanceDecision {
    /// Whether this decision authorises running check-in now.
    pub fn should_run(self) -> bool {
        matches!(self, Self::Due { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_confirmed_claims_a_resource_effect() {
        assert!(CheckinPhase::Confirmed.is_confirmed());
        // AlreadyCompleted is a success but did not cause a credit, and treating
        // it as confirmed would render a second check-in as a fresh +25.00.
        assert!(!CheckinPhase::AlreadyCompleted.is_confirmed());
        assert!(!CheckinPhase::ProviderAccepted.is_confirmed());
        assert!(!CheckinPhase::ObservationPending.is_confirmed());
        assert!(!CheckinPhase::Failed.is_confirmed());
    }

    #[test]
    fn a_waf_pause_is_not_a_terminal_failure() {
        assert!(CheckinPhase::BlockedByWaf.is_waiting_for_user());
        assert!(CheckinPhase::NeedsUserAction.is_waiting_for_user());
        assert!(!CheckinPhase::BlockedByWaf.is_terminal());
        assert!(
            !CheckinPhase::BlockedByWaf.is_retryable(),
            "resuming continues the held session; retrying would discard it"
        );
        assert!(CheckinPhase::Executing.is_retryable());
    }

    #[test]
    fn already_completed_is_a_terminal_success_not_a_failure() {
        assert!(CheckinPhase::AlreadyCompleted.is_terminal());
        assert!(!CheckinPhase::AlreadyCompleted.is_waiting_for_user());
        assert!(!CheckinPhase::AlreadyCompleted.is_retryable());
    }

    #[test]
    fn observation_pending_is_not_terminal_but_is_not_confirmed() {
        // It may still resolve, so it is not terminal; but nothing was
        // established, so it must never render as a confirmed reward.
        assert!(!CheckinPhase::ObservationPending.is_terminal());
        assert!(!CheckinPhase::ObservationPending.is_confirmed());
        assert!(CheckinPhase::ObservationPending.is_retryable());
    }

    #[test]
    fn only_user_dependent_failures_are_marked_as_needing_a_user() {
        assert!(CheckinFailure::WafBlocked.needs_user());
        assert!(CheckinFailure::UserActionRequired.needs_user());
        assert!(CheckinFailure::AuthenticationExpired.needs_user());
        assert!(!CheckinFailure::NetworkFailure.needs_user());
        assert!(!CheckinFailure::Disabled.needs_user());
        assert!(!CheckinFailure::ObservationTimeout.needs_user());
    }

    #[test]
    fn observation_timeout_is_not_safely_retryable() {
        // Retrying could grant a second reward, so it must not be advertised as
        // a safe automatic retry.
        assert!(!CheckinFailure::ObservationTimeout.is_retryable_without_user());
        assert!(CheckinFailure::NetworkFailure.is_retryable_without_user());
        assert!(!CheckinFailure::AuthenticationExpired.is_retryable_without_user());
    }

    #[test]
    fn only_structured_sources_stand_alone() {
        assert!(RewardSource::ProviderResponse.is_standalone_evidence());
        assert!(RewardSource::ProviderRecord.is_standalone_evidence());
        assert!(
            !RewardSource::EventContent.is_standalone_evidence(),
            "provider log text is localised and reworded by forks"
        );
        assert!(
            !RewardSource::BalanceDelta.is_standalone_evidence(),
            "a before/after balance difference is confounded by concurrent spend"
        );
    }

    #[test]
    fn an_already_completed_confirmation_reports_no_reward_of_its_own() {
        let none = CheckinConfirmation::AlreadyCompleted { reward: None };
        assert_eq!(none.reward(), None);

        let earlier = CheckinConfirmation::AlreadyCompleted {
            reward: Some(CheckinReward {
                amount: ResourceAmount::normalised(25.0, "USD"),
                source: RewardSource::ProviderRecord,
            }),
        };
        // A figure for the *earlier* check-in is carried, and is distinguishable
        // from one this operation caused by its source and by the phase.
        assert_eq!(
            earlier.reward().map(|r| r.source),
            Some(RewardSource::ProviderRecord)
        );
    }

    #[test]
    fn a_failure_report_stamps_completion_and_keeps_the_reason() {
        let report = CheckinReport::requested("p", AccountId("a".into()), 100).failed(
            CheckinFailure::ProviderRejected {
                code: Some("x".into()),
                message: "no".into(),
            },
            200,
        );
        assert_eq!(report.phase, CheckinPhase::Failed);
        assert_eq!(report.completed_at, Some(200));
        assert_eq!(report.started_at, 100);
        assert!(report.failure.is_some());
        assert_eq!(report.reward(), None);
    }

    #[test]
    fn an_observation_pending_report_has_no_reward() {
        let mut report = CheckinReport::requested("p", AccountId("a".into()), 1)
            .settle(CheckinPhase::ObservationPending, 2);
        report.confirmation = Some(CheckinConfirmation::ObservationPending);
        assert_eq!(report.reward(), None);
        assert!(!report.phase.is_confirmed());
    }

    #[test]
    fn a_scheduler_that_cannot_tell_why_not_run_is_not_usable() {
        // Each of these declines for a different reason, which is exactly why a
        // bare Option<i64> cannot express them.
        let decisions = [
            MaintenanceDecision::NotApplicable,
            MaintenanceDecision::Disabled,
            MaintenanceDecision::NeedsFirstObservation,
            MaintenanceDecision::NotDue { next_due_at: 9 },
        ];
        assert!(decisions.iter().all(|d| !d.should_run()));
        assert!(MaintenanceDecision::Due { since_last_secs: 0 }.should_run());
    }

    #[test]
    fn the_default_phase_is_observation_pending_not_confirmed() {
        // A default that meant "confirmed" would let an uninitialised report
        // claim a reward nobody observed.
        assert_eq!(CheckinPhase::default(), CheckinPhase::ObservationPending);
    }
}
