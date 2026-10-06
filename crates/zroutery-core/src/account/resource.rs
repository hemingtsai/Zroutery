//! Provider-neutral observation of resource events.
//!
//! # What this is for
//!
//! An account's resource balance moves for reasons that are not visible from
//! the balance alone. A number that reads `12.40` cannot say whether it is
//! `12.40` because a check-in granted a reward, a top-up landed, or nothing has
//! been spent since the account was created. [`super::types::AccountRuntime`]
//! answers *how much is there*; this module answers *what happened*, as a
//! separate thing that is never folded into the balance.
//!
//! The two stay separate because they have different lifetimes and different
//! failure modes. A balance is a reading that expires; an event is a fact that
//! does not. Collapsing them produces the single most damaging mistake this
//! subsystem can make, described in [`ResourceEffect`].
//!
//! # Why the numbers never leave this module's shape
//!
//! [`ResourceEventKind`] is Zroutery's own vocabulary, not any provider's. A
//! NewAPI `type` value is an implementation detail of that panel and is carried
//! as [`ObservedResourceEvent::raw_type`] — an opaque integer kept for
//! diagnostics and for correlating against upstream documentation, never for
//! interpretation. An adapter translates; nothing above it ever sees a raw
//! provider type.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::types::AccountId;

/// What kind of thing an account event was, in Zroutery's vocabulary.
///
/// Deliberately coarser than any provider's taxonomy and deliberately not
/// derived from it. A provider adds an event type; this list does not gain a
/// variant for it. That is the point: an unknown upstream type becomes
/// [`ResourceEventKind::Unknown`] carrying its raw value, which a panel can
/// render honestly, instead of being silently folded into a category whose
/// meaning the adapter never established.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceEventKind {
    /// Resource left the account because of model or task usage.
    Consumption,
    /// Resource entered the account as a commercial or administrative credit.
    ///
    /// "Replenishment" rather than "payment": an operator's quota adjustment
    /// and a subscription purchase are not the same thing, and this event kind
    /// is only asked to say that the balance went up by a credited amount.
    Replenishment,
    /// A previously recorded consumption was given back.
    ///
    /// Distinct from [`ResourceEventKind::Replenishment`] because the two are
    /// not interchangeable: a refund reverses a charge and can be reversed
    /// again, while a top-up does neither. An account's ledger has to keep them
    /// apart or the two cancel each other out in the wrong order.
    Refund,
    /// A system-level grant: a check-in reward, a registration gift, an
    /// invitation bonus.
    ///
    /// Not "system message". The reason this variant exists is that upstream
    /// panels commonly file account grants under a generic system-log type, and
    /// reading that as a log line rather than as a resource event loses the
    /// only record of why a balance grew.
    SystemGrant,
    /// An administrative action.
    ///
    /// Never a resource event on its own. An operator action may or may not
    /// have moved the balance, and this kind says only that it happened.
    Management,
    /// A request or upstream call that failed.
    Error,
    /// A login.
    Login,
    /// An event this adapter could not classify.
    ///
    /// Carries its raw type and an [`ResourceEffect::Unobserved`] effect, so a
    /// fork that adds a type is visible rather than guessed at.
    #[default]
    Unknown,
}

impl ResourceEventKind {
    /// Whether this kind is expected to describe a movement in resource.
    ///
    /// Asked before any amount is interpreted. This is the guard that keeps a
    /// login or a management action from being turned into a balance delta.
    pub fn is_resource_bearing(self) -> bool {
        matches!(
            self,
            Self::Consumption | Self::Replenishment | Self::Refund | Self::SystemGrant
        )
    }
}

/// How much resource moved, as far as what was observed actually establishes.
///
/// # The mistake this type exists to make impossible
///
/// `balance_delta = log.quota` is wrong, and it is wrong in a way that reads as
/// obviously correct. A consumption log carries the charge it recorded, and a
/// refund log carries what it returned, so assigning one to the other's slot
/// produces numbers that look like accounting and are not. It also breaks on
/// the cases that matter most: a check-in reward is written as a system log
/// whose `quota` field is zero while the balance grows by a real amount, so the
/// naive rule credits the account nothing for a reward it actually received.
///
/// So the amount and the event kind are decided together, and where no
/// observation establishes an amount this says so. [`ResourceEffect::Unobserved`]
/// is a load-bearing variant: it is what lets an adapter be honest instead of
/// choosing between fabricating a number and dropping the event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum ResourceEffect {
    /// Resource left the account.
    Debited { amount: ResourceAmount },
    /// Resource entered the account.
    Credited { amount: ResourceAmount },
    /// The event was observed and established to have not moved the balance.
    ///
    /// Not the same claim as [`ResourceEffect::Unobserved`]: this is a finding.
    /// A failure log records a request that was attempted and did not settle.
    NoChange,
    /// The event may have moved the balance; nothing observed says by how much,
    /// or in which direction.
    ///
    /// The honest answer for a grant whose amount lives in a different endpoint,
    /// and for any event type this adapter does not recognise.
    #[default]
    Unobserved,
}

