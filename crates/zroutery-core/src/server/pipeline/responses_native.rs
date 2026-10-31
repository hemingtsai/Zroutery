//! Request-local routing for Responses fields that cannot survive a protocol bridge.

use std::sync::Arc;

use crate::config::ModelTier;
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, Dialect};
use crate::policy::{PolicyFallback, RouteDecision, RoutingPolicy, TaskProfile};
use crate::registry::{Registry, Resolution};
use crate::router::Candidate;

/// Build a request-local registry that contains only native Responses providers.
///
/// Direct model requests stay exact and are rejected before planning when their
/// provider is not native. Tier requests may safely search a higher tier, but a
/// Chat model must never remain in the executable pool: encoding it would discard
/// hosted tools, conversation references, or context-management fields.
pub(super) fn registry_for(req: &ChatRequest, registry: &Registry) -> Arc<Registry> {
    if !req.responses_native_only {
        return Arc::new(registry.clone());
    }
    let Ok(Resolution::Tier(requested_tier)) = registry.resolve(&req.model) else {
        return Arc::new(registry.clone());
    };

    let mut config = (*registry.snapshot()).clone();
    for model in &mut config.models {
        let native = registry
            .config()
            .provider(&model.provider_id)
            .is_some_and(|provider| provider.dialect() == Dialect::OpenAIResponses);
        if !native || model.tier.is_some_and(|tier| tier < requested_tier) {
            model.enabled = false;
        }
    }
    Arc::new(Registry::new(Arc::new(config)))
}

/// Fail an exact non-Responses model before an attempt is opened or encoded.
///
/// Exact model selection is never widened to a tier: the client named that model,
/// so the only safe response is to reject rather than silently discard native
/// Responses state or route to a different identity.
pub(super) fn validate_resolution(
    req: &ChatRequest,
    registry: &Registry,
    resolution: &Resolution,
) -> Result<()> {
    if !req.responses_native_only {
        return Ok(());
    }
    let Resolution::Direct(id) = resolution else {
        return Ok(());
    };
    let provider = registry.provider_of(registry.entry(id)?)?;
    if provider.dialect() == Dialect::OpenAIResponses {
        return Ok(());
    }
    Err(native_candidate_error(&req.model))
}

/// Native-only Responses fields are a hard transport constraint, not a user's
/// general fallback preference. Keep the caller's requirements/preferences, but
/// make upward search available through Frontier whenever the requested tier has
/// no executable native member. If the current tier is usable the router never
/// takes this fallback, so same-tier behavior remains unchanged.
pub(super) fn policy<'a>(
    req: &ChatRequest,
    registry: &Registry,
    policy: &'a RoutingPolicy,
) -> std::borrow::Cow<'a, RoutingPolicy> {
    if !req.responses_native_only {
        return std::borrow::Cow::Borrowed(policy);
    }
    let Ok(Resolution::Tier(tier)) = registry.resolve(&req.model) else {
        return std::borrow::Cow::Borrowed(policy);
    };
    if tier == ModelTier::Frontier {
        return std::borrow::Cow::Borrowed(policy);
    }

    let mut adjusted = policy.clone();
    adjusted.fallback = native_fallback(tier);
    std::borrow::Cow::Owned(adjusted)
}

/// Plan a tier request with an explicit routing policy and preserve the router's
/// canonical eligibility and escalation trace.
pub(super) fn plan(
    app: &crate::server::AppState,
    registry: &Registry,
    resolution: &Resolution,
    req: &ChatRequest,
    policy: &RoutingPolicy,
    task: &TaskProfile,
) -> Result<(Vec<Candidate>, RouteDecision)> {
    let (plan, decision) = app.router().plan_with_policy(
        registry,
        resolution,
        &req.required_capabilities,
        &policy.requirements,
        &policy.preference,
        &policy.fallback,
        Some(task),
    )?;
    validate_plan(req, &plan)?;
    log_escalation(req, resolution, &plan);
    Ok((plan, decision))
}

/// Native-only tier planning when no routing policy was resolved.
///
/// Ordinary no-policy requests continue to use `Router::plan` at the call site.
/// This transport-only policy exists solely to make the required upward search
/// explicit and observable for a native-only Responses request.
pub(super) fn plan_without_policy(
    app: &crate::server::AppState,
    registry: &Registry,
    resolution: &Resolution,
    req: &ChatRequest,
    task: &TaskProfile,
) -> Result<(Vec<Candidate>, RouteDecision)> {
    let mut transport = crate::policy::default_policy();
    transport.id.clear();
    transport.name = "Responses-native transport".into();
    transport.preference.preferred_tier = None;
    transport.fallback = PolicyFallback::Reject;
    let transport = policy(req, registry, &transport);
    plan(app, registry, resolution, req, &transport, task)
}

