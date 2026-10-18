//! Projecting what the account subsystem observes into the planner's vocabulary.
//!
//! # The gap this closes
//!
//! `ml/resource_buckets.rs` and `ml/temporal_planner.rs` describe a world where one
//! account holds several buckets, each with its own expiry, reset rule and charge
//! order. `account/` describes the world that actually exists: one flat
//! [`AccountQuota`](crate::account::types::AccountQuota) per account, an optional
//! reset timestamp the NewAPI adapter always leaves `None`
//! (`adapters/newapi.rs:1507`), and no expiry at all.
//!
//! Nothing joined them, so the planner had nothing to plan over. This module is the
//! join — and it is deliberately **lossy in the honest direction**: what the account
//! system cannot establish stays unknown rather than being filled in.
//!
//! # What is projected, and what is not
//!
//! * [`AccountQuota`] → one [`ResourceBucket`], with `reset_behavior` left
//!   [`ResetBehavior::Unknown`] whenever the adapter did not report a reset time. An
//!   unstated reset rule is the common case, and assuming "refills" would make an
//!   expiring balance look free.
//! * The provider's currency hint (`newapi.quota_raw`, `quota_per_unit`) → the bucket's
//!   [`ResourceKind`], so credits are not treated as money.
//! * [`ObservedResourceEvent`] → reconciliation, keeping the distinction between an
//!   established balance change and an unobserved one.
//! * [`ObservedProviderAccounting`] → [`ChargeReconciliation`], which is where a
//!   recorded charge stops being a number and becomes a checkable claim.
//!
//! What is **not** projected: expiry. Nothing in `account/` reports it, so no bucket
//! this module produces claims to expire, and the planner consequently cannot
//! capitalise on a quota expiring today. Supplying expiry means configuring it, not
//! inferring it.
//!
//! # Feature gate
//!
//! Behind `cfg(feature = "account")`. The shipped binary enables `ml` and not
//! `account`, so this is compiled only where the account subsystem exists to project.

use crate::account::resource::{ObservedProviderAccounting, ObservedResourceEvent, ResourceEffect};
use crate::account::types::AccountRuntime;
use crate::ml::resource_buckets::{
    ChargeReconciliation, ObservationSource, ReconciliationVerdict, ResetBehavior, ResourceBucket,
    ResourceKind,
};

/// Relative tolerance when comparing a reconstructed charge with a recorded one.
const DEFAULT_RECONCILE_TOLERANCE: f64 = 0.02;

/// Project one account's observed quota into the planner's bucket model.
///
/// Returns `None` when the account has no quota to project, which is a normal state
/// for an account whose provider publishes no balance.
///
/// The reset behaviour is [`ResetBehavior::Unknown`] unless a reset time was actually
/// reported *and* the caller supplied the rule for it. The adapter cannot tell us
/// whether a reset clears the balance or tops it up, and those differ sharply enough
/// near the boundary that guessing would be a correctness bug rather than an
/// approximation.
pub fn project_quota(
    runtime: &AccountRuntime,
    reset_behavior: ResetBehavior,
    observed_at: i64,
) -> Option<ResourceBucket> {
    let quota = runtime.quota.as_ref()?;
    let kind = classify_unit(&quota.unit, runtime);
    Some(ResourceBucket {
        // The kind is part of the identity: an account may legitimately hold a credit
        // balance and a money balance at once, and two buckets sharing an id would
        // silently shadow each other in a charge order.
        resource_id: format!("{}::{}::{kind:?}", runtime.provider_id, runtime.account_id),
        provider_id: runtime.provider_id.clone(),
        account_id: runtime.account_id.to_string(),
        kind,
        unit: quota.unit.clone(),
        available_amount: quota.remaining,
        total_amount: quota.total,
        // Never inferred. Nothing in `account/` reports an expiry, and inventing one
        // would let the planner treat an ordinary balance as if it were about to be
        // destroyed.
        expires_at: None,
        resets_at: quota.resets_at,
        // The rule is only as good as the operator's word for it. With no reset time
        // at all, there is certainly no known rule.
        reset_behavior: if quota.resets_at.is_some() {
            reset_behavior
        } else {
            ResetBehavior::Unknown
        },
        valid_from: None,
        applicable_models: Vec::new(),
        // Nothing in `account/` tracks in-flight commitments; the account store holds
        // a snapshot, not a ledger. Zero is the truthful reading, and the planner's
        // own reservation field is where a caller that *can* track them should put
        // them.
        reserved_amount: 0.0,
        source: ObservationSource::ProviderReported,
        observed_at,
    })
}