impl ResourceEffect {
    /// The amount this effect moved, when one is established.
    pub fn amount(&self) -> Option<&ResourceAmount> {
        match self {
            Self::Debited { amount } | Self::Credited { amount } => Some(amount),
            Self::NoChange | Self::Unobserved => None,
        }
    }

    /// Whether the balance is known to have moved.
    pub fn is_established(&self) -> bool {
        matches!(self, Self::Debited { .. } | Self::Credited { .. })
    }

    /// The signed contribution to the balance, in `unit`.
    ///
    /// `None` whenever the effect is not established. Returning a number for
    /// [`ResourceEffect::Unobserved`] would be the very conflation this type
    /// documents against, so it is refused rather than defaulted to zero: zero
    /// is a finding, and a caller that cannot tell "no movement" from "not
    /// measured" will eventually treat them as interchangeable.
    pub fn signed_delta(&self) -> Option<f64> {
        match self {
            Self::Debited { amount } => Some(-amount.value),
            Self::Credited { amount } => Some(amount.value),
            Self::NoChange | Self::Unobserved => None,
        }
    }
}

/// An amount of account resource, in both the provider's unit and a normalised one.
///
/// The two coexist because an integer credit and a floating currency are not
/// interchangeable. The provider's own figure is the authoritative one and
/// round-trips without loss; the normalised one is what a panel shows and what
/// the core's own price and budget code works in. Conflating them is how a
/// `500_000`-per-dollar synthetic credit ends up printed as `500000.00 USD`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceAmount {
    /// The provider's own accounting unit, when it has one.
    pub provider_value: Option<f64>,
    /// The value normalised to [`Self::unit`].
    pub value: f64,
    /// The unit `value` is expressed in, e.g. `"USD"`.
    ///
    /// Same convention as [`super::types::AccountQuota::unit`], chosen by the
    /// adapter and named in every effect so no consumer has to guess.
    pub unit: String,
}

impl ResourceAmount {
    /// An amount the adapter established, in `unit`.
    pub fn normalised(value: f64, unit: impl Into<String>) -> Self {
        Self {
            provider_value: None,
            value,
            unit: unit.into(),
        }
    }

    /// An amount the adapter established in both units.
    pub fn with_provider_value(mut self, provider_value: f64) -> Self {
        self.provider_value = Some(provider_value);
        self
    }

    /// Whether the normalised value is usable.
    ///
    /// A non-finite amount would poison every sum it reaches, so it is rejected
    /// at construction by callers that build amounts from parsed provider JSON.
    pub fn is_finite(&self) -> bool {
        self.value.is_finite() && self.provider_value.is_none_or(f64::is_finite)
    }
}

/// Where an observation was read from.
///
/// Carried on the event rather than left implicit, because "the panel said so"
/// and "we worked it out from the request we sent" carry different weight, and a
/// caller deciding whether to act on an event should be able to tell them
/// apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceEventSource {
    /// Read from the provider's account-scoped event or log endpoint.
    #[default]
    AccountLog,
    /// Read from a structured account-state endpoint, not from a log.
    AccountState,
    /// Derived by the adapter from the response to an operation we sent.
    OperationResponse,
}

