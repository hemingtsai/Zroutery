//! Executable credentials and quotes, resolved before model cost decisions.

use std::collections::HashSet;

use super::{Candidate, InferenceCredential, Router};
use crate::config::AppConfig;

impl Router {
    /// One model axis, several executable identities. The first option respects
    /// manual key order; a cost policy chooses from this same set before scoring.
    /// Panel tokens are excluded even when another provider references them.
    ///
    /// Accounts, keys and rates are read from the configuration this call
    /// receives, never from the candidate's planning snapshot: an option must
    /// describe a credential that is configured *now*. The option keeps the
    /// candidate's planned provider fields (base URL, protocol, outbound
    /// settings), so [`crate::server::pipeline`] can still detect that the
    /// operator changed them between planning and sending.
    pub fn execution_options(&self, config: &AppConfig, candidate: &Candidate) -> Vec<Candidate> {
        let Some(provider) = config.provider(&candidate.provider.id) else {
            // The provider was removed after planning. Its planned snapshot has
            // no executable identity left and must not be sent.
            return Vec::new();
        };
        if !provider
            .accounts
            .iter()
            .any(|account| !account.inference_keys.is_empty())
        {
            let mut option = candidate.clone();
            option.rate_quote = crate::billing::resolve_rate(
                provider,
                &candidate.entry,
                None,
                None,
                chrono::Utc::now().timestamp(),
            );
            return if self.credential_available(&option) {
                vec![option]
            } else {
                Vec::new()
            };
        }
        let panel_refs: HashSet<_> = config
            .providers
            .iter()
            .flat_map(|provider| &provider.accounts)
            .map(|account| account.key_ref.as_str())
            .filter(|key| !key.is_empty())
            .collect();
        let panel_refs = &panel_refs;
        let mut keys: Vec<_> = provider
            .accounts
            .iter()
            .filter(|account| account.enabled)
            .flat_map(|account| {
                account.inference_keys.iter().filter_map(move |key| {
                    (key.enabled
                        && !key.key_ref.trim().is_empty()
                        && !panel_refs.contains(key.key_ref.as_str()))
                    .then_some((account, key))
                })
            })
            .collect();
        keys.sort_by(|(a, ka), (b, kb)| {
            ka.priority
                .cmp(&kb.priority)
                .then_with(|| a.account_id.cmp(&b.account_id))
                .then_with(|| ka.id.cmp(&kb.id))
        });
        let mut refs = HashSet::new();
        keys.into_iter()
            .filter_map(|(account, key)| {
                if !refs.insert(key.key_ref.as_str()) {
                    return None;
                }
                let mut option = candidate.clone();
                option.provider.key_ref = key.key_ref.clone();
                option.credential = Some(InferenceCredential {
                    account_id: account.account_id.clone(),
                    key_id: key.id.clone(),
                });
                option.rate_quote = crate::billing::resolve_rate(
                    provider,
                    &candidate.entry,
                    Some(account),
                    Some(&key.id),
                    chrono::Utc::now().timestamp(),
                );
                self.credential_available(&option).then_some(option)
            })
            .collect()
    }
}
