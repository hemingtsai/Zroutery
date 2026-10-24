//! What a model is: its tier, its naming style, and the capabilities it declares.

use serde::{Deserialize, Serialize};

use crate::ir::CapabilityState;

/// The reasoning-effort levels the gateway accepts, in the order a model picker
/// should present them.
///
/// `none` and `minimal` are accepted on the wire (see the OpenAI decoders) but
/// are not selectable "effort" levels: they turn reasoning off rather than
/// choosing how much of it to spend.
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Restrict declared effort levels to the accepted vocabulary and put them in
/// [`EFFORT_LEVELS`] order.
///
/// A stored list may come from a catalogue, a hand edit, or an older version,
/// so it is normalized every time it is read rather than trusted. Unknown
/// values are dropped: they cannot be encoded to any dialect.
pub fn normalize_effort_levels(levels: &[String]) -> Vec<String> {
    EFFORT_LEVELS
        .iter()
        .filter(|level| levels.iter().any(|value| value.eq_ignore_ascii_case(level)))
        .map(|level| (*level).to_string())
        .collect()
}

/// Capability tier a model belongs to. Assigned manually by the user; Zroutery
/// never guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Cheapest and fastest — simple tasks.
    #[serde(alias = "haiku")]
    Fast,
    /// Balanced workhorse — general purpose.
    #[serde(alias = "sonnet")]
    Standard,
    /// Most capable reasoning — complex tasks.
    #[serde(alias = "opus")]
    Reasoning,
    /// Frontier — pushing the boundary of capability.
    Frontier,
}

impl ModelTier {
    pub const ALL: [ModelTier; 4] = [
        ModelTier::Fast,
        ModelTier::Standard,
        ModelTier::Reasoning,
        ModelTier::Frontier,
    ];

    /// The virtual model id exposed to clients, e.g. "standard-class".
    pub fn virtual_id(&self) -> &'static str {
        match self {
            ModelTier::Fast => "fast-class",
            ModelTier::Standard => "standard-class",
            ModelTier::Reasoning => "reasoning-class",
            ModelTier::Frontier => "frontier-class",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelTier::Fast => "fast",
            ModelTier::Standard => "standard",
            ModelTier::Reasoning => "reasoning",
            ModelTier::Frontier => "frontier",
        }
    }

    pub fn from_virtual_id(id: &str) -> Option<ModelTier> {
        match id {
            // Internal
            "fast-class" | "standard-class" | "reasoning-class" | "frontier-class" |
            // Anthropic
            "haiku-class" | "sonnet-class" | "opus-class" | "fable-class" |
            // OpenAI
            "luna-class" | "terra-class" | "sol-class" | "astra-class" => {
                // Parse by prefix to determine the tier.
                let lower = id.strip_suffix("-class").unwrap_or(id);
                if matches!(lower, "fast" | "haiku" | "luna") {
                    Some(ModelTier::Fast)
                } else if matches!(lower, "standard" | "sonnet" | "terra") {
                    Some(ModelTier::Standard)
                } else if matches!(lower, "reasoning" | "opus" | "sol") {
                    Some(ModelTier::Reasoning)
                } else if matches!(lower, "frontier" | "fable" | "astra") {
                    Some(ModelTier::Frontier)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Next tier up (for escalation). None if already at the top.
    pub fn higher(&self) -> Option<ModelTier> {
        match self {
            ModelTier::Fast => Some(ModelTier::Standard),
            ModelTier::Standard => Some(ModelTier::Reasoning),
            ModelTier::Reasoning => Some(ModelTier::Frontier),
            ModelTier::Frontier => None,
        }
    }

    /// Next tier down (for de-escalation). None if already at the bottom.
    pub fn lower(&self) -> Option<ModelTier> {
        match self {
            ModelTier::Fast => None,
            ModelTier::Standard => Some(ModelTier::Fast),
            ModelTier::Reasoning => Some(ModelTier::Standard),
            ModelTier::Frontier => Some(ModelTier::Reasoning),
        }
    }

    /// Display name for the given naming style.
    pub fn display_name(&self, style: NamingStyle) -> &'static str {
        match (self, style) {
            (ModelTier::Fast, NamingStyle::Internal) => "Fast",
            (ModelTier::Fast, NamingStyle::Anthropic) => "Haiku",
            (ModelTier::Fast, NamingStyle::OpenAI) => "Luna",
            (ModelTier::Standard, NamingStyle::Internal) => "Standard",
            (ModelTier::Standard, NamingStyle::Anthropic) => "Sonnet",
            (ModelTier::Standard, NamingStyle::OpenAI) => "Terra",
            (ModelTier::Reasoning, NamingStyle::Internal) => "Reasoning",
            (ModelTier::Reasoning, NamingStyle::Anthropic) => "Opus",
            (ModelTier::Reasoning, NamingStyle::OpenAI) => "Sol",
            (ModelTier::Frontier, NamingStyle::Internal) => "Frontier",
            (ModelTier::Frontier, NamingStyle::Anthropic) => "Fable",
            (ModelTier::Frontier, NamingStyle::OpenAI) => "Astra",
        }
    }

    /// Virtual model id for the given naming style, e.g. "haiku-class".
    pub fn virtual_id_styled(&self, style: NamingStyle) -> &'static str {
        match (self, style) {
            (ModelTier::Fast, NamingStyle::Internal) => "fast-class",
            (ModelTier::Fast, NamingStyle::Anthropic) => "haiku-class",
            (ModelTier::Fast, NamingStyle::OpenAI) => "luna-class",
            (ModelTier::Standard, NamingStyle::Internal) => "standard-class",
            (ModelTier::Standard, NamingStyle::Anthropic) => "sonnet-class",
            (ModelTier::Standard, NamingStyle::OpenAI) => "terra-class",
            (ModelTier::Reasoning, NamingStyle::Internal) => "reasoning-class",
            (ModelTier::Reasoning, NamingStyle::Anthropic) => "opus-class",
            (ModelTier::Reasoning, NamingStyle::OpenAI) => "sol-class",
            (ModelTier::Frontier, NamingStyle::Internal) => "frontier-class",
            (ModelTier::Frontier, NamingStyle::Anthropic) => "fable-class",
            (ModelTier::Frontier, NamingStyle::OpenAI) => "astra-class",
        }
    }
}

/// How tiers and virtual model ids are presented to clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamingStyle {
    /// Internal names: fast, standard, reasoning, frontier.
    #[default]
    Internal,
    /// Anthropic-style: Haiku, Sonnet, Opus, Fable.
    Anthropic,
    /// OpenAI-style: Luna, Terra, Sol, Astra.
    OpenAI,
}

