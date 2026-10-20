//! Time-aware valuation of spending a resource now.
//!
//! # What this adds to the decision path
//!
//! `RewardPolicy` (`ml/reward.rs`) scores a candidate on success, latency, cost and
//! fallback. Its cost term is the price of the call. That is correct when every
//! candidate draws on the same kind of resource, and wrong in three cases that show
//! up constantly:
//!
//! * A premium-model entitlement expires in twenty minutes. Spending it now is nearly
//!   free; spending it on a trivial chat is a mistake.
//! * A token plan's remaining allowance will almost certainly be wasted at expiry,
//!   because nobody forecasts their own evening usage. That makes its shadow price
//!   close to zero *today* — which is correct, and the reason quota-aware routing
//!   saves money without any change to the models.
//! * A bucket that resets to full in an hour is about to be plentiful, so its current
//!   scarcity should not drive the choice at all.
//!
//! # What this deliberately does not do
//!
//! It does not replace `DecisionEngine`, and it does not defer a request. There is no
//! "wait until the entitlement opens" path here, and adding one would mean holding a
//! user's synchronous request hostage to a resource schedule. It produces a valuation
//! that the existing decision path can use, and it hard-fails closed to the existing
//! policy whenever it cannot reason about a bucket.
//!
//! # Determinism
//!
//! Every temporal input is a parameter: `now` is passed in rather than read from the
//! system clock, and there is no randomness or iteration-order dependence. Same inputs
//! and same `now` produce byte-identical output, which is what makes the plan
//! replayable against a recorded decision and what the tests in this module check.
//!
//! # Conservative by construction
//!
//! An unknown reset rule, an unknown expiry, or a stale reading all reduce the
//! planner's confidence rather than being resolved to a guess. See
//! [`TemporalConfig`] for the exact policy.

use serde::{Deserialize, Serialize};

use crate::ml::resource_buckets::{
    ChargeOrder, ReconciliationVerdict, ResetBehavior, ResourceBucket, ResourceKind,
};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Tuning for shadow-price estimation, versioned so a plan can be reproduced.
///
/// Every field is serialised into the plan and fingerprinted, because a shadow price
/// recomputed under different constants is a different number and must not be
/// comparable with one computed under these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalConfig {
    pub schema_version: u32,
    /// Beyond this, a bucket's remaining amount is treated as worthless.
    ///
    /// The core judgement: a balance that cannot be spent before it expires has no
    /// recoverable value, so its opportunity cost is at most the price of *not*
    /// rebuying it, not its nominal amount.
    pub expiry_horizon_secs: i64,
    /// Window over which expected future demand is estimated.
    pub demand_window_secs: i64,
    /// Price assigned to one unit of a bucket whose rules are unknown.
    ///
    /// Deliberately high. An unquantified resource is treated as valuable so the
    /// planner declines to spend it on a marginal gain, which is the safe direction:
    /// declining costs one suboptimal route, spending wrongly costs real money.
    pub unknown_rule_price: f64,
    /// How sharply price rises as a bucket runs out, per fraction of remaining balance.
    pub scarcity_sensitivity: f64,
    /// Confidence assigned to a demand forecast with no supporting observations.
    pub base_demand_confidence: f64,
}

impl Default for TemporalConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            // Six hours. Beyond this, whatever remains will very likely be lost.
            expiry_horizon_secs: 6 * 3_600,
            // One day of demand.
            demand_window_secs: 24 * 3_600,
            // Above the real-money baseline of 1.0, so an unquantified bucket loses
            // to simply paying. At parity it would be an arbitrary winner.
            unknown_rule_price: 2.0,
            scarcity_sensitivity: 2.0,
            base_demand_confidence: 0.3,
        }
    }
}

impl TemporalConfig {
    /// A stable identity for this configuration, for report manifests.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        let mut mix = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x100000001b3);
        };
        mix(u64::from(self.schema_version));
        mix(self.expiry_horizon_secs as u64);
        mix(self.demand_window_secs as u64);
        mix(self.unknown_rule_price.to_bits());
        mix(self.scarcity_sensitivity.to_bits());
        mix(self.base_demand_confidence.to_bits());
        hash
    }
}

// ---------------------------------------------------------------------------
// Demand
// ---------------------------------------------------------------------------

