//! Spending limits and admission: take the permits, then check the ledger.

use std::collections::BTreeSet;

use super::{AdmissionGuard, AppState};
use crate::budget::{Budget, BudgetScope, OnExceeded, Verdict};
use crate::config::{AppConfig, ModelTier};
use crate::error::{Error, Result};
use crate::registry::{Registry, Resolution};
use crate::router::Candidate;

/// Apply the spending limits to a resolved request.
///
/// The check is "have I already spent it", not "will this request spend it", because
/// the cost is only known once a request has finished. What bounds the overshoot at
/// one request instead of at one request *per racing caller* is admission: every
/// scope with a configured limit that this request could spend against is taken as
/// a permit before the check, in canonical order, and returned with the resolution
/// so the caller can hold it until the request has been settled and charged. A
/// request that arrives while another is in flight for the same scope waits, then
/// reads the ledger the finished request already wrote.
///
/// A degrade is followed at most once per tier: the cheaper tier's own budget still
/// applies, so this cannot be used to route around a limit, and a cycle of degrades
/// ends in a refusal rather than a loop. The permit set is computed over the whole
/// degrade closure, so the tier a request is degraded to is already held.
pub(super) async fn apply_budgets(
    state: &AppState,
    registry: &Registry,
    resolution: Resolution,
) -> Result<(Resolution, AdmissionGuard)> {
    let admission = state
        .admit(&main_admission_scopes(
            registry.config(),
            registry,
            &resolution,
        ))
        .await;
    let mut resolution = resolution;
    let mut visited: Vec<ModelTier> = Vec::new();

    loop {
        // The tier the request will be billed under. Direct ids participate
        // too: charging uses the target model's tier, so a tier budget must
        // also gate requests that reach the tier by exact id, or the limit
        // could be spent around while still being charged.
        let tier = match &resolution {
            Resolution::Tier(tier) => Some(*tier),
            Resolution::Direct(id) => registry.entry(id).ok().and_then(|m| m.tier),
        };
        // Every provider the request could land on, because a provider's budget has
        // to stop a request that might reach it.
        let provider_ids: Vec<String> = match &resolution {
            Resolution::Tier(tier) => registry
                .tier_members(*tier)
                .iter()
                .map(|m| m.provider_id.clone())
                .collect(),
            Resolution::Direct(id) => registry
                .entry(id)
                .map(|m| vec![m.provider_id.clone()])
                .unwrap_or_default(),
        };

        match state.budget_verdict(&provider_ids, tier) {
            Verdict::Allow => return Ok((resolution, admission)),
            Verdict::Reject { because } => return Err(Error::OverBudget(because)),
            Verdict::Degrade { to, because } => {
                if let Some(current) = tier {
                    visited.push(current);
                }
                if visited.contains(&to) {
                    // Following this would come back here, so refusing is the end.
                    return Err(Error::OverBudget(format!(
                        "{because}, and the tier it degrades to is over its own limit"
                    )));
                }
                tracing::info!("degrading to {}: {because}", to.virtual_id());
                resolution = Resolution::Tier(to);
            }
        }
    }
}

/// Every budgeted scope a main request could spend against, in canonical order.
///
/// That is the global scope, the providers the resolution it starts from could
/// land on, the tier it starts at, and — because a covering tier budget can
/// degrade the request to another tier whose own scope then applies — the
/// closure of those degrade targets. Taking the whole set before the first check
/// is what makes the acquisition order total: each request accumulates a subset
/// of the same ordered gates ascending, so two requests cannot deadlock.
///
/// Only scopes that actually have an enabled budget are listed. A scope with no
/// limit has no gate, so traffic no budget covers is never serialised.
pub(super) fn main_admission_scopes(
    config: &AppConfig,
    registry: &Registry,
    resolution: &Resolution,
) -> Vec<BudgetScope> {
    let budgets: Vec<&Budget> = config.budgets.iter().filter(|b| b.enabled).collect();
    if budgets.is_empty() {
        return Vec::new();
    }
    let budgeted = |scope: &BudgetScope| budgets.iter().any(|b| &b.scope == scope);

    let mut scopes: BTreeSet<BudgetScope> = BTreeSet::new();
    if budgets
        .iter()
        .any(|b| matches!(b.scope, BudgetScope::Global))
    {
        scopes.insert(BudgetScope::Global);
    }

    let mut pending = vec![resolution.clone()];
    let mut seen_tiers: BTreeSet<ModelTier> = BTreeSet::new();
    let mut seen_direct: BTreeSet<String> = BTreeSet::new();
    while let Some(next) = pending.pop() {
        let (providers, tier) = match &next {
            Resolution::Tier(tier) => (
                registry
                    .tier_members(*tier)
                    .iter()
                    .map(|m| m.provider_id.clone())
                    .collect::<Vec<_>>(),
                Some(*tier),
            ),
            Resolution::Direct(id) => {
                if !seen_direct.insert(id.clone()) {
                    continue;
                }
                match registry.entry(id) {
                    Ok(entry) => (vec![entry.provider_id.clone()], entry.tier),
                    Err(_) => (Vec::new(), None),
                }
            }
        };
        for provider in providers {
            let scope = BudgetScope::Provider { id: provider };
            if budgeted(&scope) {
                scopes.insert(scope);
            }
        }
        let Some(tier) = tier else { continue };
        let tier_scope = BudgetScope::Tier { tier };
        if budgeted(&tier_scope) {
            scopes.insert(tier_scope.clone());
        }
        if !seen_tiers.insert(tier) {
            continue;
        }
        for budget in &budgets {
            if budget.scope == tier_scope {
                if let OnExceeded::Degrade { to } = &budget.on_exceeded {
                    pending.push(Resolution::Tier(*to));
                }
            }
        }
    }
    scopes.into_iter().collect()
}

/// Every budgeted scope a classifier side request could spend against.
///
/// A side request is billed like any other, so global and provider limits admit
/// it, and the permits cover every provider in the pool it could fail over to so
/// a failover cannot escape the set. The class (tier) scope is deliberately
/// excluded, matching [`AppState::classifier_budget_verdict`]: the pool is not
/// chosen by tier policy, so a class budget neither degrades nor rejects a side
/// request.
pub(super) fn classifier_admission_scopes(
    config: &AppConfig,
    plan: &[Candidate],
) -> Vec<BudgetScope> {
    let mut scopes = BTreeSet::new();
    for budget in config.budgets.iter().filter(|b| b.enabled) {
        match &budget.scope {
            BudgetScope::Global => {
                scopes.insert(BudgetScope::Global);
            }
            BudgetScope::Provider { id } if plan.iter().any(|c| &c.provider.id == id) => {
                scopes.insert(BudgetScope::Provider { id: id.clone() });
            }
            _ => {}
        }
    }
    scopes.into_iter().collect()
}
