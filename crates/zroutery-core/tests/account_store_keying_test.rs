//! What a duplicated `account_id` costs once the store is in the picture.
//!
//! # Why this is a separate target
//!
//! The other half of this argument is in `account_config_test.rs`, which reads
//! only `config.rs` types and so runs under every feature set. This half needs
//! `zroutery_core::account`, which is gated, so it lives here behind its own gate.
//!
//! That split is not tidiness. It was found the hard way: this assertion used to
//! sit at the end of `account_config_test.rs`, where every account-feature run
//! passed and `cargo test --workspace` — which is a DEFAULT-features build, and is
//! what `ci.yml` runs — failed to compile with `error[E0433]: cannot find 'account'
//! in 'zroutery_core`. The file had no crate-level gate, so one test needing a
//! feature-gated module took the four that do not down with it, in the one
//! configuration where none of them can run at all.
//!
//! The lesson is the shape, not the instance: a test target that reads a
//! feature-gated module needs `#![cfg(feature = "...")]`, and a target that mixes
//! gated and ungated assertions has to be split rather than gated as a whole,
//! because gating the whole file would silently stop the ungated assertions from
//! running in the default build.

// Gated on `account`, matching the twenty existing targets that gate on
// `feature = "ml"`. Load-bearing for CI, not tidiness.
#![cfg(feature = "account")]

use zroutery_core::account::{AccountId, AccountRuntime, AccountStore};

#[test]
fn a_duplicated_account_id_loses_one_accounts_state_in_the_store() {
    // The configuration half of this is in `account_config_test.rs`: serde accepts
    // two accounts sharing an id, because it cannot know they are duplicates. What
    // that costs is here.
    //
    // The store is keyed on `(provider_id, account_id)`, so a shared id means the
    // second upsert replaces the first. That is why `account_id` is a required
    // field rather than a defaulted one: two defaulted ids would collide silently
    // with one account unreachable while the configuration still claimed both.
    let store = AccountStore::new();
    let mut runtime = AccountRuntime {
        account_id: AccountId("main".to_string()),
        provider_id: "relay".to_string(),
        ..Default::default()
    };

    store.upsert(runtime.clone());
    runtime.metadata.insert("marker".into(), "second".into());
    store.upsert(runtime);

    let listed = store.list_by_provider("relay");
    assert_eq!(
        listed.len(),
        1,
        "the second upsert replaced the first because the key is (provider, account)"
    );
    assert_eq!(
        listed[0].metadata.get("marker").map(String::as_str),
        Some("second"),
        "so a duplicated account_id loses one account's state with no error anywhere"
    );
}
