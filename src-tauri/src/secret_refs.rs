//! The keychain references a configuration still names.
//!
//! Deleting a secret is only safe when nothing names it any more, and both the
//! desktop commands (removing a key or an account) and the server's
//! provider-removal path need that answer. It lives here rather than in the
//! command module because the server compiles without the `account` feature,
//! where the commands themselves do not exist.

use std::collections::BTreeSet;

use zroutery_core::config::AppConfig;

/// Every keychain reference the configuration still names, at either level.
pub(crate) fn referenced_secrets(config: &AppConfig) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for provider in &config.providers {
        if !provider.key_ref.is_empty() {
            refs.insert(provider.key_ref.clone());
        }
        for account in &provider.accounts {
            if !account.key_ref.is_empty() {
                refs.insert(account.key_ref.clone());
            }
            for key in &account.inference_keys {
                if !key.key_ref.is_empty() {
                    refs.insert(key.key_ref.clone());
                }
            }
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::config::{AccountConfig, InferenceKeyConfig, ProviderConfig, ProviderKind};

    fn config_with_account(key_ref: &str, keys: &[(&str, &str)]) -> AppConfig {
        let mut provider = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
        provider.key_ref = "provider:p".into();
        let mut account = AccountConfig {
            account_id: "a".into(),
            key_ref: key_ref.into(),
            enabled: true,
            ..AccountConfig::default()
        };
        account.inference_keys = keys
            .iter()
            .map(|(id, key_ref)| InferenceKeyConfig {
                id: (*id).into(),
                key_ref: (*key_ref).into(),
                enabled: true,
                priority: 0,
                note: String::new(),
            })
            .collect();
        provider.accounts.push(account);
        let mut config = AppConfig::default();
        config.providers.push(provider);
        config
    }

    #[test]
    fn references_in_use_cover_provider_panel_and_inference_levels() {
        let config = config_with_account(
            "provider:p:account:a",
            &[("k1", "provider:p:account:a:inference:k1")],
        );
        let refs = referenced_secrets(&config);
        assert!(refs.contains("provider:p"));
        assert!(refs.contains("provider:p:account:a"));
        assert!(refs.contains("provider:p:account:a:inference:k1"));
        assert_eq!(refs.len(), 3);
    }

    /// The shared-secret rule: a reference still named by a sibling survives,
    /// and only the references the post-change document dropped are candidates.
    #[test]
    fn only_references_the_new_document_dropped_are_orphans() {
        let before = config_with_account(
            "shared-panel",
            &[("k1", "provider:p:account:a:inference:k1")],
        );
        let live = referenced_secrets(&before);
        assert!(live.contains("shared-panel"));

        let after = config_with_account("shared-panel", &[]);
        let live_after = referenced_secrets(&after);
        let candidates = [
            "shared-panel".to_string(),
            "provider:p:account:a:inference:k1".to_string(),
        ];
        assert!(live_after.contains("shared-panel"));
        assert!(!live_after.contains("provider:p:account:a:inference:k1"));
        let orphans: Vec<&String> = candidates
            .iter()
            .filter(|key_ref| !live_after.contains(*key_ref))
            .collect();
        assert_eq!(
            orphans.len(),
            1,
            "the shared panel reference is not an orphan"
        );
        assert_eq!(orphans[0], "provider:p:account:a:inference:k1");
    }
}
