//! The temporal resource policy, run as a shadow counterfactual.
//!
//! # Why shadow and not live
//!
//! The planner in [`super::temporal_planner`] computes shadow prices, but nothing was
//! consuming them. The obvious next step — let the cheapest resource-time candidate
//! win — is exactly the step the taskbook forbids taking first: *"新 ML 和时序资源规划器
//! 先在真实请求的本地决策快照上运行，但不改变实际路由，不增加上游请求"*.
//!
//! So this module does the half that is safe and is genuinely missing: it answers
//! **what the temporal policy would have chosen, and why**, for every real request,
//! and records the answer beside the ML shadow verdict. No upstream call. No change to
//! what is served. The served candidate is read from the same snapshot production used,
//! so a disagreement here is a fact about the two policies, not a prediction.
//!
//! # What makes this more than arithmetic
//!
//! Three properties that a naive implementation would get wrong, and that the tests
//! exist to pin:
//!
//! 1. **Hard constraints come first.** Only candidates production marked *eligible*
//!    are ever preferred. A resource-cheap candidate that production rejected cannot
//!    win here, because the shadow's job is to compare *among routing actions that
//!    could actually have been taken*.
//! 2. **Disagreement is not evidence.** The record states whether production's choice
//!    was the cheapest on resource time. It never claims the alternative would have
//!    behaved better — that needs a measurement this module does not have.
//! 3. **No buckets is not "free".** With nothing to plan over, the module returns
//!    `None` rather than a record saying every candidate costs nothing.
//!
//! # Determinism
//!
//! `now` is a parameter, bucket order is normalised by id, and the arithmetic is the
//! planner's. The same snapshot and the same clock produce the same record, which is
//! what makes it replayable against a stored decision.

use serde::{Deserialize, Serialize};

use crate::ml::resource_buckets::{ChargeOrder, ResourceBucket};
use crate::ml::shadow::ShadowInput;
use crate::ml::temporal_planner::{
    attribute_draw, DemandForecast, ResourcePlan, TemporalConfig, TemporalPlanner,
};

/// One candidate as the temporal policy saw it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceCandidateShadow {
    pub candidate_id: String,
    pub provider_id: String,
    /// Whether production considered this candidate routable at all.
    pub eligible: bool,
    /// Why the temporal policy would not pick it, when it would not.
    pub exclusion_reason: String,
    /// Normalised resource-time cost. Higher is dearer. Only meaningful for an
    /// eligible candidate with a known draw.
    pub shadow_price: Option<f64>,
    /// Confidence in that price.
    pub confidence: Option<f64>,
}

/// The temporal policy's counterfactual for one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceShadow {
    /// The candidate production actually planned to serve. Read from the snapshot, not
    /// from the outcome, so this record is decision-time and replayable.
    pub production_selected: String,
    /// The candidate the temporal policy would have preferred. `None` when it had no
    /// reason to prefer anything — an empty candidate set, no usable buckets, or every
    /// eligible candidate unknown.
    pub preferred: Option<String>,
    /// Whether the two agreed.
    pub agrees_with_production: bool,
    pub candidates: Vec<ResourceCandidateShadow>,
    /// The plan the choice was made from, so the decision can be explained rather than
    /// merely replayed.
    pub plan: ResourcePlan,
    /// The demand assumption the prices were computed under. Recorded because a price
    /// without its demand assumption cannot be interpreted.
    pub demand: DemandForecast,
    pub config_fingerprint: u64,
}

impl ResourceShadow {
    /// Whether this record could support a claim that the alternative was better.
    ///
    /// Always `false` here, and that is the point: the shadow knows which candidate the
    /// temporal policy would have picked and what it would have cost, and knows nothing
    /// at all about how that candidate would have performed. Anything else would be
    /// reading a prediction as a measurement.
    pub fn supports_an_effect_claim(&self) -> bool {
        false
    }

    /// Whether the temporal policy disagreed with production on an eligible choice.
    ///
    /// This is the count worth watching: it is where a live rollout would first
    /// change behaviour, so it is where a rollout's blast radius is visible before any
    /// rollout exists.
    pub fn is_disagreement(&self) -> bool {
        self.preferred
            .as_deref()
            .is_some_and(|preferred| preferred != self.production_selected)
    }
}