pub(super) fn tier_has_native_candidate(registry: &Registry, tier: ModelTier) -> bool {
    registry.tier_members(tier).into_iter().any(|entry| {
        registry
            .provider_of(entry)
            .is_ok_and(|provider| provider.dialect() == Dialect::OpenAIResponses)
    })
}

fn validate_plan(req: &ChatRequest, plan: &[Candidate]) -> Result<()> {
    if req.responses_native_only
        && plan
            .iter()
            .any(|candidate| candidate.provider.dialect() != Dialect::OpenAIResponses)
    {
        return Err(native_candidate_error(&req.model));
    }
    Ok(())
}

fn log_escalation(req: &ChatRequest, resolution: &Resolution, plan: &[Candidate]) {
    if !req.responses_native_only {
        return;
    }
    let (Resolution::Tier(from), Some(selected)) = (resolution, plan.first()) else {
        return;
    };
    let Some(to) = selected.entry.tier else {
        return;
    };
    if to > *from {
        tracing::info!(
            from = %from.virtual_id(),
            to = %to.virtual_id(),
            model = %selected.exposed_id,
            "Responses-native request escalated to a compatible tier"
        );
    }
}

fn native_fallback(tier: ModelTier) -> PolicyFallback {
    PolicyFallback::Escalate {
        enabled: true,
        max_steps: steps_to_frontier(tier),
    }
}

fn native_candidate_error(model: &str) -> Error {
    Error::NoCandidate(format!(
        "{model}: Responses context or hosted tools require a Responses upstream"
    ))
}

fn steps_to_frontier(mut tier: ModelTier) -> u32 {
    let mut steps = 0;
    while let Some(higher) = tier.higher() {
        steps += 1;
        tier = higher;
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind};
    use crate::policy::DecisionReason;

    #[test]
    fn native_only_tiers_escalate_to_frontier_without_a_policy() {
        let mut config = AppConfig::default();
        let chat = ProviderConfig::new("chat", "Chat", ProviderKind::OpenAICompatible);
        config.providers.push(chat);
        let mut responses =
            ProviderConfig::new("responses", "Responses", ProviderKind::OpenAICompatible);
        responses.api_surface = serde_json::from_value(serde_json::json!("responses")).unwrap();
        config.providers.push(responses);

        for (name, tier) in [
            ("fast-chat", ModelTier::Fast),
            ("standard-chat", ModelTier::Standard),
            ("reasoning-chat", ModelTier::Reasoning),
        ] {
            config
                .models
                .push(ModelEntry::for_upstream("chat", name, Some(tier)));
        }
        config.models.push(ModelEntry::for_upstream(
            "responses",
            "frontier-native",
            Some(ModelTier::Frontier),
        ));

        let state =
            crate::server::AppState::new(config.clone(), Arc::new(MemorySecretStore::new()));
        let base = Registry::new(Arc::new(config));

        for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Reasoning] {
            let req = crate::protocol::responses::decode_request(serde_json::json!({
                "model": tier.virtual_id(),
                "conversation": "conversation-fixture",
                "context_management": [{"type":"compaction","compact_threshold":120000}],
                "tools": [{"type":"web_search_preview","search_context_size":"medium"}],
                "input": "find the current fixture"
            }))
            .unwrap();
            assert!(req.responses_native_only);
            let registry = registry_for(&req, &base);
            let resolution = registry.resolve(&req.model).unwrap();
            let task = TaskProfile::from_request(&req);

            let (plan, decision) =
                plan_without_policy(&state, &registry, &resolution, &req, &task).unwrap();

            assert_eq!(plan[0].exposed_id, "responses-frontier-native");
            assert_eq!(plan[0].provider.dialect(), Dialect::OpenAIResponses);
            assert_eq!(
                decision.selected.as_deref(),
                Some("responses-frontier-native")
            );
            assert!(matches!(
                decision.reason,
                DecisionReason::Escalated { ref from_tier }
                    if from_tier == tier.virtual_id()
            ));
            assert_eq!(decision.fallback_chain, vec![tier.virtual_id()]);
        }
    }
}
