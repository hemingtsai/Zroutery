//! Maintain requested effort while narrowing only the original legal pool.

use crate::error::{Error, Result};
use crate::ir::{Capability, ChatRequest, Dialect, ReasoningRequest};
use crate::router::Candidate;

pub(super) fn same_protocol_direct(req: &ChatRequest, candidate: &Candidate) -> bool {
    let model = crate::query::strip_client_model_modifier(&req.model);
    candidate.provider.dialect() == req.source_dialect
        && (model == candidate.exposed_id
            || candidate.entry.aliases.iter().any(|alias| alias == model))
}

pub(super) fn permitted(req: &ChatRequest, candidate: &Candidate) -> bool {
    // A configured typed decision endpoint receives the classifier question,
    // not a chat-generation request. It has no generative effort/budget knob.
    if candidate.provider.kind.is_decision() {
        return true;
    }
    let Some(reasoning) = &req.reasoning else {
        return true;
    };
    let source = candidate
        .entry
        .reasoning_capability
        .as_ref()
        .filter(|source| {
            source.schema_version == crate::ir::REASONING_SCHEMA_VERSION
                && source.provider_id == candidate.provider.id
                && source.upstream_model == candidate.entry.upstream_model
                && source.dialect == candidate.provider.dialect()
        });
    match reasoning {
        ReasoningRequest::NativeEffort { value } => {
            let levels = source
                .map(|source| source.effort_levels.as_slice())
                .unwrap_or(&candidate.entry.effort_levels);
            if levels.is_empty() {
                return same_protocol_direct(req, candidate);
            }
            if !levels.iter().any(|level| level == value) {
                return false;
            }
            if candidate.provider.dialect() == req.source_dialect {
                return true;
            }
            if crate::protocol::reasoning_bridge::native_effort_bridge(
                req.source_dialect,
                candidate.provider.dialect(),
                value,
            ) {
                return true;
            }
            source.is_some_and(|source| source.effort_budgets.contains_key(value))
        }
        ReasoningRequest::TokenBudget { .. } => {
            source.is_some_and(|source| source.token_budget)
                || (candidate.entry.capabilities.thinking
                    && candidate.provider.dialect() == req.source_dialect)
        }
        // `none` is a request to disable reasoning, not a reasoning feature.
        // It is safe across dialects: native Responses sends the explicit
        // value, while Chat/Anthropic encoders omit or disable their knob.
        ReasoningRequest::Disabled => true,
    }
}

pub(super) fn filter(req: &ChatRequest, plan: Vec<Candidate>) -> Result<Vec<Candidate>> {
    if req.reasoning.is_none() {
        return Ok(plan);
    }
    let plan: Vec<_> = plan
        .into_iter()
        .filter(|candidate| permitted(req, candidate))
        .collect();
    if plan.is_empty() {
        Err(Error::NoCandidate(format!("{}: no candidate in this route supports the requested reasoning control; the effort was not lowered", req.model)))
    } else {
        Ok(plan)
    }
}

/// Narrow before scoring and the attempt cap, not after truncation. Otherwise
/// an unsupported first member hides a valid member beyond max_attempts. This
/// request-local registry retains stable IDs and never changes saved settings.
pub(super) fn registry_for(
    req: &ChatRequest,
    registry: &crate::registry::Registry,
) -> std::sync::Arc<crate::registry::Registry> {
    if req.reasoning.is_none() || matches!(req.reasoning.as_ref(), Some(ReasoningRequest::Disabled))
    {
        return std::sync::Arc::new(registry.clone());
    }
    let mut config = (*registry.snapshot()).clone();
    for model in &mut config.models {
        let Some(provider) = registry.config().provider(&model.provider_id) else {
            continue;
        };
        if provider.kind.is_decision() {
            continue;
        }
        let candidate = Candidate {
            exposed_id: model.exposed_id(),
            entry: model.clone(),
            provider: provider.clone(),
            degraded: false,
            credential: None,
        };
        if !permitted(req, &candidate) {
            model.enabled = false;
        }
    }
    std::sync::Arc::new(crate::registry::Registry::new(std::sync::Arc::new(config)))
}

