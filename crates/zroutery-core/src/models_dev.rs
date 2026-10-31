//! The public models.dev catalogue: capabilities, limits and effort levels.
//!
//! A provider's own `/models` route answers names and sometimes prices; it
//! almost never answers "can this model see images", "how large is its window"
//! or "which reasoning-effort levels does it accept". models.dev does, for
//! every model it knows, under a provider key. Discovery fills those fields
//! from it so an imported model is not a poorer entry than one typed by hand,
//! and so the takeover can publish the effort levels a client may choose.
//!
//! # Manual corrections win
//!
//! This module only shapes the *discovery* result. The candidate values are
//! copied into a [`crate::config::ModelEntry`] when the user imports a model;
//! after that the saved configuration is the authority, and a field the user
//! edited is never consulted here again. Re-fetching the catalogue cannot
//! overwrite a correction, because a configured model is not added twice.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::{ModelCapabilities, ProviderConfig, ProviderKind, EFFORT_LEVELS};
use crate::error::{Error, Result};
use crate::upstream::DiscoveredModel;

/// The canonical catalogue. `https://models.opencode.ai/api.json` serves the
/// same document; models.dev is the source of record.
pub const CATALOG_URL: &str = "https://models.dev/api.json";
/// How long a fetched catalogue is reused before it is fetched again.
const CATALOG_TTL: Duration = Duration::from_secs(3600);
/// The `provider` reported in errors from this module.
const PROVIDER_ID: &str = "models.dev";

/// One model's metadata, mapped from a catalogue entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatalogModel {
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    /// Served at zero cost on both input and output.
    pub free: bool,
    pub capabilities: ModelCapabilities,
    /// Declared effort levels, restricted to [`EFFORT_LEVELS`] and ordered by it.
    pub effort_levels: Vec<String>,
}

/// The parsed catalogue: provider id → model id → metadata.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    providers: HashMap<String, HashMap<String, CatalogModel>>,
}

static CATALOG_CACHE: Mutex<Option<(Instant, Catalog)>> = Mutex::new(None);