/// Classify a bucket from its unit, using the provider's own hints where present.
///
/// The unit is never converted. `budget.rs` already refuses to add currencies
/// together, and a bucket misclassified as `PayAsYouGo` would be priced as real money
/// when it is someone's free allowance.
fn classify_unit(unit: &str, runtime: &AccountRuntime) -> ResourceKind {
    let lowered = unit.to_ascii_lowercase();
    if lowered == "usd" || lowered == "cny" || lowered.contains("currency") {
        return ResourceKind::PayAsYouGo;
    }
    // The NewAPI adapter stashes the provider's raw figures here.
    if runtime.metadata.contains_key("newapi.quota_raw") {
        return ResourceKind::TokenPlan;
    }
    if lowered == "tokens" {
        return ResourceKind::TokenPlan;
    }
    // Credits from a relay: promotional by default, because that is what they usually
    // are and the alternative — pricing someone's free credit as money — is the
    // expensive mistake.
    if lowered == "credits" {
        return ResourceKind::PromotionalCredit;
    }
    ResourceKind::FreeAllowance
}

/// Fold one observed event into a reconciliation verdict.
///
/// The subtlety this preserves: a *balance delta* and a *provider charge* are different
/// evidence about the same request. When only the balance moved and the provider logged
/// no arithmetic, the movement is [`ReconciliationVerdict::Unattributable`] rather
/// than a match — concurrent traffic means the difference is not evidence about this
/// request, exactly as `account/checkin.rs` already states.
pub fn reconcile_event(
    event: &ObservedResourceEvent,
    local_estimate: Option<f64>,
    tolerance: f64,
) -> ChargeReconciliation {
    let recorded = event
        .accounting
        .as_ref()
        .map(|accounting| accounting.reconstructed_charge)
        .filter(|value| *value > 0.0);

    let mut reconciliation = ChargeReconciliation::compare(local_estimate, recorded, tolerance);

    // Unobserved moves carry no amount at all, and the verdict says so.
    match &event.resource_effect {
        ResourceEffect::Debited { .. } | ResourceEffect::Credited { .. } => {
            if let Some(delta) = event.balance_delta() {
                if recorded.is_none() {
                    reconciliation = reconciliation.with_unattributable_balance(delta);
                }
            }
        }
        ResourceEffect::NoChange | ResourceEffect::Unobserved => {}
    }
    reconciliation
}

/// Compare a reconstructed charge against the provider's recorded one.
///
/// `agrees_with_recorded` on the accounting is `None` when there was nothing to
/// compare against, and that `None` is carried through as
/// [`ReconciliationVerdict::Insufficient`] rather than as agreement.
pub fn reconcile_accounting(
    accounting: &ObservedProviderAccounting,
    local_estimate: Option<f64>,
    tolerance: f64,
) -> ChargeReconciliation {
    let recorded = Some(accounting.reconstructed_charge).filter(|value| *value > 0.0);
    let mut reconciliation = ChargeReconciliation::compare(local_estimate, recorded, tolerance);
    reconciliation.declared_tokens =
        Some(accounting.prompt_tokens + accounting.completion_tokens + accounting.cache_tokens);
    // The adapter's own agreement flag, when it made the call, takes precedence over
    // the reconstruction: it had the log row in front of it.
    if let Some(agrees) = accounting.agrees_with_recorded {
        if !agrees && reconciliation.verdict != ReconciliationVerdict::Insufficient {
            reconciliation.verdict = ReconciliationVerdict::Mismatched;
        }
    }
    reconciliation
}

