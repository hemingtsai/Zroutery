//! Multiple simultaneous resource buckets with real temporal semantics.
//!
//! # What is missing from the account module, and why this is separate
//!
//! Audited at `5566915`, `account/types.rs` holds exactly one `AccountQuota`
//! (`Option<AccountQuota>` on `AccountRuntime`) with `total`/`used`/`remaining`,
//! a `unit: String` and a `resets_at` that `quota_from_self` hardcodes to `None`.
//! There is no expiry field, no reset-behaviour field, no charge order, and no way
//! for one account to hold a subscription *and* a promo balance *and* a pay-as-you-go
//! wallet at once. `AccountStore::upsert` replaces the whole runtime, so even the one
//! bucket it has is last-write-wins.
//!
//! None of that is rebuilt here. This module is the *planning-side* vocabulary that
//! the account adapters project into, kept behind the `ml` feature because the shipped
//! binary enables `ml` and not `account` — putting the planner's input types behind
//! `account` would make them unreachable in the build that actually ships.
//!
//! # The rules this type exists to enforce
//!
//! Four distinctions, each of which has caused a wrong decision somewhere:
//!
//! 1. **Entitlement window ≠ quota window.** A Token Plan can be valid until Friday
//!    while its allowance resets daily at 03:00. Collapsing these into one "expiry"
//!    makes the planner both spend too early and hoard too long.
//! 2. **`expires_at` ≠ `resets_at`.** Expiry is when value is *lost*; reset is when
//!    the counter restarts. A bucket that resets daily and never expires is the common
//!    case and must not be treated as expiring.
//! 3. **Unknown reset behaviour must stay unknown.** Assuming "resets means refills"
//!    is wrong for a clear-to-zero bucket near midnight and wrong for a carry-over
//!    bucket near month end. [`ResetBehavior::Unknown`] is the correct default and
//!    degrades to the existing routing policy rather than to a guess.
//! 4. **Buckets are not interchangeable and units do not convert.** A token-plan
//!    allowance and a USD balance are different resources with different economics.
//!    `unit` is carried verbatim and never summed across kinds — `budget.rs` already
//!    refuses to convert currencies for the same reason.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Resource kind
// ---------------------------------------------------------------------------

/// What kind of thing a bucket holds.
///
/// The kind is not cosmetic: it decides what an expiry means and how much a unit is
/// worth. Two buckets with the same number and different kinds are not comparable,
/// which is why every derived quantity in this module stays keyed by
/// `resource_id` rather than being aggregated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// Provider-granted free allowance.
    FreeAllowance,
    /// Promotional or bonus credit with its own expiry.
    PromotionalCredit,
    /// A subscription or prepaid token plan.
    TokenPlan,
    /// Post-paid money balance.
    PayAsYouGo,
    /// Allowance from a periodic check-in or daily claim.
    PeriodicGrant,
    /// Time-limited access to a premium model.
    PremiumModelEntitlement,
    /// A rate-limit budget rather than a spendable balance.
    RateBudget,
}

impl ResourceKind {
    /// Whether consuming this resource costs real money.
    ///
    /// Only `PayAsYouGo` does. Every other kind is someone's prior spend, a
    /// promotion, or a cap — and the planner's job differs completely depending on
    /// whether burning it is free, was paid for in advance, or is pure opportunity cost.
    pub fn is_real_money(self) -> bool {
        matches!(self, Self::PayAsYouGo)
    }

    /// Whether losing this bucket's remainder at expiry destroys value.
    ///
    /// True for anything expiring. False for a balance that simply stops being
    /// billable, which is a different loss from a promotional credit evaporating.
    pub fn expiry_destroys_value(self) -> bool {
        matches!(
            self,
            Self::FreeAllowance | Self::PromotionalCredit | Self::TokenPlan | Self::PeriodicGrant
        )
    }

    /// Whether the bucket is an access right rather than a spendable quantity.
    pub fn is_entitlement(self) -> bool {
        matches!(self, Self::PremiumModelEntitlement)
    }
}

// ---------------------------------------------------------------------------
// Reset behaviour
// ---------------------------------------------------------------------------

