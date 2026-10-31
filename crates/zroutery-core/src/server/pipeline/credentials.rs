//! Resolve explicit account inference keys after model ranking, without changing
//! the model axis used by policy or ML. No panel credential enters this plan.

use std::collections::HashSet;

use super::AppState;
use crate::error::Error;
use crate::router::{Candidate, InferenceCredential};

pub(super) fn expand_inference_keys(
    state: &AppState,
    plan: Vec<Candidate>,
    limit: usize,
) -> Result<Vec<Candidate>, Error> {
    let mut result = Vec::new();
    let mut cooldown: Option<std::time::Duration> = None;
    let mut cooldown_scope: Option<String> = None;
    let mut unavailable_without_rate_limit = false;
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
        // Credential expansion happens after model ranking. Retain the shortest
        // key cooldown while filtering so an otherwise valid model whose whole
        // explicit pool is cooling reports 429 instead of looking unconfigured.
        let mut configured_key = false;
        let mut keys = Vec::new();
        for account in provider.accounts.iter().filter(|account| account.enabled) {
            for key in account.inference_keys.iter().filter(|key| {
                key.enabled
                    && !key.key_ref.trim().is_empty()
                    && !panel_refs.contains(key.key_ref.as_str())
            }) {
                configured_key = true;
                if state
                    .router
                    .is_account_gated(&provider.id, &account.account_id)
                {
                    unavailable_without_rate_limit = true;
                    continue;
                }
                if let Some(wait) = state.router.key_rate_limit_remaining(
                    &provider.id,
                    &account.account_id,
                    &key.id,
                ) {
                    if cooldown.is_none_or(|current| wait < current) {
                        cooldown = Some(wait);
                        cooldown_scope = Some(format!(
                            "{} account {} key {}",
                            provider.id, account.account_id, key.id
                        ));
                    }
                    continue;
                }
                if state
                    .router
                    .is_key_gated(&provider.id, &account.account_id, &key.id)
                {
                    unavailable_without_rate_limit = true;
                    continue;
                }
                keys.push((account, key));
            }
        }
        if !configured_key {
            unavailable_without_rate_limit = true;
        }
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
    if result.is_empty() && !unavailable_without_rate_limit {
        if let Some(retry_after) = cooldown {
            return Err(Error::RateLimited {
                scope: cooldown_scope.unwrap_or_else(|| "inference key pool".to_string()),
                retry_after,
            });
        }
    }
    Ok(result)
}