/// A permanently unsupported request control is a local 422, not a transient
/// 503 for a native client's retry loop. Check against the original pool before
/// mutating its request-local eligibility view.
pub(super) fn validate_scope(
    req: &ChatRequest,
    registry: &crate::registry::Registry,
    fallback: &crate::policy::PolicyFallback,
) -> Result<()> {
    let Some(reasoning) = &req.reasoning else {
        return Ok(());
    };
    let resolution = registry.resolve(&req.model)?;
    let tiers = match &resolution {
        crate::registry::Resolution::Tier(tier) => {
            let mut tiers = vec![*tier];
            let direction = match fallback {
                crate::policy::PolicyFallback::Escalate {
                    enabled: true,
                    max_steps,
                } => Some((true, *max_steps)),
                crate::policy::PolicyFallback::Degrade {
                    enabled: true,
                    max_steps,
                } => Some((false, *max_steps)),
                _ => None,
            };
            let mut next = *tier;
            if let Some((up, steps)) = direction {
                for _ in 0..steps {
                    let Some(adjacent) = (if up { next.higher() } else { next.lower() }) else {
                        break;
                    };
                    tiers.push(adjacent);
                    next = adjacent;
                }
            }
            tiers
        }
        crate::registry::Resolution::Direct(_) => Vec::new(),
    };
    let members: Vec<_> = match resolution {
        crate::registry::Resolution::Direct(ref id) => vec![registry.entry(id)?],
        crate::registry::Resolution::Tier(_) => tiers
            .into_iter()
            .flat_map(|tier| registry.tier_members(tier))
            .collect(),
    };
    if members.is_empty() {
        return Ok(());
    }
    if members.iter().any(|model| {
        registry.provider_of(model).is_ok_and(|provider| {
            let candidate = Candidate {
                exposed_id: model.exposed_id(),
                entry: (*model).clone(),
                provider: provider.clone(),
                degraded: false,
                credential: None,
            };
            permitted(req, &candidate)
        })
    }) {
        return Ok(());
    }
    let control = match reasoning {
        ReasoningRequest::NativeEffort { value } => format!("native effort `{value}`"),
        ReasoningRequest::TokenBudget { tokens } => format!("token budget {tokens}"),
        ReasoningRequest::Disabled => "disabled reasoning".into(),
    };
    Err(Error::UnsupportedReasoning {
        model: req.model.clone(),
        control,
    })
}

