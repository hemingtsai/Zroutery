//! Narrow native Messages requests before scoring; never invent a wider route.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::ir::{ChatRequest, Dialect};
use crate::registry::{Registry, Resolution};

pub(super) fn validate_scope(
    req: &ChatRequest,
    registry: &Registry,
    fallback: &crate::policy::PolicyFallback,
) -> Result<()> {
    if !req.anthropic_native_only {
        return Ok(());
    }
    let resolution = registry.resolve(&req.model)?;
    let Resolution::Tier(tier) = resolution else {
        return validate_resolution(req, registry, &resolution);
    };
    let mut tiers = vec![tier];
    let (up, steps) = match fallback {
        crate::policy::PolicyFallback::Escalate {
            enabled: true,
            max_steps,
        } => (true, *max_steps),
        crate::policy::PolicyFallback::Degrade {
            enabled: true,
            max_steps,
        } => (false, *max_steps),
        _ => (true, 0),
    };
    let mut at = tier;
    for _ in 0..steps {
        let Some(next) = (if up { at.higher() } else { at.lower() }) else {
            break;
        };
        tiers.push(next);
        at = next;
    }
    if tiers
        .into_iter()
        .flat_map(|tier| registry.tier_members(tier))
        .any(|model| {
            registry.provider_of(model).is_ok_and(|provider| {
                provider.dialect() == Dialect::Anthropic && !provider.kind.is_decision()
            })
        })
    {
        return Ok(());
    }
    crate::protocol::anthropic::validate_bridge(req, Dialect::OpenAI)
}

pub(super) fn registry_for(req: &ChatRequest, registry: &Registry) -> Arc<Registry> {
    if !req.anthropic_native_only {
        return Arc::new(registry.clone());
    }
    let mut config = (*registry.snapshot()).clone();
    for model in &mut config.models {
        let native = registry
            .config()
            .provider(&model.provider_id)
            .is_some_and(|provider| {
                provider.dialect() == Dialect::Anthropic && !provider.kind.is_decision()
            });
        if !native {
            model.enabled = false;
        }
        if matches!(registry.resolve(&req.model), Ok(Resolution::Tier(_)))
            && req
                .required_capabilities
                .iter()
                .any(|capability| !model.capabilities.supports(*capability))
        {
            model.enabled = false;
        }
    }
    Arc::new(Registry::new(Arc::new(config)))
}

pub(super) fn validate_resolution(
    req: &ChatRequest,
    registry: &Registry,
    resolution: &Resolution,
) -> Result<()> {
    if let Resolution::Direct(id) = resolution {
        let provider = registry.provider_of(registry.entry(id)?)?;
        crate::protocol::anthropic::validate_bridge(req, provider.dialect())?;
        if provider.kind.is_decision() {
            return Err(Error::invalid(
                "Messages generation cannot be answered by a SystemOne decision endpoint",
            ));
        }
    }
    Ok(())
}
