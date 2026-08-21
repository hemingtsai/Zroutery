//! Maps a client supplied model id onto either one concrete model or a class of
//! models, and produces the `/v1/models` listing.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, ModelClass, ModelEntry, ProviderConfig};
use crate::error::{Error, Result};

/// Outcome of resolving a client model id.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// The client named an exact model (or one of its aliases).
    Direct(String),
    /// The client named a virtual `*-class` id, or an alias that maps to a class.
    Class(ModelClass),
}

/// Immutable snapshot of the model registry.
///
/// Cheap to clone; the server swaps in a new snapshot when configuration
/// changes so in-flight requests keep using a consistent view.
#[derive(Debug, Clone)]
pub struct Registry {
    config: Arc<AppConfig>,
}

/// One entry of the `/v1/models` listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    /// `None` for virtual class ids.
    pub provider_name: Option<String>,
    pub class: Option<ModelClass>,
    pub virtual_model: bool,
    /// For virtual ids: how many enabled models back it.
    pub member_count: usize,
    /// Extra ids that also resolve to this model.
    pub aliases: Vec<String>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_thinking: bool,
}

impl Registry {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Registry { config }
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn snapshot(&self) -> Arc<AppConfig> {
        Arc::clone(&self.config)
    }

    /// Resolve what the client asked for.
    ///
    /// Order: exact model id, model alias, `*-class` virtual id, configured
    /// client alias, Claude-style name heuristic (opt-in), unknown-model
    /// fallback class.
    pub fn resolve(&self, requested: &str) -> Result<Resolution> {
        let asked = requested.trim();
        if asked.is_empty() {
            return Err(Error::invalid("`model` must not be empty"));
        }

        if let Some(m) = self
            .config
            .models
            .iter()
            .find(|m| m.enabled && m.exposed_id() == asked)
        {
            return Ok(Resolution::Direct(m.exposed_id()));
        }
        if let Some(m) = self
            .config
            .models
            .iter()
            .find(|m| m.enabled && m.aliases.iter().any(|a| a == asked))
        {
            return Ok(Resolution::Direct(m.exposed_id()));
        }
        if let Some(class) = ModelClass::from_virtual_id(asked) {
            return Ok(Resolution::Class(class));
        }
        if let Some(class) = self.config.routing.client_aliases.get(asked) {
            return Ok(Resolution::Class(*class));
        }
        if self.config.routing.match_claude_names {
            if let Some(class) = class_from_name(asked) {
                return Ok(Resolution::Class(class));
            }
        }
        // A disabled-but-known id gets a clearer error than a typo.
        if let Some(m) = self.config.models.iter().find(|m| m.answers_to(asked)) {
            if let Some(class) = self.config.routing.unknown_model_fallback {
                return Ok(Resolution::Class(class));
            }
            return Err(Error::UnknownModel(format!(
                "{} (disabled)",
                m.exposed_id()
            )));
        }
        if let Some(class) = self.config.routing.unknown_model_fallback {
            return Ok(Resolution::Class(class));
        }
        Err(Error::UnknownModel(asked.to_string()))
    }

    /// All enabled models of a class, sorted by priority (ascending) then id.
    pub fn class_members(&self, class: ModelClass) -> Vec<&ModelEntry> {
        let mut v: Vec<&ModelEntry> = self
            .config
            .models
            .iter()
            .filter(|m| m.enabled && m.class == Some(class))
            .filter(|m| {
                self.config
                    .provider(&m.provider_id)
                    .map(|p| p.enabled)
                    .unwrap_or(false)
            })
            .collect();
        v.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.exposed_id().cmp(&b.exposed_id()))
        });
        v
    }

    /// Look up a model by exposed id or alias, enabled or not.
    pub fn entry(&self, id: &str) -> Result<&ModelEntry> {
        self.config
            .models
            .iter()
            .find(|m| m.answers_to(id))
            .ok_or_else(|| Error::UnknownModel(id.to_string()))
    }

    pub fn provider_of(&self, entry: &ModelEntry) -> Result<&ProviderConfig> {
        self.config.provider(&entry.provider_id).ok_or_else(|| {
            Error::internal(format!(
                "model `{}` references unknown provider `{}`",
                entry.upstream_model, entry.provider_id
            ))
        })
    }

    /// The listing returned by `GET /v1/models`: concrete models first, then any
    /// class id that currently has at least one usable member.
    pub fn list(&self) -> Vec<ModelInfo> {
        let mut out: Vec<ModelInfo> = Vec::new();
        for m in self.config.models.iter().filter(|m| m.enabled) {
            let provider = self.config.provider(&m.provider_id);
            if provider.map(|p| !p.enabled).unwrap_or(true) {
                continue;
            }
            out.push(ModelInfo {
                id: m.exposed_id(),
                display_name: m
                    .display_name
                    .clone()
                    .unwrap_or_else(|| m.upstream_model.clone()),
                provider_name: provider.map(|p| p.name.clone()),
                class: m.class,
                virtual_model: false,
                member_count: 0,
                aliases: m.aliases.clone(),
                supports_tools: m.supports_tools,
                supports_vision: m.supports_vision,
                supports_thinking: m.supports_thinking,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));

        for class in ModelClass::ALL {
            let members = self.class_members(class);
            if members.is_empty() {
                continue;
            }
            out.push(ModelInfo {
                id: class.virtual_id().to_string(),
                display_name: format!("{} (auto)", class.virtual_id()),
                provider_name: None,
                class: Some(class),
                virtual_model: true,
                member_count: members.len(),
                aliases: Vec::new(),
                supports_tools: members.iter().all(|m| m.supports_tools),
                supports_vision: members.iter().all(|m| m.supports_vision),
                supports_thinking: members.iter().all(|m| m.supports_thinking),
            });
        }
        out
    }
}