/// Unknown native effort is permitted on an exact same-protocol direct request;
/// remove only the automatic thinking requirement, never any media/tool gates.
pub(super) fn direct_requirements(req: &ChatRequest, provider_dialect: Dialect) -> Vec<Capability> {
    if req
        .reasoning
        .as_ref()
        .and_then(ReasoningRequest::effort)
        .is_some()
        && provider_dialect == req.source_dialect
    {
        req.required_capabilities
            .iter()
            .copied()
            .filter(|capability| *capability != Capability::Thinking)
            .collect()
    } else {
        req.required_capabilities.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind};

    fn candidate(model: &str, levels: &[&str]) -> Candidate {
        let mut entry = ModelEntry::for_upstream("p", model, Some(ModelTier::Standard));
        entry.effort_levels = levels.iter().map(|level| (*level).to_string()).collect();
        entry.capabilities.thinking = true;
        Candidate {
            exposed_id: entry.exposed_id(),
            entry,
            provider: ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible),
            degraded: false,
            credential: None,
        }
    }

    #[test]
    fn gaps_filter_candidates_without_lowering_or_widening_routes() {
        let mut req = ChatRequest::new("standard-class", Dialect::OpenAI);
        req.reasoning = Some(ReasoningRequest::native("medium").unwrap());
        let low_high = candidate("one", &["low", "high"]);
        let medium = candidate("two", &["medium", "high"]);
        assert!(!permitted(&req, &low_high));
        let plan = filter(&req, vec![low_high, medium.clone()]).unwrap();
        assert_eq!(plan, [medium]);
        assert_eq!(req.reasoning.as_ref().unwrap().effort(), Some("medium"));
    }

    #[test]
    fn disabled_reasoning_does_not_require_a_declared_none_variant() {
        let candidate = candidate("ordinary", &[]);
        let mut req = ChatRequest::new("fast-class", Dialect::OpenAIResponses);
        req.reasoning = Some(ReasoningRequest::Disabled);
        assert!(permitted(&req, &candidate));
        assert_eq!(filter(&req, vec![candidate.clone()]).unwrap(), [candidate]);
    }

    #[test]
    fn unknown_control_is_only_preserved_on_exact_same_protocol_direct() {
        let unknown = candidate("unknown", &[]);
        let mut req = ChatRequest::new(&unknown.exposed_id, Dialect::OpenAI);
        req.reasoning = Some(ReasoningRequest::native("future_native").unwrap());
        assert!(permitted(&req, &unknown));
        req.model = "standard-class".into();
        assert!(!permitted(&req, &unknown));
        req.model = unknown.exposed_id.clone();
        req.source_dialect = Dialect::OpenAIResponses;
        assert!(
            !permitted(&req, &unknown),
            "unknown is not evidence for a protocol bridge"
        );
    }

    #[test]
    fn full_set_filter_happens_before_the_attempt_cap() {
        let one = candidate("one", &["low", "high"]);
        let two = candidate("two", &["medium"]);
        let mut config = AppConfig::default();
        config.providers.push(one.provider.clone());
        config.models = vec![one.entry, two.entry];
        config.routing.max_attempts = 1;
        let mut req = ChatRequest::new("standard-class", Dialect::OpenAI);
        req.reasoning = Some(ReasoningRequest::native("medium").unwrap());
        let registry = crate::registry::Registry::new(std::sync::Arc::new(config));
        let narrowed = registry_for(&req, &registry);
        let plan = crate::router::Router::new()
            .plan(
                &narrowed,
                &crate::registry::Resolution::Tier(ModelTier::Standard),
                &[],
            )
            .unwrap();
        assert_eq!(plan[0].entry.upstream_model, "two");
        assert_eq!(
            registry.tier_members(ModelTier::Standard).len(),
            2,
            "request narrowing never edits config"
        );
    }

    #[test]
    fn claude_native_max_can_reach_declared_openai_effort_without_budget_evidence() {
        let upstream = candidate("reasoner", &["low", "high", "max"]);
        let mut req = ChatRequest::new("frontier-class", Dialect::Anthropic);
        req.reasoning = Some(ReasoningRequest::native("max").unwrap());
        assert!(permitted(&req, &upstream));
        req.reasoning = Some(ReasoningRequest::native("minimal").unwrap());
        assert!(!permitted(&req, &upstream));
    }

    #[test]
    fn unsupported_effort_scope_is_not_a_retryable_outage_or_an_implicit_cross_tier_fallback() {
        let mut standard = candidate("standard", &["high"]);
        standard.entry.tier = Some(ModelTier::Standard);
        let mut frontier = candidate("frontier", &["max"]);
        frontier.entry.tier = Some(ModelTier::Frontier);
        let mut config = AppConfig::default();
        config.providers.push(standard.provider.clone());
        config.models = vec![standard.entry, frontier.entry];
        let registry = crate::registry::Registry::new(std::sync::Arc::new(config));
        let mut req = ChatRequest::new("standard-class", Dialect::Anthropic);
        req.reasoning = Some(ReasoningRequest::native("max").unwrap());
        assert!(matches!(
            validate_scope(&req, &registry, &crate::policy::PolicyFallback::Reject),
            Err(Error::UnsupportedReasoning { .. })
        ));
        assert!(validate_scope(
            &req,
            &registry,
            &crate::policy::PolicyFallback::Escalate {
                enabled: true,
                max_steps: 2
            }
        )
        .is_ok());
        req.model = "frontier-class".into();
        req.reasoning = Some(ReasoningRequest::native("medium").unwrap());
        assert_eq!(
            validate_scope(&req, &registry, &crate::policy::PolicyFallback::Reject)
                .unwrap_err()
                .status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