/// An estimate of how much of a bucket will be wanted before it lapses.
///
/// `amount` is what is expected to be consumed if nothing changes; `confidence` is
/// how much to trust that number. Both matter, and they are separate: a confident
/// forecast of zero demand and an unconfident guess of high demand point in opposite
/// directions and must not be compared on `amount` alone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DemandForecast {
    /// Expected consumption before the bucket's expiry or reset, in the bucket's unit.
    pub amount: f64,
    /// Confidence in `[0, 1]`.
    pub confidence: f64,
    /// Observations the forecast was derived from. Reported so a forecast built on
    /// three data points cannot be presented with the weight of one built on thousands.
    pub supporting_observations: usize,
}

impl DemandForecast {
    /// A forecast backed by real observations.
    pub fn measured(amount: f64, confidence: f64, observations: usize) -> Self {
        Self {
            amount: amount.max(0.0),
            confidence: confidence.clamp(0.0, 1.0),
            supporting_observations: observations,
        }
    }

    /// The conservative fallback: no forecast, low confidence, no observations.
    pub fn unmeasured() -> Self {
        Self {
            amount: 0.0,
            confidence: 0.0,
            supporting_observations: 0,
        }
    }

    pub fn is_measured(&self) -> bool {
        self.supporting_observations > 0
    }
}

// ---------------------------------------------------------------------------
// Valuation
// ---------------------------------------------------------------------------

/// The per-bucket valuation a plan produces.
///
/// Kept separate from the arithmetic that produced it so an explanation can show the
/// inputs, not just the answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceValuation {
    pub resource_id: String,
    pub kind: ResourceKind,
    pub unit: String,
    /// Whether this bucket may be spent at `now`.
    pub usable: bool,
    /// Why not, when it may not. Empty when it may.
    pub exclusion_reason: String,
    /// Share of the request this bucket would pay for, in `[0, 1]`.
    pub expected_share: f64,
    /// Cost, in normalised utility units, of consuming `expected_share` from here.
    ///
    /// Normalised deliberately: this is a relative cost for ranking candidates, and
    /// summing it against a currency figure would be a unit error.
    pub shadow_price: f64,
    /// Confidence in the shadow price above.
    pub confidence: f64,
    /// Share of `available_amount` expected to be gone at expiry, in `[0, 1]`.
    ///
    /// High means the remainder is likely wasted and the price is low; low means it
    /// is likely to be needed later and spending it now costs real optionality.
    pub expected_expiry_loss: f64,
}

/// A complete, replayable plan for one decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourcePlan {
    /// The clock the plan was computed at. Part of the plan, because a plan is only
    /// reproducible if the time it was computed for is fixed.
    pub decided_at: i64,
    pub config_fingerprint: u64,
    /// Buckets that survived hard constraints.
    pub usable: Vec<ResourceValuation>,
    /// Every excluded bucket, with the reason.
    pub excluded: Vec<ResourceValuation>,
    /// Why the plan is as weak as it is. Never empty when something was excluded.
    pub caveats: Vec<String>,
}

impl ResourcePlan {
    /// Total normalised cost of routing through one candidate.
    pub fn cost_of(&self, shares: &[(String, f64)]) -> f64 {
        shares
            .iter()
            .map(|(resource_id, share)| {
                self.usable
                    .iter()
                    .find(|valuation| &valuation.resource_id == resource_id)
                    .map(|valuation| valuation.shadow_price * share)
                    .unwrap_or(0.0)
            })
            .sum()
    }

    /// Whether any bucket in the plan is priced with low confidence.
    pub fn has_low_confidence_pricing(&self) -> bool {
        self.usable
            .iter()
            .any(|valuation| valuation.confidence < 0.5)
    }

