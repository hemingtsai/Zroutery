//! The shapes on both sides of an import.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zroutery_core::config::ModelTier;
use zroutery_core::query::strip_client_model_modifier;

/// One provider as CC Switch knows it, reduced to what Zroutery needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcProvider {
    /// CC Switch's own provider id, kept so a re-import can be recognised.
    pub source_id: String,
    pub name: String,
    pub base_url: String,
    /// The auth token, deliberately skipped during serialization so it is
    /// never sent to the frontend. The dashboard must not render it; the
    /// import stores it in the OS credential store directly.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    /// The tier defaults, `[1M]`/`[1m]` modifiers already stripped.
    pub models: Vec<CcModel>,
    /// The provider CC Switch currently has active.
    pub is_current: bool,
    /// `API_TIMEOUT_MS` as CC Switch wrote it. Absent or unusable means the
    /// import has no timeout opinion; see [`parse_timeout_ms`].
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcModel {
    pub upstream_model: String,
    pub tier: Option<ModelTier>,
}

/// The identity of one account behind a CC Switch entry.
///
/// A base URL alone is not an account: one relay commonly holds several
/// subscriptions with their own keys, quotas and model sets. The entry's own id
/// is the account marker CC Switch persists for it, and the target id carries
/// that marker, so a later import of the same entry recognises the provider it
/// created. The credential fingerprint is a second signal for a provider that
/// was imported before the marker existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAccount {
    pub source_id: String,
    pub name: String,
    pub base_url: String,
    /// A digest of the account's credential, when the entry carries one. The
    /// key itself never leaves the credential store.
    pub credential_fingerprint: Option<String>,
}

/// What the import preview shows for one CC Switch provider: the draft plus
/// what would happen to it. The API key is deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcProviderDraft {
    #[serde(flatten)]
    pub provider: CcProvider,
    /// The Zroutery provider id the import would write, or the id of the
    /// existing provider this entry was recognised as.
    pub target_id: String,
    /// This entry is already configured, so the import skips it; the dashboard
    /// disables the row. Decided by [`plan_import`], the same rule the import
    /// command applies.
    pub already_imported: bool,
}

/// The range a configured timeout has to land in, in milliseconds.
pub(super) const MIN_TIMEOUT_MS: u64 = 60_000;
pub(super) const MAX_TIMEOUT_MS: u64 = 3_600_000;

/// The tier defaults become class members; the general `ANTHROPIC_MODEL` and
/// the subagent model join as unclassified entries when they differ.
///
/// When every tier names the same model — the usual single-model relay — the
/// entry is classified sonnet: a conversation asks for `sonnet-class` far
/// more than anything else, and the import must leave that class usable.
pub(super) fn collect_models(env: &Value) -> Vec<CcModel> {
    let model_of = |key: &str| -> Option<String> {
        env.get(key)
            .and_then(Value::as_str)
            .map(strip_client_model_modifier)
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "default")
            .map(str::to_string)
    };

    let opus = model_of("ANTHROPIC_DEFAULT_OPUS_MODEL");
    let sonnet = model_of("ANTHROPIC_DEFAULT_SONNET_MODEL");
    let haiku = model_of("ANTHROPIC_DEFAULT_HAIKU_MODEL");

    // The shared model itself, not a boolean: when only two tiers are set there
    // is no sonnet entry to read, and doing so anyway made a legal partial
    // mapping panic the whole provider import.
    let shared_model = match (&opus, &sonnet, &haiku) {
        (Some(o), Some(s), Some(h)) if o == s && s == h => Some(s.clone()),
        (Some(o), Some(s), None) if o == s => Some(s.clone()),
        (Some(o), None, Some(h)) if o == h => Some(o.clone()),
        (None, Some(s), Some(h)) if s == h => Some(s.clone()),
        _ => None,
    };

    let mut models: Vec<CcModel> = Vec::new();
    if let Some(upstream_model) = shared_model {
        // Every tier names the same model: one entry, in the tier a
        // conversation actually requests.
        models.push(CcModel {
            upstream_model,
            tier: Some(ModelTier::Standard),
        });
    } else {
        for (model, tier) in [
            (opus, ModelTier::Reasoning),
            (sonnet, ModelTier::Standard),
            (haiku, ModelTier::Fast),
        ] {
            if let Some(upstream_model) = model {
                if !models
                    .iter()
                    .any(|m: &CcModel| m.upstream_model == upstream_model)
                {
                    models.push(CcModel {
                        upstream_model,
                        tier: Some(tier),
                    });
                }
            }
        }
    }

    // The models Claude Code can ask for by name beyond the tiers: the main
    // model override and the subagent model. They stay callable by exact id.
    for key in ["ANTHROPIC_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"] {
        if let Some(upstream_model) = model_of(key) {
            if !models
                .iter()
                .any(|m: &CcModel| m.upstream_model == upstream_model)
            {
                models.push(CcModel {
                    upstream_model,
                    tier: None,
                });
            }
        }
    }
    models
}
