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
        // Read the provider from the current configuration. The candidate
        // carries its planning snapshot for identity checks; expansion must
        // select from what is configured now, not what was configured when the
        // plan was ranked.
        let Some(provider) = config.provider(&candidate.provider.id) else {
            continue;
        };
        // A cost policy already chose an executable option before scoring. Keep
        // that exact credential first, then its available siblings as fallback.
        if candidate.credential.is_some() {
            let mut options = state.router.execution_options(&config, &candidate);
            options.sort_by_key(|option| option.credential != candidate.credential);
            let Some(selected) = options
                .first_mut()
                .filter(|selected| selected.credential == candidate.credential)
            else {
                // The decision bound this exact credential and its quote. If it
                // vanished between ranking and expansion, sending a sibling
                // would settle a different price under the recorded choice, so
                // the candidate is dropped instead of silently substituted.
                if let Some(credential) = &candidate.credential {
                    if let Some(wait) = state.router.key_rate_limit_remaining(
                        &candidate.provider.id,
                        &credential.account_id,
                        &credential.key_id,
                    ) {
                        if cooldown.is_none_or(|current| wait < current) {
                            cooldown = Some(wait);
                            cooldown_scope = Some(format!(
                                "{} account {} key {}",
                                candidate.provider.id, credential.account_id, credential.key_id
                            ));
                        }
                    } else {
                        unavailable_without_rate_limit = true;
                    }
                }
                continue;
            };
            // Keep the decision-time quote intact. Execution validates its
            // freshness rather than quietly replacing a scored tariff.
            selected.provider.key_ref = candidate.provider.key_ref.clone();
            selected.rate_quote = candidate.rate_quote.clone();
            result.extend(options);
            continue;
        }
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
            // The executed credential, not the plan-time estimate, prices the
            // attempt: re-resolve against the account and key that are
            // configured now, so a verified or manually set rate for the
            // identity actually being sent replaces the shared model-level
            // quote. A rate bound to another key of the same account never
            // transfers to this one.
            attempt.rate_quote = crate::billing::resolve_rate(
                provider,
                &attempt.entry,
                Some(account),
                Some(key.id.as_str()),
                chrono::Utc::now().timestamp(),
            );
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

