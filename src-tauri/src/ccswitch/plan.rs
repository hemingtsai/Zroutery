//! Which entries become new providers, and under what id.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use zroutery_core::config::AppConfig;

use super::convert::unique_provider_id;
use super::model::{CcProvider, SourceAccount};

/// One CC Switch entry considered against a configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportDecision {
    /// Would be imported under this provider id.
    Planned(String),
    /// Already configured: `(provider id, why it is recognised)`.
    AlreadyImported(String, AlreadyImported),
}

/// Why an entry counts as already imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlreadyImported {
    /// The configured provider was created by this source entry.
    SameSource,
    /// No marker, but the provider reaches the same endpoint with the same
    /// credential: the same account, imported before the source was recorded.
    SameAccount,
}

/// A decision plus the entry it was made about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedImport {
    pub source_id: String,
    pub decision: ImportDecision,
}

/// What an import of `accounts` would do against `config`, in the order given.
///
/// `key_fingerprints` maps a configured provider's key reference to a digest of
/// the credential stored under it, for the providers the caller could read one
/// from. That is what lets an entry be recognised as an account imported before
/// the source marker existed; without it only the marker can match.
///
/// Preview and import both call this, so the row the dashboard disables and the
/// entry the command writes are decided by the same rule.
pub fn plan_import(
    config: &AppConfig,
    accounts: &[SourceAccount],
    key_fingerprints: &BTreeMap<String, String>,
) -> Vec<PlannedImport> {
    accounts
        .iter()
        .map(|account| {
            // Deterministic in the source id, so asking again for an entry
            // already imported answers the same target id.
            let candidate = source_target_id(&account.name, &account.source_id);
            let decision = if let Some(existing) = matching_marker(config, account) {
                ImportDecision::AlreadyImported(existing, AlreadyImported::SameSource)
            } else if let Some(existing) = matching_account(config, account, key_fingerprints) {
                ImportDecision::AlreadyImported(existing, AlreadyImported::SameAccount)
            } else {
                ImportDecision::Planned(candidate)
            };
            PlannedImport {
                source_id: account.source_id.clone(),
                decision,
            }
        })
        .collect()
}

/// The provider id an imported source entry gets.
///
/// The name keeps it recognisable and the source marker keeps one relay's
/// several accounts apart, which a name or endpoint slug cannot.
pub fn source_target_id(name: &str, source_id: &str) -> String {
    let base = unique_provider_id(name, &|_| false);
    let marker = marker_of(source_id);
    format!("{base}-{:06x}", marker & 0x00ff_ffff)
}

/// The configured provider carrying this source entry's marker.
fn matching_marker(config: &AppConfig, account: &SourceAccount) -> Option<String> {
    let wanted = marker_of(&account.source_id) & 0x00ff_ffff;
    config
        .providers
        .iter()
        .find(|p| {
            p.id.rsplit_once('-')
                .and_then(|(_, suffix)| u64::from_str_radix(suffix, 16).ok())
                == Some(wanted)
        })
        .map(|p| p.id.clone())
}

/// The configured provider on the same endpoint storing the same credential.
///
/// Nothing is matched without a fingerprint: two keyless providers on one
/// endpoint are not necessarily the same account, and neither are two accounts
/// whose credentials the caller could not read.
fn matching_account(
    config: &AppConfig,
    account: &SourceAccount,
    key_fingerprints: &BTreeMap<String, String>,
) -> Option<String> {
    let fingerprint = account.credential_fingerprint.as_deref()?;
    config
        .providers
        .iter()
        .find(|p| {
            p.base_url.trim_end_matches('/') == account.base_url.trim_end_matches('/')
                && key_fingerprints.get(&p.key_ref).map(String::as_str) == Some(fingerprint)
        })
        .map(|p| p.id.clone())
}

/// The account identity of one CC Switch entry.
pub fn account_of(provider: &CcProvider) -> SourceAccount {
    SourceAccount {
        source_id: provider.source_id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        credential_fingerprint: provider
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty())
            .map(credential_fingerprint),
    }
}

/// A stable digest of a credential, so two accounts can be compared without the
/// keys themselves ever leaving the credential store.
pub fn credential_fingerprint(secret: &str) -> String {
    format!("{:016x}", fnv1a(secret.as_bytes()))
}

fn marker_of(source_id: &str) -> u64 {
    fnv1a(source_id.as_bytes())
}

/// FNV-1a. Not a security hash: it only has to be stable and spread ids apart.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