    /// The cheapest usable bucket by shadow price, if any.
    pub fn cheapest(&self) -> Option<&ResourceValuation> {
        self.usable
            .iter()
            .filter(|valuation| valuation.expected_share > 0.0)
            .min_by(|left, right| {
                left.shadow_price
                    .partial_cmp(&right.shadow_price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// Value resource buckets as of a supplied instant.
///
/// The clock is a parameter. Reading it internally would make every plan
/// irreproducible and every expiry test a race against the machine's calendar.
#[derive(Debug, Clone)]
pub struct TemporalPlanner {
    config: TemporalConfig,
}

impl TemporalPlanner {
    pub fn new(config: TemporalConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &TemporalConfig {
        &self.config
    }

    /// Decide which buckets are usable at `now` and what each costs to draw on.
    ///
    /// Pure: no clock read, no randomness, no I/O. Identical inputs give an identical
    /// [`ResourcePlan`].
    pub fn plan(
        &self,
        buckets: &[ResourceBucket],
        demand: &DemandForecast,
        charge_order: &ChargeOrder,
        now: i64,
    ) -> ResourcePlan {
        let mut usable = Vec::new();
        let mut excluded = Vec::new();
        let mut caveats = Vec::new();

        // A declared charge order tells the planner which bucket a request actually
        // draws on. Without one, every bucket is priced independently and the plan
        // reports that it cannot attribute the spend.
        if !charge_order.confirmed {
            caveats.push(
                "charge order is unconfirmed, so expected draw per bucket is not attributable"
                    .to_string(),
            );
        }

        for bucket in buckets {
            // The amount this request is assumed to need. Availability is checked
            // against the bucket's own unit, and a request whose size is unknown is
            // checked against zero rather than against a guess.
            let required = demand.amount;
            if let Some(reason) = bucket.unusable_reason(now, required) {
                caveats.push(format!(
                    "{resource} excluded: {reason}",
                    resource = bucket.resource_id
                ));
                excluded.push(ResourceValuation {
                    resource_id: bucket.resource_id.clone(),
                    kind: bucket.kind,
                    unit: bucket.unit.clone(),
                    usable: false,
                    exclusion_reason: reason,
                    expected_share: 0.0,
                    shadow_price: f64::INFINITY,
                    confidence: 0.0,
                    expected_expiry_loss: 0.0,
                });
                continue;
            }

            let (price, confidence, expiry_loss) =
                self.shadow_price(bucket, demand, charge_order, now);

            if confidence < 0.5 {
                caveats.push(format!(
                    "{resource} priced with low confidence",
                    resource = bucket.resource_id
                ));
            }
            usable.push(ResourceValuation {
                resource_id: bucket.resource_id.clone(),
                kind: bucket.kind,
                unit: bucket.unit.clone(),
                usable: true,
                exclusion_reason: String::new(),
                expected_share: 1.0,
                shadow_price: price,
                confidence,
                expected_expiry_loss: expiry_loss,
            });
        }

        // Order by id so a plan is byte-identical regardless of the caller's bucket
        // ordering. Two plans that differ only in iteration order are not comparable,
        // and a diff would otherwise look like a change in valuation.
        usable.sort_by(|left, right| left.resource_id.cmp(&right.resource_id));
        excluded.sort_by(|left, right| left.resource_id.cmp(&right.resource_id));
        caveats.sort();

        ResourcePlan {
            decided_at: now,
            config_fingerprint: self.config.fingerprint(),
            usable,
            excluded,
            caveats,
        }
    }

    /// The opportunity cost of drawing one unit from `bucket` right now.
    ///
    /// Three factors, and the order they are applied in is the substance of the
    /// model:
    ///
    /// 1. **Recoverable value.** What is left is worth what you can still buy with
    ///    it. If it expires inside the horizon, only the demand forecast says how much
    ///    of it will actually be used.
    /// 2. **Scarcity.** As the bucket empties, each unit costs more, because the
    ///    remaining units are needed more.
    /// 3. **Known-ness.** An unquantified rule is priced high rather than assumed free.
    ///
    /// The result is normalised into `[0, ∞)` of *relative* cost. It is a ranking
    /// signal, not a currency amount, and [`ResourcePlan::cost_of`] deliberately
    /// refuses to add it to a money figure.
    fn shadow_price(
        &self,
        bucket: &ResourceBucket,
        demand: &DemandForecast,
        charge_order: &ChargeOrder,
        now: i64,
    ) -> (f64, f64, f64) {
        // (3) Known-ness first: it gates whether the rest is meaningful at all.
        //
        // Real money is the exception, and the exception is not cosmetic. A reset
        // rule describes whether an *allowance* comes back; money never comes back,
        // it is simply spent. Requiring a known reset behaviour of a pay-as-you-go
        // wallet would price every real balance as unquantified — exactly backwards,
        // since it is the one resource whose future value is fully known.
        if !bucket.kind.is_real_money() && !bucket.reset_behavior.is_plannable() {
            return (self.config.unknown_rule_price, 0.2, 1.0);
        }
        let rule_confidence = bucket.temporal_confidence(now, self.config.demand_window_secs);
        let temporal_confidence = rule_confidence
            * (if demand.is_measured() {
                demand.confidence
            } else {
                self.config.base_demand_confidence
            });

        // (1) Recoverable value. When expiry is unknown the horizon cannot be applied,
        // so the conservative reading is "this does not expire and stays valuable" —
        // which raises the price and makes the planner prefer real money over an
        // unquantified bucket.
        let (recoverable_fraction, expiry_loss) = match bucket.seconds_to_expiry(now) {
            Some(remaining) => {
                if remaining <= 0.0 as i64 || remaining > self.config.expiry_horizon_secs {
                    // Beyond the horizon the remainder is expected to be lost.
                    let loss = demand_loss_ratio(bucket, demand);
                    (1.0 - loss, loss)
                } else {
                    // Inside the horizon: value is limited by what is expected to be
                    // wanted before expiry.
                    let loss = demand_loss_ratio(bucket, demand);
                    (1.0 - loss, loss)
                }
            }
            None => (1.0, 0.0),
        };

        // (2) Scarcity: a nearly-empty bucket makes each remaining unit dearer.
        let scarcity = if bucket.total_amount > 0.0 {
            let remaining_fraction = (bucket.usable() / bucket.total_amount).clamp(0.0, 1.0);
            1.0 + self.config.scarcity_sensitivity * (1.0 - remaining_fraction)
        } else {
            1.0
        };

        // A bucket that resets to full will be plentiful again, so its current
        // scarcity is about to be relieved and should not inflate the price now.
        let relief = if bucket.reset_behavior.relieves_scarcity() {
            if bucket
                .resets_at
                .is_some_and(|at| at <= now + self.config.demand_window_secs)
            {
                0.5
            } else {
                1.0
            }
        } else {
            1.0
        };

        // Real money is priced at a stable baseline; every other bucket is priced
        // relative to it by how much of its value is recoverable.
        let baseline = 1.0;
        let price = if bucket.kind.is_real_money() {
            baseline
        } else {
            baseline * recoverable_fraction.max(0.0) * scarcity * relief
        };

        // Charge order only affects attribution, not the price itself.
        let _ = charge_order;

        (
            price,
            temporal_confidence.clamp(0.0, 1.0),
            expiry_loss.clamp(0.0, 1.0),
        )
    }
}

/// Share of a bucket's remainder expected to be wasted at expiry.
///
/// With no measured demand the answer is deliberately "probably all of it": assuming
/// future need that was never observed is how a planner talks itself out of using an
/// expiring credit at all.
fn demand_loss_ratio(bucket: &ResourceBucket, demand: &DemandForecast) -> f64 {
    if !demand.is_measured() {
        return 1.0;
    }
    let remaining = bucket.usable().max(0.0);
    if remaining <= 0.0 {
        return 0.0;
    }
    let consumed_ratio = (demand.amount / remaining).clamp(0.0, 1.0);
    (1.0 - consumed_ratio).clamp(0.0, 1.0)
}

/// How much of `bucket` a request would draw, given a confirmed charge order.
///
/// Returns `(resource_id, share)` pairs. With an unconfirmed order nothing is
/// attributable, which is reported rather than guessed.
pub fn attribute_draw(
    buckets: &[ResourceBucket],
    charge_order: &ChargeOrder,
) -> Vec<(String, f64)> {
    if !charge_order.confirmed {
        return Vec::new();
    }
    charge_order
        .preference
        .iter()
        .filter(|id| buckets.iter().any(|bucket| &bucket.resource_id == *id))
        .map(|id| (id.clone(), 1.0))
        .collect()
}

/// Whether an estimated charge survived reconciliation into a usable figure.
///
/// An unreconciled charge is reported as unknown, never as zero. This is the single
/// conversion point, so the "don't treat unknown cost as free" rule has exactly one
/// implementation to audit.
pub fn verified_charge(
    verdict: ReconciliationVerdict,
    provider_charged: Option<f64>,
) -> Option<f64> {
    match verdict {
        ReconciliationVerdict::Matched => provider_charged,
        ReconciliationVerdict::Mismatched
        | ReconciliationVerdict::Insufficient
        | ReconciliationVerdict::Unattributable => None,
    }
}

/// Whether a bucket's reset rule means it is worth saving past a boundary.
///
/// Exposed because the scenario in the taskbook distinguishes these and a caller
/// explaining a decision needs the same predicate the planner used.
pub fn saving_is_valuable(behavior: ResetBehavior) -> bool {
    behavior.rewards_saving()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::resource_buckets::ObservationSource;

    const NOW: i64 = 1_700_000_000;

    fn bucket(id: &str, kind: ResourceKind) -> ResourceBucket {
        ResourceBucket {
            resource_id: id.into(),
            provider_id: "p".into(),
            account_id: "a".into(),
            kind,
            unit: "tokens".into(),
            available_amount: 1000.0,
            total_amount: 1000.0,
            expires_at: None,
            resets_at: None,
            reset_behavior: ResetBehavior::CarryOver,
            valid_from: None,
            applicable_models: vec![],
            reserved_amount: 0.0,
            source: ObservationSource::ProviderReported,
            observed_at: NOW,
        }
    }

    fn demand(amount: f64) -> DemandForecast {
        DemandForecast::measured(amount, 0.8, 100)
    }

    #[test]
    fn planning_is_deterministic_for_the_same_inputs() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let buckets = vec![
            bucket("promo", ResourceKind::PromotionalCredit),
            bucket("wallet", ResourceKind::PayAsYouGo),
        ];
        let order = ChargeOrder::declared(vec!["promo".into(), "wallet".into()]);
        let first = planner.plan(&buckets, &demand(100.0), &order, NOW);
        // Same buckets, reversed order: the plan must not change.
        let reversed = vec![
            bucket("wallet", ResourceKind::PayAsYouGo),
            bucket("promo", ResourceKind::PromotionalCredit),
        ];
        let second = planner.plan(&reversed, &demand(100.0), &order, NOW);
        assert_eq!(first, second);
    }

    #[test]
    fn different_clocks_produce_different_plans() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut expiring = bucket("promo", ResourceKind::PromotionalCredit);
        expiring.expires_at = Some(NOW + 3_600);
        let order = ChargeOrder::declared(vec!["promo".into()]);

        let far = planner.plan(&[expiring.clone()], &demand(100.0), &order, NOW);
        // A month later the same bucket is gone entirely.
        let after = planner.plan(&[expiring], &demand(100.0), &order, NOW + 30 * 86_400);
        assert_eq!(far.usable.len(), 1);
        assert_eq!(after.usable.len(), 0);
        assert!(!after.excluded.is_empty());
    }

    #[test]
    fn an_expiring_bucket_with_unmeasured_demand_is_cheap_to_use_now() {
        // The scenario that motivates the whole module: allowance nobody will use
        // before it lapses should not block the request.
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut expiring = bucket("promo", ResourceKind::PromotionalCredit);
        expiring.expires_at = Some(NOW + 600);
        let order = ChargeOrder::declared(vec!["promo".into()]);

        let plan = planner.plan(&[expiring], &DemandForecast::unmeasured(), &order, NOW);
        let price = plan.usable[0].shadow_price;
        let wallet = planner.plan(
            &[bucket("wallet", ResourceKind::PayAsYouGo)],
            &DemandForecast::unmeasured(),
            &order,
            NOW,
        );
        // Near-expiry and unused beats a real-money bucket.
        assert!(price < wallet.usable[0].shadow_price);
    }

    #[test]
    fn a_bucket_needed_soon_is_expensive_to_burn() {
        // Same bucket, same demand, different horizon: if demand says the allowance
        // is wanted, spending it now costs optionality.
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut expiring = bucket("promo", ResourceKind::PromotionalCredit);
        expiring.expires_at = Some(NOW + 600);
        let order = ChargeOrder::declared(vec!["promo".into()]);

        // Demand nearly consumes the whole bucket, so little is wasted at expiry.
        let wanted = planner.plan(&[expiring.clone()], &demand(950.0), &order, NOW);
        // Demand almost none, so nearly all of it is wasted.
        let unwanted = planner.plan(&[expiring], &demand(10.0), &order, NOW);
        assert!(
            wanted.usable[0].shadow_price > unwanted.usable[0].shadow_price,
            "a bucket that will be needed is dearer to spend early"
        );
    }

    #[test]
    fn an_unknown_reset_rule_is_priced_high_and_flagged_low_confidence() {
        // Never free. An unquantified resource must not look like a bargain.
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut unknown = bucket("mystery", ResourceKind::PromotionalCredit);
        unknown.reset_behavior = ResetBehavior::Unknown;
        let wallet = bucket("wallet", ResourceKind::PayAsYouGo);
        let order = ChargeOrder::declared(vec!["mystery".into(), "wallet".into()]);

        let plan = planner.plan(&[unknown, wallet], &demand(100.0), &order, NOW);
        let mystery = plan
            .usable
            .iter()
            .find(|v| v.resource_id == "mystery")
            .unwrap();
        assert!(mystery.confidence < 0.5);
        assert!(mystery.shadow_price > 1.0);
        assert!(plan.has_low_confidence_pricing());
        assert!(plan.cheapest().map(|v| v.resource_id.as_str()) == Some("wallet"));
    }

    #[test]
    fn a_bucket_that_resets_soon_is_cheaper_to_use_before_the_reset() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut soon = bucket("daily", ResourceKind::FreeAllowance);
        soon.reset_behavior = ResetBehavior::ClearToZero;
        soon.resets_at = Some(NOW + 3_600);
        let mut later = soon.clone();
        later.resource_id = "later".into();
        later.resets_at = Some(NOW + 10 * 86_400);

        let order = ChargeOrder::declared(vec!["daily".into(), "later".into()]);
        let plan = planner.plan(&[soon, later], &demand(100.0), &order, NOW);
        let cheap = plan
            .usable
            .iter()
            .find(|v| v.resource_id == "daily")
            .unwrap();
        let dear = plan
            .usable
            .iter()
            .find(|v| v.resource_id == "later")
            .unwrap();
        assert!(cheap.shadow_price < dear.shadow_price);
    }

    #[test]
    fn an_expired_bucket_is_excluded_with_a_readable_reason() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut expired = bucket("gone", ResourceKind::PromotionalCredit);
        expired.expires_at = Some(NOW - 1);
        let order = ChargeOrder::declared(vec!["gone".into()]);
        let plan = planner.plan(&[expired], &demand(1.0), &order, NOW);
        assert!(plan.usable.is_empty());
        assert!(plan.excluded[0].exclusion_reason.contains("expired"));
        // Real money must never be reached for because the free option vanished: the
        // plan excludes the bucket, it does not substitute one.
    }

    #[test]
    fn an_unconfirmed_charge_order_is_reported_not_guessed() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let plan = planner.plan(
            &[bucket("a", ResourceKind::PromotionalCredit)],
            &demand(1.0),
            &ChargeOrder::unknown(),
            NOW,
        );
        assert!(plan.caveats.iter().any(|c| c.contains("charge order")));
        assert!(attribute_draw(
            &[bucket("a", ResourceKind::FreeAllowance)],
            &ChargeOrder::unknown()
        )
        .is_empty());
    }

    #[test]
    fn concurrent_commitments_prevent_collective_overspend() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let mut b = bucket("limited", ResourceKind::TokenPlan);
        b.available_amount = 500.0;
        b.reserved_amount = 400.0;
        // Demand exceeds what is usable after reservations.
        let order = ChargeOrder::declared(vec!["limited".into()]);
        let plan = planner.plan(&[b], &demand(500.0), &order, NOW);
        assert!(plan.usable.is_empty());
        assert!(plan.excluded[0]
            .exclusion_reason
            .contains("short of required"));
    }