/// The catalogue, fetched once per [`CATALOG_TTL`] and cached in memory.
///
/// It is several megabytes, so it is never fetched per request; enriching a
/// discovered model list pays for it at most once an hour.
pub async fn catalog() -> Result<Catalog> {
    if let Some((at, cached)) = crate::sync::lock(&CATALOG_CACHE).as_ref() {
        if at.elapsed() < CATALOG_TTL {
            return Ok(cached.clone());
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::internal(format!("models.dev client: {e}")))?;
    let json: Value = client
        .get(CATALOG_URL)
        .send()
        .await
        .map_err(|source| Error::Transport {
            provider: PROVIDER_ID.into(),
            source,
        })?
        .json()
        .await
        .map_err(|e| Error::BadUpstreamPayload(format!("models.dev catalogue decode: {e}")))?;
    let parsed = parse(&json);
    *crate::sync::lock(&CATALOG_CACHE) = Some((Instant::now(), parsed.clone()));
    Ok(parsed)
}

/// Parse a whole catalogue document.
pub fn parse(json: &Value) -> Catalog {
    let Some(providers) = json.as_object() else {
        return Catalog::default();
    };
    Catalog {
        providers: providers
            .iter()
            .filter_map(|(provider_id, provider)| {
                let models = provider.get("models")?.as_object()?;
                let models = models
                    .iter()
                    .map(|(id, entry)| (id.clone(), catalog_model(entry)))
                    .collect();
                Some((provider_id.clone(), models))
            })
            .collect(),
    }
}

/// Parse one provider's model map out of a catalogue document.
pub fn parse_provider(json: &Value, provider: &str) -> HashMap<String, CatalogModel> {
    json.get(provider)
        .and_then(|provider| provider.get("models"))
        .and_then(Value::as_object)
        .map(|models| {
            models
                .iter()
                .map(|(id, entry)| (id.clone(), catalog_model(entry)))
                .collect()
        })
        .unwrap_or_default()
}

/// Map one catalogue entry's capability, limit and effort fields.
pub fn catalog_model(entry: &Value) -> CatalogModel {
    let limit = entry.get("limit");
    let limit_field = |name: &str| -> Option<u32> {
        limit
            .and_then(|limit| limit.get(name))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    let input_modalities = entry
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array);
    let has_input = |name: &str| {
        input_modalities.is_some_and(|list| list.iter().any(|value| value.as_str() == Some(name)))
    };
    let flag = |name: &str| entry.get(name).and_then(Value::as_bool).unwrap_or(false);
    let cost = entry.get("cost");
    let zero_cost =
        |name: &str| cost.and_then(|cost| cost.get(name)).and_then(Value::as_f64) == Some(0.0);

    CatalogModel {
        context_window: limit_field("context"),
        max_output: limit_field("output"),
        free: zero_cost("input") && zero_cost("output"),
        capabilities: ModelCapabilities {
            vision: has_input("image"),
            video: has_input("video"),
            audio: has_input("audio"),
            files: flag("attachment"),
            tools: flag("tool_call"),
            thinking: flag("reasoning"),
            structured_output: flag("structured_output"),
        },
        effort_levels: effort_levels(entry),
    }
}

/// The effort levels an entry declares, in picker order.
///
/// models.dev spells them inside `reasoning_options`; a model may offer a
/// toggle only, a fixed list, or both. Only effort options contribute, and only
/// values the gateway can encode survive.
fn effort_levels(entry: &Value) -> Vec<String> {
    if !entry
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let Some(options) = entry.get("reasoning_options").and_then(Value::as_array) else {
        return Vec::new();
    };
    let declared: Vec<&str> = options
        .iter()
        .filter(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
        .filter_map(|option| option.get("values").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    EFFORT_LEVELS
        .iter()
        .filter(|level| {
            declared
                .iter()
                .any(|value| value.eq_ignore_ascii_case(level))
        })
        .map(|level| (*level).to_string())
        .collect()
}

impl Catalog {
    /// One provider's model map, when the catalogue knows the provider.
    pub fn provider(&self, provider_id: &str) -> Option<&HashMap<String, CatalogModel>> {
        self.providers.get(provider_id)
    }

    /// Find the metadata for a configured provider's model.
    ///
    /// Relays rarely share models.dev's canonical provider ids — `workbuddy`
    /// and `docode` do not exist there — so the search widens in a fixed order:
    ///
    /// 1. the provider's own id,
    /// 2. the id its kind implies (the OpenCode family is `opencode`),
    /// 3. the model id across every provider, after stripping a relay's
    ///    `vendor/` or `zone:` prefix.
    ///
    /// A cross-provider hit set is merged conservatively: a capability is only
    /// claimed when every match claims it, and a limit is only published when
    /// every match publishes one. Two relays that disagree about a model are
    /// not evidence for the more generous answer. Cross-provider matches never
    /// supply effort levels: they do not prove this provider/API surface's set.
    pub fn lookup(
        &self,
        provider_id: &str,
        kind_hint: Option<&str>,
        model_id: &str,
    ) -> Option<CatalogModel> {
        for key in std::iter::once(provider_id).chain(kind_hint) {
            if let Some(model) = self
                .providers
                .get(key)
                .and_then(|models| models.get(model_id))
            {
                return Some(model.clone());
            }
        }

        let normalized = normalize_model_id(model_id);
        let mut matches: Vec<&CatalogModel> = Vec::new();
        for models in self.providers.values() {
            if let Some(model) = models.get(model_id) {
                matches.push(model);
            } else if normalized != model_id {
                if let Some(model) = models.get(normalized) {
                    matches.push(model);
                }
            }
        }
        match matches.as_slice() {
            [] => None,
            [only] => {
                let mut model = (*only).clone();
                model.effort_levels.clear();
                Some(model)
            }
            many => Some(merge(many)),
        }
    }
}

/// The models.dev provider a configured provider's kind implies, when any.
fn kind_hint(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::OpenCodeZenFree | ProviderKind::OpenCodeConsole => Some("opencode"),
        ProviderKind::Anthropic => Some("anthropic"),
        // Generic relays have no canonical id; their model ids are the only
        // thing that can match, so they get no provider hint.
        ProviderKind::OpenAICompatible | ProviderKind::Decision => None,
    }
}

/// A relay id without its `vendor/` or `zone:` prefix.
fn normalize_model_id(id: &str) -> &str {
    id.rsplit(['/', ':']).next().unwrap_or(id)
}

/// Merge disagreeing cross-provider matches into the conservative answer.
fn merge(entries: &[&CatalogModel]) -> CatalogModel {
    let first = entries[0];
    let mut capabilities = first.capabilities.clone();
    // Cross-relay matches do not prove this provider/model/surface effort set.
    let effort_levels = Vec::new();
    let mut free = first.free;
    for entry in &entries[1..] {
        capabilities.vision &= entry.capabilities.vision;
        capabilities.tools &= entry.capabilities.tools;
        capabilities.thinking &= entry.capabilities.thinking;
        capabilities.structured_output &= entry.capabilities.structured_output;
        capabilities.audio &= entry.capabilities.audio;
        capabilities.video &= entry.capabilities.video;
        capabilities.files &= entry.capabilities.files;
        free &= entry.free;
    }
    CatalogModel {
        // An unknown window on any match makes the merged one unknown: the
        // smaller known value would overstate the match that declared nothing.
        context_window: entries
            .iter()
            .all(|entry| entry.context_window.is_some())
            .then(|| {
                entries
                    .iter()
                    .filter_map(|entry| entry.context_window)
                    .min()
            })
            .flatten(),
        max_output: entries
            .iter()
            .all(|entry| entry.max_output.is_some())
            .then(|| entries.iter().filter_map(|entry| entry.max_output).min())
            .flatten(),
        free,
        capabilities,
        effort_levels,
    }
}

/// Fill discovered models from the catalogue.
///
/// Ids the catalogue does not describe are left untouched. The provider's own
/// `/models` response wins where it already spoke: prices are never replaced,
/// and `free` is only ever raised, because a provider that says a model is
/// free is better evidence than a catalogue's default.
pub async fn enrich(models: &mut [DiscoveredModel], provider: &ProviderConfig) -> Result<()> {
    let catalog = catalog().await?;
    enrich_from(&catalog, models, &provider.id, kind_hint(provider.kind));
    Ok(())
}

/// Enrichment against an already-fetched catalogue, split out for tests.
pub fn enrich_from(
    catalog: &Catalog,
    models: &mut [DiscoveredModel],
    provider_id: &str,
    kind_hint: Option<&str>,
) {
    for model in models {
        if let Some(entry) = catalog.lookup(provider_id, kind_hint, &model.id) {
            model.context_window = entry.context_window;
            model.max_output = entry.max_output;
            model.free |= entry.free;
            model.capabilities = Some(entry.capabilities.clone());
            model.effort_levels = entry.effort_levels.clone();
        }
    }
}

/// One provider's map from the shared catalogue, for callers that only need a
/// single section (the OpenCode family).
pub async fn provider_catalog(provider_id: &str) -> Result<HashMap<String, CatalogModel>> {
    Ok(catalog()
        .await?
        .provider(provider_id)
        .cloned()
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalogue() -> Catalog {
        parse(&json!({
            "opencode": {
                "models": {
                    "space-bunny-free": {
                        "reasoning": true,
                        "reasoning_options": [
                            {"type": "effort", "values": ["low", "medium", "high", "xhigh", "max"]}
                        ],
                        "tool_call": true,
                        "attachment": true,
                        "modalities": {"input": ["text", "image"], "output": ["text"]},
                        "limit": {"context": 1048576, "output": 524288},
                        "cost": {"input": 0, "output": 0}
                    },
                    "big-pickle": {
                        "reasoning": true,
                        "reasoning_options": [],
                        "tool_call": true,
                        "limit": {"context": 200000, "output": 32000}
                    }
                }
            },
            "zhipuai": {
                "models": {
                    "glm-5.3": {
                        "reasoning": true,
                        "reasoning_options": [{"type": "effort", "values": ["low", "high", "max"]}],
                        "tool_call": true,
                        "limit": {"context": 200000, "output": 32000}
                    }
                }
            }
        }))
    }

    #[test]
    fn an_exact_provider_match_wins() {
        let catalog = catalogue();
        let found = catalog
            .lookup("opencode", None, "space-bunny-free")
            .expect("opencode knows its own model");
        assert!(found.free);
        assert!(found.capabilities.vision);
        assert_eq!(
            found.effort_levels,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn a_kind_hint_finds_the_opencode_family() {
        let catalog = catalogue();
        let found = catalog
            .lookup("opencode-zen", Some("opencode"), "big-pickle")
            .expect("the kind hint reaches the opencode section");
        assert!(found.capabilities.tools);
        assert_eq!(found.context_window, Some(200_000));
        // A toggle-only model declares no effort levels, so no picker is
        // written for it.
        assert!(found.effort_levels.is_empty());
    }

    #[test]
    fn a_relay_model_id_matches_across_providers() {
        let catalog = catalogue();
        let found = catalog
            .lookup("workbuddy", None, "glm-5.3")
            .expect("the model id matches zhipuai");
        assert!(
            found.effort_levels.is_empty(),
            "another relay's vocabulary is not this provider's contract"
        );
        // A namespaced relay id is normalized before the global search.
        assert!(catalog.lookup("docode", None, "cn:glm-5.3").is_some());
    }

    #[test]
    fn cross_provider_matches_are_merged_conservatively() {
        let catalog = parse(&json!({
            "a": {"models": {"m": {
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low", "high"]}],
                "tool_call": true,
                "limit": {"context": 100000, "output": 1000}
            }}},
            "b": {"models": {"m": {
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["high", "max"]}],
                "tool_call": false,
                "limit": {"context": 200000}
            }}}
        }));
        let found = catalog.lookup("relay", None, "m").expect("merged match");
        assert!(!found.capabilities.tools, "one provider denies tool calls");
        assert_eq!(
            found.effort_levels,
            Vec::<String>::new(),
            "cross-provider union cannot advertise native reasoning controls"
        );
        assert_eq!(
            found.context_window,
            Some(100_000),
            "the smallest window every match declares"
        );
        assert_eq!(
            found.max_output, None,
            "an undeclared output cap makes the merged cap unknown"
        );
    }

    #[test]
    fn enrichment_fills_the_discovery_result() {
        let catalog = catalogue();
        let mut models = vec![DiscoveredModel {
            id: "glm-5.3".into(),
            pricing: None,
            context_window: None,
            max_output: None,
            capabilities: None,
            effort_levels: Vec::new(),
            free: false,
        }];
        enrich_from(&catalog, &mut models, "workbuddy", None);
        assert_eq!(models[0].context_window, Some(200_000));
        assert!(
            models[0].effort_levels.is_empty(),
            "relay effort remains unknown without exact evidence"
        );
        assert!(models[0]
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.thinking));
    }

    #[test]
    fn an_unknown_model_is_left_untouched() {
        let catalog = catalogue();
        let mut models = vec![DiscoveredModel {
            id: "nothing-like-it".into(),
            pricing: None,
            context_window: Some(4096),
            max_output: None,
            capabilities: None,
            effort_levels: vec!["high".into()],
            free: false,
        }];
        enrich_from(&catalog, &mut models, "relay", None);
        assert_eq!(models[0].context_window, Some(4096));
        assert_eq!(models[0].effort_levels, vec!["high"]);
    }
}