/// What happens to a bucket's remaining amount when its reset time arrives.
///
/// `Unknown` is a first-class variant, not a placeholder for "not yet implemented".
/// The three known behaviours differ so sharply near the boundary that assuming one is
/// a correctness bug, not a rounding error:
///
/// * `ClearToZero` — unspent allowance is destroyed at reset. Spending just before a
///   reset has near-zero opportunity cost, and *saving* for after it is pointless.
/// * `Refill` — the bucket returns to `total_amount`. Saving past a reset is strictly
///   valuable, and the pre-reset balance is about to be topped up regardless.
/// * `CarryOver` — reset does not reduce the balance. Near-reset scarcity is an
///   artefact and must not inflate the shadow price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetBehavior {
    /// The reset rule is not known. Degrade to the existing routing policy.
    #[default]
    Unknown,
    /// The remaining amount is destroyed and the bucket returns to zero.
    ClearToZero,
    /// The bucket is restored to its total.
    Refill,
    /// The remaining amount survives the reset.
    CarryOver,
    /// The window rolls: allowance is granted over time rather than all at once.
    RollingWindow,
}

impl ResetBehavior {
    /// Whether scarcity is about to be relieved regardless of what is spent now.
    pub fn relieves_scarcity(self) -> bool {
        matches!(self, Self::ClearToZero | Self::Refill)
    }

    /// Whether saving balance past the reset boundary is worthwhile.
    pub fn rewards_saving(self) -> bool {
        matches!(self, Self::Refill | Self::CarryOver)
    }

    /// Whether this bucket may be planned against at all.
    ///
    /// An unknown rule means the opportunity cost cannot be computed honestly, so the
    /// planner must fall back rather than invent a number. This is the single most
    /// important method in the type.
    pub fn is_plannable(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

// ---------------------------------------------------------------------------
// Observation source
// ---------------------------------------------------------------------------

/// How a bucket's contents became known.
///
/// Provider-reported facts and user-declared rules are different in kind: the first
/// was measured, the second is an assertion that may be wrong or stale. A bucket whose
/// expiry is user-declared is not "as good as" an observed one, and downstream
/// confidence reflects that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// The provider's API reported it.
    ProviderReported,
    /// The user configured it.
    UserConfigured,
    /// Derived from resource events.
    DerivedFromEvents,
    /// Inferred, not confirmed.
    Inferred,
}

impl ObservationSource {
    /// How much a bucket's *temporal* claims are trusted, before staleness.
    pub fn base_confidence(self) -> f64 {
        match self {
            Self::ProviderReported => 0.95,
            Self::DerivedFromEvents => 0.75,
            Self::UserConfigured => 0.60,
            Self::Inferred => 0.35,
        }
    }
}

// ---------------------------------------------------------------------------
// Resource bucket
// ---------------------------------------------------------------------------

/// One spendable or usable entitlement held by one account.
///
/// Every temporal claim is an `Option`, and the difference between `None` and a
/// timestamp is the difference between "this never expires" and "we do not know".
/// Those are not the same, so `None` is modelled as *unknown* throughout and callers
/// must decide explicitly; [`ResourceBucket::expiry_is_known`] exists so that decision
/// is visible at the call site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceBucket {
    pub resource_id: String,
    pub provider_id: String,
    pub account_id: String,
    pub kind: ResourceKind,
    /// Verbatim unit as the provider or user expressed it (`"tokens"`, `"usd"`,
    /// `"credits"`). Never normalised across kinds.
    pub unit: String,
    /// Spendable right now. Negative means the bucket is overdrawn.
    pub available_amount: f64,
    /// Full capacity of the bucket.
    pub total_amount: f64,
    /// When the entitlement or bucket stops being valid. `None` = unknown, which
    /// includes "never expires".
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// When the allowance counter restarts. `None` = unknown.
    #[serde(default)]
    pub resets_at: Option<i64>,
    #[serde(default)]
    pub reset_behavior: ResetBehavior,
    /// When the entitlement window opens, if it is not already open.
    #[serde(default)]
    pub valid_from: Option<i64>,
    /// Models this bucket may pay for. Empty means "no model restriction known",
    /// which is *not* the same as unrestricted.
    #[serde(default)]
    pub applicable_models: Vec<String>,
    /// Spend already committed by requests in flight but not yet settled.
    ///
    /// Without this every concurrent request independently sees the full balance and
    /// the account collectively overspends. Recorded as a commitment rather than
    /// subtracted from `available_amount` so that a failed request can release it.
    #[serde(default)]
    pub reserved_amount: f64,
    pub source: ObservationSource,
    /// Unix seconds this snapshot was taken. Staleness is measured against this, not
    /// against the process clock, so a replay is reproducible.
    pub observed_at: i64,
}

