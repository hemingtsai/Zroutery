//! Validating drafts and writing them into the Zroutery config.

use serde::{Deserialize, Serialize};

use zroutery_core::config::{ModelEntry, ProviderConfig, ProviderKind};

use super::model::{CcProvider, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS};

/// A Zroutery id that is unique among `taken` and still recognisable.
pub fn unique_provider_id(name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let base: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() {
        "provider".to_string()
    } else {
        base
    };

    let mut candidate = base.clone();
    let mut n = 2;
    while taken(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

/// Turn a draft into the pieces Zroutery stores: a provider plus its models,
/// with the API key kept apart for the credential store.
///
/// `provider_id` is decided by the caller, who knows the existing
/// configuration and can guarantee uniqueness. The models take `priority`
/// as their class priority — the current CC Switch provider lands at 0 so it
/// is the primary after import, the rest follow in CC Switch's own order.
pub fn to_zroutery(
    draft: &CcProvider,
    provider_id: String,
    priority: i32,
    timeout_ms: Option<u64>,
) -> (ProviderConfig, Vec<ModelEntry>) {
    let mut provider =
        ProviderConfig::new(provider_id, draft.name.clone(), ProviderKind::Anthropic);
    provider.base_url = draft.base_url.trim_end_matches('/').to_string();
    // Claude Code relays expect the client fingerprint; that is Zroutery's
    // default for Anthropic providers, and an import that silently disabled
    // it would break strict gateways. Stated as the client profile rather than
    // through the deprecated `impersonate_claude_code` switch, which is what the
    // serving path reads now.
    provider.client_profile = zroutery_core::config::ProviderClientProfile::ClaudeCode;
    // CC Switch's API_TIMEOUT_MS is milliseconds against our seconds. Values
    // outside the accepted range never reach here through the reader; a caller
    // that supplies one directly is still held to the same bounds.
    provider.timeout_secs = timeout_ms
        .map(|ms| (ms / 1000).clamp(MIN_TIMEOUT_MS / 1000, MAX_TIMEOUT_MS / 1000))
        .unwrap_or(600);

    let models = draft
        .models
        .iter()
        .map(|m| {
            let mut entry = ModelEntry::for_upstream(&provider.id, &m.upstream_model, m.tier);
            entry.priority = priority;
            entry
        })
        .collect();
    (provider, models)
}

/// The result of a CC Switch import, summarising what happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    /// Where the providers were read from (e.g. `~/.cc-switch/cc-switch.db`).
    pub source: String,
    /// How many providers made it into the config.
    pub providers_imported: usize,
    /// How many model entries were created across all providers.
    pub models_imported: usize,
    /// Non-fatal observations (e.g. a model has no tier assigned).
    pub warnings: Vec<String>,
    /// Fatal problems that prevented an entry from being imported.
    pub errors: Vec<String>,
}

/// Validate a draft before conversion.
///
/// Returns `Ok(())` if the draft is clean, or a descriptive error string when
/// it would produce an unusable provider or model entry. These checks run
/// before [`to_zroutery`], so the caller can skip bad entries and still import
/// the good ones.
pub fn validate_draft(draft: &CcProvider) -> Result<(), String> {
    if draft.base_url.trim().is_empty() {
        return Err(format!("provider `{}` has no base url", draft.name,));
    }
    // A provider with no models is not necessarily invalid (the user might
    // add models later), but it is unusual enough to warn about rather than
    // silently import.
    if draft.models.is_empty() {
        return Err(format!("provider `{}` has no model entries", draft.name,));
    }
    for m in &draft.models {
        if m.upstream_model.trim().is_empty() {
            return Err(format!(
                "provider `{}` has a model entry with no upstream model name",
                draft.name,
            ));
        }
    }
    Ok(())
}

/// Run post-import validation on the config slice the import would add.
///
/// Checks for issues that [`validate_draft`] cannot catch in isolation, such
/// as duplicate provider ids and model id collisions within the import batch.
/// Returns a list of warnings and errors; errors mean the corresponding entry
/// should not be committed.
pub fn validate_import_batch(
    providers: &[(ProviderConfig, Vec<ModelEntry>)],
) -> (Vec<String>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let mut seen_provider_ids = std::collections::BTreeMap::<String, usize>::new();
    let mut seen_model_ids = std::collections::BTreeMap::<String, usize>::new();

    for (provider, models) in providers {
        *seen_provider_ids.entry(provider.id.clone()).or_insert(0) += 1;

        if provider.base_url.trim().is_empty() {
            errors.push(format!("provider `{}` has an empty base url", provider.id,));
        }

        for m in models {
            let exposed = m.exposed_id();
            *seen_model_ids.entry(exposed.clone()).or_insert(0) += 1;

            if m.upstream_model.trim().is_empty() {
                errors.push(format!(
                    "model on provider `{}` has no upstream name",
                    provider.id,
                ));
            }
            if m.tier.is_none() {
                warnings.push(format!(
                    "model `{exposed}` has no tier assigned; it will not participate in *-class routing",
                ));
            }
        }
    }

    for (id, count) in &seen_provider_ids {
        if *count > 1 {
            errors.push(format!(
                "provider id `{id}` appears {count} times in the import batch",
            ));
        }
    }
    for (id, count) in &seen_model_ids {
        if *count > 1 {
            errors.push(format!(
                "model id `{id}` appears {count} times in the import batch",
            ));
        }
    }

    (warnings, errors)
}

/// Produce a report for a completed import.
///
/// This is a convenience constructor: pass the source path, the converted
/// pairs, and any additional warnings the caller collected (e.g. from
/// [`validate_import_batch`]).
pub fn build_report(
    source: &str,
    imported: &[(ProviderConfig, Vec<ModelEntry>)],
    extra_warnings: Vec<String>,
    extra_errors: Vec<String>,
) -> ImportReport {
    let providers_imported = imported.len();
    let models_imported: usize = imported.iter().map(|(_, m)| m.len()).sum();
    ImportReport {
        source: source.to_string(),
        providers_imported,
        models_imported,
        warnings: extra_warnings,
        errors: extra_errors,
    }
}