/// Recheck the identity and quote immediately before a send. A stale binding,
/// rate or closed credential must not send one key while accounting under
/// another snapshot.
pub(super) fn validate_execution(state: &AppState, candidate: &Candidate) -> Result<(), Error> {
    let config = state.config();
    let Some(provider) = config.provider(&candidate.provider.id) else {
        return Err(Error::NoCandidate(
            "provider was removed before execution".into(),
        ));
    };
    // The send is built from the planned snapshot. Everything that decides
    // what leaves this process — endpoint, protocol, the way the credential
    // travels and the headers around it — must still be what the plan recorded.
    if !provider.enabled
        || provider.base_url != candidate.provider.base_url
        || provider.kind != candidate.provider.kind
        || provider.dialect() != candidate.provider.dialect()
        || provider.bearer_auth != candidate.provider.bearer_auth
        || provider.anthropic_version != candidate.provider.anthropic_version
        || provider.extra_headers != candidate.provider.extra_headers
        || provider.configured_client_profile() != candidate.provider.configured_client_profile()
        || provider.quirks != candidate.provider.quirks
        || provider.decision != candidate.provider.decision
        || provider.bypass_proxy != candidate.provider.bypass_proxy
    {
        return Err(Error::NoCandidate(
            "provider changed before execution".into(),
        ));
    }
    let entry = config
        .models
        .iter()
        .find(|model| model.key() == candidate.entry.key() && model.enabled)
        .ok_or_else(|| Error::NoCandidate("model changed before execution".into()))?;
    if entry.tier != candidate.entry.tier
        || entry.capabilities != candidate.entry.capabilities
        || entry.max_output_tokens != candidate.entry.max_output_tokens
        || entry.context_window != candidate.entry.context_window
        || entry.reasoning_capability != candidate.entry.reasoning_capability
        || entry.effort_levels != candidate.entry.effort_levels
    {
        return Err(Error::NoCandidate(
            "model execution requirements changed before execution".into(),
        ));
    }
    let account = candidate
        .credential
        .as_ref()
        .map(|credential| {
            let account = provider
                .accounts
                .iter()
                .find(|account| account.account_id == credential.account_id && account.enabled)
                .ok_or_else(|| Error::NoCandidate("account changed before execution".into()))?;
            let previous_version = candidate
                .provider
                .accounts
                .iter()
                .find(|previous| previous.account_id == credential.account_id)
                .and_then(|previous| previous.binding.as_ref())
                .map(|binding| binding.version());
            if previous_version != account.binding.as_ref().map(|binding| binding.version()) {
                return Err(Error::NoCandidate(
                    "account binding changed before execution".into(),
                ));
            }
            if !account.inference_keys.iter().any(|key| {
                key.id == credential.key_id
                    && key.enabled
                    && key.key_ref == candidate.provider.key_ref
            }) {
                return Err(Error::NoCandidate(
                    "inference credential changed before execution".into(),
                ));
            }
            Ok(account)
        })
        .transpose()?;
    if account.is_none() && provider.key_ref != candidate.provider.key_ref {
        return Err(Error::NoCandidate(
            "provider credential changed before execution".into(),
        ));
    }
    if config
        .providers
        .iter()
        .flat_map(|provider| &provider.accounts)
        .any(|account| !account.key_ref.is_empty() && account.key_ref == candidate.provider.key_ref)
    {
        return Err(Error::NoCandidate(
            "the inference reference is a panel credential".into(),
        ));
    }
    // Availability can close after planning: a latched wallet, a hard gate or a
    // rate-limit window must not spend another doomed upstream call. A cooling
    // window reports 429 with its own retry hint; everything else is a refusal.
    if let Some(credential) = &candidate.credential {
        if let Some(wait) = state.router.key_rate_limit_remaining(
            &provider.id,
            &credential.account_id,
            &credential.key_id,
        ) {
            return Err(Error::RateLimited {
                scope: format!(
                    "{} account {} key {}",
                    provider.id, credential.account_id, credential.key_id
                ),
                retry_after: wait,
            });
        }
    } else if let Some(wait) = state
        .router
        .model_rate_limit_remaining(&provider.id, candidate.model_id())
    {
        return Err(Error::RateLimited {
            scope: format!("{} model {}", provider.id, candidate.model_id()),
            retry_after: wait,
        });
    }
    if !state.router.credential_available(candidate) {
        return Err(Error::NoCandidate(
            "the planned credential or provider closed before execution".into(),
        ));
    }
    let current_quote = crate::billing::resolve_rate(
        provider,
        entry,
        account,
        candidate
            .credential
            .as_ref()
            .map(|credential| credential.key_id.as_str()),
        chrono::Utc::now().timestamp(),
    );
    // Any difference rejects, including a planned unknown that has since gained
    // a price and a bound rate that expired or was re-observed: the attempt
    // settles under the planned snapshot, so the plan and the send must name
    // the same tariff.
    if current_quote != candidate.rate_quote {
        return Err(Error::NoCandidate(
            "effective price changed or expired before execution".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_budget_price(
    state: &AppState,
    candidate: &Candidate,
    main: bool,
) -> Result<(), Error> {
    let config = state.config();
    let scoped: Vec<_> = config
        .budgets
        .iter()
        .filter(|budget| budget.enabled)
        .filter(|budget| match &budget.scope {
            crate::budget::BudgetScope::Global => true,
            crate::budget::BudgetScope::Provider { id } => id == &candidate.provider.id,
            crate::budget::BudgetScope::Tier { tier } => {
                main && Some(*tier) == candidate.entry.tier
            }
        })
        .collect();
    let pricing = candidate.settled_pricing();
    if !scoped.is_empty() && pricing.is_none() {
        return Err(Error::OverBudget(
            "this budget requires an effective or explicit fallback rate".into(),
        ));
    }
    if let Some(pricing) = pricing {
        if scoped.iter().any(|budget| {
            !scoped.iter().any(|matching| {
                matching.scope == budget.scope && matching.limit.currency == pricing.currency
            })
        }) {
            return Err(Error::OverBudget(
                "the executed billing currency has no matching limit for this budget scope".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::billing::{EffectiveRate, Pricing, RateSource, RATE_SCHEMA_VERSION};
    use crate::budget::{Budget, BudgetPeriod, BudgetScope};
    use crate::config::{
        AccountBinding, AccountConfig, AppConfig, InferenceKeyConfig, MemorySecretStore,
        ModelEntry, ModelTier, ProviderConfig, ProviderKind,
    };
    use crate::ir::Dialect;

    const MODEL: &str = "model";
    const BINDING_VERSION: &str = "gen:0:0";

    fn account(
        account_id: &str,
        key_id: &str,
        key_enabled: bool,
        rate: Option<Pricing>,
    ) -> AccountConfig {
        AccountConfig {
            account_id: account_id.into(),
            key_ref: "panel-token".into(),
            enabled: true,
            binding: Some(AccountBinding {
                generation: "gen".into(),
                ..AccountBinding::default()
            }),
            inference_keys: vec![InferenceKeyConfig {
                id: key_id.into(),
                key_ref: format!("inference-{key_id}"),
                enabled: key_enabled,
                ..InferenceKeyConfig::default()
            }],
            effective_rates: rate
                .map(|pricing| {
                    vec![EffectiveRate {
                        schema_version: RATE_SCHEMA_VERSION,
                        id: format!("rate-{account_id}"),
                        upstream_model: MODEL.into(),
                        dialect: Dialect::OpenAI,
                        binding_version: BINDING_VERSION.into(),
                        inference_key_id: Some(key_id.into()),
                        group: Some("vip".into()),
                        source: RateSource::VerifiedEffective,
                        observed_at: 1,
                        expires_at: None,
                        evidence: "panel /api/pricing for the account's group".into(),
                        pricing,
                    }]
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    fn pool_provider(accounts: Vec<AccountConfig>) -> ProviderConfig {
        let mut provider = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
        provider.accounts = accounts;
        provider
    }

    /// One account, two priced keys: `first` is the manual priority head,
    /// `cheap` is the key a cost policy would score and choose.
    fn two_key_provider() -> ProviderConfig {
        let mut a = account("a", "first", true, None);
        a.inference_keys.push(InferenceKeyConfig {
            id: "cheap".into(),
            key_ref: "inference-cheap".into(),
            priority: 50,
            ..InferenceKeyConfig::default()
        });
        let key_rate = |id: &str, key_id: &str, amount: f64| EffectiveRate {
            schema_version: RATE_SCHEMA_VERSION,
            id: id.into(),
            upstream_model: MODEL.into(),
            dialect: Dialect::OpenAI,
            binding_version: BINDING_VERSION.into(),
            inference_key_id: Some(key_id.into()),
            group: Some("std".into()),
            source: RateSource::ManualOverride,
            observed_at: 1,
            expires_at: None,
            evidence: "explicit fixture tariff".into(),
            pricing: Pricing {
                per_request: Some(amount),
                ..Pricing::new("USD", 0.0, 0.0)
            },
        };
        a.effective_rates = vec![
            key_rate("rate-first", "first", 0.8),
            key_rate("rate-cheap", "cheap", 0.02),
        ];
        pool_provider(vec![a])
    }

    /// The candidate a decision hands to expansion: the planned provider
    /// snapshot, one selected credential, and the quote bound with it.
    fn quoted_candidate(provider: &ProviderConfig, account_id: &str, key_id: &str) -> Candidate {
        let entry = ModelEntry::for_upstream("p", MODEL, Some(ModelTier::Standard));
        let mut candidate = Candidate {
            exposed_id: entry.exposed_id(),
            entry,
            provider: provider.clone(),
            degraded: false,
            credential: Some(InferenceCredential {
                account_id: account_id.into(),
                key_id: key_id.into(),
            }),
            rate_quote: None,
        };
        candidate.provider.key_ref = format!("inference-{key_id}");
        let account = provider
            .accounts
            .iter()
            .find(|account| account.account_id == account_id)
            .expect("fixture account");
        candidate.rate_quote = crate::billing::resolve_rate(
            provider,
            &candidate.entry,
            Some(account),
            Some(key_id),
            chrono::Utc::now().timestamp(),
        );
        candidate
    }

    fn planned_candidate(provider: &ProviderConfig) -> Candidate {
        let entry = ModelEntry::for_upstream("p", MODEL, Some(ModelTier::Standard));
        Candidate {
            exposed_id: entry.exposed_id(),
            entry,
            provider: provider.clone(),
            degraded: false,
            credential: None,
            rate_quote: None,
        }
    }

    fn state_of(provider: &ProviderConfig) -> Arc<AppState> {
        state_with(provider, [])
    }

    fn state_with(
        provider: &ProviderConfig,
        budgets: impl IntoIterator<Item = Budget>,
    ) -> Arc<AppState> {
        let mut config = AppConfig::default();
        config.providers.push(provider.clone());
        config.models.push(ModelEntry::for_upstream(
            "p",
            MODEL,
            Some(ModelTier::Standard),
        ));
        config.budgets = budgets.into_iter().collect();
        Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())))
    }

    #[test]
    fn an_unchanged_plan_still_validates() {
        let provider = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&provider, "a", "k");
        let state = state_of(&provider);
        assert!(validate_execution(&state, &candidate).is_ok());
        assert!(validate_budget_price(&state, &candidate, true).is_ok());
    }

    #[test]
    fn a_price_changed_after_planning_is_rejected() {
        let planned = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&planned, "a", "k");
        let current = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 9.0, 40.0)),
        )]);
        let state = state_of(&current);
        assert!(validate_execution(&state, &candidate).is_err());
    }

    #[test]
    fn a_price_appearing_after_an_unknown_plan_is_rejected() {
        let planned = pool_provider(vec![account("a", "k", true, None)]);
        let candidate = quoted_candidate(&planned, "a", "k");
        assert!(
            candidate.rate_quote.is_none(),
            "the plan resolved no price for this identity"
        );
        // Unknown stays unknown when nothing appeared: still sendable.
        let state = state_of(&planned);
        assert!(validate_execution(&state, &candidate).is_ok());
        // 缺价→新价: a price arriving between plan and send changes the tariff
        // the attempt would settle under, so it must not send.
        let current = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let state = state_of(&current);
        assert!(validate_execution(&state, &candidate).is_err());
    }

    #[test]
    fn an_expired_rate_is_rejected() {
        let planned = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&planned, "a", "k");
        assert!(candidate.rate_quote.is_some());
        let mut current = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        current.accounts[0].effective_rates[0].expires_at = Some(1);
        let state = state_of(&current);
        assert!(validate_execution(&state, &candidate).is_err());
    }

    #[test]
    fn a_regenerated_binding_is_rejected() {
        let planned = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&planned, "a", "k");
        let mut current = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        current.accounts[0].binding = Some(AccountBinding {
            generation: "gen2".into(),
            ..AccountBinding::default()
        });
        let state = state_of(&current);
        let error = validate_execution(&state, &candidate).unwrap_err();
        assert!(error.to_string().contains("binding"));
    }

    #[test]
    fn a_disabled_or_rotated_key_is_rejected() {
        let planned = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&planned, "a", "k");
        let disabled = pool_provider(vec![account(
            "a",
            "k",
            false,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let state = state_of(&disabled);
        assert!(validate_execution(&state, &candidate).is_err());
        let mut rotated = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        rotated.accounts[0].inference_keys[0].key_ref = "inference-rotated".into();
        let state = state_of(&rotated);
        assert!(validate_execution(&state, &candidate).is_err());
    }

    #[test]
    fn expansion_cannot_hide_a_rotated_selected_reference() {
        let planned = two_key_provider();
        let candidate = quoted_candidate(&planned, "a", "cheap");
        let mut current = planned.clone();
        current.accounts[0].inference_keys[1].key_ref = "new-secret-reference".into();
        let state = state_of(&current);
        let expanded = expand_inference_keys(&state, vec![candidate.clone()], 3).unwrap();
        assert_eq!(expanded[0].provider.key_ref, candidate.provider.key_ref);
        assert!(validate_execution(&state, &expanded[0]).is_err());
        assert!(
            validate_execution(&state, &expanded[1]).is_ok(),
            "unchanged sibling remains executable"
        );
    }

    #[test]
    fn model_requirement_changes_cannot_send_the_old_control() {
        let provider = two_key_provider();
        let candidate = quoted_candidate(&provider, "a", "cheap");
        let state = state_of(&provider);
        let mut config = (*state.config()).clone();
        config.models[0].effort_levels = vec!["low".into()];
        state.set_config(config);
        assert!(validate_execution(&state, &candidate).is_err());
    }

    /// The mutation cases of `an_outbound_change_is_rejected`, named for the
    /// refusal each one must produce.
    type ProviderMutations = Vec<(&'static str, Box<dyn Fn(&mut ProviderConfig)>)>;

    #[test]
    fn an_outbound_change_is_rejected() {
        let planned = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&planned, "a", "k");
        let mutations: ProviderMutations = vec![
            (
                "enabled",
                Box::new(|p: &mut ProviderConfig| p.enabled = false),
            ),
            (
                "base_url",
                Box::new(|p: &mut ProviderConfig| {
                    p.base_url = "https://moved.example/v1".into();
                }),
            ),
            (
                "api_surface",
                Box::new(|p: &mut ProviderConfig| {
                    p.api_surface = serde_json::from_value(serde_json::json!("responses")).unwrap();
                }),
            ),
            (
                "bearer_auth",
                Box::new(|p: &mut ProviderConfig| p.bearer_auth = true),
            ),
            (
                "anthropic_version",
                Box::new(|p: &mut ProviderConfig| {
                    p.anthropic_version = Some("2024-10-22".into());
                }),
            ),
            (
                "extra_headers",
                Box::new(|p: &mut ProviderConfig| {
                    p.extra_headers.insert("x-tenant".into(), "t".into());
                }),
            ),
            (
                "client_profile",
                Box::new(|p: &mut ProviderConfig| {
                    p.client_profile = crate::config::ProviderClientProfile::ClaudeCode;
                }),
            ),
            (
                "quirks",
                Box::new(|p: &mut ProviderConfig| p.quirks.drop_temperature = true),
            ),
        ];
        for (name, mutate) in mutations {
            let mut current = planned.clone();
            mutate(&mut current);
            let state = state_of(&current);
            assert!(
                validate_execution(&state, &candidate).is_err(),
                "a changed {name} must not send the planned snapshot"
            );
        }
    }

    #[test]
    fn a_closed_wallet_is_refused_and_a_sibling_is_unaffected() {
        let provider = pool_provider(vec![
            account("a", "k", true, Some(Pricing::new("CNY", 1.0, 4.0))),
            account("b", "kb", true, Some(Pricing::new("CNY", 2.0, 8.0))),
        ]);
        let candidate = quoted_candidate(&provider, "a", "k");
        let sibling = quoted_candidate(&provider, "b", "kb");
        let state = state_of(&provider);
        // Wallet closed after planning: no doomed upstream call.
        state.router.hold_empty_wallet("p", Some("a"), None);
        assert!(validate_execution(&state, &candidate).is_err());
        // The sibling account never belonged to that latch.
        assert!(validate_execution(&state, &sibling).is_ok());
        state.router.release_wallet("p", Some("a"), None).unwrap();
        assert!(validate_execution(&state, &candidate).is_ok());
        // A key-level latch closes only that key.
        state.router.hold_empty_wallet("p", Some("a"), Some("k"));
        assert!(validate_execution(&state, &candidate).is_err());
        assert!(validate_execution(&state, &sibling).is_ok());
        // A provider-level latch closes everything under it.
        state
            .router
            .release_wallet("p", Some("a"), Some("k"))
            .unwrap();
        state.router.hold_empty_wallet("p", None, None);
        assert!(validate_execution(&state, &sibling).is_err());
    }

    #[test]
    fn a_cooling_key_or_model_reports_429_with_retry_after() {
        let provider = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&provider, "a", "k");
        let state = state_of(&provider);
        state
            .router
            .note_candidate_rate_limit_wait(&candidate, Duration::from_secs(30));
        match validate_execution(&state, &candidate) {
            Err(Error::RateLimited { retry_after, .. }) => {
                assert!(retry_after > Duration::from_secs(28));
            }
            other => panic!("a cooling key must report 429, got {other:?}"),
        }
        // Without a credential the same observation cools the model.
        let default_candidate = planned_candidate(&provider);
        state
            .router
            .note_candidate_rate_limit_wait(&default_candidate, Duration::from_secs(30));
        match validate_execution(&state, &default_candidate) {
            Err(Error::RateLimited { retry_after, .. }) => {
                assert!(retry_after > Duration::from_secs(28));
            }
            other => panic!("a cooling model must report 429, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_price_is_refused_only_when_a_budget_needs_it() {
        let provider = pool_provider(vec![account("a", "k", true, None)]);
        let candidate = quoted_candidate(&provider, "a", "k");
        assert!(candidate.rate_quote.is_none());
        // No budget covers this request: unknown cost is allowed and recorded.
        let state = state_of(&provider);
        assert!(validate_budget_price(&state, &candidate, true).is_ok());
        // A related budget must not admit a request whose price is unknown.
        let budgeted = state_with(
            &provider,
            [Budget::new(
                BudgetScope::Global,
                BudgetPeriod::Day,
                "USD",
                10.0,
            )],
        );
        assert!(validate_budget_price(&budgeted, &candidate, true).is_err());
    }

    #[test]
    fn tier_budgets_still_exempt_classifier_traffic() {
        let provider = pool_provider(vec![account("a", "k", true, None)]);
        let candidate = quoted_candidate(&provider, "a", "k");
        let state = state_with(
            &provider,
            [Budget::new(
                BudgetScope::Tier {
                    tier: ModelTier::Standard,
                },
                BudgetPeriod::Day,
                "USD",
                10.0,
            )],
        );
        // The classifier exemption survives: side traffic is not tier-budgeted.
        assert!(validate_budget_price(&state, &candidate, false).is_ok());
        assert!(validate_budget_price(&state, &candidate, true).is_err());
    }

    #[test]
    fn a_budget_in_another_currency_is_not_satisfied_by_this_price() {
        let provider = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = quoted_candidate(&provider, "a", "k");
        let usd = state_with(
            &provider,
            [Budget::new(
                BudgetScope::Global,
                BudgetPeriod::Day,
                "USD",
                10.0,
            )],
        );
        assert!(validate_budget_price(&usd, &candidate, true).is_err());
        let cny = state_with(
            &provider,
            [Budget::new(
                BudgetScope::Global,
                BudgetPeriod::Day,
                "CNY",
                10.0,
            )],
        );
        assert!(validate_budget_price(&cny, &candidate, true).is_ok());
    }

    #[test]
    fn the_selected_credential_stays_first_with_its_own_quote() {
        let provider = two_key_provider();
        let candidate = quoted_candidate(&provider, "a", "cheap");
        let state = state_of(&provider);
        let expanded = expand_inference_keys(&state, vec![candidate.clone()], 2).unwrap();
        assert_eq!(expanded.len(), 2);
        assert_eq!(
            expanded[0].credential.as_ref().unwrap().key_id,
            "cheap",
            "the scored choice stays first"
        );
        assert_eq!(
            expanded[0].rate_quote, candidate.rate_quote,
            "the scored quote survives expansion unchanged"
        );
        assert_eq!(
            expanded[1].credential.as_ref().unwrap().key_id,
            "first",
            "the sibling follows in manual priority"
        );
        assert_ne!(
            expanded[1].rate_quote, candidate.rate_quote,
            "the sibling carries its own price, not the scored one"
        );
    }

    #[test]
    fn a_lost_selected_credential_is_dropped_not_substituted() {
        let provider = two_key_provider();
        let candidate = quoted_candidate(&provider, "a", "cheap");
        let state = state_of(&provider);
        // The scored key's wallet closed after ranking: sending the sibling
        // would settle 0.8 under a plan that explained 0.02.
        state
            .router
            .hold_empty_wallet("p", Some("a"), Some("cheap"));
        let expanded = expand_inference_keys(&state, vec![candidate.clone()], 2).unwrap();
        assert!(
            expanded.is_empty(),
            "a lost selection must not be silently replaced by another price"
        );
        // A cooling scored key reports 429 instead of a substituted sibling.
        let cooling_state = state_of(&provider);
        cooling_state
            .router
            .note_candidate_rate_limit_wait(&candidate, Duration::from_secs(30));
        let error = expand_inference_keys(&cooling_state, vec![candidate], 2).unwrap_err();
        assert!(matches!(error, Error::RateLimited { .. }));
    }

    #[test]
    fn expansion_reads_the_current_configuration() {
        let planned = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let candidate = planned_candidate(&planned);
        // The key was disabled after planning: expansion must not offer it.
        let disabled = pool_provider(vec![account(
            "a",
            "k",
            false,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        let state = state_of(&disabled);
        assert!(expand_inference_keys(&state, vec![candidate.clone()], 2)
            .unwrap()
            .is_empty());
        // The rate was re-observed after planning: the attempt carries the
        // current price and still validates against the same configuration.
        let repriced = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 9.0, 40.0)),
        )]);
        let state = state_of(&repriced);
        let expanded = expand_inference_keys(&state, vec![candidate], 2).unwrap();
        assert_eq!(expanded.len(), 1);
        assert_eq!(
            expanded[0]
                .rate_quote
                .as_ref()
                .unwrap()
                .pricing
                .input_per_mtok,
            9.0
        );
        assert!(validate_execution(&state, &expanded[0]).is_ok());
    }

    #[test]
    fn expansion_never_borrows_a_panel_token() {
        let mut provider = pool_provider(vec![account(
            "a",
            "k",
            true,
            Some(Pricing::new("CNY", 1.0, 4.0)),
        )]);
        // A key that points at the panel credential is not an inference key.
        provider.accounts[0]
            .inference_keys
            .push(InferenceKeyConfig {
                id: "panel-clone".into(),
                key_ref: "panel-token".into(),
                ..InferenceKeyConfig::default()
            });
        let state = state_of(&provider);
        let candidate = planned_candidate(&provider);
        let expanded = expand_inference_keys(&state, vec![candidate], 5).unwrap();
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].credential.as_ref().unwrap().key_id, "k");
    }
}