/// Declared capabilities of a model. Independent of tier.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub thinking: bool,
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub video: bool,
    #[serde(default)]
    pub files: bool,
}

impl ModelCapabilities {
    /// Whether this model supports the given capability.
    pub fn supports(&self, cap: crate::ir::Capability) -> bool {
        match cap {
            crate::ir::Capability::Vision => self.vision,
            crate::ir::Capability::Tools => self.tools,
            crate::ir::Capability::Thinking => self.thinking,
            crate::ir::Capability::StructuredOutput => self.structured_output,
            crate::ir::Capability::Audio => self.audio,
            crate::ir::Capability::Video => self.video,
            crate::ir::Capability::Files => self.files,
        }
    }

    /// Tri-state capability check.
    ///
    /// With the current boolean fields this returns [`CapabilityState::Supported`]
    /// when the field is `true` and [`CapabilityState::Unknown`] when it is `false`.
    /// [`CapabilityState::Unsupported`] is not produced by the boolean config
    /// surface — it exists for future use when models can explicitly declare
    /// non-support.
    pub fn capability_state(&self, cap: crate::ir::Capability) -> CapabilityState {
        if self.supports(cap) {
            CapabilityState::Supported
        } else {
            CapabilityState::Unknown
        }
    }

    /// True if all capabilities are false (default/unset).
    pub fn is_empty(&self) -> bool {
        !self.vision
            && !self.tools
            && !self.thinking
            && !self.structured_output
            && !self.audio
            && !self.video
            && !self.files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_levels_are_normalized_to_the_accepted_vocabulary() {
        let stored = vec![
            "max".to_string(),
            "ultra".to_string(),
            "HIGH".to_string(),
            "low".to_string(),
            "high".to_string(),
        ];
        assert_eq!(
            normalize_effort_levels(&stored),
            vec!["low", "high", "max"],
            "unknown values are dropped, case is folded, and order is the picker's"
        );
    }
}
