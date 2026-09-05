//! Contract tests for the ModelTier system.
//!
//! These lock down the tier ordering, escalation/de-escalation, serde
//! round-trips, legacy aliases, virtual IDs, Fable matching, and naming
//! styles — so that any future change to the tier model is deliberate.

use serde_json::json;
use zroutery_core::config::{ModelCapabilities, ModelTier, NamingStyle};

// --------------------------------------------------------------- ordering

#[test]
fn tier_ordering_is_fast_lt_standard_lt_reasoning_lt_frontier() {
    assert!(ModelTier::Fast < ModelTier::Standard);
    assert!(ModelTier::Standard < ModelTier::Reasoning);
    assert!(ModelTier::Reasoning < ModelTier::Frontier);
}

#[test]
fn all_contains_four_tiers_in_order() {
    assert_eq!(
        ModelTier::ALL,
        [
            ModelTier::Fast,
            ModelTier::Standard,
            ModelTier::Reasoning,
            ModelTier::Frontier,
        ]
    );
}

// ---------------------------------------------------------- escalation chain

#[test]
fn higher_goes_fast_standard_reasoning_frontier() {
    assert_eq!(ModelTier::Fast.higher(), Some(ModelTier::Standard));
    assert_eq!(ModelTier::Standard.higher(), Some(ModelTier::Reasoning));
    assert_eq!(ModelTier::Reasoning.higher(), Some(ModelTier::Frontier));
    assert_eq!(ModelTier::Frontier.higher(), None);
}

#[test]
fn lower_goes_frontier_reasoning_standard_fast() {
    assert_eq!(ModelTier::Frontier.lower(), Some(ModelTier::Reasoning));
    assert_eq!(ModelTier::Reasoning.lower(), Some(ModelTier::Standard));
    assert_eq!(ModelTier::Standard.lower(), Some(ModelTier::Fast));
    assert_eq!(ModelTier::Fast.lower(), None);
}

#[test]
fn higher_then_lower_is_identity() {
    for tier in ModelTier::ALL {
        if let Some(up) = tier.higher() {
            assert_eq!(up.lower(), Some(tier), "{tier:?} -> higher -> lower should round-trip");
        }
    }
}

// --------------------------------------------------------------- serde

#[test]
fn serde_round_trip() {
    for tier in ModelTier::ALL {
        let json = serde_json::to_string(&tier).unwrap();
        let back: ModelTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tier, "round-trip for {tier:?}");
    }
}

#[test]
fn serde_lowercase() {
    assert_eq!(serde_json::to_string(&ModelTier::Fast).unwrap(), "\"fast\"");
    assert_eq!(
        serde_json::to_string(&ModelTier::Standard).unwrap(),
        "\"standard\""
    );
    assert_eq!(
        serde_json::to_string(&ModelTier::Reasoning).unwrap(),
        "\"reasoning\""
    );
    assert_eq!(
        serde_json::to_string(&ModelTier::Frontier).unwrap(),
        "\"frontier\""
    );
}

// --------------------------------------------------------- legacy aliases

#[test]
fn legacy_alias_haiku_deserializes_to_fast() {
    let tier: ModelTier = serde_json::from_str("\"haiku\"").unwrap();
    assert_eq!(tier, ModelTier::Fast);
}

#[test]
fn legacy_alias_sonnet_deserializes_to_standard() {
    let tier: ModelTier = serde_json::from_str("\"sonnet\"").unwrap();
    assert_eq!(tier, ModelTier::Standard);
}

#[test]
fn legacy_alias_opus_deserializes_to_reasoning() {
    let tier: ModelTier = serde_json::from_str("\"opus\"").unwrap();
    assert_eq!(tier, ModelTier::Reasoning);
}

// ------------------------------------------------------------ virtual IDs

#[test]
fn virtual_id_internal_style() {
    assert_eq!(ModelTier::Fast.virtual_id(), "fast-class");
    assert_eq!(ModelTier::Standard.virtual_id(), "standard-class");
    assert_eq!(ModelTier::Reasoning.virtual_id(), "reasoning-class");
    assert_eq!(ModelTier::Frontier.virtual_id(), "frontier-class");
}

