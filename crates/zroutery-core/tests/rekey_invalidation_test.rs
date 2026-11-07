//! Rekey invalidation for credential-scoped router gates.
//!
//! Authentication cooldowns and key-level 429 windows are keyed by
//! `(provider, account, key.id)`, an identity a rekey keeps while replacing the
//! secret behind it. These tests pin the configuration swap that lets the
//! replacement start without the old secret's evidence, and the edits that must
//! leave every gate standing.

use std::sync::Arc;
use std::time::Duration;

use zroutery_core::config::{
    AccountConfig, AppConfig, InferenceKeyConfig, MemorySecretStore, ModelEntry, ModelTier,
    ProviderConfig, ProviderKind,
};
use zroutery_core::failure::ClassifiedFailure;
use zroutery_core::router::{Candidate, InferenceCredential};
use zroutery_core::server::AppState;

fn branch_config() -> AppConfig {
    let mut config = AppConfig::default();
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    provider.accounts.push(AccountConfig {
        account_id: "acct".into(),
        enabled: true,
        inference_keys: vec![
            InferenceKeyConfig {
                id: "first".into(),
                key_ref: "first-ref".into(),
                priority: 0,
                ..InferenceKeyConfig::default()
            },
            InferenceKeyConfig {
                id: "second".into(),
                key_ref: "second-ref".into(),
                priority: 10,
                ..InferenceKeyConfig::default()
            },
        ],
        ..AccountConfig::default()
    });
    config.providers.push(provider);
    config.models.push(ModelEntry::for_upstream(
        "relay",
        "model",
        Some(ModelTier::Standard),
    ));
    config
}

fn candidate_for(config: &AppConfig, key_id: &str) -> Candidate {
    let provider = config.providers[0].clone();
    let entry = config.models[0].clone();
    Candidate {
        exposed_id: entry.exposed_id(),
        entry,
        provider,
        degraded: false,
        credential: Some(InferenceCredential {
            account_id: "acct".into(),
            key_id: key_id.into(),
        }),
        rate_quote: None,
    }
}

fn state_for(config: AppConfig) -> Arc<AppState> {
    Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())))
}

/// Put both an authentication cooldown and a key-level 429 window on one key.
fn cool_key(state: &AppState, key_id: &str) {
    let config = state.config();
    let candidate = candidate_for(&config, key_id);
    let routing = config.routing.clone();
    let auth = ClassifiedFailure::from_status(401, "unauthorized".into());
    state
        .router()
        .record_candidate_failure(&candidate, &auth, &routing);
    state
        .router()
        .note_candidate_rate_limit_wait(&candidate, Duration::from_secs(120));
}

fn gated(state: &AppState, key_id: &str) -> bool {
    state.router().is_key_gated("relay", "acct", key_id)
}

#[test]
fn a_rekey_forgets_the_old_secrets_credential_gates() {
    let mut config = branch_config();
    let state = state_for(config.clone());
    cool_key(&state, "first");
    assert!(gated(&state, "first"));

    config.providers[0].accounts[0].inference_keys[0].key_ref = "first-ref-v2".into();
    state.set_config(config);

    assert!(
        !gated(&state, "first"),
        "the replacement secret must not inherit the authentication cooldown or 429 window"
    );
}

#[test]
fn a_rekey_leaves_siblings_and_an_empty_wallet_latched() {
    let mut config = branch_config();
    let state = state_for(config.clone());
    cool_key(&state, "first");
    cool_key(&state, "second");
    state
        .router()
        .hold_empty_wallet("relay", Some("acct"), None);

    config.providers[0].accounts[0].inference_keys[0].key_ref = "first-ref-v2".into();
    state.set_config(config);

    assert!(
        !gated(&state, "first"),
        "the rekeyed key's own gates are gone"
    );
    assert!(
        gated(&state, "second"),
        "a sibling key's cooldown is a different identity"
    );
    assert!(
        state.router().is_account_gated("relay", "acct"),
        "an empty wallet is a fact about the account, not the secret"
    );
    assert!(
        state.router().wallet_holds().iter().any(|hold| {
            hold.provider_id == "relay"
                && hold.account_id.as_deref() == Some("acct")
                && hold.key_id.is_none()
        }),
        "the account wallet latch survives a rekey"
    );
}

#[test]
fn unrelated_edits_and_an_unchanged_reference_keep_the_gate() {
    let mut config = branch_config();
    let state = state_for(config.clone());
    cool_key(&state, "first");

    config.providers[0].name = "Renamed Relay".into();
    config.providers[0].accounts[0].inference_keys[0].note = "operator note".into();
    config.providers[0].accounts[0].inference_keys[0].priority = 7;
    config.models[0].priority = 3;
    state.set_config(config);
    assert!(
        gated(&state, "first"),
        "an edit that moves no secret must not clear a cooldown"
    );

    // The same id with an unchanged reference is not a rekey.
    let next = (*state.config()).clone();
    state.set_config(next);
    assert!(
        gated(&state, "first"),
        "saving the same reference again is not recovery evidence"
    );

    // Rekeying a sibling only forgets that sibling.
    let mut sibling_rekey = (*state.config()).clone();
    sibling_rekey.providers[0].accounts[0].inference_keys[1].key_ref = "second-ref-v2".into();
    state.set_config(sibling_rekey);
    assert!(
        gated(&state, "first"),
        "another key's rekey must not clear this key"
    );
}