/// A provider's own arithmetic, observed rather than assumed.
///
/// # This is not Zroutery's pricing
///
/// The ratios and the formula behind them are one provider's, and the numbers
/// here are that provider's. The core's own [`crate::billing`] and
/// [`crate::budget`] keep computing what a request *should* cost from local
/// configuration; nothing in this struct feeds them.
///
/// Its purpose is reconciliation: comparing what a provider charged against
/// what its own published inputs imply, which is how a mispriced relay or a
/// silently changed ratio gets noticed. The reconstruction is therefore expected
/// to be imperfect — providers round, cache-bill at several ratios, and add
/// per-call and tiered pricing that no single formula reproduces — which is why
/// [`Self::agrees_with_recorded`] reports disagreement instead of hiding it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObservedProviderAccounting {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens served from the provider's cache.
    pub cache_tokens: u64,
    /// Price multiplier the provider applied to cached prompt tokens.
    pub cache_ratio: f64,
    pub model_ratio: f64,
    pub completion_ratio: f64,
    pub group_ratio: f64,
    /// Prompt tokens that were *not* cache-served: `prompt_tokens - cache_tokens`.
    ///
    /// Saturating, because a provider reporting more cache tokens than prompt
    /// tokens is reporting something inconsistent, and the alternative is a
    /// subtraction that wraps.
    pub fresh_input_tokens: u64,
    /// `fresh_input + cache_tokens * cache_ratio`.
    pub effective_prompt_tokens: f64,
    /// `effective_prompt + completion_tokens * completion_ratio`.
    pub effective_usage_tokens: f64,
    /// `group_ratio * model_ratio * effective_usage`.
    pub reconstructed_charge: f64,
    /// Whether the reconstruction matched what the provider recorded.
    ///
    /// `None` when the provider recorded no comparable figure, or when a required
    /// ratio was absent. Never inferred from an absent comparison.
    pub agrees_with_recorded: Option<bool>,
}

impl ObservedProviderAccounting {
    /// Reconstruct a provider charge from the inputs the provider itself logged.
    ///
    /// Defaults each ratio to `1.0`, which is what a panel means by "no
    /// multiplier configured" and also what makes the reconstruction degrade to
    /// `prompt + completion` rather than to zero. Documented as a
    /// reconstruction because providers round at several points and bill some
    /// requests per call; [`Self::agrees_with_recorded`] is where a divergence
    /// becomes visible instead of becoming a wrong bill.
    pub fn reconstruct(
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_tokens: u64,
        cache_ratio: f64,
        model_ratio: f64,
        completion_ratio: f64,
        group_ratio: f64,
    ) -> Self {
        let fresh_input_tokens = prompt_tokens.saturating_sub(cache_tokens);
        let effective_prompt_tokens =
            fresh_input_tokens as f64 + cache_tokens as f64 * ratio_or_one(cache_ratio);
        let effective_usage_tokens =
            effective_prompt_tokens + completion_tokens as f64 * ratio_or_one(completion_ratio);
        let reconstructed_charge =
            ratio_or_one(group_ratio) * ratio_or_one(model_ratio) * effective_usage_tokens;
        Self {
            prompt_tokens,
            completion_tokens,
            cache_tokens,
            cache_ratio,
            model_ratio,
            completion_ratio,
            group_ratio,
            fresh_input_tokens,
            effective_prompt_tokens,
            effective_usage_tokens,
            reconstructed_charge,
            agrees_with_recorded: None,
        }
    }

    /// Record what the provider actually charged, and whether this
    /// reconstruction agrees with it.
    ///
    /// Compares with a relative tolerance rather than for equality: a provider
    /// that rounds a charge to an integer credit differs from the real-valued
    /// formula by construction, and calling that a disagreement on every request
    /// would make the flag useless.
    pub fn compare_with_recorded(mut self, recorded: f64) -> Self {
        let tolerance = recorded.abs().max(1.0) * 1e-6;
        self.agrees_with_recorded = Some((self.reconstructed_charge - recorded).abs() <= tolerance);
        self
    }
}

/// A ratio a provider left unset means "no adjustment", which is `1.0`.
///
/// A zero default would silently make every unpriced request free, and a
/// negative one would silently produce negative charges.
fn ratio_or_one(ratio: f64) -> f64 {
    if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        1.0
    }
}

