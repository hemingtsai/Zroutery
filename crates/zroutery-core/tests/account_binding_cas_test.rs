#![cfg(feature = "account")]

use zroutery_core::account::{AccountId, AccountRuntime, AccountStore};
use zroutery_core::config::{
    AccountBinding, AccountConfig, AppConfig, ProviderConfig, ProviderKind,
};

#[test]
fn changed_binding_and_deleted_recreated_account_reject_late_probe_results() {
    let store = AccountStore::new();
    let mut config = AppConfig::default();
    let mut provider = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    provider.accounts.push(AccountConfig {
        account_id: "a".into(),
        enabled: true,
        binding: Some(AccountBinding::fresh()),
        ..Default::default()
    });
    config.providers.push(provider);
    store.sync_declarations(&config);
    let old = store
        .begin_declared_probe(&config.providers[0], &config.providers[0].accounts[0])
        .unwrap();
    config.providers[0].accounts[0]
        .binding
        .as_mut()
        .unwrap()
        .credential_revision += 1;
    store.sync_declarations(&config);
    let runtime = AccountRuntime {
        account_id: AccountId("a".into()),
        provider_id: "p".into(),
        ..Default::default()
    };
    assert!(!store.publish_if_current(runtime.clone(), old));
    let before_delete = store
        .begin_declared_probe(&config.providers[0], &config.providers[0].accounts[0])
        .unwrap();
    let mut removed = config.clone();
    removed.providers[0].accounts.clear();
    store.sync_declarations(&removed);
    config.providers[0].accounts[0].binding = Some(AccountBinding::fresh());
    store.sync_declarations(&config);
    assert!(!store.publish_if_current(runtime.clone(), before_delete));
    let current = store
        .begin_declared_probe(&config.providers[0], &config.providers[0].accounts[0])
        .unwrap();
    assert!(store.publish_if_current(runtime, current));
}

#[test]
fn newer_probe_wins_without_touching_a_sibling_account() {
    let store = AccountStore::new();
    let a = AccountId("a".into());
    let b = AccountId("b".into());
    let old_a = store.begin_probe("p", &a);
    let b_version = store.begin_probe("p", &b);
    let new_a = store.begin_probe("p", &a);
    let runtime = |account| AccountRuntime {
        provider_id: "p".into(),
        account_id: account,
        ..Default::default()
    };
    assert!(!store.publish_if_current(runtime(a.clone()), old_a));
    assert!(store.publish_if_current(runtime(b), b_version));
    assert!(store.publish_if_current(runtime(a), new_a));
    assert_eq!(store.list_by_provider("p").len(), 2);
}
