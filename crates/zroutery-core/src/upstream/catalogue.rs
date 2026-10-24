//! One entry of a provider model catalogue, and the shapes catalogues arrive in.

use serde_json::Value;

use crate::billing::Pricing;
/// One entry of a provider's model catalogue.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    /// Present only when the catalogue publishes prices.
    pub pricing: Option<Pricing>,
    /// Context window, when the source publishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Maximum output tokens, when the source publishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    /// Capability flags, when the source publishes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::config::ModelCapabilities>,
    /// The reasoning-effort levels the model declares, in picker order.
    ///
    /// Empty when the catalogue describes no effort levels; a client picker is
    /// only written for a non-empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<String>,
    /// Whether the model is served on a zero-cost (free) tier.
    #[serde(default)]
    pub free: bool,
}

/// Read per-token prices out of a catalogue entry.
///
/// The shape is OpenRouter's: strings of dollars per single token, which become
/// dollars per million. Anything else is left for the user to type.
pub(super) fn pricing_from_catalogue(entry: &Value) -> Option<Pricing> {
    let pricing = entry.get("pricing")?;
    let per_mtok = |key: &str| -> Option<f64> {
        let raw = pricing.get(key)?;
        let value = raw
            .as_f64()
            .or_else(|| raw.as_str()?.trim().parse::<f64>().ok())?;
        (value.is_finite() && value >= 0.0).then_some(value * 1_000_000.0)
    };

    let input = per_mtok("prompt").or_else(|| per_mtok("input"))?;
    let output = per_mtok("completion").or_else(|| per_mtok("output"))?;
    if input == 0.0 && output == 0.0 {
        // A free model, or a catalogue that fills the fields with zeros.
        return None;
    }
    Some(Pricing {
        currency: "USD".into(),
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: per_mtok("input_cache_read"),
        cache_write_per_mtok: per_mtok("input_cache_write"),
    })
}

/// Extract model entries from a provider's catalogue response.
///
/// The OpenAI standard places models in a `"data"` array, with each entry
/// carrying an `"id"` field. Some providers use `"models"` instead of `"data"`,
/// and/or `"model"` / `"model_id"` / `"name"` instead of `"id"`. When the
/// standard layout fails, the function tries these fallbacks and logs a
/// diagnostic so an empty result does not go unnoticed.
pub(super) fn extract_model_list(json: &Value, provider_name: &str) -> Vec<DiscoveredModel> {
    // Try standard OpenAI format first: `{"data": [{"id": "..."}]}`.
    if let Some(items) = json.get("data").and_then(Value::as_array) {
        let models: Vec<DiscoveredModel> = items
            .iter()
            .filter_map(|m| {
                let id = model_id_from_entry(m)?;
                Some(DiscoveredModel {
                    id,
                    pricing: pricing_from_catalogue(m),
                    ..Default::default()
                })
            })
            .collect();
        if !models.is_empty() {
            return models;
        }
    }

    // Fallback: some providers (including MiMo) use `"models"` instead of `"data"`.
    for key in &["models", "items", "entries"] {
        if let Some(items) = json.get(*key).and_then(Value::as_array) {
            let models: Vec<DiscoveredModel> = items
                .iter()
                .filter_map(|m| {
                    let id = model_id_from_entry(m)?;
                    Some(DiscoveredModel {
                        id,
                        pricing: pricing_from_catalogue(m),
                        ..Default::default()
                    })
                })
                .collect();
            if !models.is_empty() {
                tracing::debug!(
                    provider = provider_name,
                    key,
                    count = models.len(),
                    "model list uses non-standard array key"
                );
                return models;
            }
        }
    }

    // Fallback: bare top-level array.
    if let Some(items) = json.as_array() {
        let models: Vec<DiscoveredModel> = items
            .iter()
            .filter_map(|m| {
                let id = model_id_from_entry(m)?;
                Some(DiscoveredModel {
                    id,
                    pricing: pricing_from_catalogue(m),
                    ..Default::default()
                })
            })
            .collect();
        if !models.is_empty() {
            tracing::debug!(
                provider = provider_name,
                count = models.len(),
                "model list is a bare array"
            );
            return models;
        }
    }

    // Nothing matched.
    if json.is_object() {
        tracing::warn!(
            provider = provider_name,
            top_keys = ?json.as_object().map(|o| o.keys().collect::<Vec<_>>()),
            "model catalogue response has no recognizable model array; \
             discovery returned 0 models"
        );
    }
    Vec::new()
}

/// Extract a model id string from one catalogue entry.
///
/// Tries `"id"`, `"model"`, `"model_id"`, and `"name"`, in order.
fn model_id_from_entry(entry: &Value) -> Option<String> {
    for key in &["id", "model", "model_id", "name"] {
        if let Some(id) = entry.get(*key).and_then(Value::as_str) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}