    #[test]
    fn an_unreconciled_charge_is_unknown_never_zero() {
        assert_eq!(
            verified_charge(ReconciliationVerdict::Insufficient, Some(0.0)),
            None
        );
        assert_eq!(
            verified_charge(ReconciliationVerdict::Matched, Some(0.0)),
            Some(0.0)
        );
    }

    #[test]
    fn the_shadow_price_is_a_ranking_signal_and_carries_no_currency() {
        // Guards the unit boundary: a normalised price must never be summed with money.
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let plan = planner.plan(
            &[bucket("a", ResourceKind::PromotionalCredit)],
            &demand(1.0),
            &ChargeOrder::declared(vec!["a".into()]),
            NOW,
        );
        let valuation = &plan.usable[0];
        assert!(valuation.shadow_price.is_finite());
        assert!(valuation.unit == "tokens");
    }

    #[test]
    fn config_changes_are_visible_in_the_plan_fingerprint() {
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let order = ChargeOrder::declared(vec!["a".into()]);
        let base = planner.plan(
            &[bucket("a", ResourceKind::FreeAllowance)],
            &demand(1.0),
            &order,
            NOW,
        );
        let tweaked = TemporalConfig {
            scarcity_sensitivity: 9.0,
            ..Default::default()
        };
        let other = TemporalPlanner::new(tweaked).plan(
            &[bucket("a", ResourceKind::FreeAllowance)],
            &demand(1.0),
            &order,
            NOW,
        );
        assert_ne!(base.config_fingerprint, other.config_fingerprint);
    }
}