/// One observed thing that happened to an account's resources.
///
/// Deliberately not a balance. [`ObservedResourceEvent::resource_effect`] says
/// what this one event did; it is never added to [`ResourceAmount::value`] and
/// never used to overwrite a stored balance, because a running sum of
/// partially-observed events drifts from the truth, and the truth is available
/// from the provider directly. Events explain a balance; they do not compute it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedResourceEvent {
    /// The provider's own identifier for this event, when it has one.
    pub event_id: Option<String>,
    pub provider_id: String,
    pub account_id: AccountId,
    /// When the provider says the event happened (unix seconds).
    ///
    /// The provider's timestamp, not when Zroutery read it. Reading time is
    /// tracked alongside in [`Self::observed_by_zroutery_at`] because the two
    /// answer different questions: "when did this account get the money" and
    /// "when did we find out".
    pub observed_at: i64,
    pub event_kind: ResourceEventKind,
    /// The provider's own type value, kept verbatim.
    ///
    /// Opaque. Never interpreted above the adapter that produced it.
    pub raw_type: Option<i64>,
    /// The provider's own text for the event.
    ///
    /// Carried as text and never parsed into an amount here. Upstream text is
    /// localised and forks reword it, so anything derived from parsing it
    /// belongs in the adapter that has the source in front of it.
    pub description: String,
    pub resource_effect: ResourceEffect,
    pub source: ResourceEventSource,
    pub request_id: Option<String>,
    /// The provider-side channel the request went through.
    ///
    /// An observation about the provider's internal routing, and nothing more.
    /// Seeing a channel id here says nothing about whether Zroutery can select
    /// it; only Zroutery's own configuration can decide that.
    pub channel_id: Option<i64>,
    pub model: Option<String>,
    /// The provider's own arithmetic for this event, when it logged any.
    pub accounting: Option<ObservedProviderAccounting>,
    /// Adapter-owned extras.
    pub metadata: BTreeMap<String, String>,
}

impl ObservedResourceEvent {
    /// Whether this event established that the balance moved.
    pub fn moved_resource(&self) -> bool {
        self.resource_effect.is_established()
    }

    /// The established change to the balance, in the effect's own unit.
    ///
    /// `None` when the effect was not established, which is the answer for both
    /// "this cannot have moved anything" and "nothing observed how much".
    pub fn balance_delta(&self) -> Option<f64> {
        self.resource_effect.signed_delta()
    }

