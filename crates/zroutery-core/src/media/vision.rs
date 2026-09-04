//! Describing an image with a vision model, once, on behalf of a model that
//! cannot see.
//!
//! The description request is ordinary traffic: it goes through the same
//! encoder, quirks and transport as anything else, so a vision model on any
//! provider works. Its outcome decides between two honest results — the
//! description, or the failure that leaves the placeholder.

use crate::config::{AppConfig, ModelEntry, ProviderConfig};
use crate::error::Result;
use crate::ir::{ChatRequest, ChatResponse, ContentBlock, Message};
use crate::upstream::Upstream;

/// The prompt for the describing model. Short on purpose: the description is
/// context for another model, not an answer to the user, and a long prompt
/// buys verbosity, not accuracy.
const DESCRIBE_PROMPT: &str = "Describe this image factually and completely, in one paragraph. \
     Mention text visible in the image verbatim where it matters.";

/// The resolved vision fallback: which model answers, on which provider.
pub struct VisionTarget {
    pub entry: ModelEntry,
    pub provider: ProviderConfig,
}

/// Resolve the configured vision model against the registry.
///
/// `None` when vision fallback is off, no model is set, or the reference does
/// not resolve — in every one of those cases the caller falls back to the
/// placeholder, never to silence.
pub fn resolve(config: &AppConfig) -> Option<VisionTarget> {
    if !config.vision.enabled {
        return None;
    }
    let model_id = config.vision.model.as_deref()?.trim();
    if model_id.is_empty() {
        return None;
    }
    let entry = config.model(model_id)?;
    if !entry.enabled || !entry.supports_vision {
        return None;
    }
    let provider = config.provider(&entry.provider_id)?;
    if !provider.enabled {
        return None;
    }
    Some(VisionTarget {
        entry: entry.clone(),
        provider: provider.clone(),
    })
}

/// Ask the vision model to describe one image.
pub async fn describe(
    upstream: &Upstream,
    target: &VisionTarget,
    api_key: Option<&str>,
    source: &crate::ir::MediaSource,
) -> Result<ChatResponse> {
    let mut req = ChatRequest::new(&target.entry.upstream_model, target.provider.kind.dialect());
    req.messages.push(Message {
        role: crate::ir::Role::User,
        content: vec![
            ContentBlock::Image { source: source.clone() },
            ContentBlock::text(DESCRIBE_PROMPT),
        ],
    });
    req.max_tokens = Some(512);
    req.temperature = Some(0.0);
    // Descriptions go to the same model each time, so caching the prompt part
    // would only ever help the image-less prefix — not worth the machinery.
    let body = crate::upstream::encode_for(
        &target.provider,
        &req,
        &target.entry.upstream_model,
        target.entry.max_output_tokens,
    )?;
    upstream.send(&target.provider, api_key, &body).await
}

/// The description text from a response, trimmed of the model's own padding.
pub fn description_text(resp: &ChatResponse) -> String {
    resp.text().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelClass;

    fn config_with(vision: Option<&str>) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.providers.push(ProviderConfig::new(
            "p",
            "P",
            crate::config::ProviderKind::OpenAICompatible,
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "p",
            "vision-model",
            Some(ModelClass::Haiku),
        ));
        cfg.models[0].supports_vision = true;
        cfg.vision.enabled = vision.is_some();
        cfg.vision.model = vision.map(str::to_string);
        cfg
    }

    #[test]
    fn resolves_only_when_everything_lines_up() {
        assert!(resolve(&config_with(None)).is_none());

        let with = config_with(Some("p-vision-model"));
        assert!(resolve(&with).is_some());

        // Off: no resolution even with a model configured.
        let mut off = with.clone();
        off.vision.enabled = false;
        assert!(resolve(&off).is_none());

        // A model that cannot see is not a vision fallback.
        let mut blind = with.clone();
        blind.models[0].supports_vision = false;
        assert!(resolve(&blind).is_none());

        // A dangling reference resolves to nothing, not an error.
        let mut dangling = with.clone();
        dangling.vision.model = Some("nope".into());
        assert!(resolve(&dangling).is_none());
    }

    #[test]
    fn description_text_is_trimmed() {
        let mut resp = ChatResponse {
            id: "m".into(),
            model: "v".into(),
            content: Vec::new(),
            stop_reason: crate::ir::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
        };
        resp.content.push(ContentBlock::text("  a picture of a cat.  "));
        assert_eq!(description_text(&resp), "a picture of a cat.");
    }
}