#[test]
fn from_virtual_id_accepts_all_styles() {
    // Internal
    assert_eq!(ModelTier::from_virtual_id("fast-class"), Some(ModelTier::Fast));
    assert_eq!(
        ModelTier::from_virtual_id("standard-class"),
        Some(ModelTier::Standard)
    );
    assert_eq!(
        ModelTier::from_virtual_id("reasoning-class"),
        Some(ModelTier::Reasoning)
    );
    assert_eq!(
        ModelTier::from_virtual_id("frontier-class"),
        Some(ModelTier::Frontier)
    );
    // Anthropic
    assert_eq!(
        ModelTier::from_virtual_id("haiku-class"),
        Some(ModelTier::Fast)
    );
    assert_eq!(
        ModelTier::from_virtual_id("sonnet-class"),
        Some(ModelTier::Standard)
    );
    assert_eq!(
        ModelTier::from_virtual_id("opus-class"),
        Some(ModelTier::Reasoning)
    );
    assert_eq!(
        ModelTier::from_virtual_id("fable-class"),
        Some(ModelTier::Frontier)
    );
    // OpenAI
    assert_eq!(ModelTier::from_virtual_id("luna-class"), Some(ModelTier::Fast));
    assert_eq!(
        ModelTier::from_virtual_id("terra-class"),
        Some(ModelTier::Standard)
    );
    assert_eq!(
        ModelTier::from_virtual_id("sol-class"),
        Some(ModelTier::Reasoning)
    );
    assert_eq!(
        ModelTier::from_virtual_id("astra-class"),
        Some(ModelTier::Frontier)
    );
}

#[test]
fn from_virtual_id_rejects_unknown() {
    assert_eq!(ModelTier::from_virtual_id("gpt-5.3-sol"), None);
    assert_eq!(ModelTier::from_virtual_id("claude-3-opus"), None);
    assert_eq!(ModelTier::from_virtual_id(""), None);
}

#[test]
fn virtual_id_styled_matches_from_virtual_id() {
    for tier in ModelTier::ALL {
        for style in [NamingStyle::Internal, NamingStyle::Anthropic, NamingStyle::OpenAI] {
            let vid = tier.virtual_id_styled(style);
            assert_eq!(
                ModelTier::from_virtual_id(vid),
                Some(tier),
                "virtual_id_styled({tier:?}, {style:?}) = {vid} should round-trip"
            );
        }
    }
}

// --------------------------------------------------------- display names

#[test]
fn display_name_internal() {
    assert_eq!(ModelTier::Fast.display_name(NamingStyle::Internal), "Fast");
    assert_eq!(
        ModelTier::Standard.display_name(NamingStyle::Internal),
        "Standard"
    );
    assert_eq!(
        ModelTier::Reasoning.display_name(NamingStyle::Internal),
        "Reasoning"
    );
    assert_eq!(
        ModelTier::Frontier.display_name(NamingStyle::Internal),
        "Frontier"
    );
}

#[test]
fn display_name_anthropic() {
    assert_eq!(ModelTier::Fast.display_name(NamingStyle::Anthropic), "Haiku");
    assert_eq!(
        ModelTier::Standard.display_name(NamingStyle::Anthropic),
        "Sonnet"
    );
    assert_eq!(
        ModelTier::Reasoning.display_name(NamingStyle::Anthropic),
        "Opus"
    );
    assert_eq!(
        ModelTier::Frontier.display_name(NamingStyle::Anthropic),
        "Fable"
    );
}

#[test]
fn display_name_openai() {
    assert_eq!(ModelTier::Fast.display_name(NamingStyle::OpenAI), "Luna");
    assert_eq!(ModelTier::Standard.display_name(NamingStyle::OpenAI), "Terra");
    assert_eq!(ModelTier::Reasoning.display_name(NamingStyle::OpenAI), "Sol");
    assert_eq!(ModelTier::Frontier.display_name(NamingStyle::OpenAI), "Astra");
}

// ----------------------------------------------------------- as_str

#[test]
fn as_str_returns_internal_name() {
    assert_eq!(ModelTier::Fast.as_str(), "fast");
    assert_eq!(ModelTier::Standard.as_str(), "standard");
    assert_eq!(ModelTier::Reasoning.as_str(), "reasoning");
    assert_eq!(ModelTier::Frontier.as_str(), "frontier");
}

// ----------------------------------------------------- naming style serde

#[test]
fn naming_style_default_is_internal() {
    let style: NamingStyle = serde_json::from_str("\"internal\"").unwrap();
    assert_eq!(style, NamingStyle::Internal);
}

#[test]
fn naming_style_round_trip() {
    for style in [NamingStyle::Internal, NamingStyle::Anthropic, NamingStyle::OpenAI] {
        let json = serde_json::to_string(&style).unwrap();
        let back: NamingStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, style);
    }
}

// -------------------------------------------------- capabilities default

#[test]
fn capabilities_default_is_all_false() {
    let caps = ModelCapabilities::default();
    assert!(!caps.vision);
    assert!(!caps.tools);
    assert!(!caps.thinking);
    assert!(!caps.structured_output);
    assert!(!caps.audio);
    assert!(!caps.video);
    assert!(!caps.files);
}