    /// Look up an adapter-supplied metadata value.
    pub fn metadata_value(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(effect: ResourceEffect) -> ObservedResourceEvent {
        ObservedResourceEvent {
            event_id: None,
            provider_id: "p".into(),
            account_id: AccountId("a".into()),
            observed_at: 1_700_000_000,
            event_kind: ResourceEventKind::Consumption,
            raw_type: None,
            description: String::new(),
            resource_effect: effect,
            source: ResourceEventSource::AccountLog,
            request_id: None,
            channel_id: None,
            model: None,
            accounting: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn an_unobserved_effect_yields_no_delta() {
        let e = event(ResourceEffect::Unobserved);
        assert!(!e.moved_resource());
        assert_eq!(e.balance_delta(), None);
        assert_eq!(e.resource_effect.amount(), None);
    }

    #[test]
    fn no_change_is_established_not_unobserved() {
        // The distinction the whole module rests on: "did not move" and "not
        // measured" must not read the same to a caller.
        let no_change = event(ResourceEffect::NoChange);
        assert!(!no_change.moved_resource());
        assert_eq!(no_change.balance_delta(), None);
        assert_ne!(ResourceEffect::NoChange, ResourceEffect::Unobserved);
    }

    #[test]
    fn debit_and_credit_carry_opposite_signed_deltas() {
        let debited = event(ResourceEffect::Debited {
            amount: ResourceAmount::normalised(10.0, "USD"),
        });
        let credited = event(ResourceEffect::Credited {
            amount: ResourceAmount::normalised(10.0, "USD"),
        });
        assert_eq!(debited.balance_delta(), Some(-10.0));
        assert_eq!(credited.balance_delta(), Some(10.0));
    }

    #[test]
    fn only_resource_bearing_kinds_are_resource_bearing() {
        assert!(ResourceEventKind::Consumption.is_resource_bearing());
        assert!(ResourceEventKind::Replenishment.is_resource_bearing());
        assert!(ResourceEventKind::Refund.is_resource_bearing());
        assert!(ResourceEventKind::SystemGrant.is_resource_bearing());
        // A login and an operator action are events; they are not ledger lines.
        assert!(!ResourceEventKind::Login.is_resource_bearing());
        assert!(!ResourceEventKind::Management.is_resource_bearing());
        assert!(!ResourceEventKind::Error.is_resource_bearing());
        assert!(!ResourceEventKind::Unknown.is_resource_bearing());
    }

    #[test]
    fn reconstruct_applies_the_documented_formula() {
        let a = ObservedProviderAccounting::reconstruct(44_814, 91, 0, 1.0, 2.0, 3.0, 4.0);
        // fresh = 44814 - 0; effective_prompt = 44814;
        // effective_usage = 44814 + 91*3 = 45087; charge = 4*2*45087
        assert_eq!(a.fresh_input_tokens, 44_814);
        assert!((a.effective_prompt_tokens - 44_814.0).abs() < 1e-9);
        assert!((a.effective_usage_tokens - 45_087.0).abs() < 1e-9);
        assert!((a.reconstructed_charge - 360_696.0).abs() < 1e-9);
        assert_eq!(a.agrees_with_recorded, None);
    }

    #[test]
    fn cache_tokens_are_discounted_rather_than_dropped() {
        let a = ObservedProviderAccounting::reconstruct(1_000, 0, 800, 0.1, 1.0, 1.0, 1.0);
        // fresh = 200; effective_prompt = 200 + 800*0.1 = 280
        assert_eq!(a.fresh_input_tokens, 200);
        assert!((a.effective_prompt_tokens - 280.0).abs() < 1e-9);
    }

    #[test]
    fn cache_tokens_beyond_prompt_tokens_do_not_wrap() {
        let a = ObservedProviderAccounting::reconstruct(100, 0, 900, 1.0, 1.0, 1.0, 1.0);
        assert_eq!(
            a.fresh_input_tokens, 0,
            "a saturating subtraction, not a wrap"
        );
    }

    #[test]
    fn an_unset_ratio_degrades_to_one_not_zero() {
        let a = ObservedProviderAccounting::reconstruct(100, 0, 0, 0.0, 0.0, 0.0, 0.0);
        // Every ratio fell back to 1.0, so this is 100 tokens, not 0.
        assert!((a.reconstructed_charge - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_nan_ratio_cannot_poison_the_reconstruction() {
        let a = ObservedProviderAccounting::reconstruct(100, 0, 0, f64::NAN, f64::NAN, -1.0, 0.0);
        assert!(a.reconstructed_charge.is_finite());
        assert!((a.reconstructed_charge - 100.0).abs() < 1e-9);
    }

    #[test]
    fn comparison_reports_disagreement_rather_than_hiding_it() {
        let a = ObservedProviderAccounting::reconstruct(100, 0, 0, 1.0, 1.0, 1.0, 1.0)
            .compare_with_recorded(140.0);
        assert_eq!(a.agrees_with_recorded, Some(false));

        let b = ObservedProviderAccounting::reconstruct(100, 0, 0, 1.0, 1.0, 1.0, 1.0)
            .compare_with_recorded(100.0);
        assert_eq!(b.agrees_with_recorded, Some(true));
    }

    #[test]
    fn integer_credit_rounding_is_not_reported_as_disagreement() {
        // A provider rounding a charge to whole credits differs from the real
        // valued formula by construction; flagging every request would make the
        // flag useless.
        let a = ObservedProviderAccounting::reconstruct(100, 0, 0, 1.0, 1.0, 1.0, 1.0)
            .compare_with_recorded(100.000_000_4);
        assert_eq!(a.agrees_with_recorded, Some(true));
    }

    #[test]
    fn resource_amount_keeps_both_units() {
        let a = ResourceAmount::normalised(0.05, "USD").with_provider_value(25_000.0);
        assert_eq!(a.value, 0.05);
        assert_eq!(a.provider_value, Some(25_000.0));
        assert_eq!(a.unit, "USD");
        assert!(a.is_finite());
    }

    #[test]
    fn a_non_finite_amount_is_reported_as_such() {
        let a = ResourceAmount::normalised(f64::INFINITY, "USD");
        assert!(!a.is_finite());
    }

    #[test]
    fn event_kind_serde_is_stable_and_lowercase() {
        let json = serde_json::to_string(&ResourceEventKind::SystemGrant).unwrap();
        assert_eq!(json, "\"system_grant\"");
        let back: ResourceEventKind = serde_json::from_str("\"consumption\"").unwrap();
        assert_eq!(back, ResourceEventKind::Consumption);
        // An unrecognised name must fail rather than decode into a known kind: a
        // panel reading "unknown" as "consumption" would debit an account.
        assert!(serde_json::from_str::<ResourceEventKind>("\"nonsense\"").is_err());
    }

    #[test]
    fn unknown_is_the_default_kind() {
        assert_eq!(ResourceEventKind::default(), ResourceEventKind::Unknown);
        assert_eq!(ResourceEffect::default(), ResourceEffect::Unobserved);
    }
}
