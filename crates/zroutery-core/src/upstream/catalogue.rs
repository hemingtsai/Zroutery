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
/// Two shapes arrive, and telling them apart is the whole job:
///
/// * **OpenRouter's** — strings of dollars per *single* token (`"0.0000015"`),
///   which become dollars per million by scaling. A per-token price is always
///   far below 1, so the scale is unambiguous.
/// * **NewAPI-style panels'** — the same field names borrowed, but the values
///   are the panel's own per-*million* price in whatever currency the panel
///   prices in (a domestic relay publishes `prompt: "1"`, `completion: "4"` for
///   a ¥1/¥4 model). Scaling *that* by a million is how the models page came to
///   offer a DeepSeek flash model at 1,000,000 USD — the panel's number was
///   already the price, and the currency was never dollars.
///
/// So values below 1 are scaled as per-token dollars and priced in USD; values
/// of 1 and above are already per-million and are taken as published, but the
/// currency is recorded as unknown (`?`) rather than guessed: a panel prices in
/// its own money and the catalogue does not say which. A figure past any
/// plausible per-million price is treated as garbage and dropped — a number the
/// operator would have to re-type anyway is better than one that silently
/// feeds a wrong figure into the spend estimate and the election.
///
/// # When the catalogue says what it means
///
/// A panel that states its own unit and currency is taken at its word, because
/// a stated unit is better evidence than any magnitude heuristic. A Chinese
/// relay publishes `"unit": "per_1m_tokens"` beside `"currency": "CNY"` and
/// prices of `1` / `4`: the shape heuristic reads `1` as per-million, but the
/// *label* it lost was the currency, so the figure could not be summed against
/// a budget in any named currency and every panel-priced model landed in the
/// `?` bucket. The same relay publishes `effective_pricing` beside `pricing`,
/// and the effective one is the one the panel charges by — a discounted model
/// has two price pairs and only one of them is what the customer pays.
///
/// So: `effective_pricing` is preferred, a stated currency is kept, a stated
/// `per_1m_tokens` unit settles the scale without a guess, and the cache price
/// is read under either of its two names.
pub(super) fn pricing_from_catalogue(entry: &Value) -> Option<Pricing> {
    // The two shapes do not overlap, so one line separates them. A per-token
    // dollar price above 0.01 would be $10,000 a million tokens — no model
    // costs that. A per-million price below 0.01 would be ¥0.01 a million —
    // no panel prices that. The cache fields ride the same scale as the pair
    // they belong to; a pair that straddles the line is not a shape the
    // catalogue can name, and is dropped for the operator to type instead.
    const PER_TOKEN_CEILING: f64 = 0.01;
    // The largest per-million price any real model carries. The most expensive
    // frontier model is under $100 in, under $200 out; four digits of headroom
    // catch the ×1e6 slips and the ratio fields pasted where prices belong.
    const PLAUSIBLE_PER_MTOK: f64 = 10_000.0;

    let pricing = entry
        .get("effective_pricing")
        .filter(|value| value.is_object())
        .or_else(|| entry.get("pricing"))?;
    let raw_value = |key: &str| -> Option<f64> {
        let raw = pricing.get(key)?;
        let value = raw
            .as_f64()
            .or_else(|| raw.as_str()?.trim().parse::<f64>().ok())?;
        value.is_finite().then_some(value)
    };
    // A currency the catalogue states, when it states one.
    //
    // Some catalogues publish their own currency (`"CNY"`) beside prices that
    // are already per million tokens. Ignoring it made a ¥1/¥4 panel look like
    // an unpriced-by-currency one, which then could not be summed against a
    // budget in any named currency — the figure was real, the label was lost.
    let stated_currency = |value: &Value| -> Option<String> {
        let text = value.as_str()?.trim();
        (!text.is_empty() && text != "?").then(|| text.to_uppercase())
    };
    // Whether the catalogue says the numbers are already per million tokens.
    //
    // This is stronger evidence than any magnitude heuristic, so it is read
    // before the shapes are guessed: a panel that says `per_1m_tokens` is
    // taken at its word for values in either direction.
    let stated_per_million = pricing
        .get("unit")
        .and_then(Value::as_str)
        .map(|unit| unit.trim().eq_ignore_ascii_case("per_1m_tokens"))
        .unwrap_or(false);

    let input = raw_value("prompt").or_else(|| raw_value("input"))?;
    let output = raw_value("completion").or_else(|| raw_value("output"))?;
    if input < 0.0 || output < 0.0 {
        return None;
    }
    if input == 0.0 && output == 0.0 {
        // A free model, or a catalogue that fills the fields with zeros.
        return None;
    }
    if input.max(output) > PLAUSIBLE_PER_MTOK {
        return None;
    }
    let mixed_shape =
        !stated_per_million && (input < PER_TOKEN_CEILING) != (output < PER_TOKEN_CEILING);
    if mixed_shape {
        // Half of one shape, half of the other: no unit names this pair.
        return None;
    }

    let per_token = !stated_per_million && input < PER_TOKEN_CEILING;
    let currency = pricing
        .get("currency")
        .and_then(stated_currency)
        .or_else(|| entry.get("currency").and_then(stated_currency))
        .unwrap_or_else(|| {
            if per_token {
                "USD".to_string()
            } else {
                "?".to_string()
            }
        });
    let scale = |value: f64| {
        if per_token {
            value * 1_000_000.0
        } else {
            value
        }
    };
    // Cache prices arrive under either the OpenRouter names or the shorter
    // panel names (`cache_read`), and both mean the same thing.
    let cache_read = raw_value("input_cache_read").or_else(|| raw_value("cache_read"));
    let cache_write = raw_value("input_cache_write").or_else(|| raw_value("cache_write"));
    if cache_read.is_some_and(|v| v < 0.0) || cache_write.is_some_and(|v| v < 0.0) {
        return None;
    }
    Some(Pricing {
        currency,
        input_per_mtok: scale(input),
        output_per_mtok: scale(output),
        cache_read_per_mtok: cache_read.map(scale),
        cache_write_per_mtok: cache_write.map(scale),
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