/// Recognise Anthropic-style model names sent by clients such as Claude Code.
fn class_from_name(name: &str) -> Option<ModelClass> {
    let n = name.to_ascii_lowercase();
    if n.contains("opus") {
        Some(ModelClass::Opus)
    } else if n.contains("sonnet") {
        Some(ModelClass::Sonnet)
    } else if n.contains("haiku") {
        Some(ModelClass::Haiku)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderKind, RoutingConfig};

    fn registry(cfg: AppConfig) -> Registry {
        Registry::new(Arc::new(cfg))
    }

    /// The scenario from the product brief: DeepSeek (flash + pro) and OpenAI
    /// (gpt-5.3-sol), with classes assigned by hand.
    fn brief_config() -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.providers.push(ProviderConfig::new(
            "deepseek",
            "DeepSeek",
            ProviderKind::OpenAICompatible,
        ));
        cfg.providers.push(ProviderConfig::new(
            "openai",
            "OpenAI",
            ProviderKind::OpenAICompatible,
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-v4-flash",
            Some(ModelClass::Haiku),
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-v4-pro",
            Some(ModelClass::Sonnet),
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "openai",
            "gpt-5.3-sol",
            Some(ModelClass::Opus),
        ));
        cfg
    }

    #[test]
    fn lists_concrete_and_virtual_models() {
        let r = registry(brief_config());
        let ids: Vec<String> = r.list().into_iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            vec![
                "deepseek-deepseek-v4-flash",
                "deepseek-deepseek-v4-pro",
                "openai-gpt-5.3-sol",
                "opus-class",
                "sonnet-class",
                "haiku-class",
            ]
        );
        // The short upstream name is what the listing shows as a label.
        let info = r
            .list()
            .into_iter()
            .find(|m| m.id == "openai-gpt-5.3-sol")
            .unwrap();
        assert_eq!(info.display_name, "gpt-5.3-sol");
        assert_eq!(info.provider_name.as_deref(), Some("OpenAI"));
    }

    #[test]
    fn the_same_model_on_two_providers_resolves_independently() {
        let mut cfg = brief_config();
        cfg.providers.push(ProviderConfig::new(
            "openrouter",
            "OpenRouter",
            ProviderKind::OpenAICompatible,
        ));
        // Same upstream name as the direct DeepSeek entry.
        cfg.models.push(ModelEntry::for_upstream(
            "openrouter",
            "deepseek-v4-pro",
            Some(ModelClass::Sonnet),
        ));
        let r = registry(cfg);

        let direct = r.resolve("deepseek-deepseek-v4-pro").unwrap();
        let proxied = r.resolve("openrouter-deepseek-v4-pro").unwrap();
        assert_ne!(direct, proxied);
        assert_eq!(
            r.entry("deepseek-deepseek-v4-pro").unwrap().provider_id,
            "deepseek"
        );
        assert_eq!(
            r.entry("openrouter-deepseek-v4-pro").unwrap().provider_id,
            "openrouter"
        );
        // Both are members of the class and can fail over to each other.
        assert_eq!(
            r.class_members(ModelClass::Sonnet)
                .iter()
                .map(|m| m.exposed_id())
                .collect::<Vec<_>>(),
            vec!["deepseek-deepseek-v4-pro", "openrouter-deepseek-v4-pro"]
        );
    }

    #[test]
    fn unclassified_model_is_callable_but_not_in_a_class() {
        let mut cfg = brief_config();
        cfg.models
            .push(ModelEntry::for_upstream("openai", "mystery-1", None));
        let r = registry(cfg);
        assert_eq!(
            r.resolve("openai-mystery-1").unwrap(),
            Resolution::Direct("openai-mystery-1".into())
        );
        for class in ModelClass::ALL {
            assert!(r
                .class_members(class)
                .iter()
                .all(|m| m.upstream_model != "mystery-1"));
        }
        assert!(r
            .list()
            .iter()
            .any(|m| m.id == "openai-mystery-1" && m.class.is_none()));
    }

    #[test]
    fn resolves_class_ids_and_direct_ids() {
        let r = registry(brief_config());
        assert_eq!(
            r.resolve("sonnet-class").unwrap(),
            Resolution::Class(ModelClass::Sonnet)
        );
        assert_eq!(
            r.resolve("openai-gpt-5.3-sol").unwrap(),
            Resolution::Direct("openai-gpt-5.3-sol".into())
        );
        // The bare upstream name is not an id any more.
        assert!(r.resolve("gpt-5.3-sol").is_err());
        assert_eq!(
            r.class_members(ModelClass::Opus)
                .iter()
                .map(|m| m.exposed_id())
                .collect::<Vec<_>>(),
            vec!["openai-gpt-5.3-sol"]
        );
    }

    #[test]
    fn claude_style_names_map_to_classes() {
        let r = registry(brief_config());
        assert_eq!(
            r.resolve("claude-sonnet-4-5-20250929").unwrap(),
            Resolution::Class(ModelClass::Sonnet)
        );
        assert_eq!(
            r.resolve("claude-3-5-haiku-latest").unwrap(),
            Resolution::Class(ModelClass::Haiku)
        );

        let mut cfg = brief_config();
        cfg.routing.match_claude_names = false;
        let r = registry(cfg);
        assert!(r.resolve("claude-sonnet-4-5-20250929").is_err());
    }

    #[test]
    fn explicit_alias_beats_heuristic() {
        let mut cfg = brief_config();
        // Route Claude's opus name to the cheap tier on purpose.
        cfg.routing
            .client_aliases
            .insert("claude-opus-4-1".into(), ModelClass::Haiku);
        let r = registry(cfg);
        assert_eq!(
            r.resolve("claude-opus-4-1").unwrap(),
            Resolution::Class(ModelClass::Haiku)
        );
    }

    #[test]
    fn model_alias_resolves_directly() {
        let mut cfg = brief_config();
        cfg.models[1].aliases.push("ds-pro".into());
        let r = registry(cfg);
        assert_eq!(
            r.resolve("ds-pro").unwrap(),
            Resolution::Direct("deepseek-deepseek-v4-pro".into())
        );
    }

    #[test]
    fn unknown_model_errors_unless_fallback_is_set() {
        let r = registry(brief_config());
        let err = r.resolve("does-not-exist").unwrap_err();
        assert!(matches!(err, Error::UnknownModel(_)));

        let mut cfg = brief_config();
        cfg.routing = RoutingConfig {
            unknown_model_fallback: Some(ModelClass::Sonnet),
            ..RoutingConfig::default()
        };
        let r = registry(cfg);
        assert_eq!(
            r.resolve("does-not-exist").unwrap(),
            Resolution::Class(ModelClass::Sonnet)
        );
    }

    #[test]
    fn disabled_provider_hides_its_models() {
        let mut cfg = brief_config();
        cfg.providers[0].enabled = false;
        let r = registry(cfg);
        assert!(r.class_members(ModelClass::Sonnet).is_empty());
        assert!(!r.list().iter().any(|m| m.id == "sonnet-class"));
        assert!(!r.list().iter().any(|m| m.id == "deepseek-deepseek-v4-pro"));
    }

    #[test]
    fn priority_orders_class_members() {
        let mut cfg = brief_config();
        cfg.models.push(
            ModelEntry::for_upstream("openai", "backup-sonnet", Some(ModelClass::Sonnet))
                .with_priority(-5),
        );
        let r = registry(cfg);
        assert_eq!(
            r.class_members(ModelClass::Sonnet)
                .iter()
                .map(|m| m.exposed_id())
                .collect::<Vec<_>>(),
            vec!["openai-backup-sonnet", "deepseek-deepseek-v4-pro"]
        );
    }
}