impl ResourceBucket {
    /// Whether the bucket's validity window has closed as of `now`.
    pub fn is_expired_at(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| now >= at)
    }

    /// Whether the bucket's validity window has not yet opened as of `now`.
    pub fn is_not_yet_valid_at(&self, now: i64) -> bool {
        self.valid_from.is_some_and(|from| now < from)
    }

    /// Whether the bucket can be used at `now`, ignoring balance and rate limits.
    pub fn is_valid_at(&self, now: i64) -> bool {
        !self.is_not_yet_valid_at(now) && !self.is_expired_at(now)
    }

    /// Whether the expiry claim is actually known.
    ///
    /// `None` is ambiguous between "never expires" and "not reported", so any caller
    /// planning value-dependent decisions has to make that call deliberately.
    pub fn expiry_is_known(&self) -> bool {
        self.expires_at.is_some()
    }

    /// Amount usable after in-flight commitments are subtracted.
    ///
    /// Can be negative when concurrent requests overcommitted, which is a real state
    /// and must not be clamped to zero — a clamped zero would hide the overspend.
    pub fn usable(&self) -> f64 {
        self.available_amount - self.reserved_amount
    }

    /// Whether the bucket can cover `amount` right now, accounting for reservations.
    pub fn can_cover(&self, amount: f64) -> bool {
        amount >= 0.0 && self.usable() >= amount
    }

    /// Seconds until expiry, if known. `None` when unknown.
    pub fn seconds_to_expiry(&self, now: i64) -> Option<i64> {
        self.expires_at.map(|at| at - now)
    }

    /// Whether the bucket's observed state is stale at `now`.
    ///
    /// A stale balance is not a small error: it is a balance that may already be
    /// spent, and spending against it twice is how a quota gets exceeded.
    pub fn is_stale_at(&self, now: i64, max_age_secs: i64) -> bool {
        now.saturating_sub(self.observed_at) > max_age_secs
    }

    /// Confidence in this bucket's temporal claims, combining source trust with age.
    ///
    /// Both factors apply because they are independent failures: a user-declared
    /// expiry read a second ago is still an assertion, and a provider-reported one read
    /// a week ago is still probably stale.
    pub fn temporal_confidence(&self, now: i64, max_age_secs: i64) -> f64 {
        let base = self.source.base_confidence();
        let age = now.saturating_sub(self.observed_at).max(0) as f64;
        // Linear decay to zero at twice the freshness bound, floored so a merely old
        // reading is discounted rather than discarded.
        let freshness = (1.0 - age / (2.0 * max_age_secs.max(1) as f64)).clamp(0.25, 1.0);
        base * freshness
    }

    /// Whether the bucket can be spent at `now`, on hard constraints alone.
    ///
    /// Validity window and funding are hard: a bucket that is closed or empty is
    /// simply not available, and no valuation changes that.
    ///
    /// An unknown `reset_behavior` is deliberately **not** an exclusion. It means the
    /// bucket's future value cannot be computed, not that it cannot be drawn on — and
    /// refusing to spend a pay-as-you-go wallet because its monthly rollover rule was
    /// never reported would be a worse answer than spending it at a conservative
    /// price. The unknown is handled where it belongs, in the planner's shadow price,
    /// by pricing the bucket dear and flagging the confidence low.
    pub fn is_usable_at(&self, now: i64, required_amount: f64) -> bool {
        self.is_valid_at(now) && self.can_cover(required_amount)
    }

    /// The reason a bucket cannot be used, for an explanation a user can act on.
    pub fn unusable_reason(&self, now: i64, required_amount: f64) -> Option<String> {
        if self.is_not_yet_valid_at(now) {
            return Some(format!(
                "entitlement window opens at {}",
                self.valid_from.unwrap_or_default()
            ));
        }
        if self.is_expired_at(now) {
            return Some(format!(
                "expired at {}",
                self.expires_at.unwrap_or_default()
            ));
        }
        if !self.can_cover(required_amount) {
            return Some(format!(
                "usable {} {} is short of required {required_amount}",
                self.usable(),
                self.unit
            ));
        }
        None
    }

    /// Whether this bucket's future value can be computed.
    ///
    /// Separate from [`ResourceBucket::is_usable_at`] because "usable" and "valueable"
    /// are different questions, and conflating them either blocks spending that should
    /// happen or spends spending that should not.
    pub fn is_valueable(&self) -> bool {
        self.reset_behavior.is_plannable()
    }
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// Whether an estimated charge matched what was actually observed.
///
/// Four outcomes, not two, because "the numbers differ" and "we cannot tell whether
/// the numbers differ" call for different responses: the first is a pricing bug to
/// fix, the second is a gap to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationVerdict {
    /// Estimated and observed agree within tolerance.
    Matched,
    /// Both figures exist and disagree.
    Mismatched,
    /// Only one side is available.
    Insufficient,
    /// A balance moved but the change cannot be attributed to this request.
    ///
    /// Distinct from `Mismatched`: something did change, but concurrent traffic means
    /// the difference is not evidence about this request. `account/checkin.rs` already
    /// calls this "confounded by concurrent traffic… never sufficient on its own".
    Unattributable,
}

