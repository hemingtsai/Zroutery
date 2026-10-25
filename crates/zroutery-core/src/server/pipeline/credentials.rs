//! Resolve explicit account inference keys after model ranking, without changing
//! the model axis used by policy or ML. No panel credential enters this plan.

use std::collections::HashSet;

use super::AppState;
use crate::router::{Candidate, InferenceCredential};

pub(super) fn expand_inference_keys(
    state: &AppState,
    plan: Vec<Candidate>,
    limit: usize,
) -> Vec<Candidate> {
    let mut result = Vec::new();
    let config = state.config();
    let panel_refs: HashSet<&str> = config
        .providers
        .iter()
        .flat_map(|provider| &provider.accounts)
        .map(|account| account.key_ref.as_str())
        .filter(|key| !key.is_empty())
        .collect();
    for candidate in plan {
        let provider = &candidate.provider;
        // Opt-in only. A legacy account.key_ref is a panel token; the provider's
        // default inference key remains unchanged until an explicit pool exists.
        if !provider
            .accounts
            .iter()
            .any(|account| !account.inference_keys.is_empty())
        {
            result.push(candidate);
            continue;
        }
        let mut keys: Vec<_> = provider
            .accounts
            .iter()
            .filter(|account| account.enabled)
            .flat_map(|account| {
                account
                    .inference_keys
                    .iter()
                    .filter(|key| {
                        key.enabled
                            && !key.key_ref.trim().is_empty()
                            && !panel_refs.contains(key.key_ref.as_str())
                            && !state
                                .router
                                .is_account_gated(&provider.id, &account.account_id)
                            && !state.router.is_key_gated(
                                &provider.id,
                                &account.account_id,
                                &key.id,
                            )
                    })
                    .map(move |key| (account, key))
            })
            .collect();
        keys.sort_by(|(a, ka), (b, kb)| {
            ka.priority
                .cmp(&kb.priority)
                .then_with(|| a.account_id.cmp(&b.account_id))
                .then_with(|| ka.id.cmp(&kb.id))
        });
        let mut used_refs = HashSet::new();
        for (account, key) in keys {
            if !used_refs.insert(key.key_ref.as_str()) {
                continue;
            }
            let mut attempt = candidate.clone();
            attempt.provider.key_ref = key.key_ref.clone();
            attempt.credential = Some(InferenceCredential {
                account_id: account.account_id.clone(),
                key_id: key.id.clone(),
            });
            result.push(attempt);
        }
        // An explicitly configured but disabled/exhausted pool must not silently
        // fall back to the provider default or to a personal panel access token.
    }
    // Only a no-failover plan is truncated here. With failover, a key's wallet
    // may close after an earlier send: its siblings must be skipped without
    // using the request's attempt budget or hiding the next funded account.
    if limit == 1 {
        result.truncate(1);
    }
    result
}