/// Reconcile with the module's default tolerance.
pub fn reconcile_accounting_default(
    accounting: &ObservedProviderAccounting,
    local_estimate: Option<f64>,
) -> ChargeReconciliation {
    reconcile_accounting(accounting, local_estimate, DEFAULT_RECONCILE_TOLERANCE)
}

/// Whether this bucket's contents can be trusted right now.
///
/// An account the subsystem reports as suspended or expired must not be planned
/// against, and a snapshot that is hours old may already have been spent. Both are
/// hard gates rather than discounts, because spending against a stale or dead balance
/// is how a quota goes negative.
pub fn is_trustworthy(runtime: &AccountRuntime, now: i64, max_age_secs: i64) -> bool {
    use crate::account::types::AccountStatus;
    if matches!(
        runtime.status,
        AccountStatus::Suspended | AccountStatus::AuthenticationExpired
    ) {
        return false;
    }
    match runtime.last_sync {
        Some(synced) => now.saturating_sub(synced) <= max_age_secs,
        // Never synced: the contents are a configuration value, not an observation.
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::resource::{
        ObservedResourceEvent, ResourceAmount, ResourceEventKind, ResourceEventSource,
    };
    use crate::account::types::{AccountId, AccountQuota, AccountStatus};
    use crate::ml::resource_buckets::ChargeOrder;
    use crate::ml::temporal_planner::{
        attribute_draw, DemandForecast, TemporalConfig, TemporalPlanner,
    };
    use std::collections::{BTreeMap, HashMap};

    const NOW: i64 = 1_700_000_000;

    fn runtime(quota: Option<AccountQuota>, synced: Option<i64>) -> AccountRuntime {
        AccountRuntime {
            account_id: AccountId("acct-1".into()),
            provider_id: "newapi".into(),
            status: AccountStatus::Active,
            capabilities: Default::default(),
            quota,
            usage: None,
            rate_limit: None,
            last_success: None,
            last_failure: None,
            last_sync: synced,
            metadata: HashMap::new(),
        }
    }

    fn quota(unit: &str, resets_at: Option<i64>) -> AccountQuota {
        AccountQuota {
            total: 1000.0,
            used: 250.0,
            remaining: 750.0,
            unit: unit.into(),
            resets_at,
        }
    }

    fn bucket_for(unit: &str, resets_at: Option<i64>, rule: ResetBehavior) -> ResourceBucket {
        project_quota(&runtime(Some(quota(unit, resets_at)), Some(NOW)), rule, NOW).expect("bucket")
    }

    fn accounting(reconstructed: f64, agrees: Option<bool>) -> ObservedProviderAccounting {
        ObservedProviderAccounting {
            prompt_tokens: 100,
            completion_tokens: 20,
            cache_tokens: 0,
            cache_ratio: 0.0,
            model_ratio: 1.0,
            completion_ratio: 3.0,
            group_ratio: 1.0,
            fresh_input_tokens: 100,
            effective_prompt_tokens: 100.0,
            effective_usage_tokens: 160.0,
            reconstructed_charge: reconstructed,
            agrees_with_recorded: agrees,
        }
    }

    fn event(effect: ResourceEffect) -> ObservedResourceEvent {
        ObservedResourceEvent {
            event_id: Some("e1".into()),
            provider_id: "newapi".into(),
            account_id: AccountId("acct-1".into()),
            observed_at: NOW,
            event_kind: ResourceEventKind::Consumption,
            raw_type: None,
            description: "consumed".into(),
            resource_effect: effect,
            source: ResourceEventSource::AccountLog,
            request_id: Some("req-1".into()),
            channel_id: None,
            model: None,
            accounting: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn an_account_with_no_quota_projects_to_nothing() {
        assert!(project_quota(&runtime(None, Some(NOW)), ResetBehavior::Refill, NOW).is_none());
    }

    #[test]
    fn a_reset_rule_is_refused_when_no_reset_time_was_reported() {
        // The NewAPI adapter leaves `resets_at: None`, so this is the common case. An
        // operator-supplied rule must not silently apply to a bucket that never
        // resets: assuming "refills" would make a balance that only ever drains look
        // like a free allowance.
        let bucket = bucket_for("tokens", None, ResetBehavior::Refill);
        assert_eq!(bucket.reset_behavior, ResetBehavior::Unknown);
        assert!(!bucket.reset_behavior.is_plannable());
    }

    #[test]
    fn a_reported_reset_time_admits_the_supplied_rule() {
        let bucket = bucket_for("tokens", Some(NOW + 600), ResetBehavior::ClearToZero);
        assert_eq!(bucket.reset_behavior, ResetBehavior::ClearToZero);
        assert_eq!(bucket.resets_at, Some(NOW + 600));
    }

    #[test]
    fn no_bucket_claims_to_expire() {
        // Nothing in `account/` reports an expiry, so nothing may invent one. An
        // invented expiry would let the planner treat an ordinary balance as though it
        // were about to be destroyed.
        let bucket = bucket_for("tokens", None, ResetBehavior::Refill);
        assert_eq!(bucket.expires_at, None);
        assert!(!bucket.expiry_is_known());
    }

    #[test]
    fn credits_are_not_priced_as_money() {
        let bucket = bucket_for("credits", None, ResetBehavior::Refill);
        assert_eq!(bucket.kind, ResourceKind::PromotionalCredit);
        assert!(!bucket.kind.is_real_money());
        assert_eq!(bucket.unit, "credits", "the unit is carried verbatim");
    }

    #[test]
    fn a_currency_bucket_is_real_money() {
        let bucket = bucket_for("USD", None, ResetBehavior::Refill);
        assert_eq!(bucket.kind, ResourceKind::PayAsYouGo);
        assert!(bucket.kind.is_real_money());
    }

    #[test]
    fn a_newapi_account_with_raw_credits_is_a_token_plan() {
        let mut runtime = runtime(Some(quota("units", None)), Some(NOW));
        runtime
            .metadata
            .insert("newapi.quota_raw".into(), "123456".into());
        let bucket = project_quota(&runtime, ResetBehavior::Unknown, NOW).expect("bucket");
        assert_eq!(bucket.kind, ResourceKind::TokenPlan);
        assert!(!bucket.kind.is_real_money());
    }

    #[test]
    fn a_suspended_or_never_synced_account_is_not_trustworthy() {
        // Spending against a stale or dead balance is how a quota goes negative, so
        // both are hard gates rather than discounts.
        assert!(!is_trustworthy(
            &runtime(Some(quota("tokens", None)), None),
            NOW,
            3_600
        ));
        let mut stale = runtime(Some(quota("tokens", None)), Some(NOW - 10_000));
        assert!(!is_trustworthy(&stale, NOW, 3_600));
        stale.status = AccountStatus::Suspended;
        stale.last_sync = Some(NOW);
        assert!(!is_trustworthy(&stale, NOW, 3_600));
        assert!(is_trustworthy(
            &runtime(Some(quota("tokens", None)), Some(NOW - 60)),
            NOW,
            3_600
        ));
    }

    #[test]
    fn a_balance_move_with_no_provider_arithmetic_is_unattributable() {
        let event = event(ResourceEffect::Debited {
            amount: ResourceAmount::normalised(0.02, "USD"),
        });
        let reconciliation = reconcile_event(&event, Some(0.02), 0.02);
        // The estimate and the balance delta agree numerically, but that is not
        // evidence: the provider logged no arithmetic for this request, and concurrent
        // traffic means the difference is not attributable to it.
        assert_eq!(
            reconciliation.verdict,
            ReconciliationVerdict::Unattributable
        );
        assert!(!reconciliation.is_verified());
        assert_eq!(reconciliation.balance_delta, Some(-0.02));
    }

    #[test]
    fn an_unobserved_event_reports_insufficient_rather_than_agreement() {
        let reconciliation = reconcile_event(&event(ResourceEffect::Unobserved), Some(0.02), 0.02);
        assert_eq!(reconciliation.verdict, ReconciliationVerdict::Insufficient);
        assert!(!reconciliation.is_verified());
    }

    #[test]
    fn the_adapters_own_disagreement_overrides_a_matching_reconstruction() {
        // The adapter had the log row in front of it; its verdict outranks arithmetic
        // reconstructed from ratios.
        let reconciliation =
            reconcile_accounting_default(&accounting(0.02, Some(false)), Some(0.02));
        assert_eq!(reconciliation.verdict, ReconciliationVerdict::Mismatched);
        assert!(!reconciliation.is_verified());
        assert_eq!(reconciliation.declared_tokens, Some(120));
    }

    #[test]
    fn a_reconciled_charge_is_reportable_as_a_cost() {
        let reconciliation =
            reconcile_accounting_default(&accounting(0.02, Some(true)), Some(0.02));
        assert_eq!(reconciliation.verdict, ReconciliationVerdict::Matched);
        assert!(reconciliation.is_verified());
        // The verified figure is the provider's, not the estimate.
        assert_eq!(
            crate::ml::temporal_planner::verified_charge(
                reconciliation.verdict,
                reconciliation.provider_charged
            ),
            Some(0.02)
        );
    }

    #[test]
    fn an_unreconciled_charge_never_becomes_zero() {
        // The single most important property of the bridge: a charge that could not be
        // reconciled must not read as free.
        let reconciliation = reconcile_accounting_default(&accounting(0.02, None), None);
        assert_eq!(reconciliation.verdict, ReconciliationVerdict::Insufficient);
        assert_eq!(
            crate::ml::temporal_planner::verified_charge(
                reconciliation.verdict,
                reconciliation.provider_charged
            ),
            None
        );
    }

    #[test]
    fn a_projected_bucket_feeds_the_planner_without_invention() {
        // The end-to-end join: an account the subsystem observed becomes something the
        // planner can reason about.
        let bucket = bucket_for("credits", Some(NOW + 600), ResetBehavior::ClearToZero);
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let order = ChargeOrder::declared(vec![bucket.resource_id.clone()]);
        let plan = planner.plan(
            std::slice::from_ref(&bucket),
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            NOW,
        );
        assert_eq!(plan.usable.len(), 1);
        assert!(plan.usable[0].usable);
        assert!(
            plan.caveats.is_empty(),
            "a well-understood bucket needs no caveat"
        );

        // The draw is attributable, so a caller can price the request against it.
        let draw = attribute_draw(std::slice::from_ref(&bucket), &order);
        assert_eq!(draw.len(), 1);
        assert_eq!(draw[0].0, bucket.resource_id);
        assert!(plan.cost_of(&draw) > 0.0);
    }

    #[test]
    fn an_account_with_no_declared_reset_rule_is_priced_conservatively() {
        // The conservative end of the bridge. `account/` cannot say how a quota
        // resets, so the bucket is not *refused* — refusing would also refuse a
        // pay-as-you-go wallet, which is perfectly spendable regardless — but it is
        // priced above real money and flagged as low confidence, so it never wins a
        // marginal decision on the strength of an assumption nobody made.
        let unknown = bucket_for("credits", None, ResetBehavior::Refill);
        let planner = TemporalPlanner::new(TemporalConfig::default());
        let order = ChargeOrder::declared(vec![unknown.resource_id.clone()]);
        let plan = planner.plan(
            std::slice::from_ref(&unknown),
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            NOW,
        );
        let valuation = &plan.usable[0];
        assert!(valuation.usable);
        assert!(
            valuation.confidence < 0.5,
            "an unquantified rule must not read as understood: {}",
            valuation.confidence
        );

        // Priced above a real-money bucket of the same shape.
        let wallet = bucket_for("USD", None, ResetBehavior::CarryOver);
        let wallet_plan = planner.plan(
            std::slice::from_ref(&wallet),
            &DemandForecast::measured(10.0, 0.8, 50),
            &ChargeOrder::declared(vec![wallet.resource_id.clone()]),
            NOW,
        );
        assert!(
            valuation.shadow_price > wallet_plan.usable[0].shadow_price,
            "an unquantified bucket must lose to simply paying: {} vs {}",
            valuation.shadow_price,
            wallet_plan.usable[0].shadow_price
        );
        assert!(plan.has_low_confidence_pricing());
    }
}