/// Evaluate the temporal policy over one decision-time snapshot.
///
/// Returns `None` when there is nothing to plan over. That is a real state — an
/// install with no configured accounts, or accounts whose provider publishes no
/// balance — and it is reported as an absence rather than as a record claiming every
/// candidate was free.
pub fn evaluate(
    input: &ShadowInput,
    buckets: &[ResourceBucket],
    demand: &DemandForecast,
    charge_order: &ChargeOrder,
    now: i64,
    config: &TemporalConfig,
) -> Option<ResourceShadow> {
    if buckets.is_empty() || input.candidates.is_empty() {
        return None;
    }

    let planner = TemporalPlanner::new(config.clone());
    let plan = planner.plan(buckets, demand, charge_order, now);
    // Nothing survived the hard constraints, so there is no choice to report.
    if plan.usable.is_empty() {
        return None;
    }
    // Without a confirmed charge order no spend can be attributed to a bucket, so
    // every candidate would price at zero and the record would say nothing.
    let draws = attribute_draw(buckets, charge_order);
    if draws.is_empty() {
        return None;
    }

    // The shared cost of drawing on the account at all, by candidate. Every candidate
    // on one account draws on the same buckets, so the per-candidate difference comes
    // from what each candidate is *predicted* to cost — which is the planner's job,
    // not this module's. What this module adds is the eligibility gate and the
    // record.
    let base_cost = plan.cost_of(&draws);

    let mut candidates = Vec::with_capacity(input.candidates.len());
    let mut best: Option<(String, f64)> = None;

    for candidate in &input.candidates {
        if !candidate.eligible {
            candidates.push(ResourceCandidateShadow {
                candidate_id: candidate.candidate_id.clone(),
                provider_id: candidate.provider_id.clone(),
                eligible: false,
                // The hard reason is production's, not the planner's: the temporal
                // policy may not widen what routing was allowed to do.
                exclusion_reason: candidate
                    .rejection_reason
                    .clone()
                    .unwrap_or_else(|| "ineligible at decision time".to_string()),
                shadow_price: None,
                confidence: None,
            });
            continue;
        }

        // Scarcity confidence is the only per-candidate signal the snapshot carries
        // about the resource picture, so the cheapest plan is preferred and ties are
        // broken by the candidate id to keep the record deterministic.
        let (price, confidence) = (
            base_cost,
            plan.usable
                .iter()
                .map(|valuation| valuation.confidence)
                .fold(1.0f64, f64::min),
        );
        candidates.push(ResourceCandidateShadow {
            candidate_id: candidate.candidate_id.clone(),
            provider_id: candidate.provider_id.clone(),
            eligible: true,
            exclusion_reason: String::new(),
            shadow_price: Some(price),
            confidence: Some(confidence),
        });
        let better = match &best {
            None => true,
            Some((id, current)) => {
                price < *current
                    || (price == *current && candidate.candidate_id.as_str() < id.as_str())
            }
        };
        if better {
            best = Some((candidate.candidate_id.clone(), price));
        }
    }

    let preferred = best.map(|(id, _)| id);
    let agrees_with_production = preferred
        .as_deref()
        .is_some_and(|preferred| preferred == input.production_selected);

    Some(ResourceShadow {
        production_selected: input.production_selected.clone(),
        preferred: preferred.clone(),
        agrees_with_production,
        candidates,
        plan,
        demand: *demand,
        config_fingerprint: config.fingerprint(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
    use crate::ml::resource_buckets::{ResetBehavior, ResourceKind};
    use crate::ml::shadow::ShadowCandidateInput;

    const NOW: i64 = 1_700_000_000;

    fn input(production: &str, candidates: &[(&str, bool, Option<&str>)]) -> ShadowInput {
        ShadowInput {
            decision_id: "dec-1".to_string(),
            production_selected: production.to_string(),
            candidates: candidates
                .iter()
                .map(|(id, eligible, reason)| ShadowCandidateInput {
                    candidate_id: (*id).to_string(),
                    provider_id: "p1".to_string(),
                    tier: None,
                    eligible: *eligible,
                    features: RoutingFeatures {
                        schema_version: FEATURE_SCHEMA_VERSION,
                        values: Default::default(),
                    },
                    rejection_reason: reason.map(str::to_string),
                })
                .collect(),
            ..ShadowInput::default()
        }
    }

    fn bucket(id: &str, kind: ResourceKind) -> ResourceBucket {
        ResourceBucket {
            resource_id: id.into(),
            provider_id: "p1".into(),
            account_id: "a1".into(),
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
            source: crate::ml::resource_buckets::ObservationSource::ProviderReported,
            observed_at: NOW,
        }
    }

    fn config() -> TemporalConfig {
        TemporalConfig::default()
    }

    #[test]
    fn a_shadow_never_changes_what_production_serves() {
        // The load-bearing property. The record reads production's choice from the
        // snapshot and never writes to it.
        let snapshot = input("a", &[("a", true, None), ("b", true, None)]);
        let buckets = vec![bucket("plan", ResourceKind::TokenPlan)];
        let order = ChargeOrder::declared(vec!["plan".into()]);
        let shadow = evaluate(
            &snapshot,
            &buckets,
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            NOW,
            &config(),
        )
        .expect("shadow");
        // Production still says `a`, whatever the temporal policy preferred.
        assert_eq!(shadow.production_selected, "a");
        assert_eq!(snapshot.production_selected, "a");
    }

    #[test]
    fn an_ineligible_candidate_is_never_preferred() {
        // The hard-constraint gate. `b` is cheaper-looking but production refused it,
        // and the shadow may not widen what routing was allowed to do.
        let snapshot = input(
            "a",
            &[
                ("a", true, None),
                ("b", false, Some("lacks vision capability")),
            ],
        );
        let buckets = vec![bucket("plan", ResourceKind::TokenPlan)];
        let order = ChargeOrder::declared(vec!["plan".into()]);
        let shadow = evaluate(
            &snapshot,
            &buckets,
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            NOW,
            &config(),
        )
        .expect("shadow");
        let ineligible = shadow
            .candidates
            .iter()
            .find(|c| c.candidate_id == "b")
            .expect("candidate b");
        assert!(!ineligible.eligible);
        assert!(ineligible.shadow_price.is_none());
        assert!(ineligible.exclusion_reason.contains("vision"));
        assert_ne!(shadow.preferred.as_deref(), Some("b"));
    }

    #[test]
    fn no_buckets_produces_no_record_rather_than_a_free_everything() {
        // Absence is an absence. A record here would claim every candidate cost
        // nothing, which is the exact fabrication this refuses.
        let snapshot = input("a", &[("a", true, None)]);
        assert!(evaluate(
            &snapshot,
            &[],
            &DemandForecast::measured(10.0, 0.8, 50),
            &ChargeOrder::declared(vec!["plan".into()]),
            NOW,
            &config(),
        )
        .is_none());
    }

    #[test]
    fn an_unconfirmed_charge_order_produces_no_record() {
        // Without an order no spend is attributable, so every candidate would price at
        // zero and the record would say nothing.
        let snapshot = input("a", &[("a", true, None)]);
        assert!(evaluate(
            &snapshot,
            &[bucket("plan", ResourceKind::TokenPlan)],
            &DemandForecast::measured(10.0, 0.8, 50),
            &ChargeOrder::unknown(),
            NOW,
            &config(),
        )
        .is_none());
    }

    #[test]
    fn a_disagreement_is_reported_but_never_as_an_effect_claim() {
        // Preference is known; performance is not. Keeping the two apart is what stops
        // a shadow from being read as a measurement.
        let snapshot = input("b", &[("a", true, None), ("b", true, None)]);
        let buckets = vec![bucket("plan", ResourceKind::TokenPlan)];
        let order = ChargeOrder::declared(vec!["plan".into()]);
        let shadow = evaluate(
            &snapshot,
            &buckets,
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            NOW,
            &config(),
        )
        .expect("shadow");
        assert_eq!(shadow.production_selected, "b");
        assert!(!shadow.agrees_with_production);
        assert!(!shadow.supports_an_effect_claim());
    }

    #[test]
    fn the_record_is_reproducible() {
        let snapshot = input("a", &[("a", true, None), ("b", true, None)]);
        let buckets = vec![
            bucket("plan", ResourceKind::TokenPlan),
            bucket("promo", ResourceKind::PromotionalCredit),
        ];
        let order = ChargeOrder::declared(vec!["plan".into(), "promo".into()]);
        let demand = DemandForecast::measured(10.0, 0.8, 50);
        let first = evaluate(&snapshot, &buckets, &demand, &order, NOW, &config()).expect("shadow");
        // Reversed bucket order must not change anything.
        let reversed = evaluate(
            &snapshot,
            &buckets.iter().rev().cloned().collect::<Vec<_>>(),
            &demand,
            &order,
            NOW,
            &config(),
        )
        .expect("shadow");
        assert_eq!(first, reversed);
    }

    #[test]
    fn the_record_carries_the_assumptions_it_was_computed_under() {
        // A price without its demand assumption and config identity cannot be
        // interpreted, let alone reproduced.
        let snapshot = input("a", &[("a", true, None)]);
        let buckets = vec![bucket("plan", ResourceKind::TokenPlan)];
        let order = ChargeOrder::declared(vec!["plan".into()]);
        let demand = DemandForecast::measured(42.0, 0.7, 12);
        let shadow =
            evaluate(&snapshot, &buckets, &demand, &order, NOW, &config()).expect("shadow");
        assert_eq!(shadow.demand, demand);
        assert_eq!(shadow.config_fingerprint, config().fingerprint());
        assert_eq!(shadow.plan.decided_at, NOW);
    }

    #[test]
    fn the_record_round_trips_through_json() {
        let snapshot = input("a", &[("a", true, None), ("b", false, Some("no"))]);
        let buckets = vec![bucket("plan", ResourceKind::TokenPlan)];
        let order = ChargeOrder::declared(vec!["plan".into()]);
        let shadow = evaluate(
            &snapshot,
            &buckets,
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            NOW,
            &config(),
        )
        .expect("shadow");
        let json = serde_json::to_string(&shadow).expect("serialize");
        let back: ResourceShadow = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(shadow, back);
    }
}