#[test]
fn capabilities_serde_round_trip() {
    let caps = ModelCapabilities {
        vision: true,
        tools: true,
        thinking: false,
        structured_output: true,
        audio: false,
        video: false,
        files: true,
    };
    let json = serde_json::to_string(&caps).unwrap();
    let back: ModelCapabilities = serde_json::from_str(&json).unwrap();
    assert_eq!(back, caps);
}

// ------------------------------------------------- model entry with tier

#[test]
fn model_entry_tier_serde_round_trip() {
    let entry = zroutery_core::config::ModelEntry::for_upstream(
        "deepseek",
        "deepseek-chat",
        Some(ModelTier::Standard),
    );
    let json = serde_json::to_string(&entry).unwrap();
    let back: zroutery_core::config::ModelEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.tier, Some(ModelTier::Standard));
}

#[test]
fn model_entry_legacy_class_field_deserializes_to_tier() {
    // Simulates reading an old config that uses "class" instead of "tier".
    let json = json!({
        "provider_id": "p",
        "upstream_model": "m",
        "class": "opus",
        "enabled": true,
    });
    let entry: zroutery_core::config::ModelEntry = serde_json::from_value(json).unwrap();
    assert_eq!(entry.tier, Some(ModelTier::Reasoning));
}

// ------------------------------------------------ fable model name mapping

#[test]
fn from_virtual_id_fable_class_maps_to_frontier() {
    assert_eq!(
        ModelTier::from_virtual_id("fable-class"),
        Some(ModelTier::Frontier)
    );
}

// ------------------------------------------------ naming style integration

/// The key contract: /v1/models only exposes the configured style's virtual
/// IDs, but the resolver accepts ALL styles. This test locks that distinction.
#[test]
fn naming_style_display_vs_resolution_contract() {
    // Internal style displays fast-class, standard-class, reasoning-class, frontier-class
    assert_eq!(ModelTier::Fast.virtual_id_styled(NamingStyle::Internal), "fast-class");
    assert_eq!(ModelTier::Standard.virtual_id_styled(NamingStyle::Internal), "standard-class");
    assert_eq!(ModelTier::Reasoning.virtual_id_styled(NamingStyle::Internal), "reasoning-class");
    assert_eq!(ModelTier::Frontier.virtual_id_styled(NamingStyle::Internal), "frontier-class");

    // Anthropic style displays haiku-class, sonnet-class, opus-class, fable-class
    assert_eq!(ModelTier::Fast.virtual_id_styled(NamingStyle::Anthropic), "haiku-class");
    assert_eq!(ModelTier::Standard.virtual_id_styled(NamingStyle::Anthropic), "sonnet-class");
    assert_eq!(ModelTier::Reasoning.virtual_id_styled(NamingStyle::Anthropic), "opus-class");
    assert_eq!(ModelTier::Frontier.virtual_id_styled(NamingStyle::Anthropic), "fable-class");

    // OpenAI style displays luna-class, terra-class, sol-class, astra-class
    assert_eq!(ModelTier::Fast.virtual_id_styled(NamingStyle::OpenAI), "luna-class");
    assert_eq!(ModelTier::Standard.virtual_id_styled(NamingStyle::OpenAI), "terra-class");
    assert_eq!(ModelTier::Reasoning.virtual_id_styled(NamingStyle::OpenAI), "sol-class");
    assert_eq!(ModelTier::Frontier.virtual_id_styled(NamingStyle::OpenAI), "astra-class");

    // But ALL of these resolve to the same tier, regardless of active style.
    // This is the backward-compat contract.
    assert_eq!(ModelTier::from_virtual_id("reasoning-class"), Some(ModelTier::Reasoning));
    assert_eq!(ModelTier::from_virtual_id("opus-class"), Some(ModelTier::Reasoning));
    assert_eq!(ModelTier::from_virtual_id("sol-class"), Some(ModelTier::Reasoning));

    assert_eq!(ModelTier::from_virtual_id("frontier-class"), Some(ModelTier::Frontier));
    assert_eq!(ModelTier::from_virtual_id("fable-class"), Some(ModelTier::Frontier));
    assert_eq!(ModelTier::from_virtual_id("astra-class"), Some(ModelTier::Frontier));
}

/// display_name follows the same style contract.
#[test]
fn display_name_matches_style() {
    assert_eq!(ModelTier::Frontier.display_name(NamingStyle::Internal), "Frontier");
    assert_eq!(ModelTier::Frontier.display_name(NamingStyle::Anthropic), "Fable");
    assert_eq!(ModelTier::Frontier.display_name(NamingStyle::OpenAI), "Astra");
}