/// One request's charge, seen from each side that can report it.
///
/// The four fields are deliberately separate numbers. Collapsing them into one
/// `cost` is what produces a "cost" that is sometimes an estimate, sometimes a
/// provider figure and sometimes a balance difference, with nothing recording which.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChargeReconciliation {
    /// What Zroutery predicted before sending.
    #[serde(default)]
    pub estimated: Option<f64>,
    /// Token usage the provider declared.
    #[serde(default)]
    pub declared_tokens: Option<u64>,
    /// What the provider recorded charging.
    #[serde(default)]
    pub provider_charged: Option<f64>,
    /// What the account's balance change implies.
    #[serde(default)]
    pub balance_delta: Option<f64>,
    pub verdict: ReconciliationVerdict,
    /// Relative disagreement between estimate and provider charge, when both exist.
    #[serde(default)]
    pub relative_error: Option<f64>,
}

impl ChargeReconciliation {
    /// Compare an estimate against an observed charge.
    ///
    /// A relative tolerance is used because the error that matters is proportional:
    /// a cent of drift on a five-cent request is not the same finding as a cent on a
    /// five-dollar one.
    pub fn compare(estimated: Option<f64>, provider_charged: Option<f64>, tolerance: f64) -> Self {
        let (estimated, provider_charged, relative_error) = match (estimated, provider_charged) {
            (Some(left), Some(right)) => {
                let scale = right.abs().max(1e-9);
                let error = (left - right).abs() / scale;
                (Some(left), Some(right), Some(error))
            }
            _ => (estimated, provider_charged, None),
        };
        let verdict = match (estimated, provider_charged) {
            (Some(left), Some(right)) => {
                let scale = right.abs().max(1e-9);
                if (left - right).abs() / scale <= tolerance {
                    ReconciliationVerdict::Matched
                } else {
                    ReconciliationVerdict::Mismatched
                }
            }
            // Exactly one side present: a real gap, not a zero.
            _ => ReconciliationVerdict::Insufficient,
        };
        Self {
            estimated,
            declared_tokens: None,
            provider_charged,
            balance_delta: None,
            verdict,
            relative_error,
        }
    }

    /// Record a balance observation that could not be attributed to this request.
    pub fn with_unattributable_balance(mut self, delta: f64) -> Self {
        self.balance_delta = Some(delta);
        self.verdict = ReconciliationVerdict::Unattributable;
        self
    }

    /// Whether the observed charge is trustworthy enough to bill against.
    ///
    /// An `Insufficient` or `Unattributable` verdict must never be read as "free".
    pub fn is_verified(&self) -> bool {
        self.verdict == ReconciliationVerdict::Matched && self.provider_charged.is_some()
    }
}

// ---------------------------------------------------------------------------
// Charge order
// ---------------------------------------------------------------------------

/// Which bucket to debit, and in what sequence.
///
/// An empty order means "unknown", not "no preference". Debiting a pay-as-you-go
/// wallet when a promotional credit was about to expire spends real money on value
/// the user already paid for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChargeOrder {
    /// `resource_id`s in debit priority, highest priority first.
    #[serde(default)]
    pub preference: Vec<String>,
    /// Whether the order is a confirmed fact or a default assumption.
    #[serde(default)]
    pub confirmed: bool,
}

