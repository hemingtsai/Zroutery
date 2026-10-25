//! Re-ordering the plan with the learned model, and the exploration path when no model can rank it.

#[cfg(feature = "ml")]
use std::sync::Arc;

#[cfg(feature = "ml")]
use super::AppState;
#[cfg(feature = "ml")]
use crate::policy::{self, RouteDecision};
#[cfg(feature = "ml")]
use crate::router::Candidate;

/// ml-ranking-helper-begin
/// Re-order one request's executable plan with the learned model, if one can rank
/// it.
///
/// Returns the plan and the decision unchanged when nothing can be ranked. That
/// is the normal case in a deployment that has never promoted a model, and the
/// case whenever the model is attached but unavailable, so it is written as an
/// ordinary return rather than as an error the caller has to remember to catch.
#[cfg(feature = "ml")]
pub(super) fn apply_ml_ranking(
    state: &Arc<AppState>,
    plan: Vec<Candidate>,
    routing_decision: Option<RouteDecision>,
    snapshot: Option<crate::ml::ShadowInput>,
) -> (Vec<Candidate>, Option<RouteDecision>) {
    let untouched = || (plan.clone(), routing_decision.clone());

    let (Some(decision), Some(snapshot)) = (routing_decision.clone(), snapshot) else {
        // A direct-resolution or classifier request carries no routing decision,
        // so there is no policy evidence to rank against. Reordering it on
        // features alone would put the model in charge of a request the router
        // never decided, which is a different product decision.
        return untouched();
    };
    if plan.len() < 2 {
        return untouched();
    }
    let request_id = decision.decision_id.clone();
    if !state.ml_routing().is_attached() {
        // No model, but exploration still applies — and this is the only place it
        // can. A model cannot be trained on evidence the router never gathered,
        // so if exploring required a promoted model the dataset could only ever
        // contain candidates the incumbent plan already reached, and a provider
        // added at the bottom of the priority order would be invisible forever.
        return apply_exploration_without_a_model(state, plan, decision, &snapshot, &request_id);
    }
    let ranked = match state.ml_routing().rank(&snapshot, &request_id) {
        Ok(ranked) => ranked,
        Err(reason) => {
            tracing::debug!(
                decision_id = %decision.decision_id,
                reason = ?reason,
                "the learned model could not rank this request; the deterministic plan stands"
            );
            return untouched();
        }
    };

    let plan_order: Vec<String> = plan.iter().map(|c| c.exposed_id.clone()).collect();
    let applied = crate::ml::AppliedRanking::apply(&ranked, &plan_order);
    if !applied.changed {
        return untouched();
    }

    // Rebuild the plan in the model's order. Only identities present in the
    // original plan are looked up, so the set of servable candidates is
    // unchanged and only the order differs.
    let mut reordered: Vec<Candidate> = Vec::with_capacity(plan.len());
    for id in &applied.order {
        if let Some(candidate) = plan.iter().find(|c| &c.exposed_id == id) {
            reordered.push(candidate.clone());
        }
    }
    if reordered.len() != plan.len() {
        // The ranking named something the plan does not contain in a way that
        // lost a candidate. Refuse the whole ranking rather than serve a plan
        // the router never produced.
        tracing::warn!(
            decision_id = %decision.decision_id,
            planned = plan.len(),
            reordered = reordered.len(),
            "the learned ranking did not cover the executable plan; the deterministic plan stands"
        );
        return untouched();
    }

    // Amend the decision so the recorded planned identity is the one that will
    // actually be tried, and so the model's influence is visible in the decision
    // record rather than only in a log line.
    let amended = routing_decision.map(|mut decision| {
        decision.selected = Some(applied.selected.clone());
        decision.ml_ranking = Some(policy::MlRankingTrace {
            commit_id: applied.commit_id.clone(),
            selected: applied.selected.clone(),
            reason: applied.reason.clone(),
            explored: applied.explored,
            modelled: true,
            order: applied.order.clone(),
            baseline_order: plan_order.clone(),
        });
        decision
    });

    tracing::debug!(
        decision_id = %decision.decision_id,
        commit_id = %applied.commit_id,
        selected = %applied.selected,
        explored = applied.explored,
        reason = %applied.reason,
        "the learned model re-ordered the provider plan"
    );

    (reordered, amended)
}

/// Try an eligible candidate the plan would not have reached, with no model.
///
/// The counterpart to [`apply_ml_ranking`] for the state before anything has been
/// promoted. It reorders within the executable plan exactly as the model path
/// does, and it records `modelled: false` so nothing downstream can read a
/// deliberate experiment as a decision the model made.
#[cfg(feature = "ml")]
fn apply_exploration_without_a_model(
    state: &Arc<AppState>,
    plan: Vec<Candidate>,
    decision: RouteDecision,
    snapshot: &crate::ml::ShadowInput,
    request_id: &str,
) -> (Vec<Candidate>, Option<RouteDecision>) {
    let Some(explored) = state.ml_routing().explore_plan(snapshot, request_id) else {
        return (plan.clone(), Some(decision));
    };
    if !plan.iter().any(|c| c.exposed_id == explored.selected) {
        // The draw named something the executable plan does not contain. Refuse
        // it rather than serve a candidate the router never planned.
        tracing::warn!(
            decision_id = %request_id,
            selected = %explored.selected,
            "the exploration draw named a candidate outside the executable plan; \
             the deterministic plan stands"
        );
        return (plan, Some(decision));
    }

    let mut order: Vec<String> = plan
        .iter()
        .map(|candidate| candidate.exposed_id.clone())
        .collect();
    // Move the explored candidate to the front, keeping everything else's
    // relative order. The set of servable candidates is untouched.
    order.retain(|id| id != &explored.selected);
    order.insert(0, explored.selected.clone());
    let mut reordered: Vec<Candidate> = Vec::with_capacity(plan.len());
    for id in &order {
        if let Some(candidate) = plan.iter().find(|c| &c.exposed_id == id) {
            reordered.push(candidate.clone());
        }
    }
    if reordered.len() != plan.len() {
        return (plan.clone(), Some(decision));
    }

    let plan_order: Vec<String> = plan.iter().map(|c| c.exposed_id.clone()).collect();
    let amended = {
        let mut decision = decision;
        decision.selected = Some(explored.selected.clone());
        decision.ml_ranking = Some(policy::MlRankingTrace {
            // No model, so no commit. Empty rather than a placeholder string,
            // because a field that can hold a fake commit is a field that will.
            commit_id: String::new(),
            selected: explored.selected.clone(),
            reason: explored.reason.clone(),
            explored: true,
            modelled: false,
            order: order.clone(),
            baseline_order: plan_order,
        });
        decision
    };

    tracing::debug!(
        decision_id = %request_id,
        selected = %explored.selected,
        "explored a candidate with no model attached, to gather evidence about it"
    );

    (reordered, Some(amended))
}
