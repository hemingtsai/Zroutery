use std::sync::Arc;

use zroutery_core::config::{AppConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind};
use zroutery_core::policy::{
    self, ClientMatcher, ClientProfile, Complexity, MatchContext, RoutingPolicy, ScoringContext,
    TaskProfile, TaskType,
};
use zroutery_core::registry::Registry;

// ========================================================================
// Helpers
// ========================================================================

pub(super) fn make_profile(complexity: Complexity) -> TaskProfile {
    TaskProfile {
        tier: None,
        required_capabilities: vec![],
        context_tokens: 0,
        estimated_output_tokens: 0,
        streaming: false,
        has_tools: false,
        has_vision: false,
        complexity,
        task_type: TaskType::Chat,
    }
}

pub(super) fn default_policy() -> RoutingPolicy {
    policy::default_policy()
}

pub(super) fn simple_match_ctx<'a>(model: &'a str) -> MatchContext<'a> {
    MatchContext {
        client_id: None,
        application: None,
        model,
        streaming: false,
        has_tools: false,
        has_vision: false,
        task: None,
    }
}

pub(super) fn make_scoring_ctx<'a>(
    health: f64,
    latency: f64,
    cost: Option<f64>,
    priority: i32,
    tier: Option<ModelTier>,
) -> ScoringContext<'a> {
    ScoringContext {
        health,
        avg_latency_ms: latency,
        input_per_mtok: cost,
        output_per_mtok: cost,
        priority,
        tier,
        task: None,
    }
}

pub(super) fn codex_profile() -> ClientProfile {
    ClientProfile {
        id: "codex".into(),
        name: "Codex CLI".into(),
        matchers: vec![ClientMatcher::ClientId {
            value: "codex".into(),
        }],
        policy_id: "code-policy".into(),
    }
}

pub(super) fn bot_profile() -> ClientProfile {
    ClientProfile {
        id: "bot".into(),
        name: "Bot via API key".into(),
        matchers: vec![ClientMatcher::ApiKeyPrefix {
            value: "sk-bot-".into(),
        }],
        policy_id: "bot-policy".into(),
    }
}

pub(super) fn ua_profile() -> ClientProfile {
    ClientProfile {
        id: "curl".into(),
        name: "curl client".into(),
        matchers: vec![ClientMatcher::UserAgent {
            value: "curl".into(),
        }],
        policy_id: "default".into(),
    }
}

pub(super) fn e2e_cfg_with(models: Vec<ModelEntry>) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig::new(
        "p1",
        "P1",
        ProviderKind::OpenAICompatible,
    ));
    cfg.providers.push(ProviderConfig::new(
        "p2",
        "P2",
        ProviderKind::OpenAICompatible,
    ));
    cfg.models = models;
    // Default policy preferences include cost. These capability/order fixtures
    // have an explicit common fallback; unknown-rate refusal is covered by the
    // execution-price contract suite rather than bypassed in production.
    for model in &mut cfg.models {
        if model.pricing.is_none() {
            model.pricing = Some(zroutery_core::billing::Pricing::new("USD", 1.0, 2.0));
        }
    }
    cfg
}

pub(super) fn e2e_reg(cfg: AppConfig) -> Registry {
    Registry::new(Arc::new(cfg))
}