impl ChargeOrder {
    /// An unknown order. Nothing may be inferred from it.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// A user- or provider-declared order.
    pub fn declared(preference: Vec<String>) -> Self {
        Self {
            preference,
            confirmed: true,
        }
    }

    /// The first bucket in the order that can currently pay, and can pay `amount`.
    ///
    /// Returns `None` when the order is unconfirmed. Planning against a guessed order
    /// would silently pick who pays, so an unconfirmed order is treated as no order.
    pub fn select<'b>(
        &self,
        buckets: &'b [ResourceBucket],
        amount: f64,
    ) -> Option<&'b ResourceBucket> {
        if !self.confirmed {
            return None;
        }
        // Walk the whole preference list rather than stopping at the first match: a
        // promo credit that cannot cover the request must fall through to the next
        // source rather than reporting "nothing to charge".
        self.preference.iter().find_map(|id| {
            buckets
                .iter()
                .find(|bucket| &bucket.resource_id == id)
                .filter(|bucket| bucket.can_cover(amount))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn bucket() -> ResourceBucket {
        ResourceBucket {
            resource_id: "bucket-1".into(),
            provider_id: "p".into(),
            account_id: "a".into(),
            kind: ResourceKind::TokenPlan,
            unit: "tokens".into(),
            available_amount: 1000.0,
            total_amount: 2000.0,
            expires_at: Some(NOW + 3600),
            resets_at: Some(NOW + 7200),
            reset_behavior: ResetBehavior::Refill,
            valid_from: None,
            applicable_models: vec![],
            reserved_amount: 0.0,
            source: ObservationSource::ProviderReported,
            observed_at: NOW,
        }
    }

    #[test]
    fn an_expiry_in_the_past_makes_a_bucket_unusable() {
        let mut b = bucket();
        b.expires_at = Some(NOW - 1);
        assert!(b.is_expired_at(NOW));
        assert!(!b.is_valid_at(NOW));
        assert!(!b.is_usable_at(NOW, 1.0));
        assert!(b.unusable_reason(NOW, 1.0).unwrap().contains("expired"));
    }

    #[test]
    fn an_entitlement_window_that_has_not_opened_is_not_yet_usable() {
        let mut b = bucket();
        b.valid_from = Some(NOW + 60);
        assert!(b.is_not_yet_valid_at(NOW));
        assert!(!b.is_valid_at(NOW));
        assert!(b.unusable_reason(NOW, 1.0).unwrap().contains("opens at"));
    }

    #[test]
    fn expiry_and_reset_are_independent_clocks() {
        // Daily reset, no expiry: the common real case. Treating the reset as an
        // expiry would make a permanently-valid plan look like it is about to vanish.
        let mut b = bucket();
        b.expires_at = None;
        b.resets_at = Some(NOW + 60);
        assert!(!b.is_expired_at(NOW + 60));
        assert!(b.is_valid_at(NOW + 60));
        assert!(!b.expiry_is_known());
    }

    #[test]
    fn an_unknown_reset_rule_degrades_valuation_without_blocking_spend() {
        let mut b = bucket();
        b.reset_behavior = ResetBehavior::Unknown;
        // Still spendable: unknown future value is not the same as being closed.
        assert!(b.is_usable_at(NOW, 1.0));
        assert!(b.unusable_reason(NOW, 1.0).is_none());
        // But its value cannot be computed, so the planner must price it conservatively.
        assert!(!b.is_valueable());
        assert!(!ResetBehavior::Unknown.is_plannable());
    }

    #[test]
    fn reset_behaviours_disagree_about_scarcity_and_saving() {
        assert!(ResetBehavior::ClearToZero.relieves_scarcity());
        assert!(ResetBehavior::Refill.relieves_scarcity());
        assert!(!ResetBehavior::CarryOver.relieves_scarcity());
        assert!(!ResetBehavior::ClearToZero.rewards_saving());
        assert!(ResetBehavior::CarryOver.rewards_saving());
    }

    #[test]
    fn concurrent_requests_cannot_each_spend_the_same_balance() {
        // Three requests each believing it alone may spend 500 of 1000.
        let mut b = bucket();
        b.available_amount = 1000.0;
        b.reserved_amount = 900.0;
        assert_eq!(b.usable(), 100.0);
        assert!(!b.can_cover(500.0));
        // Releasing a failed commitment restores the ability to spend.
        b.reserved_amount = 400.0;
        assert!(b.can_cover(500.0));
    }

    #[test]
    fn an_overspend_is_reported_rather_than_clamped_to_zero() {
        let mut b = bucket();
        b.reserved_amount = 1200.0;
        assert_eq!(b.usable(), -200.0);
    }

    #[test]
    fn a_model_restricted_bucket_does_not_imply_unrestricted() {
        let mut b = bucket();
        b.applicable_models = vec!["m-1".into()];
        assert!(!b.applicable_models.is_empty());
        // The field is descriptive; eligibility is decided by the caller that knows
        // the request. An empty list means "no restriction known", not "any model".
    }

    #[test]
    fn confidence_falls_with_staleness_and_with_a_weaker_source() {
        let fresh = bucket();
        let mut old = bucket();
        old.observed_at = NOW - 10_000;
        assert!(fresh.temporal_confidence(NOW, 3_600) > old.temporal_confidence(NOW, 3_600));

        let mut asserted = bucket();
        asserted.source = ObservationSource::UserConfigured;
        assert!(asserted.temporal_confidence(NOW, 3_600) < fresh.temporal_confidence(NOW, 3_600));
    }

    #[test]
    fn reconciliation_separates_disagreement_from_missing_information() {
        let matched = ChargeReconciliation::compare(Some(1.0), Some(1.0), 0.05);
        assert_eq!(matched.verdict, ReconciliationVerdict::Matched);
        assert!(matched.is_verified());

        let mismatched = ChargeReconciliation::compare(Some(1.0), Some(2.0), 0.05);
        assert_eq!(mismatched.verdict, ReconciliationVerdict::Mismatched);
        assert_eq!(mismatched.relative_error, Some(0.5));
        assert!(!mismatched.is_verified());

        // Missing information is never "free".
        let insufficient = ChargeReconciliation::compare(Some(1.0), None, 0.05);
        assert_eq!(insufficient.verdict, ReconciliationVerdict::Insufficient);
        assert!(!insufficient.is_verified());
    }

    #[test]
    fn an_unattributable_balance_change_is_not_a_measured_charge() {
        let r = ChargeReconciliation::compare(Some(1.0), Some(1.0), 0.05)
            .with_unattributable_balance(-7.0);
        assert_eq!(r.verdict, ReconciliationVerdict::Unattributable);
        assert!(!r.is_verified());
        assert_eq!(r.balance_delta, Some(-7.0));
    }

    #[test]
    fn an_unknown_charge_order_selects_nothing() {
        // Planning against a guessed order would silently decide who pays.
        assert!(ChargeOrder::unknown().select(&[bucket()], 10.0).is_none());
    }

    #[test]
    fn a_confirmed_charge_order_picks_the_first_funded_bucket() {
        let mut expensive = bucket();
        expensive.resource_id = "wallet".into();
        expensive.kind = ResourceKind::PayAsYouGo;
        expensive.available_amount = 10_000.0;
        let mut promo = bucket();
        promo.resource_id = "promo".into();
        promo.kind = ResourceKind::PromotionalCredit;
        promo.available_amount = 1000.0;

        let order = ChargeOrder::declared(vec!["promo".into(), "wallet".into()]);
        let funded = vec![expensive.clone(), promo.clone()];
        let chosen = order.select(&funded, 10.0).expect("selected");
        assert_eq!(chosen.resource_id, "promo");

        // A promo credit too small to cover the request falls through to the wallet.
        let chosen = order.select(&funded, 5000.0).expect("selected");
        assert_eq!(chosen.resource_id, "wallet");
    }

    #[test]
    fn only_a_pay_as_you_go_bucket_costs_real_money() {
        assert!(ResourceKind::PayAsYouGo.is_real_money());
        assert!(!ResourceKind::PromotionalCredit.is_real_money());
        assert!(!ResourceKind::TokenPlan.is_real_money());
        assert!(ResourceKind::PremiumModelEntitlement.is_entitlement());
    }

    #[test]
    fn a_bucket_round_trips_through_json_with_unknown_temporals_intact() {
        let mut b = bucket();
        b.expires_at = None;
        b.reset_behavior = ResetBehavior::Unknown;
        let json = serde_json::to_string(&b).expect("serialize");
        let back: ResourceBucket = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(b, back);
        assert!(!back.expiry_is_known());
    }
}
